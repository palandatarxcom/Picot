<!-- ABOUTME: 设计规格目录索引与状态总览。 -->
<!-- ABOUTME: 按实现状态分七类。 -->

# 设计规格目录

`docs/superpowers/specs/` 下的所有 `.md` 与 `.html` 文件按实现状态分类存放。
原始扁平目录（85 个文件）已拆为 6 个子目录，便于按状态检索。

## 子目录

| 子目录 | 含义 | 文件数 |
| --- | --- | --- |
| [`implemented/`](implemented/) | 实现已归档；交付范围、偏差与验证限制见各 spec | 56 |
| [`in-progress/`](in-progress/) | 设计已批准实施未完（部分代码已合入或 working tree 有进展） | 2 |
| [`not-started/`](not-started/) | 设计草案或待评审，未实施 | 6 |
| [`audits/`](../../audits/) | 审计记录（非设计 spec，不引入实现） | 1 |
| [`superseded/`](superseded/) | 已被更新的 spec 取代，保留作历史参考 | 5 |
| [`process-evidence/`](process-evidence/) | Native Runtime 迁移过程的证据 / CP 评审 / 闭环记录（不是设计规格） | 24 |
| [`prototypes/`](prototypes/) | 视觉原型（HTML），仅评审用，不进生产 | 3 |

文件数和其他条目的状态来自上轮汇总，尚未同步当前并行归档；本轮只登记 Subagents settings 的完成状态，不将这些计数作为当前目录实况。

## 分类原则

