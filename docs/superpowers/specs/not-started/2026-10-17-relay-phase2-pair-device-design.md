# Relay Phase 2：daemon 接入 relay 与 Pair Device 设计

**状态：** Draft（修订 1）— 军师评审（2026-10-17，R1–R10）已逐条吸收，见 §11；三项拍板（§9）不推翻；待 Dr. Lin 复审
**日期：** 2026-10-17（同日修订）
**演化关系：** 母 spec `implemented/2026-09-20-persistent-daemon-relay-design.md` §4（Phase 2）的实施化设计。协议事实不重复调研——全部引母 spec §2 事实表；本文只做任务分解需要的落点设计（模块归属、数据落盘、UI 形态、配置入口）与新增决策。Phase 2.5（桌面多 host 客户端）、Phase 3（relay 服务端运维）不在本文。
**参照：** Paseo `packages/app/src/desktop/components/pair-device-section.tsx`（393 行）、`pair-device-modal.tsx`、`screens/settings/host-page.tsx:262-272,1095-1122`、`packages/relay/src/{crypto,encrypted-channel}.ts`、`packages/protocol/src/connection-offer.ts`、`packages/server/src/server/{relay-transport,pairing-offer}.ts`。

## 1. 决策记录

- **需求（Dr. Lin，2026-10-17，原文）**：「学 Paseo，把 Picot daemon 注册到公司 relay（endpoint: paseo.021521.xyz:443，我刚探测 /health 200 v0.5.0 活着），获取唯一的一个 key 和 relay 链接；在 Settings→General→Daemon 页增加 "Pair Device" 功能，显示 relay URL（参照 Paseo 设置页的 Pair device 做法）」。
- **relay 已上线**：公司 relay `paseo.021521.xyz:443` 运行中（Dr. Lin 2026-10-17 探测 `/health` 200、版本 v0.5.0）。母 spec §5.3 的选型/部署核实项已由既成事实收编，不在本 spec。
- **Phase 1 已交付**（daemon-phase1 分支，验收见 `process-evidence/2026-10-16-picot-daemon-phase1-e2e.md`）：`picot-host`（零 Tauri 守卫）+ `picot-daemon` 独立进程 + admin 通道 + 设置页 Daemon 段 + `keepRunningAfterQuit`。HostServer 已硬锁 loopback（`host_server.rs:463-466`），注释明示远程访问的下一步是本 spec。
- **「唯一的一个 key」的语义**：daemon 首次启用 relay 时生成持久 Curve25519 keypair + serverId（uuid），一次生成、跨重启不变。这就是 Paseo `daemon-keypair.json` 的对应物；keypair 不轮换、不重置（v1，见 §7）。
- **修订依据（2026-10-17）**：军师复核结论「需修订后进实施」。原稿三条技术前提被证伪并已改设计：remote 权限面并非现成（`host_router.rs:265-277` 非 Desktop 的 host_request 一律 `forbidden_class`，见 §2.8 Gate B）；公共 preferences 面不能存私钥（`metadata_store.rs:864-942` 只隔离 `runtime.*`，见 §2.3）；tokio-tungstenite 0.24 未启用 TLS 后端连不了公司 WSS（见 §2.5）。逐条处置与落点见 §11。

## 2. 架构

### 2.1 模块归属：`picot-host/src/relay/`，由 DaemonState 持有

结论：relay 全部代码放 **picot-host**（新模块 `src/relay/`），`DaemonState` 增字段持有并在 daemon 启动序里按配置拉起。理由：

1. E2EE 解密后的帧必须喂进 `host_server` 的会话管线，`host_server` 就在 picot-host，同进程注入零跨进程协议；放 picot-daemon bin（现 443 行入口）则库无法引用，放 Tauri app 则 relay 存活期错绑在 app 上（app 可退出，daemon 常驻）。
2. 零 Tauri 守卫（`scripts/check-daemon-no-tauri.js`）自动覆盖新模块。
3. 依赖大体就位：picot-host 已有 `tokio-tungstenite 0.24`、`base64 0.22`、`rand`、`uuid`、`futures-util`（`crates/picot-host/Cargo.toml:28-53`）。两处缺口：tokio-tungstenite 0.24 默认构建不含 TLS 后端（连不了 `wss://`，需启用 feature，§2.5）；新增 `crypto_box`（§2.4）。

模块划分照母 spec §4.1：`relay/e2ee.rs`（keypair + NaCl box + 握手帧）、`relay/pairing.rs`（offer 编解码与生成）、`relay/transport.rs`（control/data socket 与生命周期）。

### 2.2 会话注入：泛型化 `handle_websocket`，等价 Paseo `attachSocket(transport:"relay")`

现状 `host_server.rs:1507` `async fn handle_websocket(mut socket: WebSocket, state)` 把 axum `WebSocket` 类型钉死在整条会话管线（hello → hello_ack → 事件订阅 → 请求循环）。改造为接受任何 `Stream<Item=Message> + Sink<Message>` 的会话入口（axum upgrade 路径不变，零行为变化，现有测试全绿为准）；relay 侧提供内存通道适配器：解密帧 → 适配器 → 会话管线，回程反向。这就是 Paseo `relay-transport.ts:945` `attachSocket(ws, {transport:"relay", relayConnectionId})` 注入点的 Picot 对应物。

**连接身份三分（评审 R5）**。relay 会话涉及三种 ID，职责不得混用：

