# Relay Phase 2：daemon 接入 relay 与 Pair Device 设计

**状态：** Draft — 待 Dr. Lin 评审；三个拍板点见 §9
**日期：** 2026-10-17
**演化关系：** 母 spec `implemented/2026-09-20-persistent-daemon-relay-design.md` §4（Phase 2）的实施化设计。协议事实不重复调研——全部引母 spec §2 事实表；本文只做任务分解需要的落点设计（模块归属、数据落盘、UI 形态、配置入口）与新增决策。Phase 2.5（桌面多 host 客户端）、Phase 3（relay 服务端运维）不在本文。
**参照：** Paseo `packages/app/src/desktop/components/pair-device-section.tsx`（393 行）、`pair-device-modal.tsx`、`screens/settings/host-page.tsx:262-272,1095-1122`、`packages/relay/src/{crypto,encrypted-channel}.ts`、`packages/protocol/src/connection-offer.ts`、`packages/server/src/server/{relay-transport,pairing-offer}.ts`。

## 1. 决策记录

- **需求（Dr. Lin，2026-10-17，原文）**：「学 Paseo，把 Picot daemon 注册到公司 relay（endpoint: paseo.021521.xyz:443，我刚探测 /health 200 v0.5.0 活着），获取唯一的一个 key 和 relay 链接；在 Settings→General→Daemon 页增加 "Pair Device" 功能，显示 relay URL（参照 Paseo 设置页的 Pair device 做法）」。
- **relay 已上线**：公司 relay `paseo.021521.xyz:443` 运行中（Dr. Lin 2026-10-17 探测 `/health` 200、版本 v0.5.0）。母 spec §5.3 的选型/部署核实项已由既成事实收编，不在本 spec。
- **Phase 1 已交付**（daemon-phase1 分支，验收见 `process-evidence/2026-10-16-picot-daemon-phase1-e2e.md`）：`picot-host`（零 Tauri 守卫）+ `picot-daemon` 独立进程 + admin 通道 + 设置页 Daemon 段 + `keepRunningAfterQuit`。HostServer 已硬锁 loopback（`host_server.rs:463-466`），注释明示远程访问的下一步是本 spec。
- **「唯一的一个 key」的语义**：daemon 首次启用 relay 时生成持久 Curve25519 keypair + serverId（uuid），一次生成、跨重启不变。这就是 Paseo `daemon-keypair.json` 的对应物；keypair 不轮换、不重置（v1，见 §7）。

## 2. 架构

### 2.1 模块归属：`picot-host/src/relay/`，由 DaemonState 持有

结论：relay 全部代码放 **picot-host**（新模块 `src/relay/`），`DaemonState` 增字段持有并在 daemon 启动序里按配置拉起。理由：

1. E2EE 解密后的帧必须喂进 `host_server` 的会话管线，`host_server` 就在 picot-host，同进程注入零跨进程协议；放 picot-daemon bin（现 443 行入口）则库无法引用，放 Tauri app 则 relay 存活期错绑在 app 上（app 可退出，daemon 常驻）。
2. 零 Tauri 守卫（`scripts/check-daemon-no-tauri.js`）自动覆盖新模块。
3. 依赖已就位：picot-host 已有 `tokio-tungstenite 0.24`、`base64 0.22`、`rand`、`uuid`、`futures-util`（`crates/picot-host/Cargo.toml:28-53`），唯一新增 `crypto_box`（§2.4）。

模块划分照母 spec §4.1：`relay/e2ee.rs`（keypair + NaCl box + 握手帧）、`relay/pairing.rs`（offer 编解码与生成）、`relay/transport.rs`（control/data socket 与生命周期）。

### 2.2 会话注入：泛型化 `handle_websocket`，等价 Paseo `attachSocket(transport:"relay")`

现状 `host_server.rs:1507` `async fn handle_websocket(mut socket: WebSocket, state)` 把 axum `WebSocket` 类型钉死在整条会话管线（hello → hello_ack → 事件订阅 → 请求循环）。改造为接受任何 `Stream<Item=Message> + Sink<Message>` 的会话入口（axum upgrade 路径不变，零行为变化，现有测试全绿为准）；relay 侧提供内存通道适配器：解密帧 → 适配器 → 会话管线，回程反向。这就是 Paseo `relay-transport.ts:945` `attachSocket(ws, {transport:"relay", relayConnectionId})` 注入点的 Picot 对应物。

