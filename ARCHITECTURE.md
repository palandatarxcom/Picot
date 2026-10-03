# Picot 架构（Native Runtime 迁移后）

> 本文档描述 native runtime 迁移完成后的架构。
> 迁移历史与决策记录见 `docs/superpowers/specs/2026-08-27-native-runtime-migration-design.md`。

## 一句话概览

Picot 是一个 Tauri 桌面应用，为每个工作区派生一个 headless `pi` 进程（`--mode rpc`），通过 Rust 宿主进程（HostServer）管理生命周期、授权和数据面，前端 WebView 经 v2 WebSocket 协议与运行时通信。

## 进程模型

```text
┌─────────────────────────────────────────────────┐
│  Tauri App (Rust)                               │
│  ┌───────────┐  ┌─────────────────────────────┐ │
│  │  WebView  │  │  HostServer (axum, loopback)│ │
│  │  (index)  │◄─┤  /v2/ws  /v2/bootstrap      │ │
│  │           │  │  /workspaces/:wid/:sid      │ │
│  └───────────┘  │  /api/* (compat, owner-aware)│ │
│                 └──────────┬──────────────────┘ │
│                            │ stdin/stdout RPC   │
│                 ┌──────────▼──────────────────┐ │
│                 │  pi --mode rpc               │ │
│                 │  --extension picot-bridge.mjs│ │
│                 │  (per workspace)             │ │
│                 └─────────────────────────────┘ │
└─────────────────────────────────────────────────┘
```

### 三个进程角色

1. **Tauri 主进程（Rust）**：窗口管理、系统对话框、terminal、窗口 owner 注册。每个注册工作区派生一个 native pi 进程。
2. **HostServer（Rust, axum）**：loopback-only HTTP/WS 服务器。管理 runtime 生命周期（`NativePiManager`）、操作注册表（`OperationRegistry`）、授权（`WindowOwnerRegistry` capability）、数据面（`HostDataPlane` workspace containment）。
3. **pi 子进程（Bun standalone）**：headless `--mode rpc`，加载 `picot-bridge.mjs` 扩展与 pi core API 通信。

## 启动流程（native runtime）

```rust
// main.rs: setup_native_runtime
fn native_runtime_enabled(app) -> bool {
    cfg!(debug_assertions) && env::var("PICOT_RUNTIME").is_ok_and(|v| v == "native")
}
```

1. 检查 `PICOT_RUNTIME=native`（debug 构建限定）
2. **冷启动进 landing**：不注册默认工作区、不预创建 session、不派生 Pi 进程，registry 在启动期零改动（2026-09-03「冷启动一律以 ~/.pi/tmp 为 workspace」决策已废弃）。冷启动仍零派生；landing 配置面（Models/MCP/高级配置/软件包技能/advisor）按需懒派生 bridge 服务 runtime（`NativeRuntimeType::Config`：sessionless+toolless，cwd `~/.pi/tmp`，不注册工作区，global-only，经 `ephemeral_command` 通道定址，transition commit sweep 一并回收——见 `2026-09-18-landing-bridge-runtime-design.md` v2）。owner 以 `TemporaryKind::Landing` 创建（label `native-landing`；canonical home 仅作 owner 记录占位，永不为 workspace 身份、scope 或授权输入）
3. 创建 `NativePiManager` + `HostServer`（loopback:0 绑定）
4. 打开 landing 窗口加载 `{origin}/`；WebView 在 bootstrap 期分叉加载 `landing.js`（仅构造 transport、sidebar 四 seam、transition controller、landing notice 与 landing 版 Quick Chat，不建任何 chat-lifecycle 对象）。首次进入工作区必为跨工作区原地切换（prepare → commit → navigate，overlay 换屏）；workspace 的 `same`/`cross` 分类按 owner 的 Registered 绑定派生，Landing owner 永不 same（即使占位 home 本身是已注册工作区）
5. landing owner 首次 commit 后重绑为 Registered owner；窗口销毁清理、New Session 菜单状态与 Cmd+N 派发一律按 owner 注册表记录判定，不按 label 前缀（label 终身不变，非状态信号）
6. workspace 权威性为 Registered-only：Git、terminal、文件/数据 scope、项目级 config/skills 与 Side Chat 拒绝 Landing/Temporary owner；Quick Chat 是唯一 landing 例外（自带一次性 temp cwd，显式准入路径单独测试）
7. 用户自 landing 经 sidebar 进入工作区：session 行选择 / 工作区 `+ New Chat`（零会话工作区唯一入口）/ 添加项目后导航 / Focus 四条 seam 全部路由到 `enterWorkspace`

## 网络路径

| 路径 | 协议 | 授权 | 用途 |
| --- | --- | --- | --- |
| `/` | HTTP GET | desktop capability init script（owner 感知由 `/v2/ws` hello 承担） | native 冷启动 landing 页；非 native 浏览器行为不变 |
| `/v2/ws` | WebSocket v2 | desktop capability（hello 握手） | 前端 ↔ 运行时通信 |
| `/v2/bootstrap` | HTTP GET | desktop capability（header） | 获取 RuntimeTarget |
| `/workspaces/:wid/:sid` | HTTP GET | 路由参数校验 + bootstrap 鉴权 | existing shell 入口 |
| `/api/*` | HTTP GET/POST | desktop capability（`x-picot-desktop-capability` header） | 兼容路由（owner-aware） |
| `/health` | HTTP GET | 无 | 宿主存活探针 |
| `/v2/session-export/:token` | HTTP GET | 一次性令牌（owner + generation 绑定） | 会话导出流 |
| `/v2/paste-offload` | HTTP POST | desktop capability | ≥4 MiB paste 卸载 |
| `/v2/auth/exchange` | HTTP POST | 配对令牌（5 分钟 TTL，一次性） | pairing token → device token |
| `/v2/mobile/status` | HTTP GET | device token（Bearer） | 配对设备只读状态（v1 仅 liveness） |
| `/pair.html` | HTTP GET | 无（pairing 前唯一 surface） | 手机配对页 |

### 兼容路由的唯一实现规则

`/api/*` 兼容条目是**冻结集合**：只允许继续服务已在 host 内实现的路由，不再新增 handler。
需要新能力时一律接 v2 面（`data_request` 数据 op 或 `broker_control` 控制 op），由
`host_router` 的 `current_registered_context` 做代数复核；在 HTTP 侧另写一份 handler 会
造成两份契约（已发生过的错位见 `docs/superpowers/specs/2026-08-30-p8-deletion-proof-audit.md`）。

已被 native 取代或 scope 移除的入口保留显式失败，避免静默 fallback：

| 状态 | 条目 | 响应 |
| --- | --- | --- |
| 退役（D8） | `/api/rpc` | `410 Gone` + `Deprecation: true` + 匿名 client-class 计数 |
| 已有 v2 等价 op | `/api/files/content` `/api/files/raw` `/api/file-mentions` `/api/paste-offload` `/api/open` `/api/git-branch` | `410 Gone`（`api_gone`）；前端改走 `file_read`/`file_raw`/`file_mentions`/`/v2/paste-offload`/`open_in_app` |
| scope 移除（P5/P6） | `/api/models-config` `/api/agent-config` `/api/agents-md` `/api/append-system-md` `/api/chat-config` `/api/chat-telegram/{op}` `/api/skill-install-{links,scan}` `/api/super-agent/{projects,tasks}` `/api/lan-qr` | `410 Gone`（`api_gone`） |

删除前置条件不变：D10 Stage 2+ 遥测需显示这些条目在两个稳定 release 周期内零命中。

**LAN 边界**：HostServer 默认仅绑定 `127.0.0.1`（loopback-only）。用户在 设置 → Mobile Access 显式开启后（`mobile.lanAccessEnabled`，重启生效），host 绑定 `0.0.0.0`，移动端经 `/pair.html` 用桌面铸造的配对令牌换 device token；配对后 v1 仅开放只读状态，读写面仍为 desktop capability 专属（Gate B 远程矩阵未实现前不开放）。

