#!/usr/bin/env python3
"""mvp_agent.py —— 100 行以内的 LLM Agent(参考 agent_100_line/agent.py)。
仅用标准库(不依赖 openai 库):urllib 调用 OpenAI 兼容的 /chat/completions。
用法:export LLM_API_KEY=sk-... && python mvp_agent.py [任务](环境变量见 README.md)"""
import json, logging, os, subprocess, sys, urllib.error, urllib.request
SYS_PROMPT = """你是AI助手,可用工具:
1. tool_bash — 执行系统命令(bash)
2. tool_read — 读取文件
3. tool_write — 写入文件

请逐步思考。当需要与文件系统交互、运行命令或执行代码等等时调用工具,完成后直接回复。"""
env = os.environ.get
API_KEY = env("LLM_API_KEY") or env("DEEPSEEK_API_KEY") or env("OPENAI_API_KEY") or ""
BASE_URL = env("LLM_BASE_URL", "https://api.deepseek.com/v1").rstrip("/")
MODEL = env("LLM_MODEL", "deepseek-chat")
MAX_TURNS, REQUEST_TIMEOUT = int(env("LLM_MAX_TURNS", 15)), float(env("LLM_TIMEOUT", 120))
TOOL_TIMEOUT, MAX_TOOL_CHARS = float(env("LLM_TOOL_TIMEOUT", 60)), int(env("LLM_MAX_TOOL_CHARS", 16000))
MAX_HISTORY = max(1, int(env("LLM_MAX_HISTORY", 40)))  # 保留的最近消息条数,超出则裁剪最旧历史
BASH = "/bin/bash" if os.path.exists("/bin/bash") else None  # tool_bash 尽量真的用 bash
log = logging.getLogger("mvp_agent"); _P = lambda t, d="": {"type": t, "description": d}

def _spec(n, d, props, req):
    return {"type": "function", "function": {"name": n, "description": d, "parameters": {
        "type": "object", "properties": props, "required": req, "additionalProperties": False}}}
TOOL_SCHEMAS = [
    _spec("tool_bash", "执行系统命令", {"cmd": _P("string", "命令")}, ["cmd"]),
    _spec("tool_read", "读取文件", {"fp": _P("string", "文件路径")}, ["fp"]),
    _spec("tool_write", "写入文件", {"fp": _P("string", "文件路径"), "data": _P("string", "内容")}, ["fp", "data"])]
def tool_bash(cmd):
    try:
        r = subprocess.run(cmd, shell=True, executable=BASH, capture_output=True, text=True, timeout=TOOL_TIMEOUT)
        o = r.stdout + (f"\n[stderr]\n{r.stderr}" if r.stderr else "")
        return (o + (f"\n[exit:{r.returncode}]" if r.returncode else "")).strip() or "(无输出)"
    except subprocess.TimeoutExpired: return f"[错误]命令超时(>{TOOL_TIMEOUT:g}s)"
    except Exception as e: return f"[错误]{e}"
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
TOOL_FUNCS = {"tool_bash": tool_bash, "tool_read": tool_read, "tool_write": tool_write}
def _trim(ctx):
    """裁剪过旧历史,但绝不把 tool 响应与它的 tool_calls 拆开(否则后续请求 400)。"""
    if len(ctx) <= MAX_HISTORY + 1: return ctx
    cut = len(ctx) - MAX_HISTORY
    while cut < len(ctx) and ctx[cut]["role"] == "tool": cut += 1
    return ctx[:1] + ctx[cut:]
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
        ctx[:] = _trim(ctx)  # 安全边界:此刻上一轮 tool_calls 都已有配对响应
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
    if len(sys.argv) > 1:  # 非交互:python mvp_agent.py "任务"
        return print(f"助手>{agent_loop(' '.join(sys.argv[1:]), ctx)}")
    log.info("智能助手 %s 就绪。输入消息(或'exit'退出)。\n", MODEL)
    while True:
        try: inp = input("用户> ").strip()
        except (EOFError, KeyboardInterrupt): print("\n再见。"); break
        if not inp: continue
        if inp.lower() in ("exit", "quit"): print("再见。"); break
        log.info("处理请求:%s", inp); print(f"\n助手>{agent_loop(inp, ctx)}\n")
if __name__ == "__main__": main()
