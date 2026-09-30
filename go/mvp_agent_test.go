package main

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func testConfig() *Config {
	return &Config{APIKey: "k", BaseURL: "http://127.0.0.1:1/v1", Model: "m", MaxTurns: 15,
		Timeout: 5 * time.Second, ToolTimeout: 10 * time.Second, MaxToolChars: 16000,
		MaxHistory: 40, Bash: "/bin/bash"}
}

func testCall(id, name, args string) map[string]any {
	return map[string]any{"id": id, "type": "function",
		"function": map[string]any{"name": name, "arguments": args}}
}

func assistantWithCalls(calls ...map[string]any) map[string]any {
	list := make([]any, len(calls))
	for i, c := range calls {
		list[i] = c
	}
	return map[string]any{"role": "assistant", "content": nil, "tool_calls": list}
}

// assertLegal 断言历史合法:首条 system,每个 tool_call 恰好一条配对响应。
func assertLegal(t *testing.T, msgs []map[string]any) {
	t.Helper()
	if len(msgs) == 0 {
		t.Fatalf("空 messages")
	}
	if msgs[0]["role"] != "system" {
		t.Errorf("首条不是 system:%v", msgs[0]["role"])
	}
	pending := map[string]bool{}
	for i, m := range msgs {
		switch m["role"] {
		case "assistant":
			if len(pending) > 0 {
				t.Errorf("assistant 之前仍有未配对 tool_call:%v", pending)
			}
			pending = map[string]bool{}
			if tcs, ok := m["tool_calls"].([]any); ok {
				for _, tc := range tcs {
					id, _ := tc.(map[string]any)["id"].(string)
					if pending[id] {
						t.Errorf("重复 tool_call id %s", id)
					}
					pending[id] = true
				}
			}
		case "tool":
			id, _ := m["tool_call_id"].(string)
			if !pending[id] {
				t.Errorf("孤儿 tool 响应 %s @%d", id, i)
			}
			delete(pending, id)
		}
	}
	if len(pending) > 0 {
		t.Errorf("结尾未配对 tool_call:%v", pending)
	}
}

func TestToolSchemas(t *testing.T) {
	names := []string{"tool_bash", "tool_read", "tool_write"}
	schemas := ToolSchemas()
	if len(schemas) != 3 {
		t.Fatalf("期望 3 个工具,得到 %d", len(schemas))
	}
	for i, s := range schemas {
		fn := s["function"].(map[string]any)
		if fn["name"] != names[i] {
			t.Errorf("工具名 %v != %s", fn["name"], names[i])
		}
		p := fn["parameters"].(map[string]any)
		if p["additionalProperties"] != false {
			t.Errorf("%s 缺少 additionalProperties:false", names[i])
		}
		if p["type"] != "object" {
			t.Errorf("%s parameters.type != object", names[i])
		}
	}
}

func TestToolBashOutputAndExitCode(t *testing.T) {
	got := toolBash(map[string]any{"cmd": "echo hello-stdout; echo oops >&2; exit 3"}, testConfig())
	want := "hello-stdout\n\n[stderr]\noops\n\n[exit:3]"
	if got != want {
		t.Errorf("得到 %q,期望 %q", got, want)
	}
}

func TestToolBashEmpty(t *testing.T) {
	if got := toolBash(map[string]any{"cmd": "true"}, testConfig()); got != "(无输出)" {
		t.Errorf("得到 %q", got)
	}
}

func TestToolBashTimeout(t *testing.T) {
	c := testConfig()
	c.ToolTimeout = 300 * time.Millisecond
	got := toolBash(map[string]any{"cmd": "sleep 5"}, c)
	if !strings.HasPrefix(got, "[错误]命令超时") {
		t.Errorf("得到 %q", got)
	}
}

func TestToolReadMissing(t *testing.T) {
	got := toolRead(map[string]any{"fp": filepath.Join(t.TempDir(), "nope")}, testConfig())
	if !strings.HasPrefix(got, "[错误]") {
		t.Errorf("得到 %q", got)
	}
}

func TestToolWriteCreatesParentsAndCountsRunes(t *testing.T) {
	fp := filepath.Join(t.TempDir(), "a", "b", "c.txt")
	got := toolWrite(map[string]any{"fp": fp, "data": "中文abc"}, testConfig())
	if got != "OK—写入5字符" {
		t.Errorf("得到 %q,期望 OK—写入5字符", got)
	}
	if b, err := os.ReadFile(fp); err != nil || string(b) != "中文abc" {
		t.Errorf("文件内容不对:%v %q", err, b)
	}
}

func TestRunToolUnknown(t *testing.T) {
	got := RunTool(testCall("c1", "nope", "{}"), testConfig())
	if !strings.Contains(got, "未知工具:nope") || !strings.HasPrefix(got, "[错误]工具 nope 调用失败:") {
		t.Errorf("得到 %q", got)
	}
}

