# python/ — 极简 LLM Agent(纯标准库)

参考 \`agent_100_line/agent.py\` 的思路,用 Python 标准库实现一个带工具调用的
LLM Agent,**不依赖 \`openai\` 库**——直接通过 \`urllib\` 调用 OpenAI 兼容的
\`/chat/completions\` 接口。\`mvp_agent.py\` 保持在 100 行以内。

## 快速开始

\`\`\`bash
export LLM_API_KEY=sk-...

python mvp_agent.py                      # 交互模式
python mvp_agent.py "看看当前目录有什么"   # 一次性执行后退出
\`\`\`

交互模式下输入 \`exit\` / \`quit\`、Ctrl-C 或 Ctrl-D 退出。

## 环境变量

| 变量 | 默认值 | 说明 |
|---|---|---|
| \`LLM_API_KEY\` | (无,必填) | 也接受 \`DEEPSEEK_API_KEY\` / \`OPENAI_API_KEY\` |
| \`LLM_BASE_URL\` | \`https://api.deepseek.com/v1\` | 其他兼容后端如 \`https://api.scnet.cn/api/llm/v1\` |
| \`LLM_MODEL\` | \`deepseek-chat\` | 模型名 |
| \`LLM_MAX_TURNS\` | \`15\` | 单轮最多工具循环次数 |
| \`LLM_TIMEOUT\` | \`120\` | 单次 API 请求超时(秒) |
| \`LLM_TOOL_TIMEOUT\` | \`60\` | 单个工具执行超时(秒) |
| \`LLM_MAX_TOOL_CHARS\` | \`16000\` | 回填给模型的单个工具结果上限 |
| \`LLM_MAX_HISTORY\` | \`40\` | 保留的最近消息条数,超出则裁剪最旧历史 |

## 内置工具

| 工具 | 参数 | 作用 |
|---|---|---|
| \`tool_bash\` | \`cmd\` | 执行系统命令(优先用 \`/bin/bash\`) |
| \`tool_read\` | \`fp\` | 读取文件 |
| \`tool_write\` | \`fp\`, \`data\` | 写入文件(自动创建父目录) |

> 需要跑 Python 时直接用 \`tool_bash\`(例如 \`python3 -c '...'\`),因此不再单独提供 \`tool_python\`。

## 设计要点

- **对话历史始终合法**:解析参数、查表、调用工具中的任何异常都会被转成一条
  \`tool\` 结果写回,保证每个 \`tool_call\` 都有配对的响应(否则下一轮请求会 400)。
- **上下文有界**:超过 \`LLM_MAX_HISTORY\` 时在**安全边界**(每次请求前,此时上一轮
  \`tool_calls\` 都已有响应)裁剪最旧历史;裁剪点落在 \`tool\` 结果上会继续前移,
  绝不把一个 \`tool_call\` 和它的响应拆开。
- **错误分类**:工具自身的错误标记为 \`[错误]工具 … 调用失败\`,不会误报成
  \`API失败\`;HTTP 错误会保留响应体,便于排查 400/401;上下文超长会单独提示。
- **输出控制**:工具输出超过 \`LLM_MAX_TOOL_CHARS\` 会截断并标注原始长度。
- **更严的 schema**:所有工具都带 \`additionalProperties: false\`,减少模型乱传参数。

## 测试

\`\`\`bash
python3 -m unittest -v test_mvp_agent.py
\`\`\`

25 个用例全部使用假响应 / mock,不联网、不需要真实 API Key。

## ⚠️ 安全提示

\`tool_bash\` 能以当前用户权限执行**任意命令**,\`tool_write\` 能写**任意路径**。
接外部输入前务必考虑 prompt injection(模型可能被诱导执行 \`rm -rf\` 之类),
生产环境建议加命令白名单、限制工作目录,或在沙箱/容器中运行。
