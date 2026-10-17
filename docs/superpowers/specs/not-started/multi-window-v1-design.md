# 多窗口 v1 设计

**状态：** Draft 待评审。讨论稿的七个决策点（D1–D5 及两项补充）已由 Dr. Lin 于 2026-10-17 拍板；2026-10-18 需求变更（D8/D9，§3）将双可写纳入范围并推翻「一 workspace 一窗」，本文按两阶段结构修订；实施范围以评审确认为准。
**日期：** 2026-10-17
**演化关系：** daemon phase1（`2026-10-15-picot-daemon-phase1.md` plan，T1–T13 已交付）之上的形态增量。本文服务本机场景（同一 daemon、无 relay）：与 session-resident-views Phase C 共享同一批底层能力，relay 场景互不依赖、准入放宽结论可被其复用（§8）；窗内 tab（Phase B，09-22 已拍板不做）不复活。
**参照：** 多窗口可行性讨论稿（2026-10-16，subagent 输出）；`2026-09-20-persistent-daemon-relay-design.md` §3.2.1（单可写绑定）；`2026-09-20-session-resident-views-design.md` Phase C；`2026-09-18-cross-workspace-runtime-lifecycle-divergence.md` §3。

## 1. 问题

daemon phase1 之后，runtime 的宿主是 daemon，窗口退化为客户端。但产品仍是单窗口。多窗口的真实动机（D8）：Picot 无 tab，左侧 session 导航即 vertical tab，同屏只能看一个 session；要对照两个 session 只能来回切换 workspace/session，而**切换 workspace 至今仍有全屏刷新问题**（性能痛点）。多窗口是止痛：不同窗开不同 workspace 的 session，或**同 workspace 的不同 session**。多窗天然也能打开同一 session，「两窗同看同发、消息互相同步」的双可写由此进入范围（D9，第二阶段）。

讨论稿的结论是多窗口与 daemon 架构同构，地基约七成已在。daemon 侧的堵点收敛在「准入」一处（§13.1），真正的大头在前端 chat 视图从本地状态机到 runtime 状态投影的改造（§13.2）。本 spec 定义 v1 的范围、入口、边界与两阶段切分。

**Open question（待 Dr. Lin 定）：** 切换 workspace 的全屏刷新是否独立立项修复——多窗口是止痛不是根治；若根治，部分多窗诉求（同 workspace 不同 session 并排）的优先级会下降。

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

## 6. label 与聚焦规则（S1，硬前置）

label 规则：

| 窗型 | label | 说明 |
| --- | --- | --- |
| 首个 landing 窗 | `native-landing` | 保持硬编码值（main.rs:405），window-state 几何延续 |
| 后续 landing 窗 | `native-landing-2`、`-3`… | 序号取第一个空闲值 |
| workspace 窗 | 首窗 `native-workspace-{workspace_id}`，后续 `-2`、`-3`… | **一 workspace 可多窗**（D8：同 workspace 不同 session 并排）；序号取第一个空闲值 |

聚焦规则（不做这个，同 session 入口一按就报错）：

- **同 workspace 二开（不同 session）→ 正常开新窗。** 原「一 workspace 一窗」invariant 废除（D8）：label 多实例化后 owner mint 不再撞 label（现状报错点 window_owner.rs:275）。每个新窗 = 新 owner = 自己的 runtime（`plan_prepare_reuse` 按 session 匹配，host_control_dispatch.rs:2103-2120，目标 session 不在运行即 Spawn），同 workspace 多 session 各自单活 runtime，互不干扰。restart 迁移本就按 label 逐窗（main.rs:2465/2517），无需改。
- **同 session 二开 → P1 聚焦持有窗（D4）。** `openWorkspace` 带 `sessionPath`（或窗内导航）命中正在运行且被其他活 owner 持有的 session → `PrepareReuse::Blocked`（host_control_dispatch.rs:1390-1393、:2127-2131）。不再以错误弹窗收场：由 Blocked 分支携带持有者 owner，解析其窗口 label → 经 app-surface 新增 `focus_window` op 聚焦，控制调用返回机器可读结果（如 `focusedExisting: true`），前端 toast「已在其他窗口打开」。同 workspace 两窗并存后，「B 窗导航到 A 窗正持有的 session」同样落进此分支；P2 起改写为 co-attach（§13）。
- `focus_window` 的 label 解析复用 close 流的 owner↔label 镜像（tauri_app_surface.rs:44-50 同款机制），不新建通道。

