// mvp_agent.go —— 100 行以内的 LLM Agent(Go 版,纯标准库):net/http 调用 OpenAI 兼容的 /chat/completions。为压进 100 行,刻意采用紧凑排版(gofmt 会展开它),与 python/mvp_agent.py 风格一致。用法:export LLM_API_KEY=sk-... && go run . [任务]
package main

import (
	"bufio"; "bytes"; "cmp"; "context"; "encoding/json"; "errors"; "fmt"
	"io"; "net/http"; "os"; "os/exec"; "path/filepath"; "strconv"
	"strings"; "time"
)
const sysPrompt = `你是AI助手,可用工具:
1. tool_bash — 执行系统命令(bash)
2. tool_read — 读取文件
3. tool_write — 写入文件

请逐步思考。当需要与文件系统交互、运行命令或执行代码等等时调用工具,完成后直接回复。`
type Cfg struct{ key, url, model string; turns, maxChar, maxHist int; timeout, toolTO float64 }

var cfg = load()

func load() Cfg { return Cfg{key: cmp.Or(os.Getenv("LLM_API_KEY"), os.Getenv("DEEPSEEK_API_KEY"), os.Getenv("OPENAI_API_KEY")), url: strings.TrimRight(env("LLM_BASE_URL", "https://api.deepseek.com/v1"), "/"), model: env("LLM_MODEL", "deepseek-chat"), turns: envI("LLM_MAX_TURNS", 15), timeout: envF("LLM_TIMEOUT", 120), toolTO: envF("LLM_TOOL_TIMEOUT", 60), maxChar: envI("LLM_MAX_TOOL_CHARS", 16000), maxHist: max(1, envI("LLM_MAX_HISTORY", 40))} }
func env(k, d string) string { return cmp.Or(os.Getenv(k), d) }
func envI(k string, d int) int { n, _ := strconv.Atoi(env(k, strconv.Itoa(d))); return n }
func envF(k string, d float64) float64 { f, _ := strconv.ParseFloat(env(k, strconv.FormatFloat(d, 'g', -1, 64)), 64); return f }
func prop(d string) map[string]any { return map[string]any{"type": "string", "description": d} }
func tool(name, desc string, props map[string]any, req ...string) map[string]any { return map[string]any{"type": "function", "function": map[string]any{"name": name, "description": desc, "parameters": map[string]any{"type": "object", "properties": props, "required": req, "additionalProperties": false}}} }
func schemas() []any { return []any{tool("tool_bash", "执行系统命令", map[string]any{"cmd": prop("命令")}, "cmd"), tool("tool_read", "读取文件", map[string]any{"fp": prop("文件路径")}, "fp"), tool("tool_write", "写入文件", map[string]any{"fp": prop("文件路径"), "data": prop("内容")}, "fp", "data")} }
func head(s string, n int) string { r := []rune(s); return string(r[:min(len(r), n)]) }
func repeat(s string, on int) string { return strings.Repeat(s, max(0, min(1, on))) }
var shell = func() string { if _, e := os.Stat("/bin/bash"); e == nil { return "/bin/bash" }; return "sh" }()
func bash(cmd string) string {
	ctx, cancel := context.WithTimeout(context.Background(), time.Duration(cfg.toolTO*float64(time.Second))); defer cancel()
	c := exec.CommandContext(ctx, shell, "-c", cmd); var so, se bytes.Buffer; c.Stdout, c.Stderr = &so, &se; _ = c.Run()
	if ctx.Err() != nil { return fmt.Sprintf("[错误]命令超时(>%gs)", cfg.toolTO) }
	if c.ProcessState == nil { return "[错误]命令执行失败" }
	out := strings.TrimSpace(so.String() + repeat("\n[stderr]\n"+se.String(), se.Len()) + repeat(fmt.Sprintf("\n[exit:%d]", c.ProcessState.ExitCode()), c.ProcessState.ExitCode()))
	return cmp.Or(out, "(无输出)")
}
func readF(fp string) (string, error) { b, err := os.ReadFile(fp); return strings.ToValidUTF8(string(b), "\uFFFD"), err }
func writeF(fp, data string) (string, error) { return fmt.Sprintf("OK—写入%d字符", len([]rune(data))), errors.Join(os.MkdirAll(filepath.Dir(fp), 0o755), os.WriteFile(fp, []byte(data), 0o644)) }
func str(a map[string]any, k string) string { v, _ := a[k].(string); return v }
var tools = map[string]func(map[string]any) (string, error){
	"tool_bash":  func(a map[string]any) (string, error) { return bash(str(a, "cmd")), nil },
	"tool_read":  func(a map[string]any) (string, error) { return readF(str(a, "fp")) },
	"tool_write": func(a map[string]any) (string, error) { return writeF(str(a, "fp"), str(a, "data")) },
}
func runTool(tc map[string]any) string {
	f, _ := tc["function"].(map[string]any); name, _ := f["name"].(string); raw, _ := f["arguments"].(string)
	var args map[string]any; err := json.Unmarshal([]byte(cmp.Or(strings.TrimSpace(raw), "{}")), &args)
	if err == nil && tools[name] == nil { err = fmt.Errorf("LookupError: 未知工具:%s", name) }
	out := ""; if err == nil { out, err = tools[name](args) }
	if err != nil { out = fmt.Sprintf("[错误]工具 %s 调用失败:%v", cmp.Or(name, "?"), err) }
	n := len([]rune(out))
	return head(out, cfg.maxChar) + repeat(fmt.Sprintf("\n…[已截断,原始 %d 字符]", n), n-cfg.maxChar)
}
func trim(ctx []any) []any {
	if len(ctx) <= cfg.maxHist+1 { return ctx }
	cut := len(ctx) - cfg.maxHist
	for cut < len(ctx) && ctx[cut].(map[string]any)["role"] == "tool" { cut++ }
	return append([]any{ctx[0]}, ctx[cut:]...)
}
func chat(msgs []any) (map[string]any, error) {
	body, _ := json.Marshal(map[string]any{"model": cfg.model, "messages": msgs, "tools": schemas()})
	req, _ := http.NewRequest("POST", cfg.url+"/chat/completions", bytes.NewReader(body))
	req.Header.Set("Content-Type", "application/json; charset=utf-8"); req.Header.Set("Authorization", "Bearer "+cfg.key); req.Header.Set("User-Agent", "mvp-agent/1.0")
	resp, err := (&http.Client{Timeout: time.Duration(cfg.timeout * float64(time.Second))}).Do(req)
	if err != nil { return nil, fmt.Errorf("网络错误:%w", err) }
	defer resp.Body.Close(); data, _ := io.ReadAll(resp.Body)
	if resp.StatusCode > 299 { return nil, fmt.Errorf("HTTP %d %s: %s", resp.StatusCode, http.StatusText(resp.StatusCode), head(string(data), 1000)) }
	var root struct{ Choices []struct{ Message map[string]any } }
	if err := json.Unmarshal(data, &root); err != nil { return nil, fmt.Errorf("网络错误:%w", err) }
	if len(root.Choices) == 0 { return nil, errors.New("网络错误:响应缺少 choices[0].message") }
	return root.Choices[0].Message, nil
}
func agentLoop(ctx *[]any, input string) string {
	*ctx = append(*ctx, map[string]any{"role": "user", "content": input})
	for range cfg.turns {
		*ctx = trim(*ctx)
		m, err := chat(*ctx)
		if err != nil { l := strings.ToLower(err.Error()); return fmt.Sprintf("[错误]%s:%v", map[bool]string{true: "上下文过长,请重开会话", false: "API失败"}[strings.Contains(l, "context") || strings.Contains(l, "token")], err) }
		a := map[string]any{}; for _, k := range []string{"role", "content", "tool_calls"} { if v, ok := m[k]; ok { a[k] = v } }; *ctx = append(*ctx, a)
		calls, _ := m["tool_calls"].([]any)
		if len(calls) == 0 { s, _ := m["content"].(string); return s }
		for _, t := range calls { tc, _ := t.(map[string]any); f, _ := tc["function"].(map[string]any); fmt.Fprintf(os.Stderr, "[tool_call]%v(%v)\n", f["name"], f["arguments"]); *ctx = append(*ctx, map[string]any{"role": "tool", "tool_call_id": tc["id"], "content": runTool(tc)}) }
	}
	return fmt.Sprintf("[Agent]防止死循环,达到最大轮次(%d),中断", cfg.turns)
}
func main() {
	if cfg.key == "" { fmt.Fprintln(os.Stderr, "错误:请设置LLM_API_KEY"); os.Exit(1) }
	ctx := []any{map[string]any{"role": "system", "content": sysPrompt}}
	if len(os.Args) > 1 { fmt.Printf("助手>%s\n", agentLoop(&ctx, strings.Join(os.Args[1:], " "))); return }
	fmt.Fprintf(os.Stderr, "智能助手 %s 就绪。输入消息(或'exit'退出)。\n\n", cfg.model)
	in := bufio.NewScanner(os.Stdin)
	for fmt.Print("用户> "); in.Scan(); fmt.Print("用户> ") {
		s := strings.TrimSpace(in.Text())
		if s == "" { continue }
		if s == "exit" || s == "quit" { fmt.Println("再见。"); return }
		fmt.Printf("\n助手>%s\n\n", agentLoop(&ctx, s))
	}
	fmt.Println("\n再见。")
}
