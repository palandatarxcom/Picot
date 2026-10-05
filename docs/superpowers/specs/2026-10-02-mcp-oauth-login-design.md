# Picot MCP OAuth 登录设计（/mcp login 的 GUI 化）

**日期：** 2026-10-02
**状态：** 设计稿（待 Dr. Lin 拍板）
**前置：** 内嵌 Pi ≥ 0.99.2（当前 pin 0.99.2，1.0.0 兼容已评估）

## 2026-10-05 关联说明

登录/登出/状态的宿主 CLI 链路已在当前源码实现；本文早期状态与 pin 是历史记录，不作当前版本证明。项目 override 的页面与保存契约以 [MCP 设置页 Spec](implemented/2026-09-13-mcp-settings-page-design.md) 为准，新增能力仍待实施。它不改变 token store、owner-bound 登录操作、取消或缓存失效契约。

合法 override 的状态身份是有效全局 scope/source 加项目 override 路径，不是 project scope。项目 override detail 不放 OAuth 按钮；需要认证时回全局页操作。完整服务器既有 OAuth UI 保留。无 live report 不等于未信任，信任必须由工作区/配置准入判断。host `mcp_server_status` 可增加 `refresh:true`，在配置变更后复用 runner.invalidate() 再查询，默认仍维持 60 秒 TTL、页面不轮询。

当前 host MCP ops 要求 Registered workspace，landing 的全局配置桥可用不代表 MCP CLI 登录/status 已对 landing 放行。本次不扩大此权限；状态不可用时页面降级。Pi ≥1.0.1 是项目 override 的前置，实施前核验实际内嵌 binary，不以 pin 单独证明。

## 目标

MCP 设置页对齐 Models 页 codex OAuth 的体验：每台远程 MCP 服务器可**登录（浏览器授权码流）/登出/看连接状态**，令牌由 pi 写入 `~/.pi/agent/mcp-auth.json`，Picot 全程不接触密钥。

## pi 1.0.0 侧事实（代码级证据）

| 事实 | 位置 | 含义 |
| --- | --- | --- |
| `signInMcpServer({serverUrl, store, settings, challenge?, prompt})` 注入式交互 | `src/extensions/mcp/oauth.ts:403` | `prompt.showAuthorizationUrl(url)` 展示/打开授权页 + `promptForRedirectUrl(signal)` 粘贴回调后备；内置 localhost 回调服务器（自动选端口，优先复用已注册 redirect URI 的端口） |
| CLI `pi mcp login <server>` | `src/extensions/mcp/cli.ts:517` | 先连接探活（已登录直接退出 0，并给出工具数）→ signIn → 重连验证；`--timeout` 默认 300s |
| **非 TTY 时 `interactive=false`** | `cli.ts:537,563` | `process.stdin.isTTY !== true` → 跳过终端粘贴路径，仅浏览器回调完成——**为 GUI/管道驱动显式设计** |
| `options.openUrl` 可注入 | `cli.ts:536` | 进程内注入浏览器打开函数（CLI 未暴露 flag，但非 TTY 语义已够用） |
| `pi mcp logout <server>` | `cli.ts:216-238` | 删除该服务器在 mcp-auth.json 的凭据 |
| `pi mcp list --json` | `cli.ts:189-260` | 结构化 `ServerReport`：`name/scope/source/enabled/exposure/transport/state/tools/error`，`state ∈ {connected, needs-auth, disabled, error…}`——**页面状态源** |
| 凭据仓按 name+URL 双键 | `oauth.ts:131` `McpOAuthCredentialStore.forServer` | mcp-auth.json；会话下一轮次自动使用新凭据（docs/mcp.md） |
| RPC 面无 mcp 命令 | `docs/rpc-commands.md`（grep 无命中） | RPC 路不通 |
| SDK 主入口仅导出 `createMcpExtension` 工厂 | `src/index.ts:408` | **`signInMcpServer` 不在公开导出面**——扩展深导入属私有 API |

## 方案选型

- **选定 A：宿主侧 spawn 内嵌二进制 `pi mcp login|logout|list --json`**
  公开 CLI、非交互语义现成、凭据天然落 mcp-auth.json、运行中会话下轮次即生效。
- 否决 B（扩展内深导入 `extensions/mcp/oauth.js`）：私有 API，pi 升级即碎，违反 Picot「verified public surface only」纪律（参照 `pi-oauth-login-adapter.ts` 的既有原则）。
- 否决 C（RPC）：面不存在。

## 架构（四层，镜像 codex OAuth；差异：登录走独立子进程而非会话 runtime）

### 1. Rust 宿主：新 `src-tauri/src/mcp_login_runner.rs`（模式抄 `git_pi_runner.rs:61-79`）

