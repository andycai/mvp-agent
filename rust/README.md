# rust/ — 极简 LLM Agent(99 行)

参考 \`agent_100_line/agent.py\` 的思路,用 Rust 实现一个带工具调用的 LLM Agent,
**核心逻辑 99 行**,行为与 [python/](../python/) 版逐条等价。

## 快速开始

\`\`\`bash
export LLM_API_KEY=sk-...

cargo run --quiet --                    # 交互模式
cargo run --quiet -- "看看当前目录有什么" # 一次性执行后退出
\`\`\`

交互模式下输入 \`exit\` / \`quit\`、Ctrl-C 或 Ctrl-D 退出。**不需要**系统装 \`curl\`。

## 依赖(以及为什么需要它们)

Rust 标准库**既没有 HTTP/TLS 客户端,也没有 JSON**。手写一个够用的 JSON 解析器
(对象、数组、转义、\`\\uXXXX\` 与代理对、UTF-16)要 400 多行,再加一个自实现的
HTTP,实现会膨胀到 1000 行以上。所以这里用了三个成熟 crate:

| crate | 用途 |
|---|---|
| [serde_json](https://crates.io/crates/serde_json) | JSON 解析/序列化 |
| [ureq](https://crates.io/crates/ureq) | HTTP(S) 客户端,自带 rustls,无外部命令依赖 |
| [wait-timeout](https://crates.io/crates/wait-timeout) | 给子进程加超时(std 没有) |

代价是三个依赖;收益是核心文件从 1000+ 行降到 **99 行**。

## 环境变量

| 变量 | 默认值 | 说明 |
|---|---|---|
| \`LLM_API_KEY\` | (无,必填) | 也接受 \`DEEPSEEK_API_KEY\` / \`OPENAI_API_KEY\` |
| \`LLM_BASE_URL\` | \`https://api.deepseek.com/v1\` | 其他兼容后端 |
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
| \`tool_read\` | \`fp\` | 读取文件(非法 UTF-8 按 U+FFFD 替换) |
| \`tool_write\` | \`fp\`, \`data\` | 写入文件(自动创建父目录) |

## 设计要点

- **对话历史始终合法**:参数解析、查表、调用工具中的任何异常都会转成一条
  \`tool\` 结果写回,保证每个 \`tool_call\` 都有配对响应。
- **上下文有界**:超过 \`LLM_MAX_HISTORY\` 时在**每次请求前的安全边界**裁剪最旧
  历史;裁剪点落在 \`tool\` 结果上会继续前移,绝不拆开 \`tool_calls\` 与它的响应。
- **工具超时**:\`wait_timeout\` + \`kill\`;stdout/stderr 由独立线程读取,避免
  子进程写满管道后死锁(有回归测试覆盖)。
- **错误分类**:工具错误 \`[错误]工具 … 调用失败\`、接口错误 \`[错误]API失败\`、
  上下文超长 \`[错误]上下文过长,请重开会话\`;HTTP 错误保留响应体。
- **按字符截断**:\`LLM_MAX_TOOL_CHARS\` 以字符计,与 Python 版一致。

## 测试

\`\`\`bash
cargo test            # 20 个用例
cargo clippy --all-targets
\`\`\`

测试全部离线:工具、裁剪、超时、管道死锁回归、历史合法性;端到端用
\`std::net::TcpListener\` 起线程内假 HTTP 服务,并在**每一次请求**上断言历史合法与
消息数有界。不联网、不需要真实 API Key。

## ⚠️ 安全提示

\`tool_bash\` 能以当前用户权限执行**任意命令**,\`tool_write\` 能写**任意路径**。
接入外部输入前务必考虑 prompt injection,生产环境建议加命令白名单、限制工作目录,
或在沙箱/容器中运行。
