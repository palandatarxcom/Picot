# Landing Page Design

## Status

v3.1, 2026-09-15. All decisions approved by Dr. Lin in a grilling session
(2026-09-14, Q1–Q10). Visual prototype approved by Dr. Lin on 2026-09-14:

- `2026-09-11-landing-page-prototype.html` (night, default)
- `2026-09-11-landing-page-prototype-light.html` (clean)

The prototype files are the authoritative visual spec; the implementation
must match them (they use the real `style-theme.css` night/clean tokens).

**v3 revision** retains v2's closed findings and addresses the second
review's four blockers: Landing's first transition is binding-based rather
than cwd-based; workspace authority requires a Registered owner; landing
uses a pre-`app.js` bootstrap split; and Focus has an explicit landing
transition seam.

**v2 revision** addressed Dr. Lin's design review of the same day
(4 blocking findings + 5 pre-implementation clarifications, all verified
against code at main.rs:3072/3228, window_owner.rs:111/340-360/239-262,
app.js:4650/4712, sidebar-workspace-group.js:188). Key changes from v1:

- Menu state, Cmd+N dispatch, and window-destroy cleanup are re-keyed from
  window-label prefix to **owner workspace binding**.
- The landing owner gets a concrete state model: `TemporaryKind::Landing`;
  its first workspace entry is always a cross-workspace transition.
- Landing gets a dedicated lightweight transition handler; the existing
  chat-coupled sidebar handlers are not reused in landing mode.
- Landing is a distinct application bootstrap, not workspace chrome hidden
  after `app.js` has already initialised it.
- Zero-session workspaces are entered via their existing `+ New Chat`
  button; the hint copy says so (title-row click-to-enter was rejected).
- `landing.addProject` is dropped; the button reuses `sidebar.addProject`.

## Scope

Replace the unconditional cold-start workspace with a **landing page**:
after launching, Picot shows the landing view instead of opening
`~/.pi/tmp` with a fresh session. The landing view fills everything to the
right of the left sidebar; the sidebar stays fully functional.

- **In**: Rust cold-start path, native File menu (New Session grey-out +
  new Add a Project item), owner-registry landing owner, window-destroy
  cleanup re-keying, frontend landing mode at `/`, i18n, tests,
  ARCHITECTURE.md cold-start invariant update.
- **Out of scope**: shortcut-hint block on landing (New Session is greyed,
  Add a Project has no accelerator — nothing to list); a second
  "Create workspace" button (Picot only registers existing folders);
  remembering/reopening the last active workspace; a "home" way back to
  landing after a workspace transition (close the window instead);
  title-row click-to-enter on workspace headers; mobile/pair pages;
  non-native (browser) behaviour at `/` (unchanged — see "Non-native").

## Decisions

| # | Decision point | Decision |
| --- | --- | --- |
| 1 | Cold-start scope | **Unconditional landing.** No `ensure_default_workspace`, no pre-spawned Pi runtime, no auto-created session. `~/.pi/tmp` loses its special status: it appears in the sidebar only if registered. |
| 2 | Window behaviour on workspace selection | **In-place transition in the same window** via the existing `prepareWorkspaceTarget` → `commitWorkspaceTransition` → `navigateInWindow` path (swap overlay included). No new window machinery. |
| 3 | Landing layout | **Landing covers the entire area right of the left sidebar.** Right panels, terminal, header toolbar, composer do not render at all. |
| 4 | Quick Chat | **Remains available** on landing (sidebar button; independent ephemeral runtime; overlays the landing view). Side Chat disappears naturally with the header. |
| 5 | Cmd+N / New Session menu | **Menu item greyed out** while no workspace is bound to the focused window's owner; enabled when one is. State and dispatch keyed on owner binding, never on label prefix. |
| 6 | New File menu item | **Add a Project** (no accelerator), same picker/register chain as sidebar `+` and the landing button. Available no matter which window kind is focused. |
| 7 | Shortcut-hint block | **Not built.** The hint copy covers both landing operations. |
| 8 | Visual process | HTML prototype first — done, approved. |
| 9 | Delivery | Prototype → this spec → implementation. |
| 10 | Zero-session workspaces (v2) | **Copy change, not new interaction.** Zero-session workspaces are entered via their existing `+ New Chat` button; the hint copy names both paths. Workspace-title rows keep their current expand/collapse behaviour. |
| 11 | Add a Project copy (v2) | Reuse `sidebar.addProject` (all four locales already translated). `landing.addProject` is not created. |

