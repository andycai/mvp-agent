// mvp_agent.ts —— 极简 LLM Agent(Node 标准库实现,零第三方依赖)。
// Node 24 原生支持直接运行 .ts(类型擦除),无需构建步骤;本地导入需带 .ts 扩展名。
// 用法:export LLM_API_KEY=sk-... && node mvp_agent.ts [任务](环境变量见 README.md)
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import { createInterface } from "node:readline";
import { pathToFileURL } from "node:url";

export const SYS_PROMPT = [
  "你是AI助手,可用工具:",
  "1. tool_bash — 执行系统命令(bash)",
  "2. tool_read — 读取文件",
  "3. tool_write — 写入文件",
  "",
  "请逐步思考。需要与文件系统交互、运行命令或执行代码等等时调用工具,完成后直接回复。",
].join("\n");

export type Msg = Record<string, unknown>;

export interface Config {
  apiKey: string;
  baseUrl: string;
  model: string;
  maxTurns: number;
  requestTimeout: number;
  toolTimeout: number;
  maxToolChars: number;
  maxHistory: number;
}

function envNum(key: string, def: number): number {
  const raw = process.env[key];
  if (raw === undefined || raw === "") return def;
  const v = Number(raw);
  return Number.isFinite(v) ? v : def;
}

export function configFromEnv(): Config {
  const env = (k: string): string | undefined => (process.env[k] ? process.env[k] : undefined);
  return {
    apiKey: env("LLM_API_KEY") ?? env("DEEPSEEK_API_KEY") ?? env("OPENAI_API_KEY") ?? "",
    baseUrl: (env("LLM_BASE_URL") ?? "https://api.deepseek.com/v1").replace(/\/+$/, ""),
    model: env("LLM_MODEL") ?? "deepseek-chat",
    maxTurns: envNum("LLM_MAX_TURNS", 15),
    requestTimeout: envNum("LLM_TIMEOUT", 120),
    toolTimeout: envNum("LLM_TOOL_TIMEOUT", 60),
    maxToolChars: envNum("LLM_MAX_TOOL_CHARS", 16000),
    maxHistory: Math.max(1, envNum("LLM_MAX_HISTORY", 40)),
  };
}

/** 与 Python type(e).__name__ 对齐的错误分类,只影响 [错误] 文案的中间一段。 */
export class ToolErr extends Error {
  kind: string;
  constructor(kind: string, message: string) {
    super(message);
    this.kind = kind;
  }
}

function typeName(v: unknown): string {
  if (v === null) return "NoneType";
  if (Array.isArray(v)) return "list";
  switch (typeof v) {
    case "string":
      return "str";
    case "number":
      return Number.isInteger(v) ? "int" : "float";
    case "boolean":
      return "bool";
    case "object":
      return "dict";
    default:
      return "unknown";
  }
}

function prop(t: string, d: string): Record<string, string> {
  return { type: t, description: d };
}

function spec(
  name: string,
  desc: string,
  props: Record<string, unknown>,
  req: string[],
): Record<string, unknown> {
  return {
    type: "function",
    function: {
      name,
      description: desc,
      parameters: { type: "object", properties: props, required: req, additionalProperties: false },
    },
  };
}

export function toolSchemas(): Record<string, unknown>[] {
  return [
    spec("tool_bash", "执行系统命令", { cmd: prop("string", "命令") }, ["cmd"]),
    spec("tool_read", "读取文件", { fp: prop("string", "文件路径") }, ["fp"]),
    spec(
      "tool_write",
      "写入文件",
      { fp: prop("string", "文件路径"), data: prop("string", "内容") },
      ["fp", "data"],
    ),
  ];
}

/** tool_bash 尽量真的用 bash(Python 版同样优先 /bin/bash)。 */
export const BASH = existsSync("/bin/bash") ? "/bin/bash" : "sh";

