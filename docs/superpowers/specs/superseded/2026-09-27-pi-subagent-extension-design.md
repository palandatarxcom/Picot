# Picot 本地 Pi 子代理扩展设计

**状态：** Draft，待验证与评审；仅设计，未实现。
**日期：** 2026-09-27
**试验位置：** `picot-v3/.pi/extensions/picot-orchestrator/`（本仓库 `.pi/` 被 gitignore 忽略）。通过验证后再考虑迁入 `~/tmp/datarx-picot-ext`，不在本轮修改公司仓库。

## 1. 目的与范围

让 parent Pi 把有边界的任务交给独立 child Pi：选定 agent profile、并行或串行执行、查看进度、取得结果。先在普通终端和 Picot 的受信工作区里验证同一个 Pi extension，不改 Rust host、WebView 或现有会话导航。

本设计与已有工作分工不同：`2026-09-18-subagent-display-design.md` 讨论已有 `pi-subagents` npm 扩展的显示层；本设计讨论**新扩展本身**。两者不是同一个数据契约，不得直接把该 spec 中 `subagent-async` widget、`pi-subagents_launch_metadata` marker 当成本扩展已提供的能力。Picot 的 tree view、fork 与 conversation rewind 已存在，本扩展不重做。ACP 外部 CLI 委派及持久 daemon/relay 另有 spec，不是本扩展的前置条件。

v1 只做同步 tool 调用：parent 等待 child 或整组任务结束；即使并行 child 同时运行，tool 仍在执行中。不承诺关窗后继续、后台 job 恢复、companion 交互或跨设备订阅。

## 2. 已核实的依据

- Pi 项目扩展从受信工作区的 `.pi/extensions/*/index.ts` 自动加载；非交互 RPC/JSON 模式不会弹信任确认。未存信任决定且 `defaultProjectTrust` 为 `ask` 或 `never` 时，项目扩展与项目 profile 不加载（Pi `docs/extensions.md`、`docs/usage.md` Project Trust）。Picot 正式注册工作区的启动流程会提前记录项目信任；普通终端由 Pi 自己的 trust 决定。扩展未加载时工具也不存在，无法由工具自己报错；验证时须检查 Pi 的资源/信任状态。测试不靠 Picot 额外传 `--extension`。
- Pi 官方 `examples/extensions/subagent/` 使用 `--mode json -p --no-session` 启动子进程，解析 JSONL event、将结果放入 tool details，提供 single/parallel/chain 和并发上限 8/4。Pi `docs/usage.md` 明确 `--no-session` 关闭保存；去掉该参数后 JSON 模式能否生成可定位的 child session，仍需在 PoC 中验证。
- Picot 启动的是内嵌 Pi binary（`src-tauri/src/pi_launch.rs::native_launch_spec_for` 通过 `resolve_bundled_pi` 填入 `NativeLaunchSpec.binary`），不能假设 `$PATH` 上的 `pi` 就是它。官方 sample 的 PATH fallback 对 Picot 不安全；child 启动命令及其版本必须实测。
- 现有 `pi-subagents` 扩展已经把 child JSONL 放到 parent 所在 bucket，并写入 `parentSession` 等元数据；该格式**不是** Pi 官方 sample 默认行为。新的扩展若不写 session header，就不能声称 child 自动挂到 Picot sidebar 的 parent 下。

## 3. 调用方式与 profile

扩展注册一个工具 `picot_orchestrator`，参数三选一：

```text
{agent, task}                         单任务
{tasks: [{agent, task}, ...]}          并行任务
{chain: [{agent, task}, ...]}          按顺序传结果
```

只能指定其中一种；无效组合、未知 agent、空 task 在 spawn 前报错。v1 最大 8 个并行任务，同时最多运行 4 个；chain 失败立即停止后续步骤。沿用官方 sample 的状态流与用量汇总，按各 child 的完成状态返回结果；不自动重试（重试会重复文件修改或外部副作用）。

