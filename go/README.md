# go/ — 极简 LLM Agent(纯标准库)

参考 \`agent_100_line/agent.py\` 的思路,用 Go 标准库实现一个带工具调用的 LLM
Agent,**不依赖任何第三方库**——直接用 \`net/http\` 调用 OpenAI 兼容的
\`/chat/completions\` 接口。行为与 [python/](../python/) 版逐条等价。

## 快速开始

\`\`\`bash
export LLM_API_KEY=sk-...

go run .                      # 交互模式
go run . "看看当前目录有什么"   # 一次性执行后退出
\`\`\`

交互模式下输入 \`exit\` / \`quit\`、Ctrl-C 或 Ctrl-D 退出。也可以 \`go build\`
后直接运行生成的二进制。

## 环境变量

| 变量 | 默认值 | 说明 |
|---|---|---|
| \`LLM_API_KEY\` | (无,必填) | 也接受 \`DEEPSEEK_API_KEY\` / \`OPENAI_API_KEY\` |
| \`LLM_BASE_URL\` | \`https://api.deepseek.com/v1\` | 其他兼容后端如 \`https://api.scnet.cn/api/llm/v1\` |
| \`LLM_MODEL\` | \`deepseek-chat\` | 模型名 |
| \`LLM_MAX_TURNS\` | \`15\` | 单轮最多工具循环次数 |
| \`LLM_TIMEOUT\` | \`120\` | 单次 API 请求超时(秒) |
| \`LLM_TOOL_TIMEOUT\` | \`60\` | 单个工具执行超时(秒) |
| \`LLM_MAX_TOOL_CHARS\` | \`16000\` | 回填给模型的单个工具结果上限(按字符) |
| \`LLM_MAX_HISTORY\` | \`40\` | 保留的最近消息条数,超出则裁剪最旧历史 |

## 内置工具

| 工具 | 参数 | 作用 |
|---|---|---|
| \`tool_bash\` | \`cmd\` | 执行系统命令(优先用 \`/bin/bash\`) |
| \`tool_read\` | \`fp\` | 读取文件 |
| \`tool_write\` | \`fp\`, \`data\` | 写入文件(自动创建父目录) |

> 需要跑 Python 时直接用 \`tool_bash\`(例如 \`python3 -c '...'\`)。

## 设计要点

- **对话历史始终合法**:参数解析、查表、调用工具中的任何异常都会转成一条
  \`tool\` 结果写回,保证每个 \`tool_call\` 都有配对响应(否则下一轮请求会 400)。
- **上下文有界**:超过 \`LLM_MAX_HISTORY\` 时,在**每次请求前的安全边界**裁剪最旧
  历史;裁剪点落在 \`tool\` 结果上会继续前移,绝不拆开 \`tool_calls\` 与它的响应。
- **错误分类**:工具自身错误标为 \`[错误]工具 … 调用失败\`,不会误报成
  \`API失败\`;HTTP 错误保留响应体;上下文超长单独提示。
- **按字符截断**:\`LLM_MAX_TOOL_CHARS\` 以**字符**(rune)计,与 Python 版一致,
  中文等多字节内容不会被腰斩。
- **配置显式传递**:全部参数收在 \`Config\` 结构体里,测试无需改全局环境变量。

## 测试

\`\`\`bash
go test ./...
gofmt -l . && go vet ./...
\`\`\`

测试全部离线:工具、裁剪、历史合法性;端到端用 \`httptest\` 起本地假服务,
并在**每一次请求**上断言历史合法(首条为 system、\`tool_call\` 与响应一一配对)
与消息数有界。不联网、不需要真实 API Key。

## ⚠️ 安全提示

\`tool_bash\` 能以当前用户权限执行**任意命令**,\`tool_write\` 能写**任意路径**。
接入外部输入前务必考虑 prompt injection,生产环境建议加命令白名单、限制工作目录,
或在沙箱/容器中运行。
