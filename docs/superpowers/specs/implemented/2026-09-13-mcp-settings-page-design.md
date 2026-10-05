# MCP 设置页设计：原生配置与项目覆盖

日期：2026-09-13；修订：2026-10-05。

## 状态与范围

原 MCP 设置页已实现。当前配置面为 Pi 原生 MCP：全局/当前项目两个页签、原生配置 CRUD、旧配置显式迁移。宿主 CLI 状态与 OAuth 入口已接线，并已按 Pi 1.0.2 协议修正（见 §1.2）。本次修订取代旧 adapter 六层架构描述。

Dr. Lin 已确认的新增范围（项目一键批量导入三个有效值的快照、项目 override 专用 detail）已实现；2026-10-05 军师复核批准 Spec/Plan 后实施，实施证据见下方「实施状态与证据」。本文件仍是 MCP 设置页唯一设计 Spec。

OAuth 的进程与凭据契约见 [MCP OAuth 登录设计](../2026-10-02-mcp-oauth-login-design.md)。实施计划见 [项目 MCP overrides plan](../../plans/2026-10-05-mcp-project-overrides.md)。本修订不授权提交、建分支、安装依赖或修改升级 pin/fixtures。

### 实施状态与证据（2026-10-05）

已实现（2026-10-05 验收复审后的状态，按代码实际能力陈述）：

- 后端与门禁：`extensions/mcp-native-config.ts`（严格原生 parity、override 分类、有效三值快照）、`extensions/mcp-settings.ts`（bytes revision、锁内重读并在锁内复核路径与 trust、批量导入、override 保存/toggle/delete、坏文档与坏条目拒绝、跨层 namespace 检查）、`extensions/picot-config.ts`（host 标记 + realpath 相等 + `ctx.isProjectTrusted()` + 最近 `trust.json`；agent root 与 Pi 一致，`PI_CODING_AGENT_DIR` 优先）。
- 宿主标记：`src-tauri/src/pi_launch.rs` 只在注册表验证的 primary launch 上发行 `PI_STUDIO_MCP_PROJECT_ROOT`（canonical cwd），`native_pi_manager.rs` 在 spawn 前清除继承值。
- 前端：`public/settings/mcp-override-detail.js`（三字段 detail，无连接/继承/OAuth 控件）、`mcp-page.js`（类型分流、导入按钮、渲染绑定 + 动作绑定 + `beforeSend`、状态按 name+scope+source+override 路径匹配、草稿随 revision 迁移、失效代数/请求序号）、`config-gateway.js`（MCP 专用同步 `beforeSend`）、`app.js`/`landing.js` 接线、四语言 locale（`public/i18n-keys-completeness.test.js` 覆盖键与占位符等价）。
- CLI 报告：`mcp_login_runner.rs` 接受退出 0/1 的 `{servers,errors,note?}` 并投影为安全字段（transport 经真实 URL 解析只给 `http`/`stdio`，错误与 note 固定文案、保留计数、不回显原值）；`host_server.rs` 返回 `{ok,servers,errors,note?}` 并支持 `refresh:true`；`public/app/transport.js` 转发该参数。
- 文档：`ARCHITECTURE.md` 的 MCP 段、项目覆盖段与项目信任段已同步。

验证证据（本轮命令与退出码）：focused vitest 套件（`mcp-native-config`/`mcp-settings`/`picot-config`/`mcp-page`/`mcp-override-detail`/`mcp-import-binding`/`mcp-project-import-backend`/`config-gateway`/`i18n-keys-completeness`）通过；`bun run build:extensions`、`bun run test`、`bun run check:rust` 见交付报告；真实内嵌 binary（1.0.2）隔离 stdio smoke（`scripts/mcp-project-overrides-smoke.js`）53 断言通过。

尚未验证 / 已知边界：

