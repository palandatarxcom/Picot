# 恢复跨 workspace 切换时旧 runtime 不停止

> 状态：已决策，实施未完成；目标是恢复跨 workspace 切换时运行中的 Pi 不停止，完整 transition 链路、验收和发布前验证尚未完成（2026-09-18，Dr. Lin 拍板）。
> 参照：`~/tmp/PI/picot-public-v3.3` 的用户可见生命周期，以及
> `~/tmp/PI/picot` 的 session 行绿/蓝点语义。
> 评审：`reviews/2026-09-18-cross-workspace-runtime-lifecycle-divergence.review.md`（2026-09-20，含事件边界拍板记录）。
>
> 本文第 1 节记录待恢复的旧行为；第 2 节记录可复用的工作树进度，不能视为功能完成；第 3–5 节定义恢复目标、剩余实现和验收。

## 目标

在单窗口 v3 中从 workspace A 切到 B 时，A 的 pi runtime 继续运行。用户在 B 的侧栏能看到
A 的 session 行仍在工作；回到 A 后复用原进程，而不是从 JSONL 重启一个已中断的 turn。

这只对齐 upstream 的**用户可见**生命周期和 session 行状态。v3 的单窗口、generation
授权与 host 路由机制仍不同于 upstream 的多窗口模型。

## 1. 修复前：跨 workspace commit 会停止旧 runtime

修复前，`workspace_transition_commit` 根据 `current != pending` 调用
`stop_for_owner_transition(owner, generation)`。同 workspace 切 session 会保留旧 runtime，
跨 workspace 则终止该 owner 早于新 generation 的 runtime，包含正在执行的 turn。

这是 v3 单窗口 transition 新增的路径；`picot-public-v3.3` 是一个 workspace 一个窗口，打开
B 不会经过 A 的同窗口 commit，因此 A 的 runtime 随 A 窗口生命周期继续存在。

旧实现还按 owner 过滤 `runtime_instances`，且 Subscribe 每次准入和事件投递均要求
`authorize_target`。B 页面没有 A 的绿/蓝点数据源。

## 2. 可复用的工作树进度（尚未构成功能）

### 2.1 旧 runtime 留活、旧授权失效

工作树中已有一个待验收改动：`src-tauri/src/main.rs` 的
`workspace_transition_commit` 不再调用 `stop_for_owner_transition`。这只是恢复路径的一部分；在完整 transition、授权、订阅和 e2e 均通过前，不能视为该行为已经恢复。跨 workspace 时仍必须执行 generation 相关清理：

- `revoke_session_exports`；
- `side_chat_cleanup_for_transition`；
- `skill_sources.revoke_workspace`；
- `git_service.clear_workspace_state`。

旧 runtime 继续存在，但 `authorize_target` 每次命令准入都会重新读取 owner 的当前
workspace 与 generation。owner 当前在 B 时，A runtime 的 prompt、abort、snapshot 和
对话框响应都被拒绝；这不是“运行中进程仍有旧授权”。

回到 A 时，prepare 通过 `(workspaceId, ownerId, persistedSessionId)` 查找 live runtime，命中后
调用 `rebind_owner_generation`。同一 instance 取得新 generation 后重新可达。显式
`forceNewSession` 是唯一绕过复用的路径。

清理 runtime 的时机不变：窗口销毁、owner 撤销、app 退出和显式 restart。保留
`stop_for_owner_transition` 的 manager API，供其余调用方和现有语义测试使用；commit 不再调用它。

代价已接受：一个 owner 在多个 workspace/session 留下的 runtime 会持续存在至上述清理时机。
不新增上限；这对应 upstream 多窗口下一个 live session 一个进程的用户可见成本。

### 2.2 恢复所需的 runtime 摘要与活动事件

