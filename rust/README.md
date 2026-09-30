# rust/ — 极简 LLM Agent(纯 Rust 标准库)

参考 \`agent_100_line/agent.py\` 的思路,用 Rust **标准库**实现一个带工具调用的
LLM Agent,\`[dependencies]\` **为空**——没有任何第三方 crate。行为与
[python/](../python/) 版逐条等价。

## 快速开始

\`\`\`bash
export LLM_API_KEY=sk-...

cargo run --quiet --                    # 交互模式
cargo run --quiet -- "看看当前目录有什么" # 一次性执行后退出
\`\`\`

交互模式下输入 \`exit\` / \`quit\`、Ctrl-C 或 Ctrl-D 退出。

## 两个绕不开的取舍(重要)

Rust 标准库**既没有 HTTP/TLS 客户端,也没有 JSON**。本实现的选择是:

1. **HTTPS 交给系统 \`curl\`**:通过 \`Command::new("curl")\` 子进程发请求,用
   \`--data-binary @-\` 从 stdin 喂 body,\`-w "\\n%{http_code}"\` 取状态码
   (这样非 2xx 也能拿到响应体)。**因此运行需要系统装有 \`curl\`**,且 \`LLM_BASE_URL\`
   会被原样交给 curl(走系统证书校验)。
2. **JSON 自己实现**:见 [src/json.rs](src/json.rs),约 400 行,支持对象、数组、
   字符串转义(含 \`\\uXXXX\` 与 UTF-16 代理对)、整数/浮点、嵌套结构;序列化
   分隔符与 Python \`json.dumps\` 一致。

代价是比其他语言多一个文件;好处是零依赖、\`cargo build\` 不需要联网拉 crate。

## 环境变量

| 变量 | 默认值 | 说明 |
|---|---|---|
| \`LLM_API_KEY\` | (无,必填) | 也接受 \`DEEPSEEK_API_KEY\` / \`OPENAI_API_KEY\` |
| \`LLM_BASE_URL\` | \`https://api.deepseek.com/v1\` | 其他兼容后端 |
| \`LLM_MODEL\` | \`deepseek-chat\` | 模型名 |
| \`LLM_MAX_TURNS\` | \`15\` | 单轮最多工具循环次数 |
| \`LLM_TIMEOUT\` | \`120\` | 单次 API 请求超时(秒),同时作为 curl 的 \`--max-time\` |
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
- **工具超时自己实现**:std 没有现成超时,用 \`try_wait\` 轮询 + \`kill\`;
  stdout/stderr 由独立线程读取,避免子进程写满管道后死锁。
- **错误分类**:工具错误 \`[错误]工具 … 调用失败\`、接口错误 \`[错误]API失败\`、
  上下文超长 \`[错误]上下文过长,请重开会话\`;HTTP 错误保留响应体。
- **按字符截断**:\`LLM_MAX_TOOL_CHARS\` 以字符计(不是字节),与 Python 版一致。

## 测试

\`\`\`bash
cargo test          # 21 个用例
cargo clippy --all-targets
\`\`\`

测试全部离线:JSON 解析、工具、裁剪、历史合法性;端到端用 \`std::net::TcpListener\`
起线程内假 HTTP 服务,并在**每一次请求**上断言历史合法与消息数有界。不联网、
不需要真实 API Key。

## ⚠️ 安全提示

\`tool_bash\` 能以当前用户权限执行**任意命令**,\`tool_write\` 能写**任意路径**。
接入外部输入前务必考虑 prompt injection,生产环境建议加命令白名单、限制工作目录,
或在沙箱/容器中运行。
