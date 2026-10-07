# Picot 提供方配额展示设计（Settings → 用量）

**状态：** Implemented — 2026-09-22（7 provider 探针 + Codex 重置额度双通道 + 用量页区块 + i18n 四语言；端到端真实凭据手测清单留待 Dr. Lin 走查）

实施记录：`extensions/provider-quota.ts`（解析器纯函数 + baseUrl 守卫注册表 + TTL 缓存/去重 + consume 幂等；WHAM 窗口按 primary/secondary/tertiary 角色映射，兼容 `primary`/`primary_window` 两种线格式）+ `picot-config.ts` 三 op（provider_quota_report / codex_reset_credits_inspect / codex_reset_credits_consume；codex 凭据经 getAuth + readStoredCredential 补 accountId）+ Rust 账本（`reset_credit_operations` existence-based 建表、open 复用未决行、settle settled/ambiguous、启动 60s 清扫 abandoned；数据面 op 与 cost_dashboard 同款 owner 门禁）+ `public/cost/provider-quota-panel.js`（纯 DOM 构造渲染，shadow root 内区块，dashboard rendered 事件驱动）+ transport 双 op + i18n `cost.quota.*` ×4。偏差：spec 的 metadata v7 迁移改为 existence-based 建表（public 只盖 v3 戳、Corp 拥有 v4–v6，session_bucket 先例）；`reset_credit_settle` 的 wire 参数为 `{operationId, ambiguous}`（code 不落库，表结构与 spec 逐列一致）。
**日期：** 2026-09-22
**参考实现：** opencodex `src/providers/quota*`（借用其 vendor 探针端点与数据模型，不引入代码依赖）

## 目标

1. Settings → 用量页新增「提供方配额」区块：对已配置凭据的 provider 显示当前用量窗口条（5h / 周 / 月 / 自定义窗口）与重置时间，支持手动刷新。
2. v1 覆盖 7 类 provider（按 baseUrl 匹配，不按 provider id）：OpenAI Codex plan、GLM/Z.ai（含 CN）、Opencode Go、DeepSeek、MiniMax（含 CN）、Moonshot（含 CN）、Ollama Cloud。
3. OpenAI Codex plan 支持「重置额度」：显示可用重置额度数量、明细，并可执行 consume 操作，用 Picot sqlite 持久账本防双花。

## 非目标

- Google Antigravity：pi 无此 provider，OAuth 凭据无法经 pi 取得，v1 不做（UI 不显示该 provider）。后续如需要，单独立项在 picot-bridge 用 `pi.registerProvider` 注册带 OAuth 的自定义 provider。
- 不实现、复制或反向工程任何 OAuth token exchange / refresh —— 凭据解析与刷新全部经 pi 公开 API。
- Rust 与 WebView 不直接读写 `~/.pi/agent/auth.json`（沿用 2026-08-16 oauth-model-auth spec 的边界）。
- 不做多 provider 聚合路由、账号池、被动观测（opencodex 有，Picot 不是代理，用不到）。
- 不改内嵌 pi 二进制、不升级 pi 版本 pin。

## 已验证事实