工作树中的 `runtime_instances` 改动向 authenticated native desktop owner 返回可解析为已注册 workspace
和 session file 的所有 live runtime 摘要，包括 `workspaceId`、`sessionId`、`instanceId`、
`cwd`、`sessionFile`、`pid`、`streaming`。`streaming` 直接镜像 coordinator 的 event-driven
状态：`agent_start` 后为 true，`agent_end` 或 `agent_settled` 后为 false。它让页面在错过
`agent_start` 后仍能以正确绿点恢复。它不承诺返回每个 registered workspace 的每个 runtime：无法解析
workspace 或 session file 的 target 留在 host 内部。

恢复完成后，页面据此订阅 live target。首次恢复与同 owner 的每次 spawn 后均刷新该摘要：host 在成功 spawn 后
向 owner 发 `runtime_started`，前端只订阅新增 instance 并请求其 snapshot。工作树中的 Subscribe 改动只要求 authenticated desktop owner 和 live target，
不再要求当前 workspace/generation 匹配。`runtime_request` 命令面没有放宽，仍经过
`authorize_target`。

**事件边界：**工作树改动会向订阅者转发目标 runtime 的全部非阻塞事件，而不只是
`agent_start` / `agent_end`。事件可能包含消息、tool payload、widget 或 notify 文本。阻塞
`extension_ui_request` 的 `select`、`confirm`、`input`、`editor` 是例外：只投给
`authorize_target` 通过的订阅者；返回 A 后由 pending replay 补发。

若采用当前工作树方案，边界是“持有 desktop capability 的本机窗口可观察其他 live runtime 的非阻塞事件”，
而不是仅泄漏 runtime 摘要或绿/蓝状态。若产品要求严格的状态-only 可见性，后续必须改为 host
发出最小化 activity event，并停止转发原始 background event；在那之前不得把该能力称为
只读状态通道。

**2026-09-20 拍板（Dr. Lin）：接受宽通道。**desktop capability 即内容可见边界：本机窗口可订阅其他 live runtime 的全部非阻塞事件，第 4 节第 7 项按原文执行。activity-only 收窄不再是待选路径；未来若出现明确产品需求（演示模式、多用户），按新 spec 重新立项，并将第 4 节第 7 项改写为拒绝内容 payload。

### 2.3 前端状态

恢复方案复用既有分发，不新增状态机：

| 环节 | 现有实现 |
| --- | --- |
| 非当前 runtime 事件 | `handleRPCEvent` 转交 `handleBackgroundRPCEvent` |
| 工作中 | `agent_start` 调 `sidebar.setStreaming(sessionFile, true)`；恢复/新增订阅以摘要 `streaming: true` 补亮错过的绿点 |
| 停止/未读 | `agent_end` 清 streaming 后 `markUnread(sessionFile)`；`runtime_stopped` 无条件清 streaming |
| 查看 session | `setActive` 清 unread；streaming 优先显示 |
| 订阅来源 | `subscribeToLiveRuntimeTargets` 遍历 `runtime_instances`；`runtime_started` 触发幂等增量刷新 |

不做项目级汇总点、折叠 workspace 状态点、轮询或跨 workspace 控制入口。

### 2.4 Quick / Side Chat

不变。ephemeral runtime 属临时 owner 与 ephemeral registry；跨 workspace commit 仍执行
`side_chat_cleanup_for_transition`。本设计只改变 Registered 主 runtime。

## 3. 已决策恢复目标与剩余实现

- 跨项目切换不得中断 A 的任务。
- 侧栏 session 行显示绿点表示 streaming，蓝点表示有未读输出；选中该 session 清除未读。
- workspace 折叠时不汇总状态，不新增项目级圆点。
- 单窗口架构保留，不回退为“一 workspace 一窗口”。（2026-10-17 补注：本句约束的是
  本次 09-18 恢复工程的实施范围，不构成对后续多窗口增量的禁令；多窗口 v1 见
  `not-started/multi-window-v1-design.md`。）

## 4. 剩余实现与必须完成的验证

### Rust

