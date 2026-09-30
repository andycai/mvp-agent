#!/usr/bin/env python3
"""mvp_agent.py —— 100 行以内的 LLM Agent(参考 agent_100_line/agent.py)。
仅用标准库(不依赖 openai 库):urllib 调用 OpenAI 兼容的 /chat/completions。
用法:export LLM_API_KEY=sk-... && python mvp_agent.py(环境变量见 README.md)"""
import json, logging, os, subprocess, sys, tempfile, urllib.error, urllib.request
SYS_PROMPT = """你是AI助手,可用工具:
1. tool_exec — 执行系统命令
2. tool_read — 读取文件
3. tool_write — 写入文件
4. tool_python — 执行Python代码

请逐步思考。当需要与文件系统交互、运行命令或执行代码等等时调用工具,完成后直接回复。"""
env = os.environ.get
API_KEY = env("LLM_API_KEY") or env("DEEPSEEK_API_KEY") or env("OPENAI_API_KEY") or ""
BASE_URL = env("LLM_BASE_URL", "https://api.deepseek.com/v1").rstrip("/")
MODEL = env("LLM_MODEL", "deepseek-chat")
MAX_TURNS, REQUEST_TIMEOUT = int(env("LLM_MAX_TURNS", 15)), float(env("LLM_TIMEOUT", 120))
TOOL_TIMEOUT, MAX_TOOL_CHARS = float(env("LLM_TOOL_TIMEOUT", 60)), int(env("LLM_MAX_TOOL_CHARS", 16000))
log = logging.getLogger("mvp_agent"); _P = lambda t, d="": {"type": t, "description": d}
def _spec(n, d, props, req):
    return {"type": "function", "function": {"name": n, "description": d, "parameters": {
        "type": "object", "properties": props, "required": req, "additionalProperties": False}}}
TOOL_SCHEMAS = [
    _spec("tool_exec", "执行系统命令", {"cmd": _P("string", "命令")}, ["cmd"]),
    _spec("tool_read", "读取文件", {"fp": _P("string", "文件路径")}, ["fp"]),
    _spec("tool_write", "写入文件", {"fp": _P("string", "文件路径"), "data": _P("string", "内容")}, ["fp", "data"]),
    _spec("tool_python", "执行Python代码", {"code": _P("string", "Python代码")}, ["code"])]
def _run(cmd, shell=False):
    r = subprocess.run(cmd, shell=shell, capture_output=True, text=True, timeout=TOOL_TIMEOUT)
    o = r.stdout + (f"\n[stderr]\n{r.stderr}" if r.stderr else "")
    return (o + (f"\n[exit:{r.returncode}]" if r.returncode else "")).strip() or "(无输出)"
_err = lambda e: f"[错误]工具超时(>{TOOL_TIMEOUT:g}s)" if isinstance(e, subprocess.TimeoutExpired) else f"[错误]{e}"
def tool_exec(cmd):
    try: return _run(cmd, True)
    except Exception as e: return _err(e)
def tool_python(code):
    p = None
    try:
        with tempfile.NamedTemporaryFile("w", suffix=".py", delete=False, encoding="utf-8") as t:
            t.write(code); p = t.name
        return _run([sys.executable, p])
    except Exception as e: return _err(e)
    finally:
        try: os.unlink(p or "")
        except OSError: pass
def tool_read(fp):
    try:
        with open(fp, encoding="utf-8", errors="replace") as f: return f.read()
    except Exception as e: return f"[错误]{e}"
def tool_write(fp, data):
    try:
        os.makedirs(os.path.dirname(fp) or ".", exist_ok=True)
        with open(fp, "w", encoding="utf-8") as f: f.write(data)
        return f"OK—写入{len(data)}字符"
    except Exception as e: return f"[错误]{e}"
TOOL_FUNCS = {"tool_exec": tool_exec, "tool_read": tool_read, "tool_write": tool_write, "tool_python": tool_python}
def run_tool(tc):
    n = (tc.get("function") or {}).get("name") or ""
    try:
        args = json.loads((tc.get("function") or {}).get("arguments") or "{}")
        if not isinstance(args, dict): raise ValueError(f"arguments 必须是 JSON 对象,收到 {type(args).__name__}")
        if n not in TOOL_FUNCS: raise LookupError(f"未知工具:{n}")
        out = str(TOOL_FUNCS[n](**args))
    except Exception as e: out = f"[错误]工具 {n or '?'} 调用失败:{type(e).__name__}: {e}"
    return out if len(out) <= MAX_TOOL_CHARS else out[:MAX_TOOL_CHARS] + f"\n…[已截断,原始 {len(out)} 字符]"
def chat(messages):
    body = json.dumps({"model": MODEL, "messages": messages, "tools": TOOL_SCHEMAS}, ensure_ascii=False).encode()
    req = urllib.request.Request(f"{BASE_URL}/chat/completions", data=body, headers={
        "Content-Type": "application/json; charset=utf-8", "Authorization": f"Bearer {API_KEY}",
        "User-Agent": "mvp-agent/1.0"})
    try:
        with urllib.request.urlopen(req, timeout=REQUEST_TIMEOUT) as r: return json.load(r)["choices"][0]["message"]
    except urllib.error.HTTPError as e:
        raise RuntimeError(f"HTTP {e.code} {e.reason}: {e.read().decode('utf-8', 'replace')[:1000]}") from e
    except urllib.error.URLError as e: raise RuntimeError(f"网络错误:{e.reason}") from e
def agent_loop(inp, ctx):
    ctx.append({"role": "user", "content": inp})
    for _ in range(MAX_TURNS):
        try: m = chat(ctx)
        except Exception as e:
            s = str(e); kind = "上下文过长,请重开会话" if "context" in s.lower() or "token" in s.lower() else "API失败"
            return f"[错误]{kind}:{s}"
        ctx.append({k: m[k] for k in ("role", "content", "tool_calls") if k in m})
        if not m.get("tool_calls"): return m.get("content") or ""
        for tc in m["tool_calls"]:
            log.info("[tool_call]%s(%s)", (tc.get("function") or {}).get("name"), (tc.get("function") or {}).get("arguments"))
            ctx.append({"role": "tool", "tool_call_id": tc.get("id"), "content": run_tool(tc)})
    return f"[Agent]防止死循环,达到最大轮次({MAX_TURNS}),中断"
def main():
    logging.basicConfig(level=logging.INFO, format="%(message)s")
    if not API_KEY: sys.exit("错误:请设置LLM_API_KEY")
    ctx = [{"role": "system", "content": SYS_PROMPT}]
    log.info("智能助手 %s 就绪。输入消息(或'exit'退出)。\n", MODEL)
    while True:
        try: inp = input("用户> ").strip()
        except (EOFError, KeyboardInterrupt): print("\n再见。"); break
        if not inp: continue
        if inp.lower() in ("exit", "quit"): print("再见。"); break
        log.info("处理请求:%s", inp); print(f"\n助手>{agent_loop(inp, ctx)}\n")
if __name__ == "__main__": main()
