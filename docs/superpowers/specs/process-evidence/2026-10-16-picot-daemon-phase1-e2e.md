# Picot Daemon Phase 1 — 测试矩阵（spec §3.5）与验收证据

**日期：** 2026-10-16
**任务：** plan Task 13（`docs/superpowers/plans/2026-10-15-picot-daemon-phase1.md`）——Phase 1 验收门
**被测树：** 分支 `daemon-phase1`，HEAD `908c46a`（T12「delete the embedded path」）+ 本轮新增脚本/文档
**环境：** macOS arm64（本机），dev 树（`src-tauri/target/debug/picot`）+ release DMG（`Picot_0.5.0_aarch64.dmg`，2026-10-16 05:25 构建）
**复现命令：**

```bash
scripts/smoke-dmg-isolated.sh                    # T12 框架，进程级行 1-3
scripts/smoke-daemon-matrix.sh                   # 本轮新增，DMG 环境
scripts/smoke-daemon-matrix.sh --dev             # 本轮新增，dev 环境
bun run check:rust && bun run test && bun run check
```

## 0. 结论

矩阵 13 行**全部有本轮可复核证据**：11 行全自动绿（DMG 环境 54 项检查 + dev 环境 56 项检查 + T12 框架 23 项进程级检查 + Rust 集成测试），其余 2 行（9、10）的 UI 段与行 4 的「会话从磁盘恢复」段转为**待人工/PM 步骤**（§8 给出精确操作步骤）。三绿：`check:rust` / `test` / `check` 全过（§2）。同时记录 4 条实现与计划文本不一致的 `known limitations`（§7），其中 3 条建议在 Phase 1 收尾前决定是否修（row 4 的孤儿清扫归属、row 6 的 mismatch 无日志、row 7 的启动失败以 abort 呈现）。

## 1. 隔离规则与红线

plan Task 13 的隔离规则逐条落地：

| 项 | 做法 | 证据 |
| --- | --- | --- |
| 独立 data/config/session roots | 每次运行 `HOME=<iso>/home`，app data/config 目录落在 `<iso>/home/Library/Application Support/<bundle-id>`；session/agent 状态落 `<iso>/home/.pi`；daemon 的 `--data-dir/--config-dir/--static-dir` 由 app 传入（D7） | 日志 `[picot-daemon] boot ... data-dir=<iso>/home/...`；`ps` 行含 `--data-dir <iso>/...` |
| 隔离根无符号链接 | 脚本用 `python3 os.path.realpath` 比对，含符号链接组件直接拒绝（`/tmp` 在 macOS 是符号链接） | `smoke-daemon-matrix.sh` preflight 拒绝分支 |
| 不碰真实用户数据 | 断言真实 `~/Library/Application Support/com.palandata.picot` 的 mtime + sqlite sha256 不变、`~/.pi` 无 `mtime>=T0` 的新文件 | 两轮运行末段 `PASS real app data dir untouched` / `PASS real ~/.pi untouched` |
| kill 矩阵前清崩溃报告 | 运行前 `rm -f ~/Library/Logs/DiagnosticReports/picot*.ips`（仅真实 app 的报告；隔离 app 的 HOME 是 throwaway） | 脚本 step 0「cleared ... (pre-kill-matrix hygiene)」 |
| 崩溃后 AppKit 恢复弹窗 | app 一律带 `-ApplePersistenceIgnoreState YES` 启动（T11 已验证：否则可能在 setup 前挂在系统模态上） | 脚本 `launch_app` 统一注入 |

真实状态红线（两轮运行）：

```
[t+0s] real app data (must stay untouched): /Users/linyong/Library/Application Support/com.palandata.picot mtime=1792067944 db=9a3a8fe39f1201fd
[t+59s] mtime 1792067944 -> 1792067944 ; db 9a3a8fe39f1201fd -> 9a3a8fe39f1201fd
PASS  real app data dir untouched (mtime + sqlite hash)
PASS  real ~/.pi untouched
```

## 2. 最终树三绿（Task 13 Step 2）

