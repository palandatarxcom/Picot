# ACP 外部代理委派设计

**Status:** 设计草案，待 Dr. Lin 拍板关键决策点。
**Date:** 2026-09-18
**Provenance:** 参照 upstream `picot` main `42e2a06..5e558fd` 的 ACP 实现（commit f207db0 起的系列，作者 ShixinGuo），已核实 `acp_launch.rs`、`acp_manager.rs`、`host_server.rs` ACP 分发与前端 `public/native/acp/`、`composer-agent-menu.js` 源码。
**关联:** 与 `2026-09-18-subagent-display-design.md`（pi-subagents 路线）正交，两者共存；前置依赖 `2026-09-18-upstream-immediate-migration-design.md` 第 2 节（子进程清扫）先落地。

## 1. 问题与机会

用户想在 Picot 里直接使用 Claude Code、Gemini CLI、Codex 等外部 CLI 编码代理的能力，而不离开主聊天。这些 CLI 已各自提供成熟的模型与工具生态，但彼此协议不同、启动方式各异。

**Agent Client Protocol（ACP）** 是这些 CLI 共同支持的 JSON-RPC 2.0 over stdio 协议。Picot 充当 ACP 客户端（editor 侧），外部 CLI 充当 agent 侧子进程。这不是 pi-subagents 的替代品：子代理不是另一个 Pi 实例，而是第三方 agent。两个 feature 解决同一个 UX 问题（主聊天里派发并展示子任务），但能力来源不同，并存合理。

**触发场景只有一个**：用户在 composer 使用 `#` agent 菜单，或输入 `#claude <任务文本>`。Pi 主会话始终由 Picot 自己的 runtime 管理；ACP 子进程只负责被派发的那段任务，不拥有会话。

## 2. 参照实现的关键事实（已核实）

### 2.1 启动侧：`acp_launch.rs`

- `AcpLaunchSpec { agent_id, label, command, args, cwd }`——与 `NativeLaunchSpec` 同构，但子进程讲 ACP 而非 Pi 的 flat framing。
- 内置 preset 表：claude-code（`npx @agentclientprotocol/claude-agent-acp`）、gemini（`gemini --experimental-acp`）、codex、cursor、qwen（npx adapter 或 CLI 原生 `--acp` 标志）。
- 每个 preset 带 `probe_bin`（PATH 上探测此二进制，决定「该 agent 已安装」）与可选 `api_key_env`（有 API key 即视为可用，无需 CLI 登录）。
- 命令可被 `PICOT_ACP_<ID>_CMD` 环境变量覆盖。
- spawn 时增强 PATH（`build_augmented_path()`），否则 `npx` 找不到。

### 2.2 管理侧：`acp_manager.rs`（699 行）

- `AcpAgentManager` 镜像 `NativePiManager` 的形状：复用 `PiRpcBridge` 作 stdio 传输（它是通用 newline-delimited JSON 桥，只假设 id 字段相关联——ACP 的 JSON-RPC 2.0 framing 满足此假设）；持有独立 `RuntimeCoordinator`，使同一会话可在 Pi backend 与 ACP backend 间切换而无实例碰撞。
- `spawn()`：起子进程 → `initialize` + `session/new` 握手 → 注册 runtime，返回 ACP 侧 session id（与 Picot 自己的 `RuntimeTarget.session_id` 不同）。
- 握手超时 30 秒硬上限，避免 hung adapter 卡死 `#` 菜单；帧上限 16 MB。
- 事件经 `broadcast` 通道进 `host_server.rs` 的事件泵，与 Pi runtime 事件同一条 WebSocket 通道下发；`Lagged` 时发 `event_sequence_gap` 结构化错误。
- 权限请求：agent 发起 `session/request_permission` 时挂入 `pending_permissions`（key = `{instance_id}:{request_key}`），桥接到前端应答，回包原样 echo 请求 id。
- 文件请求（`fs/read_text_file` 等）：`resolve_in_workspace` 限定在 workspace cwd 内，拒绝越界路径——安全边界。
- 生命周期：`acp_task_start` spawn 一次性任务 runtime → `acp_prompt` 驱动 → `acp_task_stop` 退役；支持按 workspace 批量 stop 与 stop_all。