- **声明 ID**：hello 帧的 `clientId`，客户端自报，只作展示/日志记录，不作路由键；
- **路由 ID**：relay 分配的 `connectionId`，transport 层维护「connectionId ↔ 会话任务」映射；
- **内部连接 ID**：host 分配（`relay:<uuid>`），`HostRouter` 的注册键。

现状 router 按声明 ID 注册、同名覆盖、断开按同名删除（`host_router.rs:202-207`），relay 场景会误删重连的新连接。泛型化后 relay 会话一律以内部 ID 注册；重连即新内部 ID，旧连接清理与新连接互不影响。loopback 路径沿用现状（声明 ID 即注册键），不改既有行为。

**relay 会话只接受 remote 准入**。会话入口携带 transport 标记（`ExternalSessionMeta::Relay`）；relay 通道上的 `clientType:"desktop"` hello（无论是否携带本地 capability）一律拒绝——本地 owner 通道只可能是 loopback。

会话协议零改动（配对扩展见 §2.6）：通道内客户端照发 `hello{clientType:"remote", deviceToken}`（`host_server.rs:1561-1570` 认证路径不动）。

### 2.3 身份持久化：metadata DB 保留命名空间 + 专用内部接口

keypair + serverId 存 SQLite `preferences` 表，单键 `relay.daemon.identity`，值 `{serverId, publicKeyB64, secretKeyB64}`；`relay.enabled` 同放 `relay.*` 键空间。同库理由不变：device token 已在同一库（`remote_auth.rs:70-76`），身份与授权同库同备份（备份语义见 §5）。

**不得走公共 preferences 接口（评审 R2）**。现状公共读写删除与空前缀 list 只隔离 `runtime.*`（`metadata_store.rs:864-942`）；若照原稿直接复用，任何本地页面都能读走私钥、改写身份、绕过启停状态机。修订：

1. `relay.*` 设为保留命名空间：`pref_get`/`pref_set`/`pref_delete`/`pref_list` 对 `relay.` 前缀键一律拒绝，list 结果同时过滤 `relay.*` 行——与 `runtime.*` 同款守卫。
2. identity 与 enabled 经专用内部方法读写（`MetadataStore` 新增，仅供 host 内部调用，不经 host_request 暴露）。
3. **并发首建**：load-or-create 在 store 互斥锁内完成「读→建→写」，两个线程并发首启用得到同一 identity（测试锁定）。
4. **损坏拒绝静默重建**：identity 行 JSON 无效或字段/长度不符 → 返回错误、拒绝启动 transport，不自动重生成。身份私钥是信任锚，静默换 key 等于全部设备失配且用户无感知。修复路径 = 手动删键（设备管理 UI 属 Phase 2.5）。
5. **私钥不外泄**：`relay.daemon.identity` 不出现在公共 preferences 返回、不进日志（`DaemonIdentity` 手写 redacted Debug；对齐 `tests/token_redaction.rs` 先例）。测试覆盖。

### 2.4 E2EE crate：RustCrypto `crypto_box`

`crypto_box = "0.9"`（x25519-dalek + XSalsa20-Poly1305），与 Paseo 的 tweetnacl `nacl.box` 加解密互操作（同一 NaCl box 构造）。wire 格式逐字节照 Paseo：密文帧 = 文本帧，内容为 `base64(nonce[24] || ciphertext)`（`paseo crypto.ts:5-9`、`encrypted-channel.ts:475-482`）；握手帧明文 JSON：`{"type":"e2ee_hello","key":"<b64公钥>"}` → `{"type":"e2ee_ready"}`（capabilities/binaryCiphertext v1 不声明，base64 路径是 Paseo 已验证默认）。每连接独立共享密钥；握手完成前不向会话管线递交任何帧（Paseo 同语义，母 spec §2）。

**互操作与边界测试（评审 R8）**：

- 向量必须由 node tweetnacl（Paseo 同库）在临时脚本里一次性生成、以字面量钉进测试；同库 Rust 客户端自环 round-trip 不构成独立验证。
- `seal` 拆两层：`seal_with_nonce`（固定 nonce，测试向量与 byte-exact 断言用）+ `seal`（随机 nonce 封装，生产路径）。
- 拒收面全覆盖：全零 X25519 共享密钥（对齐 `paseo crypto.ts:132-148`）、低阶/全零公钥、错误长度、非规范 base64、截断包、篡改包——一律断连，不进会话管线。
- 恶意 relay 重放语义见 §5 威胁模型。

### 2.5 传输策略与 TLS：逐条抄母 spec §2 事实表

control socket 重连退避 `min(30s, 1s×尝试次数)`（`relay-transport.ts:338`）、WS ping 周期 10s / 30s stale 强断（`relay-transport.ts:56-57`）、control ready 超时 8s、握手超时 10s、`perMessageDeflate` 关闭、`sync` 对账（control 连上后 relay 推全量 connectionIds，daemon 补开缺失 data socket、关多余）、每 client 独立 data socket + 独立 E2EE 通道 + 15s open 超时（`relay-transport.ts:361`）。URL：`wss://<endpoint>/ws?serverId=&role=server|client&v=2[&connectionId=]`。以上参数不在本 spec 重述理由，实现照抄。

