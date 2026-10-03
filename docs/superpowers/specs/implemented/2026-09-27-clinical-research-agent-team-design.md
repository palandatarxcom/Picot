# 真实世界临床研究团队：六岗位 agent 设计

**状态：** 试用版；六份本机岗位指令与 Paseo profile 已创建，真实团队尚未验证。
**日期：** 2026-09-27
**范围：** Paseo 的 agent profiles 与 `~/.pi/agent/agents/` 下六份岗位指令；Picot 子代理扩展另见 `2026-09-27-pi-subagent-extension-design.md`。

## 1. 目标与边界

模拟真实团队的岗位分工，让承担任务的 agent 能向其他专业岗位咨询，并把有依据的意见交给人类。先在 Paseo 测试咨询与追问；同一批 Pi 角色文件随后供 Picot 使用。Picot 侧的自研子代理扩展尚未实现，不把跨 agent 持续对话写成 Picot 现有能力。现有 tree view、fork、conversation rewind 与本设计无关。

六个 agent 可以在获得明确任务和写入授权后，编辑自己主责的交付物；跨岗位咨询、评审只提供意见，不擅改被咨询人的文件。模型输出不等于医学、统计或项目最终批准；特别是 CSR 最终由**人类医学经理**负责。研究数据均为合成数据；工作区可共享医学编码字典和研究数据字典。字典只需以文件名定位，不能编造版本；如果文件实际标有版本，照实引用。研究资料若后来换成非合成数据，必须先另行确定模型与数据使用边界，不能沿用本设计默认许可。

## 2. 岗位、主责和可用能力

| 岗位 / 建议文件名 | 主责交付物与工作 | 咨询与评审边界 | 现有 Pi 包 |
| --- | --- | --- | --- |
| 医学经理 `clinical-medical-manager.md` | 理解药企医学需求、文献、protocol；医学术语和研究设计把关；评审 CSR、论文/摘要/汇报材料 | 回答工程师的医学定义问题；就 SAR 的分析解释向统计师咨询；负责 CSR 医学内容的最终人类审核 | `datarx-academic-research`、`datarx-writings` |
| Medical writer `clinical-medical-writer.md` | 在医学经理确定的目标与依据下起草 CSR、文章、摘要及大会报告 PPT | SAR/数字有疑问时问统计师或统计程序员；医学判断交医学经理，不擅自修改 SAP/SAR | `datarx-academic-research`、`datarx-writings` |
| 统计师 `clinical-biostatistician.md` | 理解 protocol，编写/评审 SAP、TFL shell、SAR；裁定统计分析口径 | 向统计程序员解释 SAP/TFL、审 R 程序与 TFL；医学判断问医学经理；不替统计程序员批量改代码 | `datarx-make-sap-shell`、`datarx-writings` |
| 统计程序员 `clinical-statistical-programmer.md` | 根据 SAP/TFL 编写 R 程序、生成分析数据与表图、核对输出 | SAP/TFL 口径不明问统计师；解释程序和数值来源给 writer/医学经理；不自行更改分析决策 | `datarx-stats` |
| 高级数据架构师 `clinical-senior-data-architect.md` | 设计数据库结构、制定 DCP、汇总治理证据撰写 DCR | 回答数据工程师的治理规则/映射问题；医学术语有疑问问医学经理；审核工程师治理结果 | `datarx-make-dcr`、`datarx-writings` |
| 数据工程师 `clinical-data-engineer.md` | 按 protocol/DCP 执行数据映射、治理、质量检查，产出执行证据 | 医学编码或诊断定义问医学经理；治理规则和 DCP 问高级数据架构师；不自行改变医学或统计口径 | 暂无专属包；遵循 DCP 与团队咨询 |

这些包提供可调用的 skill，不是岗位指令的副本。角色只在相应包实际安装、skill 可发现时使用；缺失时说明缺失，不伪称已跑流程。尤其 `datarx-make-dcr` 现有技能围绕**已定稿 DCP 之后**的 DCR 模板、治理数据移植、核查和组装，并不自动生成 DCP；架构师仍需根据任务和材料独立起草 DCP。`datarx-make-sap-shell` 的翻译功能仍规划中，不在首轮宣称可用。`datarx-writings` 有手动触发的 PPT 技能，不要求每个写作岗位自动调用全部技能。`datarx-academic-research` 包含独立研究/论文/评审工作流；不把其中的 reviewer agent 当作临床岗位签核人。

## 3. 角色指令与 Paseo 配置