- **真 WebView 未跑**：本轮未启动 Picot app，布局、三字段交互、导入按钮、切工作区竞态与 OAuth UI 的端到端行为未在真实窗口验证；页面证据来自 jsdom 与真实 gateway/readiness/真实后端集成测试。
- 宿主级 `refresh` 权限准入测试、真实 CLI envelope → host → transport 的端到端集成测试仍缺；既有 owner/generation 准入块未改动且被全部 MCP op 共用。
- 完整项旧兼容：Pi 原生读取严格 JSON 且只认 `mcpServers`；Picot 编辑表单仍容忍 JSONC、`mcp-servers` 键与 command 数组，这类条目只给诊断，不参与导入候选或 override base（见 §1.2）。
- CLI/外部编辑器不遵守本锁，比对到 rename 之间仍有竞态；父目录替换与跨平台行为未做平台实测。
- `refresh:true` 只影响宿主缓存失效，不代表当前会话已 reload；运行中的 Picot app 是否已升级到 1.0.2 内嵌产物未验证（`PI_BIN` 覆盖路径亦未验证）。
- 测试隔离事故（处置中，未结案）：修复 agent root 期间，共享 resolver 的 `PI_CODING_AGENT_DIR` 优先级变化叠加未隔离的写盘测试，真实 `~/.pi/agent` 下 `models.json`、`AGENTS.md` 的 symlink **目标文件**曾被写入；旧报告「仅备份受损、symlink 未受影响」的结论错误。此后测试改走 `scripts/test-sandbox.js` 入口并隔离 HOME/agent root。缺事故前完整字节基线，受影响全集与恢复完整性均未证实，任何恢复动作须 Dr. Lin 授权；根因核查见 2026-10-04 事故根因报告。

## 1. 当前架构与证据

| 模块 | 当前职责 | 源码依据 |
| --- | --- | --- |
| `extensions/mcp-settings.ts` | 全局 `<agentDir>/mcp.json`、项目 `<cwd>/.pi/mcp.json` 的 inventory/CRUD；检测 adapter/shared 迁移源 | `:135-246,414-547` |
| `extensions/picot-config.ts` / `picot-bridge.ts` | `/picot-config` 配置桥，`__picotConfig` notify 按请求 id 回关 | `picot-config.ts:1631-1648`；`picot-bridge.ts:71-114` |
| `public/settings/mcp-page.js` | 两页签 master/detail、配置表单、迁移横幅、状态与 OAuth 对话框入口 | `:32-64,218-331,504-794` |
| `src-tauri/src/mcp_login_runner.rs` | spawn 内嵌 `pi mcp login/logout/list --json`，owner-bound 登录生命周期、60 秒缓存；list 输出按固定 schema 校验，只投影安全字段（transport 走真实 URL 解析而非前缀启发） | `:39-57,108-150,350-386` |
| `src-tauri/src/host_server.rs` | 已认证 desktop、Registered workspace/generation 门禁，转发 MCP host ops | `:2642-2739` |
| `public/landing/landing-config-runtime.js` | 无工作区时懒派生 sessionless/toolless 配置 runtime，配置桥可用 | `:19-105`；`host_ephemeral.rs:465-475` |

原生 MCP 随 Pi 提供，页面不再依赖 adapter 包检测。全局路径尊重 `PI_CODING_AGENT_DIR`。普通配置变更需新会话或 reload；保存成功不代表当前会话连接已变更。页面不新建同步或热重载系统。

### 1.1 当前行为中保留的部分

- 全局/项目 master 列表、每页签记忆选择；成功加载或切页后，默认选第一条。列表下方虚线“添加一个 MCP”按钮沿用 `.models-provider-add`。
- 完整服务器 detail：启用开关、名字、源文件路径；现有 transport/command/url/args/env/headers/exposure 表单。Save/Delete 共用底部 `.mcp-form-actions`。不新增 cwd、timeout、description 或 OAuth 参数输入。
- 完整项开关即时写 defining file 的 enabled；启用删除 enabled 默认键。exposure 为 codemode 时完整项沿用省略默认键规则。未知已有字段与无关顶层配置保留。
- `${VAR}`、`!command` 为配置文字，Picot 不解析、不执行、不取密钥。command 数组在旧表单未改时原样 round-trip，修改后按现有规则处理。
- 所有 gateway reject 经 `call()` 转为 `{ok:false,error}`，既有状态行显示错误；不产生未处理 rejection。
- 宿主状态查询失败降级配置显示；不轮询整个页面。OAuth 对话框保持授权 URL、打开浏览器、取消、失败/重试、成功刷新；不做自有 OAuth 协议。

