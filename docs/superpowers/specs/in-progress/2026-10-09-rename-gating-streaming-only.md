# 设计：rename 门禁收紧为「仅 active / streaming」

- 状态：待 Dr. Lin 评审 → 程序猿实施
- 范围：仅 rename 门禁及其 UI 状态。delete 门禁（`deletionBlockedReason`）本轮不改。todo panel 折叠修复不在本文（已另定，程序猿直接做）。

## 1. 目标语义（精确定义）

`public/sidebar/index.js` 的 `renameBlockedReason(filePath)`，分支与顺序：

1. `filePath === this.activeSessionFile` → `t("sidebar.renameDisabledActive")`
2. `this.streamingFiles.has(filePath)` → `t("sidebar.renameDisabledStreaming")`
3. 否则 `return null`

即：rename 仅因「该会话是当前 active」或「正在 streaming（turn 运行中）」被禁；idle 且后台存活的 runtime 不再阻止。顺序沿用现状（active 优先），两者并存时 tooltip 取 active 文案。

`deletionBlockedReason` 保持现状：active ∨ streaming ∨ isLiveSession。

**rename 与 delete 自此不再镜像**：rename 的放行条件是 delete 的超集（delete 独有 live 禁令）。语义依据：改名只向 jsonl append 一条 `session_info` name 事件（src-tauri/src/main.rs:3586 `append_session_info_name`），不重写既有行，host 侧本就无 active/live 守卫（main.rs:3546-3588；host_server.rs:979-1032）；而删除动文件本身，live 时仍危险，且 host `sessionDeleteBatch` 有 running 拒绝兜底。active 仍禁是纯 UI 语义（当前打开会话的标题即窗口上下文）。

## 2. 改动清单（最小集合，不新增模块/状态机）

- `public/sidebar/index.js`
  - `renameBlockedReason`（1235-1240）：删掉 `isLiveSession` 分支。
  - 同步改其上方 doc comment：「Rename mirrors the delete gate…」已不成立，改为「rename 仅在 active 或 streaming 时禁；idle 后台 runtime 不阻止」。
  - **不改**：`isLiveSession`（1242-1249）、`liveInstancesSnapshot` 的全部读写（fetchLiveInstances 749-758、loadRegistryProjects/loadLiveOnlySessions 246-247/651/754-758）、`deletionBlockedReason`、`applyStatusToItem`、`setActive`、`projectRowNode`、`build-session-item.js`、`workspace-focus-sidebar.js`（Focus 视图经 app.js:322 委托同一函数，自动收敛）。
- `public/locales/{en,zh,ja,es}.json`（建议，可独立回退）：删死键 `sidebar.renameDisabledRunning`（唯一引用即被删的 index.js:1238）。若担心 locale 覆盖测试，保留该键也无行为影响；删则必须跑全量 `bun run test`。

不新增模块、不引入新状态机；不需要给 app.js 补传 `getLiveInstances`。

## 3. 快照是否还需要

- **rename 路径：完全移除。** `renameBlockedReason` 不再调 `isLiveSession` 后，rename 判定只剩两个纯内存同步信号 `activeSessionFile` / `streamingFiles`，与快照和 `getLiveInstances` 回调彻底无关。生产路径 app.js 未传 `getLiveInstances` 导致的「rename 被陈旧快照误禁」随之消失；补传回调反而扩大改动面，不做。
- **侧栏整体：保留 `liveInstancesSnapshot`。** 剩余消费者是 delete 门禁（`deletionBlockedReason` 1229 行，本轮不动）与 live-only 视图的可见性合并（754-758）。

## 4. 行 DOM 陈旧问题：三种场景逐一确认，无需补丁

`isLiveSession` 移出 rename 判定后，rename 的全部输入只有 active 与 streaming，两者均已覆盖：

- **切换**：`setActive`（1018-1030）触发 `render()`；`projectRowNode` signature 含 `row.filePath === activeSessionFile`（1853），新旧 active 行 cache miss 重建、按钮重算。`clearActive`（1032-1040）对全行 `applyStatusToItem` 原地重评。
- **Refresh**：signature 含 `streamingFiles.has(...)`（1849）与 active 标志（1853）。Refresh 期间两者不变 ⇒ 命中 keyed cache 的行，其 rename 状态必与当前判定一致。快照变化不再进入 rename 按钮渲染（旧 bug 的根因正是 signature 不含快照）。
- **runtime 结束**：rename 门禁不依赖 runtime 存活，该事件不产生 rename 状态变化，无陈旧可言。streaming 起止由 `setStreaming`/`clearStreaming` → `applyStatusToItem`（225-231）原地翻按钮，已有测试覆盖。

