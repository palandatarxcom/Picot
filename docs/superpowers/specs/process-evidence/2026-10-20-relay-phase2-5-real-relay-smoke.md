# Relay Phase 2.5 — 真实 relay 冒烟与验收证据（plan T11）

**日期：** 2026-10-20
**范围：** plan `2026-10-19-relay-phase2-5-desktop-host-client.md` T11。本文件记录**已自动验证**与**待人工 GUI 验证**两部分，不做「全部通过」的笼统结论。
**被测树：** 分支 `daemon-phase1`，HEAD `e0d6da3`（T10 收尾）＋本轮文档，工作区干净。daemon 二进制 `cargo build -p picot-daemon`（dev profile，buildId `v0.3.6.1-499-ge0d6da3`）。
**环境：** macOS arm64（本机），隔离根 `$HOME/tmp/picot-relay-smoke-20261020-165714`（下称 `$ISO`），真实 relay `paseo.021521.xyz:443`。

## 0. 隔离规则

- daemon 以 `HOME=$ISO/home` 启动，`--data-dir/--config-dir` 指向 `$ISO/data`，`--static-dir` 为单行占位 `index.html`（沿用 `scripts/smoke-daemon-matrix.sh` 的 standalone 形态）。
- 驱动脚本（`/tmp/t11-driver.mjs`、`/tmp/t11-devicelist.mjs`）是一次性产物，非产品代码（沿用 Phase 2 冒烟 `2026-10-18-relay-phase2-real-relay-smoke.md` 的惯例）。管理令牌从实例文件读取，不进 argv、不进输出。
- 收尾核查：daemon 退出后真实 `~/.pi` 与真实 app data 目录无本轮写入（daemon 的 `~/.pi` 解析到 `$ISO/home`）。

## 1. 已验证（自动，可复跑）

| 步骤 | 命令 | 期望 | 实际 |
| --- | --- | --- | --- |
| relay 存活 | `curl -s -m 10 -o /tmp/relay-health.json -w "%{http_code}" https://paseo.021521.xyz/health` | 200 | **200**，`{"status":"ok","version":"v0.5.0"}` |
| daemon 起动（隔离 HOME） | `HOME=$ISO/home picot-daemon --data-dir … --static-dir …` | 实例文件出现 | 实例文件 `picot-daemon.json`（0600），buildId `v0.3.6.1-499-ge0d6da3` |
| 管理面建 owner | admin `owner.create{label,origin,port,cwd}` | 返回 ownerId | `bbe076a4dd05667015b207f1f27a462f` |
| 启用 relay | admin `relay.set_enabled{enabled:true}` | `{enabled:true}` | `{"enabled":true}` |
| **控制连接上线（真 relay + TLS）** | admin `relay.status` | `state:"online"` | `{"state":"online","endpoint":"paseo.021521.xyz:443","serverId":"89c0c46f-…"}` |
| 配对 offer | admin `relay.offer{ownerId}` | `#offer=` URL | `https://picot.021521.xyz/#offer=…`（payload 含 serverId/公钥/relay endpoint/pairingToken） |
| **客户端 E2EE 握手（真 relay）** | `relay-smoke-client --offer-url <offer>` | `e2ee handshake: ready` | `e2ee handshake: ready` |
| **一扫码配对** | 同上 | device token 铸出（脱敏） | `pairing: one-scan exchange ok (device token "picot_device…" received, redacted)` |
| **数据面探针** | 同上 | 关联响应帧 | `error -> error unauthorized_target (requestId smoke-1)`（占位 target 的预期错误码，证明请求/响应管道往返） |
| 判定 | 同上 | `SMOKE OK` | `SMOKE OK`，exit 0 |
| 设备列表（安全字段） | admin `relay.device_list` | 无 token/hash | `{"devices":[{"deviceId":"smoke-1","owner":"bbe076a4…","pairedAt":1792486966}]}` |
| 优雅停机 | admin `daemon.stop{reason}` | accepted → exit 0 | `accepted`；日志 `exit code 0 (daemon.stop)`；实例文件与锁文件均已移除 |
| 日志脱敏 | `grep -cE "picot_device_[A-Za-z0-9]{8,}|picot_pair_[A-Za-z0-9]{8,}" daemon.log` | 0 | **0** |

**结论（自动部分）**：daemon 出站 TLS、E2EE 握手、一扫码配对、数据面关联帧、设备列表安全字段、优雅停机与脱敏，在**真实 relay** 与**当前 HEAD** 上成立。这覆盖了 T11 验收矩阵中所有不依赖 GUI 的行。

## 2. 未验证（需真实 Tauri WebView + 人工操作）

以下行**本轮未执行**，不得视为通过。它们依赖真实 GUI 交互（侧栏 host 按钮、TOFU 弹窗、会话渲染），mock-relay 集成测试覆盖了其协议层，但没有覆盖端到端用户路径（军师 R11 明确要求这条门不能由 mock 替代）：

1. 主 shell 冷启动 → 侧栏 host 按钮 → 粘贴 offer → TOFU 新主机确认 → 桥建立；
2. 列出**远端**工作区（D2 导航面）并在 UI 中切换；
3. 打开远端会话并驱动一个 turn（数据面往返的可见结果）；
4. 远端文件操作、Git 操作、terminal 输入/输出（D2 全量面）；
5. 切换 host 本机↔远程（页面级重载）、双窗口各开一个 host；
6. **宿主 app 退出后远程仍可操作**（D1 的产品级实证）；
7. 从设备管理 UI 撤销 → 客户端表现为 `credentialInvalid` 且停止自动重连；
8. 断网降级 → 恢复重连的 UI 表现。
9. **真 relay 空闲 >60s 不掉线**（架构狮 F-09）：client_role 的半开判定是「30s 内无任何入站帧即判死」，靠 10s 一次的 WS ping 换 pong 刷新。若真 relay 不回 client ping，健康的空闲会话会被本地每 30s 误判死并重拨。mock relay 覆盖了这条路径，真 relay 只在短会话上验过。观测方法：连上后保持空闲 90s，daemon 侧 `relay.status` 仍为 `online`，客户端日志无 `offline→connecting` 循环。

**执行方式**（建议）：两台真机（桌面 = host，笔记本 = 客户端），或本机双隔离根 + 两个 Picot 实例。命令与前置条件见本文件 §1 与 plan T11 Step 1。

## 3. 未覆盖项（如实）

- 真实 relay 上的**桥**（`host_bridge`，hello 重写 + grant）未跑通端到端——桥运行在 app 进程（Tauri），无 GUI 场景无法驱动；其协议行为由 `tests/relay_host_bridge.rs`（10 项）与 mock-relay e2e 覆盖。
- 撤销对**活**连接的踢线未在真 relay 重演（由 `tests/relay_navigation_admission.rs` / T4 套件覆盖）。
- TLS 客户端契约（fixture CA 通过、系统根拒绝未知 CA）由 `tests/relay_client_degradation.rs` 覆盖，非真 relay 演练（真 relay 用公共 CA，见 Phase 2 证据 §1）。
- 跨平台（Windows sidecar）不在本线范围（D9 macOS-first）。

## 4. 文档状态

- ARCHITECTURE 远程访问章的 Phase 2.5 节**保持「目标设计」口径**：本次只验证了传输/加密/配对半，GUI 用户路径（§2）未验证，故不翻转为现行事实（plan T0 的翻转条件未满足）。
- 待 §2 全部通过后，再更新：ARCHITECTURE 该节 → 现行事实；母 spec §4.6 状态行；本计划勾选；pair-device spec 归档状态。