- remote 客户端的 `clientId` = relay 分配的 `connectionId`；`HostClientContext::remote(client_id)` 既有路径（`host_server.rs:1615`）原样复用。
- 会话协议零改动：通道内客户端照发 `hello{clientType:"remote", deviceToken}`（`host_server.rs:1561-1570` 认证路径不动），配对扩展见 §2.6。

### 2.3 身份持久化：metadata DB preferences 表

keypair + serverId 存 SQLite `preferences` 表（`metadata_store.rs:259`，读写接口 `metadata_store.rs:876-908`），单键 `relay.daemon.identity`，值 `{serverId, publicKeyB64, secretKeyB64}`。理由：device token 已在同一库（`remote_auth.rs:70-76` 落 `store_device_token`），身份与授权同库同备份；Paseo 的 JSON 文件（`~/.paseo/daemon-keypair.json`）在 Picot 的等价物就是这张表，不发明新文件。首次启用时生成，之后只读。

### 2.4 E2EE crate：RustCrypto `crypto_box`

`crypto_box = "0.9"`（x25519-dalek + XSalsa20-Poly1305），与 Paseo 的 tweetnacl `nacl.box` 加解密互操作（同一 NaCl box 构造）。wire 格式逐字节照 Paseo：密文帧 = 文本帧，内容为 `base64(nonce[24] || ciphertext)`（`paseo crypto.ts:5-9`、`encrypted-channel.ts:475-482`）；握手帧明文 JSON：`{"type":"e2ee_hello","key":"<b64公钥>"}` → `{"type":"e2ee_ready"}`（capabilities/binaryCiphertext v1 不声明，base64 路径是 Paseo 已验证默认）。兼容性用 tweetnacl 测试向量锁死（§10）。每连接独立共享密钥；握手完成前不向会话管线递交任何帧（Paseo 同语义，母 spec §2）。

### 2.5 传输策略：逐条抄母 spec §2 事实表

control socket 重连退避 `min(30s, 1s×尝试次数)`（`relay-transport.ts:338`）、WS ping 周期 10s / 30s stale 强断（`relay-transport.ts:56-57`）、control ready 超时 8s、握手超时 10s、`perMessageDeflate` 关闭、`sync` 对账（control 连上后 relay 推全量 connectionIds，daemon 补开缺失 data socket、关多余）、每 client 独立 data socket + 独立 E2EE 通道 + 15s open 超时（`relay-transport.ts:361`）。URL：`wss://<endpoint>/ws?serverId=&role=server|client&v=2[&connectionId=]`。以上参数不在本 spec 重述理由，实现照抄。

### 2.6 配对令牌：offer 内嵌一次性 pairingToken（一扫码完成配对）

Paseo 的 offer 只含 E2EE 信任锚，准入靠 relay password（可选）。Picot 母 spec §4.4 已拍板「认证强制 deviceToken、不引入无凭证放行」。若照搬 Paseo offer 形状，移动端扫完码还得手动抄一个 5 分钟令牌，两步且易错。故 Picot 的 offer payload 在 Paseo v2 schema 上做一个加法扩展：

```jsonc
{ "v": 2, "serverId": "…", "daemonPublicKeyB64": "…",
  "relay": { "endpoint": "paseo.021521.xyz:443", "useTls": true },
  "pairingToken": "picot_pair_…" }   // 可选字段；Picot 配对 offer 才带
```

- 「Pair Device」每次打开新铸一个 5 分钟一次性 pairingToken（复用 `RemoteAuth::create_pairing`，`remote_auth.rs:42-50`），嵌进 offer。链接与 QR 因此是即开即用的配对物，过期后重新打开即重新生成。
- 移动端链路：扫码得 offer → 连 relay（role=client）→ E2EE 握手 → `hello{clientType:"remote", pairingToken, deviceId}` → daemon 在 hello 内调既有 `RemoteAuth::exchange`（`remote_auth.rs:52-76`，语义与 `POST /v2/auth/exchange` 完全同一实现）铸 deviceToken，`hello_ack` 附 `{"deviceToken":"…"}` 回传 → 设备持久化，此后直连路径 `hello{clientType:"remote", deviceToken}` 不变。
- 「输入/内嵌 pairing token」的 HTTP 端点（`/v2/auth/exchange`，`host_server.rs:442`）原样保留服务 loopback 场景；relay 通道无 HTTP，配对走 hello 内嵌这一条新路径。

