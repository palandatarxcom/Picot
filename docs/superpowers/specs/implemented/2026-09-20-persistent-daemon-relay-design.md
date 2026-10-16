# 持久 daemon 与 relay 接入设计

**状态：** Revised — daemon 形态已拍板（§3.6：独立进程）；生命周期对齐 Paseo 已核实的 quit 行为（§3.2/§3.5/§6，关窗现状 = 退出 app）；设置页 daemon 段定稿（§3.2.2：无 CLI、三控件、无 Shutdown 按钮）；Gate B 已修订为权限跟 principal 走（§4.4）；新增 Phase 2.5 桌面多 host 客户端（§4.6）；公司 relay 已上线（paseo.021521.xyz，v0.5.0，2026-10-15 探测确认），§5.3 剩余运维项后补；同日评审二修（WebView 静态资源随迁、capability/owner 边界、admin RPC 与实例文件握手、信号分档、sidecar 打包；§3.1-§3.5、§4.4、§8）
**实施状态：** Phase 1（§3）代码已交付（T1-T12：daemon 独立进程 + admin 通道 + 生命周期/退出策略 + macOS sidecar 打包；开发期的 `PICOT_DAEMON=embedded` 进程内模式已在 T12 删除，无双轨）；最终验收矩阵（T13）是 Phase 1 的验收闸门。Phase 2、Phase 2.5、Phase 3 未启动。
**日期：** 2026-09-20（修订 2026-10-15）
**演化关系：** 建立在 `2026-09-20-session-resident-views-design.md` 已核实的 runtime 常驻现状之上（同 workspace 切 session / 跨 workspace 均不停、切回复用）。移动端 app/PWA/推送**不在本 spec**，另行立项；本 spec 只交付通道与配对。
**参照：** Paseo relay 全链路（`packages/relay/`、`packages/server/src/server/relay-transport.ts` 553 行、`pairing-offer.ts`、`public-docs/security.md`、`connectivity.md`）；官方 relay 服务端 getpaseo/paseo-relay（Elixir，fly.io）与社区 Go 实现（zenghongtu/paseo-relay）。

## 1. 问题

1. **host 栈困在 Tauri 主进程里。** NativePiManager、HostServer、MetadataStore、owner registry 全部由 `main.rs::setup_native_runtime` 创建并 `app.manage`（main.rs:1672-1776）；窗口销毁触发的清理会杀掉全部 Pi 子进程（child_supervision + owner registry 窗口清理）。结果：关窗即断执行，无持久 agent——与 Paseo「daemon 独立进程、由 app 显式托管启停」的核心差异。
2. **无外网通道。** loopback-only 绑定 + 「不放 LAN」是既定安全边界（ARCHITECTURE）；远程/移动端无法到达本机。
3. 目标形态（与 Paseo 一致）：持久 daemon 独立进程 + daemon **出站**连接公司服务器上的 relay + E2EE 端到端加密 + 配对信任锚，为移动端铺路。

## 2. 已验证事实

### Paseo 侧（本轮源码核实）

