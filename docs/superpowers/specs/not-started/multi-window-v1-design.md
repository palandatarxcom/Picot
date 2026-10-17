# 多窗口 v1 设计

**状态：** Draft 待评审。讨论稿的七个决策点（D1–D5 及两项补充）已由 Dr. Lin 于 2026-10-17 拍板；2026-10-18 需求变更（D8/D9，§3）将双可写纳入范围并推翻「一 workspace 一窗」，本文按两阶段结构修订；同日军师评审（MW-1~MW-10）意见已逐条处置并入正文，处置表见 §15；实施范围以评审确认为准。
**日期：** 2026-10-17
**演化关系：** daemon phase1（`2026-10-15-picot-daemon-phase1.md` plan，T1–T13 已交付）之上的形态增量。本文服务本机场景（同一 daemon、无 relay）：与 session-resident-views Phase C 共享同一批底层能力，relay 场景互不依赖、准入放宽结论可被其复用（§8）；窗内 tab（Phase B，09-22 已拍板不做）不复活。
**参照：** 多窗口可行性讨论稿（2026-10-16，subagent 输出）；`2026-09-20-persistent-daemon-relay-design.md` §3.2.1（单可写绑定）；`2026-09-20-session-resident-views-design.md` Phase C；`2026-09-18-cross-workspace-runtime-lifecycle-divergence.md` §3。

## 1. 问题

daemon phase1 之后，runtime 的宿主是 daemon，窗口退化为客户端。但产品仍是单窗口。多窗口的真实动机（D8）：Picot 无 tab，左侧 session 导航即 vertical tab，同屏只能看一个 session；要对照两个 session 只能来回切换 workspace/session，而**切换 workspace 至今仍有全屏刷新问题**（性能痛点）。多窗口是止痛：不同窗开不同 workspace 的 session，或**同 workspace 的不同 session**。多窗天然也能打开同一 session，「两窗同看同发、消息互相同步」的双可写由此进入范围（D9，第二阶段）。

讨论稿的结论是多窗口与 daemon 架构同构，地基约七成已在。daemon 侧的堵点收敛在准入、应答授权与订阅身份（§13.1–13.3），真正的大头在前端 chat 视图从本地状态机到 runtime 状态投影的改造（§13.4）。本 spec 定义 v1 的范围、入口、边界与两阶段切分。

**Open question（待 Dr. Lin 定）：** 切换 workspace 的全屏刷新是否独立立项修复。这是与多窗口并列的独立需求——减少切换成本与同屏对照是两种诉求，修复与否不改变本 spec 的范围与优先级。

## 2. 已验证事实（2026-10-16/17/18，本 worktree 核对）

| 事实 | 证据 |
| --- | --- |
| 通用窗口工厂 `open_native_window(label, url, mirror, owner, capability)` | main.rs:294 |
| 冷启动 landing 窗，label 硬编码 `native-landing` | main.rs:394-412 |
| workspace 窗工厂：label `native-workspace-{workspace_id}`，route `/workspaces/{wid}/sessions/{sid}`，先记 ledger 再开窗 | main.rs:414-451 |
| host 控制 `open_workspace`：mint owner → spawn → 默认 `openWindow:true` 开窗；spawn 失败撤销 owner，开窗失败 stop 刚 spawn 的 runtime 再撤销 owner | host_control_dispatch.rs:240-322（错误路径 :273-275、:309-310） |
| 同 label 二 owner 被 owner registry 拒绝（"window label is already owned"） | window_owner.rs:275 |
| daemon 经 app-surface Row A2 把开窗转发回 app 执行 | tauri_app_surface.rs:53-70、host_control_dispatch.rs:299-305 |
| WindowLedger 按窗口 label 记账（owner、workspace 绑定、route） | src/daemon_client/window_ledger.rs |
| restart 迁移逐窗进行：capture 按 ledger 捕获（含 SPA pushState 后的 live route）→ 逐窗重建；`classify_exit_request` 已有 windows_open 概念（还有窗就 Hold 不退出） | main.rs:2465/2517、main.rs:3116-3120/3207 |
| 单可写绑定在协议层：同 session 被其他活 owner 持有 → `PrepareReuse::Blocked` → 错误串 "this session is already open in another window" | host_control_dispatch.rs:1390-1393、:2082/:2135 |
| 关窗 = detach 保留 runtime（T7 语义原文 "No resident workspace/session runtime is stopped"）；显式停走 `workspace_close` | picot-host daemon/mod.rs:472-480 |
| 每窗独立加载、独立 WSClient、capability 各自注入：bootstrap-entry 按 pathname 分叉 landing.js/app.js | bootstrap-entry.js:21-23 |
| 关窗确认按 owner 寻址；菜单 ⌘N 按聚焦窗 owner 判定后向该 WebView 合成 keydown | main.rs:2973-2997 |
| 前端 ⌘N 处理器（key 'n' + meta） | app.js:5298 |
| 停止 runtime 后窗内视图停留原地（不自动回 landing；app 侧无回 landing 路径） | app.js:3039-3045、3094-3100 |
| `transport.openWorkspace`（含 `openWindow` 参数，默认 true）只有定义没有生产调用方——整条「开窗」链路接通但从未被生产 UI 锻炼 | transport.js:57-68 |
| 通知点击 `notification_activated` 由 daemon 单点广播到全部客户端，payload 含 `workspaceId`/`sessionId`/`sessionFile`/`cwd`；每窗 listener 自行判定是否导航（本窗非该 session 即导航——单窗下正确，多窗下会多窗齐动） | host_server.rs:3487-3496、app.js:2840-2851 |
| landing 进工作区走 `enterWorkspace` 原地转场（prepare → commit → 窗内导航），非开窗 | landing.js:194-238、侧栏入口 :240-255 |
| window-state 插件按 label 存几何，多 label 天然支持 | main.rs:3019 附近 |
| 任意 desktop owner 可订阅任意 live runtime，事件逐连接扇出；订阅与命令准入分离（"command admission stays authorize_target's job"） | host_server.rs:1934-1957、:2048-2066；cross-owner 订阅 admit 测试 :7599-7626 |
| 命令与快照统一闸门：`RoutedAction::Runtime` 先过 `authorize_target`，要求 `live.owner_id == client.owner_id`——第二窗（第二 owner）的写与快照均被拒 `unauthorized_target` | host_server.rs:2514、:2306-2328 |
| 阻塞对话框只投递给过 authorize 的订阅者，订阅时 pending 重放同理；应答只认 live owner（`dialog_response_allowed`） | host_server.rs:2058-2061、:1952、:2662-2675 |
| `abort_turn` 要求请求方 scope 匹配该实例最近一次 accepted operation——跨 owner 不能停对方起的 turn | native_pi_manager.rs:899-905 |
| `workspace_generation` 是 per-owner 记录：同 workspace 内 session 导航不 bump，跨 workspace 转场取全局计数器新值 | window_owner.rs:195、:382-397 |
| composer draft 按 session 身份键、DB 持久化、每窗内存写穿缓存——天然跨窗共享（经 DB）但无活性传播，双窗并发编辑存在丢更新竞态 | app/composer-draft-store.js:1-60 |
| 重连即「快照 + 事件」重建：resubscribe + `runtime_snapshot_request` 是既有路径，投影重建机制已有先例 | host_server.rs:6822/:6984 测试、public/app.js 重连处理 |
| `open_workspace` 随机生成 `session-<uuid>` 直接 spawn，不经 `plan_prepare_reuse`；复用判定只在窗内 `workspace_target_prepare`，经 `persisted_session_id_for_workspace`（canonicalize + 归属校验 + 文件头 session ID）解析持久身份 | host_control_dispatch.rs:246-322、:1345-1393（2026-10-18 评审核实） |
| app 侧 workspace 窗工厂自行计算 label；`AppSurface::open_workspace_window` 无 label 参数——label 存在 daemon/app 双方各算一份的隐患 | main.rs:422、tauri_app_surface.rs:56-69 |
| hello_ack 后先订阅再请求快照（不是「快照→订阅」）；快照前事件已开始流入，客户端有 sequence 水位/gap 恢复机制 | websocket-client.js:701-719 |
| streaming 时 Enter 直接走 steer（不进队列）；follow-up 队列是 Picot 自有 localStorage 实现（按 sessionFile 持久 + 每窗内存缓存），每窗 `agent_settled` 各自 drain，防重标志仅在本窗 | app.js:4227-4244、:3396-3413、ui/follow-up-queue.js |
| 订阅按完整 `RuntimeTarget` 匹配（Hash/Eq 含 owner_id 与 workspace_generation）；rebind 改变身份后旧订阅失配 | runtime_coordinator.rs:7-20、host_server.rs:1948、:2050 |
| `respond_extension_ui` 先 `send_frame` 后删 pending，无原子认领；host 侧 `duplicate_completed` 只是回复形状，不是去重 | native_pi_manager.rs:1828-1870、host_server.rs:2681-2692 |

