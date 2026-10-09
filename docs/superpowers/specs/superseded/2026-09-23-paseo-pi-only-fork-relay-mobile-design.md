# Paseo Pi-only fork、公司 relay 与移动端整体架构

**状态：** Approved — 2026-09-23 Dr. Lin 拍板（移动端浏览器优先）；阶段一裁剪已实测落地（pi-only worktree，见 §4.5）
**日期：** 2026-09-23
**参照：** Paseo 源码（getpaseo/paseo @ 290306fd1，0.9.1）；官方 relay（getpaseo/paseo-relay，Elixir）；社区 relay（zenghongtu/paseo-relay，Go）；Cloudflare DO 版（packages/relay）。
**演化关系：** 与 Picot daemon spec（2026-09-20-persistent-daemon-relay-design.md）**并行不悖，两条产品线各自有 daemon**：① Picot daemon（Rust）按 2026-09-20 spec 继续实施，服务 Picot 桌面的持久 runtime 与将来 Picot 自己的远程访问——该 spec 全部有效，未被本方案取消；② 本方案的 Paseo Pi-only daemon（TS/Node，Paseo 自带）服务非工程师桌面与移动端。被本方案**取代的只有一点**：移动端不再连 Picot daemon（此前「Paseo-derived mobile 接 Picot daemon」的构想作废），移动端直接连 Paseo Pi-only daemon，Picot daemon 因此**无需实现任何 Paseo 兼容协议**，其 remote surface 按 Picot 自身需求收敛。

## 1. 背景与产品定位

三条产品线，两个用户群：

| 产品线 | 用户群 | 底座 |
| --- | --- | --- |
| **Picot**（现有） | 工程师；企业 SSO/Git 链接 | Rust/Tauri，Pi 原生深度集成 |
| **Paseo Pi-only fork**（本方案） | 非工程师办公助手；桌面第二选择 | Paseo 代码库裁剪，Pi 唯一 provider |
| **移动端** | 两类用户的手机入口 | Paseo React Native app（与 fork 共享代码） |

```text
Picot Desktop ────────────── Pi runtime（本机）          工程师本机工作台
Paseo Pi-only Desktop ──┐
Paseo Pi-only daemon ◄──┤ Pi runtime（daemon 子进程）     非工程师桌面 + 常驻 agent
Mobile app (RN) ────────┤
                        ▼
                  公司 relay（出站 WSS + E2EE）
                        ▲
Mobile app (RN) ────────┘ 远程手机经 relay 连 daemon
```

关键架构事实：**Paseo desktop 与 mobile 本来就是同一个 app 代码库**（packages/app，React Native + Expo，desktop 用 react-native-web 或 Electron 壳、mobile 用原生构建），连同一个 daemon 协议（packages/client DaemonClient）。裁剪一次，桌面和移动端同时生效。

## 2. 已验证事实（本轮源码核实）

### 2.1 daemon 架构

| 事实 | 证据 |
| --- | --- |
| daemon 是独立 worker 子进程：`daemon-worker.ts`（process.title "Paseo Daemon"），由 supervisor spawn，IPC channel 传 `paseo:ready/shutdown/restart`；supervisor liveness guard（心跳 3.5s、ppid 变化、IPC 断开即自杀）防孤儿 | `packages/server/src/server/daemon-worker.ts` 全文 |
| 进程属主双层：desktop（Electron main）与 CLI 都用 `daemon-instance.ts` 的 pid lock spawn/复用同一 daemon；`waitForDaemonReady` 轮询 pid lock | `daemon-instance.ts` |
| daemon 生命周期可经 WebSocket 远程指令（shutdown/restart lifecycle intent） | `daemon-worker.ts` `handleLifecycleIntent` |
| 配置持久化 + 热重载：`DaemonConfigStore`（paseoHome 下 persisted config），CLI flag（--relay/--no-relay/--no-mcp）作为 override 层 | `daemon-config-store.ts`、`daemon-worker.ts` `applyCliFlagOverrides` |
| daemon 内承载：agent manager（timeline/subscription）、workspace registry、git service、terminal、browser-tools、checkout、plugin 系统、MCP server、relay transport | `bootstrap.ts` `createPaseoDaemon` |
| relay 默认关闭，`--relay` flag 或持久配置开启；E2EE Curve25519 keypair 存 `~/.paseo/daemon-keypair.json`；配对 offer → QR → e2ee_hello/ready | `daemon-keypair.ts`、`connection-offer.ts`、relay-transport.ts |

