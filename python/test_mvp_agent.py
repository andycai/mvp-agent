#!/usr/bin/env python3
"""mvp_agent 单元测试:全部使用假响应,不联网、不依赖真实 API Key。

运行:  python3 -m unittest -v test_mvp_agent.py
"""
import io
import json
import os
import sys
import tempfile
import unittest
import urllib.error
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import mvp_agent as agent  # noqa: E402


def _tc(name, arguments, cid="call_1"):
    return {"id": cid, "type": "function",
            "function": {"name": name, "arguments": arguments}}


def _msg(content=None, tool_calls=None):
    return {"role": "assistant", "content": content, "tool_calls": tool_calls}


def _assert_paired(tc, ctx):
    """每个 assistant.tool_calls 的 id 都必须有且只有一个 tool 结果。"""
    want = [c["id"] for m in ctx if m["role"] == "assistant" for c in (m.get("tool_calls") or [])]
    got = [m["tool_call_id"] for m in ctx if m["role"] == "tool"]
    tc.assertEqual(sorted(want), sorted(got))
    tc.assertEqual(len(want), len(got))


class TestSchemas(unittest.TestCase):
    def test_schemas_are_strict_and_complete(self):
        names = [s["function"]["name"] for s in agent.TOOL_SCHEMAS]
        self.assertEqual(names, ["tool_exec", "tool_read", "tool_write", "tool_python"])
        for s in agent.TOOL_SCHEMAS:
            self.assertIs(s["function"]["parameters"]["additionalProperties"], False)
            for req in s["function"]["parameters"]["required"]:
                self.assertIn(req, s["function"]["parameters"]["properties"])


class TestRunTool(unittest.TestCase):
    def test_ok(self):
        self.assertEqual(agent.run_tool(_tc("tool_python", '{"code": "print(6*7)"}')), "42")

    def test_bad_json_is_returned_as_tool_error(self):
        # 关键回归:坏 JSON 不能再抛出,必须变成 tool 结果
        out = agent.run_tool(_tc("tool_read", "{'fp': 'x'}"))
        self.assertTrue(out.startswith("[错误]工具 tool_read 调用失败"), out)
        self.assertIn("JSONDecodeError", out)

    def test_unknown_tool(self):
        out = agent.run_tool(_tc("tool_rm_rf", "{}"))
        self.assertIn("未知工具:tool_rm_rf", out)

    def test_unknown_parameter(self):
        out = agent.run_tool(_tc("tool_read", '{"filepath": "x"}'))
        self.assertIn("TypeError", out)

    def test_arguments_must_be_object(self):
        out = agent.run_tool(_tc("tool_read", '"just a string"'))
        self.assertIn("必须是 JSON 对象", out)

    def test_non_string_arguments_do_not_crash(self):
        out = agent.run_tool({"id": "x", "function": {"name": "tool_exec", "arguments": None}})
        self.assertTrue(out)  # 空参数 -> {} -> 执行 "cmd" 缺失 -> TypeError,但不抛
        self.assertIn("[错误]", out)

    def test_truncation(self):
        fd, path = tempfile.mkstemp()
        os.close(fd)
        try:
            with open(path, "w", encoding="utf-8") as f:
                f.write("a" * 100)
            with mock.patch.object(agent, "MAX_TOOL_CHARS", 10):
                out = agent.run_tool(_tc("tool_read", json.dumps({"fp": path})))
            self.assertIn("已截断", out)
            self.assertTrue(out.startswith("a" * 10))
        finally:
            os.unlink(path)


