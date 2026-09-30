# typescript/ — 极简 LLM Agent(Node 标准库)

参考 \`agent_100_line/agent.py\` 的思路,用 Node 标准库实现一个带工具调用的 LLM
Agent,**零运行时依赖**(\`dependencies\` 为空,只用 \`node:*\` 内置模块)。行为与
[python/](../python/) 版逐条等价。

## 快速开始

需要 **Node 22.6+**(推荐 24+):依赖 Node 原生的 TypeScript 类型擦除,直接运行
\`.ts\` 文件,**没有构建步骤**。

\`\`\`bash
export LLM_API_KEY=sk-...

node mvp_agent.ts                      # 交互模式
node mvp_agent.ts "看看当前目录有什么"   # 一次性执行后退出
npm start                              # 等价于 node mvp_agent.ts
\`\`\`

交互模式下输入 \`exit\` / \`quit\`、Ctrl-C 或 Ctrl-D 退出。

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
| \`tool_bash\` | \`cmd\` | 执行系统命令 \`bash -c\`(优先 \`/bin/bash\`) |
| \`tool_read\` | \`fp\` | 读取文件(非法 UTF-8 按 U+FFFD 替换) |
| \`tool_write\` | \`fp\`, \`data\` | 写入文件(自动创建父目录) |

HTTPS 直接用内置的全局 \`fetch\`(Node 18+),不需要任何 HTTP 库。

## 设计要点

- **对话历史始终合法**:参数解析、查表、调用工具中的任何异常都会转成一条
  \`tool\` 结果写回,保证每个 \`tool_call\` 都有配对响应。
- **上下文有界**:超过 \`LLM_MAX_HISTORY\` 时在**每次请求前的安全边界**裁剪最旧
  历史;裁剪点落在 \`tool\` 结果上会继续前移,绝不拆开 \`tool_calls\` 与它的响应。
- **错误分类**:工具错误 \`[错误]工具 … 调用失败\`、接口错误 \`[错误]API失败\`、
  上下文超长 \`[错误]上下文过长,请重开会话\`;HTTP 错误保留响应体。
- **按码点截断**:\`LLM_MAX_TOOL_CHARS\` 用 \`[...s].length\` 计数,和 Python 的
  \`len()\`、Rust 的 \`chars()\` 一样按字符而非 UTF-16 单元/字节。
- **只用可擦除语法**:不使用 \`enum\` / \`namespace\` / 构造函数参数属性,这样
  Node 的类型擦除器可直接执行(见 \`tsconfig.json\` 的 \`erasableSyntaxOnly\`)。
- **本地导入带 \`.ts\` 扩展名**:ESM 下 Node 要求显式扩展名。

## 测试与类型检查

\`\`\`bash
npm test          # node --test,19 个用例
npm run typecheck # 需要先 npm install(仅 devDependencies)
\`\`\`

\`npm test\` 全部离线:工具、裁剪、历史合法性;端到端用 \`node:http\` 起本地假服务
(走真实 \`fetch\`),并在**每一次请求**上断言历史合法与消息数有界。不联网、不需要
真实 API Key。

> \`node_modules/\` 已在仓库根 \`.gitignore\` 中忽略;\`npm install\` 只为
> \`tsc\` 类型检查服务,运行 agent 本身**不需要**任何安装。

## ⚠️ 安全提示

\`tool_bash\` 能以当前用户权限执行**任意命令**,\`tool_write\` 能写**任意路径**。
接入外部输入前务必考虑 prompt injection,生产环境建议加命令白名单、限制工作目录,
或在沙箱/容器中运行。