**TLS 后端（评审 R3）**。`Cargo.lock:5752-5762` 的 tokio-tungstenite 0.24.0 不含 TLS 后端，默认构建连不了公司 `wss://`；本地 `ws://` mock 全绿发现不了。修订：

- 依赖改为 `tokio-tungstenite = { version = "0.24", features = ["native-tls"] }`。
- 选 native-tls 而非 rustls(webpki-roots) 的理由：① picot-host 已因 reqwest 0.12（默认 native-tls）链接同一 TLS 栈，macOS 上走 SecurityFramework、无 OpenSSL vendoring 负担——启用 feature 只是把既有栈接到 WS 客户端，不引入第二套 TLS 实现；② 公司域名的证书链若来自内部 CA，系统信任库装根证书即可用，webpki-roots（Mozilla 集合）在该场景直接失败。若 Dr. Lin 有理由改 webpki-roots（如跨平台 vendored 构建），差异只是一行 feature 声明。
- 证书根来源与验证策略：标准服务器证书链 + 主机名（SNI）校验，信任锚 = 系统信任库；不自定义根、不在传输层做 TOFU。应用层的信任锚是配对 offer 里的 daemon 公钥（E2EE），传输层 TLS 只负责挡被动嗅探与错误连接。
- 测试：transport 提供 connector 注入 seam（生产默认 connector，测试注入显式信任 fixture CA 的 connector）。受信证书（fixture CA 签发的本地 `wss://`）握手成功；默认 connector 对同一自签证书拒绝。证书/CA 以 PEM 字面量入库（`tests/fixtures/`，离线脚本生成一次、脚本路径记在目录 README），测试不在运行时生成密钥。

### 2.6 配对令牌：offer 内嵌一次性 pairingToken（一扫码完成配对）

Paseo 的 offer 只含 E2EE 信任锚，准入靠 relay password（可选）。Picot 母 spec §4.4 已拍板「认证强制 deviceToken、不引入无凭证放行」。若照搬 Paseo offer 形状，移动端扫完码还得手动抄一个 5 分钟令牌，两步且易错。故 Picot 的 offer payload 在 Paseo v2 schema 上做一个加法扩展：

```jsonc
{ "v": 2, "serverId": "…", "daemonPublicKeyB64": "…",
  "relay": { "endpoint": "paseo.021521.xyz:443", "useTls": true },
  "pairingToken": "picot_pair_…" }   // 可选字段；Picot 配对 offer 才带
```

- 「Pair Device」每次生成 offer 新铸一个 5 分钟一次性 pairingToken（复用 `RemoteAuth::create_pairing`，`remote_auth.rs:42-50`），嵌进 offer。**新铸前作废上一枚未兑换 token**（`RemoteAuth` 新增 `revoke_pending`；create/exchange 的令牌语义不变）。完整生命周期见 §2.10。
- 移动端链路：扫码得 offer → 连 relay（role=client）→ E2EE 握手 → `hello{clientType:"remote", pairingToken, deviceId}` → daemon 在 hello 内调既有 `RemoteAuth::exchange`（`remote_auth.rs:52-76`，语义与 `POST /v2/auth/exchange` 完全同一实现）铸 deviceToken，`hello_ack` 附 `{"deviceToken":"…"}` 回传 → 设备持久化，此后直连路径 `hello{clientType:"remote", deviceToken}` 不变。
- 「输入/内嵌 pairing token」的 HTTP 端点（`/v2/auth/exchange`，`host_server.rs:442`）原样保留服务 loopback 场景；relay 通道无 HTTP，配对走 hello 内嵌这一条新路径。

这是对 Paseo offer schema 的有意加法（Paseo 客户端会忽略未知字段，不破坏互操作），列入 §9 拍板点 3。

### 2.7 公网入口容量边界（评审 R6）

relay 是公网不可信入口，axum 路径的物理帧上限（`host_server.rs:1504` `max_message_size`）挡不住绕开该入口的 relay adapter；无界 writer 只防事件循环阻塞，防不了慢连接拖垮 daemon 内存。全部上限进 `transport_limits.rs` 统一契约：

| 维度 | 上限 | 溢出行为 |
| --- | --- | --- |
| control 帧尺寸 | 64 KiB（新增 `RELAY_CONTROL_FRAME_BYTES`；control 帧只有小 JSON） | 断 control 连接，按退避重连 |
| data 帧密文文本 | 换算后对应明文 ≤ `WS_PHYSICAL_FRAME_BYTES`（base64 膨胀按 4/3 计入检查） | 关该 data 连接 |
| data 连接总数 | 计入 router 既有 `MAX_CLIENTS`（256，`host_router.rs:120`） | 拒绝新连接（既有 `client_limit` 路径） |
| 并发 E2EE 握手 | 4（信号量）+ 握手超时 10s（§2.5 既有） | 超时/超限关该连接 |
| 每连接出站队列 | 既有 `OUTBOUND_QUEUE_CAPACITY`（64，`transport_limits.rs:25`，启用之）——**有界队列取代无界 writer** | 满即关该连接，丢弃未发帧 |
| sync 列表长度 | connectionIds 数 ≤ `MAX_CLIENTS` | 超限断 control（协议异常） |

溢出语义统一：任何上限触发只关当事连接，本地会话与 control 连接不受影响。测试：各上限边界值 ±1、慢消费者（不读）触发队列溢出关连接且本地会话照常、恶意超大帧。