这是对 Paseo offer schema 的有意加法（Paseo 客户端会忽略未知字段，不破坏互操作），列入 §9 拍板点 3。

## 3. Pair Device UI

### 3.1 Paseo 的交互形态（已核实源码）

入口是 Settings 页一个行（「Pair a device」+ hint），点开一张 modal（`pair-device-modal.tsx`，桌面宽 480）。modal 内两态：

1. **relay 未启用 → consent 态**（`pair-device-section.tsx` `RelayConsent`）：盾形徽标 + 标题「Enable relay?」+ 一句说明（「Pairing traffic is end-to-end encrypted」）+ 外链文档 + 「暂不 / Enable relay」双按钮 + 底部「不启用也可直连」提示行。
2. **已启用 → offer 态**（`PairingOffer`）：提示行（扫码或复制链接）+ QR 码块（白底圆角 304px 方块，SVG 由 offer URL 生成，errorCorrection M）+ 链接行（只读 mono 输入框 + Copy 按钮，复制成功变勾 2s）+ 黄色警示行「Treat this pairing link like a password. Anyone with it can access this daemon.」。加载/断连/出错各有态（loading 行 / 错误 + Retry / unavailable 行）。offer 数据形态 `{relayEnabled, url}`，由 daemon RPC 生成，5 分钟 stale 后重新拉取。

### 3.2 Picot 落地形态

落在 **Settings → General → Daemon 段内**（`index.html:1211` `setting-daemon-section` 尾部新增 Pair Device 子块），不新开设置页。新模块 `public/settings/pair-device-section.js` + `pair-device-section.test.js`（daemon-section.js 已 498 行，遵守 50 行以上独立模块纪律），由 daemon-section 同层接线（同一 section 容器内、独立 init/leave 生命周期）。

交互照 Paseo 两态：

- **relay off**：一行「配对设备 / Pair a Device」+ 按钮。点击展开 consent 区：说明文案（含 E2EE 说明与公司 relay 地址）+「启用」/「暂不」。启用走控制 op（§4.1），成功即转 offer 态。
- **relay on**：relay 状态行（connecting / online / offline，来自 transport 状态上报）+ endpoint 显示 + QR + 链接只读文本 + 复制按钮（2s「已复制」）+「重新生成」（换新 pairingToken）+ 警示行（同 Paseo 语义：配对码如密码）。QR 复用现有 vendor 渲染（`PicotQr.toCanvas`，`lan-qr.js:52-54` 同款调用），URL 即 §3.3 的 offer URL。
- 启用 relay 时若 `keepRunningAfterQuit` 为 false（`daemon_status` 已返回该值，`daemon_client/commands.rs:207`），consent 文案附一句提醒「退出 Picot 会停掉 daemon，远程连接随之中断」（母 spec §3.2 联动要求；软提醒不硬卡——硬卡需要 daemon 读 app 私有策略文件，违反 Phase 1 D8 的「daemon 不感知此值」边界）。
- i18n：`settings.daemon.pairDevice.*` 四语言（en/zh/ja/es）同步交付，文案含义对齐 Paseo（enableTitle / enableDescription / hint / securityWarning / copy / copied / retry）。

### 3.3 offer URL 前缀（open question → §9 拍板点 1）

Picot 没有 Paseo 的 `app.paseo.sh` Web 客户端。两个方案：

- **方案 A（推荐）**：占位前缀常量 `https://picot.021521.xyz/#offer=<base64url>`（与 relay 同域族，可在 daemon 配置覆盖）。QR 与复制链接都是完整 URL，形态与 Paseo 完全一致；未来移动 Web 客户端上线该域名后，存量 QR/链接形态零改动生效，Phase 2.5 桌面客户端「粘贴链接解析」可直接照抄 Paseo `parseConnectionOfferFromUrl`（`connection-offer.ts:55-60`）。代价：域名上线前链接在手机上打不开（无消费者，仅形态占位），文案须注明需客户端支持。
- **方案 B**：v1 不造 URL，QR/复制内容 = offer payload 的 base64url 串本身，界面同时展示 relay 地址与公钥明文。诚实（没有假装存在的入口），但丢掉 `#offer=` 形态，未来移动客户端上线时要么换格式（已发出的链接作废）要么双格式并存。