1. **数据面已存在**：WebView 经 `ConfigGateway`（`public/settings/config-gateway.js`）→ native RPC `/picot-config` → pi 进程内 `extensions/picot-bridge.ts` → `handlePicotConfig(op, params, ctx)`（`extensions/picot-config.ts` switch 分发）。OAuth 登录、模型配置已走此通道。
2. **凭据解析 seam**：pi 公开导出 `ModelRuntime`，其 `models.getAuth(providerId)` 解析 provider 凭据，**OAuth 过期自动刷新**，失败抛 `ModelsError(code="oauth")`。api-key provider 返回 `AuthResult.auth.apiKey`。`readStoredCredential(providerId)`（pi-coding-agent 公开导出）可读 `auth.json` 的原始 credential（OAuth 含 `accountId`）。
3. **各 vendor 配额端点**（全部为 opencodex 生产验证过的端点，照抄语义）：

   | Provider | 端点 | 鉴权 | 响应关键形状 |
   | --- | --- | --- | --- |
   | openai-codex | `GET https://chatgpt.com/backend-api/wham/usage` | `Authorization: Bearer` + `ChatGPT-Account-Id` 头 | `rate_limit.primary/secondary/tertiary_window {used_percent, reset_at, limit_window_seconds}`；`rate_limit_reset_credits.available_count`；`plan_type` |
   | zai / zai-coding-cn | `GET {monitorHost}/api/monitor/usage/quota/limit`（monitorHost=api.z.ai 或 open.bigmodel.cn） | CN 裸 key（无 Bearer）；国际 `Bearer` | `data.limits[]`：`type=TOKENS_LIMIT\|CREDIT_LIMIT`，`unit/number` 编码窗口（unit3+number5=5h，unit6+number1=周），`percentage` 或 `currentValue/usage`，`nextResetTime`；`TIME_LIMIT` 行忽略（MCP 月份额度，非模型额度） |
   | opencode-go | `GET https://opencode.ai/zen/go/v1/usage` | `Bearer` | `usage.rolling/weekly/monthly {percent, resetsAt}` |
   | deepseek | `GET https://api.deepseek.com/user/balance` | `Bearer` | `balance_infos[]`（按币种一行）：`total_balance/granted_balance/topped_up_balance`。只有余额无上限 → 余额标签条，不伪造百分比 |
   | minimax / minimax-cn | `GET {host}/v1/token_plan/remains`（host=www.minimax.io 或 api.minimaxi.com） | `Bearer` | `data.remains_time`（剩余毫秒）、`total_time`（总时长）；算 consumed 占比，无 total 则只显示剩余时长 |
   | moonshotai / moonshotai-cn | `GET {host}/v1/users/me/balance`（host=api.moonshot.ai 或 api.moonshot.cn） | `Bearer` | `data.available_balance/voucher_balance/cash_balance`。余额型；CN 站 CNY、国际站 USD，单位跟 host 走 |
   | ollama-cloud | `GET https://ollama.com/api/usage` | `Bearer` | `limits.session/weekly/monthly.usage`（usage 内含已用百分比） |

4. **重置额度端点**（ChatGPT 官方功能）：`GET .../wham/rate-limit-reset-credits` → `credits[] {granted_at, expires_at}`；`POST .../wham/rate-limit-reset-credits/consume`，body `{redeem_request_id: <uuid>}`（上游幂等键），返回 code：`reset / already_redeemed / nothing_to_reset / no_credit`。数量 `available_count` 由 `wham/usage` 响应顺带返回。
5. **Pi 进程内网络**：扩展在 pi 进程（Node 环境）发 fetch，无 CORS 限制；凭据不出进程。
6. **sqlite**：Picot `metadata_store.rs` 已有迁移框架（严格增量，当前 v6），新增表走 v7。
7. **现状凭据**：经 Pi 设置录入的 API key 一律落在 `~/.pi/agent/auth.json`，扩展内用 pi 公开导出的 `readStoredCredential(providerId)` 读文件即得。Dr. Lin 的 auth.json 已有 `openai-codex`(OAuth)、`zai-coding-cn`、`opencode-go`；deepseek / minimax-cn / moonshotai-cn 需在 Picot 模型页补录 key（pi 内置 provider，录入后同样进 auth.json）；ollama-cloud pi 无内置，需建自定义 provider（baseUrl `https://ollama.com/v1`，key 存 models.json 条目）。未配置的 provider 配额区不显示。
8. **WHAM 窗口映射规则**（照抄 opencodex `parseUsageQuota`）：primary 窗口 `limit_window_seconds ≥ 28 天` 判月、`< 1 天` 判短时 burst（跳过、让位 secondary）；否则 primary=周、secondary=5h、tertiary=月补充。

## 基本原则与安全边界