func TestRunToolBadArguments(t *testing.T) {
	c := testConfig()
	if got := RunTool(testCall("c", "tool_bash", "not-json"), c); !strings.HasPrefix(got, "[错误]工具 tool_bash 调用失败:") {
		t.Errorf("非法 JSON 得到 %q", got)
	}
	if got := RunTool(testCall("c", "tool_bash", "[1,2]"), c); !strings.Contains(got, "必须是 JSON 对象") {
		t.Errorf("非对象参数得到 %q", got)
	}
	if got := RunTool(testCall("c", "tool_bash", ""), c); got != "(无输出)" {
		t.Errorf("空参数得到 %q", got)
	}
}

func TestRunToolTruncatesByRunes(t *testing.T) {
	txt := strings.Repeat("中", 20000)
	fp := filepath.Join(t.TempDir(), "x")
	if err := os.WriteFile(fp, []byte(txt), 0o644); err != nil {
		t.Fatal(err)
	}
	args := fmt.Sprintf("{\"fp\":%q}", fp)
	got := RunTool(testCall("c", "tool_read", args), testConfig())
	if !strings.Contains(got, "原始 20000 字符") {
		t.Fatalf("未截断:%d 字符", len([]rune(got)))
	}
	// 必须按字符而非字节截断:16000 个中文字符
	if n := len([]rune(strings.SplitN(got, "\n…[已截断", 2)[0])); n != 16000 {
		t.Errorf("截断长度 %d,期望 16000 字符", n)
	}
}

func TestTrimHistory(t *testing.T) {
	c := testConfig()
	c.MaxHistory = 4
	small := []map[string]any{{"role": "system"}, {"role": "user"}, {"role": "assistant"}}
	if got := TrimHistory(c, small); len(got) != 3 {
		t.Errorf("小历史不应被裁剪,得到 %d 条", len(got))
	}
	big := []map[string]any{{"role": "system"}}
	for i := 0; i < 10; i++ {
		big = append(big,
			map[string]any{"role": "user", "content": "u"},
			assistantWithCalls(testCall(fmt.Sprintf("c%d", i), "tool_bash", "{}")),
			map[string]any{"role": "tool", "tool_call_id": fmt.Sprintf("c%d", i), "content": "ok"})
	}
	got := TrimHistory(c, big)
	if got[0]["role"] != "system" {
		t.Errorf("首条不是 system:%v", got[0]["role"])
	}
	if got[1]["role"] == "tool" {
		t.Errorf("裁剪点落在了 tool 响应上:%v", got[1]["role"])
	}
	if len(got) > c.MaxHistory+3 {
		t.Errorf("裁剪后 %d 条,超出预期", len(got))
	}
	assertLegal(t, got)
}

// 裁剪点落在连续 tool 结果中间时不得越界(回归用例)。
func TestTrimHistoryDoesNotRunOffEnd(t *testing.T) {
	c := testConfig()
	c.MaxHistory = 1
	ctx := []map[string]any{{"role": "system"}}
	for i := 0; i < 5; i++ {
		ctx = append(ctx, map[string]any{"role": "tool", "tool_call_id": "x", "content": "r"})
	}
	got := TrimHistory(c, ctx) // 旧实现在这里会 panic
	if len(got) == 0 || got[0]["role"] != "system" {
		t.Errorf("得到 %#v", got)
	}
}

func TestChatHTTPErrorKeepsBody(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(400)
		_, _ = w.Write([]byte("{\"error\":{\"message\":\"boom-detail\"}}"))
	}))
	defer srv.Close()
	c := testConfig()
	c.BaseURL = srv.URL
	_, err := Chat(c, []map[string]any{{"role": "system"}})
	if err == nil {
		t.Fatal("期望报错")
	}
	if !strings.Contains(err.Error(), "HTTP 400") || !strings.Contains(err.Error(), "boom-detail") {
		t.Errorf("错误未保留响应体:%v", err)
	}
}

func TestChatNetworkError(t *testing.T) {
	c := testConfig()
	c.BaseURL = "http://127.0.0.1:1"
	_, err := Chat(c, []map[string]any{{"role": "system"}})
	if err == nil || !strings.HasPrefix(err.Error(), "网络错误:") {
		t.Errorf("得到 %v", err)
	}
}

// 端到端:假服务逐请求断言历史合法,并让 agent 跑完工具循环。
func TestAgentLoopEndToEnd(t *testing.T) {
	tmp := t.TempDir()
	var rounds []int
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var body struct {
			Messages []map[string]any "json:\"messages\""
		}
		_ = json.NewDecoder(r.Body).Decode(&body)
		assertLegal(t, body.Messages)
		rounds = append(rounds, len(body.Messages))
		n := 0
		for _, m := range body.Messages {
			if m["role"] == "assistant" {
				if _, ok := m["tool_calls"]; ok {
					n++
				}
			}
		}
		var msg map[string]any
		switch n {
		case 0:
			msg = assistantWithCalls(
				testCall("call_1", "tool_bash", "{\"cmd\":\"echo hi\"}"),
				testCall("call_2", "nope", "{}"),
				testCall("call_3", "tool_bash", "not-json"),
				testCall("call_4", "tool_write", fmt.Sprintf("{\"fp\":%q,\"data\":\"x\"}", filepath.Join(tmp, "o.txt"))))
		case 1:
			msg = assistantWithCalls(testCall("call_5", "tool_read", fmt.Sprintf("{\"fp\":%q}", filepath.Join(tmp, "o.txt"))))
		default:
			msg = map[string]any{"role": "assistant", "content": "ALL-OK"}
		}
		_ = json.NewEncoder(w).Encode(map[string]any{"choices": []any{map[string]any{"message": msg}}})
	}))
	defer srv.Close()
	c := testConfig()
	c.BaseURL = srv.URL + "/v1"
	ctx := []map[string]any{{"role": "system", "content": sysPrompt}}
	if got := AgentLoop(c, "任务", &ctx); got != "ALL-OK" {
		t.Fatalf("得到 %q", got)
	}
	if len(rounds) != 3 {
		t.Errorf("期望 3 次请求,得到 %d", len(rounds))
	}
	bodies := 0
	for _, m := range ctx {
		if m["role"] == "tool" {
			bodies++
		}
	}
	if bodies != 5 {
		t.Errorf("期望 5 条 tool 结果,得到 %d", bodies)
	}
}

