# Picot 环境与工具链维护设计

**状态：** Draft，待 Dr. Lin 评审
**日期：** 2026-11-11（第三版：新增 R 作为第 7 个工具；2026-10-06 第二版：安装机制收敛为「交给内嵌 Pi」）

## 目标

在 Settings 新增独立「环境」页，面向非程序员检查与维护 Picot 常用的外部工具：

1. host 侧确定性探测显示工具状态、版本与实际路径。
2. 安装/更新交给内嵌 Pi 执行：`pi --no-session -p "<prompt>"`，一次一个工具。
3. agent 只被授权当前工具与官方入口；结束后由 host 复检，不采信 agent 文本或 exit code。
4. 支持 macOS 与 Windows；Landing 与已打开 workspace 都可访问。
5. 遇管理员权限、UAC、重启、交互式安装器或额外依赖时停止，展示并允许复制本次实际 prompt，交用户手动继续。

## 非目标

- 不把系统工具混入「软件包」页（该页管理 Pi 的 npm packages/extensions/skills）。
- 不把功能藏进「高级配置」。
- 不支持 Linux 的安装与更新。
- 不检查或安装 hunk、不检查系统 Bun。
- 不自行实现 Homebrew、winget、UAC、sudo 或 installer 的完整安装器。
- 不写 sqlite、不新增遥测、不把安装日志上传网络。
- **不做进程监督子系统**：不保证子进程树零逃逸、不做重启后可恢复的清理状态、不做 Windows Job Object。取消维护只 kill 直接子进程，可能留下孤儿——与「不引入 OS 级沙箱」是同一种已接受的代价。

## 工具清单与分级

| 分级 | 工具 | 用途 | 官方入口 |
| --- | --- | --- | --- |
| 基础 | git | 版本控制、Git 面板、diff、历史、提交与 push | `https://git-scm.com/downloads` |
| 基础 | python3 | Python 项目、脚本与 agent 工具运行时 | `https://www.python.org/downloads/` |
| 基础 | npm | npm 来源 Pi extension 与 Node 工具 | `https://nodejs.org/en/download` |
| 基础 | uv | Python 工具/项目依赖管理 | `https://docs.astral.sh/uv/getting-started/installation/` |
| 基础 | R | R 统计计算运行时（R/Rscript，R 项目与 R 包的依赖；不是 RStudio） | `https://cran.r-project.org/` |
| 可选 | officecli | Word/Excel/PowerPoint 的 agent 工具 | `https://github.com/iOfficeAI/OfficeCLI#readme` |
| 可选 | dws | 钉钉工作台 CLI，供 agent 操作钉钉消息、文档、日历、审批等 | `https://github.com/DingTalk-Real-AI/dingtalk-workspace-cli#readme` |

基础工具缺失时标红但不阻止 Picot 启动或普通聊天；可选工具缺失标黄。

## 页面位置与可用性

- Settings 顶级「环境」页，Landing 与已打开 workspace 都可访问。
- 打开页面**不自动探测任何工具**：初始显示「尚未检查」与**检查环境**按钮；检查完成后显示**重新检查**。
- 检查在 host 侧执行，不需要模型。
- 安装/更新需要模型。未配置模型时该次运行会失败，页面显示失败原因，不额外做预检门禁。
- 页面显示本次运行的 provider/model 不做要求；`pi -p` 自行从 settings 解析默认模型。

## 每项展示与操作

- 名称、用途、级别（基础/可选）、状态、版本、实际可执行路径、官方链接。
- 状态：`尚未检查`、`已就绪`、`缺失`、`失败`（含原因）。
- 操作：`缺失` → **安装**；`已就绪` → **更新**；`失败` → **重试**。
- 运行中显示：可折叠日志、**复制 prompt**（复制实际传给 `pi -p` 的同一份字符串）、**取消**。
- 终态显示 host 复检结果与原因；`已就绪` 才算成功。更新后版本未变化时如实显示「版本未变化」。

## Host 探测

检查不使用模型、网络或 WebView shell。host 通过固定命令白名单启动短进程并解析版本：

