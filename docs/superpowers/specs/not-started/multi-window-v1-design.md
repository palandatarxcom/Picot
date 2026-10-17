# 多窗口 v1 设计

**状态：** Draft 待评审。讨论稿的七个决策点（D1–D5 及两项补充）已由 Dr. Lin 于 2026-10-17 拍板，本文按拍板结果成文；实施范围以评审确认为准。
**日期：** 2026-10-17
**演化关系：** daemon phase1（`2026-10-15-picot-daemon-phase1.md` plan，T1–T13 已交付）之上的形态增量。本文服务本机场景（同一 daemon、无 relay）：与 session-resident-views Phase C 共享同一批底层能力，但场景不同、互不依赖（§8）；窗内 tab（Phase B，09-22 已拍板不做）不复活。
**参照：** 多窗口可行性讨论稿（2026-10-16，subagent 输出）；`2026-09-20-persistent-daemon-relay-design.md` §3.2.1（单可写绑定）；`2026-09-20-session-resident-views-design.md` Phase C；`2026-09-18-cross-workspace-runtime-lifecycle-divergence.md` §3。

## 1. 问题

daemon phase1 之后，runtime 的宿主是 daemon，窗口退化为客户端。但产品仍是单窗口：两个 session 并排对照、两个 workspace 同开，都做不到。讨论稿的结论是多窗口与 daemon 架构同构，地基约七成已在；真正被既有 spec 锁死的是「同 session 双可写视图」，不是多窗口本身。本 spec 定义 v1 的范围、入口与边界。

## 2. 已验证事实（2026-10-16/17，本 worktree 核对）

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

## 3. 决策记录（Dr. Lin，2026-10-17）

| # | 决策 | 内容 |
| --- | --- | --- |
| D1 | 多窗口的窗口承担 **window 角色**（选 A） | 关窗 = detach 保留 runtime；杀 runtime 走既有「停止运行」入口。心智模型（原话转述）：Picot 无 tab 概念，左侧 session 导航 = vertical tab；「关 tab」语义由窗内承载（停 runtime → 回 landing）；主聊天视图无关闭按钮 |
| D2 | 关最后一扇窗 = app 退出（现状不动） | hide-on-close / 无窗常驻形态不进 v1，另立议题（daemon spec §3.2 已挂「待另行拍板」） |
| D3 | 「移到新窗口」v1 砍掉 | 单可写绑定下「移到」= 原窗必须降级 landing，语义链复杂，等真实需求 |
| D4 | 同 session 二开撞 Blocked → 自动聚焦既有窗 | 只读第二窗与本机外的共享场景（Phase C relay、双可写视图）均另行立项，v1 不含（§9） |
| D5 | 并排形态走多窗口 | 窗内 tab = Phase B 复活，不做；两条路不互斥，未来 tab 若复活是容器内层 |
| D6 | 新窗口入口 = File 菜单 New Window + **⌘N**；快捷键重分配：⌘N 改绑 New Window（macOS 惯例），New Session 迁 **⌘T**（与 vertical tab 心智对齐） | 迁移涉及三处：菜单 accelerator（main.rs:2826-2829）、菜单事件合成 keydown（main.rs:2973-2997）、前端处理器（app.js:5298） |
| D7 | 概念锚点（原话）：**「一个新的 Picot 窗口，就像一个移动端客户端访问 daemon。」** | 见 §4.1。多窗口对现有窗内操作零影响（§4.4 硬约束） |

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
3. **侧栏 session「在新窗口打开」**：session 属于其他 workspace 且该 workspace 无窗 → 为它开新窗（`openWorkspace` 带 `sessionPath`）；同 workspace 或该 session 已在别窗活跃 → 走 §6 的聚焦规则。

⌘N/⌘T 迁移（D6）：菜单项 New Session 的 accelerator 从 `CmdOrCtrl+N` 改 `CmdOrCtrl+T`（main.rs:2826-2829），菜单事件合成 keydown 的 key 从 `'n'` 改 `'t'`（main.rs:2973-2997），前端处理器同步改绑 ⌘T（app.js:5298）；新增菜单项 New Window 绑 ⌘N，处理器在 Rust 侧开 landing 窗，不再向聚焦窗 WebView 合成事件。菜单文案与快捷键提示同步更新。

## 6. label 与聚焦规则（S1，硬前置）

label 规则：

| 窗型 | label | 说明 |
| --- | --- | --- |
| 首个 landing 窗 | `native-landing` | 保持硬编码值（main.rs:405），window-state 几何延续 |
| 后续 landing 窗 | `native-landing-2`、`-3`… | 序号取第一个空闲值 |
| workspace 窗 | `native-workspace-{workspace_id}` | 一 workspace 一窗；二开不再报错，见下 |

聚焦规则（不做这个，任何入口一按就报错）：

