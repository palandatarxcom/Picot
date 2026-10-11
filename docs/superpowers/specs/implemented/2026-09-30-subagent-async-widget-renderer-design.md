# `subagent-async` widget 专用 renderer

**状态：** 已评审修订（军师评审 + Dr. Lin 拍板），待实施；Plan 见 `docs/superpowers/plans/2026-09-30-subagent-async-widget-renderer.md`。
**日期：** 2026-09-30
**适用契约：** Nico Bailon `pi-subagents` v0.73.1 的 RPC snapshot；不适用于 `@tintinweb/pi-subagents`。已作废的 `2026-09-18-subagent-display-design.md` v2 曾面向 tintinweb 的 `.output` 数据面，不应把两者接在同一个 renderer 上（`docs/superpowers/specs/superseded/2026-09-18-subagent-display-design.md`）。

## 目标与边界

Picot 目前把 `subagent-async` 当作未知 widget：标题显示原始 key，正文显示整行 JSON（`public/ui/widget-mirror-registry.js:83-96`）。目标是在 composer 上方把**当前 runtime** 的异步任务概览渲染为可读的只读面板；扩展仍是唯一状态来源，不读磁盘、不追踪 tool result。

## 数据契约与降级

- RPC 接收 `widgetLines` 为**恰好一行**，内容为 `PI_SUBAGENT_ASYNC_JSON:` + JSON；发送方通过 `encodeAsyncStatusSnapshotWidget` 生成该格式（`~/.pi/agent/npm/node_modules/pi-subagents/src/runs/background/async-status-snapshot.js:3,22-24`；RPC 分支见同包 `src/tui/render.js:2937-2942`）。只对 `widgetKey === "subagent-async"` 启用解析。
- JSON 必须是非数组对象、`kind === "pi-subagents.async-status-snapshot"`、`version === 1`、`runs` 为数组；每个上屏 run 只接受 `label` 字符串及已知 `state`，可选字段按类型检查。发送方当前默认上限：20 个根、每节点 8 个孩子、深度 3、序列化 JSON 32 KiB（`~/.pi/agent/npm/node_modules/pi-subagents/src/runs/shared/async-status-projection.js:7-23,527-543`）。Picot **自己**在解析前限制 JSON UTF-8 字节数为 32 KiB，不依赖发送方自报的 `caps`；可显示行数另行限制（下节）。
- 行数不对、缺前缀、超长、JSON 语法错误、未知 kind/version、缺失基本结构：**清空并隐藏该 runtime 原有面板**；不回退 DefaultTextPanel，避免把原始 JSON 或陈旧状态暴露在 UI。非关键可选字段类型不符则仅省略该字段。`omitted.runs` 仅接受非负安全整数，其余值视为 0；无有效根行且 `omitted.runs === 0` 时隐藏面板；无有效根行但 `omitted.runs > 0` 时保留面板，仅显示「N 个后台任务 · 详情不可用」（N 为 `omitted.runs`），不臆造行或状态计数。动态文本只写入 `textContent`，绝不按 HTML 解释。
- `widgetLines === undefined` 是删除信号，不当作坏 JSON。扩展在无任务时发送该信号（`~/.pi/agent/npm/node_modules/pi-subagents/src/tui/render.js:2929-2935`）。
- 版本边界：依赖 pi-subagents v0.73.1 实测契约；kind/version 未知时隐藏面板属有意保守，升级上游需重新捕获帧。

## 接线与生命周期

在 `public/app.js` 中参照 `rpiv-todos` 注册：`registerRenderer({ widgetKey: "subagent-async", toolNames: [], createPanel })`，不设 `replay`、`matchesNotify`（`public/app.js:985-994`）。现有后台事件已把 `setWidget` 连同 `runtimeId` 送进 registry（`public/app.js:3088-3093`）。

**必要的最小 registry 变更：** 当前 `handleWidgetRequest` 在选 renderer 前就拒绝已定义但非 `string[]` 的 `widgetLines`（`public/ui/widget-mirror-registry.js:68-83`）；若只在注册 renderer 分支增加解析，坏帧仍无法清除旧面板。分支顺序改为：验证 method/key 后先按 `(widgetKey, runtimeId)` 定位；`widgetLines === undefined` 沿用 `#removePanel` 并返回 `true`（字段缺席与显式 `undefined` 在此分支语义相同）；已定义但非 `string[]` 时，仅当**已有同 key、同 runtime 的注册 renderer panel 且具有 `applyWidgetLines`**，将原值交给该方法清空并隐藏，返回 `true`，不得新建 panel；其余情况继续返回 `false`，未知 widget 的既有 panel 不动。合法 `string[]` 时，未知 widget 仍走 DefaultTextPanel；注册 renderer 先 `#ensureRegisteredPanel`，仅在 panel 具有 `applyWidgetLines` 时传入 lines。该方法须自行校验输入，坏帧清空当前 panel 的显示状态，后续有效帧可恢复。rpiv-todo 没有该方法，合法帧仍只是心跳、非法帧仍返回 `false`，tool-result/replay 不变（`public/ui/widget-mirror-registry.js:68-102,175-180`）。不加新缓存或 tool-result 路由。

