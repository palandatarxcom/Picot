# Relay Phase 2.5：前端 HTTP 调用盘点与逐项处置

**状态：** Task 1 完成（plan `2026-10-19-relay-phase2-5-desktop-host-client.md` Draft 3）— 2026-10-20
**上游：** spec `not-started/2026-10-19-relay-phase2-5-desktop-host-client-design.md` §2.7 + §5.2 契约 2（R2-01）
**用途：** 远程 host 模式下 relay 通道只隧道 WS 会话帧，没有 HTTP 承接协议（`RelaySessionBridge` 只转发会话帧，`bootstrap_target` 是只认 desktop capability 的独立 HTTP handler）。因此前端每个相对 HTTP 调用都必须逐项处置：映射到既有/新增受限 WS op、远程模式显式禁用，或显式保持本机。**静默回落本机一律禁止。** 本表是 T5（服务端 op）与 T8（前端改写）的输入，并由 `public/http-inventory-drift.test.js` 守住新增调用点必须入表。

## 结论摘要

- 全量枚举结果：`public/` 下（排除 `vendor/` 与 `*.test.js`）共 **8 个** HTTP 调用点，分布在 7 个文件、5 条路径。
- 只有 `/v2/bootstrap` 需要在远程模式下真正可用（打开会话必经），处置为**新受限 WS op `workspace.bootstrap`**。
- `/v2/paste-offload`（两处）与 `/v2/session-export`（经 `exportUrl`）在远程模式**显式禁用**；会话 HTML 导出仍可走本机原生命令路径（`save_session_export` 走 host_control，Desktop-only 不变）。
- `/locales/*.json` 是 UI shell 静态资源，始终由**本机** HostServer 服务（spec §2.7：UI 也由本机提供），显式保持本机，不属数据面回落。
- `public/pair.html` 两处（`/v2/auth/exchange`、`/v2/mobile/status`）是本机 LAN 配对页，与 relay 客户端路径无关，远程模式不加载该页。
- 未发现被漏掉的隐藏调用点：全文 grep + 漂移守卫双向校验（调用点↔表格），另列出非 `fetch` 的相对 HTTP 面供 T8 参考。

## 盘点表

| 调用点（file:line） | 方法/路径 | 消费方 | 处置（既有 WS op / 新受限 op / 远程禁用） | 测试钩子 |
| --- | --- | --- | --- | --- |
| `public/app/websocket-client.js:260` | GET `/v2/bootstrap?workspaceId&sessionId`（`loadCanonicalTarget`，header 走 `host-origin.js` 注入的 desktop capability） | hello 前的 canonical 路由三元组（workspace/session/instance）解析 | **新受限 op `workspace.bootstrap`**：单方法固定 schema（入参 workspaceId+sessionId，出参与现 `bootstrap_target` 的 `RuntimeTarget` 同形），长度/超时沿 `transport_limits`（`WS_PHYSICAL_FRAME_BYTES` / 数据面帧上限），**不接受任意 URL 代理**；归属 T5 服务端 + T8 前端改写 | 漂移守卫；T5 op 契约测试（固定 schema + 越界拒绝）；T8 前端用例 |
| `public/app.js:4012` | POST `/v2/paste-offload`（body 上限 `PASTE_BODY_BYTES` 4 MiB） | 主界面 composer 的粘贴落盘（大段粘贴换成文件引用） | **远程禁用（v1）**：远程 host 上粘贴退化为纯文本就地处理，UI 显式提示不落盘，不回落本机 host | 漂移守卫；T8 远程禁用态用例 |
| `public/ephemeral-chat-view.js:201` | POST `/v2/paste-offload` | Side/Quick Chat 浮窗的粘贴落盘（同一路由、独立调用点） | **远程禁用（v1）**：同主界面口径 | 漂移守卫；T8 远程禁用态用例 |
| `public/sidebar/index.js:1802` | GET `result.exportUrl`（即 `/v2/session-export/{token}`，一次性 token，TTL 由 `SessionExportRegistry` 管） | 侧栏「导出会话 HTML」：先 `session_export` 换一次性 URL，再 fetch 成 blob 下载 | **远程禁用（v1）**：远程模式隐藏导出入口；本机原生保存路径（`save_session_export` 走 host_control）不受影响 | 漂移守卫；T8 导出入口可见性用例 |
| `public/settings/package-browse.js:85` | GET `https://raw.githubusercontent.com/palandatarxcom/Picot/main/community-extensions.json`（绝对第三方 URL，非 host 相对） | Settings → 扩展「社区包市场」目录拉取 | **远程禁用**：设置面整体是 Desktop-only 本机 SSOT（D2），远程模式下设置页不提供该能力，不以「绝对 URL 与 host 无关」为由放行 | 漂移守卫 |
| `public/i18n.js:108` | GET `/locales/${locale}.json`（静态资源，`cache: no-store`） | UI shell 文案加载（四语） | **显式保持本机**：UI 静态资源始终由本机 HostServer 服务（spec §2.7），不是数据面回落；host 切换不改变该 origin | 漂移守卫 |
| `public/pair.html:133` | POST `/v2/auth/exchange`（body: pairingToken + deviceId） | 本机 LAN 配对页（浏览器/移动端直连本机 loopback） | **远程禁用**：relay 客户端配对走 hello 内嵌 exchange + `hello_ack` 铸 token，不经此页；远程 host 无 loopback HTTP 面 | 漂移守卫 |
| `public/pair.html:155` | GET `/v2/mobile/status`（Bearer deviceToken） | 同上，配对成功后的连通性回显 | **远程禁用**：同上 | 漂移守卫 |

