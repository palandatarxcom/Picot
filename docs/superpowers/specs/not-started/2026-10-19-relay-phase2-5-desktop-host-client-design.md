# Relay Phase 2.5：桌面多 host 客户端设计

**状态：** Draft 修订 2（定稿） — 军师二轮评审（R2-01–R2-12）吸收完毕，四份实施契约（§5.2）与 §12 两项（wire 扩展、页面级重载切换）已获 Dr. Lin 同意（2026-10-20，「同意军师的意见」）。plan 按契约重排为 Draft 3；终审通过后开工。
**日期：** 2026-10-19
**演化关系：** 母 spec `implemented/2026-09-20-persistent-daemon-relay-design.md` §4.6（Phase 2.5 增补）的实施化设计。Phase 2 daemon 侧（`2026-10-17-relay-phase2-pair-device-design.md`，T0–T12）已交付：daemon 出站 relay、E2EE、一扫码配对、Gate B、设备撤销、真 relay 冒烟 OK。本文把桌面版 Picot 变成可连接任意远程 host 的完整客户端。协议事实引母 spec §2 与 pair-device spec，不重复调研。
**参照：** Paseo `packages/app/src/types/host-connection.ts`（HostProfile/RelayHostConnection）、`runtime/host-runtime.ts`（连接工厂/链接消费）、`runtime/host-confirmation.ts`（TOFU 语义与指纹格式）、`hosts/host-confirmation-sheet.tsx`、`packages/client/src/daemon-client-relay-e2ee-transport.ts`（客户端 E2EE 传输包装）。

## 1. 决策记录

- **需求（母 spec §4.6，2026-10-14 增补）**：移动端弱，先让桌面版成为可连接任意 host 的完整客户端（默认本机 daemon，可添加远程 host 的 relay 链接），移动端后续降级为薄客户端。daemon-tab spec §8 的留白（「客户端 app 交互、设备管理留 Phase 2.5」）与 pair 区 requiresClient 文案（「需要客户端 app 才能打开此链接」）都由本文补上。
- **app 进程终结 E2EE，不经本机 daemon 转发（母 spec 已定，本文细化）**：远程 host 的 E2EE 通道与 per-host loopback 代理都归 app 进程（Tauri 壳），复用 picot-host 的 relay 客户端组件（零 Tauri 守卫自动覆盖）。理由：客户端会话的生死跟着 UI 走；本机 daemon 只服务本机工作区，不背远程 host 的连接状态。
- **前端帧协议零改动，连接管理层改 URL**：WebView 对每个 host 仍说同一套 v2 会话协议（hello/订阅/请求循环）；新增的连接管理层把 wsUrl 从页面 origin 切到该 host 的 loopback 代理 origin。唯一的协议内变化在桥上（§2.2 hello 重写），WebView 无感知。
- **复用而非新建客户端握手**：`relay/smoke_client.rs`（plan T11 产物）已实现 offer 解析、role=client 拨号、e2ee_hello/ready、pairingToken 交换。本文把这些从 example 提炼进 `relay/client_role.rs`，冒烟客户端改为薄壳复用。
- **TOFU 照抄 Paseo host-confirmation 语义**：offer 与已存 host 精确匹配（serverId + relay endpoint + TLS + daemon 公钥）则自动通过；否则弹确认（新 host / key·relay·TLS 变更两态），指纹展示为首 8 位…末 8 位、4 字符分组。一次只挂一个问题，新问题取消旧问题。

## 2. 架构

### 2.1 模块归属

| 模块 | 归属 | 内容 |
| --- | --- | --- |
| `relay/client_role.rs`（新） | picot-host | role=client 单 data socket 拨号、E2EE 客户端握手（ephemeral keypair → e2ee_hello → e2ee_ready；重发 1s 对齐 Paseo 客户端（smoke_client 现为 2s，T1 统一并锁 green→green 基线）、握手超时 15s）、pairingToken 一扫码交换、断线退避重拨（参数复用 `relay/transport.rs` 常量：1s 起 30s 封顶线性） |
| `relay/host_bridge.rs`（新） | picot-host（由 app 进程持有，不进 daemon） | 每个已连接远程 host 一个 loopback WS 监听（127.0.0.1 随机端口），终结 E2EE：proxy WS 帧 ↔ 加密帧；hello 重写（§2.2）；有界队列与长度上限沿用 `transport_limits` |
| `relay/host_store.rs`（新） | picot-host | HostProfile 与 deviceToken 持久化（§2.6）；损坏语义见 §2.6（保留不回退，R9） |
| app 接线（改） | `src-tauri/src/` | Tauri commands：host 列表/添加（消费 offer URL）/删除/连接状态/TOFU 确认应答；host_bridge 生命周期随 app |
| 前端（改） | `public/` | 连接管理层 + 侧栏 host 按钮（切换器弹层）+ 设备管理（§3） |