### 1.2 当前兼容性差异，不能写成 Pi 能力

Picot 现有 `readMcpLayer` 容忍 JSONC、`mcp-servers` key；Pi 1.0.2 原生读取严格 JSON，只认 `mcpServers`。Picot 现有保存接受 command 数组，Pi 原生只接受 executable 字符串。本期保留完整项现有数组读写/编辑契约与测试；严格排除数组作为导入候选和 override base，报告具体无效原因，不新增转换/迁移功能。收紧完整项数组兼容是独立待决项，须 Dr. Lin 另行授权。

读取纯 override 不调用 normalizeEntry，不会因为缺 transport 抛保存校验错误；现有保存最终仍要求 command/url。原生 eligibility 诊断不能将已有完整项强制换成只读/禁保存页，避免以新增 metadata 改掉旧编辑契约。

依据：Picot `mcp-settings.ts:46-122,196-203,312-315,408-427`；上游 `packages/coding-agent/src/extensions/mcp/config.ts:95-107`、`core/mcp-servers.ts:265-277`。普通完整表单的兼容 round-trip 不在本次改造成另一个 GUI；inventory 必须暴露原生不生效诊断，批量导入不能把这些条目当有效全局 base。

状态协议曾有已核实的断裂（已修复，2026-10-05）：Pi 1.0.2 `mcp list --json` 输出 `{servers,errors,note?}`，配置错误、needs-auth 或部分连接失败仍输出报告并退出 1（上游 `cli.ts:478-488`）。旧 runner 先拒非零退出，再只认顶层数组；退出 0 的真实 envelope 同样被拒，且既有 fake binary 用数组/`[]`，绿测不能证明兼容。现 runner 接受退出 0/1 的有效报告（其余退出码、信号、坏 JSON、缺字段一律查询错误且不入缓存），夹具换成真实协议形状，登录/缓存断言保留；返回前投影为安全字段，见 §6。

CLI 报告不是配置导出：disabled 分支不返回 toolExposure；启用时只返回已发现工具中与服务器 exposure 不同的有效结果（上游 `cli.ts:444-468`）。磁盘/backend 验三键与原始 map，真实 CLI 验身份、enabled/exposure、继承 transport。整 map 替换、`{}` 清除和规则优先级另用隔离本机 stdio 工具验证，不从 disabled 报告虚构 map。

## 2. 保留迁移契约

旧配置只作一次性、用户显式确认的复制来源，不参与原生生效合并：

| 迁移 id | 来源 | 目标 |
| --- | --- | --- |
| adapterGlobal | `<agentDir>/mcp-adapter.json` | `<agentDir>/mcp.json` |
| adapterProject | `<cwd>/.pi/mcp-adapter.json` | `<cwd>/.pi/mcp.json` |
| sharedGlobal | `~/.config/mcp/mcp.json`、`~/.agents/mcp.json`、`~/.agents/mcp/mcp.json`，按既有后者胜出顺序 | 全局原生文件 |
| sharedProject | `<cwd>/.mcp.json` | 项目原生文件 |

`mcp_list_servers` 仅检测缺项，不写盘。迁移横幅在 master/detail 外，显示完整源路径与缺项数；点击后执行 `mcp_migrate_adapter_config {target}`，源文件保留，已有同名目标不替换。映射 `disabled:true→enabled:false`、`directTools:true→exposure:direct`；移除 adapter 专有 directTools/inheritEnv/lifecycle/disabled，丢失 inheritEnv/lifecycle 在 lossy 返回。legacy 来源继续容忍 JSONC/key variant。

新项目门禁与坏目标文件拒写同样覆盖项目迁移，不能靠迁移绕过。保持已知字段映射与返回 `{migrated,skipped,lossy}`，不把本次三字段导入混成 adapter 迁移。全局→项目新导入不复制连接或凭据；legacy 配置迁移仍是既有用户确认操作。

## 3. Pi 项目覆盖语义

基准：`/Users/linyong/tmp/PI/pi`，`v1.0.2-1-g200387122`，所查 MCP 配置文件与 v1.0.2 无差异。