## 3. 决策记录（Dr. Lin，2026-10-17）

| # | 决策 | 内容 |
| --- | --- | --- |
| D1 | 多窗口的窗口承担 **window 角色**（选 A） | 关窗 = detach 保留 runtime；杀 runtime 走既有「停止运行」入口。心智模型（原话转述）：Picot 无 tab 概念，左侧 session 导航 = vertical tab；「关 tab」语义由窗内承载（停 runtime → 回 landing）；主聊天视图无关闭按钮 |
| D2 | 关最后一扇窗 = app 退出（现状不动） | hide-on-close / 无窗常驻形态不进 v1，另立议题（daemon spec §3.2 已挂「待另行拍板」） |
| D3 | 「移到新窗口」v1 砍掉 | 单可写绑定下「移到」= 原窗必须降级 landing，语义链复杂，等真实需求 |
| D4 | 同 session 二开撞 Blocked → 自动聚焦既有窗 | **2026-10-18 修订（D9）**：聚焦语义在 P1 保留；P2 起 Blocked 改写为 co-attach（§13），同 session 双可写；Phase C relay 场景仍另行立项 |
| D5 | 并排形态走多窗口 | 窗内 tab = Phase B 复活，不做；两条路不互斥，未来 tab 若复活是容器内层 |
| D6 | 新窗口入口 = File 菜单 New Window + **⌘N**；快捷键重分配：⌘N 改绑 New Window（macOS 惯例），New Session 迁 **⌘T**（与 vertical tab 心智对齐） | 迁移涉及三处：菜单 accelerator（main.rs:2826-2829）、菜单事件合成 keydown（main.rs:2973-2997）、前端处理器（app.js:5298） |
| D7 | 概念锚点（原话）：**「一个新的 Picot 窗口，就像一个移动端客户端访问 daemon。」** | 见 §4.1。多窗口对现有窗内操作零影响（§4.4 硬约束） |
| D8 | 多窗口的真实动机：不同窗开**不同 workspace 的 session（或同 workspace 不同 session）**；Picot 无 tab，切换 workspace 至今仍有全屏刷新问题，多窗口是止痛 | 2026-10-18，Dr. Lin。推论：**「一 workspace 一窗」invariant 废除**（§6 label 规则多实例化） |
| D9 | **双可写进入范围**（两窗同 session 均可看、均可发、消息互相同步），作为 v1 第二阶段（P2）；P1 地基先行 | 2026-10-18，Dr. Lin。推翻 D4 原「另行立项」口径与 §9 原旁观记录；D4 在 P1 保留聚焦语义，P2 起改写为 co-attach（§13） |

**D1 行为澄清（评审时确认）：** 现状是停止 runtime 后窗内视图停留原地，不会自动回 landing（§2）。「停 runtime → 回 landing」是心智模型层面的表述；若要它落地，是窗内行为变更，与 D7 的零影响约束冲突，需 Dr. Lin 在评审时明确取舍。v1 默认按现状（视图停留）。

