package main

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
)

// testCfg 安装一份确定性的配置(全局 cfg,测试串行执行)。
func testCfg(url string) {
	cfg = Cfg{key: "test-key", url: strings.TrimRight(url, "/"), model: "fake-model",
		turns: 15, timeout: 20, toolTO: 10, maxChar: 16000, maxHist: 40}
}

func tc(id, name, args string) map[string]any {
	return map[string]any{"id": id, "type": "function",
		"function": map[string]any{"name": name, "arguments": args}}
}

func msgCalls(calls ...map[string]any) map[string]any {
	list := make([]any, len(calls))
	for i, c := range calls {
		list[i] = c
	}
	return map[string]any{"role": "assistant", "content": nil, "tool_calls": list}
}

func msgText(t string) map[string]any { return map[string]any{"role": "assistant", "content": t} }

func response(m map[string]any) string {
	b, _ := json.Marshal(map[string]any{"choices": []any{map[string]any{"message": m}}})
	return string(b)
}

// legality 断言历史合法:首条 system,每个 tool_call 恰好一条配对响应。
func legality(t *testing.T, msgs []any) {
	t.Helper()
	if len(msgs) == 0 {
		t.Fatal("空 messages")
	}
	if msgs[0].(map[string]any)["role"] != "system" {
		t.Errorf("首条不是 system:%v", msgs[0])
	}
	pending := map[string]bool{}
	for i, raw := range msgs {
		m := raw.(map[string]any)
		switch m["role"] {
		case "assistant":
			if len(pending) > 0 {
				t.Errorf("assistant 之前仍有未配对 %v", pending)
			}
			pending = map[string]bool{}
			if tcs, ok := m["tool_calls"].([]any); ok {
				for _, c := range tcs {
					id, _ := c.(map[string]any)["id"].(string)
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
		t.Errorf("结尾未配对 %v", pending)
	}
}

type mock struct {
	srv  *httptest.Server
	mu   sync.Mutex
	seen []map[string]any
}

func newMock(h func(i int, body map[string]any) (int, string)) *mock {
	m := &mock{}
	m.srv = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var body map[string]any
		_ = json.NewDecoder(r.Body).Decode(&body)
		m.mu.Lock()
		m.seen = append(m.seen, body)
		i := len(m.seen) - 1
		m.mu.Unlock()
		code, resp := h(i, body)
		w.WriteHeader(code)
		_, _ = w.Write([]byte(resp))
	}))
	return m
}

func (m *mock) bodies() []map[string]any {
	m.mu.Lock()
	defer m.mu.Unlock()
	return append([]map[string]any(nil), m.seen...)
}

func TestSchemasComplete(t *testing.T) {
	s := schemas()
	if len(s) != 3 {
		t.Fatalf("期望 3 个工具,得到 %d", len(s))
	}
	want := []string{"tool_bash", "tool_read", "tool_write"}
	for i, raw := range s {
		f := raw.(map[string]any)["function"].(map[string]any)
		if f["name"] != want[i] {
			t.Errorf("工具名 %v != %s", f["name"], want[i])
		}
		p := f["parameters"].(map[string]any)
		if p["additionalProperties"] != false || p["type"] != "object" {
			t.Errorf("%s 参数 schema 不完整", want[i])
		}
	}
}

func TestBashCapturesStderrAndExitCode(t *testing.T) {
	testCfg("http://127.0.0.1:1")
	got := bash("echo hello-stdout; echo oops >&2; exit 3")
	want := "hello-stdout\n\n[stderr]\noops\n\n[exit:3]"
	if got != want {
		t.Errorf("得到 %q,期望 %q", got, want)
	}
}

func TestBashEmptyAndTimeout(t *testing.T) {
	testCfg("http://127.0.0.1:1")
	if got := bash("true"); got != "(无输出)" {
		t.Errorf("空输出得到 %q", got)
	}
	cfg.toolTO = 0.3
	if got := bash("sleep 5"); got != "[错误]命令超时(>0.3s)" {
		t.Errorf("超时得到 %q", got)
	}
	cfg.toolTO = 2
	if got := bash("sleep 5"); got != "[错误]命令超时(>2s)" {
		t.Errorf("超时得到 %q", got)
	}
}

