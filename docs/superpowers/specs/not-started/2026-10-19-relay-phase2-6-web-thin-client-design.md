# Relay Phase 2.6：浏览器薄客户端设计（重点稿）

**状态：** Draft-重点稿 — 只记已确认的定位与拍板；完整 spec 待 Phase 2.5 落地后细化再评审（Dr. Lin 2026-10-19 指示）。
**日期：** 2026-10-19
**演化关系：** 母 spec `implemented/2026-09-20-persistent-daemon-relay-design.md` §4.6 之后的移动端方向；依赖 Phase 2.5（桌面多 host 客户端）的连接管理层 seam。方向拍板见 Phase 2.5 spec §7：移动端走原生壳 app（壳内复用 web UI），其第一阶段的 JS 连接层即本文；「浏览器直接可用」作为交付物随之获得。

## 1. 定位

浏览器里可用的完整远程 Picot 客户端；同一份 JS 连接层即移动端原生壳 app 的第一阶段代码。

## 2. 重点设计（要点级，细节日后展开）

- **形态**：同一份 `public/` 前端（零构建改动）静态托管；offer 链接 `https://<托管域名>/#offer=<payload>` 打开即进配对。运行时判模式：origin 在托管域名白名单 → hosted 客户端模式；daemon 服务的 origin → 现状本地模式零改动。
- **JS 连接层（核心工作）**：relay client-role 拨号（`wss://<relay>/ws?serverId&role=client&v=2`，单 data socket）；tweetnacl.js NaCl box E2EE（协议照 Rust 实现逐帧对齐，现成向量锁互通）；hello 浏览器自发 `clientType:"remote"` + deviceToken（无桥）；offer codec 出 JS 版（port `pairing.rs`）。
- **seam**：复用 2.5 T5 的 host-connection 抽象（transport + 客户端身份注入）。
- **存储/TOFU**：host 列表 + deviceToken 存 localStorage（存储做成接口，壳 app 换原生钥匙串）；TOFU 新 host 与 key/relay/TLS 变更弹确认。
- **桌面侧配套**：pair 区恢复 QR + requiresClient 文案改「用浏览器或 Picot 客户端打开」。
- **非目标**：推送/离线 PWA、token 云同步、原生壳（壳 app 届时单独立项）、per-session host。

## 3. 拍板记录

| # | 问题 | 结论 | 备注 |
| --- | --- | --- | --- |
| 托管 | 托管服务器与 relay 的关系 | **可能不同服务器（2026-10-19 Dr. Lin）**——不定死同机 | 架构上零成本：offer payload 本就独立携带 relay endpoint，`APP_BASE_URL` 只是渲染 base，二者解耦；ops 届时定 |
| 时点 | 实施时间 | **2.5 落地后，具体时点还要再仔细讨论（2026-10-19 Dr. Lin）** | 本稿只存档，不开工 |
