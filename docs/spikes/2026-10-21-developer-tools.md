# What else Hutan could include — developer tools, and UI verification by agents with Cua Driver

**Status:** research and proposal; no implementation decision is implied. Snapshot 2026-10-21. Nothing was built, started or run for this report: no Hutan process, no `cua-driver` action, no model call. The only commands run were read-only: `cua-driver --help | describe | list-tools | status | permissions status | telemetry status | mcp-config` and `git`/`grep` on this repository. Everything not observed that way is marked **[INFERENCE]**. Effort: **S** ≤ 2 implementer-days, **M** 3–7, **L** > 1 week **[INFERENCE: calibrated by eye against `docs/briefs/`]**.

Question from the user: what else could Hutan include, which tools could be integrated, and in particular how could a computer-use driver like Cua Driver let agents running inside Hutan verify the UI/UX of what they build.

## 0. Summary

**The short answer on Cua.** The plumbing to hand an agent a Cua MCP server already exists (`[mcp.<name>]` in `daemon.toml`, `docs/agents.md:190-197`). What is missing is everything around it: *scope* (which app an agent may drive), *consent* (a Hutan approval, not the harness's), *evidence* (screenshots and recordings that Hutan keeps and shows — today an ACP image block becomes the text `[image]`, `crates/agents/src/acp/mapper.rs:1221`) and a *usable AX tree of Hutan itself* (our pinned GPUI already ships AccessKit for macOS, yet no Hutan code assigns a role — §3.8). Cua Driver's own **bounded mode** (capability manifest: tools, apps by bundle id, no desktop capture) is a good enforcement layer and maps one-to-one onto Hutan's role as the "trusted launcher" (§3.3, §4).

**Ranking (details in §5).**

| # | Slice | What | Effort | Needs user decision |
|---|---|---|---|---|
| 1 | **UC-0 → UC-1** | Gated "UI check": per-session *bounded* Cua daemon, one approved app, MCP handed to the agent, daemon audit; UC-0 is a 1–2 day measurement first | M (+S) | D1 |
| 2 | **AR-1** | Daemon-owned bounded artifact store; images/recordings from agents shown in chat | M | D2 |
| 3 | **AX-1** | Accessibility roles/labels across Hutan's GPUI kit (VoiceOver *and* Cua/AX verification) | M | – |
| 4 | **UT-1** | `usertester` agent profile switched from osascript to Cua practice (dev process) | S | – |
| 5 | **CK-1** | Per-project check recipes run as tracked sessions; results attached to task delivery | M | D3 |
| 6 | **PV-1** | Listening ports per session + preview recipes (URL for the agent *and* the human) | M | D3 |
| 7 | **GH-1** | PR/CI/review-thread panel through the user's own `gh` (explicit network actions) | L | D4 |
| 8 | **MP-1** | Vetted MCP presets with honest telemetry/permission disclosure | S | – |
| 9 | **UG-1** | Local token/cost page from journals (no provider quota scraping), folded into the traces page | M | – |
| 10 | **UA-1** | Acceptance runner: scripted UI checks of Hutan's own bundle on a real display | L | D5 |

**Recommended first slice: UC-0 + UC-1 for one app, evidence on disk, with UT-1 landing in parallel as a one-hour profile edit.** Reasons: it is the user's explicit goal; the invariants force the interesting part (scope + approval + audit) to live in the daemon; it needs one ADR but no schema change beyond one additive capability; AR-1 and AX-1 follow naturally and make it pleasant. See §7 for the five decisions.

## 1. What Hutan already has, and what earlier spikes cover (not re-proposed)

| Area | State | Source |
|---|---|---|
| Daemon-owned sessions, PTYs, approvals, audit; clients render | done | `AGENTS.md` invariants; ADR 0001 |
| Control surface, three doors (CLI / `hutan mcp` / desktop), origin + `[control]` policy, "what always asks", per-turn limit | done | `docs/control.md:3-9,44-47,60-64,95`; ADR 0016 |
| Harness chats over ACP/Codex, rich input (images *to* the agent), questions | done | `docs/agents.md:131-170,236,252-291` |
| User MCP servers passed to every harness session | done (applied after daemon restart) | `docs/agents.md:190-197`; `docs/control.md:170` |
| Worktrees, tasks with dependencies, delivery states, diff review + comments, merge | done; **never fetches/pushes** | `docs/orchestration.md:91-176`; `docs/orchestration.md:178` |
| Resource manager: per-session process trees (libproc), kill/signal/interrupt | done | `docs/resources.md:7-33` |
| Usage numbers on agent nodes (`usage.cost_usd` optional) | data exists, no dashboard | `docs/protocol.md:316`; `docs/gateway.md:138` ("no cost accounting") |
| Mawas, memory, grants | done to MW-2 | `docs/assistant.md`, `docs/control.md:80-112` |
| Editor/LSP (M11), embedded browser (M14), rich content (M15) | **not started** (no `editor`/`lsp_client`/`browser` crate in `crates/`) | `docs/roadmap.md:120-123,155-160` |
| GPUI, pinned | `zed-industries/zed` rev `ef376006…` | `Cargo.toml:86` |
| Protocol | 0.32 | `docs/protocol.md:1`, `crates/protocol/src/lib.rs:75` |

**Already researched — referenced, not repeated:** traces and OTLP (`2026-10-20-laminar.md` §10: TR-1…TR-5), secrets vault and `secret://` (`2026-10-18-vault-traces.md`), schedules/automations, skills, MCP additions and the unattended-security prerequisite (`2026-10-18-meta-harness-assistant.md` §6–§9), remote hosts (`2026-10-18-remote.md`), task workspaces (`2026-10-18-task-workspaces.md`), Orca/herdr models (`2026-10-18-orca-*.md`, `2026-10-18-herdr-model.md`), settings v2 and themes. UX gaps already logged and unscheduled: send-after-turn queue (`2026-10-17-ux-audit.md:142`, row 11), accessibility completeness (row 16, line 147), PR/CI (row 15, line 146), previews/setup recipes (row 14, line 145).

## 2. Comparable tools — what they solve that Hutan does not yet

Only tools whose primary pages I could read on 2026-10-21. **Not verified:** Factory (`docs.factory.com/welcome` returned HTTP 404 from this environment). Orca and herdr are in earlier spikes.

| Tool | Developer problem it solves that Hutan does not yet | Link |
|---|---|---|
| **Claude Code Desktop** | (1) *Auto-verify*: Claude starts the dev server in a Browser pane, screenshots, clicks, fixes; dev servers from `.claude/launch.json`; (2) PR monitoring with CI status, **Auto-fix** and **Merge when ready**; (3) iOS Simulator pane; (4) *computer use* with per-app tiers (view-only browsers, click-only terminals/IDEs, full control otherwise) approved per session, background mode on macOS; (5) correct a running agent without stopping | <https://code.claude.com/docs/en/desktop>, <https://code.claude.com/docs/en/desktop-ios-simulator> |
| **Codex / ChatGPT desktop app** | Computer Use plugin: per-app approval with "Always allow" list, locked-use, refuses to automate terminals; "testing a macOS app … that ChatGPT is building" is a listed use; built-in browser preferred for local web apps | <https://learn.chatgpt.com/docs/computer-use> (reached from <https://developers.openai.com/codex/app>) |
| **Superset** | In-app browser with **Design mode** (element → agent with DOM/styles/crop) and `superset browser … cdp` endpoint scoped to the pane; **port discovery per workspace**; a `computer` skill with **Cua Driver as the default driver** (Peekaboo alternative); **Usage** page (token cost from local logs *and* quota meters read from providers' endpoints with stored agent credentials); automations/remote/CLI/SDK/MCP | <https://docs.superset.sh/browser>, <https://docs.superset.sh/ports>, <https://docs.superset.sh/computer>, <https://docs.superset.sh/usage>, <https://github.com/superset-sh/superset> |
| **Conductor** | **Checks tab** as the merge gate (git status, PR, CI, deployments, review threads, todos; can block merge); HTTP API + MCP server + webhook "routines" for cloud workspaces | <https://www.conductor.build/docs/reference/checks>, <https://www.conductor.build/docs/api> |
| **cmux** | Sidebar per workspace shows branch, **PR status**, cwd, **listening ports**, latest notification; scriptable browser (ported from agent-browser); socket API | <https://github.com/manaflow-ai/cmux> |
| **Cursor Cloud Agents** (cloud VM, not local) | **Computer use inside the agent's VM, artifacts (screenshots/videos/logs) attached to the PR**, take over the agent's desktop, event *subscriptions* (CI, PR comments, Slack), auto-fix CI | <https://cursor.com/docs/cloud-agent>, <https://cursor.com/docs/cloud-agent/capabilities.md> |
| **Zed agent panel** | Queued messages with *Steer*, checkpoints, `@diagnostics`/diff mentions | <https://zed.dev/docs/ai/agent-panel> |
| **Warp** | Code Review panel over git diffs; cloud-agent management | <https://docs.warp.dev/code/code-review>, <https://docs.warp.dev/platform/managing-cloud-agents/> (code-review page seen as a search snippet only) |
| **OpenCode** | LSP diagnostics as agent feedback — **disabled by default**, and its own docs warn it can desync, use memory and slow agents; recommends lint/typecheck CLIs instead | <https://opencode.ai/docs/lsp/> |
| **Sculptor** (Imbue) | Bundled workflow skills, e.g. `/sculptor-workflow:fix-bug` (reproduction interview → failing tests → fix) | <https://github.com/imbue-ai/sculptor> |
| **Jules / Devin** (cloud) | Plan-approval-first async tasks and `AGENTS.md` (Jules); take over the agent's browser/IDE (Devin). Cloud-hosted; Hutan is local-first, so patterns only | <https://jules.google/docs>, <https://docs.devin.ai/get-started/devin-intro> |
| **Crystal → Nimbalyst, Claude Squad, Vibe Kanban** | Nothing Hutan lacks: worktrees + sessions + diff. Market signal: Crystal is deprecated (Feb 2026) for Nimbalyst; **Vibe Kanban is sunsetting** | <https://github.com/stravu/crystal>, <https://github.com/smtg-ai/claude-squad>, <https://github.com/BloopAI/vibe-kanban> |
| **Verification servers** (agents' own tools, not orchestrators) | Chrome DevTools MCP (console/network/trace; usage statistics on by default, `--no-usage-statistics`), Playwright MCP (accessibility-tree browsing; README says CLI+skills is more token-efficient for coding agents), XcodeBuildMCP (build/sim tools; Sentry telemetry) | <https://github.com/ChromeDevTools/chrome-devtools-mcp>, <https://github.com/microsoft/playwright-mcp>, <https://github.com/getsentry/XcodeBuildMCP> |

**Patterns worth taking.** (1) *Verification is a first-class loop*: Claude, Codex and Cursor all ship "the agent runs the thing and looks at it" — browser first, computer use for what a browser cannot reach, with the **most precise tool preferred** (`https://code.claude.com/docs/en/desktop#when-computer-use-applies`). (2) *Evidence travels with the work*: artifacts attached to the PR (Cursor), not "the agent said it worked". (3) *Per-app consent with tiers* (Claude, Codex). (4) *Merge readiness is a panel* (Conductor, Claude). (5) Superset made Cua Driver its default — precedent that this driver is the de-facto choice for local, background, MCP-addressable computer use.

**Patterns not to take.** Superset reads subscription quota from providers using stored agent credentials (`https://docs.superset.sh/usage`); Hutan must not read, store or forward harness tokens (`docs/agents.md:205`, ADR 0008/0015). Cloud VMs, hosted queues and Slack-first triggering are out of scope for a local-first daemon (`docs/vision.md:76-78`).

## 3. Cua Driver deep-dive

### 3.1 What it is, and what is installed here

Open-source (MIT) driver that lets an agent *see and operate* native apps and browsers on macOS, Windows and Linux through accessibility APIs, with background delivery (no focus theft) where the platform allows (<https://cua.ai/cua-driver>, <https://github.com/trycua/cua>, <https://cua.ai/docs/cua-driver/concepts/how-cua-driver-works>). It never calls a model; the harness owns the loop. Surfaces: `cua-driver mcp` (stdio MCP), `cua-driver call <tool> '<json>'` (CLI, one disposable session per call), `cua-driver serve` (daemon), SDKs. Observed here: `cua-driver 0.37.0` at `~/.local/bin/cua-driver`, `/Applications/CuaDriver.app` present; daemon running on `~/Library/Caches/cua-driver/cua-driver.sock`, **permission mode `standard`**, no policy, no manifest; Accessibility and Screen Recording granted to `com.trycua.driver`; **telemetry enabled by default** (`cua-driver status`, `permissions status --json`, `telemetry status`, 2026-10-21). 62 lines in `list-tools`.

### 3.2 Capabilities relevant to verifying UI

| Need | Tool / feature | Notes (source: `cua-driver describe <tool>` 0.37.0 unless a URL) |
|---|---|---|
| See a window | `get_window_state {pid, window_id}` → AX tree (markdown rows `[N]` with `element_token`) **plus** screenshot | start narrow: ≤ 250 nodes by default, `query`, `max_depth`; **diff reads** `since:"latest"` return only `+ ~ -` rows; `screenshot_out_file` writes the PNG instead of embedding; `degraded_reason: screen_locked` when the display is locked; "the tree can lie on some surfaces (Electron, Catalyst, virtualized rows)" |
| Discover | `list_apps`, `list_windows`, `get_accessibility_tree` (no TCC needed), `get_desktop_state` (full display — **the dangerous one**) | |
| Act | `click`, `double_click`, `right_click`, `type_text`, `press_key`, `hotkey`, `scroll`, `drag`, `set_value`, `invoke_menu` (AX menu path, never pixels), `set_window_frame` | by `element_token`, by `{role,name,nth}`, or by window pixels |
| **Batch + assert** | `run_steps`: ≤ 64 steps, stop at first failure, per-step `wait_for` (before) and `expect` (after, ≤ 4 checks `{role,name,text,gone,value,value_contains,enabled,selected}`), one `observe` diff at the end, `foreground_fallback` | this is the "test" primitive; report says which step failed in which phase |
| Programmatic | `run_script` (sandboxed JS, no fs/net, ≤ 500 calls, ≤ 120 s), `verify_state` (≤ 8 predicates AND-ed, `stable_samples`, optional screenshot; absence is `unknown`, never success) | |
| Honest effect reporting | every action returns `effect: confirmed | unverifiable | suspected_noop | partial | refused`, `escalation` hint | "A delivered event is not an applied change" (<https://cua.ai/docs/cua-driver/concepts/how-cua-driver-works>) |
| Launch / isolation | `launch_app {bundle_id, creates_new_application_instance, webkit_inspector_port, additional_arguments}` | a new instance gets its own pid/window; background launch |
| Record | `start_recording {output_dir, record_video, include_accessibility_tree}`; `stop_recording`; CLI `recording start|stop|status|render <dir> <out.mp4>` | per-action folder `turn-NNNNN/`: `before.png`, `after.png`, `before_state.json`, `after_state.json`, `action.json`, `click.png` (red marker), `evidence.json`; MP4 via ScreenCaptureKit (macOS 15+) |
| Replay | `replay_trajectory {dir, delay_ms, stop_on_error}` | **element-token actions fail on replay; pixel clicks and keyboard replay**; "record the replay against a new build and diff trajectories" is the documented regression workflow |
| Browser | `browser_prepare`, `get_browser_state`, `browser_navigate|click|type|select|pointer|dialog|download|set_input_files` | exact `(pid, window_id)` binding; isolated driver-owned Chromium profile or **attach to logged-in profile (needs a grant)**; Chrome on macOS proven; Safari/Firefox/WKWebView get native AX + pixels only (<https://cua.ai/docs/cua-driver/concepts/platform-support>) |
| Perception (optional) | `parse_visual_regions` via the `cua-perception` extension (local OCR + icon detector, ~2–2.5 s/screenshot on macOS) | extension is **AGPL-3.0** (OmniParser detector); skip unless needed (<https://cua.ai/docs/cua-driver/concepts/how-cua-driver-works>) |
| Session UX | `start_session`/`end_session`, per-call `session` label, agent cursor overlay, `--experimental-pip` preview window | sessions share **one** screen/keyboard/focus; native input is admitted one action at a time |

Supported UI stacks on macOS: AppKit, SwiftUI, Electron, Tauri, WKWebView (browser limits above). Windows and Linux are supported too, which fits the portable-daemon direction (`docs/spikes/2026-10-18-cross-platform.md`) **[INFERENCE: fit only]**.

### 3.3 Permission model (this is what makes bounded integration possible)

All transports pass one authorization check; the *launcher* fixes mode, manifest and grants at start and an agent cannot change them; a running daemon cannot be reconfigured (<https://cua.ai/docs/cua-driver/guides/permissions.md>).

| Mode | Behaviour |
|---|---|
| `standard` (default; **what runs here now**) | Observe, click, type, screenshot, record, isolated browsers run **without prompts, in any app**; attaching to a logged-in browser profile needs a grant |
| `bounded` | A **capability manifest** is mandatory; in-scope work is silent, everything else is denied |
| `unrestricted` | Approvals bypassed; "no protection against prompt injection" |

Manifest v3 (deny by default, narrow-only): `allow.tools`, `resources.apps[{bundle_id|executable, launch, windows, terminate: driver_launched}]`, `resources.files.{read,write}`, `resources.browser.{profiles,origins}`, `desktop.display` (false keeps full-display capture and screen-coordinate input off), mandatory `expires_after` + `idle_timeout` in bounded mode, `--approve-capability-manifest` = "the trusted launcher asserts a human reviewed this exact manifest". Outside the manifest: `bounded_resource_outside_manifest` / `permission_denied`. `cua-driver revoke --session|--all` ends grants. Optional YAML/Rego policy files add argument constraints (`type_text` pattern/length). **Honest limits stated by Cua:** policies do not filter responses, rate-limit or authenticate callers, and same-user code/malware is outside the boundary. A manifest with browser origins refuses to start together with generic desktop tools.

Already exercised on this project: the integrator's *bounded* daemon refused `kill_app` on the app under test with `bounded_resource_outside_manifest` while allowing window interaction (`docs/spikes/2026-10-17-user-test-2.md:212`), and all rounds 2–4 ran through `cua-driver call --socket …/cua.sock` (`2026-10-17-user-test-2.md:15`, `2026-10-18-user-test-3.md:24`, `2026-10-18-user-test-4.md:21`).

### 3.4 Sessions, delivery, concurrency

Implicit session per connection, 5 idle minutes, tokens/snapshots die with the session; named sessions via `session` label (a label is **not** a credential). Delivery ladder: element/AX background → pixel background → browser CDP → foreground (only for that action). On macOS, TCC grants belong to `CuaDriver.app` (`com.trycua.driver`), not to the calling terminal — a good property for Hutan: **neither `hutand` nor `Hutan.app` would need Accessibility/Screen Recording** (embedding the driver would transfer that risk to Hutan: `--embedded` "inherits the host's TCC grants"). Bounded daemons on macOS are started through the app bundle: `open -n -g /Applications/CuaDriver.app --args serve …` (permissions guide). Concurrency: two sessions that use foreground delivery fight over focus **[INFERENCE from "one screen, keyboard, focus" + "one action at a time"]** → Hutan should serialise UI-check *leases* per desktop.

### 3.5 Cost observed in our own runs

Subprocess round trips were slow: median `bring_to_front` 3.7 s, `click` 6.3 s, `type_text` 5.4 s, screenshot 3.5 s, `scroll` 22.9 s (`2026-10-18-user-test-3.md:247-251`); `type_text` median 4.6 s over 55 calls (`2026-10-18-user-test-4.md:227`). The reports explicitly say these are driver round trips, not app latency (and recommend a direct timing instrument for latency claims: `2026-10-17-user-test-2.md:206`). Those runs used `bring_to_front` + foreground delivery before every action; whether background delivery works against GPUI at all is **unmeasured** (Cua lists some toolkits that refuse background input). `run_steps` batching is the lever **[INFERENCE]**; UC-0 should measure.

### 3.6 MCP vs CLI

MCP: `cua-driver mcp` (macOS: proxies to the `CuaDriver.app` daemon; `--socket` to reach a specific daemon). CLI: `cua-driver call <tool> '<json>'` plus an installable skill (`cua-driver skills install` — **writes into agents' skill directories**) and a read-only `get_guide` tool. Playwright's README argues CLI+skills is more token-efficient than loading big tool schemas (<https://github.com/microsoft/playwright-mcp>); Cua has ~60 tools, `get_window_state` already trims by default. For Hutan: **MCP for chat sessions Hutan starts** (same wiring as `hutan mcp`, `docs/control.md:170`; a sandboxed Codex shell may not reach a daemon socket **[INFERENCE]**), **CLI/skill for TUI agents and the `usertester`**.

### 3.7 Things Hutan must not do silently

- Install the driver (`curl | bash` installer) or run `cua-driver update` — Hutan *detects* harnesses and never downloads them (`docs/agents.md:178`); same here, with a version check like `SUPPORTED_VERSIONS` for Codex (`docs/agents.md:22`).
- Change Cua's telemetry (default-on, content-free, `cua-driver telemetry disable`) — disclose it (MP-1), let the user decide.
- Run `cua-driver skills install` / `mcp-config --client …` (they write into agent config): same rule as hooks, `docs/integrations.md:51`.

### 3.8 What a driver can and cannot see in Hutan (GPUI)

**Fact:** at Hutan's pinned Zed rev `ef376006f32728b15e6a67706a04849cb781c812` GPUI contains `crates/gpui/src/_accessibility.rs` and an AccessKit adapter for macOS — `accesskit_macos::SubclassingAdapter` in `crates/gpui_macos/src/window.rs` (lines ~712, 2376 in the local checkout) and `accesskit_macos 0.26.3` is in `Cargo.lock`. Upstream work: PR #51097 (closed) → #56065 "gpui: Accesskit support" (closed; testers reported VoiceOver working with the `a11y` example on macOS 26.5) — <https://github.com/zed-industries/zed/pull/56065>, <https://github.com/zed-industries/zed/blob/main/crates/gpui/src/_accessibility.rs>.
**Fact:** GPUI reports only elements that have an `id` **and** a `role`; "nodes with no role are not reported" (same file).
**Fact:** `grep` for `on_a11y_action`, `.role(Role…`, `a11y_role`, `AccessibleAction` across `crates/` finds **nothing**.
**[INFERENCE, to be measured in UC-0]:** Hutan's AX tree is therefore almost empty — the window frame and native menus only. Consistent with the test rounds, which drove Hutan by pixel coordinates from screenshots (`2026-10-17-user-test-2.md:21`) and used AX only for native menus and the status item (`2026-10-18-user-test-3.md:24,60-61`).
**Fact (fixed since):** `type_text` into Hutan fields produced "ффф" on a Russian layout because fields read only `key_char`; TI-1 moved every field to GPUI's input handler and is merged (`docs/briefs/TI-1.md:8,23`, commits `1b99571`, `70dc1d6`).
**Fact:** `Window::render_to_image` exists but is behind `test-support` (gpui `window.rs` `#[cfg(any(test, feature = "test-support"))]`), so Hutan cannot cheaply screenshot itself in release builds; an external capture (Cua, `screencapture`) is the realistic route.

**Consequence:** for verifying *Hutan itself*, Hutan's own control surface is a stronger oracle than AX today: `ui.layout` / `ui.actions` give exact layout and settings, `ui.run` runs the same action path as keyboard and palette (`docs/control.md:9-15`). Cua adds what that surface cannot: real pixels, native dialogs/menus, real input events. For verifying *someone else's* app (Tauri, Electron, SwiftUI, AppKit, web) the AX tree is the primary route and AX-1 is irrelevant.

## 4. Integration options

Common facts for all options: ADR 0001 (daemon owns processes, approvals, files) and ADR 0016 (everything an agent may ask is a daemon request with three doors; Mutate follows `[control]`, always-ask list never takes grants, `docs/control.md:60-64,95`). The origin marker is **not** a security boundary against a hostile agent (`docs/control.md:46`); the same holds here — a hostile agent can call the standard-mode Cua socket directly. The honest claim is "protects against accidents and prompt-injected well-behaved agents; the boundary against a hostile agent is the user's sandbox". Hutan can at least **detect and warn** when a `standard`-mode driver runs (`cua-driver status` reports the mode; it does on this machine).

### Option (a) — Dev process only: `usertester` drives Hutan with `cua-driver call`

- **What:** replace the osascript/`screencapture` instructions in `.omp/agents/usertester.md:12` (which no longer match practice — rounds 2–4 used Cua) with the recipe that already worked: a *bounded* daemon with a manifest for the test bundle's bundle id only, `launch_app {creates_new_application_instance}`, exact pid/window, `run_steps` with `expect`, `start_recording` into the scratch dir, `screenshot_out_file`; keep `hutan ui layout|run` as the state oracle; record the standing caveat that the machine's installed daemon is `standard` mode. Add a short § to `docs/process.md`.
- **User value:** deterministic, faster, less flaky UI test rounds (round 1 notes unreliable coordinate clicks and a 10.2 s Window-menu failure: `2026-10-17-user-test.md:81,103`); evidence folders replace ad-hoc PNGs.
- **ADR 0001/0016:** untouched; no daemon involvement.
- **Protocol:** none.
- **Security:** same-user; manifest keeps the tester to one bundle id; clipboard residue risk already seen (`user-test-3.md:33`).
- **Effort:** S. **Dependencies:** Cua installed + permissions (done on this machine), TI-1 (merged).

### Option (b) — Product feature: "UI check" capability for agent sessions

Two variants; B1 is the recommendation, B2 an upgrade.

**B1 — scoped driver per session (recommended).**
- *Flow.* The user (Start sheet toggle, `hutan chat new --ui-check <app>`, or a task's `ui_check` field) or an agent (`uicheck_start` MCP tool → `control` approval) requests a UI-check for named apps. Hutan: (1) resolves each app to a bundle id (`Info.plist` of a `.app` path), (2) renders the **approval in plain words** ("Agent in session “api” asks to drive **Hutan Test.app** (dev.hutan.Hutan): it can read its windows and screenshots, click and type there, start and stop that app; it cannot touch other apps, the whole screen, the clipboard or your logged-in browser. Ends after 60 min.") with options *Allow once* / *Allow for this session* / *Deny* — and **no** always-grant (added to the always-ask list, `docs/control.md:95`), (3) writes `$HUTAN_HOME/run/uicheck/<id>/manifest.yaml` (v3: tools allow-list below, `resources.apps` = approved bundle ids with `terminate: driver_launched`, `desktop.display: false`, `expires_after`, `idle_timeout`), (4) starts a private bounded driver on a short socket path **[INFERENCE: macOS `sun_path` is 104 bytes, so not under a long `HUTAN_HOME`]** with `--approve-capability-manifest` — hutand *is* the trusted launcher that asserts the human reviewed it, (5) adds `cua-driver mcp --socket <path>` to the session's MCP servers (same mechanism as `hutan mcp`: ACP `mcpServers`, Codex `-c mcp_servers.…`), (6) points recording at `$HUTAN_HOME/artifacts/<session>/uicheck/` (AR-1), (7) on stop/expiry/session end: `cua-driver revoke --session`, stop the driver, keep artifacts per retention.
- *Default tool allow-list [INFERENCE: from tool descriptions]:* `start_session end_session list_windows get_window_state verify_state run_steps click double_click right_click type_text press_key hotkey scroll drag set_value zoom launch_app start_recording stop_recording` + `browser_*` only when origins are listed. **Excluded by default:** `get_desktop_state`, `clipboard_*` (privacy), `kill_app` (except `driver_launched`), `bring_to_front` and `invoke_menu` (can take focus; Cua's own Gemini example leaves out `launch_app` and `invoke_menu` for that reason — we keep `launch_app` because starting the app under test is the point, and its description says it launches in the background), `run_script` (extra sandbox surface; enable later), `set_config`, `install_extension`, browser attach to an existing profile.
- *Not a Hutan UI request:* ADR 0016 covers requests an agent makes of **Hutan**. This is a capability *grant* to the agent's other tools, brokered by the daemon. It still uses the same machinery: classification table (`crates/daemon/src/router.rs`), `control` approval, audit row, MCP table row (`crates/cli/src/mcp/table.rs`), `hutan` CLI door.
- *Protocol / capability:* additive minor after 0.32, capability `uicheck`: `uicheck.status` (Read: driver found? version, TCC state via `cua-driver permissions status --json`, daemon mode, telemetry state), `uicheck.start {session_id, apps, record?, ttl_secs?}` (Mutate, always-ask), `uicheck.stop`, `uicheck.list` (Read), event `uicheck.changed`; optional `SessionInfo.uicheck`. Golden fixtures regenerate (`AGENTS.md`: `just update-golden`). Settings: `daemon.toml [uicheck] enabled=false`, `driver_bin`, `ttl_secs`, `max_artifact_mb`, `retention_days` (daemon settings are user-only, `docs/control.md:64`).
- *New ADR required* (hard-to-reverse: it grants desktop control to agents).
- *User value:* the agent that just built the app can open it, click through, read the result and fix — the loop Claude/Codex/Cursor sell — inside Hutan, with a visible approval, an audit row, a kill switch (`hutan uicheck stop`, or the existing `session.kill` tree) and evidence in the chat.
- *Security risks and mitigations:*

| Risk | Mitigation |
|---|---|
| Agent drives the whole desktop | bounded manifest, `desktop.display:false`, app by bundle id, no `get_desktop_state` |
| Prompt injection from on-screen content | scope limits blast radius; per-session approval; tell the agent not to follow instructions seen in the app (skill text); Claude/Codex rely on classifiers + per-app tiers — we do not claim equal |
| A `standard` daemon keeps running next to it | `uicheck.status` warns; optionally refuse to start while one is running (D1) |
| Screenshots leak secrets to the model provider | they must reach the checking agent (that is the feature) — say so in the approval; never index; short default retention (D2) |
| Foreground fallback types into the wrong place | no `bring_to_front`; `foreground_fallback` stays false; Cua refuses on `screen_locked` |
| Two agents fight over one screen | daemon-side lease per desktop (FIFO), `uicheck.start` waits or is refused |
| Telemetry, self-update, skill install | not touched by Hutan (§3.7) |
| TCC grants widen | grants stay on `CuaDriver.app`; Hutan never embeds the driver |
| Hostile agent bypasses via the standard socket | stated limitation, same as origin marker |

- *Effort:* **M** (+ UC-0 S). Dependencies: Cua ≥ version range pinned in code (like `SUPPORTED_VERSIONS`), AR-1 for chat-visible evidence (otherwise paths only), ADR. Unknowns to measure first (UC-0): per-session `open -n` daemon cost (RSS, start time), whether `stop`/`status --socket` give clean lifecycle, whether background delivery works against GPUI/Tauri/Electron targets, whether a manifest hides disallowed tools from `tools/list` or only refuses calls **[all INFERENCE until measured]**.

**B2 — Hutan proxy MCP (later).** `hutan mcp` exposes `uicheck_*` tools that forward to a lazily started bounded driver, so approval can be requested *mid-session* (MCP server lists are fixed at `session/new`, so B1 needs scope decided up front). Hutan then also sees every call (audit per call, artifacts without relying on the harness, screenshots attached by Hutan). Cost: Hutan must track Cua's tool schemas per version — **L**, and only if lazy approval proves necessary.

**Zero-code baseline, and why it is not enough.** `hutan config set mcp.cua '{command="cua-driver", args=["mcp"]}'` works today (`docs/agents.md:197`) but hands *every* harness session the standard-mode driver: no scope, no Hutan approval (only whatever the harness asks), no evidence store, no audit. The slice's value is the gate, not the plumbing.

**Variant worth noting (not now): VM isolation.** Run the driver inside a disposable macOS VM (Cua's Lume / Spaces, <https://github.com/trycua/cua>; Claude's `requireCoworkFullVmSandbox` is the same idea) — strongest isolation, **L**, Apple-silicon heavy; revisit if D1 says "agents may drive anything".

### Option (c) — Acceptance runner: replay recorded UI checks as regression tests

- **What:** `just ui-accept` (xtask) builds a test bundle, starts it with a temp `HUTAN_HOME`, runs versioned scripts under `tests/ui/*.json` through `cua-driver call run_steps` / `verify_state` (with `expect` on AX roles) and compares a few reference screenshots; on failure keeps the recording folder. Raw `replay_trajectory` is the *wrong* base: element tokens do not survive replay and pixel clicks break with every layout/theme change (`describe replay_trajectory`). Prefer authored `run_steps` + Hutan's `ui.layout` oracle, using recorded trajectories only as an authoring aid.
- **User value:** the repo's own status file repeatedly blocks on human visual checks — "real-window visual check still pending" (`docs/status.md:11`), "visual acceptance and B4b need an unlocked display" (`docs/status.md:26`). A repeatable check on the user's Mac (nightly or on demand) converts those into evidence.
- **ADR 0001/0016:** none; developer tooling. Must stay **out of `just check`** (needs an unlocked real display; AGENTS.md "real model/API calls never run in the default gate" is the same spirit).
- **Protocol:** none.
- **Security:** bounded driver with the test bundle only; temp `HUTAN_HOME`; never the user's real app (`docs/status.md:14` rules).
- **Effort:** **L**. **Dependencies:** UT-1 (recipes), AX-1 (stable role-based `expect`s; without it you are back to pixels), a display that is not locked (`degraded_reason: screen_locked`).

### Comparison

| | (a) usertester | (b) UI check B1 | (c) acceptance runner |
|---|---|---|---|
| Value to users of Hutan | indirect (fewer bugs) | **direct** | indirect |
| ADR | no | **yes** | no |
| Protocol | no | additive `uicheck` | no |
| Main risk | none new | desktop control by agents | brittle pixels, locked screen |
| Effort | S | M | L |

## 5. The ten additions, ranked

Ranking = evidence of developer value × fit with invariants × effort, with dependencies pulling prerequisites up. Slice ids are proposals (checked against `docs/briefs/` names).

### 1. UC-0 → UC-1 — Gated UI check (Cua pass-through, bounded driver per session)
- **Problem.** Agents building UI cannot see it; Hutan has no way to give them eyes with a scope the user approved. **Evidence:** Claude Desktop auto-verify + computer use + per-app tiers (<https://code.claude.com/docs/en/desktop>); Codex Computer Use with per-app approvals (<https://learn.chatgpt.com/docs/computer-use>); Cursor computer use + artifacts (<https://cursor.com/docs/cloud-agent/capabilities.md>); Superset ships a Cua-based `computer` skill (<https://docs.superset.sh/computer>); our own test rounds already ran on Cua (`2026-10-18-user-test-4.md:21`).
- **Scope.** UC-0 (S): on a throwaway home, measure the five unknowns in §4 B1, capture Hutan's real AX tree, and draft ADR "UI checks". UC-1 (M): `uicheck.*` requests + `uicheck_*` MCP tools + `hutan uicheck status|start|stop|ls`, daemon supervisor (`crates/daemon/src/uicheck.rs`) that renders the manifest, starts/stops the bounded driver, hands `cua-driver mcp --socket` to harness sessions, audits, enforces TTL and the desktop lease; Start-sheet toggle and a "UI check" chip on the session row (desktop is render-only); docs in `docs/control.md`. Evidence on disk first (AR-1 makes it visible).
- **Effort:** S + M. **Risks:** §4 B1 table; Cua version drift; TCC prompts need the user present. **Decision:** D1.

### 2. AR-1 — Artifacts: images, screenshots and recordings in chat
- **Problem.** Visual evidence has nowhere to live: ACP image blocks become `[image]` (`crates/agents/src/acp/mapper.rs:1221`), the chat shows no images (`docs/agents.md:295`), images are not redacted and bytes are not journaled (`docs/agents.md:236`). **Evidence:** Cursor attaches screenshots/videos/logs to PRs (<https://cursor.com/docs/cloud-agent/capabilities.md>); Claude Desktop shows the agent verifying in a pane; roadmap M15 already wants "agent artifacts open as items" (`docs/roadmap.md:160`).
- **Scope.** Daemon artifact store `$HUTAN_HOME/artifacts/<session>/<id>` + table (metadata only in the journal: id, mime, size, sha256, title, source), additive `AgentEventKind` for an artifact and capability `chat.artifacts`; ACP tool-result `image` blocks and Cua `screenshot_out_file` paths become artifacts; bounded: per file (e.g. 8 MiB), per session, global cap with oldest-first sweep, size-checked on read like rich input; chat shows a bounded thumbnail card (decode lazily; desktop RSS budget ≤ 150 MB, `docs/vision.md:82`) and opens full size in a workbench item (M15's image item, or Quick Look first); `hutan artifact ls|get`; never fed to the history index; `recording render` MP4 linked, not inlined. Core/protocol change → contract owner.
- **Effort:** M. **Risks:** memory in GPUI image decode; secrets inside screenshots (cannot be redacted — disclose); disk growth. **Decision:** D2 (retention and default for recordings).

### 3. AX-1 — Accessibility roles and labels in Hutan's GPUI kit
- **Problem.** Hutan is nearly invisible to VoiceOver and to any AX-based driver, although the framework support is already compiled in (§3.8). UX audit row 16 asks for accessibility as an acceptance condition of every new control (`2026-10-17-ux-audit.md:147`). **Evidence:** GPUI `_accessibility.rs`, PR #56065 testers (VoiceOver on macOS 26.5 works for the example), Cua's tree-first action ladder (<https://cua.ai/docs/cua-driver/concepts/how-cua-driver-works>).
- **Scope.** In the shared kit (`crates/desktop/src/workbench/ui.rs`) give every button, tab, list row, toggle, dock button, approval/question card control and text field a stable `id`, a `Role`, and a label; rely on `.on_click` auto-registering the Click action; give custom elements (`terminal_view`, `EditableText` from TI-1) `a11y_role` and, behind AT activation only, bounded synthetic children (visible rows only) **[INFERENCE that cost is paid only when an AX client activates: the adapter asks for the initial tree through an activation handler]**; ids derived from domain ids (session id, action name) — the guide warns that duplicate global ids silently drop nodes in release builds; tests: a GPUI test that walks the registered action list and asserts each reachable control has role+label, plus the `get_window_state` before/after captured in UC-0 as the acceptance evidence.
- **Effort:** M. **Risks:** GPUI API churn (R1 in `docs/roadmap.md:204`); per-frame cost; large diff across the kit (one owner, one worktree). **Decision:** none — recommend treating it as a default acceptance condition, as row 16 proposes.

### 4. UT-1 — `usertester` on Cua (option a)
- **Problem.** The agent profile prescribes osascript clicks and `screencapture`, which round 1 found unreliable; later rounds used Cua by supplied instruction. **Scope.** Rewrite `.omp/agents/usertester.md:12` and add `docs/process.md` § "UI testing with Cua": bounded manifest template for the test bundle, launch/targeting rules, `run_steps` with `expect`, recording into scratch, evidence naming, cleanup (`revoke`, quit app, stop daemon), what not to claim (driver latency is not app latency). **Effort:** S. **Risks:** none new. **Decision:** none.

### 5. CK-1 — Project check recipes with evidence
- **Problem.** "Done" is a claim; delivery states exist (`docs/orchestration.md:122-132`) but no *validation evidence* (UX audit row 6/14, lines 137,145). **Evidence:** Conductor Checks blocks merge on failed checks (<https://www.conductor.build/docs/reference/checks>); Claude auto-verifies after edits (<https://code.claude.com/docs/en/desktop#preview-your-app>); OpenCode's own LSP page recommends "run lint, typecheck, or other diagnostic CLI tools directly" (<https://opencode.ai/docs/lsp/>) — which is a stronger case for recipes than for an LSP feed.
- **Scope.** `<repo>/.hutan/recipes.toml` (not `settings.toml`, which is schema-validated settings): `[check.<name>] run, cwd?, timeout_secs, required?`; daemon `checks.list|run` starting a **tracked terminal session** in the task's checkout (existing machinery: bounded output, process tree, kill), recording `check_runs` (exit, duration, HEAD sha, dirty flag, ≤ 64 KiB log tail) linked to the task; Changes view and Tasks panel show the last result next to the delivery state; optional merge gate for `required` checks; MCP `checks_list|checks_run` (Mutate → `control` approval for agents); a **trust prompt keyed to the file's hash** before the first run (a repo file must not execute commands unasked); no env copying (B-1/vault spike). UI-check recipes (UC-1) can be one more check kind later.
- **Effort:** M. **Risks:** executing repo-controlled commands; flaky checks blocking merges (gate off by default). **Decision:** D3 (file format; auto-run after a turn or on request only).

### 6. PV-1 — Listening ports and preview recipes
- **Problem.** The human and the agent both need "where is the app running?"; UX audit row 14 (`2026-10-17-ux-audit.md:145`) is unscheduled. **Evidence:** Superset detects ports per workspace and labels them from `.superset/ports.json` (<https://docs.superset.sh/ports>); cmux shows ports in the sidebar (<https://github.com/manaflow-ai/cmux>); Claude's `launch.json` dev servers (<https://code.claude.com/docs/en/desktop#preview-your-app>).
- **Scope.** Add listening sockets to the resource manager's sampled process trees (libproc; sampled only while watched, `docs/resources.md:17-19`): additive field + capability; Resources panel column and a sidebar chip per session; click opens the URL externally (in-app browser stays M14); `[preview.<name>] run, port|url` in `recipes.toml` starting a labelled session; MCP `ports_list` so an agent knows what to point Cua/Chrome DevTools MCP/Playwright at. Killing "by port" reuses `process.signal` (always asks).
- **Effort:** M (list S, recipes S). **Risks:** same-uid libproc visibility **[INFERENCE: no extra TCC]**; port reuse between worktrees (no allocator — Superset also leaves that to scripts). **Decision:** D3 (shared file).

### 7. GH-1 — PR, CI and review threads
- **Problem.** The loop after "merged locally" lives in the browser (UX audit row 15, `2026-10-17-ux-audit.md:146`). **Evidence:** Claude Desktop CI status, Auto-fix, Merge when ready (<https://code.claude.com/docs/en/desktop#monitor-pull-request-status>); Conductor Checks; Cursor auto-fixes CI and subscribes to PR events (<https://cursor.com/docs/cloud-agent/capabilities.md>); cmux sidebar PR status.
- **Scope.** Daemon shell-out to the user's `gh` (their auth; Hutan never reads tokens): read-only first — PR for the branch, check runs, unresolved threads; "send failing log / thread to the agent" through the existing review-comment message path; **creating a PR and pushing are explicit Mutate + always-ask** requests; polling only while the panel is visible and ≥ 60 s (vision: idle CPU ≈ 0, `docs/vision.md:86`); no webhooks, no hosted relay.
- **Effort:** L. **Risks:** reverses the documented stance that Hutan never fetches or pushes (`docs/orchestration.md:178`) → ADR; rate limits; enterprise SSO. **Decision:** D4.

### 8. MP-1 — Vetted MCP presets with honest disclosure
- **Problem.** The MCP settings page is an "informational file-only island" (`2026-10-18-user-test-4.md`, R4-8) and the useful servers collect telemetry by default: Cua Driver (observed), Chrome DevTools MCP (`--no-usage-statistics`), XcodeBuildMCP (Sentry). **Scope.** A small data file of presets (cua-driver bounded, chrome-devtools-mcp, playwright-mcp, XcodeBuildMCP): command, **pinned** version, what it can reach, telemetry default and the switch that turns it off, permission notes, last-verified date; one click writes `[mcp.<name>]` through `daemon.settings.set` (user-only). Hutan downloads nothing. **Effort:** S. **Risks:** catalog rot (dated entries), unpinned `npx @latest` supply chain. **Decision:** none.

### 9. UG-1 — Local usage and cost page
- **Problem.** Users running several paid harnesses want to see consumption. **Evidence:** Superset Usage page (<https://docs.superset.sh/usage>) — we take only the *local-logs* half; reading quota endpoints with stored credentials is forbidden here (ADR 0008/0015). Hutan has the numbers: `usage` events, agent-node `usage{input,output,cache_*,cost_usd}` (`docs/protocol.md:316`), the history index over external transcripts.
- **Scope.** Fold into the traces page (TR-1/TR-2 in `2026-10-20-laminar.md` §10) rather than a second store: per day / agent / project / model tokens and *estimated* cost (labelled as list-price estimate, subscription use is not billed per token); no provider quota. **Effort:** M after TR-1. **Risk:** misleading cost numbers. **Decision:** none beyond the traces decisions already open.

### 10. UA-1 — UI acceptance runner (option c)
See §4(c). **Effort:** L. **Decision:** D5.

### Considered and deferred
- **Pull the embedded browser (M14) forward so agents can verify web apps.** Cua's browser tools (isolated Chromium profile, origins allow-list) and Chrome DevTools/Playwright MCP already serve the agent; an in-app WKWebView in GPUI is an L-sized spike (`docs/roadmap.md:155-156`). Keep M14 for the human (preview, links, Design-mode-style element pick, which Superset and Claude ship).
- **LSP diagnostics feed to agents.** Evidence is mixed (OpenCode's own warning). M11 will supervise LSP servers anyway; exposing diagnostics as an opt-in MCP tool is cheap *after* M11. CK-1 covers the common need.
- **Send-after-turn queue / steering** (Zed, Claude): real daily friction, already logged (`2026-10-17-ux-audit.md:142`), orthogonal to this report.
- **Design mode** (element → agent with DOM/styles): needs M14.
- **Subscriptions/event wake-ups** (Cursor): needs hosted event sources; schedules are covered in the meta-harness spike.

## 6. Invariant check

| Invariant (`AGENTS.md`) | UC-1 | AR-1 | CK-1 | GH-1 |
|---|---|---|---|---|
| Daemon authoritative, clients render | supervisor, manifest, approvals in daemon; desktop shows chip/card | store in daemon | runs in daemon | `gh` in daemon |
| Permissions enforced in daemon | approval + Cua bounded mode (defence in depth) | read bounds in daemon | `control` approval for agent runs | always-ask for push/PR |
| Secrets only in Keychain via daemon | no secrets; screenshots may contain some (disclosed, not indexed) | same | no env copying | user's `gh` auth, never read |
| Subscription logins only via harnesses | n/a | n/a | n/a | n/a |
| Bounded everything | TTL, idle timeout, artifact caps, one lease | caps + sweep | log tail cap, timeouts | poll only when visible |
| Protocol additive = minor + capability | `uicheck` | `chat.artifacts` | `checks` | `scm` (name TBD) |
| Hard-to-reverse → ADR | **ADR required** | in the UC ADR or amend ADR on rich content | recipe format ADR (small) | **ADR required** |

## 7. Decisions for the user (five) and recommended first slice

1. **D1 — Trust model for desktop control.** (A, recommended) per-session *bounded* driver, one approved app per request, no desktop capture, always-ask, with a warning when a `standard`-mode Cua daemon is also running; (B) additionally *refuse* to start UI checks while a standard daemon runs; (C) no desktop control, browser-only; (D) status quo (`[mcp.cua]` standard mode, ungated). Also: is Cua's default-on telemetry acceptable to leave to the user's own setting (recommended) or must MP-1 steer to `telemetry disable`?
2. **D2 — Evidence retention.** Default for screenshots/recordings: keep per task until the task is archived, 7 days, or off unless `record=true`? Never indexed is assumed.
3. **D3 — One project recipe file.** `.hutan/recipes.toml` with `[check.*]` and `[preview.*]`; run only on request (recommended) or automatically after each agent turn like Claude's auto-verify?
4. **D4 — Network actions via `gh`.** May Hutan perform explicit, always-ask PR/push actions through the user's `gh` (needs an ADR amending the "never fetches/pushes" stance), or stay read-only forever?
5. **D5 — Quality investments.** Accept AX-1 as a standing acceptance condition for new controls (recommended), and decide whether UA-1 (L) is worth building before real-window checks keep piling up in `docs/status.md`.

**Recommended first slice: UC-0 + UC-1 for a single approved app, evidence on disk, behind capability `uicheck` and `[uicheck] enabled=false`; UT-1 in parallel.** Acceptance sketch: with a bounded driver started by `hutand`, an OMP chat in a temp `HUTAN_HOME` can open the test bundle, read a named element via `get_window_state`, click it, and `verify_state`; a call outside the approved bundle id returns `bounded_resource_outside_manifest`; the approval shows in `hutan approvals` and the audit table; stopping the session revokes the driver and removes its socket; no process of the driver remains (`hutan ps`). If UC-0 shows that Hutan's own AX tree is empty (expected) the slice still ships — AX-1 is then the next slice, and the demo target is a Tauri/Electron/SwiftUI sample rather than Hutan.

Gate note: the default gate must not need a display, TCC grants or a paid model (`AGENTS.md` conventions) — daemon tests use a fake `cua-driver` script (manifest rendering, socket lifecycle, approval/audit, TTL, refusal outside scope) and the recorded ACP fixtures (`crates/agents/tests/fixtures/acp-omp-18.11/`); the real-driver run against a sample app is a manual step with the user present, recorded in the slice report.

## 8. Not verified / how to close the gap

- Hutan's actual AX tree (UC-0: `get_window_state` on a test bundle — needs an unlocked display and a driver action, which this spike was not allowed to run).
- Whether Cua background delivery works against GPUI windows; per-session bounded daemon cost; clean lifecycle commands; manifest effect on `tools/list`.
- Whether `launch_app` by bundle id finds a freshly built bundle that lives under `target/` rather than `/Applications` **[INFERENCE: depends on LaunchServices registration]**; the earlier rounds did launch a "test bundle" by bundle id (`2026-10-18-user-test-3.md:24`).
- Factory, and any competitor feature not on the pages cited above.
- Warp Code Review page was seen as a search snippet; the rest of Warp's agent features were not read.
- The merge state of PR #56065 upstream: the pinned rev contains the code, which is what matters here; its GitHub state reads "closed".
