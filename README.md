# mvp-agent

同一个极简 **LLM Agent**(带工具调用)的四种语言实现,参考 \`agent_100_line/agent.py\` 的思路。
每个实现都**直接调用 OpenAI 兼容的 \`/chat/completions\`**(不使用任何 LLM SDK),
核心逻辑都压进 **100 行以内**,方便横向对照。

| 语言 | 目录 | 实现行数 | 第三方依赖 | 运行 | 测试 |
|---|---|---|---|---|---|
| Python | [python/](python/) | **100** | 无(仅标准库) | \`python3 mvp_agent.py\` | \`python3 -m unittest -v test_mvp_agent.py\` |
| Go | [go/](go/) | **99** | 无(仅标准库) | \`go run .\` | \`go test ./...\` |
| Rust | [rust/](rust/) | **99** | serde_json / ureq / wait-timeout | \`cargo run --quiet --\` | \`cargo test\` |
| TypeScript | [typescript/](typescript/) | **100** | 运行时零依赖(Node 内置) | \`node mvp_agent.ts\` | \`node --test test_mvp_agent.test.ts\` |

## 快速开始

四个版本用法一致:设好 API Key,然后交互式对话,或直接给一个任务一次性执行。

\`\`\`bash
export LLM_API_KEY=sk-...

cd python && python3 mvp_agent.py "看看当前目录有什么"
cd go && go run . "看看当前目录有什么"
cd rust && cargo run --quiet -- "看看当前目录有什么"
cd typescript && node mvp_agent.ts "看看当前目录有什么"
\`\`\`

## 关于依赖与「100 行」

「100 行」是硬约束,而各语言的起点并不相同:

- **Python**:标准库自带 HTTP 与 JSON,\`urllib\` 足够,所以 100 行、零依赖。
- **Go**:标准库自带 \`net/http\` 与 \`encoding/json\`,同样**不需要**第三方库。
  真正的障碍是 Go 的样板代码,所以 [go/mvp_agent.go](go/mvp_agent.go) 采用了和
  Python 版一致的**紧凑排版**(多个语句写在一行)。注意 \`gofmt\` 会把 \`if\` 展开成
  三行,因此该文件**不是 gofmt 格式**——这是为压进 100 行刻意做的取舍,\`go build\` /
  \`go vet\` 都通过。
- **Rust**:标准库既没有 HTTP/TLS 也没有 JSON。手写 JSON 解析器要 400 多行,
  所以这里改用成熟 crate:[serde_json](https://crates.io/crates/serde_json) 负责
  JSON,[ureq](https://crates.io/crates/ureq) 负责 HTTPS(自带 rustls,不需要外部
  命令),\`wait-timeout\` 负责子进程超时。于是实现从 1000+ 行降到 **99 行**。
- **TypeScript**:Node 18+ 内置 \`fetch\`,JSON 也是内置的,所以**运行时零依赖**;
  \`typescript\` 与 \`@types/node\` 只是 \`devDependencies\`,用来 \`tsc --noEmit\` 类型检查。

共同点是:没有一个版本使用 LLM SDK,都自己拼 \`/chat/completions\` 请求。

## 共同契约

四个实现刻意保持**行为逐条等价**:

- **同一套环境变量**:\`LLM_API_KEY\` / \`LLM_BASE_URL\` / \`LLM_MODEL\` /
  \`LLM_MAX_TURNS\` / \`LLM_TIMEOUT\` / \`LLM_TOOL_TIMEOUT\` /
  \`LLM_MAX_TOOL_CHARS\` / \`LLM_MAX_HISTORY\`(详见各目录 README)。
- **同一套工具**:\`tool_bash\`、\`tool_read\`、\`tool_write\`,JSON Schema 一致且都带
  \`additionalProperties: false\`。
- **同一套错误文案**:工具错误 \`[错误]工具 … 调用失败\`、接口错误 \`[错误]API失败\`、
  上下文超长 \`[错误]上下文过长,请重开会话\`。
- **对话历史始终合法**:任何工具调用(含参数解析失败、未知工具)都会写回配对的
  \`tool\` 消息,避免下一轮请求因缺少配对而 400。
- **上下文有界**:超过 \`LLM_MAX_HISTORY\` 时在每次请求前的安全边界裁剪最旧历史,
  且绝不把 \`tool_calls\` 与它的响应拆开。
- **按字符截断**:\`LLM_MAX_TOOL_CHARS\` 以字符计,不是字节(Python \`len\`、Rust
  \`chars()\`、Go \`[]rune\`、TS \`[...s]\`),中文不会被腰斩。

## 一致性验证

[conformance/conformance.py](conformance/conformance.py) 用一个本地假服务(OpenAI 兼容)
同时驱动四个实现,以 Python 版为基准逐项比对:工具结果(**逐字**)、截断行为、错误分类,
并在**每一次请求**上断言历史合法性(首条 system、每个 \`tool_call\` 恰好一条配对响应)
与消息数有界。

\`\`\`bash
python3 conformance/conformance.py            # 全部实现
python3 conformance/conformance.py python go  # 只测指定实现
\`\`\`

不需要真实 API Key、不访问真实网络(实现自身的出站请求全部指向本地假服务)。
当前四个实现 × 4 个场景全部通过。

## ⚠️ 安全提示

\`tool_bash\` 能以当前用户权限执行**任意命令**,\`tool_write\` 能写**任意路径**。
接入外部输入前务必考虑 prompt injection,生产环境建议加命令白名单、限制工作目录,
或在沙箱/容器中运行。详见各目录 README。