- **同 workspace 二开 → 聚焦既有窗。** 现状两条失败路径：owner mint 撞 label（window_owner.rs:275）直接报错；或开窗失败连带 stop 刚 spawn 的 runtime（host_control_dispatch.rs:309-310）。改造为 daemon 侧预检（owner registry + mirror 的 label↔owner 记录）：目标 workspace 已有活窗 → 不 mint、不 spawn，经 app-surface 新增 `focus_window` op 聚焦既有窗，控制调用返回机器可读结果（如 `focusedExisting: true`），前端 toast「已在其他窗口打开」。
- **Blocked → 聚焦持有窗（D4）。** `openWorkspace` 带 `sessionPath` 且该 session 被其他活 owner 持有 → `PrepareReuse::Blocked`（host_control_dispatch.rs:1390-1393）。不再以错误弹窗收场：由 Blocked 分支携带持有者 owner，解析其窗口 label → `focus_window` 聚焦，返回同上机器可读结果。
- `focus_window` 的 label 解析复用 close 流的 owner↔label 镜像（tauri_app_surface.rs:44-50 同款机制），不新建通道。

## 7. 通知点击路由

现状：daemon 单点广播 `notification_activated` 到全部客户端，每窗 listener「本窗非该 session 即导航」（app.js:2840-2851）——多窗下会多窗齐动。v1 改造：

- 前端：payload 已含 `workspaceId`（host_server.rs:3487-3496），listener 增加本窗 workspace 绑定过滤——非本 workspace 的窗忽略该帧。
- app 侧：通知激活时聚焦绑定该 session 的窗（复用 §6 的 `focus_window`），再由该窗完成导航。
- 通知本体仍由 host 单点发出，无双发问题。

## 8. 与既有 spec 的关系

- **session-resident-views Phase C：场景不同、互不依赖。** Phase C 的原始场景是 relay——有了 relay 之后，一个 Picot 经 relay 加入另一个 Picot 的 host，两个窗口显示同一 session 的实时记录，是跨实例共享，不是本机多窗口的上位概念。本机多窗口（v1）是同一 daemon 内的增量，不依赖 relay，也不预支 Phase C；每窗一个前台 session、单 target 订阅（`runtime_subscribe`），全部用现有机制。两者共享同一批底层能力：宽通道事件订阅（09-20 拍板）与单可写绑定 invariant。Phase C 的 relay 场景范围不变，仍待 relay 落地后按原 spec 要求单独立项并过授权生命周期评审。该 spec Phase C 节已按此口径补注（本批修订）。
- **单可写绑定 invariant（daemon-relay §3.2.1）：多窗的安全底座，本 spec 不放宽。** 它本来就是「多个 client 同时打开同一 session」的防线；桌面多窗口只是让这条既有防线第一次在本机被日常触发（D4 的聚焦行为就是它的产品化出口）。
- **cross-workspace divergence §3「单窗口架构保留」**：已补历史语境（本批修订）——该句约束 09-18 恢复工程的范围，不构成多窗口增量禁令。
- **ARCHITECTURE.md**：v1 实施时补窗口模型事实（A2 open_workspace 从暗路径转正、多窗 label/聚焦规则、通知路由）。本批不动。

## 9. 非目标

- 「移到新窗口」（D3）。
- 同 session 只读第二窗（本机）。
- **双可写视图**（两窗同 session 均可输入发送，Slack 式）。新识别的架构问题，与 Phase C 的「实时记录共享」（只读镜像为主）不同质：daemon 侧已就绪（共享 runtime、宽通道、RPC 写路由），堵点在前端 chat 视图架构——现为本地状态机，需重构为 runtime 状态投影，draft 也不跨窗同步。将来单独立项，v1 不含。
- 窗内 tab（Phase B 已否决；D5）。
- hide-on-close / 无窗常驻（D2，另立议题）。
- Windows 平台差异：v1 只核对 macOS 菜单与快捷键路径；Windows 随 daemon Phase 1 的 Windows 打包 follow-up 一并处理。
- macOS Window 菜单多窗列表 / Dock 菜单多窗项（muda 手拼，可选，v1 不做）。

## 10. 测试计划

单测（S1/S2 红测先行，见 plan）：

- 同 workspace 二开：无 owner mint、无 spawn、无 stop，`focus_window` 被调用，返回 `focusedExisting`。
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

## 11. 资源估计（假设，待实测）

每窗一 WKWebView 约 150–300MB，每 session 一 pi 约 100–500MB；5 窗 5 session 粗估 1.5–4GB。宽通道下每窗可订阅全部 live runtime，事件量 × 窗数（follow-up 可改按需订阅）。以上均为估计值，实施期随 e2e 记录实测基线（对齐 daemon spec §3.5 的内存基线要求）。

## 12. 规模与顺序

总计 5–8 人日：

| 步 | 内容 | 量级 |
| --- | --- | --- |
| S1 | label 冲突 → 聚焦既有窗（daemon 预检 + `focus_window` op + Blocked 分支改造）+ 单测 | 2–3 人日 |
| S2 | 三个入口 + ⌘N/⌘T 迁移 + landing label 唯一化 + ledger/mirror 记账 | 2–3 人日 |
| S3 | 通知点击路由 + 多窗 e2e（§10 清单） | 1–2 人日 |

S1 是硬前置：不做，任何入口一按就报错。风险登记：`open_workspace` 链路生产未锻炼（暗路径补课是 S2 主体）、多窗事件扇出、内存估计值。均不阻塞。
