# Subagent 主聊天显示与导航设计

**Status:** v2 设计定案（2026-09-29 按 `@tintinweb/pi-subagents` 契约重写，未实现）。v1（2026-09-18 grilling 定案 Q1–Q6）基于 nicobailon `pi-subagents` 契约，随扩展选型切换作废重写；v1 的四组件结构、只读导航原则、降级思路保留。
**Date:** 2026-09-18（v1）/ 2026-09-29（v2）
**Provenance:** v2 事实底座来自 `@tintinweb/pi-subagents@0.14.3`（已装于 `~/.pi/agent/npm/node_modules/@tintinweb/pi-subagents/`）源码核实：`src/index.ts`（工具注册、通知、AgentDetails）、`src/agent-runner.ts`（child 会话创建与命名）、`src/output-file.ts`（.output transcript）、`src/ui/fleet-list.ts`（setWidget 为 TUI 回调渲染）。runtime 字段终值由实施 Task 0 实测钉死。参照 upstream `picot-public-v3.3` ACP 卡片（`f207db0`）仅取形态。

---

## 1. 问题（不变）

主代理经 subagent 扩展派发 subagent 后，Picot 有三处缺口：

1. 主聊天难看：tool card 对 `Agent` 工具无差别渲染，结构化 details 被丢弃。
2. 没有任何实时 fleet 状态面：tintinweb 的 fleet widget 是 TUI 回调渲染，headless host（Picot）无数据可镜像——v1 仰仗的 `PI_SUBAGENT_ASYNC_JSON` JSON 流**不存在**。
3. sidebar 不即时；subagent 会话现以 `{type}#{agentId8}` 名平铺出现在桶扫描里（无 parentSession、无 marker），点击会 spawn 新 pi 进程加载同一 jsonl，与原进程双写。

## 2. 事实底座（@tintinweb/pi-subagents v0.14.3，已核实到源码）

### 2.1 工具面

工具名：`Agent`、`get_subagent_result`、`steer_subagent`、`SubagentWorkflow`（workflows 本设计不覆盖）。`Agent` 默认后台：

- **后台**：tool 立即返回，`content` 为人读文本（含 `Agent ID: {id}`），`details`（`AgentDetails`）带 `agentId`、`status:"background"`、`toolUses/tokens/durationMs`，spawn 完成后补 `outputFile`。**不含 child session 路径**。
- **前台**（`run_in_background:false`）：`tool_execution_update` 的 `partialResult.details` 流式推送 `status:"running"`、`activity`（当前工具/响应摘要）、`toolUses/turnCount/tokens/durationMs`。
- 字段终值（`AgentDetails` 全集）由实施 Task 0 实测钉死并回填本节。

### 2.2 完成通知（parent 会话内的权威信号）

后台 agent 结束时，扩展向 **parent 会话**发送 custom message 行（`pi.sendMessage`，`deliverAs:"followUp"`、`triggerTurn:true`）：

- `customType === "subagent-notification"`，`display:true`
- `details = {id, description, status, toolUses, turnCount, maxTurns, totalTokens, durationMs, outputFile, error, resultPreview}`
- `status ∈ completed | steered | aborted | stopped | error`；`stopped`（人停）与 `aborted`（turn 上限）语义不同，结果文本带括注。

该行落入 parent jsonl，因此**历史回放与冷启动重建都以它为终态权威**。分组完成（smart join）时多个 agent 合并为一行，`details.others[]` 携带其余。

### 2.3 `.output` 流式 transcript（新的核心数据面）

`/tmp/{prefix}-{uid}/{encoded-cwd}/{parentSessionId}/tasks/{agentId}.output`（`output-file.ts:27`）：每 agent 一个 JSONL，按 turn_end 增量 flush（Claude Code task 输出格式）。路径含 **parentSessionId 与 agentId**，且 spawn 后即出现在 Agent toolResult `details.outputFile`。这是 v2 取代 v1「fleet snapshot JSON 流」的实时数据源：fs.watch 该文件，每次 flush 即增量解析。

### 2.4 child 会话文件