不新增任何 relay 协议帧或字段：客户端侧全是既有协议的消费方。

### 2.2 桥接与 hello 重写

**调用方认证（R1）**：随机 loopback 端口不构成认证。桥对每条 proxy 连接先验调用方再注入凭据：app 为每个窗口铸造一次性 per-window grant token（注入该窗口 init script，与 daemon 的 capability 注入同构），grant 同时绑定目标 host；连接建立时校验 grant 与 Origin 精确匹配（本机页面 origin），任一不符或首帧超时（15s）即关闭，不注入、不透传。窗口关闭/切换 host 时该窗口的 grant 立即作废。浏览器跨站 WS（无 grant、异源）被同一路径拒绝。

**hello 重写**：WebView 连 per-host proxy 端口后发送的 hello 是 `{clientType:"desktop", desktopCapability, clientId, …}`。桥截获**首帧 hello**，重写认证三元组：`clientType:"desktop"→"remote"`、`desktopCapability` 字段删除、注入 `deviceToken`；`clientId` 与其余字段（canonicalRoute、订阅意图等）原样保留。重写后的 hello 经 E2EE 通道发往远程 host，走 Phase 2 已交付的 remote 认证路径（`host_server.rs` remote 分支）。

- 重写只做一次/连接：首帧非 hello（或已重写过）→ 关闭该 proxy 连接（R1：未认证连接不享有透传）。
- **hello_ack 净化（R2）**：配对模式下远端 `hello_ack` 携带新铸 `deviceToken`（`host_server.rs:1930-1935`）——桥解析该帧，接管 token 持久化，向 WebView 转发**剔除 `deviceToken` 字段后**的 ack；持久化失败 → 关闭连接并把配对状态置「保存失败」（不静默丢 token 也不假装成功）。普通模式下 hello_ack 不含凭据，原样转发。其余帧（重写之后的双向业务帧）不解析、原样透传。
- 本机 desktopCapability 绝不出本机（远程 host 也无法验证它）；deviceToken 只在 Rust 侧出现（§2.6 存储、日志走既有 token redaction）。措辞校准（R2）：pairingToken 无法承诺「从未进页面」——用户粘贴的 offer 本身含它；约束改为「不经日志/argv/遥测落任何痕迹 + UI 渲染走 redaction」。
- 添加新 host（尚无 deviceToken）时：桥对首帧注入 `pairingToken`+`deviceId` 而不是 deviceToken，消费一扫码配对；交换成功后 token 落盘、该 host 转常态。交换失败（过期/已用）→ 桥关 proxy WS，前端给出配对失败态。

### 2.3 配对链接消费与 TOFU

1. 侧栏 host 按钮弹层「添加 host」→ 粘贴 offer URL（`#offer=` 片段格式，pair-device spec §2.4 的编解码直接复用）。
2. 解析出 `{serverId, daemonPublicKeyB64, relay:{endpoint, useTls}, pairingToken}`。QR 扫码面向移动端；桌面 v1 只做粘贴。
3. TOFU 判定（纯函数，独立测试）：与已存 host 按 serverId 匹配后逐项比对 relay endpoint/useTls/公钥——全同且本机已持有该 host 的 deviceToken → 自动通过；任何差异或新 serverId → 弹确认（§3.3）。**变更走暂存事务（R8）**：确认后新连接信息先进候选槽，远端认证成功且 token 落盘成功才提交覆盖；取消、认证失败、确认响应过期、写盘失败一律不动原 profile（原配置保持可用）。
4. 已配对过的 host 再被添加（serverId 相同、无新 pairingToken）→ 视为「重新连接」，不重新配对。
5. **配对单航班（R8）**：pairingToken 单次消费（`remote_auth.rs`），同一 host 的配对交换是 host 级单航班——首个连接消费 token，其余连接排队或拒绝，成功并落盘后才开放该 host 的普通 proxy 会话。**deviceId 稳定唯一（R8）**：app 级持久 uuid（安装唯一），不是每连接随机——远端同 deviceId 再配对会覆盖旧 token（`metadata_store.rs:795-801`），随机值会造成设备自我覆盖。

### 2.4 重连与降级

