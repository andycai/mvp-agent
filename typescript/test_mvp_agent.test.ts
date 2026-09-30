// 离线测试:不联网、不需要真实 API Key。端到端用本地 http 服务(真实 fetch),并在每次请求上断言历史合法。
import { test } from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { agentLoop, bash, cfg, chat, load, runTool, schemas, trim } from "./mvp_agent.ts";

const saved = { ...cfg };
function setCfg(url: string): void {
  Object.assign(cfg, { key: "test-key", url: url.replace(/\/+$/, ""), model: "fake-model", turns: 15, timeout: 20, toolTO: 10, maxChars: 16000, maxHist: 40 });
}
function tmp(tag: string): string { return mkdtempSync(join(tmpdir(), "mvp-agent-" + tag + "-")); }
function tc(id: string, name: string, args: string): any { return { id: id, type: "function", function: { name: name, arguments: args } }; }
function msgCalls(calls: any[]): any { return { role: "assistant", content: null, tool_calls: calls }; }
function msgText(t: string): any { return { role: "assistant", content: t }; }
function response(m: any): string { return JSON.stringify({ choices: [{ message: m }] }); }

// 历史合法性:首条 system,每个 tool_call 恰好一条配对响应。
function legality(msgs: any[]): string[] {
  const errs: string[] = [];
  if (msgs.length === 0) return ["空 messages"];
  if (msgs[0].role !== "system") errs.push("首条不是 system");
  let pending: string[] = [];
  msgs.forEach((m, i) => {
    if (m.role === "assistant") {
      if (pending.length) errs.push("assistant 之前仍有未配对 " + JSON.stringify(pending));
      pending = [];
      const calls = Array.isArray(m.tool_calls) ? m.tool_calls : [];
      for (const c of calls) pending.push(String(c.id));
    } else if (m.role === "tool") {
      const id = String(m.tool_call_id);
      const at = pending.indexOf(id);
      if (at < 0) errs.push("孤儿 tool 响应 " + id + " @" + i); else pending.splice(at, 1);
    }
  });
  if (pending.length) errs.push("结尾未配对 " + JSON.stringify(pending));
  return errs;
}

interface Mock { url: string; seen: any[]; close: () => void }
function startServer(h: (i: number, body: any) => { code: number; body: string }): Promise<Mock> {
  return new Promise((resolve) => {
    const seen: any[] = [];
    const server = createServer((req, res) => {
      let raw = "";
      req.on("data", (c) => { raw += c.toString(); });
      req.on("end", () => {
        const body = JSON.parse(raw || "{}");
        const i = seen.push(body) - 1;
        const r = h(i, body);
        res.writeHead(r.code, { "Content-Type": "application/json" });
        res.end(r.body);
      });
    });
    server.listen(0, "127.0.0.1", () => {
      const port = (server.address() as AddressInfo).port;
      resolve({ url: "http://127.0.0.1:" + port, seen: seen, close: () => { server.close(); } });
    });
  });
}

test("tool schemas 完整且禁止额外字段", () => {
  const s = schemas();
  assert.equal(s.length, 3);
  assert.deepEqual(s.map((x) => x.function.name), ["tool_bash", "tool_read", "tool_write"]);
  for (const x of s) {
    assert.equal(x.function.parameters.additionalProperties, false);
    assert.equal(x.function.parameters.type, "object");
  }
});

test("bash 汇总 stdout/stderr/退出码", async () => {
  setCfg("http://127.0.0.1:1");
  assert.equal(await bash("echo hello-stdout; echo oops >&2; exit 3"), "hello-stdout\n\n[stderr]\noops\n\n[exit:3]");
});

test("bash 空输出占位与超时文案", async () => {
  setCfg("http://127.0.0.1:1");
  assert.equal(await bash("true"), "(无输出)");
  cfg.toolTO = 0.3;
  assert.equal(await bash("sleep 5"), "[错误]命令超时(>0.3s)");
  cfg.toolTO = 2;
  assert.equal(await bash("sleep 5"), "[错误]命令超时(>2s)");
});

test("bash 大输出不死锁", async () => {
  setCfg("http://127.0.0.1:1");
  const got = await bash("yes 中文 | head -c 200000");
  assert.ok([...got].length > 60000, "大输出被截断或死锁:" + [...got].length);
});