- **pi 是凭据唯一权威**：探针在 pi 进程内经 `getAuth()` 拿凭据；OAuth 刷新失败报 `needs_login`，不在 Picot 侧重试刷新。
- **token 不出 pi 进程**：扩展返回 WebView 的数据只有归一化的配额数字与封闭错误码；无 token、无原始响应、无 URL。
- **baseUrl 守卫**：每个探针先比对 provider 的 canonical baseUrl 集合，不匹配不发包（防把 key 发到仿冒 host 或自定义中转）；`redirect: "error"`、8s 超时。
- **desktop-owner 门禁**：配额 op 沿用 ConfigGateway 现有 owner 校验；用量页在 landing 可见，landing 经 `~/.pi/tmp` 临时 pi 的 ConfigGateway 同样可用。

## 架构

```text
vendor endpoint
  → extensions/provider-quota.ts（pi 进程内探针 + 解析 + 5min 缓存）
  → handlePicotConfig 新 op：
       provider_quota_report {force?}
       codex_reset_credits_inspect {}
       codex_reset_credits_consume {operationId}
  → ConfigGateway → WebView
用量页 public/cost/dashboard.js 新区块渲染

重置额度（不可逆）双通道：
  WebView → Rust host op reset_credit_open（sqlite 记 pending，返回 operationId）
         → ConfigGateway codex_reset_credits_consume {operationId}（pi 内 POST，redeem_request_id=operationId）
         → Rust host op reset_credit_settle {operationId, code}
```

## 数据模型

```ts
// 归一化配额（对齐 opencodex ProviderQuota，TypeScript 形状）
type QuotaWindow = { label: string; percent: number; resetAt?: number };
type QuotaReport = {
  provider: string;            // pi provider id（UI 另配显示名）
  source: string;              // 如 "openai-codex:wham"、"zai:quota-limit"
  quota?: {
    fiveHourPercent?: number; fiveHourResetAt?: number;
    weeklyPercent?: number;  weeklyResetAt?: number;
    monthlyPercent?: number; monthlyResetAt?: number;
    customWindows?: QuotaWindow[];   // 余额型/时长型 provider 用
    resetCredits?: number;           // 仅 openai-codex
    updatedAt: number;
  };
  failure?: QuotaFailureCode;
};
type QuotaFailureCode =
  | "not_configured" | "needs_login" | "rate_limited"
  | "upstream_error" | "timeout" | "response_unusable" | "destination_blocked";
```

- 余额型 provider（deepseek / moonshot）显示为 `customWindows: [{label: "余额 $x.xx / ¥x.xx", percent: 0}]` 标签条，不伪造占用比。
- MiniMax 无 `total_time` 时同上（「剩余 Nh」标签条）。
- 失败保留 last-good 行 30 分钟（盖 `unavailable` 标记），照抄 opencodex 的瞬时/终态区分：4xx（非 408/429）为终态丢弃旧行，其余为瞬时保留。

## 扩展侧设计（extensions/provider-quota.ts）

1. **选择器**：`providersOfInterest()` = 遍历 `ModelRuntime.create()` 的 provider 列表 + models.json 自定义 provider，凡 canonical baseUrl 命中上表者产生探针。provider id 透传（ollama-cloud 的 id 由用户建自定义 provider 时决定，不可预测）。
2. **凭据**：api-key 型直接 `readStoredCredential(providerId).key`（经 Pi 设置录入的 key 都落在 `~/.pi/agent/auth.json`，读文件即得）；自定义 provider（ollama-cloud）读 `models.json` 对应条目的 `apiKey` 字段（与 `custom-provider-probe.ts` 合并条目同源）；仅 openai-codex 走 `getAuth(providerId)` 取自动刷新后的 access token，并以 `readStoredCredential("openai-codex")` 补 `accountId`。OAuth 刷新仍归 pi，Picot 侧 401 时报 `needs_login`，不自行重试。
3. **缓存**：进程内 Map，TTL 5 分钟，in-flight Promise 去重（key=provider id）；`force: true` 跳过。pi 进程重启/扩展 reload 后自然清空，接受。
4. **探针纪律**：`redirect: "error"`、`AbortSignal.timeout(8000)`、响应体上限 256KB、JSON 解析失败 → `response_unusable`。
5. **consume 幂等**：`codex_reset_credits_consume` 只接受外部传入的 `operationId`（来自 Rust 账本），进程内维护 pending 集合：同 id 并发/重复调用直接拒绝（`operation_in_flight`）；调用超时或响应不可读时上报 `ambiguous`（扩展侧不落盘，持久状态由 Rust 账本负责）。成功后顺带强制刷新 openai-codex 配额缓存并返回新 `available_count`（若有）。