## 7. 通知点击路由

现状：daemon 单点广播 `notification_activated` 到全部客户端，每窗 listener「本窗非该 session 即导航」（app.js:2840-2851）——多窗下会多窗齐动。同 workspace 多窗后，按 `workspaceId` 过滤也不够（同 workspace 两窗都会动）。v1 改造：

- 前端：payload 已含 `workspaceId`/`sessionFile`（host_server.rs:3487-3496），listener 过滤改按**本窗前台 session**判定——非该 session 的窗忽略该帧（session 粒度，覆盖同 workspace 多窗）。
- app 侧：通知激活时聚焦绑定该 session 的窗（复用 §6 的 `focus_window`），再由该窗完成导航。
- 通知本体仍由 host 单点发出，无双发问题。

## 8. 与既有 spec 的关系

- **session-resident-views Phase C：场景不同、互不依赖。** Phase C 的原始场景是 relay——有了 relay 之后，一个 Picot 经 relay 加入另一个 Picot 的 host，两个窗口显示同一 session 的实时记录，是跨实例共享，不是本机多窗口的上位概念。本机多窗口（v1）是同一 daemon 内的增量，不依赖 relay，也不预支 Phase C；每窗一个前台 session、单 target 订阅（`runtime_subscribe`），全部用现有机制。两者共享同一批底层能力：宽通道事件订阅（09-20 拍板）与单可写绑定 invariant。Phase C 的 relay 场景范围不变，仍待 relay 落地后按原 spec 要求单独立项并过授权生命周期评审。该 spec Phase C 节已按此口径补注（本批修订）。
- **单可写绑定 invariant（daemon-relay §3.2.1）：P1 的安全底座，P2 将其改写而非废除。** P1 维持「每 session 一个可写 client」（D4 聚焦是它的产品化出口）。P2 把它改写为「每 session N 个可写 client，全部在同一 workspace 信任域内」（§13.1）——这属于授权生命周期变更，与 session-resident-views Phase C 登记的评审要求同源，P2 开工前单独过安全评审。
- **cross-workspace divergence §3「单窗口架构保留」**：已补历史语境（本批修订）——该句约束 09-18 恢复工程的范围，不构成多窗口增量禁令。
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
- Blocked 分支：聚焦持有窗，无错误串抛给前端。
- ⌘T 迁移：⌘T 触发新 session 流程，⌘N 不再触发；菜单 accelerator 断言。

e2e 清单（多窗场景）：

1. **localStorage 串扰**：同 origin 多窗共享 localStorage（nav-cache、侧栏展开态；按键含 workspace 分区）。验证多窗并发读写互不踩。
2. **restart 多窗迁移矩阵**：N 窗（landing/app 混合、含 SPA pushState 后的 live route）→ daemon restart → 逐窗 capture/land、route 降级 landing、无窗错乱。
3. **Blocked → 聚焦**：窗 1 活 session，窗 2 打开同 session → 聚焦窗 1，无错误弹窗、无重复 runtime。
4. **通知点击路由**：≥3 窗，点通知 → 绑定窗聚焦并导航，其余窗不动。
5. **关非最后窗 = detach**：runtime 留活，重开接回（T7 首次多窗可观察）。
6. **关最后窗 = app 退出**（现状回归）。
7. **⌘N 新窗落 landing**；窗内进 workspace 后终身 app 窗（单向流回归）。
8. **taskNotifications「页面不可见才发」多窗判据**：任一窗可见即抑制（host 侧判定，预期无需改，e2e 确认）。
9. **同 workspace 双窗双 session**：窗 A session1、窗 B session2 并排，各自 turn 独立，互不串渲染。

P2 的测试计划随 §13.4 的任务卡在 plan 中增补（co-attach 准入、对话框先答先得、跨 owner abort、双 composer 并发、投影一致性）。

## 11. 资源估计（假设，待实测）

每窗一 WKWebView 约 150–300MB，每 session 一 pi 约 100–500MB；5 窗 5 session 粗估 1.5–4GB。宽通道下每窗可订阅全部 live runtime，事件量 × 窗数（follow-up 可改按需订阅）。以上均为估计值，实施期随 e2e 记录实测基线（对齐 daemon spec §3.5 的内存基线要求）。