login 子进程的父进程是 Rust host，**不依赖 pi 会话 runtime 存在**（与 codex 设备码走会话内 ModelRuntime 不同）；冷启动（无会话）时 MCP 面板不渲染本功能入口。

- `spawn_login(binary, cwd, server_name)`：`Command::new(<embedded pi>)` + `mcp login <name>`；`current_dir(workspace)`；**`stdin(Stdio::null())`**（→ 非 TTY → interactive=false）；stdout 逐行流式解析——命中 `Sign in to MCP server "X" in your browser:` 后随行即授权 URL，作为事件上行；退出码 0/1 → Succeeded/Failed；超时由 `--timeout` 承担（默认 300s）。
- `spawn_logout` / `spawn_list_json`：一次性 spawn（list 带 `--json`，stdout 整段 JSON）。
- 操作生命周期挂现有 `oauth_manager.rs` 的 `OAuthManager`（owner-bound + generation + 32 上限 + 过期清扫全部复用；`OAuthStatus` 语义不变）。

### 2. Host op 面（host_router/host_server 注册）

```
mcp_login_start {name}        → operation id（同一服务器同时仅一个活动 operation）
mcp_login_cancel {opId}       → kill 子进程 + Cancelled
mcp_login_status {opId}       → 轮询/事件
mcp_logout {name}             → 一次性
mcp_server_status {}          → list --json 结果（页面激活时调用，60s TTL 缓存，不做轮询）
```

**缓存失效**：`mcp_login` 成功与 `mcp_logout` 完成时必须主动清空 `mcp_server_status` 缓存，否则徽标最长达 60s 后才转 connected。

### 3. 扩展桥：不新增

MCP 登录不经 pi 会话进程（codex 设备码在会话内跑、MCP login 在独立子进程跑）——避免占用会话 runtime，也规避扩展进程内 spawn 同族二进制的递归风险。

### 4. WebView：`mcp-page.js` 增强 + 复用 oauth 对话框骨架（`models-oauth-login.js`）

- 每行状态徽标：`connected`（绿点+工具数）/ `needs-auth`（"登录"按钮）/ `error`（错误文案）/ `disabled`。数据 = `mcp_server_status` 按 name 合并 `mcp_list_servers`。
- 登录 → 弹进度对话框：授权 URL 展示 + 「在浏览器打开」（复用现有 `openExternal` 路径）+ 状态行 + 取消；**无设备码倒计时段**（授权码流无 user_code）。
- 有凭据的服务器（list 显示 connected 且 transport 为 http）提供「登出」。

## 边界与风险

1. **双开浏览器**：pi 子进程默认 `openBrowser` + WebView `openExternal` 可能各开一次。首版接受（WebView 的可点 URL 是后备）；若扰人，向上游提 `--no-open` flag，不私改。
2. **远程/SSH 场景不覆盖**：非 TTY 下粘贴路径被跳过（`cli.ts:563`），loopback 回调要求浏览器与本机同机——桌面 GUI 既定边界，文档注明。
3. **项目级服务器**：CLI 以 cwd 读信任项目的 `.pi/mcp.json`；未信任项目不出现在 list——与设置页现状一致。**项目级服务器的「登录」按钮同样须经 `project-trust.ts` 闸口**：未信任时禁用并提示，而非把 CLI 的未信任错误原文上抛（与 mcp_list_servers 直读文件的口径区分）。
4. **list --json 有真实连接开销**：只在页面激活 + 手动刷新触发，TTL 缓存。
5. **安全**：WebView 只见非密数据（URL、状态、工具数）；令牌仅存在于子进程与 mcp-auth.json；宿主只存 redacted 操作状态（`oauth_manager.rs` 既有语义）。
6. **取消清理（观察项，低优）**：外部 kill 子进程不触发 `cli.ts` 的 `try/finally { callback.close() }`，可能留下 pi 端 client 注册残影；`signInMcpServer` 在 redirect URI 不匹配时 `delete next.clientInformation` 自愈，实践中自修复，记录不阻断。

## 交付切分

- **P1（核心）**：runner + 5 个 host op + 状态徽标 + 登录/登出 + 对话框（URL/状态/取消）。
- **P2（可选）**：list 结果中的 exposure 覆盖、资源计数等进阶展示；非交互粘贴回调通道（等上游 CLI 支持）。

## 验证计划

- Rust 单测：runner 参数构造、URL 行解析、退出码→状态映射（fake binary，模式抄 git_pi_runner 测试）。
- 前端 vitest：mcp-page 徽标合并、登录对话框流（成功/失败/取消）、登出。
- 手动冒烟：Sentry 公开 MCP 服务器走完整浏览器流；确认 mcp-auth.json 生成且按 name+URL 键存；logout 后 needs-auth 复现；运行中会话下一轮次用上新令牌。