## 4. 窗口模型

### 4.1 窗口 = 又一个客户端

一个新的 Picot 窗口，就像一个移动端客户端访问 daemon。每个窗口 = 一个 client 实例（WebView + owner + capability），与移动端浏览器、未来的 relay 客户端完全同构、地位平等：owner 模型、capability 鉴权、单前台绑定、Blocked 分支全部复用，没有「主窗 / 从窗」。daemon 眼里，桌面第二窗和手机连进来是同一种东西。新窗口冷启动行为就是任何新 client 的冷启动：落 landing，单向流进 session。

### 4.2 关窗语义（D1 = window 角色）

- 关任意非最后窗 = `detach_owner`：runtime 留活，重开该 session 接回现场。多窗口第一次让 T7 的 detach 语义真正可观察（单窗时代唯一窗口关闭 = app 退出，detach 观察不到）。
- 杀 runtime 的入口不变：侧栏「停止运行」/ Settings 的 running-agents 列表（`workspace_close` / `agent.stop`）。主聊天视图无关闭按钮。
- D2：关最后一扇窗 = app 退出（现状）。`classify_exit_request` 的 windows_open 分支已就位。

### 4.3 landing 角色

landing 是纯入口（启动器）。新窗口打开即 landing 页；窗内点 session 替换 landing（既有 `enterWorkspace` 原地转场），单向流，无回退导航。不引入「关最后 workspace 窗自动弹 landing」的隐藏行为；restart 降级落 landing 的既有语义不变。

### 4.4 零影响硬约束

多窗口对现有窗内操作零影响：session tree 导航、替换 landing、settings、所有既有交互原样。多窗口只新增「再开一个 client」这一个动作（⌘N / File 菜单），不改任何窗内行为。单窗用户零感知。

## 5. 入口与 ⌘N/⌘T 迁移

三个入口：

1. **File 菜单 New Window（⌘N）**：开新 landing 型窗（Rust 侧直接调 landing 窗工厂，不经 WebView）。
2. **landing 项目列表「在新窗口打开」**：`transport.openWorkspace(path, { openWindow: true })`——这条链路（§2）从暗路径转正，是本 v1 最大的生产锻炼点。
3. **侧栏 session「在新窗口打开」**：任意 workspace 的 session 都可为它开新窗（`openWorkspace` 带 `sessionPath`）——同 workspace 不同 session 正是 D8 动机场景；该 session 已在别窗活跃 → 走 §6 的聚焦规则（P1 聚焦 / P2 双可写）。

⌘N/⌘T 迁移（D6）：菜单项 New Session 的 accelerator 从 `CmdOrCtrl+N` 改 `CmdOrCtrl+T`（main.rs:2826-2829），菜单事件合成 keydown 的 key 从 `'n'` 改 `'t'`（main.rs:2973-2997），前端处理器同步改绑 ⌘T（app.js:5298）；新增菜单项 New Window 绑 ⌘N，处理器在 Rust 侧开 landing 窗，不再向聚焦窗 WebView 合成事件。菜单文案与快捷键提示同步更新。

## 6. label 权威与同 session 复用/聚焦（S1，硬前置）

### 6.1 label 一次性分配的权威方（MW-2）

一次分配、一次保留，每类窗只有一个权威：

| 窗型 | 分配权威 | label 规则 |
| --- | --- | --- |
| landing 窗 | app（landing 窗无 daemon owner 参与，app 是唯一创建方） | 首窗 `native-landing`（main.rs:405，window-state 几何延续），后续 `-2`、`-3`…，序号取第一个空闲值 |
| workspace 窗 | **daemon**（唯一创建路径是 `open_workspace` 的 A2 开窗） | 首窗 `native-workspace-{workspace_id}`，后续 `-2`、`-3`…；mint owner 时占用，失败即 revoke owner 释放 |

同一个 label 贯穿全链：daemon owner registry（占用）→ `AppSurface::open_workspace_window` 新增 label 参数（现不传）→ app 窗口工厂（main.rs:422 现自行 `format!`，改为使用传入值）→ WindowLedger 行 → owner↔label 镜像。app 不再自行计算 workspace label。并发两次开窗：第二次 mint 在 registry 撞「window label is already owned」（window_owner.rs:275）——这是原子冲突点，调用方换下一个序号重试即可，app 侧不做预检（检查后占用竞态由此消除）。

### 6.2 `open_workspace` 先解析身份再 spawn（MW-1）

现状：`open_workspace` 随机生成 `session-<uuid>` 直接 spawn（host_control_dispatch.rs:246-322），`plan_prepare_reuse` 只被窗内 prepare（`workspace_target_prepare`）调用。只修 label 不修这条，同一磁盘 session 会被再起一个进程。修订后 `open_workspace` 流程：

1. **解析持久 session 身份**：带 `sessionPath` 时，用既有 `persisted_session_id_for_workspace`（canonicalize + 归属校验 + 文件头 session ID 三重校验）取持久 session ID；读不出或不属于该 workspace → 报错收场，不 spawn。
2. **判复用**：先 mint owner（§6.1 label——`plan_prepare_reuse` 需要请求方 owner 身份参与判定），再 `plan_prepare_reuse(runtimes, registry, workspace_id, owner, Some(持久ID), forceNew)` 三分支：
   - `Spawn` → 现有流程：mint owner（§6.1 label）→ 以**持久 session ID** 为 target 身份 spawn（不再用随机 ID，spawn 后身份可被后续查找命中）→ 开窗。原子保留落在 spawn 本身：coordinator 按 (workspace, session) 唯一，撞身份失败 → revoke owner → 转 Blocked 处理（重查持有者聚焦）。不引入独立的预约锁。
   - `Rebind(target)` → 持有者已 detach（窗已关/attachment 已死）：rebind 到新 owner，不开新进程，直接开窗挂现场。
   - `Blocked(target)` → 活 owner 持有：revoke 刚 mint 的 owner（label 随之释放，与既有错误路径同款回滚）→ 从 owner registry 解析持有者 label → `focus_window` → 返回机器可读结果（`focusedExisting: true`），前端 toast「已在其他窗口打开」。本路径不 spawn。