### 2.8 Gate B：remote 会话权限面（评审 R1）

原稿「remote 路径原样复用」不成立：`host_router.rs:265-277` 对所有非 Desktop 的 host_request 返回 `forbidden_class`；`host_server.rs:2306-2344` `authorize_target` 要求 Desktop + owner + workspace/generation，而 `host_router.rs:59-67` 的 Remote context 无 owner/workspace 绑定——配对成功后的 remote 客户端当前**什么数据面都进不去**。母 spec §4.4 承诺 remote 获得全量 owner 数据面，该承诺靠本节兑现，不缩（如未来要缩，需 Dr. Lin 另批）。

**device principal 模型**：

- 配对即绑定：pairingToken **铸造时**记录铸 token 的 desktop owner（`relay_pairing_offer` / `mobile_pairing_create` 两个铸造点都是 desktop-owner 门禁，owner 可知）；exchange 消费 token 时把 owner 写进 device token 记录。一枚 deviceToken 天然归属一个 owner，不需要运行时猜。
- hello 认证通过后，`HostClientContext` 携带 device principal（device_id + owner 绑定）。

**准入矩阵**（与 desktop 落同一份判定函数，不打旁路补丁）：

| 面 | desktop owner | remote device principal |
| --- | --- | --- |
| runtime_subscribe / runtime_request / snapshot / capabilities | 现状 | 放行：`authorize_target` 扩展「Remote + owner 绑定 + 该 owner 现注册（registry 复读）+ live runtime 归该 owner」 |
| data_request（owner 全局面 + workspace 绑定面） | 现状 | 同 desktop 规则，凭 owner 绑定过 `current_registered_context` + workspaceId 匹配 |
| host_request（管理面：偏好、配对管理、relay 开关、admin 面） | 现状 | **维持 `forbidden_class` 拒绝**——母 spec §4.4 通道级隔离原样保持 |

**资源回收**：remote 会话是订阅者/请求方，不拥有 runtime 进程；断开走既有 disconnect 清理（订阅、写任务、pending 帧）。owner 注销或 workspace 切换时 registry 复读让 remote 准入即刻失效（与 desktop 同一机制）。owner 被撤销 = 该 owner 铸的全部 device 失效（Phase 2.5 设备管理提供显式列表面板）。

**验收测试**：正向——remote 配对后完成会话订阅、runtime 命令往返、快照、data_request（workspace 绑定与 owner 全局各一）；拒绝——host_request `forbidden_class`、未绑定 owner 的 remote、owner 注销后、伪造 desktop 类型（§2.2）；回收——断开后订阅清空无泄漏任务。**不得把 Remote 冒充 Desktop 实现**（不借 desktop hello 类型、不造 capability 绕过）。

### 2.9 撤销：踢掉已连接设备（评审 R4）

现状 `RemoteAuth::revoke` 只删 DB（`remote_auth.rs:93-94`），token 校验只在 hello（`host_server.rs:1561-1576`）——已连接的 remote 会话不受影响，「删 token 即踢设备」不成立。修订：

- HostState 维护「device → 活跃 remote 会话」注册：认证后登记、断开注销。
- 新增 desktop 门禁控制 op `remote_device_revoke {deviceId}`：关闭该 device 全部现存 remote 连接（relay 与 loopback remote 一致处理）+ DB 删除 token；后续 hello 因 DB 校验自然拒绝。撤销即踢，不做轮询复核。
- 测试：已连接 → revoke → 该连接不能继续读写（在途请求失败/断开）→ 重连被拒。设备列表 UI 属 Phase 2.5，只交付 op。

### 2.10 配对凭据与 QR 生命周期（评审 R7）

| 状态 | 触发 | 行为 |
| --- | --- | --- |
| minted | `relay_pairing_offer`（每次调用/每次 section 激活重新拉取） | 新铸 5 分钟 token 嵌 offer；**先作废上一枚未兑换**；op 返回 `{url, expiresAt}` |
| used | exchange 成功 | 一次性移除（既有语义） |
| expired | `now > expires_at` | exchange 拒绝；UI 置灰 QR 并引导重新生成。边界：`now == expires_at` 仍可用，清理与拒绝同界（测试锁定） |
| superseded | 再次生成 offer | 旧 token 立即作废（revoke_pending） |
| relay 关闭 | `relay_set_enabled{false}` | 清空全部未兑换 pairing token；**已配对 device token 不撤** |
| 离开页面 | section leave | 停轮询；未兑换 token 按自身 TTL 自然过期（5 分钟），不主动撤 |

UI 依 `expiresAt` 展示剩余有效期；过期即失效标注，不展示「看似可用」的死码。`RemoteAuth::create_pairing` 只增不撤的现状由 revoke_pending 补齐；exchange 的过期判定（`now > expires_at`，`remote_auth.rs:70`）与清理边界对齐为同一规则。

## 3. Pair Device UI

### 3.1 Paseo 的交互形态（已核实源码）

入口是 Settings 页一个行（「Pair a device」+ hint），点开一张 modal（`pair-device-modal.tsx`，桌面宽 480）。modal 内两态：