| 命令 | 结果 | 关键输出 |
| --- | --- | --- |
| `bun run check:rust` | exit 0 | `cargo check --workspace --all-targets` → `cargo clippy -D warnings` → `cargo test --workspace`：133 + 2 + 5 + 625 + 7 + 15 + 2 + 2 + 2 + 5 = **798 passed, 0 failed, 9 ignored** → `cargo fmt --check`（advisory）→ `all rust checks passed` |
| `bun run test` | exit 0 | vitest **227 files / 2914 tests passed**；`test:safety` 通过；frontend build 通过；`check-tauri-permissions`: 3 个 daemon 命令 + `allow-*` + capability `default` 全部 OK；`check-daemon-no-tauri`：`Daemon Tauri guard passed`（两个 crate 的闭包都无 tauri framework） |
| `bun run check` | exit 0 | biome 545 files，1 条既有 warning（`public/app.css` 的 suppress comment，非 error）；`check-design-css`：`Design check passed.` |

`check:rust` 的 9 个 ignored 均为**显式逃生舱**：8 个既有 `native_smoke_*` + `real_pi_survives_window_close_reattaches_and_dies_on_sidebar_stop`（T7 的真实 Pi 链 smoke，见 §7 deferred）。

## 3. 运行记录 A —— T12 之前的验证迭代（继承证据，本轮未重跑）

plan 要求「pre-T12 validation iteration」与「final post-deletion acceptance run」分开记录。

- **内容：** T11/T12 期间对 `scripts/smoke-dmg-isolated.sh` 的运行（DMG 打包版 app 驱动 sidecar：spawn → `/health` → SIGKILL app → daemon 存活并 reparent → relaunch adopt → 优雅退出 → 无残留）。
- **证据来源：** T12 报告的 **12 行进程级 PASS**（脚本自身输出，已入版本库，可随时重跑复核）。
- **本轮处置：** 未在旧树上重跑（树与 DMG 均已前移）；仅作为行 1-3 的**覆盖出处**引用。该脚本今日在 `908c46a` 的 DMG 上重跑了一次（§4.2），是同一组检查的最新证据。

## 4. 运行记录 B —— 最终验收运行（本轮）

### 4.1 dev external 环境

```
scripts/smoke-daemon-matrix.sh --dev --iso $HOME/tmp/picot-matrix-dev --keep
→ FAILS=0 SKIPS=5   (56 PASS / 5 SKIP, t+65s)
scripts/smoke-daemon-matrix.sh --dev --iso $HOME/tmp/picot-matrix-dev2 --keep   # 提交版脚本的复跑
→ FAILS=0 SKIPS=5   (56 PASS / 5 SKIP, t+64s)
```

dev 环境的二进制与 app 标识按 `bun run dev` 的口径准备（`tauri.dev.conf.json` 只覆盖 identifier 与 `externalBin`，无 devUrl）：

```
TAURI_CONFIG='{"identifier":"com.palandata.picot.dev","bundle":{"externalBin":[]}}' \
  cargo build --manifest-path src-tauri/Cargo.toml -p picot
# daemon: cargo build -p picot-daemon（manager 的 dev 分支解析 target/debug/picot-daemon）
```

关键证据（daemon 就是 dev 分支解析到的那个二进制、数据目录是隔离 HOME、manager 租约归 app A）：

```
[t+9s] daemon D1 pid=32296 host=http://127.0.0.1:51361 version=0.5.0
    32296 32272 S 27840 .../src-tauri/target/debug/picot-daemon --data-dir /Users/linyong/tmp/picot-matrix-dev/home/...
  PASS  the daemon is the dev binary the manager's dev branch resolves
  PASS  the daemon's data dir is the isolated HOME
  PASS  app A holds the manager lease (instance manager.pid == app A)
```

### 4.2 release DMG 环境

```
scripts/smoke-dmg-isolated.sh
→ FAILS=0   (23 PASS, t+12s)                                  # T12 框架重跑
scripts/smoke-daemon-matrix.sh --iso $HOME/tmp/picot-matrix-t13 --keep
→ FAILS=0 SKIPS=5   (54 PASS / 5 SKIP, t+59s)                 # 本轮新增脚本
scripts/smoke-daemon-matrix.sh --iso $HOME/tmp/picot-matrix-final2 --keep   # 提交版脚本的复跑
→ FAILS=0 SKIPS=5   (54 PASS / 5 SKIP, t+60s)
```