func TestBashLargeOutputDoesNotDeadlock(t *testing.T) {
	testCfg("http://127.0.0.1:1")
	got := bash("yes 中文 | head -c 200000")
	if len([]rune(got)) < 60000 {
		t.Errorf("大输出被截断或死锁:%d", len([]rune(got)))
	}
}

func TestReadMissingIsToolError(t *testing.T) {
	testCfg("http://127.0.0.1:1")
	fp := filepath.Join(t.TempDir(), "nope")
	got := runTool(tc("c", "tool_read", fmt.Sprintf(`{"fp":%q}`, fp)))
	if !strings.HasPrefix(got, "[错误]工具 tool_read 调用失败:") {
		t.Errorf("得到 %q", got)
	}
}

func TestWriteCreatesParentsAndCountsChars(t *testing.T) {
	testCfg("http://127.0.0.1:1")
	fp := filepath.Join(t.TempDir(), "a", "b", "c.txt")
	got := runTool(tc("c", "tool_write", fmt.Sprintf(`{"fp":%q,"data":"中文abc"}`, fp)))
	if got != "OK—写入5字符" {
		t.Errorf("得到 %q", got)
	}
	if b, err := os.ReadFile(fp); err != nil || string(b) != "中文abc" {
		t.Errorf("文件内容不对:%v %q", err, b)
	}
}

func TestRunToolErrorPaths(t *testing.T) {
	testCfg("http://127.0.0.1:1")
	if got := runTool(tc("c", "nope", "{}")); !strings.Contains(got, "未知工具:nope") ||
		!strings.HasPrefix(got, "[错误]工具 nope 调用失败:") {
		t.Errorf("未知工具得到 %q", got)
	}
	if got := runTool(tc("c", "tool_bash", "not-json")); !strings.HasPrefix(got, "[错误]工具 tool_bash 调用失败:") {
		t.Errorf("坏 JSON 得到 %q", got)
	}
	if got := runTool(tc("c", "tool_bash", "[1,2]")); !strings.HasPrefix(got, "[错误]工具 tool_bash 调用失败:") {
		t.Errorf("非对象参数得到 %q", got)
	}
}

func TestTruncationCountsCharsNotBytes(t *testing.T) {
	testCfg("http://127.0.0.1:1")
	fp := filepath.Join(t.TempDir(), "big.txt")
	if err := os.WriteFile(fp, []byte(strings.Repeat("中", 20000)), 0o644); err != nil {
		t.Fatal(err)
	}
	got := runTool(tc("c", "tool_read", fmt.Sprintf(`{"fp":%q}`, fp)))
	if !strings.HasSuffix(got, "[已截断,原始 20000 字符]") {
		t.Fatalf("未截断:%q", got)
	}
	i := strings.Index(got, "…[已截断")
	if n := len([]rune(got[:i])) - 1; n != 16000 { // 减去标记前的换行
		t.Errorf("截断长度 %d,期望 16000 字符", n)
	}
}

func TestTrimKeepsSystemAndPairing(t *testing.T) {
	testCfg("http://127.0.0.1:1")
	cfg.maxHist = 4
	ctx := []any{map[string]any{"role": "system"}}
	for i := 0; i < 10; i++ {
		ctx = append(ctx, map[string]any{"role": "user", "content": "u"},
			msgCalls(tc(fmt.Sprintf("c%d", i), "tool_bash", "{}")),
			map[string]any{"role": "tool", "tool_call_id": fmt.Sprintf("c%d", i), "content": "ok"})
	}
	out := trim(ctx)
	if out[0].(map[string]any)["role"] != "system" {
		t.Errorf("首条不是 system")
	}
	if out[1].(map[string]any)["role"] == "tool" {
		t.Errorf("裁剪点落在了 tool 响应上")
	}
	if len(out) > cfg.maxHist+3 {
		t.Errorf("裁剪后 %d 条,超出预期", len(out))
	}
	legality(t, out)
}