## 授权模型

### 窗口 Owner 注册

```text
WindowOwnerRegistry
  ├── create_owner_with_workspace(label, root, origin, wid) → (OwnerId, capability)
  ├── authenticate(capability) → Option<OwnerId>
  ├── owner_current_workspace(owner) → OwnerWorkspaceSnapshot
  │     Registered { wid, root, generation }
  │     Temporary { kind }
  │     NoWorkspace
  └── validate_workspace_transition_generation(owner, gen)
```

- **capability** 是 32 字节 URL-safe 随机值，仅发放给 desktop 窗口
- **generation** 是单调递增的工作区代数——workspace transition 递增，旧代授权/操作/令牌全部失效；旧代 runtime 进程保留存活（upstream 语义：跨工作区切换不中断运行中的 turn），但因 wid/generation 失配被授权闸门拒之门外，返回原工作区时由 prepare rebind 到新代复用
- 远程设备经 `/v2/auth/exchange` 配对获得 device token（非 capability）

### 运行时事件可见性

任何已认证 desktop owner 可订阅任意 live runtime 的**事件流**（跨工作区侧栏绿/蓝点的数据源）；`runtime_request` 命令面仍要求 owner+workspace+generation 全匹配。阻塞型 `extension_ui_request`（select/confirm/input/editor）仅投递给 `authorize_target` 通过的订阅者，其余订阅者（以及 pending replay）不接收；`setWidget`/`notify` 等非阻塞 UI 事件与普通事件一样按订阅投递。

### Ephemeral 帧契约（Side/Quick Chat，2026-09-30 定契约）

native hub（`host_ephemeral.rs`）向 owner 投递两类 WS 帧，**均为顶层 type、不套信封**：①`ephemeral_event`（`payload` = Pi 原始会话事件帧，如 `{type:"message_start",…}`；`runtimeSequence` 由 hub 的 `EphemeralRenderState` 单调编号，与 `ephemeral_snapshot.runtimeSequenceWatermark` 同源）；②`ephemeral_snapshot`（快照字段平铺在帧顶层，含 `requestId`）。前端 `EphemeralChatRuntime.applySequencedEvent/_reduce` 按此契约消费；`{type:"event",event}` 与 payload 内嵌快照是已废弃的 Node-broker 形状，仅为兼容保留。快照的 `thinkingLevel` 只能来自 `get_state`（advisor 恢复发生在 RPC 就绪前、不发事件），`forward_command` 的快照分支必须回填。快照另携带 `turns`（turn 分组投影：`turn_start`/`turn_end` 为界，user/toolCallId 序列/assistant 归属开 turn，工具状态单源存于 `tools`、按 id 内联进各 turn）——前端按主聊天 turn 架构（`ui/turn.js` 的 process rail）渲染；无 `turns` 的旧快照走平铺回退。

### 数据面 containment

`HostDataPlane` 强制所有文件**读写**操作限制在注册工作区根目录内：

- `safe_join(root, relative_path)` — canonicalize + symlink 检查
- `strip_prefix` 包含性（分隔符安全，兄弟前缀拒绝）
- atomic write + mtime conflict 检测

### 工作区文件变更（2026-09-23 Files 面板树）

Files 面板的新建、重命名、删除走**同一条** host 控制数据面（`file_write` 的邻居），
不新增通用 shell 或任意路径 API：

| operation | 输入 | 成功结果 | 行为 |
| --- | --- | --- | --- |
| `file_create` | `parentPath`、`name`、`kind`、`idempotencyKey` | `path`、`kind` | 新建空文件或空目录；目标必须不存在 |
| `file_rename` | `path`、`name`、`idempotencyKey` | `path` | 同父目录内改名；只接受 basename |
| `file_delete` | `path`、`idempotencyKey` | `deletedPath` | 删除文件或**空目录** |

不变量：

- 门禁与 `file_write` 一致：Registered desktop owner + workspace + generation，
  走 `operation_scope(context, "workspace-files")` 与 mutation registry；
  **不接 LAN、relay、browser child webview 或 Pi runtime 直连**
- 路径解析走 canonicalize + `strip_prefix`；拒绝绝对路径、`..`、NUL、空名、
  含分隔符的 name、跨 workspace path。`parentPath` 的 `"."` 就是 workspace root
- `file_create` 用独占创建（`create_new`，unix mode 0600），**绝不覆盖**
- `file_delete` 只 `remove_dir`，非空目录返回 `directory_not_empty`：
  **不递归、不回收站、不 undo**——这是安全边界，不是待补的缺口
- 成功响应只含 workspace 相对路径；错误响应只含 host 自撰文案，不回显绝对路径
  或 OS error
- 前端全程 workspace 相对：树键、`list_files` 参数、`parentPath` 同一套拼写

### 列举可达性（mount / share 抖动）

`list_files` 是唯一区分「暂时不可达」与「已删除」的数据面 op。workspace root
或其子目录的 transient I/O（`ENOENT`、`ENOTCONN`、`ESTALE`、`EIO`、host-down）
返回 `temporarily_unavailable`，而不是 `file_access_failed` / `not_a_directory`：
语义是「刚刚还在，现在不在」——外接卷未挂载、网络 share 掉线——**永不作为删除
信号**。

WebView 侧契约（spec 2026-09-23 §6.2）：保留 listing cache、expanded 状态与已打开
的预览 tab，只显示 stale + 重试；只有同一 workspace root **连续 3 次** root
listing 失败（计数仅在内存，任何一次 list 成功归零）才丢弃该 workspace 的 cache
与持久化展开状态。变更类 op 的 ENOENT 仍返回 `file_not_found`——只有树持有
「一次误判就丢弃」的缓存。

### Files 面板树契约

- 只请求 root；展开某目录时才请求该目录，listing 缓存在内存直到显式刷新
- 展开状态按 canonical workspace root 存浏览器 `localStorage`（不落 DB），
  写入与恢复共用深度上限 5（root 为深度 0）
- 隐藏项过滤在渲染层：host 始终返回全量 listing，切换显示隐藏不产生 I/O
- 变更成功后只失效受影响的父目录 listing，不做全树 reload

**列举 ≠ 读写（2026-09-19 @ 提及宽根）：** `file_mentions` 的**搜索列举**可按
用户前缀越出 workspace（desktop capability 专属 op；spec
`2026-09-19-file-mention-paths-design.md` 显式接受——与 Pi TUI 同机同用户语义
一致），而文件**读写** containment 完全不变。列举的搜索根按 query 前缀分级，
host 是唯一权威（WebView 只提交镜像声明供全等校验，不符即 `invalid_mention_query`）：

| 前缀 | 搜索根 | 声明 `{kind, value}` |
| --- | --- | --- |
| `@foo`、`@src/foo`、`@./foo` | 注册 workspace root | `{workspace, ""}` |
| `@../foo`（可多级，封底于根） | workspace 祖先目录 | `{absolute, 爬升路径}` |
| `@~/foo` | host 进程用户 home（`~` 仅 host 展开） | `{home, "~"}` |
| `@/foo`（仅 POSIX） | 文件系统根 | `{absolute, "/"}` |
| `@C:/foo`（Windows） | 盘符根（2s 可达性探测） | `{drive, "C:/"}` |
| `@//server/share/foo`（Windows） | UNC 共享根（2s 探测） | `{unc, "//server/share"}` |

词中 `..` 一律拒绝；递归下钻不越出声明的搜索根；预算（visited 10k / collected
200 / 返回 20 / 500ms / 深度 4）照抄 upstream 纪律。宽根 walk 在
`spawn_blocking` 中执行，不占异步 worker。

### Session 删除授权（per-path）