| 工具 | 探测命令 | 版本来源 |
| --- | --- | --- |
| git | `git --version` | stdout |
| python3 | macOS/Linux `python3 --version`；Windows `py -3 --version` 后回退 `python --version` | stdout/stderr |
| npm | `npm --version` | stdout |
| uv | `uv --version` | stdout |
| officecli | `officecli --version` | stdout/stderr |
| dws | `dws --version` | stdout |
| R | `R --version`；Windows 另有注册表兜底（见下） | stdout，须 `R version` 前缀 |

- 每候选 5 秒超时；超时按失败处理，不杀主 app。
- 路径解析交给 OS 启动器（`Command` 自身按 PATH/PATHEXT 查找）；host 另做一次简单 PATH 扫描仅用于**显示**实际路径。
- 版本必须来自该工具自己的那一行：git 要求 `git version` 前缀、python3 要求 `Python` 前缀、dws 要求 `dws version` 前缀；只读 stdout（python3/officecli 例外，可读 stderr）。错误行里出现的其它版本号（如 npm 报错提到 Node 版本、连接失败提到 IP）不得被当成该工具版本。
- 每条探测返回：规范 tool id、状态、版本、可执行路径、失败原因、官方 URL、级别。

**Windows 的 R 注册表兜底。** R 的 Inno 安装器静默装完默认不把 R 写进 PATH（R for Windows FAQ 2.5），只按 PATH 探测会把装好的 R 误判为缺失。因此 Windows 上 R 的探测顺序是：先 PATH 上的 `R --version`；失败后读 `HKLM\Software\R-core\R` 的 `InstallPath`（无管理员权限的安装写在 `HKCU`），用解析出的 `bin\x64\R.exe --version` 复跑，仍要求 `R version` 前缀。复跑通过 → `已就绪`，且带 `viaFallbackPath` 标志：此时 R 仍不在 Picot 进程的 PATH 上，agent 会话要等 Picot 重启后才用得了，页面在此情形如实显示「已安装，重启 Picot 后可用」。注册表值只用于解析程序路径，版本一律来自 R 自己的输出行。macOS 不需要兜底（官方 pkg 会在 `/usr/local/bin` 建软链，Homebrew 在 PATH 上）。

**host 写用户 PATH（Windows，仅安装/更新运行内）。** 复检经注册表兜底判 `已就绪` 时，host 亲自把 R 的 bin 目录追加进用户 Path（复用 `pi_path::apply_windows`：幂等、保留 REG_EXPAND_SZ 类型、广播 `WM_SETTINGCHANGE`），失败如实写进运行原因、不翻转成功判定；agent 不参与写 PATH。写入结果以机器可读的 `Snapshot.userPathFixed`（bool，serde 为 `userPathFixed`）表达：仅当复检命中兜底且 `apply_windows` 成功（新增或已存在）时为真。提示语按该字段门控，不按 probe 就绪推断、不解析文案：纯「检查环境」或写入失败 → 「已安装，不在 PATH」的事实性提示（失败原因在运行块可见）；写入成功 → 「重启 Picot 后 agent 会话可用」，此时承诺为真。

## 安装与更新

用户点安装/更新即授权**当前这一个工具**的官方安装/更新流程。host 启动内嵌 Pi：

```text
<embedded-pi> --no-session -p "<prompt>"
```

- 工作目录设为系统临时目录，避免 agent 在 workspace 里动手。
- 同时最多一个任务；运行中其它安装按钮禁用。
- prompt 由 host 用探测到的事实渲染（工具、动作、平台、架构、状态、版本、路径、官方 URL），页面「复制 prompt」用的是**同一份字符串**。
- **R 的镜像选择（仅 R，确定性，不测速）**：渲染 prompt 前，host 拉 `https://cran.r-project.org/CRAN_mirrors.csv`（5 秒超时），过滤 `OK==1` 且 `CountryCode==cn`，取第一个官方镜像作为 `downloadUrl` 写进 prompt 上下文；拉取或解析失败回退官方根 `https://cran.r-project.org/`。`officialUrl` 始终是官方根，规则边界不变；`downloadUrl` 只是 host 从官方清单选出的下载入口，其他工具不设该字段。不做 RTT/带宽测速——确定性优先于「最快」。
- 无硬性总超时之外的预算设计：单个任务 15 分钟后被 kill。
- 用户可取消：kill 直接子进程，随后对当前工具做 host 复检。

