# Settings「守护进程」页签改版设计

**状态：** Draft — 待 Dr. Lin 评审后实施
**日期：** 2026-10-19
**演化关系：** Phase 1（daemon-phase1 分支）把 daemon 状态行、keepRunning toggle、运行中代理列表、pair 区都塞进 Settings → General 面板末尾（`index.html:1251-1370` 的 `#setting-daemon-section` 与 `:1373` 的 `#setting-remote-section`）。本 spec 把这块内容独立成页签并重排信息结构；后端命令面（`daemon_status` / `daemon_set_keep_running` / `daemon_restart` / `daemon_agent_stop` 四个 IPC）零改动。重启确认 modal（commit 81fb79b）原样保留。
**视觉真源：** `docs/superpowers/specs/prototypes/2026-10-19-daemon-tab-prototype.html`（与本 spec 同一 commit 入库）。布局、类名、交互行为以预览为准；本文记录决策与迁移面，不重复视觉细节。

## 1. 决策记录

- **需求（Dr. Lin，2026-10-19，方案原文）**：
  1. Settings 新增「守护进程」页签，排在 General 之后第 2 位，导航键复用 `settings.daemon.title`；General 面板只留 Language / Updates / Embedded Pi 三段。
  2. 页签内容：状态行 / keepRunning toggle / 运行中代理 master-detail（master 用 pkg-manager-layout 按工作区分组列 runtime，组头=cwd，行=session 短 ID+状态徽章+is-selected 选中态；detail 用 pkg-manager-status-grid 键值行 instanceId/sessionId/workspace/state/cwd，加停止按钮；working 状态的停止确认卡内嵌在 detail 面板）/ 重启按钮+modal。
  3. pair 区去掉 QR：单栏（状态行 / URL 只读框 / 过期标注 / 复制 / 重新生成 / 停用 / 警示行）；requiresClient 文案改写为「需要客户端 app 才能打开此链接」。
- **调整点裁决（Dr. Lin 2026-10-19 采纳）**：上一轮架构评估的 3 个调整点全部进实施范围（§5）；其中③ requiresClient 采纳「改写保留」而非删除。
- **modal 不重画**：81fb79b 的 body 级 overlay（`index.html:1010` `#daemon-restart-confirm`）是刚按 Dr. Lin 指示落地的，本次只随 DOM 迁移搬走容器引用，不改视觉与交互。

## 2. 现状与问题

General 面板现为 5 段（language `index.html:1140` / updater `:1162` / pi-path `:1227` / daemon `:1251` / remote `:1373`），daemon 段独占两屏，与「General」语义无关；运行中代理是平铺 `ul.daemon-agent-list`（`daemon-section.js:301` `renderAgents`），一行塞 workspace 全路径+session 全 ID+state+Stop 按钮，多工作区多 runtime 时不可扫读；pair 区带 QR canvas（`pair-device-section.js:74`），但客户端 app 未发布，QR 是死控件。

## 3. 设计

### 3.1 导航与 General 瘦身

- 导航在 general 后插入 `data-settings-tab="daemon"` / `data-settings-panel="daemon"`，label 用既有键 `settings.daemon.title`（en.json:756，四 locale 已有，无新导航键）。
- General 留 language / updater / pi-path 三段；daemon 段与 remote 段整体迁入新 panel。
- 双 shell 路由键迁移：`app.js:7358` 与 `landing.js:490` 的 `if (targetTabKey === "general") daemonSection.activate()` 改判 `"daemon"`——daemon 段的 5s 轮询生命周期绑定在新页签的开关上，语义不变（T10 契约：轮询只在面板可见时进行）。

### 3.2 页签内容（自上而下）