3. 不带 `sessionPath`（开 workspace 新 session）维持 spawn，同样经 §6.1 label 权威。

窗内 prepare 入口（`workspace_target_prepare`）已具备步骤 1–2，只受益于 §6.1 的 label 权威与 §6.3 的聚焦改造。同 workspace 二开（不同 session）在两入口下都是 `Spawn`：每个新窗 = 新 owner = 自己的 runtime，同 workspace 多 session 各自单活 runtime，互不干扰；restart 迁移本就按 label 逐窗（main.rs:2465/2517），无需改。

### 6.3 focus 失败只重核持有者，不回退开新窗（MW-2）

`focus_window` 失败（label 解析不到、窗口已不存在）只允许重核持有者，**不允许回退 normal open path**——旧稿「focus 失败 ⇒ 正常开窗」废除，它会绕过活着的持有者直接破坏单可写绑定：

- 重核后持有者**存活**（registry 记录活跃）→ 返回可重试错误（前端 toast「聚焦失败，请重试」）。镜像陈旧是常见原因（owner↔label 镜像 daemon-fed），重试通常自愈。
- 重核后持有者已 **detach**（窗已关）→ 走 Rebind：该 runtime 本就可安全接管，开窗挂现场。

`focus_window` 的 label 解析在 daemon 侧完成（owner registry 自持 label），复用 close 流的 owner↔label 镜像语义（tauri_app_surface.rs:44-50 同款机制），不新建通道。

测试断言（plan T1）：进程/实例数不增、session 身份不漂移（同 session_id）、focus 解析到正确 label——三者都要，不能只断言 focus 被调用。

## 7. 通知点击路由（确定性规则，MW-3）

现状：daemon 单点广播 `notification_activated` 到全部客户端（host_server.rs:3487-3496），每窗 listener「本窗非该 session 即导航」（app.js:2840-2851）——多窗下多窗齐动；目标不在任何窗前台时，无人处理。同 workspace 多窗后按 `workspaceId` 过滤也不够。v1 改为**单一决策点 + 确定性选择规则**：

- **决策点在 app 侧 Rust**：daemon 在广播之外，经 `AppSurface` 通道把激活事件转发给 app（`notification_activated(payload)`）；app 依据 WindowLedger（label → route，含 SPA pushState 后的 live route）与各窗焦点状态做一次决策。前端每窗 listener 退化为被动接收端（显示中的窗不自行判定导航），消除「多窗齐动」与「无人动」两端缺陷。
- **选择规则**（按序取第一个命中）：
  1. 有窗前台正显示目标 session → 聚焦其中**最近聚焦过**的一扇，不导航（已在显示）。
  2. 无窗显示该 session，但有窗绑定目标 workspace（route 前缀 `/workspaces/{wid}`）→ 聚焦其中最近聚焦过的一扇并导航到目标 session。
  3. 以上皆无（含目标窗已关、只剩其他 workspace 的窗）→ 走 §6.2 的 `open_workspace(sessionPath)` 链开新窗（该链自带复用/聚焦保护，不会双开）。
- **理由**：通知点击是明确的「我要看这个 session」意图，任何分支不允许变成无操作；但通知是被动的，不导航到其他 workspace 的窗（规则 2 只在目标 workspace 自己的窗内导航）；开新窗是最后手段且受复用链约束。「最近聚焦过」是确定性偏序（Tauri 焦点事件维护时间戳，平局取 ledger 顺序首位），单窗、多窗、P2 同 session 多窗同构。
- 身份比较以 `sessionFile` 为主（持久身份）、`sessionId` 为辅（live 身份），区分两键。
- 通知本体仍由 host 单点发出，无双发问题。

测试覆盖（plan T3）：无人显示目标、目标窗刚关闭（ledger 残留行 vs 活窗）、同 session 多窗、同 workspace 多窗四类；身份比较区分 sessionFile 与 sessionId。

## 8. 与既有 spec 的关系

- **session-resident-views Phase C：场景不同、互不依赖。** Phase C 的原始场景是 relay——有了 relay 之后，一个 Picot 经 relay 加入另一个 Picot 的 host，两个窗口显示同一 session 的实时记录，是跨实例共享，不是本机多窗口的上位概念。本机多窗口（v1）是同一 daemon 内的增量，不依赖 relay，也不预支 Phase C；每窗一个前台 session、单 target 订阅（`runtime_subscribe`），全部用现有机制。两者共享同一批底层能力：宽通道事件订阅（09-20 拍板）与单可写绑定 invariant。Phase C 的 relay 场景范围不变，仍待 relay 落地后按原 spec 要求单独立项并过授权生命周期评审。该 spec Phase C 节已按此口径补注（本批修订）。
- **单可写绑定 invariant（daemon-relay §3.2.1）：P1 的安全底座，P2 将其改写而非废除。** P1 维持「每 session 一个可写 client」（D4 聚焦是它的产品化出口）。P2 把它改写为「每 session N 个可写 client，全部在同一 workspace 信任域内」（§13.1）——这属于授权生命周期变更，与 session-resident-views Phase C 登记的评审要求同源，P2 开工前单独过安全评审。
- **cross-workspace divergence §3「单窗口架构保留」**：已补历史语境（本批修订）——该句约束 09-18 恢复工程的范围，不构成多窗口增量禁令。
- **relay phase-2：多窗口 P1 先行，两线串行（MW-10）。** 两线都改 `host_server.rs` 热路径（relay 泛型化会话入口与 remote hello；多窗口改订阅/授权/对话框），并行写入不成立——单 worktree 单写者，relay 接续多窗口 P1 之后落地，每线合入时跑对方回归。「客户端概念同构」不等于两种凭据同权：relay 用 `HostClientContext::remote`，本轮多窗口 P2 的 co-holder 放行明确限定 `ClientKind::Desktop`，remote principal 的对照是 relay spec 的议题，不在此预支。
- **ARCHITECTURE.md**：v1 实施时补窗口模型事实（A2 open_workspace 从暗路径转正、多窗 label/聚焦规则、通知路由）。本批不动。