- **底层会话丢失即关 proxy（R7）**：relay 断开/远端关 E2EE 通道时，桥关闭对应 proxy WS——不保持静默连接（静默会让前端永远等不到 `onopen` 重发 hello，呈现假在线）。重拨由桥负责：退避重拨 + 重握手后，proxy 端口重新可连，前端经既有 `WebSocketClient` 退避重连、重新（被重写的）hello、恢复订阅与快照。桥与前端各管一层重连（桥管 relay，前端管 proxy），不重复建会话。
- **凭证失效只认显式拒绝（R7）**：只有重连被远端以 `unauthorized_device` 拒绝才把 host 标记为「凭证失效」并提示重新配对（撤销踢连接后远端不再下发独立撤销帧，`host_server.rs:2041-2048`）；普通断网/断线不清 token、不进失效态。
- 远端 revoke 本设备：远端关 E2EE 通道（Phase 2 T10 已交付），桥关 proxy WS；前端重连被拒后按上条进入失效态，不做静默重试轰炸。

### 2.5 deviceToken 生命周期

- 首配对：一扫码（pairingToken 内嵌 offer）→ hello_ack 回铸 deviceToken → 落 host_store。
- 本机删 host（UI 删除）：**先取消该 host 的在途配对/重拨（R8，迟到完成不得复活已删 host）**，再删本地 profile+token；**远端仍认这台设备**——设备列表里它会留着，远端 owner 需在远端撤销（§3.4 提示文案写清）。不做自动远端撤销：撤销是 owner 的动作，本机代办越权，且本机删 host 后 token 已不可用。
- 凭证失效（上述 revoke/轮换路径）：host 保留、token 清空、状态「待重新配对」。

### 2.6 存储（A 已拍板：app 私有 0600 JSON）

app 私有 0600 JSON（`app_config_dir/picot-hosts.json`，与 `picot-daemon-settings.json` 同目录惯例）。存 HostProfile（serverId/label/连接信息/添加时间/app 级 deviceId）+ deviceToken；创建即 0600，写盘走 tmp+rename 原子替换，写失败保留旧文件。**损坏语义（R9）**：此文件是 TOFU 信任基线与持久凭据，**不走** keepRunningAfterQuit 的「损坏回退默认值」——文件缺失才初始化空表；解析失败则保留原文件、阻止覆盖性保存、UI 给「主机配置损坏，需重新配对或手动恢复」的明确提示。**已知边界（R9 事实修正）**：同用户任意进程可读；服务端存的是 token hash（`metadata_store.rs:792-802`），本文件是明文 bearer token，泄露后果**重于**服务端库——仍接受（Keychain 留作后续加固项），但 §5 不把两者说成同级。

### 2.7 与本机 daemon 的关系：HTTP 面逐项去向（R6 → R2-01 修订）

UI 静态资源仍由本机 HostServer 提供（WebView 从本机 origin 加载），远程 host 只接管 WS 数据面。**撤回修订 1 的「`/v2/*` 通用 HTTP 代理」承诺**：relay 通道上没有 HTTP 承接协议（`RelaySessionBridge` 只隧道 WS 会话帧，`bootstrap_target` 是独立 HTTP handler 且只认 desktop capability）——本地加代理路由不会让远端 handler 可达。处置改为**逐项三选一**（契约 2，§5.2）： 映射到既有/新增受限 WS op； 远程模式显式禁用； 静默回落本机一律禁止。盘点是 plan 的前置任务（早于桥实现），逐项列方法/路径/长度/超时/取消。

冷启动（R6）：全新安装的笔记本冷启动进 landing，没有工作区也没有 host 入口——landing 需补「添加远程 host」入口（不改 landing 的 host 切换定位：仅添加+跳主 shell，非完整切换器）；本机 workspace 列表为空且存在已配对 host 时，主 shell 引导选远程 host。

本机 daemon 停止时远程 host 不可用（UI 本身没了）——记录为约束，不做「无本机 daemon 的纯远程模式」。

冷启动（R6）：全新安装的笔记本冷启动进 landing，没有工作区也没有 host 入口——landing 需补「添加远程 host」入口（不改 landing 的 host 切换定位：仅添加+跳主 shell，非完整切换器）；本机 workspace 列表为空且存在已配对 host 时，主 shell 引导选远程 host。

本机 daemon 停止时远程 host 不可用（UI 本身没了）——记录为约束，不做「无本机 daemon 的纯远程模式」。

## 3. UI

### 3.1 侧栏 host 按钮（Paseo 同构；拍板 B/C，见 §9）

左侧栏底部、设置按钮旁新增 host 按钮（Server 图标）——Paseo `left-sidebar.tsx:337` `SidebarHostPicker` 的同构物。点开弹层（HostPicker）：