| 事实 | 证据 |
| --- | --- |
| relay 协议 v2：`GET /ws?serverId=&role=server\|client&v=2[&connectionId=]`；daemon control socket（每 serverId 一个）收 `sync/connected/disconnected`，per-connection data socket，client socket 由 relay 分配 connectionId；`GET /health` | `paseo/packages/relay/src/cloudflare-adapter.ts` 头注释、`protocol/src/daemon-endpoints.ts::buildRelayWebSocketUrl` |
| relay 是零认证哑管道，只转密文；安全完全由 E2EE 承担 | `cloudflare-adapter.ts` 全文（无 auth/token 逻辑）+ `public-docs/security.md` |
| E2EE：daemon 持久 Curve25519 keypair（`~/.paseo/daemon-keypair.json`）；握手 `e2ee_hello`（client 公钥）→ `e2ee_ready`；NaCl box（XSalsa20-Poly1305）；**每连接独立共享密钥**；握手完成前 daemon 不处理任何命令帧 | `packages/relay/src/e2ee.ts`、`encrypted-channel.ts`、`daemon-keypair.ts` |
| 配对 offer = 信任锚：`{v:2, serverId, daemonPublicKeyB64, relay:{endpoint,useTls}}` → `https://app…/#offer=<base64url>` → QR | `protocol/src/connection-offer.ts`、`server/src/server/pairing-offer.ts` |
| daemon 侧 relay transport：control socket 重连退避 1s→30s 封顶、WS 协议层 ping 10s 周期 / 30s stale 强断、`sync` 对账已连接 client、每 client 独立 data socket + 独立 E2EE 通道、15s open timeout | `server/src/server/relay-transport.ts:117-400` |
| 通道内跑 daemon 普通 WS 会话协议（`attachSocket(transport:"relay")`），relay 对上层透明 | `relay-transport.ts` `attachSocket` 注入 + `websocket-server.ts` ExternalSocketMetadata |
| relay 默认关闭，启用需显式确认（「relay off until enabled」） | `public-docs/security.md`、connectivity.md |
| relay 服务端三份开源实现：官方 Elixir（getpaseo/paseo-relay，生产 fly.io）、TS/Cloudflare DO（packages/relay，wrangler 部署 relay.paseo.sh）、社区 Go 单二进制（zenghongtu/paseo-relay，协议兼容，Docker 镜像） | wrangler.toml、GitHub |
| daemon 生命周期由 desktop app 托管：启动时探测（pidfile `paseo.pid` + 实例一致性校验 + `kill(pid,0)` 探活）→ 未跑则 spawn；app/daemon 版本不匹配自动 stop+重拉 | `packages/desktop/src/daemon/daemon-manager.ts:139-149,267-320` |
| **quit 默认停 daemon**：`daemon.keepRunningAfterQuit`（默认 false）为 true 才存活；`daemon.manageBuiltInDaemon`（默认 true）可关托管改连外部 daemon；before-quit 钩子经 CLI 停 daemon，带 shutdown 反馈 | `quit-lifecycle.ts:49-53`、`desktop-settings.ts:37-40`、`main.ts:1029-1049` |
| **权限跟 principal 走，不跟 transport 走**：准入通过后 direct/relay 一律 `OWNER_PERMISSIONS` 全量面；transport 字段仅作运行时计数；另有过渡期 hack `COMPAT(relayPasswordOptional)` 对 relay 无凭证放行（待旧移动版下架后移除） | `packages/server/src/server/session-admission-auth.ts` 全文、`websocket-server.ts:996-1003` |
| 前端 host 是一等公民：HostConnection 五型（directTcp/directSocket/directPipe/remoteSsh/relay）+ HostProfile 多 host 保存；配对链接（URL/QR/粘贴）新增 host 或已存 host 的 key/relay/TLS 变化需用户确认（TOFU）；桌面（Electron）与移动共用同一前端 | `packages/app/src/types/host-connection.ts:54-69`、`runtime/host-runtime.ts:1955-1975`、`desktop/src/main.ts:780-790` |
| relay 自建入口在 UI 之外：config.json `daemon.relay.{enabled,endpoint,publicEndpoint,useTls}` 或 env `PASEO_RELAY_*`；offer 链接前缀由 `app.baseUrl`（Web 客户端托管地址）决定，客户端从 offer payload 取真实 relay 地址，两者独立 | `config.ts:300-323,551`、`config-environment.ts:32-36`、`connection-offer.ts:43-50` |

### Picot 侧（本轮源码核实）

