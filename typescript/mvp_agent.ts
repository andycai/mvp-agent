// mvp_agent.ts —— 100 行以内的 LLM Agent(Node 标准库:内置 fetch + child_process + readline)。为压进 100 行采用紧凑排版,与 python/mvp_agent.py 风格一致。用法:export LLM_API_KEY=sk-... && node mvp_agent.ts [任务]
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import { createInterface } from "node:readline";
import { pathToFileURL } from "node:url";

export const SYS = "你是AI助手,可用工具:\n1. tool_bash — 执行系统命令(bash)\n2. tool_read — 读取文件\n3. tool_write — 写入文件\n\n请逐步思考。当需要与文件系统交互、运行命令或执行代码等等时调用工具,完成后直接回复。";
const env = (k: string, d: string): string => process.env[k] || d;
const numOf = (k: string, d: number): number => (process.env[k] && Number.isFinite(Number(process.env[k])) ? Number(process.env[k]) : d); // 注意 0 是合法值
export const load = () => ({ key: process.env.LLM_API_KEY || process.env.DEEPSEEK_API_KEY || process.env.OPENAI_API_KEY || "", url: env("LLM_BASE_URL", "https://api.deepseek.com/v1").replace(/\/+$/, ""), model: env("LLM_MODEL", "deepseek-chat"), turns: numOf("LLM_MAX_TURNS", 15), timeout: numOf("LLM_TIMEOUT", 120), toolTO: numOf("LLM_TOOL_TIMEOUT", 60), maxChars: numOf("LLM_MAX_TOOL_CHARS", 16000), maxHist: Math.max(1, numOf("LLM_MAX_HISTORY", 40)) });
export const cfg = load();

const prop = (d: string) => ({ type: "string", description: d });
const spec = (name: string, desc: string, props: any, req: string[]) => ({ type: "function", function: { name, description: desc, parameters: { type: "object", properties: props, required: req, additionalProperties: false } } });
export const schemas = () => [
  spec("tool_bash", "执行系统命令", { cmd: prop("命令") }, ["cmd"]),
  spec("tool_read", "读取文件", { fp: prop("文件路径") }, ["fp"]),
  spec("tool_write", "写入文件", { fp: prop("文件路径"), data: prop("内容") }, ["fp", "data"])];
export const SHELL = existsSync("/bin/bash") ? "/bin/bash" : "sh";

export const bash = (cmd: string): Promise<string> => new Promise((done) => {
  const p = spawn(SHELL, ["-c", cmd]); let out = "", err = "", ended = false;
  const fin = (v: string) => { if (!ended) { ended = true; clearTimeout(timer); done(v); } };
  const timer = setTimeout(() => { p.kill("SIGKILL"); fin("[错误]命令超时(>" + cfg.toolTO + "s)"); }, cfg.toolTO * 1000);
  p.stdout?.on("data", (b: Buffer) => { out += b; }); p.stderr?.on("data", (b: Buffer) => { err += b; });
  p.on("error", (e: Error) => fin("[错误]" + e.message));
  p.on("close", (code: number) => { const s = (out + (err ? "\n[stderr]\n" + err : "") + (code ? "\n[exit:" + code + "]" : "")).trim(); fin(s || "(无输出)"); });
});
export const readF = (fp: string): string => readFileSync(fp, "utf8");
export const writeF = (fp: string, data: string): string => { mkdirSync(dirname(fp) || ".", { recursive: true }); writeFileSync(fp, data, "utf8"); return "OK—写入" + [...data].length + "字符"; };