- 项目完整定义按精确同名整项替换全局。
- override 候选必须是项目层非 null/非数组对象，且 command/url/type 均为 undefined。仅凭空 command 或缺 command/url 不足以判断。
- 合法 override 仅含 enabled/exposure/toolExposure，必须有经原生校验与冲突处理后有效的精确同名全局 base，合并 `{...base.config,...override}` 后校验通过。
- `toolExposure` 整 map 替换；空 map 清除全局单工具规则，工具回退到当前服务器 exposure。精确工具名优先，通配规则按对象顺序匹配；不能排序或逐工具 merge。
- exposure 为 codemode/direct/deferred/hidden；旧 codemode-deferred 归一到 codemode。enabled 缺省 true，exposure 缺省 codemode，toolExposure 缺省 `{}`。
- 精确 key 匹配区分大小写。`dev-tools` 和 `dev_tools` namespace 冲突，但不是可互换引用；不同名冲突项被跳过。不得自动改名。
- 项目完整 HTTP 定义禁止 auth.provider；合法 override 可以继承全局 auth.provider。项目不能借此更换 token 发送目的地。

依据：上游 `extensions/mcp/config.ts:74-79,107-149`；`core/mcp-servers.ts:17-22,102-121,206-215,223-277`。

## 4. 已确认新增交互（待实施）

### 4.1 一键批量导入

只在“当前项目” master 底部现有添加按钮旁/下方增加“导入全局设置”，复用现有按钮风格。点击立即批量写盘，无服务器选择、向导或 detail 预填步骤。按钮忙碌时禁重复提交。详情调参仍用原有保存动作。

导入当前全局原生文件的所有有效服务器，含停用项，不含 extension 注册项；按服务器精确原名创建项目 override，仅复制三个有效值，全部显式保存：

```json
{
  "mcpServers": {
    "context7": { "enabled": true, "exposure": "codemode", "toolExposure": {} },
    "internal-tools": {
      "enabled": false,
      "exposure": "direct",
      "toolExposure": { "delete_*": "hidden" }
    }
  }
}
```

不创建空 override，不复制 command/url/env/headers/oauth/auth/token store，也不复制顶层 autoEnableCodemode。三字段是独立快照：全局以后改这三个字段不跟随；连接/auth 等其他配置仍在下一次加载时继承全局。全局增项需再次导入补缺，没有自动同步。

已有精确同名项目项原样跳过，含完整定义、override、停用或非法项。不同名 namespace 冲突跳过并报告；全局非法项也跳过。重复导入幂等，不刷新既有快照。一次重读、合并、原子写盘；无新增则不写。全局或项目文件整体损坏拒绝整批，不把坏文件当空配置。反馈“新增 N、已有 M、冲突/无效 K”，异常附名称与原因；部分导入不报全部成功。

### 4.2 detail 按类型分流

完整定义继续 §1.1 既有 UI。override 仅有名字、基础/项目文件来源提示、以下三字段及保存/移除操作，不显示任何连接参数或完整只读连接表，不增加 OAuth 按钮。

| 字段 | 控件与行为 |
| --- | --- |
| enabled | 同全局视觉的二态 `.pkg-manager-toggle`，启用/停用，不含“继承”选项；即时保存该字段。override 启用写 true，不删除键。保留其他已保存字段，不能吞掉 exposure/map 未保存草稿 |
| exposure | 现有 select，codemode/direct/deferred/hidden；显式 codemode 保留键；底部 Save 写盘 |
| toolExposure | 标准 JSON object textarea，默认显示有效 map；底部 Save 写整 map，`{}` 明确清空。无“继承”下拉；语法/类型/枚举错误显示在现有状态行并保留草稿 |

外部已有部分 override/`{}` 合法时，按 base+override 的有效三值显示。Save materialize 三个显式值；enabled 即时开关只改 enabled，不强行 materialize 尚未保存的其他字段。这样保留全局开关惯例，也不引入自动保存系统。用户若不再需要局部快照，移除整项；GUI 不提供单字段恢复继承控件。