（delete 侧的 isLiveSession DOM 陈旧是既有问题，见 §7 D2，本轮不处理。）

> 实现补记：后续 Dr. Lin 拍板「保留 delete 的 live 门禁，修陈旧 DOM」，已将 `this.isLiveSession(filePath)` 加入 `projectRowNode` signature（index.js 行签名），由 R6 用例锁定。§7 D2 据此关闭；runtime 结束后未 Refresh 的滞后仍属后续项。

## 5. 测试策略（TDD，先红后绿，`public/session-rename-delete-gating.test.js`）

新失败用例：

- **R1 切走后的旧会话可 rename**：active=A 时建行断言 rename 隐藏 → `setActive("/sessions/other.jsonl")`（走真实 render）→ 再建行断言 rename 可用。
- **R2 idle live 不阻止 rename**：`getLiveInstances = () => [{ sessionFile: A }]`（或注入 `liveInstancesSnapshot`），断言 `renameHidden===false` 且 `deleteHidden===true`（顺带钉死 delete 语义未动）。
- **R3 streaming 仍阻止 / R4 active 仍阻止**：沿用现有用例（见下），确认保留通过。
- **R5 Refresh 后前面打开过的会话可 rename**：构造 projects + `expandedWorkspaces`，`liveInstancesSnapshot` 注入 A live，`render()` 后 A 行 rename 按钮 enabled——旧代码红（快照命中 keyed cache，disabled 行被复用）。

现有断言需改两处（语义变更，非削弱测试）：

- `"streaming and live sessions gate rename too"`（~L88-95）：后半段「getLiveInstances 返回 live ⇒ renameHidden true」翻转为 `false`；streaming 前半段保留。理由：live 阻止 rename 正是本次产品决策删除的行为，断言必须随语义翻转；live 门禁的覆盖由 R2 的 `deleteHidden===true` 接管，总覆盖不降。
- `"live-instance snapshot feeds the running gate"`（~L127-134）：`renameHidden` 期望 `true` 改 `false`，并补 `deleteHidden===true` 断言。理由：该测试保护的「快照喂门禁」行为仍在，只是保护对象从 rename 移到 delete；不补 delete 断言才是削弱。
- 文件头 ABOUTME 措辞同步：rename=active/streaming，delete=active/streaming/live。

命令：`bun run test:focused public/session-rename-delete-gating.test.js`（先红后绿）；完成后 `bun run test`（locale 改动要求全量）+ `bun run check`。

## 6. 风险与回滚

- 风险 1：idle live 会话改名与 runtime 并发——改名是 append `session_info` 事件，不改写既有内容；残余风险极低，接受。
- 风险 2：同一行 delete 禁、rename 可，tooltip 不对称（delete 显示 running 文案，rename 无禁用提示）。预期语义分歧（§1），非 bug。
- 风险 3：locale 删键引发覆盖测试失败——全量 test 兜底；失败则单独回退 4 行 locale，保留死键。
- 回滚：核心 diff 是单函数删两行（+可选 4 行 locale），`git revert` 单 commit 即可；无数据/格式迁移，无跨端协议变化（host 无守卫可回滚）。

## 7. delete 侧待 Dr. Lin 后续决定（本轮不改）

- **D1**：delete 的 live 门禁继续用快照粒度吗？生产路径未传 `getLiveInstances`，快照只在 refresh 写 ⇒ 后台 runtime 刚启动时 delete 按钮可能短暂可点（host `sessionDeleteBatch` 会拒 running 并提示，host_server.rs 有兜底）。补传回调 vs 接受现状。
- **D2**：delete 行 DOM 陈旧——signature 不含快照，runtime 结束后 Refresh 命中缓存行 delete 按钮陈旧 disabled。是否把 live/isRunning 并入 signature，或统一交给 `applyStatusToItem`。
- **D3**：delete 门禁语义本身（idle live 可否删）是否参照 rename 放开——host 已有 running 拒绝兜底，UI 层严格度可再议。