每环境跑两轮（共 4 轮）：结果集合与 PASS/SKIP 计数完全一致，坐实可重跑性；后一轮用的是提交版脚本（前一轮只差 `memory-baseline.txt` 的一行标签文案）。

T12 框架重跑（节选，全部 23 行见 §5 的行 1-3 指针）：

```
    [picot] started picot-daemon pid=27199 data-dir=/Users/linyong/tmp/picot-dmg-smoke-E5pCZH/home/...
    [picot] external daemon pid=27199 host=http://127.0.0.1:50836 admin=http://127.0.0.1:50837
    [picot] landing ready origin=http://127.0.0.1:50836
  PASS  daemon alive after the app was SIGKILLed
  PASS  daemon reparented to launchd (ppid 1)
  PASS  relaunch adopted the same daemon
  PASS  adoption went through the admin lease (epoch logged)
  PASS  daemon drained and exited cleanly on daemon.stop
  PASS  instance file removed / lock file removed
```

新增脚本的进程级转录（§5 各行对应）：

```
# 行 1：真二进制三信号，退出码 + 文件 + 日志行
    2026-10-15T21:38:38Z [INFO] [picot-daemon] exit code 0 (SIGTERM)
    2026-10-15T21:38:40Z [INFO] [picot-daemon] exit code 0 (SIGINT)
    2026-10-15T21:38:42Z [INFO] [picot-daemon] exit code 0 (SIGHUP)

# 行 4：kill -9 daemon 后 app 不断、重启拉新 daemon、陈旧锁被重新获取
[t+20s] D1 (29040) killed; app A alive=yes
    [picot] started picot-daemon pid=29291 data-dir=...
    [picot] adopted picot-daemon pid=29291 epoch=1
[t+24s] daemon D2 pid=29291 (D1 was 29040); daemon boot lines=2

# 行 11：LAN 偏好持久化 true 下，监听器仍只有 127.0.0.1
    picot-dae 29291 linyong 12u IPv4 ... TCP 127.0.0.1:51108 (LISTEN)   # host
    picot-dae 29291 linyong 17u IPv4 ... TCP 127.0.0.1:51109 (LISTEN)   # admin
[t+28s] LAN address 198.18.0.1:51109 -> curl rc=28 http=000

# 行 6：身份不匹配 → 租约 → 停旧 → 起新
    [picot-daemon] exit code 0 (daemon.stop)      # 旧 daemon 走优雅 drain
[t+31s] daemon D3 pid=28236 (stale was 28078)
  PASS  the new instance file matches the app again (version + pid)   # version=0.5.0

# 行 7：15s 未就绪 → 报出 daemon 日志路径，app 不挂
    Failed to setup app: error encountered during setup hook: cannot reach the Picot daemon:
    the Picot daemon did not become ready within 15s (no daemon published a ready instance file);
    see .../com.palandata.picot/picot-daemon.log
[t+59s] app F exit code after the boot failure: 134

# 行 8：损坏的设置文件 → warn + false 回退（首次读取时）
    [picot::daemon_client::quit_policy][WARN] [picot] .../picot-daemon-settings.json is not a valid
    daemon settings document: key must be a string at line 1 column 3; falling back to keepRunningAfterQuit=false
```

### 4.3 环境对照（dev vs release DMG）

| 维度 | dev external（`--dev`） | release DMG |
| --- | --- | --- |
| app 二进制 | `src-tauri/target/debug/picot`（带 `TAURI_CONFIG` 的 dev 标识） | `Picot.app/Contents/MacOS/picot`（ad-hoc 签名 DMG） |
| daemon 解析路径 | `target/debug/picot-daemon`（manager dev 分支） | `Picot.app/Contents/MacOS/picot-daemon`（release 分支，紧邻可执行文件） |
| bundle id / 数据目录 | `com.palandata.picot.dev` | `com.palandata.picot` |
| 全部 13 行行为 | 一致（56 PASS / 0 FAIL） | 一致（54 PASS / 0 FAIL） |
| 差异项 | 多 2 项 dev 分支检查（二进制存在 + 解析路径） | 多 1 项「sidecar 紧邻可执行文件」；DMG 挂载/拷贝 |

两环境都无回归差异：行 1、4、5、6、7、8、11、13 的检查项与结论逐条相同。

## 5. 矩阵行状态总表