export function toolBash(cmd: string, timeout: number): Promise<string> {
  return new Promise((resolve) => {
    const child = spawn(BASH, ["-c", cmd], { stdio: ["ignore", "pipe", "pipe"] });
    let out = "";
    let err = "";
    let settled = false;
    const finish = (v: string): void => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve(v);
    };
    const timer = setTimeout(() => {
      child.kill("SIGKILL");
      finish("[错误]命令超时(>" + String(timeout) + "s)");
    }, Math.max(0, timeout) * 1000);
    child.stdout?.on("data", (d: Buffer) => {
      out += d.toString();
    });
    child.stderr?.on("data", (d: Buffer) => {
      err += d.toString();
    });
    child.on("error", (e: Error) => finish("[错误]" + e.message));
    child.on("close", (code: number | null) => {
      let s = out;
      if (err) s += "\n[stderr]\n" + err;
      if (code) s += "\n[exit:" + String(code) + "]";
      s = s.trim();
      finish(s || "(无输出)");
    });
  });
}

export function toolRead(fp: string): string {
  // utf8 读取,非法字节按 U+FFFD 替换,与 Python errors="replace" 对齐。
  return readFileSync(fp, "utf8");
}

export function toolWrite(fp: string, data: string): string {
  mkdirSync(dirname(fp) || ".", { recursive: true });
  writeFileSync(fp, data, "utf8");
  return "OK—写入" + String([...data].length) + "字符";
}

function need(args: Record<string, unknown>, k: string): string {
  const v = args[k];
  if (typeof v === "string") return v;
  if (v === undefined) throw new ToolErr("TypeError", k + "() missing 1 required argument");
  throw new ToolErr("TypeError", k + "() argument must be str, not " + typeName(v));
}

export async function callTool(
  name: string,
  args: Record<string, unknown>,
  cfg: Config,
): Promise<string> {
  switch (name) {
    case "tool_bash":
      return toolBash(need(args, "cmd"), cfg.toolTimeout);
    case "tool_read":
      return toolRead(need(args, "fp"));
    case "tool_write":
      return toolWrite(need(args, "fp"), need(args, "data"));
    default:
      throw new ToolErr("LookupError", "未知工具:" + name);
  }
}

/** 执行单个 tool_call:任何异常都转成一条 [错误] 结果,保证 tool_call 有配对响应。 */
export async function runTool(tc: Msg, cfg: Config): Promise<string> {
  const fn = (tc.function ?? {}) as Record<string, unknown>;
  const name = typeof fn.name === "string" ? fn.name : "";
  let out: string;
  try {
    const rawArg = fn.arguments;
    const raw =
      rawArg === undefined || rawArg === null || rawArg === "" ? "{}" : rawArg;
    if (typeof raw !== "string") {
      throw new ToolErr("TypeError", "the JSON object must be str, not " + typeName(raw));
    }
    let parsed: unknown;
    try {
      parsed = JSON.parse(raw);
    } catch (e) {
      throw new ToolErr("JSONDecodeError", e instanceof Error ? e.message : String(e));
    }
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
      throw new ToolErr("ValueError", "arguments 必须是 JSON 对象,收到 " + typeName(parsed));
    }
    out = await callTool(name, parsed as Record<string, unknown>, cfg);
  } catch (e) {
    const kind = e instanceof ToolErr ? e.kind : e instanceof Error ? e.name : "Error";
    const msg = e instanceof Error ? e.message : String(e);
    out = "[错误]工具 " + (name || "?") + " 调用失败:" + kind + ": " + msg;
  }
  const chars = [...out];
  if (chars.length <= cfg.maxToolChars) return out;
  return chars.slice(0, cfg.maxToolChars).join("") + "\n…[已截断,原始 " + String(chars.length) + " 字符]";
}

/** 裁剪过旧历史,但绝不把 tool 响应与它的 tool_calls 拆开(否则后续请求 400)。 */
export function trim(ctx: Msg[], maxHistory: number): Msg[] {
  if (ctx.length <= maxHistory + 1) return ctx;
  let cut = ctx.length - maxHistory;
  while (cut < ctx.length && ctx[cut].role === "tool") cut++;
  return [ctx[0], ...ctx.slice(cut)];
}

const REASONS: Record<number, string> = {
  400: "Bad Request",
  401: "Unauthorized",
  403: "Forbidden",
  404: "Not Found",
  408: "Request Timeout",
  413: "Payload Too Large",
  429: "Too Many Requests",
  500: "Internal Server Error",
  502: "Bad Gateway",
  503: "Service Unavailable",
  504: "Gateway Timeout",
};

