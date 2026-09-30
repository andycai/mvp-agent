#!/usr/bin/env python3
"""跨语言一致性验证:用假 OpenAI 兼容服务跑 python/go/rust/typescript 四个实现,
以 python 版为基准(oracle),比较工具结果、错误分类与历史合法性。

用法:python3 conformance.py [impl ...]   默认全部
"""
import json, os, re, subprocess, sys, tempfile, threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = os.environ.get("MVP_ROOT", "/Users/andy/Workspace/github/andycai/mvp-agent")

# name -> (argv, cwd)
IMPLS = {
    "python":     (["python3", "mvp_agent.py", "TASK"], os.path.join(ROOT, "python")),
    "go":         (["go", "run", ".", "TASK"], os.path.join(ROOT, "go")),
    "rust":       (["cargo", "run", "--quiet", "--", "TASK"], os.path.join(ROOT, "rust")),
    "typescript": (["node", "mvp_agent.ts", "TASK"], os.path.join(ROOT, "typescript")),
}

STATE = {"scenario": "tools", "records": [], "violations": [], "tmp": None}


def tool_call(i, name, args):
    return {"id": "call_%d" % i, "type": "function", "function": {"name": name, "arguments": args}}


def round0():
    t = STATE["tmp"]
    return [
        tool_call(1, "tool_bash", json.dumps({"cmd": "echo hello-stdout; echo oops >&2; exit 3"})),
        tool_call(2, "tool_read", json.dumps({"fp": os.path.join(t, "missing.txt")})),
        tool_call(3, "tool_write", "{}"),
    ]


def round1():
    t = STATE["tmp"]
    return [
        tool_call(4, "tool_read", json.dumps({"fp": os.path.join(t, "big_ascii.txt")})),
        tool_call(5, "tool_read", json.dumps({"fp": os.path.join(t, "big_cjk.txt")})),
        tool_call(6, "nope", "{}"),
        tool_call(7, "tool_bash", "not-json"),
        tool_call(8, "tool_bash", "[1,2]"),
        tool_call(9, "tool_read", ""),
    ]


def validate(msgs):
    """历史合法性:首条 system;每个 tool_call id 恰好一条配对 tool 响应。"""
    errs = []
    if not msgs:
        return ["空 messages"]
    if msgs[0].get("role") != "system":
        errs.append("首条不是 system")
    pending = {}
    for i, m in enumerate(msgs):
        r = m.get("role")
        if r == "assistant":
            if pending:
                errs.append("assistant 之前仍有未配对 tool_call %s" % sorted(pending))
            pending = {}
            for tc in m.get("tool_calls") or []:
                tid = tc.get("id")
                if tid in pending:
                    errs.append("重复 tool_call id %s" % tid)
                pending[tid] = True
        elif r == "tool":
            tid = m.get("tool_call_id")
            if tid not in pending:
                errs.append("孤儿 tool 响应 %s @%d" % (tid, i))
            else:
                pending.pop(tid)
    if pending:
        errs.append("结尾未配对 tool_call %s" % sorted(pending))
    return errs


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def _send(self, code, obj):
        body = json.dumps(obj, ensure_ascii=False).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _msg(self, message):
        self._send(200, {"choices": [{"message": message}]})

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(n)
        try:
            body = json.loads(raw)
        except Exception as e:
            STATE["violations"].append("客户端 body 非法 JSON: %s" % e)
            body = {}
        msgs = body.get("messages") or []
        STATE["violations"].extend(validate(msgs))
        STATE["records"].append(msgs)
        s = STATE["scenario"]
        if s == "http400":
            return self._send(400, {"error": {"message": "boom-detail"}})
        if s == "context":
            return self._send(400, {"error": {"message": "maximum context length is 8192 tokens"}})
        if s == "trim":
            STATE["round"] = STATE.get("round", 0) + 1
            if STATE["round"] >= 25:
                return self._msg({"role": "assistant", "content": "TRIM-OK"})
            n = STATE["round"]
            return self._msg({"role": "assistant", "content": None,
                              "tool_calls": [tool_call(100 + n, "tool_bash",
                                                       json.dumps({"cmd": "echo r%d" % n}))]})
        rounds = sum(1 for m in msgs if m.get("role") == "assistant" and m.get("tool_calls"))
        if rounds == 0:
            return self._msg({"role": "assistant", "content": None, "tool_calls": round0()})
        if rounds == 1:
            return self._msg({"role": "assistant", "content": None, "tool_calls": round1()})
        return self._msg({"role": "assistant", "content": "ALL-OK"})


def make_tmp():
    t = tempfile.mkdtemp(prefix="mvpconf-")
    with open(os.path.join(t, "big_ascii.txt"), "w") as f:
        f.write("A" * 20000)
    with open(os.path.join(t, "big_cjk.txt"), "w") as f:
        f.write("\u4e2d\u6587" * 10000)  # 20000 字符 / 60000 字节
    return t


def extract(records):
    """返回 [(id, name, content)],同一 id 以最后一次出现为准。"""
    res, order = {}, []
    for msgs in records:
        names = {}
        for m in msgs:
            if m.get("role") == "assistant":
                names = {tc["id"]: tc["function"]["name"] for tc in (m.get("tool_calls") or [])}
            elif m.get("role") == "tool":
                tid = m.get("tool_call_id")
                if tid not in res:
                    order.append(tid)
                res[tid] = (names.get(tid, "?"), m.get("content") or "")
    return [(tid,) + res[tid] for tid in order]