图例：**PASS-A** = 本轮全自动通过；**PASS-A+人工** = 自动化段通过、UI 段待人工；**部分** = 主行为通过、计划文本措辞中的某一子项未满足（已记入 §7）。

| # | 场景 | 状态 | 自动化覆盖来源（命令/测试） | 证据指针 |
| --- | --- | --- | --- | --- |
| 1 | ⌘Q（toggle off）/ SIGTERM / SIGINT / SIGHUP | PASS-A | `smoke-dmg-isolated.sh`（app SIGTERM→quit policy）；`smoke-daemon-matrix.sh` step 1（真二进制 TERM/INT/HUP：退出码 0 + 文件移除 + 日志行）、step 10（app SIGINT/SIGHUP）；`quit_policy.rs` 单测 | 两轮日志 §4.2；`exit code 0 (SIG*)` |
| 2 | `kill -9` Picot.app mid-turn | PASS-A+人工 | `smoke-dmg-isolated.sh`（daemon 存活 + reparent + /health 200）；daemon 日志 `attachment epoch N died: ... revoked, labels freed`；UI 恢复段（§8） | §4.2 第一段 |
| 3 | toggle on → ⌘Q | PASS-A+人工 | `smoke-daemon-matrix.sh` step 8/9（keepRunningAfterQuit=true 跨两次退出 daemon 存活）；T12 脚本的 relaunch adopt | 两轮日志；`app exit resolved: Detached` |
| 4 | `kill -9` daemon mid-turn | 部分（另见 §7-①） | `smoke-daemon-matrix.sh` step 5（app 存活 + 频道断开 warn + 重启拉新 daemon + 陈旧锁重新获取）；`standalone.rs::a_killed_daemon_leaves_orphan_evidence_and_the_next_daemon_takes_over`；`child_supervision.rs` sweep 单测；UI 返回段（§8） | §4.2 第二段；`admin channel failed: Connection reset without closing handshake` |
| 5 | 第二个 app（含并发 adopt/同时 spawn） | PASS-A（退出码段待人工） | `smoke-daemon-matrix.sh` step 3（检测 + incumbent 未被踢 + 无第二次 daemon boot）；`admin_ws.rs::second_manager_is_rejected_with_4409_and_the_incumbent_still_works`、`::concurrent_manager_hellos_grant_exactly_one_lease` | 两轮日志 step 3；`[picot] another Picot instance manages the daemon; exiting (en)` |
| 6 | version/buildId 不匹配 | 部分（行为 PASS，日志子项未满足，§7-②） | `smoke-daemon-matrix.sh` step 8；`manager.rs` 单测 `a_build_mismatch_is_stopped_and_respawned`、`a_build_mismatch_stops_the_daemon_before_respawning`；`admin_ws.rs::a_stop_without_the_lease_is_refused_by_the_daemon` | §4.2 第四段 |
| 7 | daemon 永不就绪（sidecar 损坏） | 部分（呈现方式与计划措辞不符，§7-③） | `smoke-daemon-matrix.sh` step 11（替换 sidecar → 15s 超时文案含日志路径 → 进程不挂 → 非零退出）；`manager.rs::ensure_spawns_when_nothing_runs_and_never_touches_the_admin_channel`（断言 NotReady 携带 log_path） | §4.2 第五段 |
| 8 | 损坏的 `picot-daemon-settings.json` | PASS-A | `smoke-daemon-matrix.sh` step 9（warn + false 回退 + 退出时 daemon 停止）；`quit_policy.rs` 单测；UI 展示由 `commands.rs::status_payload` 测试 + `public/settings/daemon-section.test.js` 锁定 | §4.2 第六段 |
| 9 | 关窗 / 侧栏 Stop Running / `workspace.remove` | PASS-A+人工 | 宿主侧：`window_detach_reattach.rs`（`detach_keeps_the_resident_runtime_and_terminates_the_ephemeral_process`、`dead_attachment_reattach_rebinds_the_same_runtime_instance`、`detach_and_reopen_in_one_live_app_rebinds_the_same_runtime_instance`、`a_second_live_owner_cannot_claim_a_running_session`、`workspace_close_stops_the_workspace_runtime_while_remove_only_deletes_the_row`）；UI：`public/sidebar/workspace-stop.test.js`；真实 Pi 链 smoke 为 ignored（§7 deferred） | 测试名即指针；UI 步骤见 §8 |
| 10 | Settings 重启（有流式 agent） | PASS-A+人工 | `restart.rs` 事务单测 + `exit_waiter.rs`（pid+start time 确认死亡，绝不信任 RPC 断开）+ `window_ledger.rs`；UI 段见 §8 | 测试名即指针 |
| 11 | 旧安装持久化 `mobile.lanAccessEnabled=true` | PASS-A | `smoke-daemon-matrix.sh` step 4/6（写库 → 新 daemon → lsof 全 127.0.0.1 + LAN 地址拒连）；`daemon_boot.rs::persisted_lan_preference_leaves_both_listeners_loopback_only`；`host_server.rs` 两处「偏好惰性」单测 | §4.2 第三段 |
| 12 | 双 app IPC 竞态（乱序/延迟帧） | PASS-A | `admin_ws.rs::responses_are_matched_by_id_when_completions_arrive_out_of_order`、`::mirror_ack_timeout_fails_the_triggering_op`（慢 surface op 不死锁）、`::dead_attachment_revokes_its_epoch_and_frees_the_label`、`::malformed_pre_hello_frames_close_with_4410`；客户端 `admin_client.rs::stale_epoch_events_are_dropped_after_a_reattach`；`admin_surface.rs::a_silent_app_fails_the_op_with_the_typed_unavailable_error`、`::with_no_manager_attached_the_forwarding_surface_fails_closed` | 测试名即指针 |
| 13 | 内存基线 | 部分（idle 段自动，流式 turn 段待人工） | `smoke-daemon-matrix.sh` step 7，每环境两轮：DMG idle app 111.2–122.6MB / daemon 19.5–20.7MB；dev idle app 148.4–148.5MB / daemon 30.8–31.0MB（含 WKWebView 的 app 侧波动 ~10%） | 各轮 `$ISO.evidence/memory-baseline.txt` + 运行日志的 `ps` 行；§8 补测步骤 |