## 12. 规模与顺序（两阶段）

| 阶段 | 步 | 内容 | 量级 |
| --- | --- | --- | --- |
| P1 | S1 | workspace/landing label 多实例化 + 同 session Blocked → `focus_window` 聚焦 + 单测 | 2–3 人日 |
| P1 | S2 | 三个入口（任意 workspace 均可开新窗）+ ⌘N/⌘T 迁移 + ledger/mirror 记账 | 2–3 人日 |
| P1 | S3 | 通知点击路由（session 粒度）+ 多窗 e2e（§10 清单） | 1–2 人日 |
| P2 | S4 | daemon：co-attach 准入放宽（authorize_target/对话框应答/abort 跨 owner）+ 安全评审 | 2–3 人日 |
| P2 | S5 | 前端：chat 视图投影化 + 双 composer 并发语义 + draft 语义落地 | 4–6 人日 |
| P2 | S6 | 双可写 e2e（§13.4 清单）+ 修尾 | 1–2 人日 |

P1 合计 5–8 人日，P2 合计 7–11 人日。P2 的 gate：P1 e2e 矩阵全绿 + Dr. Lin 对 §13.1 准入放宽方案过安全评审后放行。风险登记：`open_workspace` 链路生产未锻炼（暗路径补课是 S2 主体）、同 workspace 双 runtime 路径未锻炼、多窗事件扇出、内存估计值。均不阻塞 P1。

## 13. 双可写（P2）：设计评估与范围

### 13.1 daemon 侧：改动收敛在「准入」，不是零改动

先修正旧口径（§9 原文「daemon 侧已就绪」）：**daemon 不是零改动**，但改动全部落在准入与应答授权一处，事件面零改动。逐项核验：

| 项 | 结论 | 依据与改动 |
| --- | --- | --- |
| 宽通道订阅（第二窗订阅同 session） | **零改动** | 订阅与命令准入分离（§2）：任意 desktop owner 可订阅任意 live target，事件逐连接扇出（host_server.rs:1947-1957、:2048-2066），cross-owner 订阅已有 admit 测试（:7599-7626） |
| turn 流式输出双窗同时呈现 | **零改动** | 同上，事件广播面向全部订阅者；侧栏跨 workspace 活动点已证明扇出可用 |
| session 文件并发追加 | **零改动** | 单 runtime 单写者：两窗的写都经 RPC → 同一 pi 进程，pi 内部串行；host 不并发写 session 文件 |
| 写路由 + 快照准入 | **需改** | `RoutedAction::Runtime` 统一闸门要求 `live.owner_id == client.owner_id`（host_server.rs:2514、:2326-2328）：第二窗第二 owner 的 `runtime_request` 与 `runtime_snapshot_request` 全被拒 `unauthorized_target`。改法：authorize_target 增加 co-holder 分支——请求 owner 的 `owner_current_workspace` 命中 runtime 的 workspace 且 registry 活跃即放行（workspace 信任域语义） |
| 幂等/序列化 | **语义确认，无结构改** | operation registry 按 scope+idempotencyKey 去重；两 owner = 两个 scope 命名空间，各自幂等独立（合理，无需合并）；pi 进程内天然串行，mid-turn 提交即 queue（单窗既有语义直接复用） |
| 跨 owner abort | **需改** | `abort_turn` 要求请求方 scope 匹配该实例最近一次 accepted operation（native_pi_manager.rs:899-905）：B 停不了 A 起的 turn。放宽为「任何 co-holder 可停」，产品语义 = 两窗都是一手客户端 |
| Blocked 分支 / 单可写 invariant | **改写，不是废除** | `plan_prepare_reuse` 的 `PrepareReuse::Blocked`（host_control_dispatch.rs:2127-2131）改为新变体 `CoAttach(target)`：不 rebind、不 spawn，第二 client 挂靠既有 runtime。invariant 从「每 session 一个可写 client」演进为「每 session N 个可写 client、全部在同 workspace 信任域」；跨 workspace 同 session 结构上不存在，防线语义保留。owner 模型本身不共用：两窗仍是两个 owner（共享 owner 会破坏 owner↔label 1:1、close 流与 generation 追踪，否决） |
| 阻塞对话框路由 | **半自动生效 + 一处改** | 投递面：对话框事件只发给过 authorize 的订阅者（host_server.rs:2058-2061），co-holder 放行后**两窗都收**，无需额外改。应答面：`dialog_response_allowed` 只认 live owner（:2662-2675）→ 放宽同 co-holder。语义定为**先答先得**：第二个应答者收到「已应答」，daemon 返回 duplicate 形状、前端按成功收场（竞态窗口极小） |
| generation 模型 | **需适配** | `workspace_generation` 是 per-owner 记录（window_owner.rs:382-397）：B 的 co-attach 不得 bump A 的；authorize 的 co-holder 分支按 workspace 身份 + registry 活跃度比对，而非比 B 的 generation 与 runtime 的 generation。此处是安全敏感点，S4 评审的主体 |