1. **状态字段为第一信号**——spec 文件首部的 `**Status:**` / `## Status` / `**状态:**`。
2. **代码/工作树为第二信号**——若 status 与实际代码不一致（如 spec 写"未实施"但代码已落地），以代码为准，并在备注列说明偏差。
3. **spec 显式标注 SUPERSEDED/Superseded → superseded/**。
4. **不是设计规格的过程文档**一律放 `process-evidence/`。

## 完整状态表

| 状态 | 文件 | 备注 |
| --- | --- | --- |
| ✅ implemented | [2026-07-08-i18n](implemented/2026-07-08-i18n-design.md) | i18n.js 引擎 + 4 locale 全 parity；§4.13 cost iframe 与 §4.14 bootstrap.html 因架构演进 obsoleted |
| ✅ implemented | [2026-07-12-company-extensions](implemented/2026-07-12-company-extensions-design.md) | Corp fork 实现；public 不含（schema v6 不建三表） |
| ✅ implemented | [2026-07-12-file-preview-editor](implemented/2026-07-12-file-preview-editor-design.md) | File tree 集成预览/编辑，commit `b149091` |
| ✅ implemented | [2026-07-14-pinned-projects-sidebar](implemented/2026-07-14-pinned-projects-sidebar-design.md) | workspace Pin 落地；session Pin 已于 08-25 移除 |
| ✅ implemented | [2026-07-15-quick-and-side-chat](implemented/2026-07-15-quick-and-side-chat-design.md) | QuickChatDialog + SideChatManager，commit `f32c494`/`2dd74f4` |
| ✅ implemented | [2026-07-21-terminal-panel](implemented/2026-07-21-terminal-panel-design.md) | 6 个 JS + 5 个 Rust 模块；xterm addon 增强（commit `018d818`） |
| ✅ implemented | [2026-07-24-cjk-font-bundling](implemented/2026-07-24-cjk-font-bundling-design.md) | LXGW WenKai GB2312 子集，commit `5564e44` |
| ✅ implemented | [2026-07-24-skills-page](implemented/2026-07-24-skills-page-design.md) | Skills 三标签页，commit `ccdf1b8`/`c584b68` |
| ✅ implemented | [2026-07-25-sidebar-titlebar-focus-archive](implemented/2026-07-25-sidebar-titlebar-focus-archive-design.md) | sidebar 重构 + Focus + 永久删除，commit `a01f137`/`f8e94a5` |
| ✅ implemented | [2026-07-26-git-panel](implemented/2026-07-26-git-panel-design.md) | GitPanel + GitHistoryPanel + git_pi_runner，commit `7727e9a`/`25efff1` |
| ✅ implemented | [2026-07-26-session-rename](implemented/2026-07-26-session-rename-design.md) | Pi TUI 同持久化模型，commit `1540453` |
| ✅ implemented | [2026-07-27-package-skills-tab](implemented/2026-07-27-package-skills-tab-design.md) | package-skill-inventory.ts |
| ✅ implemented | [2026-07-27-skill-link-installation](implemented/2026-07-27-skill-link-installation-design.md) | skills-install-tab.js |
| ✅ implemented | [2026-08-10-message-toolbar](implemented/2026-08-10-message-toolbar-design.md) | 复制/时间戳/用量 tool card，commit `0eca134` 起的连续修复 |
| ✅ implemented | [2026-08-16-oauth-model-auth](implemented/2026-08-16-oauth-model-auth-design.md) | Phase 0+1 完成；Codex device-code commit `6f2f5a0`+`be6ea93` |
| ✅ implemented | [2026-08-19-settings-extensions-package-manager](implemented/2026-08-19-settings-extensions-package-manager-design.md) | Settings → Extensions，commit `f9a7ea3`/`899d488` |
| ✅ implemented | [2026-08-21-info-panel](implemented/2026-08-21-info-panel-design.md) | 右栏 info tab，commit `5dd2bb9`/`c71ec15` |
| ✅ implemented | [2026-08-24-git-history-panel](implemented/2026-08-24-git-history-panel-design.md) | git-history-panel.js，commit `7727e9a` |
| ✅ implemented | [2026-08-26-workspace-registry](implemented/2026-08-26-workspace-registry-design.md) | 注册制 + SQLite 数据源，commit `7acbc0a` |
| ✅ implemented | [2026-08-27-native-runtime-migration](implemented/2026-08-27-native-runtime-migration-design.md) | D1–D10 拍板；P1–P8 大部分完成 |
| ✅ implemented | [2026-09-04-appearance-settings-page](implemented/2026-09-04-appearance-settings-page-design.md) | theme-grid + Preview + 字号五档，commit `778625b` |
| ✅ implemented | [2026-09-04-terminal-display-settings](implemented/2026-09-04-terminal-display-settings-design.md) | 终端字号/scrollback/WebGL，commit `10c0da1` |
| ✅ implemented | [2026-09-13-advisor-extension-settings](implemented/2026-09-13-advisor-extension-settings-design.md) | advisor host ops，commit `899d488`；landing 09-21 |
| ✅ implemented | [2026-09-13-ask-user-question-rich-renderer](implemented/2026-09-13-ask-user-question-rich-renderer-design.md) | questionnaire-card.js，commit `34c14f1` |
| ✅ implemented | [2026-09-13-fff-extension-settings](implemented/2026-09-13-fff-extension-settings-design.md) | fff host ops，commit `899d488` |
| ✅ implemented | [2026-09-13-mcp-settings-page](implemented/2026-09-13-mcp-settings-page-design.md) | 6 层对齐 mcp-page，commit `5548f03` |
| ✅ implemented | [2026-09-13-widget-mirror-registry](implemented/2026-09-13-widget-mirror-registry-design.md) | runtime widget mirrors，commit `606ad17` |
| ✅ implemented | [2026-09-14-landing-page](implemented/2026-09-14-landing-page-design.md) | TemporaryKind::Landing，commit `a19aa5c` |
| ✅ implemented | [2026-09-16-cache-optimizer-extension-settings](implemented/2026-09-16-cache-optimizer-extension-settings-design.md) | cache_optimizer_config.rs + renderCacheOptimizerSettings |
| ✅ implemented | [2026-09-16-chat-window-turn-ia-scroll-and-type-scale](implemented/2026-09-16-chat-window-turn-ia-scroll-and-type-scale-design.md) | turn 模型 + history 折叠，commit `cc1ab26` |
| ✅ implemented | [2026-09-16-composer-interaction](implemented/2026-09-16-composer-interaction-design.md) | composer C1–C5，commit `090e5ad`/`adcbd32` |
| ✅ implemented | [2026-09-16-extension-settings-rollout-inventory](implemented/2026-09-16-extension-settings-rollout-inventory.md) | 14 项扩展设置全部落地 |
| ✅ implemented | [2026-09-16-goal-extension-settings](implemented/2026-09-16-goal-extension-settings-design.md) | goal_config.rs + renderGoalSettings |
| ✅ implemented | [2026-09-16-lens-extension-settings](implemented/2026-09-16-lens-extension-settings-design.md) | lens_config.rs + renderLensSettings |
| ✅ implemented | [2026-09-16-plan-mode-extension-settings](implemented/2026-09-16-plan-mode-extension-settings-design.md) | plan-mode renderer（bridge 通道） |
| ✅ implemented | [2026-09-16-ponytail-extension-settings](implemented/2026-09-16-ponytail-extension-settings-design.md) | ponytail_config.rs + renderPonytailSettings |
| ✅ implemented | [2026-09-16-rpiv-ask-user-question-extension-settings](implemented/2026-09-16-rpiv-ask-user-question-extension-settings-design.md) | rpiv_config::get_askuser_config + renderAskUserSettings |
| ✅ implemented | [2026-09-16-rpiv-todo-extension-settings](implemented/2026-09-16-rpiv-todo-extension-settings-design.md) | rpiv_config::get_todo_config + renderTodoSettings |
| ✅ implemented | [2026-09-16-safety-guard-extension-settings](implemented/2026-09-16-safety-guard-extension-settings-design.md) | renderSafetyGuardSettings；09-21 被 datarx fork 取代 |
| ✅ implemented | [2026-09-16-vcc-extension-settings](implemented/2026-09-16-vcc-extension-settings-design.md) | vcc_config.rs + renderVccSettings |
| ✅ implemented | [2026-09-16-web-access-extension-settings](implemented/2026-09-16-web-access-extension-settings-design.md) | web-access host ops + renderWebAccessSettings |
| ✅ implemented | [2026-09-17-anydoc-office-preview](implemented/2026-09-17-anydoc-office-preview-design.md) | anydoc_preview.rs + 测试，commit `257a9f8` |
| ✅ implemented | [2026-09-18-cross-workspace-runtime-lifecycle-divergence](implemented/2026-09-18-cross-workspace-runtime-lifecycle-divergence.md) | 跨工作区订阅 runtime 事件流（旧代留活），commit `edc3721` |
| ✅ implemented | [2026-09-18-landing-bridge-runtime](implemented/2026-09-18-landing-bridge-runtime-design.md) | landing-config-runtime.js，working tree |
| ✅ implemented | [2026-09-18-upstream-immediate-migration](implemented/2026-09-18-upstream-immediate-migration-design.md) | 5 项全落地：`7710a78`+`25efff1`+`2d63dcf`+`744d99a`+`b63347a` |
| ✅ implemented | [2026-09-19-file-mention-paths](implemented/2026-09-19-file-mention-paths-design.md) | spec 自标"两步均已实施并通过验证"（09-19） |
| ✅ implemented | [2026-09-19-steering-and-queue-ux](implemented/2026-09-19-steering-and-queue-ux-design.md) | Enter=steer + Esc 1s，commit `adcbd32`/`a62a97a` |
| ✅ implemented | [2026-09-19-turn-files-card](implemented/2026-09-19-turn-files-card-design.md) | public/ui/turn-files-card.js + test；spec 标"待复核"指 spec 文本未定稿 |
| ✅ implemented | [2026-09-20-history-scroll-auto-load](implemented/2026-09-20-history-scroll-auto-load-design.md) | 滚动触发状态机 + history gate；Paseo 参照移植 |
| ✅ implemented | [2026-09-20-session-resident-views](implemented/2026-09-20-session-resident-views-design.md) | 前端视图层（host session 常驻已实现）；tab LRU 保活 |
| ✅ implemented | [2026-09-21-datarx-safety-guard-pi](implemented/2026-09-21-datarx-safety-guard-pi-design.md) | safety-guard-dialog.js（fork @firstpick/pi-extension-safety-guard） |
| ✅ implemented | [2026-09-22-browser-pane-annotation](implemented/2026-09-22-browser-pane-annotation-design.md) | browser-pane/ 目录（tab-renderer + annotations）；09-25 改注入 pane |
| ✅ implemented | [2026-09-22-provider-quota-display](implemented/2026-09-22-provider-quota-display-design.md) | cost/provider-quota-panel.js + quota-locale |
| ✅ implemented | [2026-09-23-embedded-pi-path-toggle](implemented/2026-09-23-embedded-pi-path-toggle-design.md) | Settings → 通用 PATH 开关 |
| ✅ implemented | [2026-09-23-files-tree-and-paseo-icons](implemented/2026-09-23-files-tree-and-paseo-icons-design.md) | file-browser.js 1174 行 + file-tree.js + file-context-menu.js + file-type-icons.js + material-file-theme/，commit `3ab827e` |
| ✅ implemented | [2026-09-26-local-follow-up-queue](implemented/2026-09-26-local-follow-up-queue-design.md) | public/ui/follow-up-queue.js（125 行），commit `039bd49` |
| ✅ implemented | [2026-09-30-subagent-settings](implemented/2026-09-30-subagent-settings-design.md) | 候选盘点、只读定义、四字段名字级覆盖与 Skills 同款 UI 已落地；新建仍禁写，live parity 未证实；最终范围与限制见 §0 |
| 🚧 in-progress | [2026-09-18-subagent-display](in-progress/2026-09-18-subagent-display-design.md) | 设计定案；widget + tool card 未实现 |
| 🚧 in-progress | [2026-09-27-clinical-research-agent-team](in-progress/2026-09-27-clinical-research-agent-team-design.md) | 试用版；六份岗位指令 + Paseo profile 已建，真实团队未验证 |
| 📋 not-started | [2026-09-18-acp-external-agent-delegation](not-started/2026-09-18-acp-external-agent-delegation-design.md) | 设计草案，Dr. Lin 2026-09：暂时不做 |
| 📋 not-started | [2026-09-20-persistent-daemon-relay](not-started/2026-09-20-persistent-daemon-relay-design.md) | 持久 daemon + relay 接入；Draft 待拍板 |
| 📋 not-started | [2026-09-22-environment-toolchain](not-started/2026-09-22-environment-toolchain-design.md) | Settings 「环境」页；Draft 待评审 |
| 📋 not-started | [2026-09-22-line-review-workflow](not-started/2026-09-22-line-review-workflow-design.md) | 行级 Code Review 工作流；Draft 待评审 |
| 📋 not-started | [2026-09-23-paseo-pi-only-fork-relay-mobile](not-started/2026-09-23-paseo-pi-only-fork-relay-mobile-design.md) | Paseo Pi-only fork + relay + 移动端；阶段一裁剪已实测 |
| 📋 not-started | [2026-09-27-pi-subagent-extension](not-started/2026-09-27-pi-subagent-extension-design.md) | Draft 仅设计未实现 |
| ⛔ superseded | [2026-07-25-at-file-mention](superseded/2026-07-25-at-file-mention-design.md) | 由 [2026-09-19-file-mention-paths](implemented/2026-09-19-file-mention-paths-design.md) 扩展并部分取代 |
| ⛔ superseded | [2026-07-27-claude-skills-discovery](superseded/2026-07-27-claude-skills-discovery-design.md) | 由 [2026-08-07-composer-skill-discovery-and-execution-fixes](superseded/2026-08-07-composer-skill-discovery-and-execution-fixes.md) 取代 |
| ⛔ superseded | [2026-08-07-composer-skill-discovery-and-execution-fixes](superseded/2026-08-07-composer-skill-discovery-and-execution-fixes.md) | §3.9 设计点 1-2 由 Pi 0.84.2 原生 expandPromptTemplates 取代（08-25） |
| ⛔ superseded | [2026-09-03-picot-external-terminal](superseded/2026-09-03-picot-external-terminal-design.md) | Path 2 拍板后暂停；tty7/liney 调研后认为非 WebView 嵌组件 |
| ⛔ superseded | [2026-09-20-session-scan-bounded-io](superseded/2026-09-20-session-scan-bounded-io-design.md) | 实施后 revert（`e4079fb`）：64 KiB head window 截断在 ~150 KB system/context；256 KiB tail 命中 name 0%（实测最深 5.7 MB）。bounded scan 不能服务 Pi session_info 语义，保留作分析存档 |

## 相关参考

- 实施计划：[`../plans/`](../plans/)
- 审计记录：[`../audits/`](../audits/)
- 主架构：[`../../../ARCHITECTURE.md`](../../../ARCHITECTURE.md)
- Agent 指南：[`../../../AGENTS.md`](../../../AGENTS.md)
