#!/usr/bin/env python3
"""mvp_agent.py —— 极简 LLM Agent(参考 agent_100_line/agent.py)。

仅用 Python 标准库(不依赖 openai 库):通过 urllib 调用 OpenAI 兼容的
/chat/completions 接口,工具的 function calling 由本地函数执行。

用法:
    export LLM_API_KEY=sk-...
    python mvp_agent.py
环境变量:
    LLM_API_KEY       必填,也接受 DEEPSEEK_API_KEY / OPENAI_API_KEY
    LLM_BASE_URL      默认 https://api.deepseek.com/v1
                      (其他兼容后端,如 https://api.scnet.cn/api/llm/v1)
    LLM_MODEL         默认 deepseek-chat
    LLM_MAX_TURNS     单轮最多工具循环次数,默认 15
    LLM_TIMEOUT       单次 API 请求超时(秒),默认 120
    LLM_TOOL_TIMEOUT  单个工具执行超时(秒),默认 60
    LLM_MAX_TOOL_CHARS  回填给模型的单个工具结果上限,默认 16000
"""
import json, logging, os, subprocess, sys, tempfile, urllib.error, urllib.request

SYS_PROMPT = """你是AI助手,可用工具:
1. tool_exec — 执行系统命令
2. tool_read — 读取文件
3. tool_write — 写入文件
4. tool_python — 执行Python代码

请逐步思考。当需要与文件系统交互、运行命令或执行代码等等时调用工具,完成后直接回复。"""

API_KEY = os.environ.get("LLM_API_KEY") or os.environ.get("DEEPSEEK_API_KEY") or os.environ.get("OPENAI_API_KEY", "")
BASE_URL = (os.environ.get("LLM_BASE_URL") or "https://api.deepseek.com/v1").rstrip("/")
MODEL = os.environ.get("LLM_MODEL", "deepseek-chat")
MAX_TURNS = int(os.environ.get("LLM_MAX_TURNS", "15"))
REQUEST_TIMEOUT = float(os.environ.get("LLM_TIMEOUT", "120"))
TOOL_TIMEOUT = float(os.environ.get("LLM_TOOL_TIMEOUT", "60"))
MAX_TOOL_CHARS = int(os.environ.get("LLM_MAX_TOOL_CHARS", "16000"))

log = logging.getLogger("mvp_agent")

def _spec(name, desc, props, required):
    return {"type": "function", "function": {"name": name, "description": desc,
            "parameters": {"type": "object", "properties": props, "required": required,
                           "additionalProperties": False}}}
TOOL_SCHEMAS = [
    _spec("tool_exec", "执行系统命令", {"cmd": {"type": "string", "description": "命令"}}, ["cmd"]),
    _spec("tool_read", "读取文件", {"fp": {"type": "string", "description": "文件路径"}}, ["fp"]),
    _spec("tool_write", "写入文件", {"fp": {"type": "string", "description": "文件路径"},
                                     "data": {"type": "string", "description": "内容"}}, ["fp", "data"]),
    _spec("tool_python", "执行Python代码", {"code": {"type": "string", "description": "Python代码"}}, ["code"])]

def tool_exec(cmd):
    try:
        r = subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=TOOL_TIMEOUT)
        o = r.stdout + (f"\n[stderr]\n{r.stderr}" if r.stderr else "")
        return (o + (f"\n[exit:{r.returncode}]" if r.returncode else "")).strip() or "(无输出)"
    except subprocess.TimeoutExpired:
        return f"[错误]命令超时(>{TOOL_TIMEOUT:g}s)"
    except Exception as e: return f"[错误]{e}"

def tool_read(fp):
    try:
        with open(fp, encoding="utf-8", errors="replace") as f:
            return f.read()
    except Exception as e: return f"[错误]{e}"

def tool_write(fp, data):
    try:
        os.makedirs(os.path.dirname(fp) or ".", exist_ok=True)
        with open(fp, "w", encoding="utf-8") as f:
            f.write(data)
        return f"OK—写入{len(data)}字符"
    except Exception as e: return f"[错误]{e}"