- host 列表：本机（默认，固定在首）+ 已添加远程 host，每行=label + 连接状态徽章（connecting/online/offline/凭证失效），当前 host 高亮；v1 不做搜索框（host 数量少）。
- 「添加 host」入口：粘贴 offer URL → TOFU 确认（§3.3）→ 配对进度 → 完成。
- 行内管理：删除（§2.5 语义文案）；label v1 不可编辑（默认取 relay endpoint host，来自 offer）。
- 选择 host = **整窗切换**：整个 app 视图（侧栏会话列表、聊天、工作区）切到该 host 的数据面。当前 host 存本地偏好。不做 per-session host（母 spec §3.2.1 单写者不变量的自然延伸：一个 composer 只属于一个 host）。
- **切换事务（R5）**：整窗切换 ≠ 换一个 WebSocketClient——`app.js:595-626` 的 wsClient/transport/preferences/ConfigGateway 是模块级闭包，草稿身份（`composer-session-identity.js`）不含 host 维度。切换必须定义事务序：保存/确认未发送输入 → 失效旧 host 代数（后续所有迟到结果按 host+连接代数拒收）→ 解除订阅并拒绝旧 pending → 清侧栏/聊天投影 → 绑新 host（wsUrl + HTTP base + 草稿/缓存键按 host 隔离）→ 新 host bootstrap。实现取「稳定 transport façade」或「整层重建」二选一（plan 定），不允许只替换局部变量。跨 host 的同路径/同 ID 不代表同一目标——草稿与 keyed cache 的键必须含 host 身份。

### 3.2 Settings 不设远程主机段

host 的添加/切换/删除全部走 §3.1 弹层，Settings 不再设「远程主机」页签或段落（Paseo 的 per-host settings 页有 appearance/label 编辑，Picot v1 砍掉）。「守护进程」页签保持 daemon 侧内容不变：状态/keepRunning/重启/运行中代理/pair 区/设备管理（§3.4）。

### 3.3 TOFU 确认弹窗

key 指纹（首 8…末 8、4 字符分组）+ relay endpoint + 变更项差异列表；「新设备/密钥已变更」两态文案；确认/取消。一次一个问题；新问题自动取消未答旧问题。

### 3.4 设备管理（补 Phase 2 留白）

「守护进程」页签 pair 区旁新增已配对设备列表：deviceId、配对时间。撤销按钮 = 既有 `relay.revoke_device` op（踢连接语义已交付），确认文案列出该设备当前是否有活跃会话。**列表读取是新接口（R10）**：现有 admin 面只有 status/offer/revoke（`daemon/admin.rs:1187-1223`），status 不含设备列表——需新增只返回安全字段（无 token/hash）的本机 admin 列表 op + IPC 接线；「最后活跃」字段服务端不存在，v1 不做（「有则显示」降级为不显示）；活会话查询经既有会话注册表读取面。这是**本机 daemon 被谁连**的管理面；与 §3.1 弹层「本机连了谁」互为镜像，UI 上放同一页签可互见。

## 4. 配置入口

无新配置文件：host 列表即配置（§2.6 JSON）；relay endpoint/useTls 逐 host 存在 profile 里（来自 offer，不可手改——改连接信息只能走重新粘贴 offer 的 TOFU 路径）。

## 5. 安全边界

- 信任锚仍是 offer 链接（按密码对待）。TOFU 防「relay 被换/offer 被调包」：key/endpoint/TLS 任何变化必须人确认，变更走 §2.3 暂存事务。
- **本地桥授权（R1）**：proxy 连接须持 per-window grant（app 铸造、绑定 host、窗口关闭/切换即作废）+ Origin 精确校验；无 grant、异源、伪造 grant、跨 host grant 一律关闭。随机端口不是认证。
- deviceToken 等价远端数据面权限（实际范围见 §2.8/D2 拍板）；明文落 0600 JSON，泄露后果重于服务端 token hash（§2.6）；日志全走 token redaction；**前端收到的每一帧不含 deviceToken（R2，含配对 hello_ack 净化）**。
- 远端 admin 面（daemon.status/stop/restart）不经 relay 暴露（Phase 2 既定），客户端也**不提供**对远端 admin 的任何入口。
- 桥不解析业务帧（除首帧 hello 重写与配对 hello_ack 净化）：relay/远端注入的恶意帧最多到达 WebView，受前端既有校验面约束；超长帧由 `transport_limits` 丢弃并关该连接。

### 5.1 服务端授权面（R3/R4；D1/D2 已拍板）

军师复核证实两个产品级缺口，**不解决则「完整客户端」承诺不成立**：