| 事实 | 证据 |
| --- | --- |
| **远程客户端骨架已存在**：WS hello 已支持 `clientType:"remote"` + `deviceToken` 认证，通过后进入 `HostClientContext::remote(client_id)` | `host_server.rs:1428-1482` |
| **配对与令牌已存在**：`RemoteAuth`（pairing token 5 分钟一次性 → device token 持久存 DB）；HTTP 端点 `POST /v2/auth/exchange` | `remote_auth.rs` 全文（197 行）、`host_server.rs:390,3143-3155` |
| host_server.rs:3160 注释明示 remote surface 是规划的「Gate B」工作（reads/visibility/commands 权限面待定） | 同上 |
| Cargo 已有 tokio-tungstenite（WS 客户端，relay 出站可用）、sha2、uuid、rand；缺 X25519/NaCl box（RustCrypto `crypto_box` crate 一个补齐） | `src-tauri/Cargo.toml:39-51` |
| host 栈规模：main.rs 4327 行 / host_server.rs 6253 / native_pi_manager.rs 2701 / window_owner.rs 1258；状态对象已 Arc 共享，由 setup_native_runtime 集中创建 | wc + main.rs:1672-1776 |
| runtime 常驻已实现（本仓库 09-18 成果 + session-resident-views spec 核实）：切换不停、切回复用 | `main.rs workspace_transition_commit` 注释、`find_existing_runtime_for_prepare` |
| 窗口销毁清理按 owner 记录杀子进程（关窗即全停）：`handle_window_destroyed` 直调 `stop_for_owner` + `stop_for_window_destroy` + ephemeral owner_cleanup | main.rs:4705-4735、child_supervision.rs、owner registry 窗口清理 |
| **生产零 Tauri command**：WebView 生产路径全部走 HostServer v2 WS/HTTP（`invoke_handler(generate_handler![])` 为空，注释明示无 legacy IPC） | main.rs:5023-5026 |
| **HostServer 同时服务 WebView 静态资源**：ServeDir + `/v/<fingerprint>/` 版本化前缀 + `<base href>` 改写 + no-store（防 WebView 缓存钉死旧资产）；窗口以 `WebviewUrl::External(host origin)` 创建，capability init script 与导航授权由 app 侧注入 | host_server.rs:314-356、main.rs:725-760 |
| **host 端口临时分配、无持久化**：`bind((host, 0))` 端口只存进程内 atomic；注释「stable port across restarts」与实现不符（过时表述） | host_server.rs:455-468 |
| **退出/信号清理路径**：`RunEvent::Exit` 停全部 runtime + 清子进程注册表；SIGTERM/SIGINT/SIGHUP 另有 termination handlers 兜底（tauri dev Ctrl-C / 登出 / 关终端）；SIGKILL 不可捕获 | main.rs:5033-5100 |
| tauri-plugin-shell 2 已在依赖（sidecar spawn 就绪，无需新增插件） | src-tauri/Cargo.toml:27 |

## 3. Phase 1：picot-daemon 独立进程

### 3.1 目标形态（Paseo 同构）

新 binary `picot-daemon`，承载 host 栈全部：NativePiManager、HostServer（**含 WebView 静态资源服务**）、MetadataStore、RemoteAuth、EphemeralRegistry、SkillSourceRegistry、GitService、TerminalManager。Picot.app 退化为本地客户端：经 loopback（127.0.0.1，admin RPC 同限）连 daemon（WebView 与 GUI 壳走同一条 HTTP/WS 路径），daemon 不监听 LAN/公网（§4.4）。窗口关闭只影响窗口；runtime 的宿主是 daemon，app 是 daemon 的管理面板（启停语义见 §3.2）。与 Paseo 的同构面 = 协议与生命周期语义；实现差异（无 CLI、无外部 daemon/托管暂停）见 §3.2.2 与 §8。

### 3.2 生命周期（对齐 Paseo 已核实行为）

管理权在 app，daemon 是被管理对象（Paseo：`manageBuiltInDaemon` 默认 true）。三层语义分开，不混为一谈：

| 用户动作 | daemon 行为 |
| --- | --- |
| 关窗口（现状语义 = 退出 app） | Picot 关窗即退出（末窗关闭 → app 退出），走下行「Cmd+Q 退出」路径。窗口层不直接触及 daemon；是否改 macOS 惯例 hide-on-close（关窗不退、agent 照跑，独立于 daemon 的窗口策略）待另行拍板，不在本 spec |
| Cmd+Q 退出（默认） | 优雅停 daemon：drain、落盘、终止 pi 子进程、清 pidfile；UI 给「正在停止」反馈 |
| Cmd+Q 退出（开启「退出后保持运行」） | daemon 存活；`keepRunningAfterQuit`，默认 false |

