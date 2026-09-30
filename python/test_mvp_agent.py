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
        self.assertEqual(names, ["tool_bash", "tool_read", "tool_write"])
        for s in agent.TOOL_SCHEMAS:
            self.assertIs(s["function"]["parameters"]["additionalProperties"], False)
            for req in s["function"]["parameters"]["required"]:
                self.assertIn(req, s["function"]["parameters"]["properties"])

    def test_no_python_tool(self):
        self.assertNotIn("tool_python", agent.TOOL_FUNCS)
        self.assertNotIn("tool_exec", agent.TOOL_FUNCS)


class TestRunTool(unittest.TestCase):
    def test_ok(self):
        self.assertEqual(agent.run_tool(_tc("tool_bash", '{"cmd": "printf hi"}')), "hi")

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
        out = agent.run_tool({"id": "x", "function": {"name": "tool_bash", "arguments": None}})
        self.assertTrue(out)
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


class TestTrim(unittest.TestCase):
    def test_short_history_untouched(self):
        ctx = [{"role": "system", "content": "s"}, {"role": "user", "content": "u"}]
        self.assertIs(agent._trim(ctx), ctx)

    def test_keeps_system_and_most_recent(self):
        ctx = [{"role": "system", "content": "s"}]
        for i in range(30):
            ctx += [{"role": "user", "content": f"u{i}"}, {"role": "assistant", "content": f"a{i}"}]
        with mock.patch.object(agent, "MAX_HISTORY", 4):
            out = agent._trim(ctx)
        self.assertEqual(out[0]["role"], "system")
        self.assertEqual(len(out), 5)
        self.assertIs(out[-1], ctx[-1])

    def test_never_starts_with_orphan_tool_result(self):
        """裁剪边界落在 tool 响应上时必须继续前移,否则 call/response 被拆散 -> 400。"""
        ctx = [{"role": "system", "content": "s"},
               {"role": "user", "content": "u"},
               {"role": "assistant", "content": None,
                "tool_calls": [_tc("tool_bash", '{"cmd":"true"}', "x")]},
               {"role": "tool", "tool_call_id": "x", "content": "out"},
               {"role": "user", "content": "u2"},
               {"role": "assistant", "content": "a2"}]
        with mock.patch.object(agent, "MAX_HISTORY", 3):
            out = agent._trim(ctx)
        self.assertEqual(out[0]["role"], "system")
        self.assertNotEqual(out[1]["role"], "tool")
        self.assertEqual(out[1]["content"], "u2")
        _assert_paired(self, out)

    def test_trim_does_not_run_off_the_end(self):
        """裁剪点落在一长串 tool 结果中间时不能越界(曾会 IndexError)。"""
        ctx = [{"role": "system", "content": "s"}, {"role": "user", "content": "u"},
               {"role": "assistant", "content": None, "tool_calls": [
                   _tc("tool_bash", '{"cmd":"t"}', "a"),
                   _tc("tool_bash", '{"cmd":"t"}', "b"),
                   _tc("tool_bash", '{"cmd":"t"}', "c")]},
               {"role": "tool", "tool_call_id": "a", "content": "1"},
               {"role": "tool", "tool_call_id": "b", "content": "2"},
               {"role": "tool", "tool_call_id": "c", "content": "3"}]
        with mock.patch.object(agent, "MAX_HISTORY", 2):
            out = agent._trim(ctx)
        # 只能整体丢弃 call+全部响应,不能留下孤儿 tool
        self.assertEqual([m["role"] for m in out], ["system"])
        _assert_paired(self, out)


class TestAgentLoop(unittest.TestCase):
    def test_happy_path(self):
        replies = [_msg(tool_calls=[_tc("tool_bash", '{"cmd": "printf 5"}')]), _msg(content="答案是 5。")]
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
        replies = [_msg(tool_calls=[_tc("tool_bash", '{"cmd":"printf 1"}', "a"),
                                    _tc("tool_bash", '{"cmd":"printf 2"}', "b")]), _msg(content="ok")]
        ctx = []
        with mock.patch.object(agent, "chat", side_effect=replies):
            agent.agent_loop("t", ctx)
        _assert_paired(self, ctx)
        self.assertEqual([m["content"] for m in ctx if m["role"] == "tool"], ["1", "2"])

    def test_max_turns(self):
        def always_tool(_ctx):
            return _msg(tool_calls=[_tc("tool_bash", '{"cmd":"true"}')])
        ctx = []
        with mock.patch.object(agent, "chat", side_effect=always_tool), \
             mock.patch.object(agent, "MAX_TURNS", 3):
            out = agent.agent_loop("t", ctx)
        self.assertIn("最大轮次(3)", out)
        _assert_paired(self, ctx)

    def test_history_is_trimmed_in_place_and_pairs_survive(self):
        """历史被原地裁剪:调用方持有的 ctx 对象也要跟着变小,且始终合法。"""
        ctx = [{"role": "system", "content": "s"}]
        with mock.patch.object(agent, "chat", return_value=_msg(content="ok")), \
             mock.patch.object(agent, "MAX_HISTORY", 3):
            for i in range(20):
                agent.agent_loop(f"m{i}", ctx)
        self.assertEqual(ctx[0]["role"], "system")
        self.assertLessEqual(len(ctx), agent.MAX_HISTORY + 2)  # 有界,不随轮数增长
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
            def __enter__(self):
                return io.BytesIO(b'{"choices":[{"message":{"role":"assistant","content":"hi"}}]}')

            def __exit__(self, *a):
                return False

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
        self.assertEqual([t["function"]["name"] for t in captured["body"]["tools"]],
                         ["tool_bash", "tool_read", "tool_write"])


class TestMain(unittest.TestCase):
    def test_one_shot_mode_with_argv(self):
        """python mvp_agent.py "任务" 应一次性执行并打印结果,不进 REPL。"""
        out, seen = io.StringIO(), []

        def fake_chat(msgs):  # ctx 是可变对象,必须当场快照
            seen.append([dict(m) for m in msgs])
            return _msg(content="你好")

        with mock.patch.object(sys, "argv", ["mvp_agent.py", "打个招呼"]), \
             mock.patch.object(agent, "API_KEY", "k"), \
             mock.patch.object(agent, "chat", side_effect=fake_chat), \
             mock.patch("logging.basicConfig"), mock.patch("sys.stdout", out):
            agent.main()
        self.assertIn("你好", out.getvalue())
        self.assertNotIn("用户>", out.getvalue())
        self.assertEqual(seen[0][0]["role"], "system")
        self.assertEqual(seen[0][-1]["content"], "打个招呼")


if __name__ == "__main__":
    unittest.main(verbosity=2)