1. **状态行**：`settings.daemon.status` + 状态值（running/stopped/unknown + 原因），现 `#setting-daemon-status-row` 原样迁移。
2. **keepRunning toggle**：现 `#setting-daemon-keep-running` 行原样迁移，note 文案逻辑不动。
3. **运行中代理 master-detail**：替换 `ul.daemon-agent-list` 平铺。
   - master：`pkg-manager-layout` / `pkg-manager-groups` 结构（先例：extensions `index.html:1983`、mcp `:2211`）。按 `cwd` 分组，组头=工作区路径（长路径 wrap，`overflow-wrap: anywhere`）；组内每行=`pkg-manager-sidebar-row daemon-runtime-row`，内容 `session · <短ID>` + 状态徽章（tone 沿用 data-tone: ok/error/无），选中态 `is-selected` + `aria-pressed`。session 短 ID = 全 ID 截断前 8 位（预览口径 `s-8bd12a`）。
   - detail：`pkg-manager-detail` + `pkg-manager-status-grid`，五键值行 Runtime ID / Session ID / Workspace / State / CWD，全部可 wrap；底部 Stop 按钮（危险样式）。空列表时 master 显示 `agentsEmpty` 文案、detail 显示「Select a runtime…」占位（预览两个空态都已建模）。
   - 状态徽章文案沿用 `settings.daemon.agentState.*` 八态字典，不发明新状态。
4. **重启按钮 + modal**：现 `#btn-daemon-restart` 行迁移；modal（body 级 `#daemon-restart-confirm`）位置、样式、Escape/busy 守卫全部不动。

### 3.3 pair 区去 QR，改单栏

`#setting-remote-section` 迁入 daemon 页签，`#pair-device-offer` 状态改单栏：

- 状态行（relay state 三态）不变；
- 删 QR canvas（`#pair-device-qr`）及 `pair-device-section.js` 的渲染分支（:142-158 一带）；
- URL 只读输入框（`#pair-device-url`，readonly）保留为唯一链接呈现；
- 过期标注（`pairDevice.expired`，7115f66 已有）保留在 URL 下方；
- 按钮组：Copy link / New code / Turn off（现文案键复用）；
- 底部警示行（`pair-device-warning`）保留；
- `requiresClient` 文案改写：en 从「Scanning needs a Picot client app, which is not available yet. The code and link below are for when it ships.」改为「You need the Picot client app to open this link.」（其余三 locale 对应改写，zh：「需要客户端 app 才能打开此链接。」）。QR 已删，「scanning/code」措辞不再成立。

### 3.4 停止确认卡内嵌 detail 面板（调整点②）

现 `#daemon-stop-confirm`（`index.html:1309`）是 daemon section 内独立 settings-row，全列表共享。改版后迁为 detail 面板内 `daemon-confirm` 块（预览 `#working-stop-confirm`）：只在选中 runtime 为 working 时随 Stop 点击展开。**逻辑零改动**：`requestStop`（daemon-section.js:314）working 先确认、其余直停；`stopAgent` 的 triple 参数（workspaceId/sessionId/instanceId）、busy/stopping 守卫、停止后重读 status 的非乐观更新全部保持。

## 4. 与预览的对照（预览为视觉真源）

`docs/superpowers/specs/prototypes/2026-10-19-daemon-tab-prototype.html` 是静态 HTML，直接引用生产样式表（`../public/style-theme.css` + `../public/style.css`），新增样式集中在文件头 `<style>` 块（`daemon-runtime-row` 布局 + dialog 外观），共 3 条规则：

| 预览区块 | 对应实施落点 | 说明 |
| --- | --- | --- |
| 导航 Daemon 项（general 后第 2 位） | `index.html` nav + 测试断言 | 键复用 `settings.daemon.title` |
| 状态行 / keepRunning toggle | 现 DOM 原样迁移 | 行为不动 |
| master 分组列表 + 选中态 | 新 markup + `renderAgents` 重写 | 按工作区分组 |
| detail 五键值行 + Stop | 新 markup | pkg-manager-status-grid |
| working 停止确认卡（detail 内嵌） | `#daemon-stop-confirm` 迁移 | 逻辑不动 |
| Restart + dialog | 81fb79b 原样保留 | modal 不重画 |
| pair 单栏（URL 框/过期/三按钮/警示） | `#pair-device-offer` 改造 | 删 QR |