### 2.2 provider 体系与裁剪面

| 事实 | 证据 |
| --- | --- |
| provider 清单是**一个数组**：`AGENT_PROVIDER_DEFINITIONS`（claude/codex/copilot/opencode/pi/omp 6 内置 + dev mocks） | `packages/protocol/src/provider-manifest.ts:197-260` |
| provider 客户端在一个文件集中组装：provider-registry.ts 顶部 import 全部 client 类，按 definition 建 `createClient` 工厂 | `provider-registry.ts:30-62` |
| 已有 per-provider 启停：`AgentManager.providerEnabled` Map + `ProviderEnabledFlag`，`providerEnabled.get(provider) === false` 时拒绝创建 | `agent-manager.ts:721-723,1053,5230` |
| 存量 agent 校验：`isStoredAgentProviderAvailable(record, validProviders)`，不可用 provider 的存量 agent 加载时显式报错 | `agent-loading.ts:101` |
| Pi provider 完整度高：AgentClient 全接口（catalog/run/steer/queue/import sessions/rewind/usage/permissions/slash-commands）+ pi CLI 子进程模式（与 Picot 的 cli-runtime 同构） | `providers/pi/agent.ts`、`pi/` 目录 9 文件 |
| UI 的 provider 选择由 daemon 下发（modeControl.providerDefinitions），app 侧无硬编码 provider 清单 | `composer/agent-controls/index.tsx:102,210` |

### 2.3 mobile

| 事实 | 证据 |
| --- | --- |
| Expo 54 + React Navigation 7 + expo-router（file-based routing） | `packages/app/package.json` |
| 屏幕集：欢迎/配对（pair-scan QR）、host 列表、sessions、agent chat、workspace、settings、schedules、plugin surfaces | `packages/app/src/app/` 目录树 |
| 连接层：`getHostRuntimeStore`/`useHostRuntimeSnapshot` → DaemonClient（WS）；支持多 host（serverId 维度） | `app/h/[serverId]/agent/[agentId].tsx` |
| relay 连接是 host 类型之一：`RelayHostConnection { type:"relay", relayEndpoint }`，扫码配对即得 | `types/host-connection.ts:43-56` |
| 桌面与移动端共享全部业务组件（composer/timeline/panels） | packages/app 单一代码库 |

### 2.4 relay 生态

三份开源实现（此前已核实）：官方 Elixir（生产 fly.io）、Cloudflare DO（TS）、社区 Go 单二进制（zenghongtu）。协议 v2：`GET /ws?serverId&role=server|client&v=2`；哑管道零认证；E2EE 端到端；每 client 独立 data socket。

## 3. 方案总览

三个交付物，按依赖排序：

```text
A. paseo-pi fork（worktree 裁剪）────► B. 公司 relay 部署 ────► C. 移动端发布
        │ Pi-only daemon + 桌面版           │ 出站通道                │ 扫码配对
        └────────── 共用 packages/app ──────┴────────────────────────┘
```

**明确不做**：Picot daemon 实现 Paseo 协议兼容层；Picot 与 fork 共享代码；移动端另写 native app。

## 4. Phase A：paseo-pi fork（worktree 裁剪）

### 4.1 仓库与分支策略

