# Relay Phase 2 — 真 relay 冒烟（plan T12）证据

**日期：** 2026-10-18
**任务：** plan T12——daemon 对公司真 relay（`paseo.021521.xyz:443`）的实跑冒烟：TLS + control 连接 → relay.set_enabled → relay.offer → CLI 客户端走完 offer→E2EE→配对→数据面探针 → 同 offer 重放拒绝。
**被测树：** 分支 `daemon-phase1`，HEAD `7115f66` + 本轮文档（工作区），daemon 二进制 `cargo build -p picot-daemon`（dev profile，buildId `v0.3.6.1-478-g7115f66-dirty`）。
**环境：** macOS arm64（本机），隔离 throwaway 根 `$HOME/tmp/picot-relay-smoke-20261018-155646`（下称 `$ISO`）。
**结论先行：SMOKE OK**——daemon 经 admin op 连上真 relay 并达 `online`；CLI 客户端对真 relay 完成 TLS 拨号、E2EE 握手、一扫码配对（device token 铸出）、数据面探针收到关联响应帧（`unauthorized_target`，占位 target 的预期错误码）；同一 offer 二次消费被拒（`unauthorized_device`）。证书链是 Let's Encrypt（ISRG Root X1），系统信任库原生可验，无公司 CA 阻塞。

## 0. 隔离规则

- daemon 以 `HOME=$ISO/home` 启动，`--data-dir/--config-dir/--static-dir` 全指向 `$ISO` 内目录（static 为单行占位 index.html，复用 `scripts/smoke-daemon-matrix.sh` 的 standalone 模式）。
- admin token/port 从实例文件 `$ISO/data/picot-daemon.json` 读取（该文件只由 daemon 写，tmp+rename 原子）。
- 冒烟客户端本身零落盘（库文档声明：不读不写用户数据/配置目录，token 不进 argv）。
- 红线核查（运行后）：真实 `~/.pi/picot-runtimes` 与 `~/.pi/agent` 下无 daemon 写入（该窗口新增的 6 个文件全部是**本 agent 会话自己的 transcript / cache-optimizer 统计**，与 picot-daemon 无关；daemon 的 `~/.pi` 解析到 `$ISO/home`）；真实 app data 目录未出现任何 daemon 实例文件。
- 已知副作用（无害，记录在案）：首次尝试曾以 `HOME=$ISO/home` 跑 `cargo run`（后改为先 build 再直跑二进制），cargo 把 registry 缓存写进了 `$ISO/home/.cargo`——全部在 throwaway 根内。

## 1. 前置：relay 存活与证书链

| 步骤 | 命令 | 期望 | 实际 |
| --- | --- | --- | --- |
| /health | `curl -s -o /tmp/relay-health.json -w "%{http_code}" https://paseo.021521.xyz/health` | 200 | **200**，body `{"status":"ok","version":"v0.5.0"}` |
| 证书链 | `openssl s_client -connect paseo.021521.xyz:443 -servername paseo.021521.xyz` | 公共 CA 链 | Let's Encrypt（`CN=paseo.021521.xyz` ← YE2 中间证书 ← ISRG Root X1/X2），系统信任库原生可验——**无公司 CA 不在信任库的问题** |

## 2. daemon 起动（隔离 HOME）

```bash
cargo build -p picot-daemon                      # exit 0, 2m20s
HOME="$ISO/home" src-tauri/target/debug/picot-daemon \
  --data-dir "$ISO/data" --config-dir "$ISO/data" --static-dir "$ISO/static" &
```

期望：实例文件 `<data>/picot-daemon.json` 出现（daemon ready 后才发布）。实际（adminToken 脱敏）：

```
{'pid': 64245, 'hostOrigin': 'http://127.0.0.1:54347',
 'adminOrigin': 'http://127.0.0.1:54348', 'version': '0.5.0',
 'buildId': 'v0.3.6.1-478-g7115f66-dirty'}
```

## 3. admin op 驱动（enable → online → offer）

一次性驱动脚本（bun，内置 WebSocket；完整源码在 `$ISO/evidence/admin-relay-driver.mjs`，要点）：读实例文件拿 adminOrigin/adminToken → `ws://…/admin` → `admin_hello{role:"manager"}` → `owner.create` → `relay.set_enabled{enabled:true}` → 轮询 `relay.status` 至 `online` → `relay.offer{ownerId}`。

实测输出（token/公钥字段脱敏不变——本步输出本身不含 secret）：

```
[ok] admin_welcome received
[ok] owner.create -> 22c5bf47f56f33e334f1ee331ee20941
[ok] relay.set_enabled -> {"enabled":true}
[ok] relay online: endpoint=paseo.021521.xyz:443 serverId=cc1cba36-ba5a-4625-92cb-6e1696675be5
[ok] relay.offer expiresAt=1792311027 url=https://picot.021521.xyz/#offer=eyJ2IjoyLCJzZXJ2ZXJJZCI6ImNjMWNiYTM2…（截断）
```