## 9. 非目标（P1 范围外）

- 「移到新窗口」（D3）。
- **双可写同 session 第二窗**：不再是旁观项（D9），已入范围为 P2（§13）；只读第二窗不单独立项——双可写交付后只读是其自然子集。
- 窗内 tab（Phase B 已否决；D5）。
- hide-on-close / 无窗常驻（D2，另立议题）。
- Windows 平台差异：v1 只核对 macOS 菜单与快捷键路径；Windows 随 daemon Phase 1 的 Windows 打包 follow-up 一并处理。
- macOS Window 菜单多窗列表 / Dock 菜单多窗项（muda 手拼，可选，v1 不做）。

## 10. 测试计划

单测（S1/S2 红测先行，见 plan）：

- workspace label 多实例化：同 workspace 二开（不同 session）各得唯一 label、各自 runtime，无 "window label is already owned" 报错。
- Blocked 分支：聚焦持有窗，无错误串抛给前端；进程/实例数不增、session 身份不漂移（不止断言 focus 被调用）；focus 失败且持有者存活 → 可重试错误而非开新窗（§6.3）。
- ⌘T 迁移：⌘T 触发新 session 流程，⌘N 不再触发；菜单 accelerator 断言。

e2e 清单（多窗场景）：

1. **localStorage 串扰**：同 origin 多窗共享 localStorage（nav-cache、侧栏展开态；按键含 workspace 分区）。验证多窗并发读写互不踩。
2. **restart 多窗迁移矩阵**：N 窗（landing/app 混合、含 SPA pushState 后的 live route）→ daemon restart → 逐窗 capture/land、route 降级 landing、无窗错乱。
3. **Blocked → 聚焦**：窗 1 活 session，窗 2 打开同 session → 聚焦窗 1，无错误弹窗、无重复 runtime。
4. **通知点击路由**：≥3 窗覆盖 §7 四类（无人显示目标、目标窗已关、同 session 多窗、同 workspace 多窗），点通知 → 按规则聚焦/导航/开新窗，其余窗不动。
5. **关非最后窗 = detach**：runtime 留活，重开接回（T7 首次多窗可观察）。
6. **关最后窗 = app 退出**（现状回归）。
7. **⌘N 新窗落 landing**；窗内进 workspace 后终身 app 窗（单向流回归）。
8. **taskNotifications「页面不可见才发」多窗判据**：任一窗可见即抑制（host 侧判定，预期无需改，e2e 确认）。
9. **同 workspace 双窗双 session**：窗 A session1、窗 B session2 并排，各自 turn 独立，互不串渲染。

P2 的测试计划随 §13.6 的任务卡在 plan 中增补（co-attach 授权矩阵、共享成员生命周期、对话框原子认领、follow-up 单执行权、加入/快照/gap/重连确定性测试、双 composer 并发、投影一致性）。

## 11. 资源估计（假设，待实测）

每窗一 WKWebView 约 150–300MB，每 session 一 pi 约 100–500MB；5 窗 5 session 粗估 1.5–4GB。宽通道下每窗可订阅全部 live runtime，事件量 × 窗数（follow-up 可改按需订阅）。以上均为估计值，实施期随 e2e 记录实测基线（对齐 daemon spec §3.5 的内存基线要求）。

## 12. 规模与顺序（两阶段）

| 阶段 | 步 | 内容 | 量级 |
| --- | --- | --- | --- |
| P1 | S1 | label 权威统一（§6.1）+ `open_workspace` 身份解析/复用/聚焦（§6.2-6.3）+ 单测 | 3–4 人日 |
| P1 | S2 | 三个入口（任意 workspace 均可开新窗）+ ⌘N/⌘T 迁移 + ledger/mirror 记账 | 2–3 人日 |
| P1 | S3 | 通知路由确定性规则（§7）+ 多窗 e2e（§10 清单） | 1–2 人日 |
| P2 | S4 | daemon：co-attach 授权矩阵（§13.1）+ 订阅身份稳定化（§13.2）+ 对话框原子认领（§13.3）+ 安全评审 | 3–4 人日 |
| P2 | S5 | 前端：chat 投影化（模块化）+ follow-up 单执行权 + 草稿方案落地（§13.5 拍板后） | 5–7 人日 |
| P2 | S6 | 双可写 e2e（§13.6 清单）+ 修尾 | 2–3 人日 |

P1 合计 6–9 人日，P2 合计 10–14 人日（2026-10-18 评审后重估：MW-4~MW-9 缺口补入 S4–S6，原 7–11 人日估计作废）。P2 的 gate：P1 e2e 矩阵关键行全绿（§10，例外逐项由 Dr. Lin 裁决）+ Dr. Lin 对 §13.1 授权矩阵与 §13.2 订阅身份变更过安全评审后放行。P1 先行只是实施顺序（Dr. Lin 已批两份都做），不是 P2 范围收缩。风险登记：`open_workspace` 链路生产未锻炼（暗路径补课是 S2 主体）、同 workspace 双 runtime 路径未锻炼、多窗事件扇出、内存估计值。均不阻塞 P1。

## 13. 双可写（P2）：设计评估与范围

### 13.1 daemon 侧：co-holder 完整契约与授权矩阵（MW-4）

先修正旧口径（§9 原文「daemon 侧已就绪」）：daemon 不是零改动；改动落在准入、应答授权与订阅身份三处（§13.2），事件扇出面不动。准入放宽前先把现有放行语义写全——现有 `authorize_target`（host_server.rs:2306-2344）是**四项合取**：①请求方为 Desktop 客户端且有 owner；②live runtime 存在且 wire 三元组（workspace/session/instance）与 live 记录全等；③`live.owner_id == 请求方 owner`；④registry 实时重读：请求方当前 workspace 绑定为 Registered 且 wid == live.workspace_id 且 generation == live.workspace_generation。P2 的 co-holder 分支必须完整保留这套语义，不得只挑其中两三样：

