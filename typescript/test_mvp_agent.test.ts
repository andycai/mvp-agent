// 离线测试:不联网、不需要真实 API Key。
// 端到端用本地 http 服务(真实 fetch),并在每次请求上断言历史合法。
import { test } from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  agentLoop,
  callTool,
  chat,
  configFromEnv,
  runTool,
  toolBash,
  toolSchemas,
  trim,
  type Config,
  type Msg,
} from "./mvp_agent.ts";

function cfg(baseUrl: string): Config {
  return {
    apiKey: "test-key",
    baseUrl,
    model: "fake-model",
    maxTurns: 15,
    requestTimeout: 20,
    toolTimeout: 10,
    maxToolChars: 16000,
    maxHistory: 40,
  };
}

function tmp(tag: string): string {
  return mkdtempSync(join(tmpdir(), "mvp-agent-" + tag + "-"));
}

function tc(id: string, name: string, args: string): Msg {
  return { id, type: "function", function: { name, arguments: args } };
}

function msgCalls(calls: Msg[]): Msg {
  return { role: "assistant", content: null, tool_calls: calls };
}

function msgText(t: string): Msg {
  return { role: "assistant", content: t };
}

function response(m: Msg): string {
  return JSON.stringify({ choices: [{ message: m }] });
}

/** 历史合法性:首条 system;每个 tool_call 恰好一条配对 tool 响应。 */
function legality(msgs: Msg[]): string[] {
  const errs: string[] = [];
  if (msgs.length === 0) return ["空 messages"];
  if (msgs[0].role !== "system") errs.push("首条不是 system");
  let pending: string[] = [];
  msgs.forEach((m, i) => {
    if (m.role === "assistant") {
      if (pending.length) errs.push("assistant 前仍有未配对 " + JSON.stringify(pending));
      pending = [];
      const calls = Array.isArray(m.tool_calls) ? (m.tool_calls as Msg[]) : [];
      for (const t of calls) pending.push(String(t.id));
    } else if (m.role === "tool") {
      const id = String(m.tool_call_id);
      const at = pending.indexOf(id);
      if (at < 0) errs.push("孤儿 tool 响应 " + id + " @" + String(i));
      else pending.splice(at, 1);
    }
  });
  if (pending.length) errs.push("结尾未配对 " + JSON.stringify(pending));
  return errs;
}

interface Mock {
  url: string;
  seen: Msg[];
  close: () => void;
}

/** 起一个最小 HTTP 假服务,记录它收到的每个请求体。 */
function startServer(handler: (i: number, body: Msg) => { code: number; body: string }): Promise<Mock> {
  return new Promise((resolve) => {
    const seen: Msg[] = [];
    const server = createServer((req, res) => {
      let raw = "";
      req.on("data", (c) => {
        raw += c.toString();
      });
      req.on("end", () => {
        const parsed = JSON.parse(raw || "{}") as Msg;
        const i = seen.push(parsed) - 1;
        const r = handler(i, parsed);
        res.writeHead(r.code, { "Content-Type": "application/json" });
        res.end(r.body);
      });
    });
    server.listen(0, "127.0.0.1", () => {
      const port = (server.address() as AddressInfo).port;
      resolve({ url: "http://127.0.0.1:" + String(port), seen, close: () => server.close() });
    });
  });
}

// ---------------- Schema / 工具 ----------------

test("tool schemas 完整且禁止额外字段", () => {
  const schemas = toolSchemas();
  assert.equal(schemas.length, 3);
  const names = schemas.map((s) => (s.function as Record<string, unknown>).name);
  assert.deepEqual(names, ["tool_bash", "tool_read", "tool_write"]);
  for (const s of schemas) {
    const p = (s.function as Record<string, unknown>).parameters as Record<string, unknown>;
    assert.equal(p.additionalProperties, false);
    assert.equal(p.type, "object");
  }
});

test("toolBash 汇总 stdout/stderr/退出码", async () => {
  const got = await toolBash("echo hello-stdout; echo oops >&2; exit 3", 10);
  assert.equal(got, "hello-stdout\n\n[stderr]\noops\n\n[exit:3]");
});

test("toolBash 空输出占位", async () => {
  assert.equal(await toolBash("true", 10), "(无输出)");
});

test("toolBash 超时文案与 Python 一致", async () => {
  assert.equal(await toolBash("sleep 5", 0.3), "[错误]命令超时(>0.3s)");
  assert.equal(await toolBash("sleep 5", 2), "[错误]命令超时(>2s)");
});

test("读取不存在的文件是工具错误", async () => {
  const fp = join(tmp("missing"), "definitely-missing");
  const got = await runTool(tc("c", "tool_read", JSON.stringify({ fp })), cfg("http://127.0.0.1:1"));
  assert.match(got, /^\[错误\]工具 tool_read 调用失败:/);
});