- **启动序**：app 探测（实例文件 + 实例一致性校验 + `kill(pid,0)` 探活 + `/health`）→ 未运行则 spawn（detached，不进任何 kill-on-exit 名单）→ 版本不匹配自动 stop+重拉（app 升级后老 daemon 仍在跑，照抄 Paseo daemon-manager）→ 等 ready → 连接。
- **实例文件（pidfile 扩展）**：pid / listen origin / startedAt / version，tmp+rename 原子写，Paseo daemon instance 同构。HTTP/WS 端口仍临时分配（现 bind 0 无持久化，§2「stable port」注释已过时），daemon ready 后把 origin 写入实例文件，app 读到 origin 才造窗与连接；ready 等待超时 15s，超时给明确错误与日志路径。
- **崩溃恢复**：主路径 = 下次开 app 按需拉起。launchd LaunchAgent（登录自启 + 崩溃自动重启）降级为设置页 opt-in、默认关——不装用户没要求的常驻后台；Windows 服务 / Linux systemd 同为后续 opt-in 增量。
- **重启**：设置页按钮（§3.2.2）→ admin RPC（仅 loopback desktop capability；remote 客户端不开放停机权限——管理面专属约定，非传输收窄）。重启 = agent 停止，恢复的是磁盘现场而非跑到一半的 turn；确认弹窗列出正在跑的 agent。**2026-10-16 修订（窗口落位）**：重启排空全部 runtime，因此窗口迁移时 **session 路由降级为 landing**——捕获到的 `/workspaces/{id}/sessions/{id}` 丢弃 route 与 workspace 绑定，窗口落在新 origin 的 landing 页。理由是死 runtime 上的聊天视图有误导性：用户看到的是一个可用 UI，而输入框背后的 runtime 已经不存在（输入框骗人）。导航镜像的路由保真机制本身不变，留给未来「不排空 runtime」的迁移场景（如 §3.2.1 的 companion 订阅迁移）。**关闭**：不设设置页按钮（2026-10-15 拍板），仅由 quit 流程触发（开关为 false 时）。
- **远程访问联动**：启用 relay 必须同时开启「退出后保持运行」，否则 quit 即断连，远程访问形同虚设。
- **单实例**：文件锁；重复启动即退出复用现有实例。
- **设置页展示**：见 §3.2.2。

### 3.2.1 会话视图绑定不变量（为 companion session-chat 预留）

daemon 使 session 成为可独立于 GUI 持续的对象，main window 与将来的 companion panel 都只是 session 的订阅视图。**同一 session 在整个窗口内最多绑定一个可写聊天视图**（main 与所有 companion tab 合计）；用户试图在第二处打开它时，必须聚焦并显露既有视图，绝不创建第二个 composer。关闭 companion 只解除订阅，不 stop runtime、不关闭 session。v1 明确不支持同一 session 的多视图/双 composer：它意义不足以抵消输入 owner、草稿、queue/steer、焦点交接和读写不对称的额外契约；若未来出现真实需求，另立 spec。

### 3.2.2 设置页 daemon 段（Settings → General，2026-10-15 Dr. Lin 拍板）

- **无 Picot CLI**：Paseo 靠外部 CLI（`paseo daemon start|stop|restart|status`）管理 daemon；Picot 没有等价 CLI，daemon 启停由 app 进程 Rust 侧直接承担（spawn / kill / pidfile 探活 / 版本比对），设置页经 Tauri command 调用，运行中管理动作走 admin RPC。
- **位置**：Settings → General 页尾（Embedded Pi 段之后，General 页最后一个 section）。
- **控件（三个）**：
  1. **状态行**：daemon 状态（running/stopped）+ version，照 Paseo status probe。
  2. **开关「退出 Picot 后保持 daemon 运行」**（`keepRunningAfterQuit`，默认 false = 启动 Picot 时启动 daemon、quit Picot 时 shutdown daemon）。**改值立即生效**：改的是当前 daemon 实例的 quit 策略；因不引入 Paseo 的 `manageBuiltInDaemon`（外部 daemon / 托管暂停，YAGNI），开关只决定 quit 时是否 shutdown，不改变当前托管关系。文案明示生效语义。
  3. **Restart daemon 按钮**：confirm 弹窗列出正在跑的 agent；确认后走 admin RPC 重启，app 自动重连。