`session_delete_batch` control op 采用 Desktop+owner 身份门禁（`require_native_owner`）加逐路径校验，不依赖 owner 当前的 workspace 绑定：每个 path 必须存在于 `~/.pi/agent/sessions`、是可解析 header 的 `.jsonl`，且 header 记录的 cwd canonicalize 后命中注册 workspace root；运行中 session 另由 running 列表保护。这与 `workspace_sessions` data 路由的授权模型对齐：landing owner（无 workspace 绑定）能列出已注册 workspace 的 session，也就能删除它们；授权边界是 desktop owner capability，不是 workspace 绑定。被拒路径必须进入响应的 `errors` 数组而非静默丢弃——前端把「不在 errors 中」视为已删除，静默丢弃会伪造成功并让 session 在 refresh 后复活。

## 运行时生命周期

```text
spawn → Starting → Ready → Working ↔ Idle → Stopped
                ↘ Crashed（EOF/child-exit/writer-fail/frame-fatal）
                ↘ Suspended（resume → Starting with new generation）
```

`runtime_instances` exposes each resolvable live runtime's `streaming` flag from this state machine: only `Working` is true. The event pump sets `Working` on `agent_start`, and clears it through `Idle` on `agent_end` or `agent_settled`. After every successful main-runtime spawn, host sends owner-scoped `runtime_started`; WebView refreshes its live-runtime subscription set idempotently, subscribes only new instance IDs, requests snapshots, and initializes a green sidebar dot from `streaming`. A subscribed `runtime_stopped` always clears that dot. This preserves live transcript continuation after a JSONL-fast-path page return even when that page missed `agent_start`.

### 工作区会话目录
### 工作区会话目录

Pi 进程内部会将 canonical workspace path 映射到确定性 session bucket（例如 `~/.pi/agent/sessions/--<canonical-path-with-separators-folded-to-dash>--`）：

```text
~/.pi/agent/sessions/--<canonical-path-with-separators-folded-to-dash>--
```

workspace 注册时，Picot 将 `session_bucket` 留空；注册成功后，当前窗口必须先通过 owner-bound 的 `workspace_target_prepare(forceNewSession: true)` 创建一个新的主 Pi runtime，再 commit transition 并导航到该 session。目标页面首屏会先渲染 route 对应的 provisional session，避免 bucket 尚未写回时显示空行；Pi runtime 的 `get_state.data.sessionFile` 是唯一 bucket 来源，不根据 workspace 路径计算 bucket，也不扫描全局 sessions root 发现 bucket。bucket 写回有两条路径：每次正式 spawn（`workspace_target_prepare`、`workspace_open`、`restart_runtime`）成功后，host 会起一个 detached 任务直接向新 runtime 发送 `get_state` 并写回 bucket——Pi 在 session 创建时就分配持久化文件路径，因此 spawn 后首个 `get_state` 即携带它，landing 添加项目后无需任何 WebView 动作即可完成登记（否则冷启动会话计数会因快照时机错失新会话而归零）；正常 runtime 的 `runtime_snapshot_request` 代理路径仍是兜底。不再启动无 owner 的探测进程。已有 registry row 再次 register 也创建新 session。临时 session 必须先绑定正式 session id，再保存 Pi 返回的 bucket。SQLite 只持久化 workspace registry（`workspace_id`、canonical path、display/pin/open 状态和 Pi 返回的 `session_bucket`）及 preferences；不持久化 session visibility、subagent classification 或 session-count cache，也不再创建或保留废弃的 `session_sidebar_visibility` 表。浏览器 cookie 中的 sidebar/navigation cache 只是跨路由首屏加速，可能过期或丢失，不能作为权限、workspace 身份或 session 列表的权威来源。bucket 缺失或 bucket 目录不存在表示 0 session，不移除 workspace；首次加载 registry 时，`workspace.list` 检查每个 canonical path 是否仍为目录，只删除已消失的 registry row，不删除物理目录或 session 文件。

Sidebar session discovery follows Pi `/resume`: one registered workspace bucket is enumerated. The `workspace_sessions` full-read path parses JSONL files concurrently (up to 10 workers per bucket; at most two such scans host-wide). `workspace_sessions(countOnly: true)` only reads directory entries and returns the exact `.jsonl` file count without parsing contents; a full read returns the exact count of successfully parsed sessions. Each valid session is retained; `parentSession` is used only to build cross-file parent/child relationships, not to hide or classify sessions. `pi-subagents_launch_metadata` no longer has a special visibility rule: Normal, Focus, search, list, and workspace batch deletion treat that file as an ordinary session. Search reads each candidate JSONL in one streaming pass.

Normal and Focus share `public/sidebar/session-tree-model.js`: missing parents and malformed cycles are promoted/broken without dropping files, then flattened with Pi-style branch prefixes. Normal shows five sessions initially per expanded workspace and adds ten per request; Focus uses the same five/ten pagination. Normal loads registry history lazily on expansion; Focus ensures the selected workspace history when entered. Session selection is not a registry data refresh: same-workspace selection updates the active row and chat history without a WebView reload, while cross-workspace selection prepares/commits a new runtime and navigates to its host-origin route. `focusWorkspaceId` is carried only when the target canonical cwd matches the focused workspace and is removed for cross-workspace or unknown navigation.

已知实现边界：host-wide 的两个完整扫描 permit 当前只包住 `workspace_sessions`；兼容/独立的 `list_sessions` 与 `search_sessions` 仍各自 `spawn_blocking`，不共享该上限。sidebar 的 registry count warmup 由 WebView 对所有 registry rows 并行发起，冷启动通过 `requestIdleCallback({ timeout: 800 })` 调度，空闲不足时也会在该上限到期后执行。它们是性能债务，不是 session 数据一致性或授权依据；若扩大 workspace 数量或搜索频率，应先将这些路径纳入统一 scan scheduler，再提高任何并发上限。

### 子进程注册表与孤儿清扫

pi runtime 的存活不依赖 Picot 的 teardown：`pi` 在 stdin EOF 时退出，但卡死的 runtime 读不到 EOF，而 Picot 被 SIGKILL/崩溃时根本不会执行清理。因此每次 spawn 成功后，host 把 `{pid, 启动时间}` 写入 `~/.pi/picot-runtimes/<supervisor-pid>.json`（`child_supervision.rs`），正常 stop 时删除对应条目、全部清空时删除文件。启动时 `sweep_orphans()` 扫描该目录：supervisor 进程已不存在且条目 pid 仍存活、且 OS 报告的启动时间与登记值一致的条目，才按进程组 `SIGKILL`，随后删除该注册表文件——启动时间是必需的身份校验，避免 pid 复用后误杀无关进程。SIGTERM/SIGINT/SIGHUP 另有信号兜底（`RunEvent::Ready` 时安装），走与正常退出相同的 `stop_for_app_exit()` + 清注册表路径；SIGKILL 无法捕获，正是清扫要覆盖的场景。进程组/Job Object 的终止语义仍由 `process_tree.rs` 负责，本注册表不重复实现。

### 操作注册表（OperationRegistry）

- 逻辑 scope `(owner, workspace, session, generation)`
- 幂等键去重：`accepted_pending` / `duplicate_pending` / `duplicate_completed`
- crash/restart → Pending → Indeterminate（不可重放）
- 聊天终止（主聊天 + Quick/Side Chat）：宿主确认 owner/workspace/session/instance 与运行中操作 scope（每实例最近受理操作）后，向 Pi 发送原生 `{ "type": "abort" }`。Pi 0.85.1 的 `agent_start`/`turn_start` 与命令响应均不携带 `turnId`（实测 + rpc-commands.md），turn-bound abort 机制（turnId 事件绑定、显式 turnId 校验）已整体移除；若未来 Pi 上报 turnId，按 `pi-upgrade-impact` 流程基于新文档重新设计，不复活旧实现。

## 模块清单