```bash
# fork getpaseo/paseo → 公司 org（如 palandata/paseo-pi）
git remote add upstream https://github.com/getpaseo/paseo
git worktree add ../paseo-pi -b pi-only   # 或直接 clone fork
```

- **保留 upstream remote**，定期 rebase/merge 同步（Paseo 迭代快：0.9.0→0.9.1 同日两版）。
- 裁剪 commit 保持**机械、可重放**（见 4.4「同步策略」），避免深度重写导致每次 upstream 更新都是冲突地狱。
- `appId/productName` 改名（`sh.company.paseopi` / "Pi Studio" 之类，名称待定），数据目录随之独立（`~/.paseo-pi` 或 fork 内改 `resolvePaseoHome`），避免与用户可能已装的原版 Paseo 冲突。

### 4.2 两阶段裁剪（先禁用后删除）

**阶段一（产品面收窄，几乎零风险）**：

1. `provider-manifest.ts`：`AGENT_PROVIDER_DEFINITIONS` 只留 `pi` 条目（保留数组结构与类型，只裁成员）。
2. `provider-registry.ts`：删除非 Pi 的 client import 与工厂分支（编译器会指出全部触点）。
3. `AgentManager`：沿用现有 `providerEnabled` 机制——非 pi provider 一律 `enabled:false`；存量 agent 引用被裁 provider 时，加载报错文案本地化（`agent-loading.ts:101` 既有路径）。
4. UI 零改动原则：provider 选择 UI 由 daemon 下发的 definitions 驱动，清单收窄后 New Agent 自动只剩 Pi；不逐屏删 UI 代码。
5. dev mocks（mock-load-test/mock-slow）按 `isDev` 保留在 DEV_AGENT_PROVIDER_DEFINITIONS，不影响生产。

**阶段二（运行稳定后再删，可选）**：

- 删 `providers/claude|codex|codex-app-server|opencode|omp|*-acp-agent` 目录及其 e2e 测试。
- 删 provider 专属 UI 分支（voice 的 provider 默认模型、Claude/Codex 设置页）。
- 收敛依赖（如 claude/codex options schema 的 zod 依赖）。

**不删的「看似多 provider」基础设施**：multi-host（serverId）模型、workspace registry、subscription/timeline、plugin 系统、ACP 传输层（Pi RPC 本身走同一子进程模式）。这些是 relay/mobile 的地基，不是 provider 包袱。

### 4.3 daemon 交付形态

- 直接用 Paseo 的 daemon：desktop 安装即含 daemon spawn（supervisor 模式）；CLI 用户可 `paseo daemon start` 独立起。
- 配置默认值在 fork 内调整：`--relay` 保持默认关闭（对齐 Paseo 安全默认），文档引导用户填公司 relay endpoint。
- **fork 内不自建 daemon**：Paseo daemon 已生产验证（daemon-e2e 全家桶含 pi.real.e2e），fork 直接复用。Picot 产品线的 Rust daemon 是另一条独立工作（2026-09-20 spec），与本 fork 互不替代。

### 4.4 同步策略（fork 生命周期最大风险）

- 每月（或每 upstream minor）从 upstream merge 一次；冲突集中在 4.2 阶段一的两三个文件（manifest、registry）。
- 阶段二删除越深，同步成本越高——这是把阶段二设计为「可选」的原因。
- 公司内建改动（品牌、默认 relay endpoint、禁用项）尽量放**配置层**（daemon-config persisted + CLI override）而非代码层，能配置不改码。

### 4.5 阶段一裁剪实测记录（2026-09-23）

已在 `~/tmp/PI/paseo-pi`（worktree，分支 `pi-only`，基于 upstream 290306fd1）完成并提交（`eed5c51ac`，2 文件 +4/−222）：