## Rust 侧：重置额度账本

sqlite 新表（metadata_store v7 迁移）：

```sql
CREATE TABLE IF NOT EXISTS reset_credit_operations (
  id TEXT PRIMARY KEY,          -- uuid，即 redeem_request_id
  opened_at INTEGER NOT NULL,
  settled_at INTEGER,
  status TEXT NOT NULL          -- pending | settled | ambiguous | abandoned
);
```

两个 host op（host_data 数据面 + host_control 控制面各一，走既有 WS 路由与 owner 门禁，比照 `cost_dashboard`）：

- `reset_credit_open`：插入 pending 行，返回 uuid。同账户已有 pending/ambiguous 未决行时返回该行 id（阻止开新单）。
- `reset_credit_settle {operationId, code}`：按 code 落 `settled`（reset/already_redeemed/nothing_to_reset/no_credit）或 `ambiguous`（超时/不可读）。

**恢复策略**：启动时扫 `pending` 且 `opened_at` 早于 60s 的行标 `abandoned`（扩展进程已亡，重放无门）；`ambiguous` 行在用户下次点「重置额度」时**允许带同一 operationId 重放**——账本至多保留一条未决行并复用其 id（`metadata_store.rs::reset_credit_open`），该 id 即上游幂等键，故重放已生效的操作只会得到 `already_redeemed` / `nothing_to_reset`，不会双花。账本行只存 id/时间/状态，不存任何凭据或上游响应。
> **2026-09-23 修订**：原设计要求扩展先 `GET rate-limit-reset-credits` 对比 `available_count` 是否减少，再决定能否重放。该对比不做——基线取自带 5 分钟 TTL、可能为 30 分钟 last-good 的配额报告，且另一设备或 chatgpt.com 网页端的重置同样会让当前值变小，两个方向都不可靠，用它驱动「可能已生效」的提示反而会误导。重放安全性由 id 幂等提供，与计数无关；UI 改为如实提示。

## WebView UI（public/cost/dashboard.js 新区块）

1. 配额是「使用量」页内的独立子页签（「使用量」/「配额」两个 tab，2026-09-23 修订：原设计为 `renderShell` 追加 `<section id="usage-provider-quota">` 于 usage-models 之后，实装改为 Settings 主 DOM 的 `#settings-provider-quota`——配额不是成本统计的一格）；区块标题「提供方配额」+ 整体刷新按钮。
2. 每个有报告的 provider 一张卡：显示名（id→显示名映射表，未知 id 原样显示）+ 窗口条列表（percent 进度条 + label + 相对重置时间）+ 数据更新时间。
   进度条色档（2026-09-27 修订）：<75% 绿、75–90% 橙（含 90）、>90% 红。
   窗口行为两行式（2026-09-27 修订）：首行 label + 剩余徽标（与 bar 同色系淡底/深字圆角标签，显示「剩余 {n}%」即探针已用百分比的补数）+ 右侧重置时间；次行全宽 bar（全宽即天然左对齐，宽度同样按剩余量绘制、随消耗递减）。重置时间分档：≤60 分钟「{n} 分钟后重置」、<24h 同日「{n} 小时后重置」、跨午夜「明天 HH:MM 重置」、更远「M月D日 HH:MM 重置」。余额行为单行（label + 金额，无 bar）。
   codex 卡标题：显示名 + 账户类型徽标（绿底绿字，无边框；取 WHAM 响应根字段 `plan_type`，缺失时回退解码 access token JWT 的 `chatgpt_plan_type` claim，opencodex 同款）+ 重置额度 chip（橘底橘字，紧跟账户类型）。
   加载行为（2026-09-27 修订）：首次/刷新/每次进入「使用量」页先渲染骨架卡（动画条）再异步探测（`provider_quota_report` 内各 provider 探测为 `Promise.all` 并行；进入 Usage 页由 `selectSettingsTab` 重触发非 force 加载，5 分钟缓存命中时即时出卡；`loadReports` 串行化，飞行中的调用合并）；`loadReports` 串行化（飞行中的调用合并）；探测失败且无旧数据时保留错误提示条而非隐藏。