test("toolWrite 自动建父目录并按字符计数", async () => {
  const fp = join(tmp("write"), "a", "b", "c.txt");
  const args = JSON.parse(JSON.stringify({ fp, data: "中文abc" })) as Record<string, unknown>;
  assert.equal(await callTool("tool_write", args, cfg("http://127.0.0.1:1")), "OK—写入5字符");
  assert.equal(readFileSync(fp, "utf8"), "中文abc");
});

test("未知工具与非法参数都转成工具错误", async () => {
  const c = cfg("http://127.0.0.1:1");
  const unknown = await runTool(tc("c", "nope", "{}"), c);
  assert.match(unknown, /^\[错误\]工具 nope 调用失败:LookupError: 未知工具:nope$/);

  const badJson = await runTool(tc("c", "tool_bash", "not-json"), c);
  assert.match(badJson, /^\[错误\]工具 tool_bash 调用失败:JSONDecodeError:/);

  const notObject = await runTool(tc("c", "tool_bash", "[1,2]"), c);
  assert.match(notObject, /必须是 JSON 对象,收到 list/);

  const emptyArgs = await runTool(tc("c", "tool_bash", ""), c);
  assert.match(emptyArgs, /^\[错误\]工具 tool_bash 调用失败:TypeError:/);
});

test("截断按字符而不是 UTF-16/字节", async () => {
  const fp = join(tmp("trunc"), "big.txt");
  writeFileSync(fp, "中".repeat(20000), "utf8");
  const got = await runTool(tc("c", "tool_read", JSON.stringify({ fp })), cfg("http://127.0.0.1:1"));
  assert.ok(got.endsWith("[已截断,原始 20000 字符]"), "应以截断后缀结尾");
  const head = got.split("\n…[已截断")[0];
  assert.equal([...head].length, 16000);
});

// ---------------- 历史裁剪 ----------------

test("trim 保留 system 且不拆散配对", () => {
  const ctx: Msg[] = [{ role: "system" }];
  for (let i = 0; i < 10; i++) {
    ctx.push({ role: "user", content: "u" });
    ctx.push(msgCalls([tc("c" + String(i), "tool_bash", "{}")]));
    ctx.push({ role: "tool", tool_call_id: "c" + String(i), content: "ok" });
  }
  const out = trim(ctx, 4);
  assert.equal(out[0].role, "system");
  assert.notEqual(out[1].role, "tool");
  assert.ok(out.length <= 4 + 3, "裁剪后 " + String(out.length) + " 条");
  assert.deepEqual(legality(out), []);
});

test("trim 对短历史不改动", () => {
  const ctx: Msg[] = [{ role: "system" }, { role: "user", content: "x" }];
  assert.equal(trim(ctx, 40).length, 2);
});

test("trim 裁剪点落在连续 tool 结果上不越界", () => {
  const ctx: Msg[] = [{ role: "system" }];
  for (let i = 0; i < 5; i++) ctx.push({ role: "tool", tool_call_id: "x", content: "r" });
  const out = trim(ctx, 1);
  assert.equal(out[0].role, "system");
});

// ---------------- HTTP ----------------

test("chat 保留 HTTP 错误响应体", async () => {
  const srv = await startServer(() => ({ code: 400, body: '{"error":{"message":"boom-detail"}}' }));
  try {
    await assert.rejects(chat(cfg(srv.url), [{ role: "system" }]), (e: Error) => {
      assert.match(e.message, /^HTTP 400 Bad Request:/);
      assert.match(e.message, /boom-detail/);
      return true;
    });
  } finally {
    srv.close();
  }
});

test("chat 网络错误归类为网络错误", async () => {
  await assert.rejects(chat(cfg("http://127.0.0.1:1"), []), (e: Error) => {
    assert.match(e.message, /^网络错误:/);
    return true;
  });
});

// ---------------- 端到端 ----------------