/** POST {BASE_URL}/chat/completions,返回 choices[0].message。 */
export async function chat(cfg: Config, messages: Msg[]): Promise<Msg> {
  const body = JSON.stringify({ model: cfg.model, messages, tools: toolSchemas() });
  let res: Response;
  try {
    res = await fetch(cfg.baseUrl + "/chat/completions", {
      method: "POST",
      headers: {
        "Content-Type": "application/json; charset=utf-8",
        Authorization: "Bearer " + cfg.apiKey,
        "User-Agent": "mvp-agent/1.0",
      },
      body,
      signal: AbortSignal.timeout(cfg.requestTimeout * 1000),
    });
  } catch (e) {
    throw new Error("网络错误:" + (e instanceof Error ? e.message : String(e)));
  }
  const text = await res.text();
  if (!res.ok) {
    const snippet = [...text].slice(0, 1000).join("");
    throw new Error(
      "HTTP " + String(res.status) + " " + (res.statusText || REASONS[res.status] || "") + ": " + snippet,
    );
  }
  let root: { choices?: { message?: Msg }[] };
  try {
    root = JSON.parse(text) as { choices?: { message?: Msg }[] };
  } catch (e) {
    throw new Error("网络错误:响应不是合法 JSON:" + (e instanceof Error ? e.message : String(e)));
  }
  const msg = root.choices?.[0]?.message;
  if (!msg) throw new Error("网络错误:响应缺少 choices[0].message");
  return msg;
}

/** 主循环:追加 user,最多 MAX_TURNS 轮;**每次请求前**裁剪历史。 */
export async function agentLoop(cfg: Config, input: string, ctx: Msg[]): Promise<string> {
  ctx.push({ role: "user", content: input });
  for (let i = 0; i < cfg.maxTurns; i++) {
    // 安全边界:此刻上一轮 tool_calls 都已有配对响应。
    const trimmed = trim(ctx, cfg.maxHistory);
    if (trimmed !== ctx) {
      ctx.length = 0;
      ctx.push(...trimmed);
    }
    let m: Msg;
    try {
      m = await chat(cfg, ctx);
    } catch (e) {
      const s = e instanceof Error ? e.message : String(e);
      const low = s.toLowerCase();
      const kind =
        low.includes("context") || low.includes("token") ? "上下文过长,请重开会话" : "API失败";
      return "[错误]" + kind + ":" + s;
    }
    const assistant: Msg = {};
    for (const k of ["role", "content", "tool_calls"]) {
      if (k in m) assistant[k] = m[k];
    }
    ctx.push(assistant);
    const calls = Array.isArray(m.tool_calls) ? (m.tool_calls as Msg[]) : [];
    if (calls.length === 0) return typeof m.content === "string" ? m.content : "";
    for (const tc of calls) {
      const fn = (tc.function ?? {}) as Record<string, unknown>;
      console.error("[tool_call]" + String(fn.name) + "(" + String(fn.arguments) + ")");
      ctx.push({ role: "tool", tool_call_id: tc.id, content: await runTool(tc, cfg) });
    }
  }
  return "[Agent]防止死循环,达到最大轮次(" + String(cfg.maxTurns) + "),中断";
}

export async function main(): Promise<void> {
  const cfg = configFromEnv();
  if (!cfg.apiKey) {
    console.error("错误:请设置LLM_API_KEY");
    process.exit(1);
  }
  const ctx: Msg[] = [{ role: "system", content: SYS_PROMPT }];
  const args = process.argv.slice(2);
  if (args.length > 0) {
    // 非交互:node mvp_agent.ts "任务"
    console.log("助手>" + (await agentLoop(cfg, args.join(" "), ctx)));
    return;
  }
  console.error("智能助手 " + cfg.model + " 就绪。输入消息(或'exit'退出)。\n");
  let said = false;
  const rl = createInterface({ input: process.stdin, output: process.stdout, prompt: "用户> " });
  rl.prompt();
  for await (const line of rl) {
    const inp = line.trim();
    if (inp && (inp.toLowerCase() === "exit" || inp.toLowerCase() === "quit")) {
      console.log("再见。");
      said = true;
      break;
    }
    if (inp) {
      console.error("处理请求:" + inp);
      console.log("\n助手>" + (await agentLoop(cfg, inp, ctx)) + "\n");
    }
    rl.prompt();
  }
  if (!said) console.log("\n再见。");
}

const entry = process.argv[1] ? pathToFileURL(process.argv[1]).href : "";
if (import.meta.url === entry) await main();