1. **relay 未启用 → consent 态**（`pair-device-section.tsx` `RelayConsent`）：盾形徽标 + 标题「Enable relay?」+ 一句说明（「Pairing traffic is end-to-end encrypted」）+ 外链文档 + 「暂不 / Enable relay」双按钮 + 底部「不启用也可直连」提示行。
2. **已启用 → offer 态**（`PairingOffer`）：提示行（扫码或复制链接）+ QR 码块（白底圆角 304px 方块，SVG 由 offer URL 生成，errorCorrection M）+ 链接行（只读 mono 输入框 + Copy 按钮，复制成功变勾 2s）+ 黄色警示行「Treat this pairing link like a password. Anyone with it can access this daemon.」。加载/断连/出错各有态（loading 行 / 错误 + Retry / unavailable 行）。offer 数据形态 `{relayEnabled, url}`，由 daemon RPC 生成，5 分钟 stale 后重新拉取。

### 3.2 Picot 落地形态

落在 **Settings → General → Daemon 段内**（`index.html:1211` `setting-daemon-section` 尾部新增 Pair Device 子块），不新开设置页。新模块 `public/settings/pair-device-section.js` + `pair-device-section.test.js`（daemon-section.js 已 498 行，遵守 50 行以上独立模块纪律），由 daemon-section 同层接线（同一 section 容器内、独立 init/leave 生命周期）。

交互照 Paseo 两态：

- **relay off**：一行「配对设备 / Pair a Device」+ 按钮。点击展开 consent 区：说明文案（含 E2EE 说明与公司 relay 地址）+「启用」/「暂不」。启用走控制 op（§4.1），成功即转 offer 态。
- **relay on**：relay 状态行（connecting / online / offline，来自 transport 状态上报）+ endpoint 显示 + QR + 链接只读文本 + 复制按钮（2s「已复制」）+「重新生成」（换新 pairingToken，旧 token 随之作废）+ 剩余有效期（依 `expiresAt`；过期置灰 QR + 失效提示）+ 警示行（同 Paseo 语义：配对码如密码）+「需 Picot 客户端支持」提示行（§3.3：**文案不得暗示当前扫码即可使用**）。QR 复用现有 vendor 渲染（`PicotQr.toCanvas`，`lan-qr.js:52-54` 同款调用），URL 即 §3.3 的 offer URL。
- 启用 relay 时若 `keepRunningAfterQuit` 为 false（`daemon_status` 已返回该值，`daemon_client/commands.rs:207`），consent 文案附一句提醒「退出 Picot 会停掉 daemon，远程连接随之中断」（母 spec §3.2 联动要求；软提醒不硬卡——硬卡需要 daemon 读 app 私有策略文件，违反 Phase 1 D8 的「daemon 不感知此值」边界）。
- i18n：`settings.daemon.pairDevice.*` 四语言（en/zh/ja/es）同步交付，文案含义对齐 Paseo（enableTitle / enableDescription / hint / securityWarning / copy / copied / retry / requiresClient / expired）。

### 3.3 offer URL 前缀（已拍板方案 A，§9）

Picot 没有 Paseo 的 `app.paseo.sh` Web 客户端。两个方案：

- **方案 A（已拍板）**：占位前缀常量 `https://picot.021521.xyz/#offer=<base64url>`（与 relay 同域族，可在 daemon 配置覆盖）。QR 与复制链接都是完整 URL，形态与 Paseo 完全一致；未来移动 Web 客户端上线该域名后，链接**格式兼容**——客户端按同一 `#offer=` 格式解析即可；但已过期的配对凭据不会因客户端上线恢复有效，届时需重新生成 offer。Phase 2.5 桌面客户端「粘贴链接解析」可直接照抄 Paseo `parseConnectionOfferFromUrl`（`connection-offer.ts:55-60`）。代价：域名上线前链接在手机上打不开（无消费者，仅形态占位），文案须注明需 Picot 客户端支持、不得暗示当前扫码可用。
- **方案 B（否决）**：v1 不造 URL，QR/复制内容 = offer payload 的 base64url 串本身。诚实，但丢掉 `#offer=` 形态，未来移动客户端上线时要么换格式（已发出的链接作废）要么双格式并存。

## 4. relay 配置入口

结论：**默认公司 relay 常量 + 环境变量覆盖（开发/自建），UI 只做启用开关与 Pair Device，不做 endpoint 输入框**。

- 默认 endpoint 常量：`paseo.021521.xyz:443`（useTls 恒 true，443 端口 TLS 是既定部署）。
- daemon 环境变量 `PICOT_RELAY_ENDPOINT`（如 `host:port` 形态）覆盖默认——开发期连本地 mock relay、冒烟期换自建实例用；不进 UI。
- `enabled` 状态存 DB `relay.enabled`（保留命名空间内，§2.3），默认 false（relay 默认关闭显式启用，§5）。UI 开关经控制 op 写入并即时启停 transport。
- 不引入 Paseo 的 `publicEndpoint`/`endpoint` 分离（公司 relay 单地址，daemon 出站与客户端入站同一地址）；不做 config.json 文件（Paseo 的 config.json 承载它整个 daemon 配置面，Picot 的等价物是 DB preferences + argv，不为 relay 开新配置面）。

### 4.1 控制面 op（desktop owner 经既有 WS 控制通道）