override 缺 base、非法字段/额外键、类型错误或 namespace 冲突：显示具体错误，不伪装正常继承；禁开关与保存，允许移除项目原项。base 消失不补连接定义，不转为完整服务器。完整项独立保留既有编辑入口，并显示 native eligibility 诊断；非对象等无法形成表单的坏条目只提供错误与移除。

### 4.3 移除与状态

override 删除文案“移除项目覆盖”，说明“只从当前项目 mcp.json 移除；恢复全局设置，不等于停用”。完整项目项维持“删除 MCP”，若有合法同名全局项，下次加载恢复它。base 已消失时移除仍可执行。

Pi override 的运行身份为有效 global scope/source，加项目 override 路径（上游 `config.ts:118`；`cli.ts:446-450`）。master 状态只接受该身份与当前项目路径的匹配；不能按 activeTab=project 丢掉状态，也不能将任意同名全局报告当成项目覆盖已生效。未报告/缓存过期显示未知；不从未知状态猜项目信任。

OAuth 保持 [登录 Spec](../2026-10-02-mcp-oauth-login-design.md) 的 CLI/token/取消/缓存契约。override detail 无登录/登出按钮；需认证提示用户回全局页，仍以有效服务器 name+URL 登录，不复制认证。当前 host ops 在 landing 缺 Registered workspace 时拒绝，配置页仍可使用全局配置桥并显示状态不可用。

状态 runner 接受退出 0 或 1 且形状有效的 `{servers,errors,note?}`。退出 1 表示可用报告含失败，不等于宿主查询失败；页面保留成功行、展示 needs-auth/失败行及配置诊断。坏 JSON、缺必需数组/字段、其他退出码、信号终止或 spawn 失败返回查询错误，不缓存伪报告。host 返回 `{ok:true,servers,errors,note?}`，保留 source/override 身份；errors/note 置于页面级状态提示，与配置保存结果分开。原始 errors、server.error、stderr 可含 URL/凭据/子进程输出，不传 WebView、不记日志；宿主以固定安全文案表达诊断数量/服务器失败和未信任 note。transport 只返回 http/stdio 类型标签，不透传 URL 或命令（含路径/args）；完整项连接表单仍读自身配置，override 无需连接详情。状态字段按已知非密字段投影，不能继续“任意 CLI 字段原样透传”。身份路径/名称用 textContent，非密计数与有效工具 exposure 可保留。

配置 import/save/toggle/remove 成功后，先递增页面状态失效代数，使已有查询失效，再清状态与诊断、刷新 inventory，并以 `mcp_server_status {refresh:true}` 使宿主缓存失效再查询。每次查询捕获 context key、失效代数和递增请求序号；仅当前 context、当前代数且最新请求可发布成功或失败。工作区切换/页面销毁同样使查询失效。默认仍保持 60 秒 TTL，不新增轮询。runner epoch 只防旧结果重入缓存（`mcp_login_runner.rs:370-386`），不能阻止旧响应返回页面；仅比较 context key 也挡不住同工作区 A 查询晚于刷新 B 返回。旧成功不得恢复 connected，旧失败不得清掉 B 的状态或诊断。查询失败不回滚已成功配置写盘；状态是 CLI 磁盘视图，不代表当前会话已 reload。

## 5. 授权、校验与写盘（待实施）

现有 MCP CRUD 只看 ctx.cwd，没有 trust 判断（`picot-config.ts:352-354,1631-1648`）。Landing 实际 cwd 是 `~/.pi/tmp`，所以“cwd 非空/非 home”不等于真实工作区。OAuth UI 未报告提示也不是文件写权限闸口。

保持 `/picot-config` 数据路径。宿主只给经注册表验证的主 runtime 提供 canonical MCP 项目根标记；Config/Quick/Side runtime 清除该标记。桥端将标记与 realpath(ctx.cwd) 全等比较，核对 Pi `ctx.isProjectTrusted()`，并在锁内重读 trust.json 最近显式决定；缺标记、根不符、未信任一律拒项目读写。标记不接受 WebView params 传入。已有 host runtime owner/generation 准入继续逐帧复验；前端工作区变更清空旧列表/草稿，不能带旧 revision 写新项目。