### Prompt 内容

prompt 内联在 host 代码中（`environment_prompt.rs`），包含：当前工具与动作、官方 URL、验证命令、停止条件、以及「完成后 host 会自己复检」的说明。停止条件：需要 `sudo`/管理员密码/UAC、需要重启、需要交互式或 GUI 安装器、需要额外语言运行时；遇到即停下说明，不绕过。同时要求不读取或修改项目文件、凭据、登录状态与 Picot 设置。

### 复检

- agent 的 exit code、自然语言总结都不是成功依据。
- 进程退出后 host 立即重新探测该工具；只有探测为 `已就绪` 才算成功。
- 进程正常退出但复检未就绪 → `失败`，显示复检原因；更新后版本未变化 → 显示「版本未变化」。

## R 的平台现实（已接受的行为）

- **macOS**：CRAN `.pkg` 装到 `/Library/Frameworks` 并需要管理员密码，会触发 prompt 的停止条件——点安装多半停在「需要管理员密码」，页面如实展示。这是 Dr. Lin 在 2026-11-11 明确接受的后果（保留通用 prompt，不做 Homebrew 引导）。arm64 pkg 需 macOS 14+。
- **Windows**：Inno 安装器支持 `/VERYSILENT /CURRENTUSER` 静默安装，无 UAC 也可完成；装完不进 PATH，由上面的注册表兜底复检兜住。
- 验证命令为 `R --version`，版本行如 `R version 4.6.2 (2025-10-31)`。

## 跨平台范围

- **macOS**：Apple Silicon 与 Intel；Homebrew 或官方 pkg/下载均可，由 agent 按官方说明选择。
- **Windows**：Git for Windows、Python Launcher、Node.js/npm、uv、officecli、dws 的官方安装路径。`py -3` 不可用时回退 `python`。
- 两平台都不引入 OS 级隔离或进程容器；取消只 kill 直接子进程。
- **Linux**：不提供安装/更新入口，仅检查。

## 状态机

```text
尚未检查 → 检查中 → 已就绪 | 缺失 | 失败
缺失 | 已就绪 | 失败 → 运行中 → 已就绪 | 失败 | 已取消
```

`已就绪` 只能由 host 复检给出。

## 安全与所有权

- **host**：唯一执行探测、启动/取消安装进程、执行复检的一方。
- **prompt**：定义范围与官方来源，不是强制隔离；agent 保有 shell 能力，遵守程度属模型行为。这是 v1 明确接受的残余风险，不引入 OS 级沙箱。
- **WebView**：只请求检查/安装、展示状态与日志；不执行 shell。
- 所有环境请求继承 desktop-owner 门禁；Landing 放行，LAN/mobile client 不放开。

## 测试与验证

1. **探测单测**：七工具的版本解析、错误行不被误读、畸形版本被拒、前缀要求、超时与启动失败、缺失；R 的注册表兜底解析为纯函数可跨平台测试，`viaFallbackPath` 标志如实传递；镜像选择（CSV 解析、OK/CountryCode 过滤、取第一个、拉取失败回退官方根）为注入 fetcher 的单测；R 的 prompt 上下文含 `downloadUrl`、其他工具不含。
2. **prompt 单测**：只含当前工具与其 URL、上下文 JSON 是合法单行、含停止条件、不出现其它工具。
3. **复检单测**：agent 正常退出但复检未就绪 → 失败；复检就绪 → 成功；更新后版本未变化如实标注；取消优先。
4. **前端单测**：打开不自动探测、六行渲染与状态、基础/可选样式、按钮差异、运行中禁用与取消、终态展示、复制 prompt 用 Snapshot.prompt、错误可见。
5. **手测**：macOS 至少一次真实安装或更新；Landing 与 workspace 都能打开页面。

## Follow-up

- 实施后更新 `ARCHITECTURE.md`：环境页的 desktop-owner 边界、host 探测白名单、四个控制 op、prompt 所有权与复检契约。
- R 安装的后续观察项：macOS 停在管理员密码的频率；是否值得在后续版本对 macOS 引导 Homebrew formula。
- Linux 安装支持。
- 「维护全部」：v1 只做单工具；批量需要再设计（注意它正是上一版失控的入口）。
