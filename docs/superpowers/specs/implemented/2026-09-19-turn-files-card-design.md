# 本轮修改文件卡片（Turn Files Card）设计

**Status:** Revised — 2026-09-19 三轮修订（critical review + 五项定案 + Dr. Lin 手工测试反馈）后待复核。Q1–Q5 决议不变。
**Date:** 2026-09-19
**Provenance:** Dr. Lin 手动测试建议：把「本轮修改的文件 (n)」chips 行改造为类似「思考中」的折叠卡片。
2026-09-19 critical review 后修订，随后就 5 个开放项定案；同日手工测试又提两项布局反馈，delta 见「本次修订」。

## 目标

turn 结束后，把该轮写过的文件从一行 chips 升级为一张**默认折叠**的卡片：每文件一行，
带 git 状态徽标与 +N -M 统计；点击行打开文件预览。

## 决议表（grilling 结论，不变）

| # | 决策点 | 结论 |
| --- | --- | --- |
| Q1 | 统计口径 | **B：工作区累计**——`git status --porcelain` 分类 + `git diff --numstat HEAD`（HEAD unborn 时 `--cached`）。非逐轮基线；同文件多轮时数字含前轮累计，接受。 |
| Q2 | 时效 | **turn 结束取一次，冻结进卡片**。展开不查询；后续轮次不改旧卡片数字。 |
| Q3 | 历史 turn | **只列文件，无统计无徽标**。冻结数字不持久化；重开会话退化为纯文件列表。 |
| Q4 | Untracked | **显示 `+总行数`**（host 数行数），与 Added/Modified 行格式统一。 |
| Q5 | 壳与交互 | thinking-block 式样基底 + `turn-files-card` 修饰类（todo 浮动面板否决）；默认折叠；每行 = 徽标 + 文件名（悬浮全路径）+ 统计；点击行 → 预览面板；首写排序；`agent_end` 出现；**完全替换** chips 行。 |

## 本次修订

### 第一轮（critical review）

原批准稿的决策与结构不变，以下为评审补入的契约项，每项对应一个可复现的失败场景：

1. **第三种状态**：原稿让「git 失败/非 git 工作区」与「已提交（clean）」渲染完全一致。新增 `unavailable`。
2. **路径 containment**：原稿只写「绝对路径自动剥工作区前缀」。改为组件级判定（禁止字符串前缀比较）。
3. **untracked 行数上限**：原稿未限，实现会整文件读入内存。新增读入上限与 `additionsCapped`。
4. **生命周期兜底**：原稿只写 `agent_end`。补 `agent_settled` 兜底，否则漏掉 `agent_end` 的那一轮文件会被下一轮卡片消费。
5. **`deleted` 行语义**：原稿写「不返回、不显示」，实现只在 host 侧剔除统计、行仍渲染。改为前端按 status 决定是否渲染行。
6. **验证矩阵**：原稿只要求「op 编译」。补 Rust 单测矩阵（现无任何 Rust 测试）。
7. **影响面与清理**：补删除物清单（含已无引用的 `.turn-file-chip*` 样式）。

### 第二轮（五项定案）

| # | 项 | 定案 |
| --- | --- | --- |
| 1 | untracked 读入上限 | **256 KiB 且 5 万行（先到者）**，流式分块计数；非 UTF-8 不计数 |
| 2 | 第三态展示 | **折叠头同一行的行尾弱化文字**（`…（3）· 统计不可用`），并写入 `title` 与可访问名 |
| 3 | `additionsCapped` 展示 | **`+≥N`**，本地化文案只用于 `title`/可访问名 |
| 4 | 越界路径 | **不再剔除**：`files` 恒等于入参集合，越界行以 `status: "unavailable"` 渲染（无徽标无数字） |
| 5 | Q1-B 口径措辞 | 可见标题不动，口径写入折叠头 `title`/可访问名 |

### 第三轮（Dr. Lin 手工测试反馈）

| # | 反馈 | 定案 |
| --- | --- | --- |
| 1 | 展开后文件多时把 answer 挤出视野 | 展开体加 `max-height: 260px` 与内部滚动（`.tool-output` 同值同滚条样式），折叠头保持可见 |
| 2 | copy 按钮与时间戳被挤到卡片上方 | 卡片挂到 answer 与 `.message-actions` **之间**：`mountTurnFilesCard()` 插在 `.message-actions` 之前；无工具条的消息（不可复制、`suppressToolbar`）回落到消息元素之后 |