Profile 使用 Markdown + YAML frontmatter，至少包含 `name`、`description`、正文 system prompt，可选 `tools`、`model`、`thinking`：无 `model` 继承 parent 的 model，无 `thinking` 继承 parent 的 thinking level。解析在官方 sample 字段（name/description/tools/model）之上仅增加 `thinking`，由本扩展解析并映射 `--thinking`；`mode`、`aliases`、`systemPromptMode`、`defaultContext` 等 pi-subagents 专有字段一律静默忽略。目录扫描沿用 sample：仅顶层 `*.md`、非递归，禁止递归 glob 把归档或草稿目录捞进池子。来源优先级为：扩展随包内置的只读 profile → 用户 `~/.pi/agent/agents/*.md` → 显式启用的当前项目 `.pi/agents/*.md`（同名后者覆盖前者）。内置 profile 首版只放确实用到的角色，不预装大套 workflow。

默认只列内置与用户级 profile。项目 profile 必须由调用方显式开启，且所在项目已受信；无法核实受信状态时拒绝。不得提供绕过确认的 tool 参数。在普通 TUI 可追加确认；在 Picot RPC 无交互确认时仅允许已受信且显式开启的项目 profile，绝不把 `ctx.hasUI=false` 当作默认同意。Profile 是可执行指令来源，不是安全沙箱；`tools` 限制 agent 可用工具，不限制其 Bash 可访问的文件范围。只读角色不得凭 prompt 文案冒充权限隔离。

## 4. 子进程与 session

每次任务启动独立 Pi 进程，使用 `--mode json -p`，**不传 `--no-session`**。是否能持久保存并准确取得 child session 身份，由首轮真实试验验证；验证失败不得宣称可打开 child session。child 的 cwd 默认为 parent 的 `ctx.cwd`；v1 不开放任意 cwd 参数。显式继承父调用的模型、thinking、Pi agent root 等必要环境；不要把整个 parent 环境、桥接密钥或 Picot 控制面凭据写入日志。profile 正文按官方 sample 写入权限仅 owner 可读的临时 prompt 文件，退出时清理。注入沿用 sample 的 `--append-system-prompt`：正文附加在 Pi base prompt（含全局模式、项目指令）之后，不是 replace；profile 文案不得依赖「独占 system prompt」的假设，边界与禁令须在正文中自我重申。

优先使用当前 parent 所用的 Pi 可执行文件；若运行环境只提供通用 Bun/Node 而不能确定 Pi 入口，必须报明确错误，**不回退到 `$PATH` 中的 `pi`**。Picot 和普通终端各做一次真实启动试验，确认 child 的 Pi 版本、模型认证和 session 文件路径。需要额外入口配置时，再根据试验结果设计，不能默认 Picot 的 PATH 开关已开启。

Child 仍会加载当前项目可见的 Pi 扩展。为防本扩展在 child 中递归委派，spawn 时设置仅作用于 child 的禁派发标记；扩展检测到标记时不注册 `picot_orchestrator` 工具，不影响其他扩展/模型提供者加载。不能为防递归直接加 `--no-extensions`，否则可能禁掉 child 所需的 provider 扩展。整个 tool 调用由 parent 管理：取消时终止仍在运行的 child，超时和异常退出记录失败，不返回假成功；参照 sample 的 SIGTERM→限时强制终止，在 Windows 上另验证进程树清理。parent 退出后的孤儿进程风险必须实测，不能凭 tool AbortSignal 推断已清理。

每个 child 要能回报其实际 `sessionFile` 和 session ID。JSON event 是否携带这些字段，以及能否稳定获取，属于首个验证关口；不得从 cwd 猜 session bucket、从最新文件 mtime 反推身份。获取失败时允许 child 完成、回传结果，但标记「无法定位 child session」，不生成可打开链接。v1 把 parentSessionFile、childSessionFile、runId 的关联保存在扩展的 run metadata 中；不修改既有 JSONL session header，不许诺 child 自动显示在 Picot 的树形 sidebar 或运行中只读视图。若确需 child 在 sidebar 挂树，需后续另定 Pi session header 写入与 host 只读路由的契约，禁止运行中用第二个 Pi 进程打开同一 session 写入。

## 5. 信息传递与结果

Child 各有独立上下文，不共享 Pi message history，也不继承 parent 会话（无 fork）：

- 为继承式角色（如决策一致性复核）调用时，parent 必须把决策基线——已定决策、约束、待查问题——写进 task 文本；child 无基线时应先索要，不得凭空推断。
- single：parent 传 task；child 的最终回答与成功/失败状态作为 tool result 回 parent。
- parallel：各 child 只向 parent 返回；child 间不共享中间状态。结果按输入顺序列出，不因结束顺序改变角色归属。
- chain：parent 把上一步最终回答作为**有界、标明来源的不可信资料**放入下一步 task；下一步自行核验文件与事实，不能把 handoff 内容当 system prompt 或可执行指令。任何一步失败立即停止，保留已完成步骤的记录。