| 模块 | 职责 |
| --- | --- |
| `host_server.rs` | axum 服务器、路由、v2 WS 协议、兼容路由 |
| `host_router.rs` | v2 hello 握手、客户端注册、帧路由 |
| `host_data.rs` | 数据面（list/read/write/containment/cost/export 令牌） |
| `pi_path.rs` | 内置 Pi 系统级 PATH 开关（`pi_path_status`/`pi_path_configure` 控制op）：POSIX marker 块管理 rc 文件、Windows HKCU 用户 Path + WM_SETTINGCHANGE；desktop-native owner、release-only、启动自愈，偏好键 `pi.pathEnabled` |
| `host_files.rs` | 文件读写安全（symlink/TOCTOU/atomic/0600） |
| `host_config.rs` | 设置/agent 文本文件（proper-lockfile） |
| `host_capability.rs` | capability 存储（mint/validate/revoke） |
| `native_pi_manager.rs` | 运行时生命周期管理、操作注册表集成 |
| `runtime_coordinator.rs` | 运行时状态机、turn 绑定、事件序列 |
| `operation_registry.rs` | 幂等操作注册表（scope/TTL/eviction/revoke） |
| `oauth_manager.rs` | OAuth 操作生命周期（generation 绑定） |
| `paste_offload.rs` | paste 临时文件（TTL/quota/symlink/.gitignore） |
| `transport_limits.rs` | 帧/响应/事件/快照/进度大小限制 |
| `cost_compat.rs` | cost-dashboard payload parity |
| `metadata_store.rs` | SQLite 工作区注册 + preferences；每个注册项持久化单个 Pi `session_bucket`；正式 spawn（`workspace_target_prepare`、`workspace_open`、`restart_runtime`）后由 host 主动 `get_state` 写回，`runtime_snapshot_request` 作为兜底；写入内容仅为 Pi `get_state.data.sessionFile` 的父目录，sidebar 只读该 bucket，不扫描全局 Pi sessions。schema 兼容契约：接受 user_version ≤ 6（Corp v4–v6 表归 Corp 构建，public 只读不建），public 迁移只完成 v1–v3 并只盖 v3 戳；public-owned `session_bucket` 列按存在性增量补齐，绝不改 Corp 版本戳 |
| `window_owner.rs` | 窗口 owner 注册与 capability |
| `remote_auth.rs` | 远程设备配对与 device token |
| `ephemeral_registry.rs` | Side/Quick chat 生命周期 |
| `git_service.rs` | owner-scoped Git status, diff, history, commit, and push operations；push 与写操作共享 per-root 写槽，认证严格非交互（GIT_TERMINAL_PROMPT=0 + askpass 抑制 + SSH BatchMode），超时 120s 后按进程组终止 |

| `pi_launch.rs` | 启动契约共享基底（binary/args/env/extensions） |
| `telemetry.rs` | D10 匿名遥测 schema（Stage 0 接线） |
| `process_tree.rs` | 进程树管理（Unix pgid / Windows Job Object） |
| `child_supervision.rs` | 运行时注册表 + 启动孤儿清扫 + 终止信号兜底（复用 process_tree 的终止语义） |

## Widget mirror registry

The main chat mirrors Pi `setWidget` payloads through `public/ui/widget-mirror-registry.js`. Ambient panels are keyed by the pushing runtime identity, so switching sessions hides inactive runtime panels and restores them when that runtime returns. Registered renderers such as rpiv-todo may consume tool results and history replay; unknown widget keys use a tolerant preformatted text panel. Blocking questionnaire UI is intentionally separate in `public/ui/questionnaire-card.js` because it has a one-shot lifecycle and must own cancellation and response draining. The card renders inline in the live turn's card slot (see below; Esc only while focused inside the card), not as a window modal. Because runtimes survive session switches, that card state (plus any walker requests already in flight) parks in `public/ui/background-questionnaire-store.js` keyed by session file / runtime id instead of being destroyed: a backgrounded runtime's `extension_ui_request` queues there with a sidebar unread badge, and the foreground mirror-sync path rebuilds the card and replays the queue when the user returns to that session.

The same survival applies to Pi's steer/followUp queue, but nothing on the wire can repaint it: Pi emits `queue_update` only when the queue mutates (push, dequeue on `message_start`, clear) and `get_state` reports just `pendingMessageCount`. `public/ui/pi-queue-park.js` therefore parks the last reported queue per session file (in memory only, dropped when that runtime stops or crashes, and not persisted — it mirrors Pi's live queue rather than becoming a second queue of record), and a session switch paints the incoming session's parked queue in place of the outgoing session's pills. Follow-ups themselves queue Picot-side instead (2026-09-26 spec): pi's queue protocol offers no per-item removal (`clear_queue` is all-or-nothing), so `public/ui/follow-up-queue.js` owns the follow-up truth per session file in localStorage (`pi-studio:followup-queue:<sessionFile>`), renders per-item edit/delete/send-now in `#queued-messages`, and drains one item at a time on the authoritative idle signals (`agent_settled`, an idle snapshot) — never on the optimistic idle an abort applies. Enter-while-streaming still goes to pi as a steer; pi's followUp bucket simply has no Picot writer anymore.

Blocking prompts — the datarx-safety-guard bash approval (`public/ui/safety-guard-dialog.js`) and the ask-user-question questionnaire (`public/ui/questionnaire-card.js`) — render **inline in the live turn that triggered them**, in `createTurnSection`'s unified `card` slot: the turn's last element, after the answer and its footer, so a required decision reads as the turn's newest stream item and never sits inside the rail's collapsible disclosure. Both fall back to the `#dialog-container` modal when no live turn can host them (replayed background request, parked restore, transcript re-render, abort). `closeLiveTurn` re-homes a still-pending card into the modal before the transcript drops its host turn: destroying the card would leave that runtime waiting on `extension_ui_response` forever, and answering `cancelled` on its behalf would silently Block.

Esc is card-scoped rather than document-level for both cards: an inline card shares the page with the composer and the stop button, and a page-level Esc must not be hijacked into a silent Block (questionnaire abandon) or a silent Block (approval cancel).