## 状态徽标

`XY` porcelain 分类（v1）与其渲染：

| XY | status | 渲染 |
| --- | --- | --- |
| `??` | `untracked` | 行 + Untracked 徽标（灰 `--text-dim`）+ `+总行数` |
| 含 `D` | `deleted` | **不渲染该行** |
| 含 `A` | `added` | 行 + Added 徽标（`--success`）+ `+N` |
| 含 `R`/`C`，或含 `M`，或其余（`T`/`U` 等） | `modified` | 行 + Modified 徽标（`--accent-text`）+ `+N -M` |
| pathspec 无匹配 | `clean` | 行 + 无徽标 + 无数字（保留隐藏占位以对齐三列） |
| 越界、无法判定 | `unavailable` | 行 + 无徽标 + 无数字（同 clean 的视觉） |

- `D` 优先于 `A`：`AD`、`MD` 表示工作区已删除，按 `deleted` 处理。
- rename/copy 的**源路径**在结果中记为 `deleted`（`-z` 在目标记录之后给出源路径），其行同样不渲染；
  目标路径记为 `modified`。
- 统计数字：`+N` 用 `--success`，`-M` 用 `--error`；binary 的 `-` 按 0；`additionsCapped` 时显示 `+≥N`。
- 徽标为**名词**文案：EN `Added`/`Modified`/`Untracked`（当前 EN 的 `Add` 为动词，需改），zh 新增/修改/未跟踪。
- 状态不得只靠颜色表达（徽标已带文字）；`clean`/`unavailable` 的隐藏占位保持 `aria-hidden="true"`。
- **卡片级降级标注**：整次取数失败（非 git 工作区、git 非零退出、超时）时，折叠头那一行在计数后
  追加 `· messages.turnFilesUnavailable`（`--text-dim`，窄栏 `text-overflow: ellipsis` 保留计数）。
  行照常渲染为纯文件名。历史卡片（Q3）是设计内缺失，**不加**该标注。
- 折叠头的 `title` 与可访问名承载两段说明：本地化口径（`messages.turnFilesCaliber`：统计为自 HEAD
  的工作区累计，含你在该工作区的未提交改动；已提交的本轮改动显示为无数字行）与历史场景的
  「历史不含统计」。

## 数据面

新增 host 控制面 op `git_turn_stats`，走既有 `dispatch_git_host_operation`（入口先做
`registered_workspace(...)`，Registered-only，见 `src-tauri/src/main.rs:1692`）。

### 入参

`{ paths: string[] }`

- 条数 ≤ 200，单条长度 ≤ 4096；超限返回 `Err`（显式失败，不静默截断）。
- 每条可能是绝对路径（常见，工具参数原样传入）或工作区相对路径（模型有时这么写），两种都要处理：
  - 绝对路径：对**词法归一化**后的路径做组件级 `Path::strip_prefix(root)`，并拒绝剩余部分含 `..` 组件。
    root 的两种拼写都要接受（调用方给出的路径与 `fs::canonicalize(root)`），否则 macOS 的
    `/var` 与 `/private/var` 差异会把合法写入判成越界。
  - 相对路径：视为已相对于 root，同样拒绝 `..` 组件。
- **不用 `canonicalize` 做判定**：对不存在的文件会失败，且工作区内经符号链接指向外部的合法写入会被误判。
- 禁止字符串前缀比较（`docs/engineering-lessons.md` #3）：`/root-evil/x.ts` 不得被判为 root 内。
- 越界、含 `..`、无法判定的路径**仍出现在 `files` 中**，`status: "unavailable"`。

### 出参

`{ files: [{ path, status, additions, deletions, additionsCapped }], dropped: number }`

- `files` 恒等于入参集合，按入参顺序；`path` 原样返回（卡片按它匹配行）。
- `status` ∈ `added` | `modified` | `untracked` | `deleted` | `clean` | `unavailable`。
- `dropped` 为判定为越界的条数，仅供 Rust 测试与诊断断言「确实判为越界，而不是静默无匹配」；
  前端不使用它。