- **R3 远程操作面（D2 已拍板：数据面 + 工作区导航）**：现有 Gate B 只放行数据面（`host_router.rs:135-145`：HostRequest/HostControl 仅 Desktop），而工作区列表/切换、打开会话全走 host_request（`transport.js:248-265, 716-732`），owner bootstrap 也只发 Desktop（`host_server.rs:1955-1982`）。落地方案：服务端准入谓词为 device principal 放行**导航子集**（工作区列表/会话列表/切换/打开会话的既有 op），工作区创建/删除/停止、设置、admin 维持 Desktop-only；bootstrap 同步发给 device principal。前端把任何 hello_ack 硬编码为 `{native:true}`（`websocket-client.js:714-718`），改为读服务端 capabilities。
- **R4 owner 生命周期（D1 已拍板：必须退出后可用）**：配对 token 铸给请求窗口的 owner（`daemon/admin.rs:1199-1212`、`remote_auth.rs:98-121`），runtime 请求依赖该 owner 当前 Registered workspace 与 generation（`host_server.rs:2757-2815`）——现状下宿主 app 退出（窗口 detach）后 token 能认证但失去操作授权。落地方案：device principal 的授权对象与窗口 owner 解耦——目标解析走 daemon 常驻状态（Phase 1 已保证 runtime/会话跨 detach 存活），不要求「窗口 owner 当前 Registered」；撤销语义（T10 踢连接）与单写者不变量不放松。

### 5.2 四份实施契约（军师二轮评审产物，架构定案；A/B/C/D1/D2 不重开）

**契约 1：远程会话授权与导航状态模型（R2-02/R2-03）**

- 新概念 **device view**：每个已配对设备在 daemon 侧持有独立视图状态（current workspace + session binding + generation，存 MetadataStore `relay.device_view.<deviceId>`），与窗口 owner registry 完全正交；远程导航操作 device view，**绝不触碰**窗口 owner 的 binding/navigation permit/`focusedExisting` 语义（后者留在 Desktop 路径）。
- 动作矩阵（D2 全量，逐真实 handler 验证，非仅 `admits()` 分类）：`workspace.list`/`workspace_target_prepare|commit|cancel` → device-view 分支（`host_control_dispatch.rs:1465-1755` 的 owner-registry 依赖点全部改走 device view）；Git（`git-client.js:21-36` 走 host_request `git.*`）→ 归入导航面按请求参数校验；terminal（`terminal_command`，`host_server.rs:2147-2188` 现仅 Desktop）→ 新 device 分支，cwd 取 device view 的 workspace 根；文件/聊天/steer → 既有数据面零变化；建删停工作区/设置/admin → Desktop-only 不变。路由层直查点（`host_router.rs:346-370`、`host_server.rs:3307`）全部入改动面。
- 测试口径：两远程设备 + 本地窗口并存、各自独立切换、detach/rebind、旧代请求拒绝——语义先按本契约写定，再落测试（不接受「语义由测试钉住」）。

**契约 2：HTTP 逐项去向（R2-01/R2-11）**

- 前置盘点任务枚举前端全部相对 HTTP 调用，每项三选一： 等价/新增**受限 WS op**（单方法固定 schema，长度/超时沿 `transport_limits`，禁止任意 URL 代理）； 远程模式显式禁用； 静默回落本机禁止。初判：`/v2/bootstrap` 是打开会话必经→（a）映射为受限 op；paste-offload/export 大概率 v1 禁用。
- wire 约束修订（明示，见 §12）：「不改既有帧；新增 op 白名单化」。
- bridge 端口统一 `/v2/ws`（`websocket-client.js:44-45` 校验）；HTTP 若 WS 化则无跨端口 fetch/CORS 面；`host-origin.js` 同源 capability 注入不适用于跨端口——grant 走 WS hello 字段（契约 3）。

**契约 3：握手/凭据/grant 状态机（R2-05/R2-06/R2-07/R2-09/R2-10）**

- 分层负责：`client_role` = 纯传输（dial + E2EE + 有界帧管道 + 退避重拨；状态 Connecting/Online/Offline；**不含**应用层 hello 与配对）；host 级配对事务归 host_client（唯一应用层 hello 发送者、ack 消费、token 持久化提交点、单航班）；普通 proxy 会话各自发 hello（桥重写）。每个 relay 会话恰好一次应用层 hello。
- grant：命令从注入的 `WebviewWindow` 推导身份（复用 `daemon_client/commands.rs:17-31` caller check，不信自报 window_id）；grant 作为 **hello 附加字段 `bridgeGrant`** 随首帧传输（非 HTTP header），绑定 (window, host)；窗口存活期内断线重连由桥续发新 grant；窗口关闭/切 host → registry 作废 + **桥主动关闭该窗口的活 proxy 连接**。`hello_ack` 增补 `capabilities` 对象（additive，字段集按契约 1 的面枚举；老客户端忽略；wire 修订见 §12）。
- 远端 token 不可回滚（R2-06）：staged API 只保本地；「交换成功但本地保存失败」→ 内存候选保留供重试保存；进程死亡 → 该 host 须重新生成 offer 再配对（如实文案）；不改服务端 token 模型。
- 句柄契约：client_role/桥句柄可 stop/join；删除 host 取消在途 dial/握手/等 ack。HostStore API 收敛：`load` 返回 Err(Corrupt) 即阻塞态（无双轨状态）；staged 带 generation 校验；补 delete 接口。
- 客户端资源边界（R2-10）：client data socket 半开检测（idle 无帧 30s → 关闭重拨）、wire/解密后长度上限沿 `transport_limits`、截断/篡改密文关对应连接、慢消费者只关本连接。