| 检查项 | owner 路径（原样保留） | co-holder 路径（新增） |
| --- | --- | --- |
| Desktop + owner 有值 | 保留 | 保留 |
| wire 三元组 == live 三元组 | 保留 | 保留 |
| owner 身份 | `live.owner_id == 请求方` | 请求方 ≠ live owner（live owner 走左列） |
| workspace 信任域 | 请求方 registry 绑定 wid == live.workspace_id | 同左：双方 registry 注册身份相等（见下） |
| generation | 请求方 generation == live.workspace_generation（per-owner 语义原样保留） | **不比较**——跨 owner 的 generation 无相等语义，比较即永假；以「请求方 registry 记录当前活跃」替代 |
| registry 每请求实时重读 | 保留 | 保留——旧页面延迟请求、转场取消后的请求在重读时失效，既有防线原样贯穿 |

**「workspace 信任域」精确定义**：runtime 的 workspace 绑定 = live 记录的 workspace_id；请求方的 workspace 绑定 = owner registry 中该 owner 的 `owner_current_workspace` 快照。co-holder 成立 ⇔ 两者为同一注册 workspace（以 registry 注册身份为准）且请求方记录活跃。不是目录字符串巧合相等，不是同 generation，更不是跨 workspace 放行。

**命令分级**：co-holder 分支放行 `RoutedAction::Runtime` 中的 turn 级与读操作——prompt/steer/abort、`runtime_snapshot_request`、对话框应答（经 §13.3 原子认领）。**身份变更类命令（`new_session`、session 切换类）保持 owner-only**：它们作废其他窗的 route 与订阅身份，单权威留给持有窗。授权矩阵以可判定表写入安全评审记录（S4 交付物）。逐项核验：

| 项 | 结论 | 依据与改动 |
| --- | --- | --- |
| 宽通道订阅（第二窗订阅同 session） | **匹配语义需改** | 订阅准入分离不变（§2），但订阅按完整 target 匹配会在 rebind 后断流——身份与授权归属分离，见 §13.2 |
| turn 流式输出双窗同时呈现 | **零改动** | 同上，事件广播面向全部订阅者；侧栏跨 workspace 活动点已证明扇出可用 |
| session 文件并发追加 | **零改动** | 单 runtime 单写者：两窗的写都经 RPC → 同一 pi 进程，pi 内部串行；host 不并发写 session 文件 |
| 写路由 + 快照准入 | **需改** | `RoutedAction::Runtime` 统一闸门要求 `live.owner_id == client.owner_id`（host_server.rs:2514、:2326-2328）：第二窗第二 owner 的 `runtime_request` 与 `runtime_snapshot_request` 全被拒 `unauthorized_target`。改法：authorize_target 增加 co-holder 分支——请求 owner 的 `owner_current_workspace` 命中 runtime 的 workspace 且 registry 活跃即放行（workspace 信任域语义） |
| 幂等/序列化 | **需改（一处）** | operation registry 按 scope+idempotencyKey 去重，per-owner 命名空间保留；但 follow-up drain 需要**跨 owner 单执行**——为这一类操作引入 session 级幂等命名空间（该 session 全部 holder 共享，见 §13.4）。事实修正（MW-7）：streaming 时 Enter 走 steer 不进队列，「mid-turn 提交即 queue」的说法不成立 |
| 跨 owner abort | **需改** | `abort_turn` 要求请求方 scope 匹配该实例最近一次 accepted operation（native_pi_manager.rs:899-905）：B 停不了 A 起的 turn。放宽为「任何 co-holder 可停」，产品语义 = 两窗都是一手客户端；**scope/实例匹配判定原样保留**——它保证停的是当前 turn、不误停下一 turn，删掉的只是「谁」的检查，不是「停哪个」的判定 |
| Blocked 分支 / 单可写 invariant | **改写，不是废除** | `plan_prepare_reuse` 的 `PrepareReuse::Blocked`（host_control_dispatch.rs:2127-2131）改为新变体 `CoAttach(target)`：不 rebind、不 spawn，第二 client 挂靠既有 runtime（挂靠不改变 runtime 的 owner；owner 改变只经 rebind）。invariant 从「每 session 一个可写 client」演进为「每 session N 个可写 client、全部在同一 workspace 信任域（上文定义）」；跨 workspace 同 session 结构上不存在，防线语义保留。owner 模型本身不共用：两窗仍是两个 owner（共享 owner 会破坏 owner↔label 1:1、close 流与 generation 追踪，否决） |
| 阻塞对话框路由 | **需改** | 投递面：对话框事件只发给过 authorize 的订阅者（host_server.rs:2058-2061），co-holder 放行后两窗都收。应答面：现实现无原子认领（§2），先答先得不能靠 `duplicate_completed` 回复形状假装成立——完整方案见 §13.3 |
| generation 模型 | **需适配** | `workspace_generation` 是 per-owner 记录（window_owner.rs:382-397）：B 的 co-attach 不得 bump A 的；co-holder 路径不比较 generation（见上表），owner 路径语义不变。此处是安全敏感点，S4 评审主体 |

### 13.2 共享成员生命周期与订阅身份（MW-5）

事实：`RuntimeTarget` 的 Hash/Eq 含 owner_id 与 workspace_generation（runtime_coordinator.rs:7-20）；订阅保存完整 target，事件按 `subscriptions.contains(&event.target)` 匹配（host_server.rs:1948、:2050）。rebind（旧 owner detach 后新 owner 接管，host_control_dispatch.rs:2127-2133）改变 target 的 owner/generation → 按当前匹配规则，事件带新身份，其他窗的旧订阅全部失配断流。「事件面零改动」的旧结论不成立。