预览另含两个演示开关（Show empty state、preview-feedback 状态行）与「Static preview」水印——**这些是评审脚手架，不进实施**。实施版的选中态切换、空态、确认卡显隐行为须与预览一致（验收 §7）。

## 5. 三个调整点（全部入实施范围）

1. **断言重写 + 双 shell 路由迁移**：`settings-pages-layout.test.js:116`「daemon/remote 段在 General 末尾」与 `:150` 导航序断言（现锁定十项无 daemon）两条都要按新结构重写——第一条改为「daemon 段在 daemon 面板、General 只剩三段」，第二条插入 `daemon` 于 `general` 后；`app.js:7358` / `landing.js:490` 的键判从 `"general"` 改 `"daemon"`。
2. **停止确认卡迁 detail 面板**：见 §3.4，确认交互逻辑不动。
3. **requiresClient 改写保留**：见 §3.3，四 locale 同步。

## 6. 实施清单

| 落点 | 工作量 | 内容 |
| --- | --- | --- |
| `public/index.html` | ~150 行 | 新 nav 项+新 panel；daemon/remote 两段迁出 General；master-detail 骨架；pair 区去 QR 改单栏 |
| `public/settings/daemon-section.js` | ~120 行 | `renderAgents` 重写为分组 master-detail + 选中态 + detail 渲染；停止确认卡改挂 detail；其余（轮询/restart 事务/busy 守卫）不动 |
| `public/settings/pair-device-section.js` | −30 行 | 删 QR 渲染分支 |
| `public/style.css` | ~30 行 | `daemon-runtime-row` 三条规则（从预览 `<style>` 块收编） |
| `public/locales/{en,es,ja,zh}.json` | 4×2 行 | requiresClient 改写 + 可能的 detail 键值行 label 键 |
| 测试 3 文件 |  | `settings-pages-layout.test.js`（两条断言重写）、`daemon-section.test.js`（master-detail+确认卡新挂点）、`pair-device-section.test.js`（去 QR 断言，:285 的 requiresClient 查询同步） |

顺序：先 markup+样式（预览即蓝本）→ renderAgents 重写 → 双 shell 路由 → pair 去 QR → 测试。每步跑对应 vitest 文件保持绿。

## 7. 验收标准

1. 导航序 = general, daemon, appearance, …（测试锁定）；General 面板仅 language/updater/pi-path 三段（测试锁定）。
2. 双 shell（app + landing）daemon 页签开关正确驱动轮询起停；无 Tauri IPC 环境静默不报错（C10②行为保持）。
3. master-detail：按 cwd 分组、组头完整路径、行=短 ID+徽章、点选切换 `is-selected`/`aria-pressed` 且 detail 五行刷新；空列表显示两个空态占位。
4. 停止：非 working 直停；working 弹 detail 内嵌确认卡，Cancel/Stop anyway 语义与现版一致；busy/重启事务期间按钮禁用。
5. pair 区：无 QR canvas；URL 只读框+过期标注+三按钮+警示行；requiresClient 四 locale 为新文案。
6. 重启 modal：视觉与行为与 81fb79b 完全一致（Escape 取消、overlay 点击不关、焦点落 Cancel）。
7. **预览一致性：实施版的可交互行为（选中切换、空态切换、确认卡展开、modal 开关、toggle）与 `docs/superpowers/specs/prototypes/2026-10-19-daemon-tab-prototype.html` 一致**；视觉 token 全部来自既有 style.css 变量，预览 `<style>` 块仅 `daemon-runtime-row` 三条规则收编进生产样式。
8. 全量 `bun test`（或既有测试命令）绿。

## 8. 非目标

- General 不合并其他页签、不重排剩余三段。
- 不发明新视觉 token / 新组件库；pkg-manager 结构复用即止。
- 重启 modal 不重画（81fb79b 刚定稿）。
- 不动后端：四个 daemon IPC、轮询契约、restart 事务、stopAgent triple 语义零改动。
- 不做 pair 区新功能（客户端 app 交互、设备管理留 Phase 2.5）。
- 预览的演示开关（Show empty state / feedback 行 / Static preview 水印）不进实施。