class TestAgentLoop(unittest.TestCase):
    def test_happy_path(self):
        replies = [_msg(tool_calls=[_tc("tool_python", '{"code": "print(2+3)"}')]), _msg(content="答案是 5。")]
        ctx = []
        with mock.patch.object(agent, "chat", side_effect=replies):
            self.assertEqual(agent.agent_loop("算 2+3", ctx), "答案是 5。")
        _assert_paired(self, ctx)
        self.assertEqual([m for m in ctx if m["role"] == "tool"][0]["content"], "5")

    def test_bad_json_does_not_pollute_history(self):
        """回归:旧实现在这里会 return 且丢掉 tool 结果,导致下一轮 400。"""
        replies = [_msg(tool_calls=[_tc("tool_read", "{'fp': 'x'}")]), _msg(content="已改用正确格式。")]
        ctx = []
        with mock.patch.object(agent, "chat", side_effect=replies):
            agent.agent_loop("读文件", ctx)
        _assert_paired(self, ctx)
        # 第二轮(即下一次用户输入)仍可正常继续
        with mock.patch.object(agent, "chat", return_value=_msg(content="继续正常")) as m:
            self.assertEqual(agent.agent_loop("继续", ctx), "继续正常")
            self.assertTrue(m.called)
        _assert_paired(self, ctx)

    def test_unknown_tool_and_param_are_not_reported_as_api_errors(self):
        replies = [_msg(tool_calls=[_tc("nope", "{}", "c1"), _tc("tool_read", '{"filepath":"x"}', "c2")]),
                   _msg(content="done")]
        ctx = []
        with mock.patch.object(agent, "chat", side_effect=replies):
            out = agent.agent_loop("t", ctx)
        self.assertEqual(out, "done")
        _assert_paired(self, ctx)
        tool_text = " ".join(m["content"] for m in ctx if m["role"] == "tool")
        self.assertNotIn("API失败", tool_text)
        self.assertIn("未知工具", tool_text)

    def test_multiple_tool_calls_all_paired(self):
        replies = [_msg(tool_calls=[_tc("tool_python", '{"code":"print(1)"}', "a"),
                                    _tc("tool_python", '{"code":"print(2)"}', "b")]), _msg(content="ok")]
        ctx = []
        with mock.patch.object(agent, "chat", side_effect=replies):
            agent.agent_loop("t", ctx)
        _assert_paired(self, ctx)
        self.assertEqual([m["content"] for m in ctx if m["role"] == "tool"], ["1", "2"])

    def test_max_turns(self):
        def always_tool(_ctx):
            return _msg(tool_calls=[_tc("tool_python", '{"code":"print(0)"}')])
        ctx = []
        with mock.patch.object(agent, "chat", side_effect=always_tool), \
             mock.patch.object(agent, "MAX_TURNS", 3):
            out = agent.agent_loop("t", ctx)
        self.assertIn("最大轮次(3)", out)
        _assert_paired(self, ctx)

    def test_context_length_error_is_hinted(self):
        ctx = []
        with mock.patch.object(agent, "chat", side_effect=RuntimeError("HTTP 400: context length exceeded")):
            out = agent.agent_loop("t", ctx)
        self.assertIn("上下文过长", out)
        _assert_paired(self, ctx)

    def test_generic_api_error(self):
        ctx = []
        with mock.patch.object(agent, "chat", side_effect=RuntimeError("HTTP 401 Unauthorized: bad key")):
            out = agent.agent_loop("t", ctx)
        self.assertIn("API失败", out)
        self.assertIn("401", out)


class TestChatHttp(unittest.TestCase):
    def test_http_error_body_is_preserved(self):
        err = urllib.error.HTTPError("http://x", 400, "Bad Request", {},
                                     io.BytesIO('{"error":"bad tool schema"}'.encode()))
        with mock.patch("urllib.request.urlopen", side_effect=err):
            with self.assertRaises(RuntimeError) as cm:
                agent.chat([{"role": "user", "content": "hi"}])
        self.assertIn("400", str(cm.exception))
        self.assertIn("bad tool schema", str(cm.exception))

    def test_network_error(self):
        with mock.patch("urllib.request.urlopen", side_effect=urllib.error.URLError("boom")):
            with self.assertRaises(RuntimeError) as cm:
                agent.chat([{"role": "user", "content": "hi"}])
        self.assertIn("网络错误", str(cm.exception))

    def test_request_shape(self):
        captured = {}

        class FakeResp:
            def __enter__(self): return io.BytesIO(b'{"choices":[{"message":{"role":"assistant","content":"hi"}}]}')
            def __exit__(self, *a): return False

        def fake_urlopen(req, timeout=None):
            captured["url"] = req.full_url
            captured["headers"] = {k.lower(): v for k, v in req.headers.items()}
            captured["body"] = json.loads(req.data.decode("utf-8"))
            captured["timeout"] = timeout
            return FakeResp()

        with mock.patch("urllib.request.urlopen", side_effect=fake_urlopen):
            out = agent.chat([{"role": "user", "content": "中文"}])
        self.assertEqual(out["content"], "hi")
        self.assertTrue(captured["url"].endswith("/chat/completions"))
        self.assertIn("authorization", captured["headers"])
        self.assertIn("user-agent", captured["headers"])
        self.assertEqual(captured["timeout"], agent.REQUEST_TIMEOUT)
        self.assertEqual(captured["body"]["messages"][0]["content"], "中文")


if __name__ == "__main__":
    unittest.main(verbosity=2)