**契约 4：host 切换 = 页面级重载（R2-08/R5）**

- **实现策略选定**：整窗切换 = 导航到 host 参数化 URL，WebView 整页重载——模块闭包（`app.js:595-626`）、投影、pending、在途请求全部随重载消亡，零残留；草稿/缓存键按 host 命名空间存 localStorage 跨重载保留。`beginSwitch` 事务函数退场；「保存失败不切」复用现有 quit 前 flush 机制。迟到回写/代数守卫天然成立（旧页面已死）。UX 代价 ≈1s 重载闪烁，v1 接受。
- **host 选择按窗口（与多窗口 v1 组合）**：host 参数在窗口 URL 上，不在全局——窗口 A 切换只导航窗口 A，其余窗口不受影响；两个窗口各开一个 host（本机/远程或两个不同远程）是支持形态。新窗口默认打开全局最近使用的 host（仅默认值，不构成同步）。
- **桥按 host 共享、按窗口引用计数**：同一远程 host 被多个窗口使用时共享一个桥，桥上多条 proxy 连接 = 多条独立 relay 会话（plan T5 已测「two proxy connections → two independent sessions」）；窗口全部离开该 host 后桥才停（引用计数；app 退出停全部）。窗口切换/关闭只作废自己的 grant 与连接。
- **同 host 双窗口共享 device view**（契约 1 的 view 按设备不按窗口）：两窗口在同 host 各开会话互不干扰（每连接独立 hello/订阅/canonicalRoute）；但工作区级切换操作共享 device view——并发的 prepare/commit 按代数拒绝旧请求（显式报错，非静默错乱），v1 接受 last-write-wins。
- ConfigGateway/偏好：远程模式只读远端数据面可得项；本机偏好面显式降级禁用，不静默。

## 6. 与现有 LAN 配对（RemoteAuth / pair.html）的关系

并存互不影响：pair.html 是本机 LAN 直连 HTTP 配对（既有），本文是 app 作为客户端连远程 host。host_store 只存 relay 型连接；不迁移、不合并 LAN 配对数据。

## 7. 非目标

- directTcp / directSocket / remoteSsh 连接型（母 spec §8 既定不做）。
- 移动端/浏览器客户端。方向已定（2026-10-19 Dr. Lin）：移动端走原生壳 app（壳内复用现有 web UI），其第一阶段的 JS 连接层（relay client + E2EE）即原薄客户端核心，届时一并交付；「浏览器直接可用」作为独立交付物继续延后。
- per-session host 选择、多 host 并行双开（v1 整窗切换）。
- 无本机 daemon 的纯远程模式（§2.7）。
- keypair 轮换工具（远端轮换后走重新配对即可）。
+ **landing 也渲染 host 切换入口（2026-10-20 修订，取代原非目标）**：`bootstrap-entry.js` 在 native 且 `pathname === "/"` 时加载 landing，而切 host 的目标就是 `/`（`hostUrl` 重置 pathname）——若 landing 无切换器，切到任何 host 后用户都进不了工作区（B1，架构狮评审第 3 轮发现）。因此 landing 同样接 `setupHostSwitcher`：landing 承担「选 host + 选该 host 的工作区」两件事（它本就会 `sidebar.refresh()`，且 T5 已让 device principal 通过 `workspace.list`）。

## 8. 阶段划分与工作量

修订 2 口径（二轮评审后）：一轮的 19–27 人日未含远程会话状态模型、HTTP 逐项改造、切换重载接线。重估在 plan Draft 3 按任务粒度给出；最大超支点确认为 T3/T4 授权与导航执行、HTTP 改造、全窗状态重绑（军师二轮风险节），非 E2EE 算法。

## 9. 拍板记录