对模型可见的每步回答设字节上限；截断时注明「输出已截断」，完整产物路径只在已验证可读时附上。大输出由 child 写工作区内明确的 artifact，parent 传用户可检查的路径，不自动读任意 child 自报的绝对路径。v1 不引入通用共享消息总线、直接 child-to-child 聊天、自动 fan-in 状态机或自定义 handoff JSON schema。

运行记录可在 Pi agent root 下按 runId 存最小 `result.json`（角色、任务摘要、状态、exit code、sessionFile、最终回答或截断标记）与受限 `stderr`，仅供诊断和迁移验证；写盘用私有目录及原子替换，避免多进程互盖。parent 的 tool result 是实际交付通道，记录文件不是后台 job 的状态权威。脱敏、体积和清理策略在实现前做实测；不可把凭据或完整环境变量写入记录。

## 6. 安全与失败边界

- 项目 profile 和项目 extension 均视作可执行的仓库内容。受信检查失败关闭该路径；不信任项目时不能靠命令行 `--approve` 偷开。
- child 进程拥有与用户相同的 OS 权限；cwd 不是 filesystem sandbox。并发写同一文件可能互相覆盖：并行模式推荐只读任务；写任务需隔离工作区或串行，v1 不承诺自动冲突解决。
- 结果文本、artifact、stderr 都是不可信内容；chain handoff 不提升其指令优先级。
- 非零 exit code、Pi assistant stopReason `error`/`aborted`、信号取消、JSON 解析失败都应按失败上报；不能把子进程静默退出解释为成功。JSON stdout 每行设上限，stderr 与 tool 回传截断，防止大输出拖垮 parent。
- 失败/取消不能撤销已产生的文件、网络和外部服务副作用；本设计不实现 file checkpoint。Picot 的 conversation rewind、tree、fork 也不能回滚工作区文件。

## 7. 验证顺序与迁移条件

先在 `.pi/extensions/picot-orchestrator/` 做最小真实试验，再决定是否扩展；该目录被 gitignore 忽略，属于本机 PoC。测试项：

1. 普通终端和 Picot 的已注册、受信工作区分别确认工具可发现；未受信且缺省 `ask`/`never` 的项目资源不加载，工具缺席时提示用户检查 Pi 项目信任。确认无 TUI 交互时未授权项目 profile 被拒绝。
2. 用内嵌 Pi 执行 single（实际模型或可控测试 provider），核对 binary 路径、Pi 版本、保存的 child session ID/file、退出与错误；不要用用户 PATH `pi` 代替。同时用角色文件里的自定义 provider 模型名（如 `OpenCodex/gpt-6-sol`、`minimax-cn/MiniMax-M3`）验证 child 的 `--model` 解析。验证 parent 重启后 session 文件仍可读取。
3. parallel 证明 8/4 上限、结果顺序与失败归属；chain 证明 handoff 截断与失败停止。取消/超时后检查不残留 child 进程，Windows 同样检查进程树。
4. 核对 child 是否会被 Picot `workspace_sessions` 扫描并显示，以及点击运行中 child 是否可能双写。v1 验收不依赖 child 出现在 sidebar；不满足安全路由时只通过 tool result 展示 session 路径。
5. 先跑聚焦测试。修改 `public/` 或 `extensions/` 时运行 `bun run check`；修改 extension 构建源时再跑 `bun run build:extensions`；触及 session/path 时运行 `bun run test`。跨平台未验证的能力标明限制。

迁到 `~/tmp/datarx-picot-ext` 前：普通 Pi 与 Picot 的真实单任务均通过；项目 profile 信任与递归禁派发可证明；取消、错误、独立 session 定位可复现；明确许可证与 Pi 版本兼容范围。公司扩展库的打包、自动更新及 Picot 专用 UI 接入另行评审。

## 8. 取舍记录

先沿官方 sample 的 JSON 流实现同步调度，去掉 `--no-session` 后实测 child session 身份；借鉴 mitsuhiko 的运行记录，不采用 tmux。当前不实现 daemon 接管，也不假定 child 会自动挂到 sidebar。出现关窗持续或运行中 child 可写接管需求时，再设计进程所有权和只读路由。