### 13.2 前端侧：真正的大头——chat 视图从本地状态机到 runtime 状态投影

现状 app.js（8448 行）是本地状态机：optimistic 气泡等 agent_start 认领、turn 计时与 settle 兜底、只信自己视角的事件。投影化的三块地基已在：**快照+事件重建**（重连路径：resubscribe + `runtime_snapshot_request`，§2）、**磁盘历史读取**（`readSessionMessages` 数据面）、**事件驱动渲染**（后台 turn 的 user echo 已能渲染非本窗发起的消息）。

分层方案（S5 的改造边界）：

1. **必须投影**（两窗收敛到同一事实）：消息 transcript（快照 + 事件重放收敛）、turn 生命周期（agent_start/end → composer 状态、计时）、queue/steer 可见状态、settle 兜底（missed agent_end 的收敛）。
2. **保持本地**（各窗私有）：draft、滚动位置、输入焦点与选择、本窗 delivery records（C3 的 awaiting→accepted/rejected/unconfirmed 只描述本窗自己的 RPC 回执）。optimistic 气泡只认本窗发出的消息——他窗的 user 消息经事件到达，直接渲染，不做 optimistic。
3. **同步机制**：不新建「快照+增量」协议——沿用重连同款「读盘历史 → 快照 → 订阅事件流」冷启动序列，第二窗中途加入与重连是同一条路径。改造点是把「只有我在操作」的状态机假设清出来（认领逻辑 per-window 化）。
4. **双 composer 并发提交**：A 窗 mid-turn 时 B 窗提交 → 走 pi 既有 queue 语义（与单窗 mid-turn 排队一致），B 窗的「排队中」状态从事件流推导；不引入跨窗拒绝。前端唯一新交互文案：queue 状态在两窗都可见。

风险：turn settle 兜底逻辑在双窗下竞态放大（两窗各自兜底同一 turn）；磁盘 fetch 与快照的竞态（sessionDebug 机制）需按窗隔离；工作量主体在把 8.4k 行状态机里的隐式「单写者假设」逐个显式化——这是 S5 估 4–6 人日的原因。

### 13.3 draft 语义：保持 session 键共享 + 聚焦重取，不做跨窗同步

现状 draft 按 session 身份键、DB 持久化、每窗内存写穿缓存（§2）：天然跨窗共享（经 DB）但无活性传播，双窗并发编辑存在丢更新（debounce 落库互相覆盖）。结论：**「各窗独立草稿」不是合理默认**——按 (session, window) 双键会破坏「重开 session 草稿还在」的既有体验。取默认：保持 session 键共享 + **窗口聚焦时从 DB 重取一次**（消除大部分陈旧窗口），接受 last-write-wins 的残余竞态；live 跨窗同步不做。若实测仍踩丢更新，再议 window 维度键控。

### 13.4 P2 交付与验收要点

- daemon：CoAttach 分支 + authorize co-holder 放行 + 对话框先答先得 + abort 放开；安全评审记录归档。
- 前端：投影化改造完成，双窗同 session 并发使用 30 分钟无渲染分叉（transcript 逐条一致）。
- e2e：双窗双 composer 并发（含 mid-turn queue）、对话框双弹先答先得、A 起 turn B 停、B 中途加入追齐历史、A 关窗（detach）后 B 继续可写、draft 聚焦重取。

## 14. Open question

- **切换 workspace 全屏刷新是否独立立项修复**（待 Dr. Lin 定）：多窗口是止痛不是根治；若决定根治，§1 动机中的「同 workspace 不同 session 并排」诉求优先级下降，P1 的 workspace 多窗部分可相应减配，但 label 多实例化与聚焦规则不受影响。