type Args = Record<string, any>;
const str = (a: Args, k: string): string => (typeof a[k] === "string" ? a[k] : "");
const fns: Record<string, (a: Args) => Promise<string> | string> = {
  tool_bash: (a) => bash(str(a, "cmd")),
  tool_read: (a) => readF(str(a, "fp")),
  tool_write: (a) => writeF(str(a, "fp"), str(a, "data")),
};
export const runTool = async (tc: any): Promise<string> => {
  const f = (tc && tc.function) || {}; const name = typeof f.name === "string" ? f.name : "";
  let out: string;
  try {
    const raw = typeof f.arguments === "string" && f.arguments.trim() ? f.arguments : "{}";
    const args = JSON.parse(raw);
    if (typeof args !== "object" || args === null || Array.isArray(args)) throw new Error("arguments 必须是 JSON 对象");
    const fn = fns[name]; if (!fn) throw new Error("未知工具:" + name);
    out = await fn(args);
  } catch (e: any) { out = "[错误]工具 " + (name || "?") + " 调用失败:" + (e && e.name ? e.name : "Error") + ": " + (e && e.message ? e.message : e); }
  const chars = [...out];
  return chars.slice(0, cfg.maxChars).join("") + (chars.length > cfg.maxChars ? "\n…[已截断,原始 " + chars.length + " 字符]" : "");
};
export const trim = (ctx: any[]): any[] => {
  if (ctx.length <= cfg.maxHist + 1) return ctx;
  let cut = ctx.length - cfg.maxHist;
  while (cut < ctx.length && ctx[cut].role === "tool") cut++;
  return [ctx[0], ...ctx.slice(cut)];
};
export const chat = async (msgs: any[]): Promise<any> => {
  let res: Response;
  try {
    res = await fetch(cfg.url + "/chat/completions", { method: "POST", headers: { "Content-Type": "application/json; charset=utf-8", Authorization: "Bearer " + cfg.key, "User-Agent": "mvp-agent/1.0" }, body: JSON.stringify({ model: cfg.model, messages: msgs, tools: schemas() }), signal: AbortSignal.timeout(cfg.timeout * 1000) });
  } catch (e: any) { throw new Error("网络错误:" + (e && e.message ? e.message : e)); }
  const text = await res.text();
  if (!res.ok) throw new Error("HTTP " + res.status + " " + res.statusText + ": " + [...text].slice(0, 1000).join(""));
  let root: any; try { root = JSON.parse(text); } catch (e: any) { throw new Error("网络错误:" + e.message); }
  const m = root.choices && root.choices[0] && root.choices[0].message;
  if (!m) throw new Error("网络错误:响应缺少 choices[0].message");
  return m;
};
export const agentLoop = async (ctx: any[], input: string): Promise<string> => {
  ctx.push({ role: "user", content: input });
  for (let i = 0; i < cfg.turns; i++) {
    const t = trim(ctx); if (t !== ctx) { ctx.length = 0; ctx.push(...t); }
    let m: any;
    try { m = await chat(ctx); } catch (e: any) { const s = e && e.message ? e.message : String(e); const l = s.toLowerCase(); return "[错误]" + (l.includes("context") || l.includes("token") ? "上下文过长,请重开会话" : "API失败") + ":" + s; }
    const a: any = {}; for (const k of ["role", "content", "tool_calls"]) if (k in m) a[k] = m[k]; ctx.push(a);
    const calls = Array.isArray(m.tool_calls) ? m.tool_calls : [];
    if (!calls.length) return typeof m.content === "string" ? m.content : "";
    for (const tc of calls) { console.error("[tool_call]" + (tc.function ? tc.function.name : "") + "(" + (tc.function ? tc.function.arguments : "") + ")"); ctx.push({ role: "tool", tool_call_id: tc.id, content: await runTool(tc) }); }
  }
  return "[Agent]防止死循环,达到最大轮次(" + cfg.turns + "),中断";
};
export const main = async (): Promise<void> => {
  if (!cfg.key) { console.error("错误:请设置LLM_API_KEY"); process.exit(1); }
  const ctx: any[] = [{ role: "system", content: SYS }];
  const args = process.argv.slice(2);
  if (args.length) { console.log("助手>" + (await agentLoop(ctx, args.join(" ")))); return; }
  console.error("智能助手 " + cfg.model + " 就绪。输入消息(或'exit'退出)。\n");
  const rl = createInterface({ input: process.stdin, output: process.stdout, prompt: "用户> " });
  rl.prompt();
  for await (const line of rl) {
    const s = line.trim();
    if (s === "exit" || s === "quit") break;
    if (s) { console.error("处理请求:" + s); console.log("\n助手>" + (await agentLoop(ctx, s)) + "\n"); }
    rl.prompt();
  }
  console.log("\n再见。");
};
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) await main();