复用 `panelKey(widgetKey, runtimeId)` 与 `handleRuntimeChange` 隐藏/恢复，不串 runtime；不启用历史 `replay`，因此会话切换、重开后不会从 jsonl 重建，切回**仍存活且未清除**的 runtime 时可恢复该 runtime 最后一次有效 snapshot（`public/ui/widget-mirror-registry.js:129-150,197-199`）。沿用默认 `aboveEditor`，即 composer `<form>` 前；不另开悬浮层（`public/ui/widget-mirror-registry.js:3,73-75,157-172`）。

## 数据到展示

| snapshot 字段 | 第一版展示 |
| --- | --- |
| `runs[].label` | agent 名；workflow 时为发送方提供的 agent 名组合，不解析 `id` 猜名称。 |
| `runs[].state` | queued、running、complete、failed、partial、paused、stopped、rejected 映射为状态文字；其他值不渲染该行。 |
| `runs[].activity.currentTool`、`turnCount`、`toolCount` | 有值才显示当前工具、轮次、工具调用次数；合法的 0 显示为 0。 |
| `runs[].activity.state` | `needs_attention` 只增加「需关注」叠加徽标，主状态（如「运行中」）保留；v0.73.1 的另一个已知值 `active_long_running` 不显示徽标，枚举外值（包括 `active`）一律忽略，不替代主状态。 |
| `runs[].startedAt`、`updatedAt`、`endedAt`、`activity.currentToolStartedAt` | 有 `startedAt` 才算任务耗时：终态优先 `endedAt - startedAt`，无 `endedAt` 时以 `updatedAt - startedAt` 估算；运行中以本次 snapshot 渲染时间减 `startedAt`。有当前工具及合法 `currentToolStartedAt` 时可附工具已运行时长。负值/非有限值不显示；**仅收到新 snapshot 才重算**，不加本地计时器或逐秒跳动。 |
| `runs[].children` | v1 完全忽略；不展开、不缩进、不显示子项数量。每个根 run 仅一行。 |
| `runs`、`omitted.runs` | 从 runs 数组计算已收到的有效根总数及各状态计数；`+N` 仅计算超过 4 行的有效根行与合法的 `omitted.runs`（不重计）；`children`、`omitted.children` 完全忽略，不把局部根计数冒称全部任务。例如 5 个有效根、`omitted.runs: 2`、`omitted.children: 9`：上屏 4 行，摘要 `+3`（1 个未展示有效根 + 2 个省略根），绝非 `+12`。 |

面板标题为「后台任务」；遵循发送方根节点顺序（running、queued 优先，`~/.pi/agent/npm/node_modules/pi-subagents/src/runs/shared/async-status-projection.js:213-220,531-540`）。**最多展示前 4 个有效根**是 Picot 的 UI 决定，参考上游 TUI 的 `MAX_WIDGET_JOBS=4`，并非 RPC 协议约束；每根一行，剩余有效根与省略根显示 `+N` 摘要。`runs: []` 且 `omitted.runs > 0` 时用上文「N 个后台任务 · 详情不可用」替代空面板。第一版没有展开按钮、树形结构或子项计数；即使 20 个根也不会把输入框顶离屏幕。原型树形场景已移除。

**协议缺口：** v0.73.1 snapshot 的 run 节点只提供 `id/kind/label/state/startedAt/updatedAt/endedAt/activity/children`（同包 `src/runs/shared/async-status-projection.js:291-305`）；activity 含 `state/currentTool/lastActivityAt/currentToolStartedAt/turnCount/toolCount`（同文件 `:90-103`）。没有 task 文本、tokens、progress、error；不设计这些字段的展示，也不从聊天记录或 needs_attention 事件通道猜。tokens 属于另一条 `projectAsyncWorkflowRows` 投影，不能拼成这个 widget 的字段（同文件 `:519-540`）。

## 样式与风险

复用现有 `.widget-mirror-panel` 的版位、边框与文本 token（`public/style.css:6328-6351`）；视觉细节留给并行 UI/UX 原型。**不直接复用** `.rpiv-todo-panel` 类名：其布局是绝对定位的浮动面板（`public/ui/rpiv-todo-mirror.css:1-18`），而 v3 CSS 只针对该类覆盖磨砂背景（`public/ui/rpiv-todo-mirror-v3.css:1-27`）；若原型要求同样磨砂质感，使用 `--bg-frosted` 等现有 token 在本面板自己的类上实现，不修改 todo 样式。

状态必须有文字，不仅靠颜色；行是只读文本，不伪装按钮。面板/摘要给读屏器可理解的任务数和状态文字；快照高频到达时不逐帧播报工具名或耗时，也不自动移动焦点。Paseo 将状态计数与可达性标签一起生成（`/Users/linyong/tmp/PI/paseo/packages/app/src/subagents/track-presentation.ts:74-97`），但 Picot 只需按收到的 snapshot 更新，不引入 Paseo 的流事件节流队列。

