# pi-subagents 0.77.0 → 0.88.0 对 Picot 的影响

**日期：** 2026-11-10  
**Picot Pi pin：** 当前提交中的 `scripts/pi-version.json` 为 1.5.2；本评估未修改此文件。  
**pi-subagents 实际安装：** `~/.pi/agent/settings.json` 将 `npm:pi-subagents` 与 `datarx-agents-team` 作为独立 package 来源启用；`~/.pi/agent/npm/package.json` 声明 `pi-subagents: ^0.88.0`，当前解析版本为 **0.88.0**。上次记录的已评估版本为 0.77.0，因此本报告审查 0.77.0→0.88.0。  
**公司包：** 本地 `datarx-agents-team/package.json` 版本 0.1.4，通过 `pi-subagents.agents: ["./agents"]` 提供 agent 定义；它没有在 `dependencies`/`peerDependencies` 中声明 npm 依赖。两者是 Pi 设置里分别安装、运行时协作的包。

## 结论

**静态契约核对未发现 Picot 设置页、公司 agent 定义或 async widget 与 pi-subagents 0.88.0 冲突。运行行为尚未实测：升级后应在 Picot 中跑一个公司 agent 和一个 workflow，核验 provider pin 与子任务权限路径；fork 副本默认 7 天清理，需单独验证其保留策略。** 已知 UI 差异是 Picot 暂不显示 0.88.0 新增的子代理进度文本。

## Picot / 公司包契约核对

- **版本和旧故障点：** 当前安装为 pi-subagents 0.88.0，Pi pin 为 1.5.2。旧版后台 runner 曾因 `@earendil-works/pi-agent-core/node` 缺失而失败；0.88.0 的 `src/runs/background/runner-aliases.js:20` 将 `./node` 子路径标记为 optional，且 peer dependency 接受 Pi 的 agent-core 版本。该历史兼容问题已在上游修正；Pi 1.5.2 满足 0.80.0 起的 Pi ≥1.2.0 要求。
- **async widget：** `subagent-async`、前缀 `PI_SUBAGENT_ASYNC_JSON:`、`kind: pi-subagents.async-status-snapshot` 和 `version: 1` 均保持不变（上游 `src/shared/types.js:116`、`src/runs/background/async-status-snapshot.js:4`、`src/runs/shared/async-status-projection.js:7-8`）。Picot 的解析器检查同一契约（`public/ui/subagent-async-snapshot.js:16-18,75-81`），并容忍未识别字段。0.88.0 新增 `progressText`/`progressUpdatedAt`（上游 `async-status-projection.js:140-152`）；Picot 当前不渲染这些字段，因此 widget 不会失效，但看不到子代理最近报告的进度。
- **设置页和 agent 格式：** Picot 盘点器自行读取 package agent 定义并识别 name、description、alias、runner（`src-tauri/crates/picot-host/src/subagents_inventory.rs:163-240`）；设置页只写 `model/thinking/advertise/disabled`。对本机 25 份公司 agent 文件，将上游 `parseFrontmatter` 解析结果与 Picot 当前前端字段解析规则逐文件对照：name、description、alias、runner、package 均为 **0 差异**；24 个 `tools` 列表归一化后也与上游解析相同。公司定义均为单行列表，没有多行 YAML block 或 `+tool/-tool` 语法。0.83.1 起上游修正了多行 frontmatter 空行解析；0.82 起不再接受 Pi 风格 `+tool/-tool` 作为 agent 的 `tools` 项，本地定义未使用这些形式。
- **override 校验：** pi-subagents 0.83 起会拒绝未知 `agentOverrides` 字段和无效 thinking 值，而不是静默忽略。本机 `settings.json` 的 25 条 override 仅包含 `model`、`thinking`；字段名均在当前允许范围内，thinking 值均有效。Picot 设置页当前写入的字段也属于支持字段。
- **agent 启用门：** Picot 继续通过 `--exclude-tools subagents_enable` 让 `subagent` 工具常驻（`src-tauri/crates/picot-host/src/native_pi_manager.rs:126-129`）。0.88.0 仍注册同名 loader 工具（其 `src/extension/tool-activation.js:7`），未发现 gate 改名。

## 主要版本变化及影响