- `provider-manifest.ts`：`AGENT_PROVIDER_DEFINITIONS` 只留 pi；删除四个未使用 mode 常量（CLAUDE/CODEX/COPILOT/OPENCODE_MODES）；`OMP_MODES` 保留（仍被 omp/agent.ts 引用，阶段二随 provider 目录一并处理）。
- `provider-registry.ts`：内置 client 工厂只留 pi + dev mocks；`PROVIDER_CONTRACTS` 清空；**ACP override 路径保留**（用户自定义 provider 基建，非内置包袱）——恢复 ACP client import 与 `OmpRuntime`/`OpenCodeBridge` type import 即为它服务。
- 验证链：lefthook pre-commit 全 workspace typecheck（11 包）+ lint + format 全绿；server/cli build 绿；`expo export --platform web` 绿（产出 dist/）。
- **daemon 冒烟**：独立 `PASEO_HOME=/tmp/paseo-pi-home` 起停成功；`provider ls` 只列 `pi` 一行。
- **共存实证**：本机原版 Paseo daemon 占用默认端口 6767，fork 以 config.json 改 6768 并存——证实 §4.1「端口/数据目录必须隔离」；fork 后续 commit 应改默认端口常量。

| Phase | 规模 | 说明 |
| --- | --- | --- |
| A1 fork + manifest/registry 裁剪 + 改名 | 小（1-2 天量级） | 核心是机械裁剪 + 构建配置 |
| A2 品牌与首次设置向导 | 小 | 产品化打磨 |
| B relay 部署 | 小（半天 + sysadmin 核实） | Go 版 Docker 起服 |
| C 移动端 web 部署 | 小 | expo export 静态产物 + nginx 托管 |
| 长期：每月 upstream 同步 | 持续 | fork 生命周期成本 |

A1 完成即得「Paseo Pi-only 桌面版」（桌面第二选择）；B 完成打通远程；C 完成移动端闭环（浏览器）。三步各自独立可验收，任一步停下都有可用产物。原生 app（iOS/Android）明确不在当前范围，留作未来增量。


## 5. Phase B：公司 relay 部署

### 5.1 选型（建议 Go 版，待核实）

| 实现 | 优点 | 代价 |
| --- | --- | --- |
| **社区 Go（zenghongtu/paseo-relay）** | 单二进制、内存占用小、Docker 一条命令 | 社区维护，需自读源码审计 |
| 官方 Elixir（getpaseo/paseo-relay） | 上游生产版 | BEAM 运行时重，运维面大 |
| Cloudflare DO（TS） | 免运维 | 依赖 CF 账号，国内访问存疑 |

### 5.2 部署要点（沿用 2026-09-20 daemon spec §5 的核实清单）

- 反代 nginx/Caddy 终结 TLS；`/ws` upgrade + `/health` 透传。
- 防火墙：公网仅 443 入站；员工机出站 wss。
- relay 零落盘用户数据；仅访问日志。
- **域名建议**：`relay.<company-domain>`；证书走公司 CA 或 Let's Encrypt。
- 员工 daemon 配置 relay endpoint：装机引导写入持久配置（fork 内做首次设置向导，属产品增量）。

### 5.3 安全模型（继承 Paseo，零修改）

- relay 不受信：只转密文；E2EE NaCl box 每连接独立密钥。
- 信任锚 = 配对 QR（按密码对待）。
- 撤销 = daemon 侧删 keypair 重配对。
- relay 服务器被攻破的后果上限：可见连接元数据（IP/时序），不可读内容。

## 6. Phase C：移动端（浏览器优先，原生 app 不做）

### 6.1 形态：Web 构建（Dr. Lin 2026-09-23 拍板）

packages/app 是一套 Expo 代码多端构建，官方自己就部署 web 版（`https://app.paseo.sh`，Cloudflare Pages，repo 内有 `deploy:web` 脚本）：