| op | 入参 | 返回 | 语义 |
| --- | --- | --- | --- |
| `relay_status` | — | `{enabled, endpoint, connectionState, serverId}` | connectionState ∈ connecting/online/offline，来自 transport 上报 |
| `relay_set_enabled` | `{enabled}` | `{enabled}` | 写 preferences（保留命名空间）+ 启/停 transport；enable 时确保 identity 存在（首建 keypair+serverId）；disable 时清空未兑换 pairing token（§2.10，不撤已配对 device） |
| `relay_pairing_offer` | — | `{relayEnabled, url, expiresAt}` | Paseo 同形 + expiresAt；relayEnabled=false 时 url 为 null。每次调用先作废旧 token 再新铸 pairingToken 嵌入 offer（§2.6/§2.10） |
| `remote_device_revoke` | `{deviceId}` | `{ok}` | 撤销设备：关现存连接 + 删 token（§2.9）；设备列表 UI 属 Phase 2.5 |

对应 Paseo 的 `daemonStatusRpc` / `patchConfig({relay:{enabled}})` / `getDaemonPairingOffer` 三件套，但走 Picot 既有 host control op 通道（desktop owner 门禁沿用 `mobile_pairing_create` 的先例，`host_server.rs:2954`），不新增 capability。

## 5. 安全边界

- **relay 不受信**：只转密文；daemon 出站唯一新增网络面。信任锚 = 配对 offer（QR/链接按密码对待，UI 警示行照 Paseo）。
- **relay 可重放/乱序/丢弃密文帧（评审 R8，威胁模型）**：NaCl box 无序号，v1 不做反重放窗口（与 Paseo 同级，见 §7 非目标）。后果：重放 pairing hello 无效（token 一次性）；重放 runtime_request 会重复执行一次命令——已知已接受的风险，Phase 2.5+ 可加 nonce 窗口；重放事件帧对客户端是冗余渲染。E2EE 保证的是机密性与完整性，不是新鲜度。
- **默认关闭，显式启用**：`relay.enabled` 默认 false；consent 态就是显式确认（Paseo「relay off until enabled」同原则）。
- **握手前零处理**：E2EE 握手完成前，data socket 上任何帧不进会话管线（transport 层丢弃并断开，Paseo 同语义）。
- **认证强制 deviceToken**：不引入 Paseo 的 `relayPasswordOptional` 无凭证放行（母 spec §4.4 已拍板）；pairingToken 仅换 token 一次性可用、5 分钟过期（`remote_auth.rs:12`）。
- **管理面隔离**：admin RPC 与 host_request 管理面不经 relay 开放（母 spec §4.4 通道级隔离；remote 权限矩阵见 §2.8）。
- **私钥与保留命名空间**：identity/enabled 走专用内部接口，公共 preferences 面不可读写删列（§2.3）；私钥不进返回/日志。
- **容量溢出局部化**：公网入口各上限（§2.7）触发只关当事连接，本地会话不受影响。
- **同库备份/恢复的信任锚语义（架构师裁量项）**：metadata DB 备份会把 identity 私钥与 device tokens 一并带走。恢复到新机器 = 信任锚随备份迁移（旧设备配对关系保留），**不是**静默生成新 key。风险：旧机器若仍以同一 serverId 连 relay，两台 daemon 竞争同一 serverId，行为依 relay 服务端语义（后连顶替或拒绝）；用户需自行保证旧机停用。备份文件因此按敏感凭据对待。
- **TOFU 的落点**：daemon 侧 v1 的 TOFU 对应物 = keypair/serverId 持久不变，客户端侧（存过的 offer 公钥变化时弹确认）属 Phase 2.5 桌面客户端工作，不在本 spec。keypair 轮换工具 v1 不做（§7），轮换语义 = 全部设备重新配对。

## 6. 与现有 LAN 配对（RemoteAuth / pair.html）的关系

并存，机制共享、场景互斥：

- **共享 RemoteAuth**：pairingToken 池与 device token 库是同一份。一个 deviceToken 不区分它来自 LAN 还是 relay 配对——授权面相同（都是全量 owner 数据面，母 spec §4.4、§2.8）。`remote_auth.rs` 的令牌语义不变，但有三处小改：exchange 消费点新增 hello 内嵌调用；pending token 增 owner 绑定 + device token 记录落 owner（§2.8 device principal）；新增 `revoke_pending`（§2.10）。「新 offer 换 token」与「daemon 信任锚轮换」是两回事，后者 v1 不做。
- **场景互斥**：Phase 1 后 HostServer 硬锁 loopback（`host_server.rs:463-466`），`pair.html` + `POST /v2/auth/exchange` 实际只剩本机浏览器可达（LAN 配对已随 LAN 绑定移除而休眠，`mobile_access_info` 恒 `enabled:false`，`host_server.rs:2941-2952`）。relay 配对是远程场景的唯一入口。UI 语义区分：relay 配对叫「配对设备 / Pair a Device」；`pair.html` 不动不删（loopback 内仍可用，Phase 2.5 桌面客户端桥接时还有用）。
- 文档与代码注释统一用语：LAN pairing（HTTP exchange）vs relay pairing（offer + hello 内嵌 exchange）。

## 7. 非目标

- 移动端 app / PWA / 移动 Web 客户端本体（offer 的消费者；单独立项，母 spec §8）。
- Phase 2.5 桌面多 host 客户端（offer 消费、TOFU 确认、设备管理列表 UI）。
- relay 服务端运维（已上线，母 spec §5.3 收尾项另行）。
- keypair 轮换 / 重置工具、配对设备管理列表（随 Phase 2.5 设备管理一起做；本 spec 只交付 `remote_device_revoke` op）。
- E2EE 反重放（nonce 序号窗口）——v1 与 Paseo 同级不做，威胁与后果见 §5；Phase 2.5+ 可加。
- 密文 binary 帧（binaryCiphertext）协商——base64 文本帧是 Paseo 默认已验证路径，帧体积开销后续有实测再说。
- 双可写视图（多窗口线）——见 §8 实施顺序约束。

