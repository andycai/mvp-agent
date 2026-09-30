# typescript/ — 极简 LLM Agent(100 行)

参考 \`agent_100_line/agent.py\` 的思路,用 Node 实现一个带工具调用的 LLM Agent,
核心逻辑 **100 行**,行为与 [python/](../python/) 版逐条等价。

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

## 依赖:运行时零依赖

- **HTTP**:用 Node 18+ 内置的全局 \`fetch\`,不需要任何 HTTP 客户端库。
- **JSON**:\`JSON.parse\` / \`JSON.stringify\` 内置。
- **子进程 / 读文件 / REPL**:\`node:child_process\`、\`node:fs\`、\`node:readline\`。

所以 \`package.json\` 的 \`dependencies\` 是空的。\`typescript\` 与 \`@types/node\` 只是
\`devDependencies\`,用于 \`npm run typecheck\`(\`tsc --noEmit\`);**不 \`npm install\`
也能直接运行 agent 本身**。

## 环境变量

| 变量 | 默认值 | 说明 |
|---|---|---|
| \`LLM_API_KEY\` | (无,必填) | 也接受 \`DEEPSEEK_API_KEY\` / \`OPENAI_API_KEY\` |
| \`LLM_BASE_URL\` | \`https://api.deepseek.com/v1\` | 其他兼容后端 |
| \`LLM_MODEL\` | \`deepseek-chat\` | 模型名 |
| \`LLM_MAX_TURNS\` | \`15\` | 单轮最多工具循环次数 |
| \`LLM_TIMEOUT\` | \`120\` | 单次 API 请求超时(秒) |
| \`LLM_TOOL_TIMEOUT\` | \`60\` | 单个工具执行超时(秒) |
| \`LLM_MAX_TOOL_CHARS\` | \`16000\` | 回填给模型的单个工具结果上限(按码点) |
| \`LLM_MAX_HISTORY\` | \`40\` | 保留的最近消息条数,超出则裁剪最旧历史 |

## 内置工具

| 工具 | 参数 | 作用 |
|---|---|---|
| \`tool_bash\` | \`cmd\` | 执行系统命令 \`bash -c\`(优先 \`/bin/bash\`) |
| \`tool_read\` | \`fp\` | 读取文件(非法 UTF-8 按 U+FFFD 替换) |
| \`tool_write\` | \`fp\`, \`data\` | 写入文件(自动创建父目录) |

## 设计要点

- **对话历史始终合法**:参数解析、查表、调用工具中的任何异常都会转成一条
  \`tool\` 结果写回,保证每个 \`tool_call\` 都有配对响应。
- **上下文有界**:超过 \`LLM_MAX_HISTORY\` 时在**每次请求前的安全边界**裁剪最旧
  历史;裁剪点落在 \`tool\` 结果上会继续前移,绝不拆开 \`tool_calls\` 与它的响应。
- **错误分类**:工具错误 \`[错误]工具 … 调用失败\`、接口错误 \`[错误]API失败\`、
  上下文超长 \`[错误]上下文过长,请重开会话\`;HTTP 错误保留响应体。
- **按码点截断**:用 \`[...s]\` 计数,避免把代理对切成两半(和 Python \`len\`、Rust
  \`chars()\`、Go \`[]rune\` 对齐)。
- **只用可擦除语法**:不使用 \`enum\` / \`namespace\` / 构造函数参数属性,这样 Node 的
  类型擦除器可直接执行(见 \`tsconfig.json\` 的 \`erasableSyntaxOnly\`)。
- **本地导入带 \`.ts\` 扩展名**:ESM 下 Node 要求显式扩展名。

## 测试与类型检查

\`\`\`bash
node --test test_mvp_agent.test.ts   # 19 个用例,无需安装任何依赖
npm install && npm run typecheck     # 可选:tsc --noEmit
\`\`\`

测试全部离线:工具、裁剪、超时、管道死锁回归、历史合法性;端到端用 \`node:http\` 起
本地假服务(走真实 \`fetch\`),并在**每一次请求**上断言历史合法与消息数有界。
不联网、不需要真实 API Key。

> \`node_modules/\` 已在仓库根 \`.gitignore\` 中忽略。

## ⚠️ 安全提示

\`tool_bash\` 能以当前用户权限执行**任意命令**,\`tool_write\` 能写**任意路径**。
接入外部输入前务必考虑 prompt injection,生产环境建议加命令白名单、限制工作目录,
或在沙箱/容器中运行。