项目操作还须绑定发起动作时的 RuntimeTarget，不只绑定响应 context。现有 `ConfigGateway.call` 在 readiness 完成后的微任务才取当前 target（`public/settings/config-gateway.js:63-78`；实际接线 `public/app.js:602-618`）：A 已 ready，点击导入后、发送前切 B，旧 `{}` 可发到 B。B 的宿主准入只能验证 B 合法，不能识别 A 的用户意图；丢旧响应和文件 revision 不能撤销错发导入。已完成 Promise 也不能由旧 waiter 拒绝补救。

工作区 MCP 页面通过 shell 回调取实际 routing triple，在用户动作同步阶段复制、冻结 `{workspaceId,sessionId,instanceId?}`，连同 context key（含 owner generation）形成单次操作 binding。项目 import、完整定义/override save/delete/toggle、项目 legacy migration 必须使用 `ConfigGateway.call(op,params,{target,beforeSend})`；缺 target 或旧表单 binding 立即拒绝。inventory 与后续 reload 在工作区也绑定各自发起时的 target，不能沿用旧操作重新取新工作区发送。readiness 后、实际 runtime.request 前，同步 beforeSend 复验页面存活、context 与完整 routing triple；不符则拒绝，不发请求，不回退新 target、不重试。钩子在 gateway 内仅为本 MCP 边界增加，默认调用不变；不改 wire、readiness 框架或 landing lazy runtime。Landing 不提供项目 binding，保留全局配置代理及项目禁用。

发送前检查到 runtime.request 间不插 await/微任务。已经发送后发生切换无法靠前端取消回滚：只可能仍指向原 target，由既有 host owner/generation 准入裁决，绝不能重定址新工作区。响应仍按 context/请求代数丢弃，继续遵守 §4.3。验收使用真实 ConfigGateway＋createConfigReadiness：A 已 ready 后发送微任务前切 B、A gate 等待时切 B、页面销毁、同工作区换 session/instance，以及缺 target；断言不向 B 发配置请求，带实际 backend 的隔离临时 fixture 证明 B 文件 bytes 不变。仅 page gateway mock 的延迟响应测试不足以覆盖此边界。

项目路径限定 `<canonicalRoot>/.pi/mcp.json`；拒绝 `.pi` 或目标文件 symlink 导致实际路径逃出项目根。读取仅 ENOENT 算不存在，EACCES/结构错误等报告并拒写。不使用字符串 startsWith 判断路径归属。

所有 MCP 同目标写入复用现有 `withSettingsLock` 协议，在锁内重读、校验、合并、原子 rename；保持 0600、2 空格与尾换行，无关字段原样保留。list 返回文件 bytes 的非密 revision；新建要求 expectedRevision 与目标不存在，编辑/删除/toggle 要求预期 revision 与类型不变。override 保存另复验全局 revision/base，防止外部将目标换成完整项时覆写。CLI/外部编辑器不遵守锁时仍有最后比对→rename 竞态，验收必须注明，不能宣称完全事务隔离。

完整项保存保留现有 UI 字段与数组兼容范围；补项目 auth.provider 禁令、跨层 namespace 与坏文件/防覆盖边界，不借此改掉已有 command 数组编辑测试。严格原生校验只用于新导入、有效 base 与 override 合并结果。新增 override 分支严格三字段白名单，拒绝隐藏传入连接字段，保存覆盖三字段而不是 merge 旧快照；不机械取消完整项 command/url 校验。

Pi validator/loadMcpConfig 未从 public SDK 导出（上游 `src/index.ts:409-410` 只导出 types/工厂）。不能私有深导入。实现使用 MCP 专用的窄 parity 校验模块，按上游配置规则测试；真实内嵌 binary 隔离临时配置验收作最终语义证据，不安装上游依赖或增第二套配置 authority。

## 6. 配置桥契约（待实施的增量）

保留 `mcp_list_servers/save_server/delete_server/toggle_server/migrate_adapter_config`；新增 `mcp_import_global_overrides`，无客户端文件路径与 entries 参数。