## Implementation deviations (recorded 2026-09-15, post-implementation review)

Found in the critical review of the working-tree implementation (after the
v3 revision below). All are accepted deviations; each supersedes the
corresponding v2 text where noted.

1. **Entry architecture** — v2 said "app.js branches into landing mode".
   Actual: `bootstrap-entry.js` discriminates the route BEFORE any app
   module loads: native `/` boots `landing.js` directly and **app.js never
   loads on the landing page** (app.js constructs the chat object graph at
   import time and cannot run without a workspace session). landing.js
   constructs its own transport + sidebar + Quick Chat + transition
   controller. The v2 "Frontend: landing mode" section's branching model is
   superseded by this entry split; everything else there (seams,
   enterWorkspace contract, forbidden objects) holds.
2. **Landing Settings = visible tabs are exactly the functional ones**
   (v2 was silent; settled 2026-09-15 after three hands-on review rounds).
   Visible and functional at landing: **General, Appearance, Usage, Skills,
   Extensions**. The Pi-bound tabs (models, mcp, configuration) are
   **hidden entirely** — everything visible works, nothing visible is dead.
   Supporting changes: the agent controls (auto-compaction, thinking
   default, show-thinking) moved from General into the Configuration page
   (they ride the config bridge; show-thinking is already cookie/DB
   dual-tracked — `reconcileAgentPreferences`); the `cost_dashboard` data op
   is admitted for any authenticated desktop owner (global session scan,
   same rationale as `workspace_sessions`; frames may carry no workspaceId
   at landing); discovered-skills inventory switched from the config bridge
   to host control ops (single path, works at landing) while the
   package-skills sub-tab stays bridge-bound (host port drifted: no
   project-delta semantics) and is hidden at landing; the three package
   ops (list/check/set-disabled) accept a landing owner with global-only
   locations (`locations_for_workspace(None)`); project package scope
   errors with "requires an open workspace" at landing. Evolution:
   General-only → +Appearance → all tabs with notices → functional-only
   (general/appearance/usage) → +skills/extensions (final).
3. **Host model cache deleted** (v2 was silent): `ModelCache`, the
   `get_cached_models` control op, `models_from_runtime_reply`, and all
   frontend call sites (`transport.getCachedModels`, `fetchModelInfo`
   cache-first block, ephemeral view cache block) are removed. Consequence:
   model dropdowns (workspace, Side/Quick Chat) live-query their runtime —
   no instant cold render. Accepted trade-off of "no runtime at startup".
4. **Justified additions** (v2 silent, all reviewed): macOS drag strip on
   the landing area (no chat header otherwise owns window dragging);
   Quick Chat dialog roots reparent to `<body>` at landing; same-cwd
   generation retention now requires a Registered binding (placeholder-home
   edge, unit-tested). *Retracted 2026-09-16 after Dr. Lin's hands-on test:*
   "Focus as a fourth enterWorkspace seam" — every registry row showed the
   focus `>` at landing, but Focus-mode gating is "a workspace session is
   selected" and landing selects none. The seam (`canFocusWorkspace` /
   `onWorkspaceFocus` wiring and the focusWorkspaceId transition option)
   is removed from landing.js; the classic sidebar predicate (active
   session or current workspace) never passes at landing, so no `>`
   button renders there.