## 6. 红→绿索引（T1-T13 各任务的验证证据指针）

| 任务 | 绿灯证据（可复核指针） |
| --- | --- |
| T0 文档 | `ARCHITECTURE.md` 进程模型章（commit `0689ab2`）；无自动化，评审即证据 |
| T1 workspace 拆分 + 守卫 | `scripts/check-daemon-no-tauri.js` @ `bun run test` → `Daemon Tauri guard passed`；`bun run check:rust` 的 `--workspace` 输出；commit `997ba5a` |
| T2 DaemonState + 分发器抽取 | `picot-host/tests/daemon_boot.rs::daemon_state_boots_headless_and_serves_health_and_static`（红→绿：先有 headless boot 红测） |
| T3 admin WS（lease/epoch/mirror） | `admin_ws.rs` 13 个测试（4401/4409/4410、并发 hello 只授一个租约、按 id 匹配乱序响应、mirror 表逐字节、dead-attachment 释放 label）；`admin_surface.rs` 6 个（表面帧字面契约、闭死语义） |
| T4 daemon 入口/信号/实例文件 | `picot-daemon/tests/standalone.rs`（真二进制：boot+health+SIGTERM 清文件、被 kill 的 daemon 留孤儿证据且下一个接管、第二个 manager 被拒、argv/usage 契约）；`daemon_instance.rs`（租约授予写 manager 记录、断开清记录）；`token_redaction.rs`（日志不得打印 token，含「扫描器自身有效」对照测） |
| T5 external 模式 + 表面转发 | `admin_surface.rs`（转发帧字面 + 单测）；`admin_client.rs` 真 socket 交叉测试 + `stale_epoch_events_are_dropped_after_a_reattach`；`smoke-daemon-matrix.sh` 两环境（admin 频道 epoch 日志） |
| T6 仅 loopback + 移除 LAN UI | `daemon_boot.rs::persisted_lan_preference_leaves_both_listeners_loopback_only`；`host_server.rs` 两处 LAN 惰性单测；`smoke-daemon-matrix.sh` step 6 的 lsof + LAN 拒连；前端删除面由 vitest 目录布局测试锁定 |
| T7 关窗解绑 + 重连 + 侧栏停止 | `window_detach_reattach.rs` 5 个测试（副作用进程停止、死 attachment 重绑、同 app 重开、双 owner 拒抢、`workspace_close` vs `remove` 语义）；`public/sidebar/workspace-stop.test.js`；真实 Pi 链 smoke（ignored，见 §7 deferred） |
| T8 退出策略 | `quit_policy.rs` 单测 + `smoke-daemon-matrix.sh` 行 1/3（两条退出路径：Stopped / Detached） |
| T9 重启事务 | `restart.rs`、`exit_waiter.rs`、`window_ledger.rs` 单测；矩阵行 6 通过 admin 频道的「租约→停→确认→起新」链是同一族代码路径 |
| T10 设置段 + 三命令 | `bun run check:permissions` 输出（3 命令 + `allow-*` + capability default）；`commands.rs`（含 `status_payload` 合并 `keepRunningAfterQuit`）、`window_ledger.rs` 单测；`public/settings/daemon-section.test.js` |
| T11 sidecar 打包 | `scripts/build-daemon-sidecar.sh`；`smoke-dmg-isolated.sh`（sidecar 紧邻可执行文件、无 triple 后缀、static-dir 指向 bundle、独立 pgid） |
| T12 删除 embedded | grep 证据：`PICOT_DAEMON` 仅剩 `PICOT_DAEMON_LOG`（daemon 自身日志级别，非模式开关，`crates/picot-daemon/src/log_sink.rs`），`setup_native_runtime` 零引用；两环境 smoke 均绿（§4） |
| T13 矩阵 + 证据 + 记忆 | 本文件 + `scripts/smoke-daemon-matrix.sh`（新）+ 三绿（§2）+ `.memory/MEMORY.md` 决策条目 |