`SessionManager.create(父 cwd, 默认 session dir)`（`agent-runner.ts:782`）→ 与 parent 同桶（**待 Task 0 实测确认**， Picot 按 parent session 文件路径记桶，child 应同目录）。命名 `session.setSessionName("{type}#{agentId前8位}")`（如 `coder#a1b2c3d4`、`Explore#9f2e…`）。

**与 v1 的关键差异**：child jsonl **无 `parentSession` 头、无 `launch_metadata` marker**。识别与挂树只能靠：① 名称模式 `^.+#[0-9a-f]{8}$`；② `#` 后 8 位与 parent 会话 Agent toolResult `details.agentId` 前缀 join。`persist_session` 默认开（`rememberAgents` 默认 true），child 会话在 `/resume` 树中可见。

### 2.5 Picot 侧现状（不变的部分）

`read_session_messages`（`host_data.rs:311`）纯 host 读同桶任意 session；`fetchDiskHistory → renderTranscriptEntries` 管线现成；sidebar 桶扫描、可见性门槛（≥1 user message + name seal）、`provisionalSession` 单 slot 机制同 v1 §2.3。ToolCardRenderer 仍无差别渲染；`collapseCompletedTurn` 折叠行为同 v1。

### 2.6 双写风险（不变）

subagent 运行中其 jsonl 被孙进程追加，任何「spawn 新 runtime 打开它」的路径都是双写。只读导航原则保留。

## 3. 设计总览（v2）

四组件与 v1 相同，数据源全部换血：

```text
主聊天:  Agent tool card（P2）— 单次 dispatch 记录 + 展开读 .output
composer 上方: fleet panel（P1）— .output fs.watch 驱动的实时概览
sidebar（P4）: provisional 行（agentId 键）+ 桶扫描 join 挂树
导航（P3）: 三入口一律只读视图，渲染 .output JSONL
终态权威: parent 会话 subagent-notification 行（P1/P2/P4 共用）
```

与 v1 的结构性差异：

| v1 | v2 | 原因 |
| --- | --- | --- |
| P1 吃 setWidget JSON snapshot | P1 吃 `.output` 文件 watch + notification 行 | TUI 回调渲染无 JSON 可镜像 |
| join 键 = details.sessionFile | join 键 = `agentId`（details/notification/名称后缀/.output 路径四处一致） | sessionFile 字段不存在 |
| P2 展开读 child jsonl（read_session_messages） | P2/P3 展开**统一读 `.output`**（路径在 details，确定性最高）；child jsonl 仅作完成后「在 sidebar 打开」的常规会话入口 | 免去名称后缀 join 才能拿 transcript 的脆弱链路 |
| P4 marker 探测 `subagent: bool` | P4 名称模式分类 + live 期捕获的 parent→child 链接入 metadata_store | marker 行不存在 |
| host 无状态 | metadata_store 增 `subagent_link` 表（workspace, parent_session, agent_id, child_session?） | 冷启动后树形挂靠需要持久链接 |

## 4. P1：fleet panel（`public/ui/subagent-fleet-panel.js`）

WidgetMirrorRegistry 仍按 `subagents` key 注册 ambient panel（v1 的 registry `applyWidgetLines` 契约改动**取消**——不再有 widget 数据流，panel 由本模块自驱）。

- 数据源：`Agent` tool_execution_end（details.outputFile）→ 对该文件 `fs.watch`（经 host 新 op `watch_file`，见 §10）→ 每次 change 增量读新行（记住文件 offset）→ 解析为行状态。终态以 parent 会话 `subagent-notification` 行翻转（notification 先到则直接终态并停 watch）。
- 行形态沿用 v1（状态图标 + `type · description` + running 第二行 `当前工具 · N tool uses`，数据来自 .output 最新 turn 的工具调用）。
- 冷启动重建：扫 parent 会话历史中的 Agent toolResult（有 outputFile 且无终态 notification 的重新挂 watch）；有终态的不重建面板（v1 同理）。
- join 不上的（.output 已删、tmp 清理）：行降级为「状态未知 · 已派发」，不可点。

## 5. P2：Agent tool card（`public/ui/subagent-agent-card.js`）

`tool-card.js` 分流点条件从 `toolName === "subagent"` 改为 `=== "Agent"`（guard：`details?.agentId` 不满足则回通用渲染）。