5. **Review round fixes**: `getLanguagePreference` import bug (landing
   Settings language selector threw ReferenceError; found in review, fixed
   by Dr. Lin's other agent and verified); the `dispatch` control gate was
   re-keyed to Desktop-kind + owner presence with per-op authority — the
   root cause of the "settings/main window/add project all broken"
   outage Dr. Lin reported.

Single centered column, vertical position slightly above center
(`padding-bottom: 12vh`):

Column order (Dr. Lin, 2026-09-17): logo → tagline → "Picot" wordmark → hint.

| Element | Spec |
| --- | --- |
| Logo mark | The pi.dev hero SVG geometry (three paths, viewBox 0 0 800 800) recolored to the three Palan brand colors — blue `#00A0E9` top bar, green `#6EB92B` left leg, orange `#F08200` right square (sampled from `datarx.cn/images/palan-logo/logo-palan.png`, Dr. Lin 2026-09-24; supersedes the pi.dev `#F09082`/`#4D9ABF`/`#F1BE58`) — rendered inline at 88×88px with **no container** (Dr. Lin, 2026-09-17); fixed colors, no light/dark swap |
| Tagline | pi.dev hero slogan "There are many agent harnesses but this one is yours"; Plantin italic serif stack (`"Plantin MT Pro", …, Georgia, serif` — the commercial face is NOT bundled, Georgia is the fallback), `clamp(1.5rem, 4vw, 2.1rem)`, line-height 1.02, `letter-spacing: -0.015em`, `var(--text-primary)`; the word `yours` carries the Palan accent blue `#00A0E9` (Dr. Lin 2026-09-24, supersedes the pi.dev `#6a9fcc`). **Not an i18n key** — every locale shows the same English sentence (Dr. Lin, 2026-09-17) |
| App name | "Picot", 56px, weight 800, `letter-spacing: -0.02em`; brand name, never translated; `margin-top: var(--space-3)` tight under the tagline |
| Hint | `var(--font-size-lg)`, `var(--text-secondary)`, centered, max-width 420px; copy `landing.hint` |
| Button | Height 40px, `padding: 0 24px`, `border-radius: var(--radius-md)`, `var(--bg-glass-hover)` fill, `1px var(--border-bright)` outline, 16px lucide `plus` icon, label `sidebar.addProject` |

The landing view must theme correctly in all Picot themes (it consumes the
same CSS custom properties as the rest of the app; prototype demonstrates
night + clean).

## i18n

`landing.hint` is the only new key, in all four locales **specified here,
not deferred to implementation**:

| Locale | `landing.hint` |
| --- | --- |
| en | Select a session from the sidebar to begin, or click a workspace's + to start a new session. |
| zh | 从侧栏选择一个会话开始，或点击工作区的 + 开始新会话。 |
| ja | サイドバーからセッションを選ぶか、ワークスペースの + をクリックして新しいセッションを開始してください。 |
| es | Selecciona una sesión en la barra lateral para empezar, o pulsa el + de un espacio de trabajo para iniciar una nueva. |

The landing button reuses the existing `sidebar.addProject`
("Add project" / 添加项目 / existing ja/es). The approved prototype's
"Add a project" wording is superseded by decision #11. New user-visible
copy goes through `t()` and is covered by the language-switch redraw test
convention.

## Rust: owner state model

`window_owner.rs` facts the design relies on (verified):

- `commit_workspace_transition[_with_workspace]` already rebinds the owner
  record on commit (`workspace_id`, `canonical_cwd`, `temporary_kind`,
  `workspace_generation`). A landing owner becomes a normally bound owner
  after its first in-place transition — no new state machine.
- `authorize_navigation` allows same-origin navigation. The landing route
  `/` and all workspace routes share the host origin, so `/` →
  `/workspaces/{ws}/{session}` is already authorized. No privilege grant
  needed.
- Today there is no constructible "existing owner without a workspace":
  `workspace_id: None` records are `Temporary` with a mandatory
  `canonical_cwd` + `primary_port`.

Design:

1. **New `TemporaryKind::Landing`** variant.
2. The cold start creates the landing owner via
   `create_owner_with_workspace("native-landing", canonical_home_dir(), 0,
   host_origin, None, TemporaryKind::Landing)`. `canonical_home_dir()` must
   canonicalize the resolved home directory before owner creation.
   - The home path is an owner-record placeholder only; it is **never** a
     workspace identity, workspace scope, or authorization input. The
     landing owner has no registered workspace.
   - `primary_port: 0` (no runtime).
3. **Landing is never same-workspace.** `workspace_target_prepare` must
   derive `same_cwd` from `owner_current_workspace(owner)`: only
   `Registered { wid, .. }` with `wid == target_workspace_id` is same. A
   `TemporaryKind::Landing` owner always starts a cross transition, even if
   its placeholder home is itself a registered workspace. This increments
   generation and ensures landing-scoped ephemeral state is cleaned on the
   first commit.
4. **Registered-only workspace authority.** All workspace-scoped host
   operations must require `OwnerWorkspaceSnapshot::Registered`, not
   `current_workspace()` or a `Temporary` root. This includes Git, terminal,
   file/data scope, project-scoped config/skills, and Side Chat. Quick Chat
   is the sole landing exception: it uses its own ephemeral temporary cwd,
   never the owner placeholder and never a workspace-scoped operation.
   `workspace_snapshot_for` therefore rejects Landing/other Temporary owners
   for Side Chat and workspace operations; the Quick Chat admission path is
   explicit and separately tested.
5. **Label is never a state signal.** `native-landing` stays the window
   label for the window's whole life (Tauri labels are immutable), while
   the owner record carries the truth. Every place that previously keyed
   on `native-workspace-*` and must also apply to the transitioned landing
   window is re-keyed on the owner record (next section).

## Rust: cold start

`setup_native_runtime` changes:

1. **No default workspace**: delete the `ensure_default_workspace` call
   (and helpers if orphaned). The registry is untouched at startup.
2. **No runtime spawn**: no session target, no Pi process.
3. **Landing window**: open one window labelled `native-landing` at
   `{origin}/` (the host server already falls back to index.html for any
   route). Title "Picot", same size/icon/init-script treatment as
   workspace windows; owner per the state model above.
4. **Window destroy cleanup** (`handle_window_destroyed`, main.rs:3072):
   the full-cleanup branch is re-keyed from
   `label.strip_prefix("native-workspace-")` to "the window's owner exists
   in the registry". Within that branch:
   - `stop_for_owner` runs unconditionally (no-op when no runtime was
     ever spawned) — this is what makes a transitioned landing window kill
     its Pi subprocess.
   - `stop_for_window_destroy(workspace_id)` runs only when the owner
     record carries a `workspace_id`.
   - The stale comment "Native-only startup labels every window
     `native-workspace-{workspace_id}`" is removed/rewritten.
   - Regression tests: existing workspace window destroy still stops its
     runtime; landing window destroy pre-transition revokes the owner and
     stops nothing; landing window destroy post-transition stops the
     spawned runtime.
5. **Menu state (macOS) — New Session item**: enabled iff the focused
   webview window's owner has a workspace binding (`workspace_id: Some`),
   read from the registry — not from the label. Recompute on window focus
   events **and** on workspace transition commit (a transition inside the
   landing window changes binding without any focus change). Startup
   default: disabled (the first window is the landing). The
   enable/disable decision lives in a pure helper for unit testing.
6. **Add a Project menu item**: inserted after New Session in the File
   menu, no accelerator. Handler re-dispatches into the focused webview
   window (any owner-bound window) so the web side runs the real flow —
   same pattern as the New Session synthetic event:

   ```js
   document.getElementById("add-project-btn")?.click();
   ```

   Picker cancellation is a no-op (existing behaviour). Non-macOS keeps no
   native menu (unchanged); the sidebar `+` and landing button cover it.
7. **Cmd+N dispatch** (main.rs:3228): re-keyed from label prefix to "the
   focused window's owner has a workspace binding". Consequence: after the
   landing window's first transition, Cmd+N reaches it even though its
   label is still `native-landing` — matching the menu item being enabled
   by the same rule.

**Invariant rewrite**: `native_cold_start_always_selects_default_workspace`
is replaced by tests asserting cold start → landing window, no spawned
runtime, no registry mutation. The 2026-09-03 decision「冷启动一律以
~/.pi/tmp 为 workspace」is superseded; record that in the memory bank when
this lands.

## Frontend: landing mode

- **Route discriminator and bootstrap**: `/` (no
  `/workspaces/:ws/sessions/:session` match) is the native landing route.
  Bootstrap must decide this from the canonical route plus the native
  capability injected by the host **before** importing `app.js`: update
  `bootstrap-entry.js` to import `landing.js` for native `/`, and retain
  `app.js` for canonical workspace routes and all non-native/browser routes.
  It must not wait for the post-connect `hostCapabilities` event, because
  `app.js` constructs the chat object graph before that event. Non-native
  `/` behaviour remains unchanged and out of scope.
- **Landing bootstrap**: `landing.js` constructs only the WebSocket/control
  transport, native sidebar, landing-local transition controller, landing
  notice, and a landing-compatible Quick Chat. It does **not** construct or
  register listeners for MessageRenderer, ToolCardRenderer, composer,
  ConfigGateway, Side Chat, file preview/browser, Git panel, terminal,
  model picker, or their owner-bootstrap handlers. In particular, no Git
  refresh may run at landing generation 0.
- **Quick Chat**: the landing Quick Chat view has no active-session model
  catalog dependency (`ConfigGateway` requires one). It uses the existing
  ephemeral Quick Chat protocol with a landing-specific view/profile path;
  it cannot create Side Chat or invoke workspace-scoped tools. Its close and
  first-workspace-transition cleanup follow the existing ephemeral registry
  lifecycle.
- **Markup**: landing markup lives in `index.html` (hidden by default);
  `landing.js` reveals it and hides the workspace chrome (`.workspace`
  content: header, messages, composer, right rail). View construction,
  i18n application and button wiring live in `landing.js`.
- **Landing transition handler** (`landing.js`): the only way landing
  enters a workspace.

  ```text
  enterWorkspace(path, { sessionPath?, forceNewSession? })
    → transport.prepareWorkspaceTarget(path, {…})
    → transport.commitWorkspaceTransition(generation)
    → snapshotUiStateForNavigation()   // null-safe: chat fields absent
    → navigateInWindow(prepared.targetOrigin, { targetCwd: path })
  ```

  It must not touch `messageRenderer`, `resetUiForNewSession`,
  `quickChatDialog`, `filePreviewPanel`, `terminalPanel`, model picker, or
  any other chat-lifecycle object (review finding: the existing
  `handleSessionSelectImpl` / `handleNewProjectChat` depend on them and
  throw in landing mode). Errors render into a landing-local notice, not
  the chat error renderer.
- **Sidebar wiring in landing mode** — four injected seams, all routing
  to `enterWorkspace`:
  1. session-row selection → `enterWorkspace(path, { sessionPath })`
     (`reuseExisting` per live-instance check, as today);
  2. workspace `+ New Chat` → `enterWorkspace(path, { forceNewSession:
     true })` — this is how zero-session workspaces are entered;
  3. `onRegisterWorkspace` (post-add-project navigation);
  4. Focus capability: `SessionSidebar` receives explicit
     `canFocusWorkspace(project)` and `onWorkspaceFocus(project)` seams.
     Landing enables Focus for registered rows and routes it to
     `enterWorkspace(path, { focusWorkspaceId })`; the prepared target URL
     carries the existing focus parameter. It does not call workspace-page
     `enterFocus`, which only mutates the current document.
  The picker/register part of `addProjectViaPicker` is reused as-is; only
  its post-register navigation lands in the injected seam. Overlapping
  clicks are guarded by the existing launch-in-progress serialization
  pattern (a landing-local flag, since `workspaceLaunchInProgress` is
  chat-side).
- **Sidebar state under landing** (all existing behaviour, no new
  semantics): search filters rows client-side; expand/collapse state is
  the existing in-memory `expandedWorkspaces`; pin/置顶 is the DB-backed
  registry pin; row context menus (rename/remove/pin) work — "remove
  current workspace" protection is moot because no workspace is current;
  there is no active-workspace highlight. Focus is the explicit transition
  seam above, not the workspace-page `enterFocus` implementation.
- **Zero-session workspaces render**: a registered workspace with no
  session bucket yet (freshly added, no JSONL ever written) still appears
  as a sidebar row — `list_workspaces_and_prune` only prunes vanished
  directories, and merge/render have no has-sessions filter. Its `+ New
  Chat` button is the entry path (decision #10); `session_bucket` NULL is
  a navigation detail, not a display condition.
- **Stale bucket pointers are out of scope (do not "fix")**: a manually
  deleted bucket directory degrades gracefully —
  `read_workspace_session_bucket` returns zero sessions for a missing dir
  (host_data.rs `!bucket.exists()`), the row stays as a zero-session
  workspace, and the pointer self-heals when Pi recreates the
  deterministic bucket name on the next session
  (`record_pi_session_bucket` re-stamps from the reported sessionFile).
  Prune must NOT remove rows for NULL/stale buckets: NULL is the landing
  flow's normal post-add state, and row removal stays an explicit sidebar
  action.
- **Quick Chat**: shown in native landing; its dialog overlays the landing
  view. It exposes no Side Chat or workspace-scoped tools.

## ARCHITECTURE.md (hard sync item)

The same change must update the ARCHITECTURE.md startup/lifecycle section
that still documents「冷启动固定注册并打开 ~/.pi/tmp、预创建 session」.
Shipping the code without this update fails review, same as a failing
test. The memory bank note recording the superseded 2026-09-03 decision
lands in the same change.

## Verification

- **Rust unit tests**:
  - cold start → landing window, no runtime, no registry mutation;
  - menu-enable helper truth table (focused owner binding × commit state);
  - destroy cleanup matrix: workspace window / landing pre-transition /
    landing post-transition (runtime stopped, owner revoked);
  - Cmd+N dispatch reaches a bound `native-landing`-labelled window and
    not an unbound one;
  - Add a Project menu id present, no accelerator;
  - `TemporaryKind::Landing` owner construction + commit rebind; Home
    registered as a workspace still produces a cross first transition
    (generation increments, landing Quick Chat cleanup runs);
  - Landing owner rejects Git, terminal, Side Chat, file/data and
    project-scoped config/skill operations; landing Quick Chat remains
    allowed without using the owner placeholder root.
- **Frontend (vitest)**:
  - native `/` selects `landing.js` before `app.js`; it constructs none of
    MessageRenderer, terminal, file-preview, Git, Side Chat, or ConfigGateway;
  - route `/` renders landing and hides workspace chrome (native);
  - `enterWorkspace` calls prepare → commit → navigate and touches no
    chat-lifecycle object;
  - four sidebar seams route to `enterWorkspace` (session row, + New
    Chat, post-add-project, Focus with `focusWorkspaceId`);
  - hint + button labels follow locale (en/zh at minimum);
  - Quick Chat button visible in landing mode and opens without an active
    session or model-catalog request.
- **Manual e2e (end-user view)**:
  fresh launch → landing, `ps` shows no Pi child;
  click a session → same-window transition with overlay, menu enabled;
  zero-session workspace → `+ New Chat` enters it;
  Add a project from landing button and from File menu → picker → new
  workspace opens in place;
  Cmd+N on landing → item greyed, keypress no-op;
  Cmd+N after transition → works;
  close the transitioned window → Pi subprocess keeps running (window close
  detaches; the runtime survives for a reattach);
  the sidebar workspace menu's “Stop Running” entry (confirm dialog) stops
  that workspace's Pi subprocess;
  Quick Chat opens over landing without workspace tools; theme switch
  redraws landing (dark/light); landing Focus enters the selected workspace's
  Focus view.
- `bun run check:rust`, `bun run check`, focused tests first, then
  `bun run test`.
