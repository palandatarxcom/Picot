# Subagents 设置页设计

**状态：** 已实现，2026-10-03 按 Dr. Lin 确认归档。当前交付范围与限制见 §0；后文保留设计和实施阶段记录，不表示其中每个预期场景均已验收。

**日期：** 2026-09-30
**原型：** [`../prototypes/2026-09-30-subagent-settings-prototype.html`](../prototypes/2026-09-30-subagent-settings-prototype.html)，离线模拟，非运行证据。

**本地实施计划：** `docs/superpowers/plans/2026-09-30-subagent-settings.md`，保留原任务清单与验证缺口；该目录由本地 Git exclude 忽略，不属于仓库交付。最终记录已写入本 spec。
**架构契约：** [`ARCHITECTURE.md — Settings → Subagents`](../../../../ARCHITECTURE.md#settings--subagents候选盘点与受限名字级覆盖)。

## 0. 最终实现与归档说明

本节记录 2026-10-03 的实际交付；与后文初始设计冲突时，以本节和架构契约为准。

### 页面与交互

- 一级页签为「全局 / 当前项目」，默认全局；landing 只显示全局。页签表示文件归属和覆盖写入目标，不改变 `/run` 的 `both` 发现范围。
- 每个作用域先显示描述文字，再显示「自定义 / 扩展包」分段按钮，之后是计数行与 master/detail。「自定义」显示 `{n} 个子代理`；「扩展包」显示 `{n} 个子代理 · {m} 个扩展包`。`n` 为当前浏览分类的候选数；扩展包视图含 builtin，`m` 只按 package 来源的 `packageIdentity` 去重，不将 builtin 组另算一个包。
- 分段按钮复用技能页的 `.skills-scope-tabs` / `.skills-scope-tab`；描述与计数复用 `.settings-help`，不用单独的字体、颜色或间距规则。扩展包按包身份分组，builtin 单列只读定义组。
- 子页签切换只投影当前 inventory，不重发 host 请求；作用域切换重置到「自定义」。选中列表条目后保持 master 滚动位置。
- 启用开关放在 detail 的代理名称行右端，点击即写 `disabled`，不保存或丢弃 model/thinking/advertise 草稿。没有本层 `disabled: true` 时开关显示启用；此状态不是 live runner 状态。
- 模型控件复用 rpiv-advisor 的 `loadModelChoices` / `appendModelOptions`，使用原生 `select`，首项为「不设置（继承父代理）」，随后为范围模型与全部已启用模型。目录加载经 `ConfigGateway`，landing 可按需派生全局配置会话；目录不可用时退为经校验的模型 ID 输入，不提供模拟选项。已有 `inherit` 或目录外 ID 保留为当前值。
- thinking 支持未设置、JSON `false` 与 `off/minimal/low/medium/high/xhigh/max`；catalog 不提供逐模型档位能力，页面不承诺所选档位必定生效。advertise 提供未设置、true、false。
- 原始定义限量只读展示，不拼接 YAML；被遮蔽条目不跳转。移除了常驻「仅显示磁盘候选」横幅和页底范围外来源列表；候选状态、逐条禁写原因、错误及保存反馈仍保留。范围外来源诊断留在 host 响应中，不读取其 prompt。

### 写入与安全边界

- 自定义、package、builtin 的覆盖统一写 `subagents.agentOverrides.<runtimeName>`，当前 UI 暴露 model、thinking、advertise、disabled 四字段；不修改 `.md`。写入层跟随一级页签，回显本层 `savedOverride`，不冒充合并后的生效值。
- 每字段使用 set/clear/keep；清空只删除本层字段，保留未知字段与其他代理设置。启用开关禁用时 set `disabled: true`，启用时 clear。revision 取目标层 `settingsRevisions[scope]`。
- 2026-10-02 已批准放宽覆盖资格：native 候选在本作用域快照内没有已知名字/alias 冲突、且范围内扫描完整时允许名字级保存。`writeQualified` 不证明 live winner；范围外未知占用不再阻断此类覆盖。external/未知 runner、被遮蔽或同层重名候选仍拒写，project root 分歧禁止项目写入。
- 四个专用 host op 为 `subagents_inventory`、`subagents_get_detail`、`subagents_set_override`、`subagents_create`。仅认证 desktop owner 可用；项目 op 校验 Registered owner、`workspaceId`、`workspaceGeneration`、canonical root 与项目信任。详情通过 host candidate ID 重扫读取，前端没有任意路径权限。
- 覆盖通过单锁 read–compare–modify–write、私有备份和原子替换落盘。保存只表示写盘成功，现有会话须 `/reload` 或新建会话，再用 `/subagents-models` / `/run` 核实。
- `.md` 创建表单与排他发布事务已实现，但生产模式仍是 `disk-candidates-only`，创建按钮及 host 创建门均拒写。归档不表示新建功能已开放。

### 证据与保留限制

实现位置：`src-tauri/src/subagents_inventory.rs`、`src-tauri/src/subagents_settings.rs`、`src-tauri/src/host_config.rs`、`public/settings/subagents-tab.js`；接线位于 `host_server.rs`、`public/app/transport.js`、`app.js`、`landing.js`。前端回归在 `subagents-tab.test.js`，host 回归与实现同模块。四语言文案覆盖 en/zh/ja/es。

收尾提交：`b22bd87`（分段按钮）、`a0f09bf`（描述与计数）、`d45af48`（计数样式）、`036377d`（描述样式漏改修复）。后两次样式修复须合看；`036377d` 的主标题属于 MCP，子代理的一行修复被并入该提交。`a0f09bf` 还携带 Skills CSS 改动，归档未拆分或重写提交历史。

本会话已有验证记录：前端聚焦测试 61/61；host 覆盖开放时 Subagents 聚焦 31/31、Rust 全量串行 576/576。后两项是实施时记录，不是此次文档整理重跑结果。WebView landing 走查确认分段类名、原生模型选择器、detail 开关、真实包列表与空态；该轮发生在描述文字加入之前，不证明最终描述样式或全套视觉验收。

保留限制：

1. parity spike 未证明 live `/run` 一致性；动态注册、额外扫描目录与包过滤仍不能当作已覆盖的 live inventory。全局请求不带项目身份，不掌握当前工作区的完整遮蔽关系；不显示推算生效值。
2. 新建仍关闭；provider 定向覆盖、per-run 参数和逐模型 thinking 能力不在此 UI 的验证范围。测试清单不等于每条设计矩阵已执行。
3. 未完成 Windows/Linux 路径与 hard-link 平台实测、全主题/窄窗/完整键盘及 locale WebView 走查。测试 harness 缺少新增文案键会产生 i18n 警告；真实四语言文件有键，但新增文案本身未由断言完整覆盖。
4. 不遵循 settings 锁的外部写者仍有最终比对到 rename 之间的竞态；父目录 symlink 竞态也保留为已知限制。host 包源还接受裸 `ssh://`，与此前核对的 pi-subagents 0.74 resolver 有差异。

此次文档整理重新运行 `bun run vitest run public/settings/subagents-tab.test.js`：61/61 通过，退出码 0；stderr 有 8 条 i18n 缺键警告，包含测试 stub 中缺少的 scope 描述、计数和模型分组键。未重跑 Rust 全量、`bun run check` 或 WebView。文档路径检查未发现新增断链，`git diff --check` 通过。

以下 §1–§7 为原设计、验证矩阵及阶段记录。原型保留供历史参考，不再要求它与生产 UI 同步，也不将其模拟行为作为验收证据。

## 1. 目标与边界

在 Picot Settings 新增 Subagents 页：左侧按「全局 / 当前项目」查看子代理定义，右侧只读展示纳入本页范围的 YAML frontmatter 与 prompt，按页签保存胜出名字的 model、thinking 覆盖；可在所选作用域创建新 `.md`。全局排在当前项目前并默认选中：公用 package、extension、sub-agent 通常安装在全局；项目级仍受支持。列出定义来源、在当前工作区的生效情况及同名遮蔽关系。执行方是已安装的 **Nico `pi-subagents`**，不是 Pi 内核自带的 agent 管理器；本页不启动、删除、编辑或安装子代理，不编辑 package `.md`，不接管 `/run` 或运行中的子代理。

两个页签首先是**文件归属与写入目标**，并非互斥的运行时视图。`/run` 使用当前会话 cwd 下 `agentScope: "both"`；全局设置可能影响项目代理，项目设置也可能覆盖全局定义。UI 不得把「只在该页签列出」译为「只在该页签生效」。原型的五条记录、模型选项、预览值、成功提示全是内存示例；刷新即消失，不可视为实现。

## 2. 核实依据与偏差

下列结论基于本机安装的 `~/.pi/agent/npm/node_modules/pi-subagents/package.json:4`（初稿核实于 **0.73.1**；2026-10-02 架构狮审查已按当前安装 **0.74.0** 复核 `/run` both 语义、来源优先级与包扫描的引用）、该包源码/文档、仓库代码与 Pi 随仓文档，不代表将来的兼容承诺。

| 依据 | 对本设计的影响 |
| --- | --- |
| `pi-subagents/src/agents/agents.js:216-248,304-314,320-392` | 包代理由包 `package.json` 中 `pi-subagents.agents` **或** `pi.subagents.agents` 声明；扫描项目/全局 npm、`settings.json` 的包源、项目根包，且可能扫描系统 npm root。目录不等于原型中示意的 `<cwd>/.pi/npm/example-review/agents`；实际 npm 路径含 `node_modules`，git/file 包路径又不同。列表须依据配置和 manifest，不递归把所有缓存 `.md` 当成已安装代理。 |
| `pi-subagents/src/agents/agents.js:567-631,2082-2095,2304-2350`；`docs/configuration.md:8-40` | 运行时项目根可能由最近 `.pi`/`.agents` 或 `projectRootResolution: "git-root"` 决定，不必等于 Picot 注册工作区 `<cwd>`。还可扫描递归 `.agents/`、额外 `agentScanDirs`、全局 `~/.agents/`、环境扫描根，并可用 `agentExcludeDirs` 排除；只扫 `<cwd>/.pi/agents/*.md` 会伪称列表等于 `/run`。 |
| `pi-subagents/src/agents/agents.js:1664-1672,1819-1840,2000-2038`；`src/agents/identity.js:1-23` | 递归扫描 `.md`，排除 `.chain.md`；有效文件要求 frontmatter 中 `name`、`description`，`package` 可使运行时名称变成 `package.name`，不能从文件名推断身份。读取原文展示，解析失败保留诊断，不假冒有效代理。 |
| `pi-subagents/src/agents/agent-selection.js:1-23`；`src/agents/agents.js:2570-2574,2631-2638,411-445`；`src/slash/slash-commands.js:85-108,599-647` | `/run` 按 `both` 发现并按运行时名解析；来源优先级 builtin < package < user < project，运行时注册代理还可进入发现结果；同名遮蔽按解析名、alias/localName 等匹配，不能只按显示名判定。异常定义还可能阻断同名调用。 |
| `pi-subagents/docs/agents.md:3-29,200-247`；`docs/models.md:3-41,111-145`；`src/agents/agents.js:966-1024,1283-1299` | 覆盖确为 `subagents.agentOverrides.<runtimeName>.{model,thinking}`；custom/package 代理 user→project 逐字段叠加，项目优先；builtin 有项目覆盖条目时直接采用该条目，**清空项目字段并非逐字段回退 user**，还须考虑 `disableBuiltins`/`disableThinking` 分支。frontmatter/默认值、provider 定向覆盖和单次 `/run` 参数也会影响结果。`thinking` 包含 `minimal/xhigh/max`、`false`，原型四档与空值语义不完整。 |
| `node_modules/@earendil-works/pi-coding-agent/docs/settings.md:3-24,330-355`；`ARCHITECTURE.md:295-305` | Pi 项目 `.pi/settings.json` 信任门控，嵌套 JSON 逐层合并；Picot 注册或打开工作区会尝试写 trust，但属于 best-effort，不能据注册状态断言项目配置实际已加载。 |
| `ARCHITECTURE.md:46-50,274-276`；`public/app.js:7219-7250`；`public/landing.js:361-371,466-504` | 现有 Settings 是 `settings-nav-item`/`settings-tab`，主会话的配置桥依赖运行时；landing 无绑定工作区，配置能力依赖按需创建的 global-only bridge。项目页必须隐藏或禁用并明确提示「先进入工作区」。 |
| `src-tauri/src/host_server.rs:2600-2621,2748-2840,2880-2920`；`src-tauri/src/host_config.rs:29-90` | 控制帧只准已认证 desktop owner；现有泛用 `settings_get/put` 的 global 路径硬编码 `~/.pi/agent`，非 global 可走绑定工作区，但 `settings_put` 接受整个对象、会覆盖并发修改，且 `agent_text_file_put` 只准根目录文件名，不可拿来读写嵌套 agent `.md`。需要专用窄操作，不能给 WebView 任意路径写权限。 |

Pi 的资源过滤由 Pi 管，扩展另有其扫描规则：`node_modules/@earendil-works/pi-coding-agent/docs/packages.md:187-245` 定义包过滤与同包作用域覆盖。`pi-subagents` 的包扫描直接读取本地包根与其 manifest，**不保证**逐项等同 Pi 的资源启停列表。需以当前安装版本实测禁用资源、未加载包、同包跨作用域时 `/run` 真正发现集；不可把 package-skills inventory 直接当成子代理权威数据。

## 3. 页面契约

1. 沿现有 Settings 导航新增入口，原型的独立侧栏只是布局示意；沿用 Picot 主题、i18n、焦点及移动宽度适配。作用域顺序为「Global / Current project」，默认 Global；项目级入口仍可用。左 master 按来源列条目，右 detail 对本页范围内的文件展示 **实际文件原文**、解析出的名称/描述、来源作用域、路径、只读提示、适用时的覆盖编辑器；无选择、读取/解析失败、有未保存更改、保存冲突均提供可见状态。原型目前仍用 `baseModel: Inherit parent` 等示例值拼出伪 YAML；实施不得这样展示，原型须另行修订，不把当前演示视为已修复。
2. 当前项目 tab：展示所绑定工作区 `<cwd>/.pi/agents` 及本项目安装包的候选；「新建」只写该 `.pi/agents/`，覆盖只写该 `.pi/settings.json`。全局 tab：展示 `~/.pi/agent/agents` 与全局包；「新建」只写该 `agents/`，覆盖只写 `~/.pi/agent/settings.json`。路径用主机解析出的 Pi agent root（尊重 `PI_CODING_AGENT_DIR`），不要把 WebView 中的 `~` 当真实路径。两个 tab 不复制、移动另一作用域的文件；包条目始终只读，覆盖只改 settings。共享包根出现于两种配置时注明双来源，避免重复假装两个不同文件。**浏览分类（2026-10-02 拍板；同日按 builtin 归位复核）**：每个作用域 tab 内分两个子页签——「自定义」（该作用域 agents 目录的候选，只剩 user/project）与「扩展包」（该作用域的包来源候选，按包身份分组、行显示包身份）。子页签是纯前端投影：host 请求仍只带 global/project 作用域，同一次盘点内切换子页签不重新请求；作用域切换时子页签重置为「自定义」。「另见于」徽标按当前视图相对计算（排除自身作用域），不得自指。
3. builtin 不属于项目/全局 `.md` 或用户安装包。为避免漏列默认可运行代理，在全局的扩展包子页增独立「pi-subagents 扩展内置（只读）」组，源路径与 bundled 名称如实显示；项目 tab 可以在状态说明中引用 builtin 作为被遮蔽来源，不把它混作项目文件。若已安装版本内置目录不存在、被 `disableBuiltins` 禁用或与用户定义冲突，按诊断展示，不硬编码 0.73.1 名单。跨页签同名条目继续分开列、以物理文件路径/包身份识别；标「当前工作区生效」「被 X 遮蔽」「禁用」「不可用/待验证」，并指出胜出者路径（无文件的来源给出身份）。被遮蔽条目仅展示只读详情，不提供跳转或 model/thinking 保存入口；名字级覆盖会影响胜出者，不能暗示只修改被遮蔽文件。全局 tab 的状态依赖当前工作区；无工作区时只标「全局候选，无法判断当前项目最终生效」。
4. 对胜出且支持原生 Pi 覆盖的代理，展示两个明确值：**本作用域已保存覆盖**（可编辑）与**当前工作区推算值**（带覆盖来源）。前者留空表示「删除本层字段」，不是强制写入 `"inherit"`；custom/package 清空项目字段后可由 user 同字段补位，builtin 若仍有项目覆盖条目则选择该项目条目，不会对被清空字段逐字段回退 user；两个字段都清空而项目条目还有其他字段时亦然。后者可由 global/project 覆盖、frontmatter、扩展默认、parent model、按 provider 覆盖及本次 `/run` 选项决定。builtin 与 custom 合并细节如上，仍须用运行结果核对。未掌握 live parent/provider/模型注册表时，只写「需运行时确认」，不得声称固定「生效模型」；保存后提示外部文件已更新、现有 Pi 会话可能仍用旧快照，需 `/reload` 或新会话并用 `/subagents-models`/`/run` 核实（`pi-subagents/docs/agents.md:246-250`、`docs/models.md:154-163`）。
5. 覆盖按 **胜出者解析出的 runtime name** 写 `subagents.agentOverrides[name]`，不按文件 basename、显示昵称或 package 的本地名写。每项允许无覆盖 / 合法 Pi 模型 ID / thinking 档位；`thinking: false` 和 `model: "inherit"` 若已有需可读、可保留，不可在用户仅改另一字段时误删。选项取 Pi live model registry 与模型支持档位，或提供可校验 ID 的文本输入；没有 live catalog 时不伪造原型中的两个模型。仅保存 model/thinking，不动同一条目既有 description/tools/disabled 等未知字段。覆盖同名时 settings 是**名字级**而非文件级：两个定义共用同一运行时名时，不可能只覆盖其中某个包文件；被遮蔽详情只读，不提供跳转或针对该文件的保存入口。runner 为 `external-cli`/`external-job` 等外部执行器、不支持原生 Pi model/thinking 覆盖时，禁用对应控件，标「外部 runner 不支持原生 Pi 模型/思考覆盖」，不显示虚构的生效值；未知 runner 能力按不可编辑处理并给诊断（`agents.js:1763-1770`；`src/runs/foreground/subagent-executor.js:2733-2738`）。
6. 新建表单至少收集 name、description、**非空 prompt 正文**；修订后的原型已演示 prompt 输入、非空校验及同名来源确认，但创建仍只更新页面内存。实际实现须存标准 YAML frontmatter + 正文；新建后详情仍只读。校验扩展可解析的名字及描述、唯一目标文件名、碰撞/别名提示；有同名全局/包/builtin 时明确「新文件将遮蔽 X」，要求确认，不能静默声称新名字不可用。拒绝空正文、路径分隔符、`.`/`..`、控制字符、超长输入或不合法 YAML；文件名不必等于解析名的旧文件要按内容检测名称冲突。新建失败不遗留半文件。原型的同名判断只用模拟记录，真实来源与遮蔽关系仍需运行时验证。

## 4. 发现与 `/run` 对齐

建议以独立的 host-owned **只读盘点**为最小实施面，再返回结构化结果给 UI：`list(scope, workspaceId?) → {agentRoot, projectRoot?, entries[], diagnostics[], resolutionContext}`。本页范围内候选携带 `runtimeName/localName/source/sourceScope/package identity/filePath/rawDefinition/parsedFields/readOnly/diagnostic`、在 `both` 下的 winner/被遮蔽原因；`getDetail(id)` 根据已盘点的受限真实路径重读并限量返回，避免客户端任意路径探测。范围外扫描源仅显示来源与诊断，不读取或返回 prompt/原文。磁盘源与当前会话 live 快照须区分。列表按两个页签物理归属投影，但**同一快照必须同时计算 `both` 的胜者**；不可分别调用 `discoverAgents(project)` 和 `discoverAgents(user)` 再拼接：两者的包、默认值、设置解析及合并语义不等价于 `both`（`agents.js:2511-2558`）。

实现前优先评估在受信 Pi 扩展上下文内能否获得精确的发现快照（源码中的 `discoverAgentSnapshot`/`discoverAgentsAll` 和 runtime registry 是内部模块，**不是已确认稳定、可从 Picot 直接导入的 API**；不要把读取另一个 npm 安装副本冒充内嵌 Pi 的 live 实例）。若无法可靠复用，按 0.73.1 的扫描、解析、来源优先、阻断诊断写窄只读适配层，并配与真实 `/run` 的对照测试；若无法证明 parity，就以「磁盘候选」呈现、关闭断言式「生效」标签。不要为了一个设置页重新实现执行器或 package manager。0.73.1 的 `agentScanDirs`、`.agents/`、`~/.agents/`、环境目录、runtime 注册代理可落在两个约定页签之外：首版以「其他来源（仅来源/诊断）」提示其可影响胜出，不读 prompt。无法盘点动态 runtime 注册代理时显式显示「运行时扩展代理未纳入磁盘列表，生效未验证」。筛选不会改变 `/run` 的作用域。

项目 tab 的 `<cwd>` 必须取当前 Registered owner 的 canonical workspace root；但扩展可能在嵌套 `.pi`/`git-root` 解析成别的 projectRoot（`agents.js:567-631`）。**冲突时不允许把写到 `<cwd>/.pi/...` 描述为 `/run` 将读取。**盘点应给出工作区 root 与实际扩展 projectRoot 两者；存在分歧时禁用项目级创建及覆盖写入，说明根不一致，并移除「会立即生效」文案；不擅自改变已定的写入目标。其他 Pi agent root / 启动 env 失配同理报错，不猜。

## 5. 保存、授权与恢复

建议专用 host control op：`subagents_inventory`（读）、`subagents_create`（创建）、`subagents_set_override`（仅该 runtimeName 的两个字段）。最终传输参数为 `scope`, `workspaceId?`, `candidateId/runtimeName`, `expectedRevision`, `model/thinking` 的显式 set/clear 操作，响应附新 revision、更新后的盘点与生效时机提示；host 须复验候选胜出关系及 runner 能力；不得借被遮蔽文件标识保存；对不支持原生 Pi 覆盖的 runner 拒绝写入。标识符仅供 host 在授权快照中查找，**不是前端传任意绝对路径**。具体命名可在实施时调整，权限和行为不可缩水。

- 所有 op 限已认证 desktop owner，拒绝 LAN/browser。项目读写要求当前 Registered owner + 同一 workspaceId/generation + canonical root；进入异步锁前后与实际落盘前复验，跨工作区切换取消旧请求。即使全局 tab 可由 landing 使用，landing 也不能偷偷传 project cwd 或显示「当前项目生效」；全局读写只准 host 解析出的 Pi agent root。项目未受信或信任不可核实时拒绝项目盘点/写入，明确错误，不自动把 `defaultProjectTrust: ask/never` 当成允许。不可复用 host `settings_put` 的宽对象替换或 `agent_text_file_put` 的顶层文件操作（`host_server.rs:2749-2840`）。
- 覆盖写采用锁下重新读取 JSON 对象 → 比对目标 entry/revision → 只修改 `subagents.agentOverrides[name].model/thinking` → 临时文件 + 原子 rename；缺文件可建最小对象，损坏/非对象/超限或 `subagents`/`agentOverrides` 形状非法则**拒写**，不能覆写修复。清空两个字段时删除空 entry，但保留其余未知字段及其他代理和 Pi 配置；保持权限私有，检查符号链接/真实路径与目录逃逸。沿用/核实 `host_config.rs:29-90` 锁与 512KiB 限制，注意其 `write_json` 并不独自完成「锁内读-改-写」，须确保整个事务受同一锁保护；同时核对 Pi/其他扩展各自写 settings 的锁协议。
- 新建只准选中固定 agents 目录，安全验证父目录链及最终路径，`create_new` 排他写入到受限临时文件/最终文件（不得覆盖已有文件、symlink 或 package 文件）；落盘后复读并检查扩展可解析。失败清理新建临时文件；若写入后并发路径变化/验证失败，报告需人工检查，不悄悄声称回滚完成。
- 保存前按 revision 检测外部修改，冲突提示重新载入并保留用户草稿，不做 last-writer-wins。原文件已存在时保留备份/提供明确可恢复副本，故障后可按已保存的上一版恢复；备份不能含超出本功能需要的凭据副本到公开目录。原子替换可避免半写，但不能回滚已运行会话内存状态，外部进程并发写必须通过可重复测试验证。

## 6. 验证矩阵与验收

| 场景 | 必须证明 |
| --- | --- |
| 发现/来源 | 空目录、递归 `.md`、`.chain.md`、非法 YAML、`package` 前缀与 alias、内置、两类 manifest、npm/git/file 包根、系统 npm root、项目根包、排除/额外扫描目录；项目/全局重名与包同名；盘点候选与当前 0.73.1 `/run`、`/subagents-models` 的真实输出对照，差异标诊断。 |
| 覆盖 custom/package | 全局/项目定义及包分别覆盖；global 值影响项目定义，project 值影响 global/package；逐字段 user→project 合并、清空项目字段后由 user 同字段补位；provider 特定覆盖、frontmatter/default/per-run 优先链；不丢其他 JSON 字段；thinking `false/minimal/xhigh/max` 与不支持模型档位报错。 |
| 覆盖 builtin/外部 runner | builtin 项目条目优先于 user 整条覆盖；清空项目字段但保留项目条目其他字段时不逐字段回退 user，项目条目全删后才重新落到 user/默认分支；验证 `disableBuiltins`/`disableThinking`；外部 runner 不支持原生 Pi 覆盖时禁 model/thinking 控件且无虚构生效值；被遮蔽条目只读、无跳转和保存入口。 |
| 信任/身份 | Registered 正确 wid、generation 与重绑；landing 仅全局；LAN/browser、伪造 workspaceId、过期 generation、未受信项目均拒绝；nested root 分歧禁止项目级创建与覆盖写入，不误报生效；自定义 `PI_CODING_AGENT_DIR` 与 Windows/macOS/Linux 路径。 |
| 写入/恢复 | `.md` 创建排他、同名遮蔽确认、symlink/`../`/外部路径拒绝；settings 损坏/过大、锁冲突、并发外部改写、磁盘满、原子替换失败、恢复备份；包 `.md` 字节不变、其他设置字段不变。 |
| UI/生效 | Global 排在 Current project 前且默认选 Global，项目级仍可用；master-detail、选中/空/错/未保存与键盘导航；跨 tab 与跨 workspace 旧详情失效，但未保存草稿必须按条目/作用域保留，或离开前明确要求确认丢弃，不可静默清空；返回 landing 时项目内容清空前亦需处理草稿；保存后只称「写盘成功」，已运行 Pi 会话 stale 显示并提示 reload/new session；真实模型列表不可用时不显示模拟值；原文展示不得拼伪 YAML。当前原型切 tab 时新建表单草稿会丢，且拼伪 YAML：仍待修改原型，非已修复。 |

验收门槛：以上盘点与同一 cwd 的 `/run` 输出一致，或者明确降级为候选且不显示未经证实的生效；所有写入限定批准路径、失败不损坏旧文件；项目/全局在 landing 与跨工作区场景不串权；原型中的示意模型、模拟来源与静态路径不能混入真实 UI，用户输入的 prompt 须实际写入并校验。实施后按 `AGENTS.md` 运行聚焦检查及所涉 frontend/host 的相应测试；本轮仅文档，不执行构建或测试。

## 7. 历史待验证与评审项（当前限制见 §0）

1. 当前 Picot 内嵌 Pi 版本见 `scripts/pi-version.json:2`；0.73.1 子代理扩展内部发现接口是否可在该进程合法调用并拿到动态注册代理/实时快照？若否，盘点适配层与 `/run` parity 的可接受降级范围需先实测。
2. 扩展实际 resolved projectRoot 与 `<cwd>/.pi/` 写入目标冲突时，项目级创建与覆盖禁写并提示；仍须验证两种根的判定与错误展示，不改已定写入目标。
3. Pi 包资源 filter、`autoload:false` delta、全局/项目包源的真正运行时行为与扩展本身的扫描规则是否一致？尤其禁用/未加载的包目录不能凭目录存在显示成有效 `/run`。
4. 模型下拉与 thinking 支持级别、`"inherit"`/`false` 的 UI 命名，以及 runtime 注册代理是否可获得可写 runtimeName，须用 live model registry 与当前会话验证；无法取得则使用校验输入或只读诊断。
5. 被遮蔽条目只读且不跳转；范围外来源只显示来源与诊断，不读 prompt。原型虽演示非空 prompt 与同名来源确认，仍只操作模拟记录；还存在被遮蔽条目可保存、拼伪 YAML、切 tab 丢新建草稿，须单独修订原型；Global 顺序和默认选中已与最新方向对齐。实际创建的 YAML 序列化、真实同名遮蔽判断及确认流程需实现并验证；跨平台原子重命名、symlink 竞态与多写者锁兼容也须用真实文件夹验证。

**取舍：** Global 优先并默认选中，仍保留项目写入页签；状态以实际 `both` 解析为准；保留只读包定义，用名字级 settings 覆盖，不复制包文件；被遮蔽条目只读、不跳转；范围外只列诊断；项目根不一致时禁写；新建仅有限 frontmatter 与 prompt，不建设通用 YAML 编辑器。这样不会让设置页成为另一套与 Nico 执行语义分叉的子代理管理系统。

6. **host 待办（2026-10-02 更新）**：① `savedOverride/settingsRevision` 写入层投影已随统一覆盖实施落地（原待办解除）；② Global 页签请求仍不带项目身份，host 以 `workspace=None` 扫描，页面状态注记（如遮蔽关系）缺少当前工作区上下文，不满足 §4 的 both 快照目标——写资格已按「快照内冲突」标准放宽，此缺口现为准确性限制而非安全阻断；③ 与 0.74 的已知差异：host 额外接受裸 `ssh://` 包源（0.74 resolver 拒绝），不声称 live parity。

### §3.4/§3.5 实施状态修订（2026-10-02 统一覆盖拍板后）

- **统一写入路径**：自定义（user/project 来源）、扩展包、builtin 三类候选一律走 `subagents.agentOverrides.<runtimeName>` 名字级覆盖——全局层写 `~/.pi/agent/settings.json`、项目层写 `<cwd>/.pi/settings.json`（与页签一致）；**不直改任何 `.md`**（0.74 已证实三类来源均接受该覆盖，custom/package user→project 逐字段叠加、builtin 项目条目整条选择；alias/localName 仅参与调用解析，不参与查键）。
- **编辑资格**：本作用域磁盘快照内无已知同名/alias 冲突（runtimeName↔runtimeName、runtimeName↔alias、alias↔alias 任一相交即拒，含被遮蔽者）且 runner 为 native 的候选可编辑保存；`writeQualified` 只表示「此快照内允许名字级保存」，不证明 live winner。范围内扫描不完整（settings 解析失败/超预算/不可读）时受影响名字继续拒写；仅范围外未知占用（`.agents/`、运行时注册等）降为警告。
- **控件与载荷**：model 优先真实 catalog 下拉（无 catalog 退化带校验文本输入，多段 provider/id 放行），已存 `"inherit"`/目录外 ID 以「保留当前」选项呈现不静默清空；thinking 为 空/false(JSON 布尔)/off…max 枚举；未触碰字段 keep、选空 clear；`expectedRevision` 取目标层 revision。编辑器只回显目标写入层已保存覆盖（空占位起步，不回显 frontmatter、不回显另一层），并显示「写入层：{scope} · 名字级覆盖 {runtimeName} · 不修改定义文件」提示。
- **仍禁写**：`.md` 新建（create 独立拒写门不动）、external/未知 runner、被遮蔽条目、项目根分歧、范围外来源 prompt 读取。