test("读取不存在的文件是工具错误", async () => {
  setCfg("http://127.0.0.1:1");
  const got = await runTool(tc("c", "tool_read", JSON.stringify({ fp: join(tmp("missing"), "nope") })));
  assert.match(got, /^\[错误\]工具 tool_read 调用失败:/);
});

test("tool_write 自动建父目录并按字符计数", async () => {
  setCfg("http://127.0.0.1:1");
  const fp = join(tmp("write"), "a", "b", "c.txt");
  assert.equal(await runTool(tc("c", "tool_write", JSON.stringify({ fp: fp, data: "中文abc" }))), "OK—写入5字符");
  assert.equal(readFileSync(fp, "utf8"), "中文abc");
});

test("未知工具与非法参数都转成工具错误", async () => {
  setCfg("http://127.0.0.1:1");
  const unknown = await runTool(tc("c", "nope", "{}"));
  assert.match(unknown, /^\[错误\]工具 nope 调用失败:.*未知工具:nope/);
  assert.match(await runTool(tc("c", "tool_bash", "not-json")), /^\[错误\]工具 tool_bash 调用失败:/);
  assert.match(await runTool(tc("c", "tool_bash", "[1,2]")), /^\[错误\]工具 tool_bash 调用失败:/);
});

test("截断按码点而不是 UTF-16 单元/字节", async () => {
  setCfg("http://127.0.0.1:1");
  const fp = join(tmp("trunc"), "big.txt");
  writeFileSync(fp, "中".repeat(20000), "utf8");
  const got = await runTool(tc("c", "tool_read", JSON.stringify({ fp: fp })));
  assert.ok(got.endsWith("[已截断,原始 20000 字符]"), "应以截断后缀结尾");
  const at = got.indexOf("…[已截断");
  assert.equal([...got.slice(0, at)].length - 1, 16000);
});

test("trim 保留 system 且不拆散配对", () => {
  setCfg("http://127.0.0.1:1");
  cfg.maxHist = 4;
  const ctx: any[] = [{ role: "system" }];
  for (let i = 0; i < 10; i++) {
    ctx.push({ role: "user", content: "u" }, msgCalls([tc("c" + i, "tool_bash", "{}")]), { role: "tool", tool_call_id: "c" + i, content: "ok" });
  }
  const out = trim(ctx);
  assert.equal(out[0].role, "system");
  assert.notEqual(out[1].role, "tool");
  assert.ok(out.length <= cfg.maxHist + 3, "裁剪后 " + out.length + " 条");
  assert.deepEqual(legality(out), []);
});

test("trim 对短历史不改动", () => {
  setCfg("http://127.0.0.1:1");
  assert.equal(trim([{ role: "system" }, { role: "user", content: "x" }]).length, 2);
});

test("trim 裁剪点落在连续 tool 结果上不越界", () => {
  setCfg("http://127.0.0.1:1");
  cfg.maxHist = 1;
  const ctx: any[] = [{ role: "system" }];
  for (let i = 0; i < 5; i++) ctx.push({ role: "tool", tool_call_id: "x", content: "r" });
  assert.equal(trim(ctx)[0].role, "system");
});

test("chat 保留 HTTP 错误响应体", async () => {
  const srv = await startServer(() => ({ code: 400, body: '{"error":{"message":"boom-detail"}}' }));
  try {
    setCfg(srv.url);
    await assert.rejects(chat([{ role: "system" }]), (e: Error) => {
      assert.match(e.message, /^HTTP 400 Bad Request:/);
      assert.match(e.message, /boom-detail/);
      return true;
    });
  } finally { srv.close(); }
});

test("chat 网络错误归类为网络错误", async () => {
  setCfg("http://127.0.0.1:1");
  await assert.rejects(chat([]), (e: Error) => {
    assert.match(e.message, /^网络错误:/);
    return true;
  });
});