六份 `.md` 放在用户的 `~/.pi/agent/agents/`，使用不与现有 13 个通用 agent 重名的 `clinical-*` 名称。frontmatter 使用 Pi 已支持的字段，正文包含职责、可写范围、咨询路径、证据与交接规则、不能确认时的处理。角色的专业技能在正文按名称引用，不复制包内的长工作流。只读咨询不修改任何文件；明确授权的主责工作才可写入。职责限制不是 OS 沙箱，工具权限与人工审阅另行约束。

Paseo 在 `~/.paseo/config.json` 的 `daemon.agentProfiles` 中新增六条同名 profile，保留原有 profile 不变。`provider` 固定为 `pi`（Dr. Lin 指定），`model` 不填，由 Dr. Lin 后续手工选择；不猜默认模型，也不输出配置里的凭据。notes 各含适用任务、咨询对象、对应 `.md` 的路径，以及**创建 agent 时的 initialPrompt 首句**：

```text
先完整读取 ~/.pi/agent/agents/<对应文件>.md，以其为岗位指令执行本任务；若文件不可读，停止并报告，不要冒充该岗位。
```

Paseo profile 只保存 provider/model/mode/thinking/notes 等启动预设，**不自动注入 system prompt**。选中 profile 的人或 orchestrator 必须把上述首句实际放入 `initialPrompt`；随后在同一个 prompt 中指定本次任务和材料。`list_profiles` 只供发现 notes，`create_agent` 没有 `profile` 参数：调度者需显式复制选定 profile 的 provider/model 等字段。共享目录或远程主机上必须先确认该绝对路径可读；找不到指令则停下，而不是仅靠 notes 模仿角色。不会自动修改已有 agent 的配置或重启现有会话。

一个岗位 agent 以**一项交付物任务**为会话单位新建；同一问题可继续追问。新交付物或材料明显变化时起新 agent，并重新指定文件名和当前任务，不复用旧对话中的过期判断。Paseo 的 agent 间咨询使用其现有的创建和发送 prompt 能力；Pi/Picot 首版通过 parent 委派 child、拿到答复后由 parent 继续工作，不声称 child 之间已有持续通信。

## 4. 交接单：agent 与人都能读

咨询方须提供具体问题及可访问的材料文件名；收到意见后由主责岗位核对并决定是否采纳。不新增 JSON schema 或审计系统。回复使用固定 Markdown 标题：

```markdown
## 问题
谁问什么；所涉及的研究交付物。

## 依据
实际读取的文件名，以及能找到的章节、表号、变量名或 R 代码位置；字典用文件名和条目即可。

## 答复与理由
结论、解释，以及是否基于材料推断。不得编造不存在的分析结果或字典版本。

## 待确认事项
缺资料、矛盾、仍需人类岗位裁决的事项；无则写“无”。
```

若尚未读取原件，写“未核对原件”；定位不到证据写“待核实”，不能凭摘要替代来源。咨询意见不能覆盖源 SAP/DCP 等已明确的规则；规则冲突交还给该交付物主责和人类负责人。示例路线：统计程序员问统计师解释 SAP/TFL → 按意见改 R 程序 → 统计师审查；writer 问统计师 SAR 的统计口径，问程序员表格数值来源；数据工程师问医学经理字典映射的医学含义，问架构师 DCP 治理规则。无需给每种关系单独开发工具。

## 5. 分步实施与真实团队试用

1. 先写六份 `.md`，不改现有通用 agent。人工核对六份正文与 §2、§4 一致，并在普通 Pi 会话中验证文件可读、名称不冲突。此步只是手工冒烟，不设自动化测试。
2. Paseo 新增六条 `provider: "pi"` profile，保留原有 profile；模型与思考等级由 Dr. Lin 手工调整。修改前备份配置、检查文件现状及权限，不覆写未知字段。创建新 agent 后核对它**确实读了**指定 `.md`，不能只看 profile 名字。
3. 真实医学经理、writer、统计师、统计程序员、数据工程师和架构师在合成数据及共享字典工作区自由试用，反馈角色边界、依据质量、遗漏、实际咨询体验。由对应人类岗位核对输出；出现错引、编造结论或越权修改，按反馈修订文件。没有固定场景验收或自动化测试要求。
4. Picot 子代理扩展完成后再验证相同六份指令在 Picot 的委派路径；两端交接格式保持一致，但不要求其进程生命周期相同。

不实现自动医疗判断、研究数据脱敏器、跨岗位自动审批、GxP 审计系统、长期自动运行或新的统计/治理工具。Paseo 配置及六份 agent 文件属于用户级配置；不随 Picot 仓库发布。后续若需公司统一分发，另议包位置、版本与许可。