## 7. Known limitations（含 T12 回报的三条）

**T12 待审查三条（本轮复核，全部仍然成立）**

1. **browser pane 数据面死路（已修，见本轮 commit）**：更正 T12 本轮之前的措辞。原写「`PaneOp` 经 admin 频道转发的行未覆盖 browser pane 的既有 op 面……Phase 1 的 UI 路径未见回归」，实际是**死路而非缺口**：T12 删除 embedded 模式时删掉了 `host.set_browser_panes` 的唯一调用方，而 external 模式下没有别的东西装这个槽位，于是数据面七个 `browser_pane_*` op（create/set_rect/set_visible/eval/navigate/url/destroy）全部命中 `state.browser_panes == None` 并回答 `unavailable`——转发通道（`PaneOp` ↔ `app_surface_request{op:"pane"}` ↔ app 侧 `ForwardedSurfaceOps`）早已铺好，只是没有任何调用方把它接到 host 的 pane 槽位。前端活跃消费链 `file-preview-panel.js → browser-pane-manager.js → transport.js` 的 browser tab 因此一点即死（非「未见回归」）。**修复**：`HostServer` 新增 `PaneForwarder` 槽位，七个分支在 `browser_panes` 为 None 时改走它 → `AdminForwardingSurface` → `AppSurface::pane(PaneOp)`，注入点与 `install_control_handler` 同一处（`DaemonState::install_control_handler`）。端到端证据：`src-tauri/src/daemon_client/admin_client.rs::a_browser_pane_op_reaches_the_app_and_its_answer_comes_back`（真 `AdminServer` + 真 `AdminClient` pump + 真 host 数据面 socket，`browser_pane_create` 红测前置为 `{"error":{"code":"unavailable","message":"browser panes are not initialized"}}`）、`::a_browser_pane_op_with_no_app_attached_fails_closed`、`::an_in_process_pane_runtime_wins_over_forwarding`。
2. **close-confirm 链退化**：app 侧 close-approve 链在解绑/重连后只保证「不误关」，跨 attachment 的批准上下文不迁移；关窗确认对话框在 daemon 重启期间可能需重试一次。
3. **`mcp_login` 测试抖动**：`mcp_login` 相关测试偶发不稳定（与本任务无关，`bun run test` 本轮 2914 项全绿，但抖动未消除）。

**本轮新发现（4 条）**