3. openai-codex 卡额外显示「重置额度 N 个」；点击 → 确认对话框（列 credits 明细 granted_at/expires_at）→ 确认后走 open→consume→settle 流程，结果 toast（成功/无可重置/无额度/结果未知）。
4. `ambiguous` 结果 toast 如实提示「可能已生效，重试安全（沿用同一请求 id）」；不做 inspect 计数对比（见上「恢复策略」2026-09-23 修订）。
5. 空态：无可探测 provider 时整块隐藏（不显示「暂无数据」占位）。2026-09-27 修订：隐藏仅指「成功探测且零个已配置 provider」；**请求失败**（超时/传输错误）保留区块——标题 + 刷新按钮 +「暂时不可用」提示条，可手动重试。启动时那次探测失败后整段隐藏曾把页面变成无法自愈的白板。
6. landing：用量页 landing 本就可见，本区块随 ConfigGateway 可用性正常工作；ConfigGateway 不可用（无任何 pi）时区块隐藏。

## 错误状态

| 码 | UI 表现 |
| --- | --- |
| not_configured | 不显示该 provider（探测名单本来就按配置过滤） |
| needs_login | 显示「需重新登录」+ 指向模型页 OAuth 入口 |
| rate_limited / upstream_error / timeout | 显示「暂不可用」，保留 last-good 数字（30 分钟内） |
| response_unusable / destination_blocked | 显示「暂不可用」，丢弃旧行 |
| operation_in_flight（consume） | 按钮 loading 态，toast「操作进行中」 |
| ambiguous（consume） | toast「结果未知」，引导重开对话框核对 |

## 测试计划

1. **解析器单测**（vitest，`extensions/provider-quota.test.ts`）：每个探针喂 opencodex 注释/实测形状的样例 JSON，断言归一窗口；WHAM 窗口映射的边界（28 天月窗、短时 burst 窗、缺失窗）；baseUrl 守卫对非 canonical host 拒发；余额型不伪造 percent。
2. **缓存与幂等单测**：TTL 内二次调用不发包；force 发包；同 operationId 重复 consume 被拒。
3. **Rust 账本**：metadata_store v7 迁移单测；open/settle/恢复（abandoned 扫描、ambiguous 重放同 id）单测。
4. **UI**：`public/cost/` 新模块（如 provider-quota-panel.js）given reports → 渲染断言（条数、重置按钮仅 codex、空态隐藏）；遵循「50 行以上独立模块」纪律。
5. **端到端手测清单**（真实凭据，不入自动化）：8 家真实拉取截图核对、重置额度全流程含断网中途 kill pi 的 ambiguous 恢复。

## Follow-up（不在本期）

- Antigravity：picot-bridge `pi.registerProvider` 注册自定义 OAuth provider（device flow + token 刷新移植），单独立项。
- ARCHITECTURE.md 增补「Provider quota probes」节（实施时同步）。
- 配额数据的历史趋势（opencodex 有 quota/history，Picot 暂不做）。
- OpenCodex 代理聚合（若 Dr. Lin 想让 Picot 直接读 localhost 代理的 /api/provider-quotas，可作可选探针，另议）。