def tool_python(code):
    p = None
    try:
        with tempfile.NamedTemporaryFile(mode="w", suffix=".py", delete=False, encoding="utf-8") as t:
            t.write(code); p = t.name
        r = subprocess.run([sys.executable, p], capture_output=True, text=True, timeout=TOOL_TIMEOUT)
        o = r.stdout + (f"\n[stderr]\n{r.stderr}" if r.stderr else "")
        return o.strip() or "(无输出)"
    except subprocess.TimeoutExpired:
        return f"[错误]代码执行超时(>{TOOL_TIMEOUT:g}s)"
    except Exception as e: return f"[错误]{e}"
    finally:
        if p:
            try: os.unlink(p)
            except OSError: pass

TOOL_FUNCS = {"tool_exec": tool_exec, "tool_read": tool_read, "tool_write": tool_write, "tool_python": tool_python}

def _clip(text):
    """限制回填给模型的结果长度,避免撑爆上下文。"""
    if len(text) <= MAX_TOOL_CHARS:
        return text
    return text[:MAX_TOOL_CHARS] + f"\n…[已截断,原始 {len(text)} 字符]"

def run_tool(tc):
    """解析并执行一次工具调用。任何异常都转成文本结果,绝不向外抛出。"""
    name = (tc.get("function") or {}).get("name") or ""
    try:
        raw = (tc.get("function") or {}).get("arguments") or "{}"
        args = json.loads(raw)
        if not isinstance(args, dict):
            raise ValueError(f"arguments 必须是 JSON 对象,收到 {type(args).__name__}")
        if name not in TOOL_FUNCS:
            raise LookupError(f"未知工具:{name}")
        return _clip(str(TOOL_FUNCS[name](**args)))
    except Exception as e:
        return _clip(f"[错误]工具 {name or '?'} 调用失败:{type(e).__name__}: {e}")

def chat(messages):
    """POST /chat/completions,返回 assistant 消息(不依赖 openai 库)。"""
    body = json.dumps({"model": MODEL, "messages": messages, "tools": TOOL_SCHEMAS},
                      ensure_ascii=False).encode("utf-8")
    req = urllib.request.Request(f"{BASE_URL}/chat/completions", data=body, headers={
        "Content-Type": "application/json; charset=utf-8",
        "Authorization": f"Bearer {API_KEY}", "User-Agent": "mvp-agent/1.0"})
    try:
        with urllib.request.urlopen(req, timeout=REQUEST_TIMEOUT) as resp:
            return json.load(resp)["choices"][0]["message"]
    except urllib.error.HTTPError as e:  # 400/401 的有用信息全在 body 里
        detail = e.read().decode("utf-8", "replace")[:1000]
        raise RuntimeError(f"HTTP {e.code} {e.reason}: {detail}") from e
    except urllib.error.URLError as e:
        raise RuntimeError(f"网络错误:{e.reason}") from e

def agent_loop(inp, ctx):
    ctx.append({"role": "user", "content": inp})
    for _ in range(MAX_TURNS):
        try:
            m = chat(ctx)
        except Exception as e:
            msg = str(e)
            if "context" in msg.lower() or "token" in msg.lower():
                return f"[错误]上下文过长,请重开会话:{msg}"
            return f"[错误]API失败:{msg}"
        ctx.append({k: m[k] for k in ("role", "content", "tool_calls") if k in m})
        if not m.get("tool_calls"):
            return m.get("content") or ""
        for tc in m["tool_calls"]:  # 逐个执行并必定回填,保证 tool_call 与 tool 结果一一配对
            log.info("[tool_call]%s(%s)", (tc.get("function") or {}).get("name"), (tc.get("function") or {}).get("arguments"))
            ctx.append({"role": "tool", "tool_call_id": tc.get("id"), "content": run_tool(tc)})
    return f"[Agent]防止死循环,达到最大轮次({MAX_TURNS}),中断"

def main():
    logging.basicConfig(level=logging.INFO, format="%(message)s")
    if not API_KEY:
        sys.exit("错误:请设置LLM_API_KEY")
    ctx = [{"role": "system", "content": SYS_PROMPT}]
    log.info("智能助手 %s 就绪。输入消息(或'exit'退出)。\n", MODEL)
    while True:
        try:
            inp = input("用户> ").strip()
        except (EOFError, KeyboardInterrupt):
            print("\n再见。"); break
        if not inp: continue
        if inp.lower() in ("exit", "quit"): print("再见。"); break
        log.info("处理请求:%s", inp)
        print(f"\n助手>{agent_loop(inp, ctx)}\n")

if __name__ == "__main__":
    main()