- **不设 Shutdown 按钮**：shutdown 仅由 quit 流程触发（开关为 false 时）；「回到干净状态」诉求由 Restart 覆盖——避免「daemon 已停、Picot 还开着、无 runtime 可用」的死胡同状态。
- **存储与真相源**：`keepRunningAfterQuit` 是 app 退出策略，存 app 私有配置文件（`app_config_dir` 下 JSON，Paseo config.json 同构），**由 app 进程独占读写、daemon 不感知此值**；before-quit 钩子读它决定是否调 `daemon.stop`，无双进程真相同步问题。**不进 MetadataStore**（Phase 1 后 app 与 daemon 只经 loopback 交互、不共享 SQLite 句柄，§3.3 第 7 条）。toggle 改值只更新文件与 UI 文案，**不触发 daemon 任何动作**（不重启、不暂停、不探活）；下次 quit 走新策略。

### 3.3 迁移步骤（工程序）

0. **进程模型先行**：ARCHITECTURE.md 先落「进程模型」章节（app 进程 / picot-daemon 进程 / WebView / pi 子进程、loopback 端口与实例文件、资源路径、SQLite 归属），Phase 1 代码动工前合入，后续步骤以此对照。
1. **HostServer 整体外迁**：host 栈全部状态对象（HostServer 含 axum 全部 HTTP/WS 路由与静态资源服务、capability 校验、MetadataStore、RemoteAuth、EphemeralRegistry、SkillSourceRegistry、GitService、TerminalManager、NativePiManager）迁入 daemon 的 `DaemonState`（纯 tokio，无 Tauri 依赖）。对象已是 Arc 共享，主要工作是把 `main.rs::setup_native_runtime` 的构造与 axum 接线整块剥到 daemon 入口。**WebView 静态资源服务随迁**（index.html/JS/CSS 现由 HostServer 提供，§2）；app 窗口 `WebviewUrl::External(daemon_origin)`，拿到 origin 才造窗。生产路径现为零 Tauri command（§2），无「残余 command 收编」工作。
2. **capability 与 owner 边界切分**：capability 签发与校验**留在 daemon**（token 唯一签发方）；owner↔窗口绑定**留在 app**（window_owner.rs 是 Tauri API 强耦合：window label、capability init script 注入、导航授权，daemon 看不到 Tauri 窗口事件）。app 造窗时向 daemon 申请该 owner 的 capability token 注入 init script，后续请求由 daemon 校验；app 进程自身以 desktop capability 客户端接入（现有 `clientType:"desktop"` 路径）。
3. **daemon loopback admin RPC**：daemon 监听仅 127.0.0.1 的 admin 端点：`daemon.status` / `daemon.stop(reason)` / `daemon.restart(reason)`。设置页 Restart 按钮、quit 钩子、启动序探测统一走它；仅 app 进程可达（管理面 capability，见 §4.4），不对任何 transport 开放。
4. **窗口销毁语义反转**：现 `handle_window_destroyed` 进程内直调 `stop_for_owner` + `stop_for_window_destroy`（§2）；daemon 化后改为对 daemon 的两种显式语义：`detach_owner`（关窗：撤 capability 与 UI 绑定，workspace/session 按 session-resident 语义保留）与 `close_workspace`（显式关闭工作区动作才终止 pi 子进程）。既有 e2e「关转场窗口杀子进程」条目语义同步改写。
5. **打包**：picot-daemon 作为 Tauri sidecar（`bundle.externalBin`），macOS/Windows 各目标三元组独立签名；tauri-plugin-shell 已在依赖（§2）。dev 开关 `PICOT_DAEMON=embedded|external` 见 §3.4；`scripts/build.sh` 增 sidecar 构建步骤。
6. **资源**：daemon 复用 `src-tauri/resources/pi/` 内嵌 Pi（不重复打包）；fetch:pi 流程不变。
7. **数据目录**：daemon 直接使用 app_data_dir（同一 SQLite），app 与 daemon 只经 loopback HTTP/WS 交互，不再共享文件句柄。app 侧私有状态（如 §3.2.2 的 daemon 开关）存 app_config_dir 下自有 JSON，不触碰 SQLite。