## 8. 阶段划分、实施顺序与工作量（plan 的 R 序列）

**实施顺序（评审 R9）**：relay 线与多窗口线共享 `host_server.rs`（握手、准入、会话上下文）、`app.js`/`landing.js`/`index.html`、locales 与 ARCHITECTURE，「互不碰对方文件」无法隔离这些交叠。约定：**多窗口 P1 先行、relay 随后；单 worktree 单写者**——relay 开工前多窗口 P1 必须已合入，relay 全程独占其 worktree。remote principal（Gate B）与多窗口 co-holder 的 owner 放宽必须落在同一份授权判定代码（`authorize_target` / `current_registered_context`），不得出现两个互相冲突的补丁；relay T9（Gate B）在多窗口 P1 的准入代码之上扩展。

| 阶段 | 内容 | 产出 | 估人日 |
| --- | --- | --- | --- |
| R1 | crypto_box 依赖 + daemon identity（保留命名空间隔离、并发首建、损坏拒建）+ E2EE 模块（握手帧、加解密、边界拒收、固定 nonce seam、tweetnacl 独立向量）+ offer 编解码 | 纯库层，无网络无 UI | 2.5 |
| R2 | 会话入口泛型化（green→green 回归重构 + 适配器行为测试）+ TLS（native-tls feature、connector seam、证书测试）+ relay transport（control/data socket、退避/ping/stale/sync、三 ID 模型、容量边界、mock relay 集成测试） | daemon 可连 mock relay 走完整 E2EE 会话 | 4.5 |
| R3 | 配对面：hello 内嵌 pairingToken + 凭据生命周期（作废/清理/expiresAt）+ 控制面 op + DaemonState 接线启停 + Pair Device UI（两态 + QR + 失效展示 + i18n×4） | 设置页可用全链路（对 mock relay） | 3 |
| R4（Gate B） | device principal + owner 绑定 + 准入矩阵（数据面放行/管理面拒绝）+ 资源回收 + 撤销踢已连设备 | remote 配对后可操作会话，撤销即踢 | 3 |
| R5 | 独立 CLI 冒烟工具（参数/隔离目录/凭据脱敏）+ 真 relay 冒烟（paseo.021521.xyz:443）+ 不可达降级验证 + 验收证据文档 + ARCHITECTURE.md 远程访问章节定稿 | 证据文档进 process-evidence/ | 2 |

依赖链 R1→R2→R3→R4→R5 严格串行（每步 TDD 红绿；R4 的正向测试依赖 R3 的配对链路）。合计约 15 人日（原估 8 人日在 Gate B、撤销、容量边界、TLS、冒烟工具入卡后不成立，重估依据见 §11）。

## 9. 拍板记录（2026-10-17，Dr. Lin）

1. **offer URL 前缀 = 方案 A**：占位域名 `https://picot.021521.xyz/#offer=<base64url>`，域名即定为 picot.021521.xyz，可在 daemon 配置覆盖。格式一次定型；与未来移动端客户端**格式兼容**（同一 `#offer=` 解析格式）——已过期的配对凭据不因客户端上线恢复有效，需重新生成；上线文案不得暗示当前扫码即可使用。
2. **relay 配置入口 = 常量 + env**：endpoint 默认公司 relay 常量，`PICOT_RELAY_ENDPOINT` 覆盖；Settings UI 只做开关与 Pair Device，不做 endpoint 编辑。
3. **配对流程 = 一扫码配对**：offer 内嵌 5 分钟一次性 pairingToken（复用 RemoteAuth 机制，Paseo v2 schema 加法扩展），扫码一次完成信任锚 + 认证。

修订不改三项结论。另：`docs/superpowers/plans/` 维持 .git/info/exclude 本地排除（plan 只在磁盘、不入 git，与 daemon-phase1/multi-window plan 同口径；已跟踪的历史 plan 不回滚）。

## 10. 测试与验收