| 上游变化 | 对 Picot / datarx-agents-team 的影响 | 判定 |
|---|---|---|
| 0.78：修复 Pi 1.2.0 下后台子代理不能启动；0.80：最低 Pi 版本提高到 1.2.0 | 当前 Pi pin 1.5.2 满足要求；0.77→0.78 的启动故障修复对 Picot 有直接收益 | 兼容 / 收益 |
| 0.79.1：嵌入 Pi 的宿主启动子代理时优先使用宿主自己的 Pi，而不是扩展旁边安装的 Pi | 有利于 Picot：子代理沿用内嵌 Pi，减少宿主与子进程 runtime 漂移 | 收益 |
| 0.83：新增 `subagent:jobs-changed` 通知；设置中的拼写错误开始报错 | Picot 的 widget 镜像仍由 Pi RPC `setWidget` 更新，不依赖新事件；现有 override 字段已核对有效 | 无需改动 |
| 0.84：修复 workflow 脚本越出沙箱的安全问题 | 工作流脚本边界更严格；按官方建议应使用包含此修复的版本 | 安全收益 |
| 0.85：workflow 启动子代理前会发 `subagent_launch` 工具调用，供权限扩展检查和阻止 | 本机没有单独的 pi-guard/pi-permission-system；`settings.json` 独立启用了 Datarx safety-guard，它只处理 bash/write/edit（`datarx-safety-guard-pi/index.ts:604` 起），其余工具名直接放行。因此本机这条调用不会被该 guard 拦截。其他机器若有拒绝未知工具的权限扩展，workflow 可能需要授权 | 本机无影响；团队其他配置需留意 |
| 0.87：`provider/model` pin 改为严格使用指定 provider；provider 不可用时失败，不再悄悄回退到相似模型 | 本机 25 条 agent model override 都是 `provider/id` 形式。无效 provider/auth 会从静默换模型变为明确启动失败；模型健康检查和配置页应能帮助定位 | 运行行为变化，需注意配置 |
| 0.88：widget 展示 `contact_supervisor(progress_update)` 的最近进度；`list` 最多显示 30 个 agent 并支持 search | Picot 会忽略新进度字段，因此只缺少进度细节；公司包本身有 25 个 agents，Picot 设置页使用自己的全量盘点，不受 `list` 上限影响。若把内置、全局和项目 agent 合并后超过 30，模型通过 `list` 浏览时需使用 search | 轻微 UI / 列表行为变化 |
| 0.88：fork 子代理写入的父会话副本按 `cleanupDays` 清理，默认 7 天 | 会影响闲置 fork 副本的保留期，不会删除公司 agent 定义或主会话。需要长期留存子会话副本时应另行归档 | 数据保留行为变化 |

## 推荐动作

1. **不用改 Picot 设置页、`datarx-agents-team` 或 `bun.lock` 来适配 0.88.0。** 当前 snapshot、agent 定义和 override 字段均兼容。
2. 升级后做一次实际运行核验：在 Picot 中启动一个公司 agent，并启动一个会派生子代理的 workflow；确认子代理使用 Pi 1.5.2、`subagent_launch` 没被其他权限扩展意外拦截，agent 设置页仍能盘点和读写配置。若依赖 fork 子会话副本留存，再单独核验 `cleanupDays` 的配置和清理时机；默认 7 天不能由一次短时 smoke 证明。
3. 若希望 Picot 展示 0.88.0 的最新子代理进度，再单独考虑扩展 `public/ui/subagent-async-snapshot.js` 与镜像组件；本次升级不要求这项改动。
4. `datarx-agents-team` 没有声明 pi-subagents 的包管理依赖；当前用户 settings 将两者分开安装。团队部署时应继续确保两个 package 都在目标 agent root 中。

## 限制

- `0.77.0` 是 2026-10-11 本地影响记录中的基线；本轮确认的当前安装为 0.88.0。报告结合 0.77→0.88 changelog、0.88.0 已安装源码、Picot 集成面及本地公司包定义做静态核对。25 份 agent 定义的核对使用上游 parser 和 Picot 当前受支持字段的解析逻辑；没有直接调用 Rust 私有 `frontmatter` 函数。
- 未运行 Picot 集成 smoke 或测试套件。provider 可用性、完整 workflow 运行结果及 fork 文件清理时机仍待实机验证；默认 7 天的清理周期不能由短时 smoke 验证。其他机器上的权限扩展配置也可能不同。
- `~/.pi/agent/settings.json` 中 package 来源与已安装 package root 一致；不同用户、workspace override 或隔离 agent root 可能加载其他版本，部署时应核对实际版本。