test("agentLoop 端到端:历史始终合法且错误调用也配对", async () => {
  const dir = tmp("e2e");
  const outFp = join(dir, "o.txt");
  const srv = await startServer((_i, body) => {
    const msgs = (body.messages ?? []) as Msg[];
    const n = msgs.filter((m) => m.role === "assistant" && m.tool_calls !== undefined).length;
    if (n === 0) {
      return {
        code: 200,
        body: response(
          msgCalls([
            tc("call_1", "tool_bash", '{"cmd":"echo hi"}'),
            tc("call_2", "nope", "{}"),
            tc("call_3", "tool_bash", "not-json"),
            tc("call_4", "tool_write", JSON.stringify({ fp: outFp, data: "x" })),
          ]),
        ),
      };
    }
    if (n === 1) {
      return { code: 200, body: response(msgCalls([tc("call_5", "tool_read", JSON.stringify({ fp: outFp }))])) };
    }
    return { code: 200, body: response(msgText("ALL-OK")) };
  });
  try {
    const ctx: Msg[] = [{ role: "system", content: "sys" }];
    assert.equal(await agentLoop(cfg(srv.url), "任务", ctx), "ALL-OK");
    assert.equal(srv.seen.length, 3);
    srv.seen.forEach((b, i) => {
      assert.deepEqual(legality((b.messages ?? []) as Msg[]), [], "第 " + String(i) + " 次请求历史非法");
    });
    assert.equal(ctx.filter((m) => m.role === "tool").length, 5);
    assert.deepEqual(legality(ctx), []);
  } finally {
    srv.close();
  }
});

test("agentLoop 区分上下文超长", async () => {
  const srv = await startServer(() => ({
    code: 400,
    body: '{"error":{"message":"maximum context length is 8192 tokens"}}',
  }));
  try {
    const ctx: Msg[] = [{ role: "system", content: "sys" }];
    const got = await agentLoop(cfg(srv.url), "任务", ctx);
    assert.match(got, /^\[错误\]上下文过长,请重开会话:/);
  } finally {
    srv.close();
  }
});

test("agentLoop 普通失败归类为 API失败", async () => {
  const srv = await startServer(() => ({ code: 401, body: '{"error":{"message":"bad key"}}' }));
  try {
    const ctx: Msg[] = [{ role: "system", content: "sys" }];
    const got = await agentLoop(cfg(srv.url), "任务", ctx);
    assert.match(got, /^\[错误\]API失败:HTTP 401/);
  } finally {
    srv.close();
  }
});

test("agentLoop 到最大轮次会中断", async () => {
  const srv = await startServer(() => ({
    code: 200,
    body: response(msgCalls([tc("c", "tool_bash", '{"cmd":"true"}')])),
  }));
  try {
    const c = cfg(srv.url);
    c.maxTurns = 3;
    const ctx: Msg[] = [{ role: "system", content: "sys" }];
    const got = await agentLoop(c, "任务", ctx);
    assert.match(got, /达到最大轮次\(3\)/);
    assert.equal(srv.seen.length, 3);
    assert.deepEqual(legality(ctx), []);
  } finally {
    srv.close();
  }
});

test("长工具循环下历史始终有界", async () => {
  const srv = await startServer((i) => ({
    code: 200,
    body: response(i >= 24 ? msgText("TRIM-OK") : msgCalls([tc("c" + String(i), "tool_bash", '{"cmd":"true"}')])),
  }));
  try {
    const c = cfg(srv.url);
    c.maxHistory = 6;
    c.maxTurns = 40;
    const ctx: Msg[] = [{ role: "system", content: "sys" }];
    assert.equal(await agentLoop(c, "任务", ctx), "TRIM-OK");
    assert.equal(srv.seen.length, 25);
    let peak = 0;
    for (const b of srv.seen) {
      const msgs = (b.messages ?? []) as Msg[];
      peak = Math.max(peak, msgs.length);
      assert.deepEqual(legality(msgs), []);
    }
    assert.ok(peak <= 9, "历史未被有界裁剪,峰值 " + String(peak) + " 条");
  } finally {
    srv.close();
  }
});

// ---------------- 配置 ----------------

test("configFromEnv 读取与默认值", () => {
  const keys = [
    "LLM_API_KEY",
    "DEEPSEEK_API_KEY",
    "OPENAI_API_KEY",
    "LLM_MODEL",
    "LLM_MAX_HISTORY",
    "LLM_BASE_URL",
    "LLM_TOOL_TIMEOUT",
  ];
  const saved = keys.map((k) => [k, process.env[k]] as const);
  try {
    delete process.env.LLM_API_KEY;
    delete process.env.OPENAI_API_KEY;
    process.env.DEEPSEEK_API_KEY = "dk";
    process.env.LLM_MODEL = "m1";
    process.env.LLM_MAX_HISTORY = "0";
    process.env.LLM_BASE_URL = "http://x/v1/";
    process.env.LLM_TOOL_TIMEOUT = "2.5";
    const c = configFromEnv();
    assert.equal(c.apiKey, "dk");
    assert.equal(c.model, "m1");
    assert.equal(c.maxHistory, 1);
    assert.equal(c.baseUrl, "http://x/v1");
    assert.equal(c.toolTimeout, 2.5);
    assert.equal(c.maxTurns, 15);
    assert.equal(c.maxToolChars, 16000);
  } finally {
    for (const [k, v] of saved) {
      if (v === undefined) delete process.env[k];
      else process.env[k] = v;
    }
  }
});