### 分类

`git --literal-pathspecs status --porcelain -z -- <paths>`

- `-z` 与 `--literal-pathspecs` 必须显式使用：前者避免含空格/特殊字符路径被 C-quote，后者避免路径被当 glob。
- pathspec 限定到入参，不做全工作区扫描（大仓库下每次 `agent_end` 都会调用）。
- rename/copy 记录的第二段（无 XY 头）必须跳过。

### 统计

- `added`/`modified`：`git diff HEAD --numstat -- <paths>`；HEAD unborn 时回退
  `git diff --cached --numstat -- <paths>`（对齐 `change_stats`）；binary 的 `-` 按 0。
- `untracked`：行数计入 `additions`，`deletions = 0`。**流式分块计数**（内存恒定），读入总量上限
  **256 KiB 或 5 万行**（先到者），触顶时 `additionsCapped = true` 并停止读取。禁止整文件读入。
  非 UTF-8（含 NUL 字节）视为二进制，不计数（`0/0`、`additionsCapped = false`）。只统计普通文件：
  先取 `symlink_metadata`，符号链接与非普通文件不跟进、不计数。恰好读完整个文件（字节数等于文件长度）
  不算触顶，计数为精确值。
  无尾换行的文件计为「行数 + 1」。整次调用共享一个 `GIT_READ_DEADLINE` 预算；预算耗尽时同样置
  `additionsCapped = true`（下界语义不变）。
- `clean`、`unavailable`：`0/0`；`deleted`：不计算。

### 失败

非 git 工作区、非零退出、超时（`GIT_READ_DEADLINE`，`src-tauri/src/git_service.rs:30`）一律返回
`Err`；前端进入卡片级降级标注，不得静默当作 `clean`。

实现位置：`git_service.rs::turn_stats`；复用 `git()`、`assert_workspace_root`。

## 生命周期

- **live**：`agent_end` → `state.settleTurnWrites()`（drain）→ 一次 `git_turn_stats` →
  冻结渲染 → 经 `mountTurnFilesCard()` 插入 answer 与 `.message-actions` 之间；宿主元素已断开则不插入。
- **兜底**：`agent_settled`（`agent_end` 未到达：重连、崩溃、abort）也必须 drain 并渲染一次。
  两条路径对同一 turn 幂等，只渲染一张卡片。`settleTurnWrites()` 本身是 drain
  （`public/app/state.js:67`），因此任何路径只要调用它，就不会把上一轮文件带进下一轮卡片。
- **会话切换 / 重渲染**：卡片随 transcript 重建，无额外状态；取数响应晚到时若宿主元素已
  断开则丢弃，不跨会话插入。
- **历史**：`historyTurnWrites` 重建路径清单 → 无统计渲染（同款壳），不显示降级标注，
  `title` 说明历史不含统计；挂载槽与 live 相同（历史消息同样带 `.message-actions` 工具条）。
- **降级**：取数失败或超时 → 纯文件列表 + 折叠头降级标注；不重试（Q2 冻结语义）。

## 不做

- 逐轮 git 基线快照（Q1-A 否决）；展开时实时取数（Q2-B 否决）；统计持久化（Q3-C 否决）。
- bash 工具间接改动的文件（沿既有 write 工具路径清单边界）；todo 面板式浮动壳；deleted 行。
- 不新增设置开关或环境变量；不改 `git_status`/`git_diff` 等既有 op 语义；不做卡片内联 diff 预览。

## 影响面与清理

| 文件 | 变化 |
| --- | --- |
| `src-tauri/src/git_service.rs` | `turn_stats` 与 `TurnFileStat`（新增 `deleted`/`unavailable`/`additionsCapped`）+ 单测 |
| `src-tauri/src/main.rs` | `git_turn_stats` 分发分支（沿用 Registered-only 门禁） |
| `public/app/transport.js` | `gitTurnStats(paths)` |
| `public/app.js` | `agent_end` 与 `agent_settled` 双路径、行点击委托、宿主断开守卫、两处挂载点改走 `mountTurnFilesCard` |
| `public/ui/turn-files-card.js` | 卡片渲染、按 status 决定渲染行、折叠头降级标注、`+≥N`、`mountTurnFilesCard` 挂载槽 |
| `public/ui/turn-files-card.test.js` | 见「验证」 |
| `public/style.css` | 保留 `.turn-files-*`；**删除**已无引用的 `.turn-file-chip*`（`style.css:4760` 起）；展开体上限 260px 与内部滚动、滚条 |
| `public/turn-file-chips.js`、`public/turn-file-chips.test.js` | 已删除（「完全替换 chips 行」的一部分） |
| `public/locales/{en,zh,ja,es}.json` | 复用 `messages.turnFiles`；新增 `messages.turnFilesUnavailable`、`messages.turnFilesAtLeast`、`messages.turnFilesCaliber`、`messages.turnFilesHistoryNote`（四语言齐） |