- 折叠态：`[状态图标] {type} · {description}`（details/notification 的 status 驱动；AcpCard 形态参照不变）。
- 展开态：懒加载 `.output` JSONL → 新渲染器 `subagent-output-transcript.js`（§6）。运行中每次展开重读全量（文件小，Claude Code task 格式按 turn 分行）。
- `get_subagent_result` 卡：通用渲染即可（其 content 本身是结果文本），不接管。
- agent_end 折叠豁免同 v1；历史回放同 v1（details 持久化于 parent jsonl toolResult；`.output` 被删则降级纯记录行）。

## 6. `.output` 渲染器（`public/ui/subagent-output-transcript.js`，新增共享模块）

- 输入：`.output` 文件路径（受 host 侧读取 op 限制，见 §10）。
- 解析：JSONL 按 Claude Code task 格式逐行解析（Task 0 实测钉死行 schema），渲染为「消息 + 工具调用时间线」的紧凑列表，复用现有 transcript 条目样式类。
- 三处消费：P1 行内摘要、P2 展开体、P3 只读视图主体。一个模块三用，不建通用 per-tool registry。

## 7. P3：只读视图（`public/ui/subagent-readonly-view.js`）

与 v1 相同：非模态 overlay、横幅（`type · description · 状态 · 耗时`）+ 返回、不进 URL 状态机、v1 无「继续此会话」按钮。差异：

- 主体渲染 `.output`（§6），不再走 `read_session_messages`。
- 入口：P1 行点击（agentId）、P2「打开 transcript」链接、P4 subagent 行（provisional 与扫描行；扫描行若 `.output` 已删而 child jsonl 在桶内，退回 `read_session_messages` 渲染——child 完成后这是更好的档案视图）。

## 8. P4：sidebar provisional 行 + host 分类与链接

### 8.1 前端注入（`public/sidebar/index.js`）

- `provisionalSubagents: Map<agentId, session>` 从 `Agent` tool_execution_end details 注入 `{name: "{type}#{agentId8}", parentSession: parentFilePath, running: true}`——**无 filePath**，行挂在 parent 下靠 Map 里的 parent 指针。
- 桶扫描发现名称匹配 `{type}#{agentId8}` 的真实行 → filePath 补齐、provisional 撤销换真实行（树位置不变）。
- 终态：subagent-notification 行到达 → 行状态翻转、running 标记撤除。
- 行点击进 P3。

### 8.2 host 侧（`host_data.rs` + `metadata_store`）

- `session_summary_value` 增 `subagent: bool`：**名称模式分类**（`^[^#]+#[0-9a-f]{8}$`）。不用 marker（不存在）。误报面：用户手动起同名会话——记入降级矩阵，可接受。
- `metadata_store` 新表 `subagent_link(workspace_id, parent_session_id, agent_id, child_session_file NULL, created_at)`：host 转发 toolExecutionEnd/notification 事件时捕获写入；桶扫描时用它把 child 行挂回 parent（冷启动后树不丢）。**没有链接记录的 subagent 行**：按名称分类仍显示 subagent 徽标，平铺在顶层（诚实降级，不猜父）。
- 9/17 可见性契约（不隐藏、不分类影响可见性）不变：`subagent` 是行属性，不是过滤规则。

## 9. 降级矩阵（v2）

| 场景 | 行为 |
| --- | --- |
| 扩展未安装 / 无 Agent 调用 | 零成本，无面板无卡片 |
| details 无 `agentId`（guard 失败） | 回通用 tool card |
| `.output` 已删（tmp 清理 / 重启） | P1 行降级「状态未知」；P2 降级记录行；P3 若 child jsonl 在桶 → 走 read_session_messages，否则隐藏入口 |
| child 会话不在桶内（`persist_session:false` / `session_dir` 覆盖 / Quick Chat parent） | sidebar 无该行；P2/P3 经 `.output` 仍完整可用 |
| `SubagentWorkflow` 派发 | v2 不接管（卡片走通用渲染）；后续另议 |
| 用户自建 `foo#abcd1234` 同名会话 | 被分类为 subagent（平铺+徽标）；误报已知，接受 |
| 冷启动无 subagent_link 记录 | subagent 行平铺显示，树挂靠缺失，功能不减 |