- **Rust 单测**：identity 首建幂等（二次加载同 key）+ 并发首建唯一 + 损坏拒建不静默重建 + 私钥不进公共 preferences 返回/日志；E2EE 加解密与 tweetnacl 独立向量互操作断言（锁 wire 兼容）+ 边界拒收（全零共享密钥/低阶公钥/错误长度/非规范 base64/截断/篡改）+ 固定 nonce seam；握手状态机（hello→ready、非握手帧在 ready 前丢弃断连）；offer 编解码 round-trip 与 `#offer=` fragment 解析；transport 状态机（sync 对账补开/关闭、退避序列、ping/stale 用假时钟）+ 容量边界值 ±1 + 慢消费者溢出关连接且本地不受影响。
- **TLS**：受信证书（fixture CA）握手成功；默认 connector 对自签证书拒绝。
- **Gate B（§2.8 验收矩阵）**：正向（remote 订阅/命令/快照/data_request 两面）+ 拒绝（host_request `forbidden_class`、无 owner 绑定、owner 注销后、relay 通道伪造 desktop）+ 回收（断开清订阅无泄漏）。
- **撤销（§2.9）**：已连接 → revoke → 不能继续读写 → 重连被拒。
- **生命周期（§2.10）**：重新生成作废旧 token；relay 关闭清未兑换不撤已配对；`now == expires_at` 边界；expiresAt 返回。
- **集成**（本地 mock relay，axum 测试服务实现协议 v2 最小面）：daemon 出站连接 → connected → data socket + E2EE → hello remote（deviceToken 与 pairingToken 两路）→ 控制帧往返 → disconnected 清理 → 断连重连；三 ID：重连同名声明 ID 不覆盖、旧连接清理不误删新连接。
- **UI**：pair-device-section.test.js（jsdom + mock transport）：consent→offer 状态机、QR 渲染调用（PicotQr.toCanvas 入参即 offer URL）、复制反馈、keepRunning 提醒条件、expiresAt 失效展示、i18n 切换重绘。
- **CLI 冒烟工具**：参数解析、隔离目录（不读不写用户数据目录）、凭据脱敏（token 不打印、不进 argv）。
- **真 relay 冒烟**（R5 验收项）：`curl https://paseo.021521.xyz/health` 200；daemon 对公司 relay 完成 TLS + control 连接（日志断言 online）；CLI 工具走完 offer→E2EE→配对→会话往返（含一条 Gate B 数据面 op）；撤销一台已连设备；拔网线/改错 endpoint 场景本地功能零影响、退避重连日志断言。
- 全量 `bun run test` + `bun run check:rust` + `bun run check` 三绿；ARCHITECTURE.md 增远程访问（relay 出站 + TLS + E2EE + 配对 + Gate B 权限矩阵 + 容量边界）章节。

## 11. Review-Fix Mapping（军师评审 2026-10-17 → 处置 → 落点）

| 意见 | 级别 | 处置 | 落点 |
| --- | --- | --- | --- |
| R1 remote 权限面不存在，Gate B 被漏掉 | 阻塞 | 采纳：补 Gate B 设计与任务卡（device principal、owner 绑定、准入矩阵、资源回收、正反向测试）；不冒充 Desktop；母 spec 全量数据面承诺不缩（如需缩另请 Dr. Lin 批，本 spec 未缩） | §2.8、§8 R4、§10；plan T9 |
| R2 私钥拟存公共 preferences，可读可改可删 | 阻塞 | 采纳：`relay.*` 保留命名空间 + 专用内部接口 + 并发首建 + 损坏拒建 + 私钥不外泄测试 | §2.3、§5；plan T1/T6 |
| R3 tokio-tungstenite 未启用 TLS | 阻塞 | 采纳：`native-tls` feature（选型理由：复用 reqwest 既有栈、系统信任库覆盖公司 CA；备选 webpki-roots 一行之差）；标准链+主机名校验；受信/错误证书测试 | §2.5、§10；plan T4 |
| R4 删 token 不踢已连设备 | 重要 | 采纳：device→活跃会话注册 + `remote_device_revoke` op 即踢；已连接→revoke→读写失败测试 | §2.9、§4.1；plan T10 |
| R5 relay 身份与 hello.clientId 绑定缺失 | 重要 | 采纳：声明 ID/路由 ID/内部连接 ID 三分，host 分配内部 ID 为注册键；relay 会话只收 remote | §2.2；plan T5 |
| R6 公网入口缺容量边界 | 重要 | 采纳：control 帧上限/连接数/并发握手/有界出站队列入 `transport_limits` 契约；溢出只关当事连接 | §2.7、§5；plan T4/T5 |
| R7 token/QR 生命周期未闭合 | 重要 | 采纳：六状态表（重新生成/关 relay/已用/到期/离开页面/作废）；返回 expiresAt；关 relay 清未兑换不撤已配对；边界同界 | §2.10、§4.1；plan T6/T8 |
| R8 crypto 测试不足以覆盖安全边界 | 重要 | 采纳：全零/低阶/截断/篡改/长度/base64 拒收；固定 nonce seam；tweetnacl 独立向量；重放写入威胁模型（接受+后果分析，反重放列非目标） | §2.4、§5、§7；plan T1 |
| R9 两线共享产品文件不能并行 | 重要 | 采纳：多窗口 P1 先行、relay 随后、单 worktree 单写者；Gate B 与 co-holder 共用同一份准入代码 | §8 前言、§7；plan Global Constraints/T9 |
| R10 plan 三处可执行性问题 + 工作量缺依据 | 建议 | 采纳：T3 如实标 green→green + 补行为测试；冒烟补独立 CLI 工具任务卡；删尾部过时待决项（与 §9 冲突）；工作量 8→15 人日重估 | plan T3/T11/T12/Review-Fix Mapping/估算表 |
| 占位 URL「存量链接未来可用」表述 | 建议 | 采纳（并入 R10）：改为「格式兼容」，过期凭据不复活；文案不得暗示当前扫码可用 | §3.3、§9.1；plan T8 |
| 同库备份/恢复复制 daemon 身份（军师风险提示） | 架构师裁量 | 采纳：恢复 = 信任锚迁移而非静默新 key；双机同 serverId 竞争风险与用户责任写入安全边界 | §5；plan T0（ARCHITECTURE 章节） |

三项拍板（§9）与模块归属结论未推翻；军师报告全文见 session artifacts（3475ee1d）。