func TestAgentLoopContextLength(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(400)
		_, _ = w.Write([]byte("{\"error\":{\"message\":\"maximum context length is 8192 tokens\"}}"))
	}))
	defer srv.Close()
	c := testConfig()
	c.BaseURL = srv.URL
	ctx := []map[string]any{{"role": "system", "content": sysPrompt}}
	got := AgentLoop(c, "任务", &ctx)
	if !strings.HasPrefix(got, "[错误]上下文过长,请重开会话:") {
		t.Errorf("得到 %q", got)
	}
}

func TestAgentLoopMaxTurns(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		msg := assistantWithCalls(testCall("c", "tool_bash", "{\"cmd\":\"true\"}"))
		_ = json.NewEncoder(w).Encode(map[string]any{"choices": []any{map[string]any{"message": msg}}})
	}))
	defer srv.Close()
	c := testConfig()
	c.BaseURL = srv.URL
	c.MaxTurns = 3
	ctx := []map[string]any{{"role": "system", "content": sysPrompt}}
	got := AgentLoop(c, "任务", &ctx)
	if !strings.Contains(got, "达到最大轮次(3)") {
		t.Errorf("得到 %q", got)
	}
	assertLegal(t, ctx)
}

// 长工具循环下,历史必须始终有界且合法。
func TestAgentLoopHistoryStaysBounded(t *testing.T) {
	var counts []int
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var body struct {
			Messages []map[string]any "json:\"messages\""
		}
		_ = json.NewDecoder(r.Body).Decode(&body)
		assertLegal(t, body.Messages)
		counts = append(counts, len(body.Messages))
		n := len(counts)
		var msg map[string]any
		if n >= 25 {
			msg = map[string]any{"role": "assistant", "content": "TRIM-OK"}
		} else {
			msg = assistantWithCalls(testCall(fmt.Sprintf("c%d", n), "tool_bash", "{\"cmd\":\"true\"}"))
		}
		_ = json.NewEncoder(w).Encode(map[string]any{"choices": []any{map[string]any{"message": msg}}})
	}))
	defer srv.Close()
	c := testConfig()
	c.BaseURL = srv.URL
	c.MaxHistory = 6
	c.MaxTurns = 40
	ctx := []map[string]any{{"role": "system", "content": sysPrompt}}
	if got := AgentLoop(c, "任务", &ctx); got != "TRIM-OK" {
		t.Fatalf("得到 %q", got)
	}
	if len(counts) != 25 {
		t.Fatalf("期望 25 次请求,得到 %d", len(counts))
	}
	peak := 0
	for _, n := range counts {
		if n > peak {
			peak = n
		}
	}
	if peak > 9 {
		t.Errorf("历史未被有界裁剪,峰值 %d 条:%v", peak, counts)
	}
}

func TestConfigFromEnv(t *testing.T) {
	t.Setenv("LLM_API_KEY", "k1")
	t.Setenv("LLM_MODEL", "m1")
	t.Setenv("LLM_MAX_HISTORY", "0")
	t.Setenv("LLM_BASE_URL", "http://x/v1/")
	t.Setenv("LLM_TOOL_TIMEOUT", "2.5")
	c := ConfigFromEnv()
	if c.APIKey != "k1" || c.Model != "m1" {
		t.Errorf("配置读取错误:%+v", c)
	}
	if c.MaxHistory != 1 {
		t.Errorf("MaxHistory 下限失效:%d", c.MaxHistory)
	}
	if c.BaseURL != "http://x/v1" {
		t.Errorf("BaseURL 未去掉尾部斜杠:%q", c.BaseURL)
	}
	if c.ToolTimeout != 2500*time.Millisecond {
		t.Errorf("ToolTimeout 解析错误:%v", c.ToolTimeout)
	}
}

func TestConfigFromEnvFallbacks(t *testing.T) {
	t.Setenv("LLM_API_KEY", "")
	t.Setenv("DEEPSEEK_API_KEY", "dk")
	t.Setenv("OPENAI_API_KEY", "ok")
	if got := ConfigFromEnv().APIKey; got != "dk" {
		t.Errorf("回退顺序错误:%q", got)
	}
}
