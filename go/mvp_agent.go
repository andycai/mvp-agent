// mvp_agent.go —— 极简 LLM Agent(Go 标准库实现,不依赖任何第三方库)。
// 用 net/http 调用 OpenAI 兼容的 /chat/completions。
// 用法:export LLM_API_KEY=sk-... && go run . [任务](环境变量见 README.md)
package main

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

const sysPrompt = `你是AI助手,可用工具:
1. tool_bash — 执行系统命令(bash)
2. tool_read — 读取文件
3. tool_write — 写入文件

请逐步思考。当需要与文件系统交互、运行命令或执行代码等等时调用工具,完成后直接回复。`

// Config 保存全部运行参数(显式传递,便于测试)。
type Config struct {
	APIKey       string
	BaseURL      string
	Model        string
	MaxTurns     int
	Timeout      time.Duration
	ToolTimeout  time.Duration
	MaxToolChars int
	MaxHistory   int
	Bash         string
}

func env(k, def string) string {
	if v := os.Getenv(k); v != "" {
		return v
	}
	return def
}

func envInt(k string, def int) int {
	if v, err := strconv.Atoi(os.Getenv(k)); err == nil {
		return v
	}
	return def
}

func envFloat(k string, def float64) float64 {
	if v, err := strconv.ParseFloat(os.Getenv(k), 64); err == nil {
		return v
	}
	return def
}

func firstNonEmpty(ss ...string) string {
	for _, s := range ss {
		if s != "" {
			return s
		}
	}
	return ""
}

// ConfigFromEnv 从环境变量读取配置。
func ConfigFromEnv() *Config {
	bash := "/bin/bash"
	if _, err := os.Stat(bash); err != nil {
		bash = "sh"
	}
	h := envInt("LLM_MAX_HISTORY", 40)
	if h < 1 {
		h = 1
	}
	return &Config{
		APIKey: firstNonEmpty(os.Getenv("LLM_API_KEY"), os.Getenv("DEEPSEEK_API_KEY"),
			os.Getenv("OPENAI_API_KEY")),
		BaseURL:      strings.TrimRight(env("LLM_BASE_URL", "https://api.deepseek.com/v1"), "/"),
		Model:        env("LLM_MODEL", "deepseek-chat"),
		MaxTurns:     envInt("LLM_MAX_TURNS", 15),
		Timeout:      time.Duration(envFloat("LLM_TIMEOUT", 120) * float64(time.Second)),
		ToolTimeout:  time.Duration(envFloat("LLM_TOOL_TIMEOUT", 60) * float64(time.Second)),
		MaxToolChars: envInt("LLM_MAX_TOOL_CHARS", 16000),
		MaxHistory:   h,
		Bash:         bash,
	}
}

func prop(t, d string) map[string]any { return map[string]any{"type": t, "description": d} }

func spec(name, desc string, props map[string]any, req ...string) map[string]any {
	return map[string]any{"type": "function", "function": map[string]any{
		"name": name, "description": desc,
		"parameters": map[string]any{"type": "object", "properties": props,
			"required": req, "additionalProperties": false}}}
}

// ToolSchemas 返回三个工具的 JSON Schema。
func ToolSchemas() []map[string]any {
	return []map[string]any{
		spec("tool_bash", "执行系统命令", map[string]any{"cmd": prop("string", "命令")}, "cmd"),
		spec("tool_read", "读取文件", map[string]any{"fp": prop("string", "文件路径")}, "fp"),
		spec("tool_write", "写入文件",
			map[string]any{"fp": prop("string", "文件路径"), "data": prop("string", "内容")}, "fp", "data"),
	}
}

type toolFunc func(map[string]any, *Config) string