## 10. 实现清单

| 文件 | 动作 |
| --- | --- |
| `public/ui/subagent-output-transcript.js` | 新建（§6，三用渲染器） |
| `public/ui/subagent-agent-card.js` | 新建（P2） |
| `public/ui/subagent-readonly-view.js` | 新建（P3） |
| `public/ui/subagent-fleet-panel.js` | 新建（P1，自驱 panel） |
| `public/ui/tool-card.js` | 小改：`Agent` 分流点 |
| `public/ui/widget-mirror-registry.js` | **不改**（v1 的 applyWidgetLines 契约取消） |
| `public/sidebar/index.js` | 扩展：provisionalSubagents Map + join + 只读路由 |
| `public/app.js` | 接线：事件 → 三组件；折叠豁免 |
| `src-tauri/src/host_data.rs` | 小改：`subagent: bool` 名称分类 |
| `src-tauri/src/metadata_store.rs` | 新表 `subagent_link` + 事件捕获写入 |
| `src-tauri/src/host_server.rs` 或 host_data | 新 op：受控读 `/tmp` 下 `.output` 文件 + 轻量 `watch_file`（**安全评审点**：路径必须源自本会话事件携带的 outputFile，不做任意路径读） |
| `public/locales/*.json` ×4 | 文案 |

实现顺序：Task 0（契约实测）→ §6 渲染器 → P3 → P2 → P1 → P4（含 host）。

## 11. 测试计划（在 v1 基础上换数据夹具）

- 夹具：真实 tintinweb 跑一轮产生的 parent jsonl（Agent toolResult + notification 行）、child jsonl（`coder#…` 命名）、`.output` 文件——Task 0 产出并入库 `public/testdata/subagent/`。
- `subagent-output-transcript.test.js`：JSONL 解析、增量 offset、坏行跳过、空文件。
- `subagent-agent-card.test.js`：折叠/展开/降级（无 agentId、无 .output）、notification 终态翻转。
- `subagent-fleet-panel.test.js`：watch 驱动刷新、终态停 watch、冷启动重建、join 不上降级。
- `subagent-readonly-view.test.js`：三入口、`.output` 删后退 child jsonl、返回。
- sidebar/host：provisional 生命周期、名称分类正误报、subagent_link 读写、无链接平铺降级。
- e2e：真实扩展跑 foreground+background 各一，核对四组件。

## 12. 决策记录

v1 Q1–Q6 中保留：Q1 两层分工（widget 概览 + card 记录）、Q2 行形态、Q3 三入口只读 + v1 无继续按钮、Q6 模块纪律与降级矩阵。v2 新增：

| # | 决策 | 理由 |
| --- | --- | --- |
| V1 | 实时数据面 = `.output` fs.watch，终态 = notification 行 | JSON snapshot 流已不存在；.output 每 turn flush 且路径确定性给出 |
| V2 | join 主键 = agentId（放弃 sessionFile） | 该字段在新契约中不存在；agentId 四处一致（details/notification/名称后缀/.output 路径） |
| V3 | transcript 主读 `.output`，child jsonl 退居完成后档案视图 | 免名称后缀 join 的脆弱链路；read_session_messages 保留为降级路径 |
| V4 | host 无状态 → metadata_store subagent_link 表 | parentSession/marker 双消失后，冷启动树挂靠需要持久链接；诚实降级为平铺 |
| V5 | subagent 分类 = 名称模式（非 marker） | marker 不存在；误报面窄且只影响徽标与路由 |
| V6 | SubagentWorkflow 不接管 | 单独编排面，数据形状不同，YAGNI |

## 13. 参照与分歧记录

- v1 全部参照记录仍有效（upstream ACP 卡片形态 `f207db0`、hide 分歧 `04be4f6` vs `1619c4b`、跨 workspace 生命周期另文）。
- nicobailon `pi-subagents` 已卸载（2026-09-29），其契约文档仅存于 git 历史；`~/.pi/agent/agents/` 24 个角色文件已按 tintinweb frontmatter 迁移（skills/inherit_context 键、tools 去 web_search）。
- `.memory/topics/pi-subagents-contract.md` 为 nicobailon 契约记录，已标注 superseded。