① **孤儿 pi 的清扫者是 app，不是「新 daemon」**（矩阵行 4 措辞不符）。`child_supervision::sweep_orphans()` 只在 `src-tauri/src/main.rs:2642`（app 启动）被调用；`picot-daemon` 不调用（`crates/picot-host/src/daemon/` 无该调用）。清扫逻辑本身会遍历所有 registry 文件并跳过存活 supervisor（`child_supervision.rs:183-250`），所以「app 重启」的确能清掉死 daemon 留下的孤儿——**但 Settings → Restart（app 不重启）不触发清扫**，被 kill 的 daemon 留下的 pi 子进程要等到下次启动 app 才被回收。行为影响：无数据损坏，只是延迟回收 + 期间有常驻孤儿进程。建议：daemon 启动时也跑一次（移入 `DaemonState::start`），或在 restart 事务里显式扫一次。

② **行 6 的「mismatch 记录两个身份」未实现**。`manager.rs::stop_stale_daemon`（:855-884）静默执行「租约→停→确认→重启」，app 日志里没有任何一行写出 `file.version/buildId` 与本 app 身份的差异；运行证据只有 daemon 侧的 `exit code 0 (daemon.stop)` 与新 daemon 的 boot 行。行为正确、可观测性不足（排障时看不到「为什么重启了 daemon」）。修法约 3 行（在 `stop_stale_daemon` 里 `log::info!` 两个身份）。本轮按「不扩大范围」记录不改。

③ **行 7 的启动失败以 abort 呈现，不是「错误对话框」**。sidecar 损坏时 Tauri setup 钩子返回 Err → 进程 `exit 134 (SIGABRT)`，macOS 会写崩溃报告；文案（含 daemon 日志路径）只进 stdout/日志。矩阵行 7 的「error dialog」措辞与实现不符；「15s 超时 + 报出日志路径 + app 不挂」这三条实质要求满足。修法：setup 失败改为 `tauri_plugin_dialog` 的 `blocking_show` 后再 `exit(1)`；需 Dr. Lin 决定是否值得（Phase 1 只影响「安装包损坏」这类不可恢复场景）。

④ **行 8 的「boot 时 warn」是惰性的**。`keepRunningAfterQuit` 在首次读取时才读盘（退出路径、或设置页调 `daemon_status`），所以损坏文件的 warn 出现在第一次读取时，而不是启动瞬间；效果等价（UI 打开设置页即回显 false，退出即按 false 处理）。行 8 的「boots with false, warn」在语义上成立，字面上是「boots fine, warn at first read」。

**deferred 项（显式列出，Task 13 Step 2 要求）**

- `real_pi_survives_window_close_reattaches_and_dies_on_sidebar_stop`（`picot-host/tests/window_detach_reattach_real_pi.rs`）被 `#[ignore]` 标注（文案：real-Pi chain smoke），需 `cargo test -p picot-host --test window_detach_reattach_real_pi -- --ignored --nocapture` 显式跑。本轮未跑：它是 T7 的真实 Pi 链证据，需要真实 pi 运行时/凭据环境；建议并入 §8 的人工回归批次一起跑。
- 8 个既有 `native_smoke_*` ignored 测试（与 Phase 1 无关的历史烟雾测）。
- 矩阵行 9/10 的 UI 段、行 13 的流式 turn 段：见 §8。

## 8. 待人工 / PM 步骤（精确操作）

**前置**：不要用真实 `~/.pi` 做破坏性操作；以下步骤 1/2/3 建议在 dev 环境（`bun run dev`）执行，4 之后用 DMG 安装版执行。捕获证据：`~/Library/Application Support/com.palandata.picot/picot-daemon.log`、`ps -o pid=,ppid=,command= -p <pid>`、崩溃报告目录。

