# Session 常驻视图：并行会话的即时切换与完整视图

**状态：** Phase A Implemented — 2026-09-22；Phase B/C 不做（Dr. Lin 拍板：A 已交付核心价值「切回零请求、gate/滚动/内容恢复、后台增量」，B 的无闪烁升级边际价值小、动渲染核心风险高，永久缓后视体验再定；C 维持「另出 spec」不变）。

实施记录：`public/app/session-view-cache.js`（LRU 5、appendMessage 按 entryId 增量、mtime+sizeBytes stamp 校验）+ app.js 接线（select no-op 短路 / 渲染落定 capture / leave 捕获 scrollTop+revealedCount / background `message_end` 增量、`compaction_end`/`session_tree` 失效）+ wire 新增 `sizeBytes` 字段（`session_summary_value`）+ `restoringCachedView` 抑制 bottom-anchor settle 清零恢复滚动。偏差：leafId 不作命中条件（后台无新 leafId 来源，作渲染参数用）；命中率判据 = trusted（事件流增量）或 stamp 匹配。
**演化关系修正：** host 层 session 常驻**已经实现**，本 spec 收窄为前端视图层）
**日期：** 2026-09-20
**参照：** Paseo `docs/agent-lifecycle.md`（runtime residency）、`docs/timeline-sync.md`
（no-op/增量/替换三路径）、`workspace-panel-host.tsx` + `retained-panel.tsx`
（tab LRU 保活）、`use-mounted-tab-set.ts`（cap 3）；Picot 09-18
「跨项目切换不停 runtime」决策与实施

## 问题（含修正）

原始诉求来自与 Paseo 的对比：切 session / 切 workspace 不中断执行、不整屏刷新。
本轮源码核实修正了问题边界——**host 层已经做到「不中断」**：

- 同 workspace 内切 session A→B：B 走新 runtime，**A 不停**（可能有活动 turn），
  A 的事件降级为后台路由（侧栏绿/蓝点）。
- 跨 workspace 切换：旧 workspace 的 runtime 存活（09-18 删
  `stop_for_owner_transition` 的既定成果），仅撤销旧 generation 的导出令牌/
  skills/Git/临时对话。
- 切回 A：prepare 按 `(workspace, owner, session_id)` 找到存活 runtime，
  rebind generation 后采纳，不重派生进程。

剩余差距全部在前端视图层：

1. **切回即全量重渲染**：`handleSessionSelectImpl` 同 workspace 分支走
   `requestRuntimeSnapshot + fetchDiskHistory → renderSessionHistory`，整棵
   transcript DOM 重建、`turnRegistry.clear()`、gate 重置。Paseo 切回是
   no-op（timeline 在 store，同 epoch+maxSeq 直接跳过，上滚视口原样）。
2. **后台会话只有点，没有视图**：每个时刻 WebView 只有一个前台 routing
   context；其余 runtime 的输出只喂侧栏状态点。Paseo 每个 tab 是完整视图
   （LRU 保活 cap 3）。
3. **无多窗口/多 tab 同 session**：上一轮已分析（Paseo 靠 daemon 单写者 +
   N 订阅者；Picot 每窗口单前台绑定）。

> 说明：此前讨论中的「#3（session 切换 no-op 短路 + DOM 留存）」即差距 1，
> 并入本 spec 作为 Phase A，不再单独立项。

## 已验证事实

| 事实 | 证据 |
| --- | --- |
| 同 workspace 切 session 不停旧 runtime（注释明示「may have an active turn」） | `main.rs workspace_transition_commit` 分支注释（3601 起） |
| `stop_for_owner_transition` 无生产调用方，仅测试 | 全仓 grep：命中均在 `native_pi_manager.rs` 测试 |
| 切回采纳：`find_existing_runtime_for_prepare` 按 workspace+owner+session_id 匹配，rebind generation | `main.rs:712`、prepare 分支 |
| coordinator 以 `(workspace, session)` 唯一登记；wire 层 `new_session`/`switch_session` 被禁（身份不可经 wire 替换） | `runtime_coordinator.rs:135-194` |
| 事件按 `__target` 三元组区分归属，非当前 runtime 走 `handleBackgroundRPCEvent`（点状态），多 runtime 事件路由已存在 | `app.js:2631-2650` |
| WS 前台订阅是单 target（`runtime_subscribe`） | `websocket-client.js:545` |
| 同 owner 后台 runtime 帧可寻址（authorize_target 实时校验 owner+live，不限定前台） | `host_server.rs` runtime-admission 分支 |
| Paseo：tab LRU cap 3 + 修改态强制保留 + `display:none` 不卸载 + 深层失活感知 | `workspace-panel-host.tsx`、`retained-panel.tsx`、`use-mounted-tab-set.ts` |
| Paseo：同 epoch+maxSeq 是 display no-op；仅真 gap/epoch 变化才原子替换 | `docs/timeline-sync.md` |
| Pi 会话 jsonl 是追加型，磁盘读取有 mtime 可做失效判据 | 现状 `fetchDiskHistory` 与 `session_activity_time` |

## 设计

分三阶段，A 是 B/C 的地基；每阶段独立可交付、独立可回滚。

### Phase A：切回 no-op（会话视图缓存）