| 断言 | 期望 | 实际 |
| --- | --- | --- |
| TLS control 连接 | `relay.status` 达 `online`（native-tls + 系统信任库对真 relay 握手成功） | **online**（首次启用即连上，未触发退避窗口） |
| identity 持久 | serverId 稳定 | 两轮驱动（同 daemon 实例）serverId 均为 `cc1cba36-ba5a-4625-92cb-6e1696675be5` |
| offer TTL | `expiresAt − now ≈ 300s` | mint 时 `now=1792310727`，`expiresAt=1792311027`（差恰 300s） |
| offer 形态 | `https://picot.021521.xyz/#offer=<base64url>` | 一致（fragment 解码含 `serverId`/`daemonPublicKeyB64`/`relay{endpoint,useTls:true}`/`pairingToken`） |
| 未知 owner 拒绝 | `relay.offer{ownerId:"smoke-owner-1"}`（未注册 owner）应拒 | 首轮误传未注册 id 得 `{"ok":false,"error":"unknown owner smoke-owner-1"}`（守卫有效；改用 `owner.create` 的 id 后通过） |

## 4. CLI 冒烟客户端（e2ee + 配对 + 数据面探针）

```bash
cargo build -p picot-host --example relay-smoke-client   # exit 0
HOME="$ISO/home" RELAY_SMOKE_OFFER="$(cat $ISO/evidence/offer-url.txt)" \
  timeout 60 src-tauri/target/debug/examples/relay-smoke-client
```

首轮实际输出（原文，secrets 构造性脱敏）：

```
picot relay smoke client (plan T11)
endpoint: paseo.021521.xyz:443 (tls)
serverId: cc1cba36-ba5a-4625-92cb-6e1696675be5
e2ee handshake: ready
pairing: one-scan exchange ok (device token "picot_device…" received, redacted)
data-plane probe: error -> error unauthorized_target (requestId smoke-1)
SMOKE OK
```

退出码 **0**。逐行解读：

- `tls` + `e2ee handshake: ready`：对真 relay 的 wss 拨号与 E2EE 握手（`e2ee_hello`→`e2ee_ready`）完成；
- `one-scan exchange ok`：hello 内嵌 pairingToken，`hello_ack` 带回铸出的 device token（只渲染前缀）；
- `data-plane probe: error -> error unauthorized_target`：探针 target 是占位值（无 live runtime），收到**关联的错误响应帧**——加密请求/响应管道往返成立的证据；正向数据面 op（`runtime_subscribed` 等）由 mock-relay 集成测试覆盖（`tests/relay_*`），本冒烟不重复；
- `SMOKE OK`：报告判定 = 配对交换 + 关联响应帧均达成。

## 5. 同 offer 重放（一次性凭据）

同 offer URL 立即二次消费（TTL 未到，排除过期因素）：

```
relay-smoke-client: pairing rejected: unauthorized_device
```

退出码 **1**。期望达成：pairingToken 一次性，二次 exchange 被拒。

## 6. 收尾与 daemon 侧证据

- `kill -TERM 64245` → daemon 日志 `exit code 0 (SIGTERM)`，实例文件与锁文件均被移除（优雅退出路径完好）。
- daemon INFO 日志不含 relay 会话细节（`relay session attached` 等为 debug 级，log_sink 装在 Info）；本冒烟的连接/会话证据以客户端输出 + admin op 返回为准。若后续要 daemon 侧日志断言，需临时调 log level 或读 debug 构建——不在本轮范围。
- 观察到（记录，非缺陷）：驱动脚本退出即 admin 断连，daemon 按短宽限回收该 attachment epoch 并撤销其 owner（`attachment epoch N died: 1 owner(s) revoked`）——这是设计内行为；本冒烟的探针不依赖 live owner，不受影响。
- 驱动脚本自身第一版有 bug（welcome 等待覆盖了 `admin_result` 分发导致 op 超时），修复后全绿——脚本是本轮一次性产物，非产品代码。

## 7. 未覆盖项（如实）

- 正向 Gate B 数据面 op（remote 配对后对 live runtime 的 `runtime_subscribed`/命令往返）——需要活 owner+runtime 的编排，属 mock-relay 集成测试域；真 relay 冒烟以关联错误帧证明管道。
- 撤销已连设备（`relay.revoke_device` 对真 relay 踢连接）——同上，测试域（`tests/` 覆盖 kick 语义），未在真 relay 重演。
- 不可达/错 endpoint 降级与退避序列——mock-relay 契约测试覆盖，未在真 relay 拔线。