推荐 A：格式一次定型，占位域名成本为零。

## 4. relay 配置入口

结论：**默认公司 relay 常量 + 环境变量覆盖（开发/自建），UI 只做启用开关与 Pair Device，不做 endpoint 输入框**。

- 默认 endpoint 常量：`paseo.021521.xyz:443`（useTls 恒 true，443 端口 TLS 是既定部署）。
- daemon 环境变量 `PICOT_RELAY_ENDPOINT`（如 `host:port` 形态）覆盖默认——开发期连本地 mock relay、冒烟期换自建实例用；不进 UI。
- `enabled` 状态存 DB preferences（`relay.enabled`），默认 false（relay 默认关闭显式启用，§5）。UI 开关经控制 op 写入并即时启停 transport。
- 不引入 Paseo 的 `publicEndpoint`/`endpoint` 分离（公司 relay 单地址，daemon 出站与客户端入站同一地址）；不做 config.json 文件（Paseo 的 config.json 承载它整个 daemon 配置面，Picot 的等价物是 DB preferences + argv，不为 relay 开新配置面）。

### 4.1 控制面 op（desktop owner 经既有 WS 控制通道）

| op | 入参 | 返回 | 语义 |
| --- | --- | --- | --- |
| `relay_status` | — | `{enabled, endpoint, connectionState, serverId}` | connectionState ∈ connecting/online/offline，来自 transport 上报 |
| `relay_set_enabled` | `{enabled}` | `{enabled}` | 写 preferences + 启/停 transport；enable 时确保 identity 存在（首建 keypair+serverId） |
| `relay_pairing_offer` | — | `{relayEnabled, url}` | Paseo 同形；relayEnabled=false 时 url 为 null。每次调用新铸 pairingToken 嵌入 offer（§2.6） |

对应 Paseo 的 `daemonStatusRpc` / `patchConfig({relay:{enabled}})` / `getDaemonPairingOffer` 三件套，但走 Picot 既有 host control op 通道（desktop owner 门禁沿用 `mobile_pairing_create` 的先例，`host_server.rs:2954`），不新增 capability。

## 5. 安全边界

- **relay 不受信**：只转密文；daemon 出站唯一新增网络面。信任锚 = 配对 offer（QR/链接按密码对待，UI 警示行照 Paseo）。
- **默认关闭，显式启用**：`relay.enabled` 默认 false；consent 态就是显式确认（Paseo「relay off until enabled」同原则）。
- **握手前零处理**：E2EE 握手完成前，data socket 上任何帧不进会话管线（transport 层丢弃并断开，Paseo 同语义）。
- **认证强制 deviceToken**：不引入 Paseo 的 `relayPasswordOptional` 无凭证放行（母 spec §4.4 已拍板）；pairingToken 仅换 token 一次性可用、5 分钟过期（`remote_auth.rs:12`）。
- **TOFU 的落点**：daemon 侧 v1 的 TOFU 对应物 = keypair/serverId 持久不变，客户端侧（存过的 offer 公钥变化时弹确认）属 Phase 2.5 桌面客户端工作，不在本 spec。keypair 轮换工具 v1 不做（§7），轮换语义 = 全部设备重新配对。
- **撤销**：既有 device token DB 删除路径（`remote_auth.rs:82-88` revoke）即踢设备；设备管理 UI 是 Phase 2.5 范围。
- **管理面隔离**：admin RPC 不经 relay 开放（母 spec §4.4 通道级隔离，本 spec 不改变）。

## 6. 与现有 LAN 配对（RemoteAuth / pair.html）的关系

并存，机制共享、场景互斥：

- **共享 RemoteAuth**：pairingToken 池与 device token 库是同一份（`remote_auth.rs` 不改内部逻辑，只新增 hello 内嵌 exchange 调用点）。一个 deviceToken 不区分它来自 LAN 还是 relay 配对——授权面相同（都是全量 remote principal，母 spec §4.4）。
- **场景互斥**：Phase 1 后 HostServer 硬锁 loopback（`host_server.rs:463-466`），`pair.html` + `POST /v2/auth/exchange` 实际只剩本机浏览器可达（LAN 配对已随 LAN 绑定移除而休眠，`mobile_access_info` 恒 `enabled:false`，`host_server.rs:2941-2952`）。relay 配对是远程场景的唯一入口。UI 语义区分：relay 配对叫「配对设备 / Pair a Device」；`pair.html` 不动不删（loopback 内仍可用，Phase 2.5 桌面客户端桥接时还有用）。
- 文档与代码注释统一用语：LAN pairing（HTTP exchange）vs relay pairing（offer + hello 内嵌 exchange）。