### 3.4 兼容与回滚

开发期保留环境开关（如 `PICOT_DAEMON=embedded|external`）：embedded 即现行单进程模式。验证一个迭代后删 embedded 路径（不留双轨，对齐仓库无向后兼容惯例）。

### 3.5 测试

全量 `bun run test` + `check:rust`；新增：
- **信号分档**：`RunEvent::Exit` / SIGTERM / SIGINT / SIGHUP（现 termination handlers 路径，§2）→ 优雅停 daemon（toggle off 时；agent 停止、无孤儿 pi 进程）；SIGKILL Picot.app → daemon 必然存活（detached 收不到信号），重开接回现场（含运行中 turn）。
- 开启「退出后保持运行」后 quit 存活、重开接回现场。
- **daemon 进程被 SIGKILL**：app 断连、设置页状态行反映 stopped/errored；重开 app 按启动序拉起，pi 孤儿接管或明确降级语义（实现时定案）。
- **双 app 实例**：daemon 单实例文件锁；第二个 app 探测到 daemon 已被托管即直接退出并提示（无 attach 模式，§8）。
- 版本不匹配自动 stop+重拉。
- daemon 启动超时（15s 无 ready）：app 给明确错误 + 日志路径，不闷死。
- 设置文件损坏：fallback `keepRunningAfterQuit=false`，不阻塞启动，日志 warn。
- 记录 app / daemon 双进程内存基线（实现期填数）。

### 3.6 已拍板：一步到位独立进程（2026-10-14，Dr. Lin）

托盘常驻（app 关窗不退、不杀 runtime）否决：改动虽小但拿不到真 daemon——移动端连的还是桌面进程，app 退出仍全停，与 relay 目标矛盾，且事后仍要二次拆分。「关窗不断」由「退出后保持运行」设置或窗口策略（hide-on-close，待另行拍板）达成，无需托盘跳板。

## 4. Phase 2：relay 接入（daemon 出站 + E2EE + 配对）

### 4.1 模块（src-tauri/src/relay/）

- `transport.rs`：control socket（wss → `relay/ws?serverId&role=server&v=2`）+ per-connection data socket + 重连退避（1s 起、30s 封顶）+ WS 协议层 ping/pong（10s 周期 / 30s stale 强断）+ `sync` 对账 + 15s open timeout。逐条对齐 `paseo relay-transport.ts`，协议参数不再发明。
- `e2ee.rs`：持久 Curve25519 keypair（存 metadata DB，等价 PaseO daemon-keypair.json）；`e2ee_hello`/`e2ee_ready` JSON 明文握手帧；NaCl box XSalsa20-Poly1305 帧加密。RustCrypto `crypto_box`（x25519-dalek + xsalsa20poly1305）补一个依赖。密文帧第一版 base64（Paseo 已验证路径），binary 能力位后续。
- `pairing.rs`：offer 生成（serverId + 公钥 + relay 公网 endpoint → 链接 + QR 码 payload）。serverId = uuid，存 DB。

### 4.2 通道复用（Picot 的独有优势）

data socket 解密后的帧，经 adapter 接进 HostServer 既有 WS 会话管线：远程客户端走 **已存在的** `clientType:"remote"` + `deviceToken` hello 路径（§2）。HostServer 近零改动；Paseo 需要的「通道内应用层认证」Picot 已有实现。

### 4.3 配对全流程