登记字段说明：`file:line` 只作人工导航（行号会随无关编辑漂移），漂移守卫按**文件 + 每个文件的调用点数 + 字面路径**断言，不核对行号。

## 非 `fetch` 的相对 HTTP 面（不入漂移表，T8 需按用途处理）

- **`/v2/ws`（WebSocket，非 HTTP 调用）**：`public/app/websocket-client.js:29,44`——地址由页面 origin 拼出并强制以 `/v2/ws` 结尾。T8 把它泛化为「当前 host 的桥 origin」，桥端口同路径。
- **`/v2/files/raw`**：`public/app/transport.js:472` 的 `fileRawUrl()` 仍构造 `/v2/files/raw?workspaceId&path`，但图片/PDF 预览已改走数据面 `file_raw`（`file-preview-panel.js` 取 base64 → data URL），当前**没有**活的浏览器子资源请求；该 URL 只在非图片/PDF 分支被算出而未被渲染器使用（渲染器只对 image/pdf 消费 `rawUrl`）。若将来让 `<img src>` 直接指向该路由，远程模式须禁用（子资源请求无法携带 capability/grant）。
- **`/v2/session-history/{session_id}`**：host 侧仍注册该路由，前端无调用点（历史走 `read_session_messages` 数据面 op，已 grep 确认），不登记。
- **静态 shell 与模块加载**：`public/index.html` 的 `<script src>`、`public/bootstrap-entry.js` 的动态 `import()`、`pdf.worker.js`（Worker）、`vendor/chart.js`、`/locales/*.json`——都由本机 HostServer 的静态兜底服务，随 host 切换不变。
- **`public/vendor/pdf.js` / `pdf.worker.js`**：第三方打包产物内部的 `fetch(`/`XMLHttpRequest`（标准字体、cmap、worker 资源）跟随其入参 URL（当前是 data URL 或同源 vendor 资产），不属本项目 API 面。

## 扫描边界与漂移守卫

`public/http-inventory-drift.test.js` 的扫描口径：

- 范围：`public/**/*.{js,html}`；**排除** `public/vendor/**`（第三方打包）与 `*.test.js`（测试替身，不是产品调用点）。
- 命中模式：`fetch(`、`fetchImpl(`（可注入 fetch 的包装，如 `loadCanonicalTarget`）、`new XMLHttpRequest(`、`new Request(`。
- 断言：扫描到的文件集合与本表文件集合互相覆盖；每个文件的调用点数与本表行数逐一相等（新增调用点必须新增一行，删调用点必须删行）；每个调用点第一参数的字面路径（截到 `?` 前）出现在该文件的某一行里；每行「处置」列非空且属于四类之一（既有 WS op / 新受限 op / 远程禁用 / 显式保持本机）。
- 不核对行号（见上），也不扫描 `src-tauri/`（Rust 侧 HTTP 客户端不在本契约面内）。

## 与计划的偏差

- plan T1 只点了 bootstrap / paste-offload / export 三项。实测多出 5 个调用点：`/locales/*`（静态资源）、社区目录绝对 URL、`pair.html` 两个 LAN 配对调用。处置按用途判定，drop-in 与计划意图一致：只有 bootstrap 升级为受限 WS op，其余显式禁用或显式保持本机。
- 表内新增第四类处置「显式保持本机」，仅用于 UI shell 静态资源。契约 2 的三选一（受限 op / 远程禁用 / 禁止静默回落本机）针对数据面；静态资源本来就必须留在本机（spec §2.7 UI 由本机提供），把它登记成「远程禁用」会误导 T8 去砍语言包。若评审要求严格三分类，可把该行并入「远程禁用」并把语义注为「静态资源不随 host 切换」。