def run(impl, scenario, tmp, port):
    STATE.update(scenario=scenario, records=[], violations=[], tmp=tmp, round=0)
    argv, cwd = IMPLS[impl]
    env = dict(os.environ)
    env.update(LLM_API_KEY="test-key", LLM_BASE_URL="http://127.0.0.1:%d/v1" % port,
               LLM_MODEL="fake-model", LLM_MAX_HISTORY="40", LLM_TOOL_TIMEOUT="30")
    if scenario == "trim":
        env.update(LLM_MAX_HISTORY="6", LLM_MAX_TURNS="40")
    try:
        p = subprocess.run(argv, cwd=cwd, env=env, capture_output=True, text=True, timeout=300)
        rc, out, err = p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired:
        rc, out, err = "TIMEOUT", "", ""
    except FileNotFoundError as e:
        rc, out, err = "MISSING", "", str(e)
    return {"rc": rc, "stdout": out, "stderr": err,
            "records": [list(r) for r in STATE["records"]],
            "counts": [len(r) for r in STATE["records"]],
            "violations": list(STATE["violations"])}


def norm_oracle(s):
    """把语言相关的错误细节折叠,只要求错误类别一致。"""
    if s.startswith("[错误]"):
        return "<[错误]...>"
    return s


def compare(oracle, other):
    diffs = []
    o = {t[0]: (t[1], t[2]) for t in oracle}
    n = {t[0]: (t[1], t[2]) for t in other}
    for tid in o:
        if tid not in n:
            diffs.append("%s 缺失" % tid)
            continue
        if o[tid][0] != n[tid][0]:
            diffs.append("%s 工具名 %r != %r" % (tid, n[tid][0], o[tid][0]))
        a, b = o[tid][1], n[tid][1]
        if norm_oracle(a) != norm_oracle(b):
            diffs.append("%s 结果不同\n    oracle=%r\n    other =%r" % (tid, a[:300], b[:300]))
    for tid in n:
        if tid not in o:
            diffs.append("%s 多出" % tid)
    return diffs


def main():
    which = sys.argv[1:] or ["python", "go", "rust", "typescript"]
    tmp = make_tmp()
    srv = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    port = srv.server_address[1]
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    print("mock server on 127.0.0.1:%d  tmp=%s\n" % (port, tmp))

    results = {}
    for scenario in ("tools", "http400", "context", "trim"):
        print("=" * 70)
        print("场景 %s" % scenario)
        print("=" * 70)
        for impl in which:
            r = run(impl, scenario, tmp, port)
            results[(scenario, impl)] = r
            print("--- %s: rc=%s violations=%d requests=%d"
                  % (impl, r["rc"], len(r["violations"]), len(r["records"])))
            if r["violations"]:
                for v in r["violations"][:5]:
                    print("    ✖ %s" % v)
            if r["counts"]:
                print("    消息数: %s (峰值 %d)" % (r["counts"], max(r["counts"])))
            tail = r["stdout"].strip().splitlines()
            print("    stdout: %s" % (tail[-1][:160] if tail else "(空)"))
            if r["stderr"].strip():
                print("    stderr: %s" % r["stderr"].strip().splitlines()[-1][:200])
            if scenario == "tools" and impl in which:
                for tid, name, c in extract(r["records"]):
                    print("    %-9s %-11s %d 字符" % (tid, name, len(c)))
        print()
        if scenario == "tools" and "python" in which:
            oracle = extract(results[(scenario, "python")]["records"])
            for impl in which:
                if impl == "python":
                    continue
                diffs = compare(oracle, extract(results[(scenario, impl)]["records"]))
                print("%s vs python: %s" % (impl, "一致 ✓" if not diffs else "差异 %d 处" % len(diffs)))
                for d in diffs:
                    print("    ✖ %s" % d)
        print()

    print("=" * 70)
    print("汇总")
    print("=" * 70)
    bad = []
    for (scenario, impl), r in sorted(results.items()):
        ok = r["rc"] == 0 and not r["violations"]
        if scenario == "http400":
            ok = ok and ("API失败" in r["stdout"] and "boom-detail" in r["stdout"])
        if scenario == "context":
            ok = ok and ("上下文过长" in r["stdout"])
        if scenario == "tools":
            ok = ok and ("ALL-OK" in r["stdout"])
        if scenario == "trim":
            ok = ok and ("TRIM-OK" in r["stdout"]) and len(r["counts"]) >= 20 \
                 and (not r["counts"] or max(r["counts"]) <= 9)
        print("%-11s %-8s %s  rc=%s" % (impl, scenario, "PASS" if ok else "FAIL", r["rc"]))
        if not ok:
            bad.append((impl, scenario))
    if "python" in which:
        oracle = extract(results[("tools", "python")]["records"])
        for impl in which:
            if impl == "python":
                continue
            diffs = compare(oracle, extract(results[("tools", impl)]["records"]))
            print("%-11s %-8s %s" % (impl, "一致", "PASS" if not diffs else "FAIL"))
            if diffs:
                bad.append((impl, "一致"))
    print()
    print("结论:" + ("全部通过 ✓" if not bad else "失败 %s" % bad))
    srv.shutdown()
    return 0 if not bad else 1


if __name__ == "__main__":
    sys.exit(main())