- 构建命令现成：`npm run build:web --workspace=@getpaseo/app`（expo export --platform web），产出静态 `dist/`——本轮已在 pi-only worktree 实测通过。
- 部署即静态托管：公司服务器 nginx 托管 dist/，与 relay 同机；手机浏览器打开即用。可加 PWA manifest 支持「添加到主屏幕」（可选增量）。
- **配对走 offer 链接而非扫码**：web 端相机扫码有官方降级提示（pair-scan.tsx isWeb 分支），但 offer 本来就是 URL 形态（`https://host/#offer=<base64url>`），`OfferLinkListener` 监听 `#offer=` fragment 直接导入（app/_layout.tsx:700-740）。桌面生成 offer 链接 → 手机点开/手输 → 导入 host 配置 → 连 relay。
- 原生 iOS/Android 构建**不做**（App Store/企业分发/MDM/签名全部移出范围）；若浏览器体验验证后出现推送/离线等硬需求，再评估原生增量（Expo 原生构建命令保留在 repo）。

部署形态：

```text
公司服务器 nginx
  ├─ https://app.<company>/     ← packages/app web 构建的静态文件
  └─ https://relay.<company>/ws ← Go relay（反代透传）
```

### 6.2 裁剪面

移动端与桌面共享 packages/app，Phase A 的 provider 裁剪自动生效。web 端额外工作：

1. **品牌**：app 名/图标/launcher（同 4.1 改名）。
2. **首次引导**：默认引导连公司 relay（预填 app/relay 域名 + offer 链接导入），隐藏「自建 host」高级路径。
3. **不需要移植的**：Picot 的 SSO/企业配置页一律不做——移动端身份 = 设备配对（device pairing），不是账号体系。

### 6.3 与 Picot 的关系

```text
Picot（桌面，工程师）
   └─ 本机 Pi runtime，无 relay 依赖
Paseo Pi-only（桌面+daemon，非工程师）
   └─ relay ← mobile app
```

两产品各自独立。未来若要「手机看 Picot 会话」，届时 Picot 侧再评估接同一 relay（Picot daemon spec 的 Phase 2 通道与 Paseo relay 协议兼容性已预研），但 v1 不做。

## 7. 风险与开放问题

| 风险 | 缓解 |
| --- | --- |
| upstream 演进快，fork 同步成本累积 | 阶段一最小裁剪 + 配置层定制；每月同步节奏；裁剪 commit 保持机械可重放 |
| Pi provider 上游变更（pi 0.84→未来） | 与 Picot 共享「pi 版本升级」经验（upgrade-embedded-pi skill 流程） |
| 社区 Go relay 的维护风险 | 部署前源码审计；锁定版本；备选官方 Elixir |
| 手机浏览器能力边界（推送、后台保活） | v1 接受：打开页面才收流；推送需求出现时再评估 PWA 通知或原生增量 |
| web 端相机扫码不可用 | 不依赖：offer 链接导入是 web 端一等公民路径（OfferLinkListener） |
| Paseo Pi-only 桌面版与 Picot 定位重叠 | 按 4.1 定位分工：工程师 vs 非工程师；共享 Pi 生态但 UI/分发独立 |
| 员工 daemon 常驻的内存占用（Electron desktop 场景 Paseo 已优化到 ~150MB） | 非工程师桌面预期可接受；CLI-only daemon 更轻 |
| **开放**：fork 产品名、公司 org、relay 域名 | 待 Dr. Lin / sysadmin 定 |

## 8. 测试与验收

- fork CI：Paseo 既有测试套件（vitest 全家 + daemon-e2e 的 pi 子集）全绿；被裁 provider 的 e2e 标记 skip。
- 手测清单：桌面安装→New Agent 只有 Pi→run/steer/queue→关 app daemon 存活→重开接回；手机浏览器开 web app→offer 链接导入→经公司 relay 发 prompt 收流式；relay 断连重连退避；删 keypair 后旧设备拒连。
- 与原版 Paseo 共存：不同 appId + 不同数据目录，互不污染（安装双版本实测）。

## 9. 实施顺序与规模判断