A session that returns to the front while its Pi run is still going gets no second `agent_start`, and history rendering carries no status row, so its running tail would otherwise read as a finished transcript behind a live stop button — and a run blocked on a question would look dead. Two rules keep that honest without trusting a cache: the **newest turn's rail always mounts expanded** (older turns stay folded, and the newest is the one holding this run's thinking, tool calls and any pending question, so it is never gated on a streaming flag that can be momentarily wrong exactly when the model is blocked); and the run-active signal is the OR of the host state machine (`runtime_snapshot`'s `lifecycle === "Working"`, event-driven and authoritative), the sidebar's streaming set, and Pi's instantaneous `isIdle` sample — only when all three say idle is the session treated as idle. On top of that, `handleMirrorSync` (and the disk-history render funnel) opens the adopted live turn after the transcript render (`maybeOpenAdoptedLiveTurn`), and the parked prompt is restored after that, so the card lands inline in the turn instead of in the modal container shared with the plain dialogs. Pi reports no run start, so that turn's elapsed readout begins at its first live output (`turn.js` `status.beginElapsed`) rather than at adoption; a turn that ended before producing anything is dropped instead of leaving an empty rail and status row behind. Because the in-memory park does not survive a reload or a cross-workspace return, the questionnaire is also derivable from the log itself: `public/ui/pending-questionnaire.js` finds the newest turn's `ask_user_question` call with no tool result, and an arriving blocking request with no card open rebuilds the card from that entry and binds the request (`adoptLoggedPendingQuestionnaire`) — the card's own matcher rolls the rebuild back if the request belongs to another tool, so a stale entry can never swallow someone else's dialog.

## Settings 数据面（/picot-config 桥）

Settings → Models/Configuration 的 catalog、API key、models.json、OAuth 操作不再走静态 `host_models` 读盘路径，而是通过 `extensions/picot-bridge.ts` 注册的 `/picot-config` 命令在 Pi 进程内执行：WebView 以 `runtime_request(prompt)` 发起，结果经 `ctx.ui.notify` 的 `__picotConfig` 帧按 request id 回关（`public/settings/config-gateway.js`）。模型 catalog 与认证状态读 Pi live `modelRegistry`，因此 shell 环境变量凭证（如 `ANTHROPIC_API_KEY`）能正确显示。Codex OAuth 走同一通道：login/logout 以 `oauth_logout`/`start_oauth_login` op 触发，事件以 `__picotOauth` 帧流式返回，前端在 runtimeEvent 分发前按 M3 互斥优先消费（`public/settings/oauth-gateway.js`）。Settings 的 skills inventory/mutation、默认 thinking level 也走 bridge。Settings → MCP 页（三层页签，pi-mcp-adapter 检测到才显示）同样走 bridge：`mcp_list_servers`/`mcp_save_server`/`mcp_delete_server`/`mcp_toggle_server` 四个 op（`extensions/mcp-settings.ts`）读写 adapter 的分层配置，只写 pi-owned 层（`<agent dir>/mcp-adapter.json` 与项目 `.pi/mcp-adapter.json`；adapter 3.0 起不再读 `mcp.json`），读盘仅检测旧 `<agent dir>/mcp.json` 并上报 `legacyMigration.available`（损坏则在 groupErrors 报错）；复制须用户在 MCP 页确认后经 `mcp_migrate_legacy_config` op 显式执行——adapter 已安装且 `mcp-adapter.json` 不存在时才可迁移，源文件保留、永不覆写（内嵌 Pi 无内建 MCP，旧文件否则无人读），enable/disable 复刻 adapter 的项目层覆盖语义（含 `.mcp.json` 下层判定、无变化跳写、空条目删除）。Settings → 已安装扩展详情页的 advisor 配置渲染器（`public/settings/package-extension-settings.js`）同样走 bridge：`advisor.config.get`/`advisor.config.set`（`extensions/extension-settings.ts`）读写 `~/.config/rpiv-advisor/advisor.json`，read-modify-write 保留未知键、tmp+rename 原子写 + best-effort 0600，模型列表与 effort 档位取自进程内 modelRegistry + pi-ai `getSupportedThinkingLevels`，生效时机为下次 session_start（advisor 每次 session_start 重读磁盘）。host 侧旧的静态 catalog、OAuth、skills inventory 路由已删除；`host_models.rs` 仅保留 settings.json IO。

Settings → 已安装扩展详情页的 pi-fff 配置渲染器（同文件 fff 条目）走 **host 控制面 op** 而非 bridge：`get_fff_config`/`set_fff_config`（`src-tauri/src/fff_config.rs`，main.rs 控制分发，Desktop+owner 门禁接受 landing owner）读写 `~/.pi/agent/pi-fff.json`（尊重 `PI_CODING_AGENT_DIR`）。get 计算逐字段 env > file > default 有效链与 shadow 集（host 进程 env 即内嵌 Pi 继承的 env；flag 检测放弃——内嵌 Pi 的 argv 从不含 `--fff-*`，终端 pi 逐实例不可观测，`flagShadowed` 恒空保持载荷形状），set 为单键 save-on-change：宽松读入后重建 schema 干净文件（additionalProperties:false，未知键丢弃、永不写出 invalid 文件），`reset` 写仅含 `$schema` 的最小文件，写经 `host_config::write_json`（proper-lockfile + tmp+rename + 0600）。走 host 意味着 landing 页（无 Pi 进程）也能配置 fff；文件编辑需重启 Picot 生效（fff 在模块加载时读一次配置）。

### Settings → Subagents（候选盘点与受限名字级覆盖）

`public/settings/subagents-tab.js` 经 `WsTransport` 调用四个专用 v2 host op：`subagents_inventory`、`subagents_get_detail`、`subagents_create`、`subagents_set_override`。控制帧仅允许当前认证的 desktop owner；项目请求必须附当前 Registered owner 的 `workspaceId` 与 `workspaceGeneration`，host 在阻塞扫描前后重新检查 owner 绑定并核验项目信任。Landing 只展示全局页，不提供项目身份。前端不传任意路径；host 从 `pi_launch::resolve_pi_agent_root()` 解析全局根，从 owner 注册表取 canonical workspace root。

`subagents_inventory.rs` 将全局、项目、安装包与可用内置定义整理成同一磁盘候选快照，列表只返回有限元数据；详情通过 host 发行的 candidate ID 重新扫描后限量读取本页范围内的原始 `.md`。`.agents/`、`agentScanDirs`、环境扫描目录和运行时注册代理等范围外来源只列来源与诊断，不读取或返回 prompt。扩展解析的 project root 与工作区根不同则禁止项目创建和覆盖。

当前 `scripts/subagents-parity-spike.mjs` 对内嵌 Pi 的 `/run` 与 `/subagents-models` 不能取得足够的运行时证据，结论是 `disk-candidates-only`：host 不声称磁盘候选即生效代理，不显示 winner/推算值。**受限名字级覆盖已开放（2026-10-02）**：自定义/扩展包/builtin 三类候选统一写 `subagents.agentOverrides.<runtimeName>`（全局层 `~/.pi/agent/settings.json`、项目层 `<cwd>/.pi/settings.json`，不修改任何 `.md`）；资格门为「本作用域快照内无已知 runtimeName/alias 冲突（含 alias↔alias）且 runner 为 native」——`writeQualified` 只表示快照内允许保存，**不证明 live winner**；范围内扫描不完整继续拒写，仅范围外未知占用（`.agents/`、运行时注册等）降为警告。已知被遮蔽者、external/未知 runner 不可保存；`.md` 新建的独立拒写门维持关闭。保存仍受 owner/trust/root/revision 校验与单锁原子事务保护。

写入实现位于 `subagents_settings.rs` 与 `host_config.rs`：覆盖限定 scope 的 `settings.json`、按写入层投影回显、锁内按文件 revision 读-比-改-原子替换并保存私有备份；新建限定 agents 目录（当前禁用），用同目录私有临时文件与 hard-link 排他发布，并在确认及发布前复验 inventory revision、同名候选集合。外部不遵循本锁的写者仍存在最终比对到 rename 间的竞态；父目录 symlink 竞态与 Windows/Linux hard-link 行为仍需平台实测。与 pi-subagents 0.74 的已知差异：host 额外接受裸 `ssh://` 包源。

#### pi-subagents 的来源优先级（0.75.0 源码实证）

静态定义在 `agentScope: "both"` 下按 **builtin < package < user < project** 合并（Map 覆盖序实现，`agent-selection.js:1-21`）：跨层同名有确定性胜者，低层被过滤出 effective 列表（`/subagents` 管理界面仍列全部来源层）。**同层**同名（两个 user 文件、两个包）胜者取决于扫描次序——0.75.0 未承诺稳定次序，Picot 对此维持双方拒写。alias 不参与覆盖查键（查键只有完整 runtimeName），但参与调用解析（canonical → localName → alias）；不同 runtimeName 共享 alias 在 `/run` 调用时报 ambiguous。runtime 注册代理参与发现但不参与 settings 覆盖，同名注册在合并时抛错。

#### 覆盖字段面（settings.json `subagents.agentOverrides.<runtimeName>`）

对 builtin/package/user/project 四类静态定义全部生效（custom/package user→project 逐字段叠加、project 胜出；builtin 项目条目整条替换 user 条目，且有效显式条目可绕过同层 `disableBuiltins`）。settings 覆盖 **替换** frontmatter 同名字段，优先级链为 settings > frontmatter > 默认值（provider 定向覆盖与单次 `/run` 参数更高）。runtime 注册代理的覆盖面收窄为 model/provider/fast/thinking 四项。

可覆盖字段（`parseBuiltinOverrideEntry`，`agents.js:700-873`）：`model`（默认继承父会话）、`thinking`（默认模型缺省；枚举 off/minimal/low/medium/high/xhigh/max/false）、`advertise`（默认 false；true 时 name+description 注入父 system prompt 的 agent 目录，上限 16 个/12KB）、`disabled`（默认未设=启用）、`description`、`tools`/`excludeTools`（子代理工具白/黑名单）、`systemPrompt`/`systemPromptMode`（默认 replace）、`inheritProjectContext`（默认 true）/`inheritGlobalContext`（默认 false，仅前者为 true 时有意义）/`inheritSkills`（默认 true）、`defaultContext`（默认 fresh；fork=携带父会话副本，仅一致性守门角色如军师/老法师使用）、`output`/`outputMode`/`defaultReads`、`machine`、`skills`、`extensions`/`subagentOnlyExtensions`、`allowNestedSubagents`/`allowedAgents`、`acceptance`/`acceptanceRole`、`fast`/`defaultProvider`、`mutationTools`/`toolBudget`。不可覆盖：`aliases`/`package`（身份字段）、`async`/`timeoutMs`/`skillPath`/`memory`（仅 frontmatter/调用层）。

Picot 设置页 UI 目前暴露 `model`/`thinking`/`advertise`/`disabled` 四字段（高频调优与运营开关）；其余字段属定义/契约性质（安全边界、人格、环境装配），留定义文件层。

#### 设置页的两个正交维度

- **scope 页签（Global / Current project）= 文件归属与写入目标**，不是运行范围：全局写 `~/.pi/agent/settings.json`、项目写 `<cwd>/.pi/settings.json`；`/run` 始终按 `both` 合并发现，页签归属不改变生效范围。项目层可覆盖全局定义（user→project 叠加）。
- **子页签（自定义 / 扩展包）= 纯前端浏览分类**：自定义 = 该 scope 的 agents 目录候选（全局含 builtin 只读组于扩展包子页）；扩展包 = 该 scope 的包来源候选按包身份分组。子页签切换不重新请求（同一盘点快照投影）；host 请求只带 global/project。

页面顺序为 scope 页签 → 描述 → 子页签 → 计数 → master/detail。子页签复用技能页 `.skills-scope-tabs` / `.skills-scope-tab`，描述与计数复用 `.settings-help`。自定义计数为该视图候选数；扩展包计数包含 builtin 候选，但包数仅按 package 来源的 `packageIdentity` 去重。启用开关位于 detail 名字行右端，即时写入/清除本层 `disabled`，保留其他字段草稿。模型选择器复用 rpiv-advisor 的 `loadModelChoices` / `appendModelOptions` 原生 select；经 `ConfigGateway` 加载真实 catalog 与 scoped 模型，landing 同样可按需派生全局配置会话。常驻盘点横幅和页底范围外诊断不再渲染；逐候选状态与禁写原因仍展示。归档 spec 见 [`2026-09-30-subagent-settings-design.md`](docs/superpowers/specs/implemented/2026-09-30-subagent-settings-design.md)。

当前生产用法（2026-10-03）：Dr. Lin 的 24 agent 团队定义全部在 git 源包 `datarx-agents-team`（`agents/{research,software,writing}/` 递归子目录），`.md` 不含 model/thinking；每角色分工经 `~/.pi/agent/settings.json` 的 agentOverrides 配置（23 条，与 Paseo agentProfiles 字节一致），小工按设计不配（继承调用方模型）。

### Settings → Skills（自定义 / 扩展包两页签）

技能页一级页签为「自定义」与「扩展包」（原三页签「已发现/安装/扩展包」已于 2026-10-03 收敛）。

**自定义页签**：内部保留 Global / Project scope 切换，按 scope 请求 host 控制面 `list_skill_inventory` 并按 `roots[].scope` 过滤展示；启停经 `set_skill_enabled`。「已发现」→「自定义」是浏览分类改名，语义不变（含自动发现目录与 customRules，非仅手写技能）。

**内联安装**：安装入口是 scope 行右端的「安装新技能」按钮（标准按钮样式），代表「安装到当前显示的 scope」（global → 用户级、project → 项目级；安装 payload 恒用 `global|project`，绝不写 inventory 的 `user`）。点击直接弹出原生目录选择器（无中间 idle 步骤），取消即收起安装区。安装区（pick→scan→select→confirm→install，`public/settings/skills-install-tab.js`）不是页签：app.js / landing.js 将其作为独立面板挂在自定义列表下方，`open(scope)` 固定目标并立即触发 picker；打开期间自定义页锁定（scope 页签/重扫/启停禁用），切换到扩展包页签仅隐藏面板、安装会话保留；「重新选择目录」只在 scan 落地（selecting）或失败（error）后出现。候选只传 opaque `{kind,id}`；host freshness 校验要求源变化重扫后再次确认。Landing 仅全局入口（无项目安装按钮）。安装成功后按安装 scope 刷新 `list_skill_inventory`，刷新失败不改变安装成功结论；runtime 不热重载 skills，须新建 session 或重启 Pi 生效。

**分组头控件**（2026-10-03 恢复）：自定义页 group 行 = 右对齐「{n}/{N} 已启用」徽章（span.skills-group-status）+ 三态开关（input.skills-switch；all-on 勾选、all-off 空、mixed 停中间 indeterminate），点击开关整组启停。b359730 曾把两页合并为单文字按钮，自定义页已改回旧形态；扩展包页维持合并按钮（其数据本就无 mixed 态）。

#### Packages（扩展包技能配置契约）

Pi 的 package skill 配置属于 `settings.json` 的 `packages[]` entry，而非独立的 skill enabled 表。entry 可为 source 字符串，或带 resource filter 的对象：

```json
{
  "packages": [
    {
      "source": "npm:example",
      "skills": ["!skills/**", "+skills/foo", "-skills/bar"]
    }
  ]
}
```

`skills` 未定义表示该 package 的 skills 按默认规则加载；空数组 `[]` 表示不加载该 resource type。普通 pattern 选择匹配资源，`!pattern` 从集合排除，`+path` 强制精确包含，`-path` 强制精确排除且有最终优先级。pattern 相对 package root 匹配 skill directory/`SKILL.md`。因此单个开关写入精确 `+relativePath` 或 `-relativePath`，不应将 UI 的 enabled state 持久化为另一套配置格式。

global `~/.pi/agent/settings.json` 与 trusted project `<workspace>/.pi/settings.json` 均可声明 package。project 普通 entry 按 identity 覆盖 global entry；匹配 global source 的 project `autoload:false` entry 是 delta：继承 global source/installed root，并以 project resource filter 覆盖 effective state。未受信任项目不得读取或写入 project package settings。

Picot 的 Packages tab 由 `public/settings/package-skills-tab.js` 渲染；它经 `/picot-config` 的 `list_package_skill_inventory` 取得 `extensions/package-skill-inventory.ts` 解析出的 effective package candidates 和 enabled state。单项切换发送 `set_package_skill_enabled`，bridge (`extensions/picot-config.ts`) 在同一 scope 的 settings 文件上使用 settings lock 与 atomic write 更新 `packages[].skills`，随后返回重算后的 inventory。该修改只影响后续 Pi resource discovery，响应携带 `runtimeRestartRequired: true`；当前 runtime 不热重载 skills，用户须新建 session 或重启 Pi 后生效。

### 项目信任（trust.json）

Pi 以 `~/.pi/agent/trust.json`（键为 canonical 路径，值为 true/false/null）决定是否加载项目本地 `.pi/` 资源（skills/prompts/extensions/settings.json 等）。RPC 模式无 UI 询问、Picot 的 `project_trust` extension 分支返回 `undecided`、Pi 对「ask 且无 UI」的兑底是不信任，因此 Picot 必须自己建立信任决定：

- **写入点**（`src-tauri/src/project_trust.rs`，均为 best-effort，失败仅 `log::warn` 不阻断）：①`workspace.add` 控制面 op 成功后，按返回的 `canonicalPath` 写入；②`pi_launch::native_launch_spec`（open_workspace / restart_runtime / workspace transition 的统一封装）在 spawn 前写入。通过 Picot 添加或打开已注册 workspace 即显式信任手势，会覆盖该路径的显式 `false`。ephemeral/quick/side-chat runtime 走 `native_launch_spec_for`，**不**写信任（临时目录保持隔离）。
- **写入协议**：复刻 Pi 的 proper-lockfile 语义——`create_dir`（原子 EEXIST，绝不可用 `create_dir_all`）在 `trust.json.lock` 目录上获取锁，10s mtime 过期阈值，20ms 重试、上限 750 次，`remove_dir` 释放；read-modify-write 保留其他条目，键排序 + 2 空格 JSON + 尾随换行与 Pi 的 `writeTrustFile` 逐字节一致，tmp+rename 原子落盘。
- **查询语义**：`skill_scope_context` 改用 `is_project_trusted`，对齐 Pi 的 `findNearestTrustEntry`——从项目根向上找最近的 true/false 条目（更近的显式 `false` 覆盖受信父目录），null/缺失继续上溯，无条目则不信任。

## Office 文件原生预览（anydoc）

选中候选 Office 文件（后缀 `doc/docx/rtf/odt/ppt/pptx/odp/xls/xlsx/ods` 共十种）时，`file_read` 走内嵌 `anydoc` crate（精确 pin `=0.2.4`，MIT）的原生转换分支，产物为只读 Markdown（`previewStatus:"ready"` + `renderAs:"markdown"`）。安全与资源边界：

- **输入上限分层**：普通读保持 8 MiB（`host_files::read`）；仅候选分支经 `read_with_cap` 用 32 MiB 专用上限。
- **输出上限**：转换 Markdown 超 2 MiB UTF-8 即失败（远低于 WebSocket 响应上限的 JSON 转义最坏情形）。
- **并发**：进程级两枚信号量 permit；请求先过 permit 才读盘/检测/解析，permit 随 blocking 闭包持有到缓冲区全部离开作用域。第三份并发请求只会等待，不占输入内存。提高上限需先做 macOS/Windows 双平台峰值 RSS 基准。
- **授权跨长任务**：`PreviewScope`（owner/workspace/generation）在 permit 等待前、permit 到手后、解析完成后三点重校验；workspace 转场中途落地则丢弃结果返回 `unauthorized_target`，绝不返回旧内容。
- **无硬取消**：进程内解析不可强停；浏览器 abort 只是忽略响应，不释放运行中转换的 permit。需要硬超时/硬取消时必须改为可杀死的隔离 worker 进程。
- **错误去敏**：adapter（`anydoc_preview.rs`）持有封闭错误码枚举，AnyDoc 细节（part 名、限额、路径、字节、Display 文案）不出模块、不进日志（host 只可记录固定码）；浏览器只见通用 `conversionFailed`。`ConvertError` 为 `#[non_exhaustive]`，通配分支只映 `Internal`。升级 anydoc 版本须重审依赖树 + 全错误码契约测试。
- **fail-closed**：候选后缀但内容检测为 PDF → `conversionFailed`，绝不改道 PDF 原始路由；检测出的非 Office 格式（EPUB/CSV 等）同样拒绝。
- **图片策略**：转换文档的 Markdown 渲染只接受 base64 栅格 data URI（png/jpeg/gif/webp），其余来源（SVG/远程/相对/未知 MIME）替换为本地化文本 `files.preview.converted.remoteImageHidden`。
- **无网络/无 OCR**：不调用 AnyDoc 托管 OCR/API key/任何网络路径；PDF 留在既有 PDF 预览路由。

## Provider 配额探针（Settings → 使用量 → 配额）

`extensions/provider-quota.ts` 在 pi 进程内对已配置 provider 的用量端点做只读探针（spec 2026-09-22，端点语义照抄 opencodex 生产实现）。边界：

- **候选 provider 来自 pi 自己的 provider 列表**（`ModelRuntime.getProviders()` + `getRegisteredProviderIds()`），且**只有 pi 报告已配置凭据（`hasConfiguredAuth`）的才探测**；「已配置」= pi 自己的 `checkAuth` 判定（`snapshot.configuredProviders` 由 `models.checkAuth()` 逐 provider 得出，另有一个只反映 auth.json 的窄集合 `storedProviders`），因此**环境变量来源的凭据同样算数**——宿主启动时从登录 shell 同步 PATH 与 provider 环境（`main.rs` 的 login-shell 同步 + `native_pi_manager` 的 `.envs(&launch.environment)`），Dock 启动也有；不要改用「auth.json 有无条目」当判据，那会漏掉 env-only 的 provider。models.json 自定义 provider 的凭据是条目自带的 `apiKey`。模型 catalog 不能当枚举源：它只知道「带 baseUrl 的模型」，会漏掉只有 provider 身份、没有对应模型的 codex/opencode-go/zai-coding-cn，却把无凭据的 provider 放进名单（实测：只有 deepseek 一张卡且是失败态）。
- **按 canonical baseUrl 选择，不按 provider id**：探针注册表只认固定 host 集合（chatgpt.com / api.z.ai / open.bigmodel.cn / opencode.ai / api.deepseek.com / minimax.io / minimaxi.com / moonshot.ai / moonshot.cn / ollama.com）；baseUrl 不匹配不发包（防把 key 发到仿冒 host）。
- **凭据不出 pi 进程**：api-key 走 `readStoredCredential`；models.json 自定义 provider 读条目 `apiKey`；openai-codex 走 `ModelRuntime.getAuth`（OAuth 刷新归 pi，失败报 `needs_login`）+ `readStoredCredential` 补 accountId。返回 WebView 的只有归一化配额数字与封闭错误码。
- **探针纪律**：`redirect:"manual"`（3xx 显式记为 `destination_blocked` 而非误报 timeout）、8s 超时、256KB 响应体上限（**流式**断读：`fetch` 的 `text()/json()` 会先缓冲全文，故按 reader 分块计字节，超限即 cancel）；瞬时失败（429/5xx/超时）保留 last-good 行 30 分钟，`response_unusable` 丢弃旧行；进程内缓存 TTL 5 分钟 + in-flight 去重，`force` 跳过。
- **Codex 重置额度双通道（不可逆操作）**：WebView → Rust 账本 `reset_credit_open`（单条条件 INSERT——未决行存在即不插入，故并发开单也不会留下两条 pending；operationId 即上游幂等键 `redeem_request_id`）→ pi 内 `consume` POST → Rust `reset_credit_settle`（settled/ambiguous）。启动清扫 60s 前的 pending 行为 abandoned；ambiguous 行下次重开对话框时先 `inspect` 对比 `available_count` 再决定重放同 id。账本表 `reset_credit_operations` 为 public-owned、existence-based 建表（session_bucket 先例，不动 Corp 拥有的版本戳）。
- **owner 门禁**：`reset_credit_open/settle` 与 `cost_dashboard` 同款——已认证 desktop owner、landing 可见。
- **UI 归属**：配额是「使用量」页内的独立子页签（「使用量」/「配额」两个 tab），面板挂载于 Settings 主 DOM 的 `#settings-provider-quota`，**不在**成本仪表盘 shadow root 内（原实装把它放进 infobar 的模型直方图旁，2026-09-23 拆出）。两页签各自首次选中时懒加载；`provider_quota_report` 经 `ConfigGateway`，载荷为 `{ok, data:{reports}}`——`ConfigGateway.call` 兑现 handler 载荷，不是 handler 自己的 data 对象。

## 浏览器面板与元素标注（browser pane）

右侧 file-preview 面板的 `browser` tab 在主窗口内叠加 `tauri::webview` child webview（spec 2026-09-22，需 `unstable` feature；`window.add_child(builder, position, size)` 创建即定位）。安全边界：

- **外部 webview 无 capability 初始化脚本**——它不是 owner，没有任何 host 控制权；`on_new_window` 一律 Deny。
- **URL 白名单**：仅 http(s)；file:/自定义 scheme 拒绝。host 自身按**别名 + 端口**封禁而非只比序列化 origin——`http://localhost:<host端口>/`、`http://0.0.0.0:<host端口>/`、`http://[::1]:<host端口>/` 以及 LAN 地址（含 RFC1918/链路本地）在 host 端口上全部拒绝，因为这些别名都指向同一台服务器却序列化不同（`pane_url_allowed` + `resolves_to_host`）。`on_navigation` 运行时同策略，防外部页跳回 host 窃取窗口上下文；userinfo 不是 host 旁路（`http://x@evil.com` 的 host 就是 evil.com，属普通外部页）。非 host 端口上的私有地址照常放行（LAN dev server 是正当目标）。
- **pane 身份窗口限定**：pane key = `<windowLabel>:<tabId>`（WebView 侧生成），故两个 workspace 窗口可同时持有同一文件的两份 pane，派生 webview label 也全局唯一。
- **布局同步**：pane 容器 ResizeObserver → rAF 合并 → `browser_pane_set_rect`（logical 坐标，rect = 容器盒，无内缩）；宽/高 < 2px 隐藏原生视图。ResizeObserver 只报尺寸、不报位移，故侧栏展开/收起这类**纯平移**由 app 广播 `picot-layout-settled`（`transitionend` + 350ms fallback），面板收到后重下发 rect。
- **标注评论框注入页面内**（原生 child webview 永远压在宿主 DOM 之上，宿主侧卡片只能活在 pane 外的预留条里，视觉不可接受）：经 eval 桥把 Paseo 式卡片（fixed、水平居中、贴页底、内联样式 + 宿主运行时读取主题 `--accent` 注入，页面看不到应用 CSS 变量）注入 pane 页面；宿主轮询 `__picotAnnotationResult` 标记（`__picotAnnotationAlive` 消失 = 页面重载，按取消处理）。页内 Esc/Cmd+Enter 由注入脚本自行处理。
- **pane 生命周期跨页面重载**：切 workspace 会整页重载宿主 WebView，原生 pane 在 Rust 侧存活而宿主 PANES 清零。因此 `browser_pane_create` 为**替换语义**（残留 pane 关闭重建，否则恢复 tab 恒报 `pane_already_exists`）；`closePane` 在无宿主记录时按派生键补发 destroy，防止残留泄漏。tab 切换走 hide（webview 存活）；tab 关闭才 destroy。窗口销毁时 `destroy_all_for_window` 清映射，label 可复用。
- **全窗覆盖层与 pane 互斥**：`#settings-panel`/`#dialog-container`（todo 清除、safety-guard、问卷确认共用）/`#session-search-overlay`/`#lan-qr-modal`/`#config-editor-overlay` 全部经 FilePreviewPanel 的 class MutationObserver 列表驱动 `hideAllPanes`，全部落下后恢复活动 pane；图片 lightbox 为 body 追加节点型，由 body childList observer 兜住。z-index 抬不动 OS 级 child webview，任何新增全窗覆盖层必须进这个列表（或改为 body 追加节点型）。
- **eval 桥**：`browser_pane_eval` = Rust `eval_with_callback` + try/catch 包装（Windows 异常被平台吞掉，靠包装脚本返回 `{ok:false,error}` 传回）+ 5s oneshot 超时；pane 锁作用域块级收束（MutexGuard 不得跨 await）。WebView 侧拿到的**就是 envelope 本身**（`data_response` 的兑现值），不是 `{result}` 包装，且编码层数随运行时可变（对象或 1..N 层 JSON 字符串），故 `unwrapEnvelope()` 逐层解到非字符串为止；读不出来时必须报形状（`eval_failed:keys:…`），不得退化成裸 `eval_failed`。
- **元素选择器**（`public/browser-pane/element-selector.js`，Paseo 移植）：IIFE 注入 + `window.__picotSelectorResult` 200ms 轮询 + session token 防串台 + Esc 取消 + 30s 超时；增强 `closest('[data-path]')` 采集 `docPath`。officecli watch 页面 `data-path` 与 `officecli set/add` 坐标同源——标注即**可执行修改坐标**（`<office-element>` 附件含 `suggested: officecli set …`）；普通网页走 Paseo `<browser-element>` 格式。附件以文本块进 composer（`#message-input`），用户可改后再发送。
- **officecli watch 生命周期**（`officecli_watch.rs`）：canonical 路径去重（同文件多 tab 共享一个 watch；启动竞态在锁内二次确认，输家进程立即回收而不会被覆盖泄漏）；空闲端口分配；stdout 解析 `Watch: http://localhost:PORT`（15s 启动超时）；`WatchEntry` 的 `Drop` 统一 SIGTERM→2s→SIGKILL 并 wait 收尸，故任何丢弃路径（覆盖插入、死进程清理、stop、退出清场）都不留孤儿进程或僵尸。面板销毁时同步停掉其 office 文件的 watch。`watch mark` 服务端打标（刷新不丢）。Rust 侧 watch 与 pane 均 owner 门禁 data op。
- **入口分工**：office 文件点击 → anydoc markdown 预览（快、零依赖）→「内置浏览器打开」按钮 → `officecli_watch_start` → browser tab（保真 + 标注 + agent 闭环）。无 officecli 时按钮报 `officecli_missing` toast。
- **不做（一期）**：元素截图、普通网页徽标 overlay、历史/书签、agent 反向自动化（Paseo 22 命令，三期）。

## 兼容路由（P8 删除候选）

`/api/*` compatibility routes maintain existing shell behavior on host origin. Each route uses owner capability authorization. Runtime traffic must use `/v2/*` and `/v2/ws`; retained HTTP routes are explicit compatibility or retirement responses, never Pi-origin forwarding.

## 静态资源

构建产物按内容指纹版本化：`/v/<fingerprint>/...`，`Cache-Control: no-store` 防止 auto-update 后 WebView 缓存旧 release。

## 安全边界

1. **Loopback 默认**：HostServer 默认只绑 loopback；`mobile.lanAccessEnabled` 显式开启后才绑全部网卡（D4 移动接入；缺省一律 loopback，配对 token 仅桌面端可铸造）
2. **Owner capability**：每个桌面窗口持唯一 32 字节随机 capability
3. **Workspace containment**：所有文件读写限制在注册根目录内；`file_create`/`file_rename`/`file_delete` 同样只在注册根内生效，且受 owner+workspace+generation 门禁；`file_mentions` 的列举按上表根分级可越出（仅 desktop，读写不受影响）
4. **Generation 失效**：workspace transition 使旧代授权、操作与导出令牌全部失效；旧代 runtime 进程保留存活但不可达（授权闸门拒收），至窗口销毁/owner 撤销/app 退出、显式 restart（`restart_runtime` 控制面命令，Registered owner 经 Settings 触发）或返回 rebind
5. **跨 workspace 事件可见性**（2026-09-20 拍板）：持有 desktop capability 的本机窗口可订阅任意 live runtime 的全部非阻塞事件（消息正文、tool 输出、widget、notify）；阻塞式 `extension_ui_request`（select/confirm/input/editor）仍只投 `authorize_target` 通过的订阅者。desktop capability 只由原生窗口 owner registry 铸发，LAN 配对设备（Browser 类客户端）拿不到。
6. **匿名遥测**：仅 allowlisted 粗粒度字段，无 per-user/per-token 维度