1. 完成 A→B→A 的完整 transition 生产调用链并补测试；当前
   `cross_workspace_return_reuses_the_prior_runtime` 只覆盖 helper 级 rebind。
2. A runtime 在 A→B commit 后仍为 live；B 当前 owner 对它的 `runtime_request` 被
   `authorize_target` 拒绝。
3. 返回 A 的 prepare 复用同一 `instanceId`，并将 generation rebind；随后命令重新获准。
   这是 transition-level async test，不能只测 `find_existing_runtime_for_prepare` 和
   `rebind_owner_generation` 两个 helper。
4. `runtime_instances` 含其他 workspace 的可解析 live instance，且不返回无法解析的 target。
5. authenticated desktop owner 能订阅其他 live target 并收到 `agent_start`；未认证、非 desktop
   或已停止 target 被拒。
6. blocking `extension_ui_request` 只送给 `authorize_target` 通过者；pending replay 遵守同一条件。
7. 非阻塞事件的跨 target 转发覆盖 message/tool/widget payload，明确锁定当前 desktop-capability
   边界；若未来收窄为 activity-only，这条测试必须改为拒绝内容 payload。
8. 同 workspace transition、窗口销毁、owner revoke、app exit 和 explicit restart 的既有清理
   语义回归不变。
9. `restart_runtime` 必须按当前 workspace 选靶：owner 在多个 workspace 留有 live runtime 时，
   重载只停当前 workspace 的实例，不得误停其他 workspace 的 runtime。这是旧 runtime 留活引入的
   回归面——原实现按 owner 首个命中选靶（HashMap 遍历序不定），建立在「每 owner 单 live
   runtime」的旧前提上。**已修（2026-09-20）**：选靶改为 `restart_target_for_owner`
   （main.rs），workspace 从 owner 注册表当前绑定派生（不信任前端传参），同 workspace 多
   live session 取最新 generation；单测 `restart_targets_only_the_current_workspaces_runtime`
   覆盖跨 workspace 不误停与未绑定 workspace 无靶两分支。

### 前端

10. background `agent_start` / `agent_end` 对 session 行调用 `setStreaming` / `markUnread`，且
   选中行清除 unread。
11. 切到 B 时 A 的 background event 不渲染进 B 的 transcript；其 session 行状态仍更新。
12. A 的 blocking dialog 在 B 不出现，返回 A 后 pending replay 只呈现一次。
12. A 的 blocking dialog 在 B 不出现，返回 A 后 pending replay 只呈现一次。
13. A streaming 中切到 B 后返回 A：JSONL 快照先显示；任意后续 `message_update` 接入同一 live turn，绿点保持，transcript 继续实时增长。

### 手工 e2e

14. 在 A 让 agent 执行一段可观察的长任务，切到 B，再回 A：任务不中断、同一 session 继续输出；
    B 侧栏中的 A 行先绿后蓝。
15. 在 A 打开 blocking questionnaire 后切到 B：B 不弹问卷；回到 A 后可继续回答。

运行 `bun run check:rust`、`bun run check`、相关 focused tests，最后运行 `bun run test`。当前
`cross_workspace_return_reuses_the_prior_runtime` 仅覆盖 helper 级 rebind，且曾出现
`no reactor running`；修复其测试 runtime 环境前，不能把它当作本设计已完成的验证。

## 5. 架构文档契约

`ARCHITECTURE.md` 必须保持以下事实：workspace transition 使旧 generation 的授权、操作和
导出令牌失效；旧 runtime 进程留活但在当前 workspace 下不可达，直到窗口销毁、owner 撤销、
app 退出、显式 restart，或回到原 workspace 后 rebind。

显式 restart 指 `restart_runtime` 控制面命令（main.rs）：Registered owner 经 Settings 包管理页
「重载」按钮手动触发，`mark_host_restart` 后 stop 旧实例并以新 instanceId respawn，
workspace/session/generation 不变。