新增 `public/app/session-view-cache.js`（职责单一：per-session 视图状态登记）。

**缓存内容**（key = sessionFile）：

- `entries`：最近一次渲染用的会话条目快照（含 leafId）。
- 视图状态：`revealedCount`（gate）、scrollTop、turnRegistry 摘要
  （rail 所需的 turn id/preview 列表，非 DOM）。
- 失效判据：`mtime + size`（sidebar 扫描同款元数据，Pi 追加写语义下足够）。

**切换路径改造**（`handleSessionSelectImpl` 同 workspace 分支）：

1. 命中缓存且 `mtime/size` 未变且 leafId 相同 → **no-op 短路**：不请求
   snapshot、不读磁盘，恢复缓存视图状态（gate/滚动/rail）。
2. 未命中或已失效 → 现行全量路径（snapshot + disk history + 渲染），完成后
   写入缓存。
3. **后台增量**：runtime 事件到达且其 session ∈ 缓存但非前台 → 把消息增量
   append 进缓存 entries（不动 DOM；Paseo「增量 append」路径）。前台切回时
   命中的是已增量维护的缓存，仍 no-op。
4. **LRU 上限**（`SESSION_VIEW_CACHE_LIMIT = 5`，Paseo cap 3 加余量）：超限逐出
   最久未访问项。逐出无副作用——再切回走全量路径。

**快照与事件的竞争**：沿用既有 `pendingMirrorSessionFile` 令牌语义；缓存写入
只在渲染落定后发生，防止旧进程快照污染（与现状同一守卫）。

### Phase B：DOM 留存（display:none 切换）

A 验证后，把「恢复缓存视图状态」升级为「留存 DOM 子树」：

- messagesElement 内容按 session 包一层容器（`data-session-file`），非活动
  容器 `display:none`，不卸载。上限同为 LRU 5，逐出即卸载。
- live 渲染器（正在进行的前台 turn）天然只有一个，不受影响。
- 事件增量落 DOM：仅当该 session 容器处于留存态时 append（后台 session 不再
  只喂点）。这是「后台会话有完整视图」的最小形态。
- 内存护栏：留存容器挂载的 turn 数沿用 gate 上限（旧 turn 已折叠，留存的
  DOM 量 = 已揭示量），可控。

### Phase C（可选，结构层）：多前台视图 / 多窗口

- host：`runtime_subscribe` 支持多 target 或多客户端各自订阅（事件广播已
  存在，缺的是 UI 侧多前台路由与 composer 按 target 寻址）。
- 权限对话框路由：现状 owner-only（`extension_ui_requires_owner`）；多视图时
  定策略（Paseo 先答先得 vs Picot 前台优先）。
- 本阶段牵涉授权生命周期与 ARCHITECTURE 安全边界章节，实施前须单独评审，
  本 spec 只登记方向，不展开。
- **2026-10-17 补注；2026-10-18 修订**：本机多窗口已独立立项
  （`not-started/multi-window-v1-design.md`），复用本阶段登记的底层能力（宽通道
  事件订阅、单可写绑定）。其 P2（双可写同 session，Dr. Lin 2026-10-18 拍板）会
  在本机场景内回答本阶段登记的对话框路由策略问题（先答先得 vs 前台优先，该 spec
  §13.1 定为先答先得），并把单可写绑定改写为「同 workspace 信任域内 N 可写」；
  本阶段的 relay 跨实例场景范围不变，仍待 relay 落地后立项，届时可复用 P2 的
  准入放宽与先答先得结论。

## 不做的事

- **不动 runtime 生命周期**——已达标（本 spec 的核心修正）。
- **不做独立 daemon 进程**——Paseo 的进程拓扑（关窗后 agent 继续跑）是独立
  产品决策，另行立项。
- 不引入 React/虚拟化框架；A/B 全部 vanilla JS + 既有渲染器。

## 测试计划

A：
- 切回命中缓存：断言无 `runtime_snapshot_request`、无磁盘读（mock transport
  计数），视图状态（gate revealedCount、scrollTop）恢复。
- mtime 变化 → 缓存失效 → 走全量路径。
- 后台事件增量：切走后事件到达，缓存 entries 增长；切回 no-op 且内容新。
- LRU 逐出与重进。
- 现有 `app-canonical-snapshot.test.js` / `app-real-locate.test.js` 扩展 +
  全绿。

B：
- 容器切换不重建 DOM（节点引用相等断言）。
- 后台留存容器的增量 append。
- 逐出后内存路径（无孤儿监听器）。

C：实施前另出 spec。

## 验收条件

- A：同 workspace 切回已看过的 session，无网络请求、无磁盘读、无整屏重渲染，
  滚动位置与 gate 状态保留。
- B：切换为 `display:none` 级别，肉眼无闪烁；后台 session 的输出在切回时
  完整在场。
- `bun run check` + `bun run test` 全绿；`bun run check:rust`（host 无改动，
  预期零差异确认）。

## 实施顺序建议

1. 本 spec Phase A 与 2026-09-20 扫描 spec、滚动自动加载 spec 相互独立，
   可并行；建议顺序：扫描（最小）→ 滚动（小）→ A（中）→ B（中）→ C（评估）。