| # | 问题 | 结论 | 备注 |
| --- | --- | --- | --- |
| A | deviceToken 存储 | **已拍板（2026-10-19 Dr. Lin）**：app 私有 0600 JSON | macOS Keychain（`keyring`）留作后续加固项，不在本期 |
| B | 远程主机设置放哪 | **已拍板（2026-10-19 Dr. Lin）**：不进 Settings——侧栏底部设置按钮旁新增 host 按钮（Paseo `SidebarHostPicker` 同构），弹层内添加/切换/删除 | 原「守护进程页签分段」与「独立页签」两案作废 |
| C | host 切换形态 | **随 B 定型（2026-10-19）**：整窗切换（弹层选择即切）；per-session host 留 v2 再议 | |
| D1 | 远程访问是否必须在**宿主 app 退出后**持续可操作（R4） | **已拍板（2026-10-19 Dr. Lin）：是，必须** | principal 授权与窗口 owner 解耦（§5.1 落地方案，服务端任务） |
| D2 | 「完整远程客户端」的动作面（R3） | **已拍板（2026-10-19 Dr. Lin）：数据面 + 工作区导航**（列工作区/会话列表/切换/打开会话/聊天/steer/文件/git/terminal）；工作区创建/删除/停止、设置、admin 远程不放行 | 导航子集进准入谓词；bootstrap 发 device principal（§5.1） |


## 10. 测试与验收

- **Rust 单测**：TOFU 判定纯函数全分支（精确匹配自动通过/新 host/key·relay·TLS 每项变更/拒绝/**暂存事务的失败不覆盖**）；hello 重写（认证三元组替换、其余字段逐字节保留、**首帧非 hello 关闭**）；**hello_ack 净化（前端侧收到的帧不含 deviceToken）**；host_store round-trip/**损坏保留+阻止覆盖保存**/0600 创建/原子替换/写失败保旧；client_role 握手超时与重发节奏（注入时钟）；**桥调用方认证全拒绝分支（无 grant/异源/伪造/跨 host/窗口已关）**。
- **mock relay e2e**（复用 Phase 2 mock relay 测试基座，双端同进程：app 桥 ↔ mock relay ↔ picot-daemon server-role）：添加 host 一扫码全流程（offer → TOFU 自动通过 → pairingToken hello → hello_ack deviceToken → 落盘）；配对后完整会话往返（订阅/请求/快照，Gate B 面）；断 relay → 退避重连 → 会话恢复；远端 revoke → 桥关 proxy → 前端标记凭证失效；慢消费者只关该连接；TLS 契约（fixture CA 过、系统根拒未知 CA）复用 Phase 2 测试形态。
- **jsdom**：host 弹层状态机（connecting/online/offline/失效、当前项高亮、选择持久化、添加/删除流）、TOFU 弹窗两态与单问题排队、四 locale 键全覆盖。
- **真实 relay 冒烟（非 CI）**：本机两套隔离 root（app 侧 + daemon 侧）经 `paseo.021521.xyz` 走完添加 host → 会话往返 → 撤销，证据文档进 `process-evidence/`。
- 完成门：`bun run check:rust` + `bun run check` + `bun run test` 三绿；ARCHITECTURE 远程访问章补客户端侧事实。

## 11. Review-Fix Mapping（军师评审 2026-10-19 → 处置 → 落点）

| Item | Disposition | Landing |
| --- | --- | --- |
| R1 loopback 桥缺调用方认证 | Accepted — per-window grant + Origin 精确校验 + 作废时机 | §2.2、§5；plan T3/T4 重拆 |
| R2 双向透传与「token 不进 WebView」冲突 | Accepted — 配对 hello_ack 净化 + 持久化接管 + 保存失败反馈；pairingToken 措辞校准 | §2.2、§2.5、§5；plan Secrets/T3 |
| R3 Gate B 不支持完整远程操作面 | Accepted — 升产品拍板 D2 + 动作矩阵 + 服务端配套任务；capabilities 门控改读服务端 | §5.1、§9 D2；plan 重拆（服务端前置任务） |
| R4 配对绑定窗口 owner，退出后失权 | Accepted — 升产品拍板 D1 + owner 生命周期修订（若拍「是」） | §5.1、§9 D1；plan 重拆 |
| R5 整窗切换清理面远超换 wsClient | Accepted — 切换事务序 + 代数守卫 + 草稿/缓存键含 host | §3.1；plan T5 |
| R6 HTTP bootstrap/冷启动入口未处理 | Accepted — 桥增 HTTP 代理 + HTTP base 随 host 切 + 盘点清单 + landing 添加入口 | §2.7；plan T5/T6 |
| R7 重连状态机矛盾（假在线） | Accepted — 底层丢失即关 proxy；桥管 relay 重拨、前端管 proxy 重连；失效只认显式拒绝 | §2.4；plan T1/T3/T5/T7 |
| R8 配对竞争/deviceId 覆盖/替换事务 | Accepted — host 级单航班 + app 级稳定 deviceId + TOFU 暂存事务 + 删除取消在途 | §2.3、§2.5；plan T2-T4 |
| R9 损坏回退丢 TOFU 基线；hash≠明文 | Accepted — 损坏保留+阻止覆盖保存+明确提示；泄露后果表述修正 | §2.6、§5；plan T2 |
| R10 设备列表接口不存在 | Accepted — 新增安全字段 admin 列表 op + IPC；最后活跃 v1 不做 | §3.4；plan T4/T6 |
| R11 缺真实用户路径验收门 | Accepted — 真实 Tauri WebView + 真 relay 的冷启动→添加→列工作区→开 session→切回矩阵；T1 lift 加 green→green 基线 | plan T1/T8 重拆 |
| R12 文档状态/参数残留 | Accepted — 状态头改「修订 1」；重发间隔契约区分继承（smoke 2s）与选择（对齐 Paseo 1s），T1 统一 | 状态行；plan T1 |