修订：**订阅身份与授权归属分离**。订阅集合的键从完整 target 改为稳定运行时身份三元组（workspace_id, session_id, instance_id）——runtime 的进程身份在 rebind 前后不变，这是事件流真正标识的东西；owner/generation 是授权归属，由事件投递时的既有实时检查承担（对话框事件的 `authorize_target` 门照旧），不进匹配键。重绑时订阅无需迁移：B 窗订阅在 C rebind 后自然继续命中。

生命周期场景与规则（S4 自动化用例）：

- **A/B 共看（A owner、B co-holder）→ A 关窗（detach，runtime 留活，owner 变 reattachable）→ C 打开该 session（prepare 命中 Rebind，C 成 owner）→ B、C 继续收发**：B 靠稳定身份不断流，C 走 owner 路径。不能只测「A 关后 B 当场可写」。
- 全部持有者离开：runtime reattachable，对话框无 authorize 通过者即不再投递；再打开走 Rebind。
- CoAttach 第二窗关窗：只删自己的订阅，其余窗不受影响。

### 13.3 对话框原子认领（MW-6）

现状：`respond_extension_ui` 先 `send_frame` 后从 pending 集合删除（native_pi_manager.rs:1828-1870），两窗并发应答会双双转发；host 侧 `duplicate_completed`（host_server.rs:2681-2692）只是回复形状，不是去重。修订：

1. **认领先于转发**：以 (instance_id, dialogId) 在 pending 集合内原子认领（现有锁内先移除再转发）；认领失败即已应答。
2. **保存已决结果**：认领成功者的应答记录在案（随实例生命周期存续）；重复应答返回明确结果（已应答 + 应答摘要），不再以 duplicate 形状假装幂等。
3. **向其他窗发布结算事件**：对话框结算后广播通知，让还显示着该对话框的窗收起 UI。P2 双窗都收对话框后这是必须项，不是优化。
4. **发送失败回滚认领**：`send_frame` 失败把 dialog 放回 pending，允许重试，不做「认领即吃掉」。
5. 测试：并发屏障下只转发一次；重复应答得明确结果；发送失败可重试；跨 owner abort 保留「停当前 turn 不误停下一 turn」判定（§13.1）。

### 13.4 前端侧：投影化、follow-up 单执行权、交接顺序（MW-7/9/10）

现状 app.js（8448 行）是本地状态机：optimistic 气泡等 agent_start 认领、turn 计时与 settle 兜底、只信自己视角的事件。投影化的三块地基已在：**订阅+快照重建**（重连路径，见下）、**磁盘历史读取**（`readSessionMessages` 数据面）、**事件驱动渲染**（后台 turn 的 user echo 已能渲染非本窗发起的消息）。

分层方案（S5 的改造边界）：

1. **必须投影**（两窗收敛到同一事实）：消息 transcript（快照 + 事件重放收敛）、turn 生命周期（agent_start/end → composer 状态、计时）、queue/steer 可见状态、settle 兜底（missed agent_end 的收敛）。
2. **保持本地**（各窗私有）：draft（按 §13.5 拍板方案）、滚动位置、输入焦点与选择、本窗 delivery records（C3 的 awaiting→accepted/rejected/unconfirmed 只描述本窗自己的 RPC 回执）。optimistic 气泡只认本窗发出的消息——他窗的 user 消息经事件到达，直接渲染，不做 optimistic。
3. **同步机制与交接顺序（MW-9 修正）**：实际序列是 hello_ack → **先订阅后快照**（websocket-client.js:713-719），不是旧稿的「读盘 → 快照 → 订阅」。订阅先行意味着快照到达前事件已开始流入，客户端缓冲早到事件；快照携带 sequence 水位（既有 `sequenceGapRecoveryPending` 机制）；快照落地后 seq ≤ 水位的事件去重丢弃、> 水位依序应用；检测到 gap 触发重快照；instance 更换（rebind）使水位失效 → 重订阅 + 重快照。磁盘历史与快照的竞态按窗隔离（既有 sessionDebug 机制）。改造点是把「只有我在操作」的状态机假设清出来（认领逻辑 per-window 化）。
4. **follow-up 队列真源与单执行权（MW-7）**：事实见 §2（streaming Enter 走 steer；队列是 Picot 自有 localStorage + 每窗缓存；每窗 `agent_settled` 各自 drain，防重标志仅本窗——双窗会重复执行同一项）。修订：**执行围栏 = daemon 幂等**——drain 发送带 `idempotencyKey = "followup:{sessionFile}:{itemId}"`，S4 为这一类操作引入 session 级幂等命名空间（§13.1）；两窗同时 drain，先到者执行、后到者收 duplicate 结果即静默收场。任何窗可 drain，执行窗关闭不产生接管问题——这是选幂等围栏而非「owner 窗独占 drain」的理由：后者在 owner 窗关闭且无人 rebind 时队列停摆，还要另造接管机制。跨窗编辑/删除：存储本就共享（localStorage），每窗缓存靠 `storage` 事件失效，drain 前同步重读存储再 shift，不用陈旧缓存。双实例自动化验证一项只执行一次（S6）。
5. **双 composer 并发提交**：A 窗 mid-turn 时 B 窗提交 → 走 pi 既有队列语义（指 pi 进程内队列，与 Picot follow-up 队列是两回事，文案注意区分），B 窗的「排队中」状态从事件流推导；不引入跨窗拒绝。前端唯一新交互文案：队列状态在两窗都可见。

**模块纪律（MW-10）**：投影化新逻辑不进 app.js（orchestrator，AGENTS.md 约定约 50 行以上职责独立成模块）——新增独立模块承载投影状态机，follow-up 改造落在 `ui/follow-up-queue.js`，app.js 只接线。

风险：turn settle 兜底逻辑在双窗下竞态放大（两窗各自兜底同一 turn）；磁盘 fetch 与快照的竞态需按窗隔离；工作量主体在把 8.4k 行状态机里的隐式「单写者假设」逐个显式化——这是 S5 估 5–7 人日的原因。