1. 桌面端 Settings「远程访问」页 → 生成 relay 配对 offer（QR/链接；显式确认才启用 relay，对齐 Paseo 默认关闭）。
2. 移动端扫码 → 得 serverId/公钥/endpoint。
3. 移动端连 relay（role=client）→ E2EE 握手。
4. 通道内调既有 `/v2/auth/exchange`：输入/内嵌 pairing token → 换 device token（持久化于设备）。
5. 后续连接：hello 携 device token 即认证完成。

### 4.4 安全模型（ARCHITECTURE.md 须同步新增章节）

- **relay 不受信**：只转密文；见 IP/时序/包大小/serverId。信任锚 = 配对 offer（QR 按密码对待，不公开分享）。
- **远程身份与权限面（Gate B，2026-10-14 修订：权限跟 principal 走，不跟 transport 走）**：Paseo 同构——准入通过后 direct/remote 一律全量 owner 面（§2 `session-admission-auth.ts`），remote 客户端不按传输收窄，git / terminal / 文件写照常可用；不建「transport 维度」gate 层，host_server.rs:3160 的原收窄设想作废。**认证强制 deviceToken**：E2EE 握手后走既有 `/v2/auth/exchange`；不引入 Paseo 的 `relayPasswordOptional` 无凭证放行。风险敞口 = 设备失窃即全权控制，兜底为 per-device 撤销 + keypair 轮换。loopback desktop 路径语义不变。
- **撤销**：device token DB 删除即踢设备；keypair 轮换 = 全部重新配对。
- **管理面与数据面分离**：admin RPC（`daemon.status/stop/restart`，§3.3 第 3 条）是独立 capability，仅 loopback 的 app 进程持有；不对任何 transport（含 relay）开放：管理面根本不进 principal 权限面，是通道级隔离，不是权限收窄。
- **不动的事**：「不放 LAN」决策保持——远程一律走 relay 出站，daemon 永远不监听公网/局域网。

### 4.5 测试

- Rust 单测：E2EE 握手/加解密往返；用 Paseo `packages/relay` 测试向量做帧格式兼容断言。
- 本地 e2e：docker 起社区 Go relay（或官方 Elixir 镜像），daemon 出站连它，模拟客户端走完配对→会话→断连重连。
- relay 不可达：本地功能零影响、退避重连（日志断言）。
- Phase 2.5 增补：桌面客户端连远程 host 走完配对 → TOFU 确认 → 会话 → 断连重连；已存 host 的 key 变化触发确认框。

### 4.6 Phase 2.5：桌面多 host 客户端（Paseo 同构）

动机：移动端弱，先让桌面版成为可连接任意 host 的完整客户端（默认本机 daemon，可添加远程 host 的 relay 链接），移动端后续降级为薄客户端。

- **client-role 镜像**：§4.1 三模块的客户端侧——连 relay（`role=client&serverId=`）、单 data socket（无 control/sync/多 client 扇出）、消费 offer（serverId + daemon 公钥 + endpoint）、E2EE 握手；通道内走既有 `clientType:"remote"` + deviceToken。严格比 server role 简单。
- **桥接**：app（Rust）终结 E2EE 通道，为每个远程 host 在 loopback 暴露一个代理 WS 端口；WebView 视角每个 host 就是一个普通 HostServer 端点——前端消息协议零改动，改动集中在连接管理层。
- **host 模型（学 Paseo）**：HostProfile（label/appearance/连接方式）+ host 切换器；已存 host 与 deviceToken 入钥匙串；配对链接（粘贴/QR）新增 host 或已存 host 的 key/relay/TLS 变化需用户确认（TOFU，照抄 host-confirmation 模式）。
- **权限**：remote host = 全量 owner 面（§4.4 修订）；**不做**按 host 的面板隐藏——原「能力门控」工作项随 Gate B 修订取消。
- **设备管理**：设置页列出已配对设备，撤销 = 删 deviceToken。
- **不做**：directTcp / remoteSsh 连接型（Paseo 有）——维持 §8「LAN 直连、SSH 隧道不进产品」。
- **依赖**：Phase 2 三模块 + 既有 auth/exchange；不依赖 Phase 3——开发期可连公共 relay.paseo.sh 验证全链路（元数据暴露给其运营方，开发期可接受），生产换公司 relay。