- list：保留 groups/groupErrors/migrations；补 projectAvailable（真实身份）、projectTrusted、各文件 revision；条目 kind=`definition|override|invalid`、validationError、effective 三值、有效 source/scope/override identity。definition 的 native 校验错误仅作诊断，不收紧既有表单契约；invalid 用于坏条目/非法 override。override 不向 detail 返回全局连接/凭据。
- import：返回 `{imported:string[], skipped:{name,reason,conflictWith?}[], changed:boolean, path:string, revision:string}`。reason=`existing|namespace-conflict|invalid-global`；整体文件错误走 `{ok:false,error}`。
- save：增加 `kind`、`expectedRevision`；override 为 project-only，entry 必须显式三个值，另带 `expectedGlobalRevision`。definition create/edit 区分 intent，防止新建覆写同名。返回新 revision 与 runtimeReloadRequired=true。
- toggle：override 写显式 enabled，并保留其他原始合法覆盖键；支持外部部分 override。仍受 base/全局与目标 revision 校验；完整项默认删除 enabled 规则不变。
- delete：增加 expectedRevision，移除孤立 override 不要求 base 存在；已不存在返回 changed=false，不删除全局。

详细类型、文件责任与红绿步骤以关联 Plan 为执行依据。无 generic settings framework、同步名册或新 HTTP route。

## 7. 验收

1. 一键导入包含停用项与缺省 true/codemode/{}，项目输出仅三键；原全局 bytes 与密钥不变。已有同名完整/override/非法项不变，重复无写入；冲突反馈可读。
2. 完整项 UI 不扩字段，CRUD/transport/默认值/选择/OAuth 回归通过。override DOM 无连接控件、OAuth 控件或“继承”选项；部分 override 按有效值显示，Save 变显式三值，开关 true 与 codemode 不被删键。
3. 整 map 替换、空 map、键顺序/通配优先与 Pi 一致；错误输入保留草稿。base 移除禁保存但可移除，移除恢复全局并提示不等于停用。
4. 全局+override 路径状态正确；真实 envelope 退出 0/1 均可展示，坏 JSON/异常退出拒绝，errors/note 安全显示。同 context 的旧成功/失败均不能覆盖刷新结果；无状态不当作不信任，登录/登出/取消/redaction 不回退。
5. 无工作区、scratch runtime、未信任、cwd 标记不符、stale generation/revision、坏文件、symlink 越界、隐藏字段写入均拒绝；拒绝前后目标 bytes 不变。真实 ConfigGateway/readiness 接线验证项目操作绑定发起 target，发送前切工作区/换 runtime/销毁不向新目标发送，B 文件 bytes 不变；已发送旧 target 的 host 拒绝测试保留。
6. Bun focused tests→build:extensions→check→全量 test；有 Rust 改动再 check:rust，串行。真实 embedded Pi 临时 HOME/project：disabled CLI 验身份、enabled/exposure、继承 transport 与丢 base 错误；磁盘/backend 验三键/原始 map；无外部网络的确定性 stdio fixture 验实际工具 exposure、整 map 替换、空 map 与精确/通配优先。真 WebView 检查布局/导入/三字段/删除/同工作区刷新竞态/切工作区。未跑 GUI 明示未验证。

版本前置为 Pi >=1.0.1。2026-10-05 本轮只读检查：他方 working pin 已是 1.0.2；`src-tauri/resources/pi/pi --version` 与 `.version` 均为 1.0.2。这只证明当前 checkout 内嵌产物版本，不证明正在运行的 Picot app 或 Windows/Linux 产物已升级；实施前重验实际 resolver 路径和进程身份。本任务不碰 pin、fixtures 或升级影响评估。

实施交付时必须同步 `ARCHITECTURE.md:274` 的旧 adapter 段，写入两层原生、批量三字段快照、项目资格、override 状态与 OAuth 链接；同步 `:46-50,340-346` 相关不变量，但不得借此扩大其他设置范围。本轮只更新 Spec/Plan，ARCHITECTURE 的实施同步列为后续任务。

## 不在范围内

复制全局连接配置/凭据、逐项导入向导、继承下拉、完整只读连接表、更多完整项 GUI 字段、自动同步/热重载、自实现 OAuth、修改 Pi 私有 API、runtime 注册服务器导入、整批替换项目配置。