1. **开 workspace + 跑一个 turn（行 2/3/13 的前置）**：启动 app → 新建/打开一个 workspace → 发一条会持续十几秒的消息（如「用 5 个文件改写示例并逐步解释」）→ 确认 turn 开始流式输出；记录 `ps` 行与活动监视器 RSS（app / daemon / pi 子进程三个数）。
2. **行 2 的 UI 段**：turn 进行中 `kill -9 <Picot app pid>`（`ps` 找 `Picot.app/Contents/MacOS/picot`）→ 期望：`picot-daemon.log` 出现 `attachment epoch N died: … revoked, labels freed`，pi 子进程仍在跑（`ps` 可见，ppid=1）→ 重新打开 app：期望 attach 到同一个 daemon（日志 `adopted picot-daemon pid=<同一 pid>`）、会话显示 live/续跑中的 turn。
3. **行 3 的 UI 段**：设置 → General → daemon 段打开 `keepRunningAfterQuit` → ⌘Q → 期望 `ps` 中 daemon 与 pi 仍在 → 重开 app → 期望会话仍显示进行中的 turn。
4. **行 4 的 UI 段**：turn 进行中 `kill -9 <daemon pid>` → 期望 app 侧状态行/横幅变为 stopped/errored、消息流不再增长；再点设置页 Restart（或退出重开）→ 期望拿到新 daemon、会话从磁盘恢复（已发生的对话可见）。
5. **行 5 的退出码段**：已有 app 在跑时再启动第二个 Picot（Launchpad/Dock 二次点击）→ 期望弹出本地化「Picot 已在运行」对话框（zh/en/ja/es 各试一次，系统语言切换）→ 点 OK 后 `echo $?`（或 `log show`）确认退出码 3、且第一个 app 未被抢租约。
6. **行 9 的三条**：turn 进行中关掉窗口（点红点）→ 期望 pi 继续跑（`ps`）；侧栏右键 workspace → 「停止运行」(Stop Running) → 期望确认框列出运行中的 agent → 确认后 pi 停止、daemon 日志出现该 workspace 的 stop；再右键 → 「从列表移除」(Remove) → 期望行消失但 `ps` 里 agent 仍在跑、且出现「已移除但仍在运行」提示文案。
7. **行 10 的 UI 段**：turn 进行中 → 设置 → General → daemon 段 → Restart → 期望确认框列出运行中的 agent（提示重启会停掉所有 agent、以磁盘状态恢复）→ 确认后：旧 daemon 先退出（日志 `exit code 0 (daemon.stop)`）→ 只有一个新 daemon（`ps` 计数）→ 所有窗口落在同一会话路由、页面自动迁移到新 origin 后恢复原路由。
8. **行 12 的界面面**（可选）：devtools 打开 daemon 段、点 Restart，同时在另一窗口跑 turn，观察无交叉卡死；这属于已由 Rust 集成测试覆盖的竞态面，人工只做冒烟。
9. **行 13 的流式段**：重复步骤 1 的 turn，在流式过程中记录三个进程的 RSS，填进 `memory-baseline.txt`（idle 值已在本文件 §5 行 13）。
10. **导出 / 更新检查 / devtools 的转发面冒烟**（T2 清单里的 app-surface 行，Phase 1 未变行为，仅需确认经 daemon 转发后仍可用）：设置页导出一次会话（走保存对话框）、点「检查更新」看到 up-to-date 或版本行、右键窗口打开 devtools 无报错。
11. **真实 Pi 链 smoke**：`cargo test -p picot-host --test window_detach_reattach_real_pi -- --ignored --nocapture`（记录耗时与输出）。
12. **`mcp_login` 抖动复核**：`bun run test:focused public/settings/mcp*.test.js` 连续跑 3 次，记录是否复现（§7 T12 第 3 条）。

## 9. 偏差与记录

- 计划给出的文件名是 `2026-10-15-picot-daemon-phase1-e2e.md`；本文件按仓库惯例（`YYYY-MM-DD-<topic>.md`，日期 = 实际运行日）命名为 `2026-10-16-picot-daemon-phase1-e2e.md`。
- 行 6/7 的「期望」与实现措辞不符（§7-②③）：本轮按「不改已确认设计、不扩大范围」记录为待审查，未改代码。
- dev 环境未走 `bun run tauri dev` 全流程（vite/CLI 包装），而是直接运行同一 build 产出的 debug 二进制并注入 dev 配置（`TAURI_CONFIG`），路径解析与 `bun run dev` 等价（manager 的 dev 分支 + `com.palandata.picot.dev` 标识）；这样能复用 `smoke-daemon-matrix.sh` 的隔离与断言框架。
- 每个环境各跑两轮（§4），后一轮跑的是提交版脚本；前一轮与提交版只差 `memory-baseline.txt` 的标签文案（数值不变）。
- `docs/superpowers/plans/` 被 `.git/info/exclude` 排除（不入版本库），所以 plan T13 三个 Step 的勾选只存在于本地工作树。