### 2.3 前端

- `composer-agent-menu.js`：`#` picker 列出 host 检测到的 agent（`control.listAcpAgents()`，懒探测，失败时显示全列表，CLI 缺失由卡片上报原因）。
- 选中后插入 `#<token> `；`sendComposerInput` 用正则 `^[#/]([a-z][a-z0-9-]*)[ \t]+([\s\S]+)$` 解析，token 后的整行成为任务文本。
- `subagent-runs.js` + `subagent-card.js`：每个 run 在 Pi 消息流内渲染为可折叠卡片，支持对运行中的 run 追加消息（follow-up），完成态收进历史。
- `acp-store.js`：runs 的前端状态（含合成 sessionId/instanceId 的 target 处理）。

## 3. v3 落点

v3 无 `public/native/` 目录，upstream 前端模块需映射：

| upstream | v3 落点（建议） |
| --- | --- |
| `public/native/acp/*.js` | `public/acp/`（store、runs、card、css 分文件） |
| `public/native/composer/composer-agent-menu.js` | `public/composer-agent-menu.js`，接入 `public/app.js` 的 `sendComposerInput` |
| `acp_launch.rs` / `acp_manager.rs` | 原路径迁入，几乎零改动 |
| `child_supervision.rs` | 由其注册表/清扫能力覆盖（迁移 spec 第 2 节）；上游的新进程组逻辑在 v3 已存在于 `process_tree.rs`，不重复引入 |

后端（Rust）与 upstream 高度同源，可直接参照实现；前端落点和 `app.js` 接线必须按 v3 自己的编排结构重做，不能照抄。

## 4. 安全边界（不可简化）

1. **workspace cwd 限定**：所有 agent 发起的文件请求必须经 `resolve_in_workspace` 解析，`../` 与绝对路径越界一律拒绝。
2. **权限请求上屏**：`session/request_permission` 必须呈现给用户，不允许默认放行；无应答请求随 runtime stop 一并清理。
3. **非交互凭据**：preset 只识别 API key 环境变量与已登录 CLI，不实现任何凭据录入。
4. **握手与帧上限**：30 秒握手超时、16 MB 帧上限、broadcast 背压，防止 hung adapter 拖垮 host。
5. **进程监管**：ACP 子进程纳入子进程注册表（依赖迁移 spec 第 2 节），保证不遗留孤儿。
6. **PATH 增强**：spawn 使用与 pi 相同的 augmented PATH，不额外注入用户环境。

## 5. 决策点（待 Dr. Lin 拍板）

- **D1 preset 子集**：全部五个（claude-code/gemini/codex/cursor/qwen）还是先只做 claude-code + gemini（原生 ACP、无 adapter 依赖）？建议先做两个原生的，npx adapter 三个后续按需加。
- **D2 follow-up 消息**：第一版是否支持对运行中 run 追加消息（upstream fe08a0f 追加的能力）？建议第一版支持——它是卡片交互的核心价值，成本不高。
- **D3 卡片样式**：复用 v3 既有 tool-card / widget mirror 体系，还是照搬 upstream 的 subagent-card.css？建议复用 v3 体系，只加 ACP 特有的状态段。

## 6. 里程碑

1. **M1 后端**：`acp_launch.rs` + `acp_manager.rs` + host_server 分发 + child_supervision 接入；Rust 单测（握手、路径越界拒绝、owns/stop 语义）。
2. **M2 前端**：`public/acp/` 模块 + composer `#` 菜单 + sendComposerInput 解析；vitest 覆盖解析、store、卡片渲染。
3. **M3 集成验证**：本机装有 Claude Code CLI 的真实端到端（真 spawn、真 ACP 握手、真权限提示），不做 mock。
4. **M4 文档**：`ARCHITECTURE.md` 增补 ACP 边界与生命周期；i18n 四语言。

## 7. 排除项

- SSH 远程工作区下的 ACP（upstream 有 disable 逻辑；v3 无 SSH 功能，不引入）。
- 非 stdio 传输（TCP/WS）。
- 多 agent 并行 runs 的编排（第一版单 run 串行派发，upstream 亦如此）。