test("agentLoop 端到端:历史始终合法且错误调用也配对", async () => {
  const out = join(tmp("e2e"), "o.txt");
  const srv = await startServer((_i, body) => {
    const n = (body.messages || []).filter((m: any) => m.role === "assistant" && m.tool_calls).length;
    if (n === 0) {
      return { code: 200, body: response(msgCalls([
        tc("call_1", "tool_bash", '{"cmd":"echo hi"}'),
        tc("call_2", "nope", "{}"),
        tc("call_3", "tool_bash", "not-json"),
        tc("call_4", "tool_write", JSON.stringify({ fp: out, data: "x" }))])) };
    }
    if (n === 1) return { code: 200, body: response(msgCalls([tc("call_5", "tool_read", JSON.stringify({ fp: out }))])) };
    return { code: 200, body: response(msgText("ALL-OK")) };
  });
  try {
    setCfg(srv.url);
    const ctx: any[] = [{ role: "system", content: "sys" }];
    assert.equal(await agentLoop(ctx, "任务"), "ALL-OK");
    assert.equal(srv.seen.length, 3);
    srv.seen.forEach((b, i) => assert.deepEqual(legality(b.messages), [], "第 " + i + " 次请求历史非法"));
    assert.equal(ctx.filter((m) => m.role === "tool").length, 5);
    assert.deepEqual(legality(ctx), []);
  } finally { srv.close(); }
});

test("agentLoop 区分上下文超长", async () => {
  const srv = await startServer(() => ({ code: 400, body: '{"error":{"message":"maximum context length is 8192 tokens"}}' }));
  try {
    setCfg(srv.url);
    const got = await agentLoop([{ role: "system", content: "sys" }], "任务");
    assert.match(got, /^\[错误\]上下文过长,请重开会话:/);
  } finally { srv.close(); }
});

test("agentLoop 普通失败归类为 API失败", async () => {
  const srv = await startServer(() => ({ code: 401, body: '{"error":{"message":"bad key"}}' }));
  try {
    setCfg(srv.url);
    assert.match(await agentLoop([{ role: "system", content: "sys" }], "任务"), /^\[错误\]API失败:HTTP 401/);
  } finally { srv.close(); }
});

test("agentLoop 到最大轮次会中断", async () => {
  const srv = await startServer(() => ({ code: 200, body: response(msgCalls([tc("c", "tool_bash", '{"cmd":"true"}')])) }));
  try {
    setCfg(srv.url);
    cfg.turns = 3;
    const ctx: any[] = [{ role: "system", content: "sys" }];
    assert.match(await agentLoop(ctx, "任务"), /达到最大轮次\(3\)/);
    assert.equal(srv.seen.length, 3);
    assert.deepEqual(legality(ctx), []);
  } finally { srv.close(); }
});

test("长工具循环下历史始终有界", async () => {
  const srv = await startServer((i) => ({ code: 200, body: response(i >= 24 ? msgText("TRIM-OK") : msgCalls([tc("c" + i, "tool_bash", '{"cmd":"true"}')])) }));
  try {
    setCfg(srv.url);
    cfg.maxHist = 6; cfg.turns = 40;
    const ctx: any[] = [{ role: "system", content: "sys" }];
    assert.equal(await agentLoop(ctx, "任务"), "TRIM-OK");
    assert.equal(srv.seen.length, 25);
    let peak = 0;
    for (const b of srv.seen) {
      assert.deepEqual(legality(b.messages), []);
      peak = Math.max(peak, b.messages.length);
    }
    assert.ok(peak <= 9, "历史未被有界裁剪,峰值 " + peak + " 条");
  } finally { srv.close(); }
});

test("load 读取环境变量与默认值", () => {
  const keys = ["LLM_API_KEY", "DEEPSEEK_API_KEY", "OPENAI_API_KEY", "LLM_MODEL", "LLM_MAX_HISTORY", "LLM_BASE_URL", "LLM_TOOL_TIMEOUT"];
  const before = keys.map((k) => [k, process.env[k]] as const);
  try {
    for (const k of keys) delete process.env[k];
    process.env.DEEPSEEK_API_KEY = "dk";
    process.env.LLM_MODEL = "m1";
    process.env.LLM_MAX_HISTORY = "0";
    process.env.LLM_BASE_URL = "http://x/v1/";
    process.env.LLM_TOOL_TIMEOUT = "2.5";
    const c = load();
    assert.equal(c.key, "dk");
    assert.equal(c.model, "m1");
    assert.equal(c.maxHist, 1);
    assert.equal(c.url, "http://x/v1");
    assert.equal(c.toolTO, 2.5);
    assert.equal(c.turns, 15);
  } finally {
    for (const [k, v] of before) { if (v === undefined) delete process.env[k]; else process.env[k] = v; }
    Object.assign(cfg, saved);
  }
});