## 7. 非目标

- 移动端 app / PWA / 移动 Web 客户端本体（offer 的消费者；单独立项，母 spec §8）。
- Phase 2.5 桌面多 host 客户端（offer 消费、TOFU 确认、设备管理 UI）。
- relay 服务端运维（已上线，母 spec §5.3 收尾项另行）。
- keypair 轮换 / 重置工具、配对设备管理列表（随 Phase 2.5 设备管理一起做）。
- 双可写视图（另一条线在跑，文件不碰）。
- 密文 binary 帧（binaryCiphertext）协商——base64 文本帧是 Paseo 默认已验证路径，帧体积开销后续有实测再说。

## 8. 阶段划分与工作量（plan 的 R 序列）

| 阶段 | 内容 | 产出 | 估人日 |
| --- | --- | --- | --- |
| R1 | crypto_box 依赖 + daemon identity（keypair/serverId 首建与 DB 持久化）+ E2EE 模块（握手帧、加解密、tweetnacl 向量锁定）+ offer 编解码（payload/URL/fragment round-trip） | 纯库层，无网络无 UI | 1.5 |
| R2 | 会话入口泛型化（零行为回归）+ relay transport（control/data socket、退避/ping/stale/sync、mock relay 集成测试） | daemon 可连 mock relay 走完整 E2EE 会话 | 3 |
| R3 | 配对面：hello 内嵌 pairingToken + 控制面三 op + DaemonState 接线启停 + Pair Device UI（两态 + QR + i18n×4） | 设置页可用全链路（对 mock relay） | 2.5 |
| R4 | 真 relay 冒烟（paseo.021521.xyz:443）+ 不可达降级验证 + 验收证据文档 + ARCHITECTURE.md 远程访问章节定稿 | 证据文档进 process-evidence/ | 1 |

依赖链 R1→R2→R3→R4 严格串行（每步 TDD 红绿）。合计约 8 人日。

## 9. 留给 Dr. Lin 的拍板点

1. **offer URL 前缀**（§3.3）：A 占位域名 `https://picot.021521.xyz/#offer=…`（推荐）vs B 裸 payload。若选 A，域名是否定为 picot.021521.xyz。
2. **relay 配置入口**（§4）：默认常量 + env 覆盖、UI 只做开关——确认即可；若想要 UI 可改 endpoint 请明示（会增加配置校验与错误态工作量约 0.5 人日）。
3. **一扫码配对**（§2.6）：offer 内嵌 5 分钟一次性 pairingToken（Paseo schema 加法扩展，推荐）vs 照抄 Paseo 纯 offer + 移动端手动输令牌两步流。

## 10. 测试与验收

- **Rust 单测**：identity 首建幂等（二次加载同 key）；E2EE 加解密与 tweetnacl 测试向量互操作断言（锁 wire 兼容）；握手状态机（hello→ready、非握手帧在 ready 前丢弃断连）；offer 编解码 round-trip 与 `#offer=` fragment 解析；transport 状态机（sync 对账补开/关闭、退避序列、ping/stale 用假时钟）。
- **集成**（本地 mock relay，axum 测试服务实现协议 v2 最小面）：daemon 出站连接 → connected → data socket + E2EE → hello remote（deviceToken 与 pairingToken 两路）→ 控制帧往返 → disconnected 清理 → 断连重连。
- **UI**：pair-device-section.test.js（jsdom + mock transport）：consent→offer 状态机、QR 渲染调用（PicotQr.toCanvas 入参即 offer URL）、复制反馈、keepRunning 提醒条件、i18n 切换重绘。
- **真 relay 冒烟**（R4 验收项）：`curl https://paseo.021521.xyz/health` 200；daemon 对公司 relay 完成 control 连接（日志断言 online）；测试客户端脚本走完 offer→E2EE→配对→会话往返；拔网线/改错 endpoint 场景本地功能零影响、退避重连日志断言。
- 全量 `bun run test` + `bun run check:rust` + `bun run check` 三绿；ARCHITECTURE.md 增远程访问（relay 出站 + E2EE + 配对）章节。