### 13.5 draft 语义：待 Dr. Lin 拍板（MW-8）

旧稿矛盾修正：§13.4（旧 §13.2）称 draft 各窗私有，旧 §13.3 又选共享 session 键 + last-write-wins，两处矛盾；且共享方案会丢输入——A 发送并 clear 会删掉 B 的未提交输入（save/clear 写同一 DB 键，composer-draft-store.js:98-124）。「沿用 loadAll 守卫」不成立：loadAll 对已缓存 identity 一律跳过（:45-56），是跳过、不是跨窗版本协调。

**硬底线（无论选哪案）**：不得覆盖、删除本窗未提交输入。

两案待 Dr. Lin 拍板：

- **案 A 共享活动草稿**：session 键共享，窗口聚焦时重取（仅当本窗无未提交修改）；残余竞态仍在，靠 dirty 检测做对才不丢字。
- **案 B 独立活动草稿 + 仅跨重开恢复（架构师推荐）**：活动期各窗输入私有互不干扰（底线结构性满足）；「重开 session 草稿还在」由重开恢复路径保留（开窗时该 session 无活动输入才回填共享存底）；发送 clear 只清存底，不碰其他窗活动 buffer。推荐理由：案 A 的正确性依赖 dirty 检测处处做对，案 B 不共享活动状态，失败模式只剩「存底比活动旧」——可见、可恢复、不丢数据。

### 13.6 P2 交付与验收要点

- daemon：CoAttach 分支 + 授权矩阵（含命令分级）+ 订阅身份稳定化 + 对话框原子认领 + follow-up session 级幂等；安全评审记录归档（矩阵为评审主体）。
- 前端：投影化（模块化）+ follow-up 单执行权 + 草稿方案落地（§13.5 拍板后）。
- 自动化：§13.2 生命周期用例（A/B 共看→A 关→C 加入→B/C 继续收发）、对话框并发只转发一次、follow-up 双实例只执行一次、加入期间持续输出/快照延迟/gap/重连四类确定性测试（替代「30 分钟人工目检」——人工看 transcript 一致证明不了无丢帧/无重放）。
- e2e：双窗双 composer 并发（含 mid-turn steer 与 settle 后 follow-up drain）、A 起 turn B 停（只停当前 turn）、B 中途加入追齐历史、A 关窗后 B 继续可写、草稿按拍板方案的行为验证。

## 14. 待批事项与 Open question

- **draft 方案（MW-8，待 Dr. Lin 拍板）**：§13.5 两案，硬底线 = 不覆盖本窗未提交输入；架构师推荐案 B（独立活动草稿 + 仅跨重开恢复）。
- **切换 workspace 全屏刷新是否独立立项修复**（待 Dr. Lin 定）：与多窗口并列的独立需求，立项与否不改变本 spec 范围与优先级。

（原「若根治则同 workspace 多 session 并排优先级下降」的推论已删——无需求依据：减少切换成本与同屏对照是两种需求，MW-10。）

## 15. 军师评审处置表（2026-10-18）

| # | 军师意见 | 处置 | 落点 | 理由 |
| --- | --- | --- | --- | --- |
| MW-1 | `open_workspace` 不走复用分支，T1 假定不成立 | 采纳 | §6.2、plan T1 | 代码核实：随机 session ID 直接 spawn。身份解析 → `plan_prepare_reuse` 三分支；原子保留落在 coordinator 唯一性（spawn 撞身份即失败重查），不引入预约锁 |
| MW-2 | label 无唯一权威；focus 失败回退违背单 runtime 约束 | 采纳 | §6.1/§6.3、plan T1 | workspace 窗唯一创建路径是 daemon `open_workspace`，daemon 当权威链路最短；「focus 失败 → 正常开窗」废除，改为重核持有者——回退开窗会绕过活持有者，直接破坏单可写绑定 |
| MW-3 | 通知路由缺无人显示等分支 | 采纳，规则见 §7 | §7、plan T3 | 点击通知 = 明确查看意图，禁止无操作；不导航其他 workspace 的窗；开新窗是最后手段且受 §6.2 复用链保护 |
| MW-4 | co-holder 契约不完整 | 采纳 | §13.1 | 放行公式按四项合取完整保留；信任域 = registry 双侧注册身份相等；身份变更命令 owner-only（作废他窗 route 的操作留单权威） |
| MW-5 | 订阅身份随 rebind 漂移断流 | 采纳 | §13.2 | 订阅键改稳定三元组，授权归属留在投递时实时检查；A/B→A 关→C 加入→B/C 继续收发入自动化 |
| MW-6 | 对话框应答非原子 | 采纳 | §13.3 | 认领先于转发 + 已决结果 + 结算广播 + 失败回滚；`duplicate_completed` 不再当幂等凭据 |
| MW-7 | 队列真源与单执行权 | 采纳 | §13.4(4) | 选 session 级幂等围栏而非 owner 独占 drain：owner 窗关闭无人 rebind 时后者停摆还要造接管机制，前者结构上无此缺口 |
| MW-8 | 草稿方案矛盾且丢输入 | 采纳，列 Dr. Lin 待批 | §13.5 | 硬底线先立；两案并列，推荐案 B，理由见 §13.5 |
| MW-9 | 交接顺序写反、验收不足 | 采纳 | §13.4(3) | 以实际先订阅后快照为准，水位/去重/gap/instance 失效规则成文；四类确定性测试替代 30 分钟人工目检 |
| MW-10 | 验收矛盾 / T5 模块纪律 / P2 工期 / 两线串行 | 采纳 | §8、§12、§13.4（模块纪律）、plan | 关键矩阵必须过、例外逐项裁决；新逻辑独立成模块；P2 重估 10–14 人日；多窗口 P1 → relay 串行入 §8 |
| 附 | 删「全屏刷新修复降低并排优先级」推论 | 采纳 | §1、§14 | 无需求依据，两种需求并存 |