func toolBash(a map[string]any, c *Config) string {
	cmd, _ := a["cmd"].(string)
	ctx, cancel := context.WithTimeout(context.Background(), c.ToolTimeout)
	defer cancel()
	p := exec.CommandContext(ctx, c.Bash, "-c", cmd)
	var so, se bytes.Buffer
	p.Stdout, p.Stderr = &so, &se
	_ = p.Run()
	if ctx.Err() == context.DeadlineExceeded {
		return fmt.Sprintf("[错误]命令超时(>%gs)", c.ToolTimeout.Seconds())
	}
	out := so.String()
	if se.Len() > 0 {
		out += "\n[stderr]\n" + se.String()
	}
	if p.ProcessState != nil && p.ProcessState.ExitCode() != 0 {
		out += fmt.Sprintf("\n[exit:%d]", p.ProcessState.ExitCode())
	}
	if out = strings.TrimSpace(out); out == "" {
		return "(无输出)"
	}
	return out
}

func toolRead(a map[string]any, c *Config) string {
	fp, _ := a["fp"].(string)
	b, err := os.ReadFile(fp)
	if err != nil {
		return "[错误]" + err.Error()
	}
	return strings.ToValidUTF8(string(b), "�")
}

func toolWrite(a map[string]any, c *Config) string {
	fp, _ := a["fp"].(string)
	data, _ := a["data"].(string)
	_ = os.MkdirAll(filepath.Dir(fp), 0o755)
	if err := os.WriteFile(fp, []byte(data), 0o644); err != nil {
		return "[错误]" + err.Error()
	}
	return fmt.Sprintf("OK—写入%d字符", len([]rune(data)))
}

var toolFuncs = map[string]toolFunc{
	"tool_bash": toolBash, "tool_read": toolRead, "tool_write": toolWrite}

func clip(s string, n int) string {
	if r := []rune(s); len(r) > n {
		return string(r[:n])
	}
	return s
}

// RunTool 执行一次工具调用;任何失败都转成文本结果,保证历史始终配对。
func RunTool(tc map[string]any, c *Config) string {
	fn, _ := tc["function"].(map[string]any)
	name, _ := fn["name"].(string)
	raw, _ := fn["arguments"].(string)
	if raw == "" {
		raw = "{}"
	}
	var args map[string]any
	if err := json.Unmarshal([]byte(raw), &args); err != nil {
		return fmt.Sprintf("[错误]工具 %s 调用失败:ValueError: arguments 必须是 JSON 对象:%v", name, err)
	}
	f, ok := toolFuncs[name]
	if !ok {
		return fmt.Sprintf("[错误]工具 %s 调用失败:LookupError: 未知工具:%s", name, name)
	}
	out := f(args, c)
	if r := []rune(out); len(r) > c.MaxToolChars {
		return string(r[:c.MaxToolChars]) + fmt.Sprintf("\n…[已截断,原始 %d 字符]", len(r))
	}
	return out
}

// Chat 调用一次 /chat/completions,返回 choices[0].message。
func Chat(c *Config, msgs []map[string]any) (map[string]any, error) {
	body, err := json.Marshal(map[string]any{
		"model": c.Model, "messages": msgs, "tools": ToolSchemas()})
	if err != nil {
		return nil, fmt.Errorf("序列化失败:%v", err)
	}
	req, err := http.NewRequest("POST", c.BaseURL+"/chat/completions", bytes.NewReader(body))
	if err != nil {
		return nil, fmt.Errorf("网络错误:%v", err)
	}
	req.Header.Set("Content-Type", "application/json; charset=utf-8")
	req.Header.Set("Authorization", "Bearer "+c.APIKey)
	req.Header.Set("User-Agent", "mvp-agent/1.0")
	resp, err := (&http.Client{Timeout: c.Timeout}).Do(req)
	if err != nil {
		return nil, fmt.Errorf("网络错误:%v", err)
	}
	defer resp.Body.Close()
	data, _ := io.ReadAll(resp.Body)
	if resp.StatusCode < 200 || resp.StatusCode > 299 {
		return nil, fmt.Errorf("HTTP %d %s: %s", resp.StatusCode,
			http.StatusText(resp.StatusCode), clip(string(data), 1000))
	}
	var root map[string]any
	if err := json.Unmarshal(data, &root); err != nil {
		return nil, fmt.Errorf("响应不是合法 JSON:%v", err)
	}
	choices, _ := root["choices"].([]any)
	if len(choices) == 0 {
		return nil, fmt.Errorf("响应缺少 choices")
	}
	first, _ := choices[0].(map[string]any)
	msg, _ := first["message"].(map[string]any)
	if msg == nil {
		return nil, fmt.Errorf("响应缺少 message")
	}
	return msg, nil
}