## 5. Phase 3：公司服务器部署 relay

### 5.1 选型判断（待核实后定案）

默认推荐**社区 Go 单二进制**（zenghongtu/paseo-relay）：资源占用最小、单文件部署、协议 v2 兼容已由其文档与 main.go 与官方对齐记录佐证；官方 Elixir 镜像为备选（功能同、运行时更重）。若公司已有 Cloudflare 使用习惯，TS/DO 版亦可。**此项在 sysadmin 核实后定案，不在本 spec 锁死。**

### 5.2 部署要件

- 反代：nginx/Caddy 终结 TLS，`/ws` WebSocket upgrade + `/health` 透传（Paseo README 给有最小 nginx 配置）。
- 防火墙：公网仅入站 443；daemon 侧仅出站 wss。
- 容量：无状态字节转发，内存 ≈ 并发连接 × 每连接缓冲（Go 版默认 200 帧上限）。
- 落盘：零用户数据；仅访问日志（IP/时序）。

### 5.3 待系统管理员核实清单

1. 服务器选型与资源（可用哪台、Docker 权限有无）。
2. 域名与 TLS 证书签发（公司 CA vs Let's Encrypt；`relay.<company>` 子域）。
3. 员工机出站防火墙是否放行 wss:443（relay 模式只出站，通常无障碍，需确认）。
4. 运维归属：健康检查接哪个监控、升级流程。

## 6. 验收条件

- 崩溃/SIGKILL 强杀 Picot.app：agent 继续执行（不走退出清理路径，detached daemon 收不到信号）；重开接回现场（含运行中 turn）。优雅退出路径（Cmd+Q / SIGTERM / SIGINT / SIGHUP）按 §3.2 表与 §3.5 信号分档走。关窗现状 = 退出 app。
- 设置页 General → Daemon 段（§3.2.2）：状态行实时反映 daemon 状态与版本；「退出 Picot 后保持 daemon 运行」开关改值立即生效（随后 quit 行为随之改变）；Restart 按钮带确认弹窗（列出正在跑的 agent），重启后 app 自动重连。
- 退出 Picot.app（默认）：daemon 优雅停止，无孤儿 pi 进程；开启「退出后保持运行」后退出：agent 继续执行，重开接回现场。
- daemon 仅出站连接公司 relay；经公网（手机热点）配对后，`?mobile=1` 浏览器客户端可读时间线、发 prompt、收到流式输出。
- relay 服务器不可达：本地零影响，重连退避恢复。
- `bun run test`、`bun run check:rust` 全绿；ARCHITECTURE.md 新增进程模型与远程信任维度章节（含与「不放 LAN」决策的关系说明）。

## 7. 规模与顺序判断

Phase 1 大（进程边界重构 + 窗口清理语义反转，main.rs/host_server 是主战场）；Phase 2 中（三模块，协议参数有 553 行参照逐条抄）；Phase 2.5 中偏小（client-role 镜像 + 前端 host 管理/TOFU/设备管理；能力门控已随 Gate B 修订取消）；Phase 3 小（部署 + 核实）。1→2 强依赖（relay 连的必须是持久 daemon）；2.5 依赖 2、不依赖 3；3 与 2 可并行推进（核实清单先行）。

## 8. 不做的事

- 移动端 app/PWA/推送通知（APNs/FCM）——单独立项；本 spec 交付的通道与配对即其地基。Phase 2.5 落地后桌面已可远程访问，移动端可降级为薄客户端、优先级下降。
- Hub 类触发器产品（GitHub/Slack 事件启动 agent）。
- LAN 直连、SSH 隧道等其它通道（Tailscale 用户可自行直连，不在产品内建）。
- 外部 daemon 接入（Paseo `manageBuiltInDaemon=false` 的同形能力）——Picot 无 CLI，Phase 1 只支持 app 托管的内置 daemon，不做「托管暂停 / 连外部 daemon」开关；若未来要连外部 host，属 Phase 2.5 客户端侧工作。
