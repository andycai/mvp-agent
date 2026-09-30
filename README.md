# mvp-agent

一个极简的 **LLM Agent**(带工具调用)的多语言实现集合,参考
\`agent_100_line/agent.py\` 的思路。核心理念:**不依赖任何第三方 LLM SDK**——
每个版本都只用本语言的标准库直接调用 OpenAI 兼容的 \`/chat/completions\` 接口。

| 语言 | 目录 | 运行 | 测试 |
|---|---|---|---|
| Python | [python/](python/) | \`python3 mvp_agent.py\` | \`python3 -m unittest -v test_mvp_agent.py\` |
| Go | [go/](go/) | \`go run .\` | \`go test ./...\` |
| Rust | [rust/](rust/) | \`cargo run --quiet --\` | \`cargo test\` |
| TypeScript | [typescript/](typescript/) | \`node mvp_agent.ts\` | \`node --test test_mvp_agent.test.ts\` |

## 快速开始

四个版本的用法一致:设好 API Key,然后交互式对话,或直接给一个任务一次性执行。

\`\`\`bash
export LLM_API_KEY=sk-...

cd python && python3 mvp_agent.py "看看当前目录有什么"
cd go && go run . "看看当前目录有什么"
cd rust && cargo run --quiet -- "看看当前目录有什么"
cd typescript && node mvp_agent.ts "看看当前目录有什么"
\`\`\`

## 共同契约

四个实现刻意保持**行为逐条等价**,方便对照学习:

- **同一套环境变量**:\`LLM_API_KEY\` / \`LLM_BASE_URL\` / \`LLM_MODEL\` /
  \`LLM_MAX_TURNS\` / \`LLM_TIMEOUT\` / \`LLM_TOOL_TIMEOUT\` /
  \`LLM_MAX_TOOL_CHARS\` / \`LLM_MAX_HISTORY\`(详见各目录 README)。
- **同一套工具**:\`tool_bash\`(执行命令)、\`tool_read\`(读文件)、
  \`tool_write\`(写文件),JSON Schema 一致且都带 \`additionalProperties: false\`。
- **同一套错误文案**:工具错误 \`[错误]工具 … 调用失败\`、接口错误
  \`[错误]API失败\`、上下文超长 \`[错误]上下文过长,请重开会话\`。
- **对话历史始终合法**:任何工具调用(包括参数解析失败、未知工具)都会写回一条
  配对的 \`tool\` 消息,避免下一轮请求因缺少配对而 400。
- **上下文有界**:超过 \`LLM_MAX_HISTORY\` 时,在每次请求前的安全边界裁剪最旧
  历史,且绝不把 \`tool_calls\` 与它的响应拆开。

## 一致性验证

[conformance/conformance.py](conformance/conformance.py) 用一个本地假服务
(OpenAI 兼容)同时驱动四个实现,以 Python 版为基准逐项比对:工具结果、截断行为、
错误分类,并在**每一次请求**上断言历史合法性(首条为 system、每个
\`tool_call\` 恰好一条配对响应)与消息数有界。

\`\`\`bash
python3 conformance/conformance.py            # 全部实现
python3 conformance/conformance.py python go  # 只测指定实现
\`\`\`

不需要真实 API Key、不访问真实网络(实现自身的出站请求全部指向本地假服务)。

## ⚠️ 安全提示

\`tool_bash\` 能以当前用户权限执行**任意命令**,\`tool_write\` 能写**任意路径**。
接入外部输入前务必考虑 prompt injection,生产环境建议加命令白名单、限制工作目录,
或在沙箱/容器中运行。详见各目录 README。