// TrimHistory 裁剪过旧历史,但绝不拆开 tool_calls 与它的响应。
func TrimHistory(c *Config, ctx []map[string]any) []map[string]any {
	if len(ctx) <= c.MaxHistory+1 {
		return ctx
	}
	cut := len(ctx) - c.MaxHistory
	for cut < len(ctx) && ctx[cut]["role"] == "tool" {
		cut++
	}
	out := make([]map[string]any, 0, 1+len(ctx)-cut)
	out = append(out, ctx[0])
	return append(out, ctx[cut:]...)
}

// AgentLoop 执行一轮完整对话(可能包含多次工具调用)。
func AgentLoop(c *Config, input string, ctx *[]map[string]any) string {
	*ctx = append(*ctx, map[string]any{"role": "user", "content": input})
	for i := 0; i < c.MaxTurns; i++ {
		*ctx = TrimHistory(c, *ctx) // 安全边界:此刻上一轮 tool_calls 都已有配对响应
		m, err := Chat(c, *ctx)
		if err != nil {
			s := err.Error()
			kind := "API失败"
			if low := strings.ToLower(s); strings.Contains(low, "context") || strings.Contains(low, "token") {
				kind = "上下文过长,请重开会话"
			}
			return fmt.Sprintf("[错误]%s:%s", kind, s)
		}
		msg := map[string]any{}
		for _, k := range []string{"role", "content", "tool_calls"} {
			if v, ok := m[k]; ok {
				msg[k] = v
			}
		}
		*ctx = append(*ctx, msg)
		tcs, _ := m["tool_calls"].([]any)
		if len(tcs) == 0 {
			s, _ := m["content"].(string)
			return s
		}
		for _, t := range tcs {
			tc, _ := t.(map[string]any)
			fn, _ := tc["function"].(map[string]any)
			argStr, _ := fn["arguments"].(string)
			log.Printf("[tool_call]%v(%s)", fn["name"], argStr)
			*ctx = append(*ctx, map[string]any{
				"role": "tool", "tool_call_id": tc["id"], "content": RunTool(tc, c)})
		}
	}
	return fmt.Sprintf("[Agent]防止死循环,达到最大轮次(%d),中断", c.MaxTurns)
}

func main() {
	log.SetFlags(0)
	c := ConfigFromEnv()
	if c.APIKey == "" {
		fmt.Fprintln(os.Stderr, "错误:请设置LLM_API_KEY")
		os.Exit(1)
	}
	ctx := []map[string]any{{"role": "system", "content": sysPrompt}}
	if len(os.Args) > 1 { // 非交互:go run . "任务"
		fmt.Printf("助手>%s\n", AgentLoop(c, strings.Join(os.Args[1:], " "), &ctx))
		return
	}
	log.Printf("智能助手 %s 就绪。输入消息(或'exit'退出)。\n", c.Model)
	in := bufio.NewScanner(os.Stdin)
	in.Buffer(make([]byte, 1<<20), 1<<20)
	for {
		fmt.Print("用户> ")
		if !in.Scan() {
			fmt.Println("\n再见。")
			return
		}
		s := strings.TrimSpace(in.Text())
		if s == "" {
			continue
		}
		if s == "exit" || s == "quit" {
			fmt.Println("再见。")
			return
		}
		log.Printf("处理请求:%s", s)
		fmt.Printf("\n助手>%s\n\n", AgentLoop(c, s, &ctx))
	}
}