func TestTrimSmallHistoryUntouched(t *testing.T) {
	testCfg("http://127.0.0.1:1")
	ctx := []any{map[string]any{"role": "system"}, map[string]any{"role": "user", "content": "x"}}
	if len(trim(ctx)) != 2 {
		t.Errorf("小历史不应被裁剪")
	}
}

// 裁剪点落在连续 tool 结果中间时不得越界(旧实现在这里会 panic)。
func TestTrimDoesNotRunOffEnd(t *testing.T) {
	testCfg("http://127.0.0.1:1")
	cfg.maxHist = 1
	ctx := []any{map[string]any{"role": "system"}}
	for i := 0; i < 5; i++ {
		ctx = append(ctx, map[string]any{"role": "tool", "tool_call_id": "x", "content": "r"})
	}
	if out := trim(ctx); out[0].(map[string]any)["role"] != "system" {
		t.Errorf("得到 %#v", out)
	}
}

func TestChatKeepsHTTPErrorBody(t *testing.T) {
	m := newMock(func(int, map[string]any) (int, string) {
		return 400, `{"error":{"message":"boom-detail"}}`
	})
	defer m.srv.Close()
	testCfg(m.srv.URL)
	_, err := chat([]any{map[string]any{"role": "system"}})
	if err == nil || !strings.Contains(err.Error(), "HTTP 400 Bad Request:") || !strings.Contains(err.Error(), "boom-detail") {
		t.Errorf("得到 %v", err)
	}
}

func TestChatReportsNetworkError(t *testing.T) {
	testCfg("http://127.0.0.1:1")
	if _, err := chat([]any{}); err == nil || !strings.HasPrefix(err.Error(), "网络错误:") {
		t.Errorf("得到 %v", err)
	}
}

// 端到端:假服务逐请求断言历史合法,并让 agent 跑完工具循环。
func TestAgentLoopEndToEnd(t *testing.T) {
	out := filepath.Join(t.TempDir(), "o.txt")
	m := newMock(func(_ int, body map[string]any) (int, string) {
		msgs, _ := body["messages"].([]any)
		n := 0
		for _, raw := range msgs {
			if mm, ok := raw.(map[string]any); ok && mm["role"] == "assistant" && mm["tool_calls"] != nil {
				n++
			}
		}
		switch n {
		case 0:
			return 200, response(msgCalls(
				tc("call_1", "tool_bash", `{"cmd":"echo hi"}`),
				tc("call_2", "nope", "{}"),
				tc("call_3", "tool_bash", "not-json"),
				tc("call_4", "tool_write", fmt.Sprintf(`{"fp":%q,"data":"x"}`, out))))
		case 1:
			return 200, response(msgCalls(tc("call_5", "tool_read", fmt.Sprintf(`{"fp":%q}`, out))))
		}
		return 200, response(msgText("ALL-OK"))
	})
	defer m.srv.Close()
	testCfg(m.srv.URL)
	ctx := []any{map[string]any{"role": "system", "content": sysPrompt}}
	if got := agentLoop(&ctx, "任务"); got != "ALL-OK" {
		t.Fatalf("得到 %q", got)
	}
	bodies := m.bodies()
	if len(bodies) != 3 {
		t.Fatalf("期望 3 次请求,得到 %d", len(bodies))
	}
	for _, b := range bodies {
		legality(t, b["messages"].([]any))
	}
	tools := 0
	for _, raw := range ctx {
		if raw.(map[string]any)["role"] == "tool" {
			tools++
		}
	}
	if tools != 5 {
		t.Errorf("期望 5 条 tool 结果,得到 %d", tools)
	}
	legality(t, ctx)
}

func TestAgentLoopClassifiesContextOverflow(t *testing.T) {
	m := newMock(func(int, map[string]any) (int, string) {
		return 400, `{"error":{"message":"maximum context length is 8192 tokens"}}`
	})
	defer m.srv.Close()
	testCfg(m.srv.URL)
	ctx := []any{map[string]any{"role": "system", "content": sysPrompt}}
	if got := agentLoop(&ctx, "任务"); !strings.HasPrefix(got, "[错误]上下文过长,请重开会话:") {
		t.Errorf("得到 %q", got)
	}
}