### Round 2（军师二轮 2026-10-19 → 处置 → 落点）

| Item | Disposition | Landing |
| --- | --- | --- |
| R2-01 HTTP 无跨 relay 承接协议 | Accepted — 撤回通用代理；盘点 + 逐项三选一 | §2.7、§5.2 契约 2；plan Draft 3 前置任务 |
| R2-02 导航仍依赖窗口 owner，admits() 不够 | Accepted — device view 状态模型 + 全 handler 改动面 | §5.2 契约 1；plan T3/T4 重排 |
| R2-03 D2 遗漏 Git/terminal | Accepted — 动作矩阵纳入 git.* 与 terminal_command device 分支 | §5.2 契约 1；T4/T10 |
| R2-04 capabilities 只改收端 | Accepted — hello_ack 增补 capabilities（wire 修订入 §12） | 契约 3；plan T4 发送端 |
| R2-05 T1/T5 双握手、句柄缺取消 | Accepted — 分层负责：client_role 纯传输，配对归 host_client；stop/join 契约 | 契约 3；plan T1/T5/T6 |
| R2-06 暂存事务保不了远端旧 token | Accepted — 如实恢复态：内存候选重试保存，进程死亡须重配对 | 契约 3；plan T2/T6 |
| R2-07 grant 信任链/续发/活连接 | Accepted — 注入窗口推导身份 + hello 字段传输 + 作废即关活连接 | 契约 3；plan T5/T6/T7 |
| R2-08 切换未选策略、无失败契约 | Accepted — 选定页面级重载（§12 待点头） | 契约 4；plan T7 重排 |
| R2-09 HostStore 接口不自洽 | Accepted — Err(Corrupt) 单轨阻塞态 + generation + delete | 契约 3；plan T2 |
| R2-10 缺半开检测/客户端资源边界 | Accepted — idle 30s 半开 + transport_limits 全套 | 契约 3；plan T1/T5/T9 |
| R2-11 /ws vs /v2/ws、commands 注册/CSP | Accepted — 统一 /v2/ws；main.rs generate_handler! + permissions/default.toml 入清单；CSP 不放宽 | 契约 2/3；plan 文件清单 |
| R2-12 daemon 重启语义/估算/状态残留 | Accepted — 重启=磁盘会话重开（非 turn 续跑）；§2.1/§8 状态行已修；估算入 Draft 3；T0 章节标「目标设计」 | §2.1、§8、契约 1；plan T0/T3/T10 |

**军师确认成立、不重开的假设**：app Rust 终结 E2EE + per-host loopback 桥；role=client 单 data socket；offer codec/NaCl box/base64 帧复用（smoke_client 已证明组件存在，但它是一次性客户端非长期状态机）；relay 上必须 Remote 身份（前提是桥先验本地调用方，R1）；0600 文件/侧栏按钮/整窗切换；不做 directTcp/SSH/移动端；不必为薄客户端预造框架。

## 12. 已点头（2026-10-20 Dr. Lin：同意军师意见）

1. **wire 扩展**（契约 2/3）：新增受限 WS op（如 bootstrap 映射）+ `hello.bridgeGrant` 字段 + `hello_ack.capabilities` 对象——均为 additive、白名单化、老客户端兼容；「不改 wire」约束修订为「不改既有帧，新增 op/字段白名单化并逐项记录」。不点头则远程模式砍掉对应功能（bootstrap 不可 WS 化 = 远程无法打开会话，实质否定 D2）。
2. **页面级重载切换**（契约 4）：切换 host = 整页重载（≈1s 闪烁），换取零状态残留；不做闭包内热切换。不点头则回退「稳定 façade」路线（工作量 +2~3 人日，R2-08 全部失败态仍需逐项处理）。

**状态**：两项均已同意，plan 重排为 Draft 3（`docs/superpowers/plans/2026-10-19-relay-phase2-5-desktop-host-client.md`）；终审通过后开工。