可访问性要求：行与折叠头均为 `<button>`；折叠头维护 `aria-expanded`；折叠头的可访问名包含计数、
降级标注与口径说明；行的可访问名包含预览动作、文件名、状态与统计（有则附）；`title` 保留全路径。

## 验证

### Rust 测试矩阵（`git_service.rs`，TDD 先行）

1. 分类矩阵：`A `、`M `、`??`、clean、`R `（记为 modified）、`D `（返回 `deleted`）。
2. `AD`/`MD` 优先级：`D` 先判，返回 `deleted`。
3. 含空格与含中文的文件名正确匹配（验证 `-z` + `--literal-pathspecs`）。
4. HEAD unborn：`--cached` 回退给出 added 统计。
5. 越界输入：兄弟前缀（`/root-evil/x.ts`）与含 `..` 组件的路径返回 `status: "unavailable"`，且
   `dropped` 计数正确、`files` 仍含该条。
6. 相对路径入参按 root 相对处理；含 `..` 的相对路径同样为 `unavailable`。
7. binary 的 `-` → `0/0`；untracked 且含 NUL 字节的文件不计数。
8. untracked 行数：无尾换行文件计为行数 + 1。
9. 超过读入上限（>256 KiB 或 >5 万行）时 `additionsCapped = true` 且读取提前停止。
10. 非 git 目录 → `Err`。
11. 入参顺序在 `files` 中保持；`files` 长度恒等于入参条数。
12. 入参超上限（条数/长度）→ `Err`。

### 前端模块（`turn-files-card.test.js`）

- 折叠默认关闭、toggle 切换 `aria-expanded`。
- 徽标与冻结数字（含 untracked 无 `-M`、clean 隐藏占位对齐）。
- `deleted` 行不渲染；`unavailable` 行渲染为无徽标无数字；标题计数等于渲染行数。
- `additionsCapped` 显示 `+≥N`。
- 卡片级降级标注在展开/折叠两种状态都可读；历史卡片不显示该标注。
- stats 为 `null` 时回退列 writes（历史），无徽标无数字。
- 空与退化输入返回 `null`。
- 挂载槽：带 `.message-actions` 的消息内顺序为 content → card → actions；无工具条时卡片落在消息元素之后；`card` 为 `null` 时不改动 DOM。
- 展开体规则含 `overflow-y: auto` 与 `max-height`（读 `public/style.css` 断言，写法同 `composer-opacity.test.js`）。

### app 级

- `agent_end` 与 `agent_settled` 各只渲染一张卡片（幂等）；漏 `agent_end` 时下一轮卡片不含上一轮文件。
- 点击行打开预览（委托在 `public/app.js:2395`，模块测试断言不到）。
- 会话切换后到达的统计响应不插入到新会话。

### 手工 e2e

- git 工作区一轮改 2+ 文件（含 1 新建）：卡片折叠、徽标与 `+N -M` 与 `git diff --numstat` 一致。
- 非 git 工作区：折叠头出现「统计不可用」，而非静默无数字。
- 重开会话：历史卡片无降级标注，`title` 说明不含统计。
- 大文件：新建一个 >256 KiB 的文件，确认 `+≥N`，进程内存无异常增长。
- 工作区外写入：一轮里让 agent 写一个工作区外文件，确认该行仍出现且无徽标无数字。

### 命令

`bun run check`、`bun run check:rust`、`bun run test`；Rust 侧 `cargo test` 串行执行（并发会争
构建锁，见 `docs/engineering-lessons.md`）。

### 覆盖缺口