不做 steer/停止/点击导航、子代理树、后台轮询、持久化或历史重放。已知边界：snapshot 是瞬态的；若 runtime 消失但未收到清除信号，旧 panel 只随 runtime 切换隐藏，不把它误称为历史真实状态；本版不另建运行时清理通道。

## 验收口径（实施阶段）

- Plan 第一项捕获真实 Pi→Picot RPC `setWidget` 更新帧与删除帧，确认 `widgetLines` 字段缺席与显式 `undefined` 在实际传输中各是什么形态；以捕获帧为来源制作 fixture，校验前缀、单行结构和删除语义，不用只从代码抄出的夹具替代协议证据（`docs/engineering-lessons.md:13-25`）。若真实传输形态不符合本设计，先修订契约再实施。
- 聚焦测试：有效多 run 刷新/4 行截断、`children` 与 `omitted.children` 不上屏、5 有效根加 `omitted.runs: 2` 得 `+3`、`runs: []` 加 `omitted.runs: 2` 显示「2 个后台任务 · 详情不可用」且无虚构状态、完全空根隐藏、`needs_attention` 徽标不覆盖主状态、未知 `activity.state` 忽略、耗时仅随 snapshot 更新；坏 JSON/未知版本/超长清空且不露裸 JSON、`undefined` 删除、两个 runtime 隔离及切回恢复。另断言已存在 subagent panel 收到非 `string[]`（如 `null` 或 `[1]`）即清空并隐藏，后续有效帧可恢复；同类坏帧对无 renderer widget 返回 `false` 且原 panel 不动、对 rpiv-todo 返回 `false` 且任务不变，合法 rpiv-todo 心跳与 tool-result 行为不变。手工核对状态文字/读屏摘要及高频更新不抢焦点、不反复播报。
- 手工检查 composer 上方位置、长 label 截断、暗/亮主题与窄窗；不需要通过本 spec 预定视觉稿。设计经评审后才进入实施范围。

## 参考实现对比（Paseo）

- **来源不同，不移植解析器。** Paseo todo 从 provider tool-call 输入解析，或由 daemon timeline `todo` 事件直接给出；事件归约为 `taskSnapshot`，按 server/agent 写入 `agentTasks`，恢复历史时从最新 `todo_list` 重建（`/Users/linyong/tmp/PI/paseo/packages/app/src/utils/tool-call-parsers.ts:276-338`；同项目 `packages/app/src/timeline/session-stream-reducers.ts:1640-1643`；`packages/app/src/stores/session-store.ts:353-370,1434-1437`）。Paseo subagent 一路选 session store 中 `parentAgentId` 匹配的托管 agent，一路用 daemon provider-subagent descriptor 的 list/update，按 server/parent 隔离（同项目 `packages/app/src/subagents/select.ts:72-155`、`packages/app/src/subagents/provider-store.ts:53-91,147-225`）。Picot 此 renderer 只有 `setWidget` RPC snapshot，禁止从 tool result 或 event 流补数据。
- **展示值得借鉴。** Paseo todo 复用只读任务行，subagent 先归一 label/status 再绘制行；混合状态分别计数，读屏标签拼出明确状态，空任务列表不渲染（同项目 `packages/app/src/composer/task-list/index.tsx:9-43`、`packages/app/src/components/task-list-row.tsx:28-44`、`packages/app/src/subagents/track-presentation.ts:18-44,74-97`、`packages/app/src/subagents/track.tsx:70-105`）。Picot 采纳明确状态文字、扁平行、读屏摘要；不采纳 Paseo 的可点击导航/归档操作或 provider 描述字段，因为 snapshot 缺少相应交互与 task 文本。
- **高频与生命周期不照搬。** Paseo 按 agent 汇集事件，在 animation frame 提交（隐藏标签页用 48ms timer 保底），同一 batch 的 todo 使用最后一次 snapshot；UI 由 memo/equality 避免无关重渲染（同项目 `packages/app/src/timeline/session-stream-reducers.ts:20,1669-1712,1716-1780,1842-1876`、`packages/app/src/subagents/select.ts:157-184`、`packages/app/src/composer/task-list/index.tsx:9-18`）。Picot 不承担 daemon 高频事件归约，现有 registry 按 runtime key 隔离并隐藏非活动 runtime；保持立即应用新 snapshot、耗时仅随 snapshot 更新、无逐秒计时器，不额外引入节流/缓存（`public/ui/widget-mirror-registry.js:69-102,129-150,197-199`）。Paseo 面板随活跃未归档 agent 的 composer 存在，todo 空列表消失、subagent 空且无归档操作时消失（同项目 `packages/app/src/panels/agent-panel.tsx:1159-1174,1258-1273`、`packages/app/src/subagents/track.tsx:70-74`）；Picot 对应 `undefined` 删除、坏帧/完全空根隐藏、仅省略根保留摘要与 runtime 切换隔离已在上文定义。