func TestAgentLoopReportsAPIFailure(t *testing.T) {
	m := newMock(func(int, map[string]any) (int, string) {
		return 401, `{"error":{"message":"bad key"}}`
	})
	defer m.srv.Close()
	testCfg(m.srv.URL)
	ctx := []any{map[string]any{"role": "system", "content": sysPrompt}}
	if got := agentLoop(&ctx, "任务"); !strings.HasPrefix(got, "[错误]API失败:HTTP 401") {
		t.Errorf("得到 %q", got)
	}
}

func TestAgentLoopStopsAtMaxTurns(t *testing.T) {
	m := newMock(func(int, map[string]any) (int, string) {
		return 200, response(msgCalls(tc("c", "tool_bash", `{"cmd":"true"}`)))
	})
	defer m.srv.Close()
	testCfg(m.srv.URL)
	cfg.turns = 3
	ctx := []any{map[string]any{"role": "system", "content": sysPrompt}}
	if got := agentLoop(&ctx, "任务"); !strings.Contains(got, "达到最大轮次(3)") {
		t.Errorf("得到 %q", got)
	}
	if len(m.bodies()) != 3 {
		t.Errorf("期望 3 次请求")
	}
	legality(t, ctx)
}

// 长工具循环下,历史必须始终有界且合法。
func TestAgentLoopHistoryStaysBounded(t *testing.T) {
	m := newMock(func(i int, _ map[string]any) (int, string) {
		if i >= 24 {
			return 200, response(msgText("TRIM-OK"))
		}
		return 200, response(msgCalls(tc(fmt.Sprintf("c%d", i), "tool_bash", `{"cmd":"true"}`)))
	})
	defer m.srv.Close()
	testCfg(m.srv.URL)
	cfg.maxHist, cfg.turns = 6, 40
	ctx := []any{map[string]any{"role": "system", "content": sysPrompt}}
	if got := agentLoop(&ctx, "任务"); got != "TRIM-OK" {
		t.Fatalf("得到 %q", got)
	}
	bodies := m.bodies()
	if len(bodies) != 25 {
		t.Fatalf("期望 25 次请求,得到 %d", len(bodies))
	}
	peak := 0
	for _, b := range bodies {
		msgs := b["messages"].([]any)
		if len(msgs) > peak {
			peak = len(msgs)
		}
		legality(t, msgs)
	}
	if peak > 9 {
		t.Errorf("历史未被有界裁剪,峰值 %d 条", peak)
	}
}

func TestLoadReadsEnvWithFallbacks(t *testing.T) {
	for _, k := range []string{"LLM_API_KEY", "DEEPSEEK_API_KEY", "OPENAI_API_KEY", "LLM_MODEL", "LLM_MAX_HISTORY", "LLM_BASE_URL", "LLM_TOOL_TIMEOUT"} {
		t.Setenv(k, "")
	}
	t.Setenv("DEEPSEEK_API_KEY", "dk")
	t.Setenv("LLM_MODEL", "m1")
	t.Setenv("LLM_MAX_HISTORY", "0")
	t.Setenv("LLM_BASE_URL", "http://x/v1/")
	t.Setenv("LLM_TOOL_TIMEOUT", "2.5")
	c := load()
	if c.key != "dk" || c.model != "m1" {
		t.Errorf("env 读取错误:%+v", c)
	}
	if c.maxHist != 1 {
		t.Errorf("maxHist 下限失效:%d", c.maxHist)
	}
	if c.url != "http://x/v1" {
		t.Errorf("url 未去掉尾部斜杠:%q", c.url)
	}
	if c.toolTO != 2.5 {
		t.Errorf("toolTO 解析错误:%v", c.toolTO)
	}
	if c.turns != 15 {
		t.Errorf("turns 默认值错误:%d", c.turns)
	}
}