Windows 路径分隔符与大小写不敏感行为未验证；`git diff` 在超大仓库的耗时未测量。

## 已定项与残余开放项

五项开放项已于 2026-09-19 定案（见「本次修订 · 第二轮」）：

1. untracked 读入上限取 **256 KiB / 5 万行**（先到者）。
2. 第三态取**折叠头同一行的行尾文字**（备选：图标 + `title`，不占行高，已否决）。
3. `additionsCapped` 取 **`+≥N`**（备选：超限不显示数字，与 clean 同形，已否决）。
4. 越界路径**照常渲染**为 `unavailable` 行；理由：同一 turn 的工具卡片已在 transcript 中显示该
   文件的完整路径（`public/ui/tool-card.js` 的 `filePathFromArgs` 直接取工具参数），卡片隐藏
   它换不到暴露面的减少，只会让卡片与工具卡互相矛盾。
5. Q1-B 口径不改可见标题，写入 `title`/可访问名；用户自身未提交改动造成的歧义无法用文案消除，
   只能靠逐轮基线，而 Q1-A 已否决。

残余开放项：无。Windows 与超大仓库的验证缺口见「验证 · 覆盖缺口」。

## 实施记录（2026-09-19）

- **Rust**：`git_service.rs::turn_stats` 重写（组件级 containment、`D` 优先于 `A`、rename 源路径记为 `deleted`、
  untracked 流式计数 256 KiB / 5 万行上限、入参上限、共享 `GIT_READ_DEADLINE`），新增 13 项单测。
- **前端**：`public/ui/turn-files-card.js` 只过滤 `deleted` 行、标题计数=渲染行数、折叠头降级标注、`+≥N`；
  `public/app.js` 的 `agent_settled` 兜底 drain+渲染一次、历史调用点传 `history: true`；删除死样式
  `.turn-file-chip*`；四语言新增 4 个 key。
- **验证**：`cargo test turn_stats` 13 passed；`bun run check:rust`（clippy 警告即错误）通过并
  400 passed / 7 ignored；`biome check` 通过；卡片测试 6 passed；串行全量 171 files / 1858 tests passed。
- **已知既有失败（与本改动无关）**：`public/sidebar/workspace-registry.test.js` 在并行全量下偶发多一次
  session fetch；单独运行与 `--no-file-parallelism` 全量均通过。
- **未覆盖**：手工 e2e（非 git 工作区的降级标注、>256 KiB 文件、工作区外写入行）与 Windows 路径行为。

### 手工测试反馈落地（2026-09-19）

- `public/ui/turn-files-card.js` 新增 `mountTurnFilesCard(messageEl, card)`；`public/app.js` 的 live（`appendTurnFilesCard`）与历史两处挂载点改走它。
- `public/style.css` 的 `.turn-files-card.expanded .turn-files-body` 加 `max-height: 260px`、`overflow-y: auto`、`overscroll-behavior: contain` 与 4px 滚条（同 `.tool-output`）。
- 验证：`turn-files-card.test.js` 10 passed；`bun run vitest run public/ui/` 219 passed；`bun run check` 0 error 且 design check passed。

### 路径来源补充：子代理结构化收据（2026-10-12）

卡片路径清单原只来自 write 家族工具（含行式 `edit` 的 `details.files`）。子代理 fan-out 的写入
只在结构化收据里，故补一处来源，实时与历史共用同一提取器（`public/file-preview-follow.js` 的
`subagentChangedFiles`）：

- 仅消费**成功**子代理结果的三段结构 `details.results[].acceptance.childReport.changedFiles`
  （`exitCode === 0`）；外层失败、子结果失败、空报告与非字符串路径不纳入；跨结果按入参顺序去重。
- **不解析**自然语言正文、`mission` 重复摘要或 `toolCalls` 文本，**不扫描/加载**子 JSONL。
- 子代理在自己的 cwd 下执行：`args.cwd` 与主 workspace 不一致时其**相对**路径不得归属本工作区，
  整段丢弃；绝对路径仍交既有 containment 判定。策略沿用上文越界路径第 4 条语义：路径照常出现在
  卡片里，判定/包含由既有边界处理。
- 不打开预览：fan-out 无唯一目标，只记账。该收据是**子代理自述**，非宿主独立核验的文件写入审计。
