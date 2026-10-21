# Performance budgets: method, results, fixes

Budgets are the ones in `docs/vision.md` § Budgets (unchanged here: a miss is reported, never
adjusted away). Numbers below are from `cargo run -p xtask --release -- bench` (`just bench`),
median of 3 runs, release binaries, on:

| | |
|---|---|
| Machine | Apple M4 (MacBook Air, 24 GB), macOS 26.6.2, `rustc 1.98.0` |
| Displays | built-in 60 Hz Liquid Retina, external 100 Hz; the window opens on whichever macOS picks |
| Session | normal desktop session; the bench window is on screen and active (`b4_window_active = 1` in every B4 run) |

## Results

Report `target/bench/1792001491.json` (commit `8a1de1d`, ADR 0013 store; before = the M2-F table, same harness).

| # | Metric | Budget | Median of 3 | Runs | Before | Result |
|---|---|---|---|---|---|---|
| B1 | Idle desktop RSS, 1 window, 4 terminals | ≤ 150 MB | **110.9 MB** | 110.5, 110.9, 111.8 | 102.7 MB | pass |
| B2 | Idle daemon RSS, 10 sessions × 10k-line scrollback, 80 columns, desktop attached | ≤ 120 MB | **40.1 MB** | 40.1, 39.8, 40.1 | 254.6 MB | pass |
| B2w | Same at 200 columns (~190-column lines), no client | ≤ 150 MB | **84.6 MB** | 84.6, 84.6, 84.9 | 538 MB (measured with the old store, B2w did not exist) | pass |
| B3 | Idle CPU, daemon + desktop, 30 s | ≤ 0.5 % | **0.48 %** | 0.48, 0.43, 0.58 | 0.46 % | pass, at the limit |
| B4a | Hutan path: key handled → frame with the echo submitted, p95 over 500 keys | ≤ 2 ms | **1.34 ms** | 1.17, 1.35, 1.34 | n/a (new) | pass |
| B4b | On screen, p95 over 500 keys | ≤ 1 frame + 2 ms = 18.7 ms (60 Hz, read from the platform) | **18.50 ms** | 106.9*, 18.5, 18.0 | 18.0 ms (vs 16 ms) | pass, marginal (below) |
| B5 | 200 MB+ flood: p95 key latency in a second terminal | ≤ 50 ms | **17.25 ms** | 17.25, 17.27, 17.21 | 16.9 ms | pass |
| B5 | 200 MB+ flood: daemon RSS growth | ≤ 50 MB | **2.28 MB** | 2.30, 2.25, 2.28 | 13.6 MB | pass |
| B6 | Cold start → first terminal paint with its prompt | ≤ 500 ms | **196 ms** | 179, 199, 196 | 213 ms | pass |

\* the window was not being presented at the display rate during that run (see B4b below); the median is not affected, and the harness now repeats such runs (and fails instead of reporting them).

A second complete run (`1792002157.json`) agrees on everything except B4b: B2 40.0 MB, B2w 84.9 MB, B4a 1.33 ms, B5 17.3 ms / 2.3 MB, B6 181 ms, B1 108.5 MB; B4b gave 18.90 ms and 18.59 ms in its two valid runs, i.e. a median **above** 18.7 ms (FAIL by 0.2 ms; the third run was spoiled and repeated 3 times, see below). B3 read 0.00 % there because the window was occluded. **B4b therefore passes or misses by a few tenths of a millisecond depending on the run**; it was not possible to produce a clean third full report (the session locked, `gpui_macos: could not create display link, code: -6661`).

Other numbers behind the table (medians): B1 physical footprint 68.8 MB; daemon with the 10
sessions before the desktop attached 208 MB; B3 desktop 0.46 %, daemon 0.00 %; B4 median latency
10.1 ms; B5 median 9.0 ms, 245 keystrokes measured while the flood ran (12.2 s); B6 timeline
(ms after spawn): `main` 15, window opened 142, first frame 159, connected 171, first terminal text
painted 213.

`xtask bench` exits non-zero while a budget is missed. The gate (`just check`) does not run it.

## Desktop surfaces: B3p, B7–B10 (slice P-1)

Budgets from the P-1 brief; scenarios in `xtask/src/bench_ui.rs`, driven by the window itself
(`HUTAN_BENCH_UI`, `crates/desktop/src/bench_ui.rs`) and timed by `crates/desktop/src/frames.rs`.
Medians of 3 runs (`just bench --only B3,B7,B8,B9,B10`), release, same Mac as above, display
60 Hz (16.67 ms, limit one frame + 2 ms = 18.67 ms). **Load:** a second agent tested the app and
other agents built and ran tests at the same time (1-minute load average 2.4–15 during the
runs); the harness repeats a run whose window was covered or not presented. Before = this
harness on the code without the fixes below; after = with them.

| # | Metric | Budget | Before | After | Result |
|---|---|---|---|---|---|
| B3 | Idle CPU, default layout, 30 s (see the setup note) | ≤ 0.5 % | 0.72 % (0.72, 0.75, 0.71) | 0.70 % (0.70, 0.68, 0.70) | **miss: platform floor** (below) |
| B3p | Idle CPU, 20 projects, 3 workspaces, 56 terminals + 4 chats, 6 tasks; sidebar, Tasks, Resources, Agents open | ≤ 0.5 % | 0.76 % (0.84, 0.75, 0.76) | 0.82 % (0.78, 0.82, 0.83) | **miss: platform floor** |
| B7a | Chat of 5 603 journal events (markdown, code, command and patch cards): open → first paint | ≤ 150 ms | 44.7 ms (38.6, 55.2, 44.7) | 54.0 ms (55.4, 44.6, 54.0) | pass (spread is load) |
| B7b | Same: scroll to the top (all pages load) and back, frame interval p95 | ≤ 18.7 ms | 19.69 ms | 19.89 ms | **miss by 1.2 ms** (below) |
| B7c | Same: desktop RSS growth | ≤ 60 MB | 10.8 MB | 15.1 MB | pass |
| B8a | Reply streamed at 200 deltas/s for 24 s, composer typed into: frame interval p95 | ≤ 18.7 ms | 18.43 ms | 18.46 ms | pass |
| B8b | Same: longest frame | ≤ 50 ms | 21.5 ms | 20.9 ms | pass |
| B8c | Same: key → frame p95 (handlers + frame work) | ≤ 16 ms | 8.2 ms | 8.4 ms | pass |
| B9a | 300 sessions / 40 projects: filter key p95 (handlers + frame work) | ≤ 16 ms | **54.1 ms** (64.8, 54.1, 39.8; p50 32.5) | **11.9 ms** (11.9, 16.5, 11.8; p50 7.3) | fixed (1 run in 3 at 16.5) |
| B9b | Same: drag a project onto a workspace, frame interval p95 | ≤ 18.7 ms | **20.96 ms** (frame work p50 18.2 ms) | **18.81 ms** (frame work p50 3.0 ms) | 0.1 ms over: display floor |
| B10 | ⌘1…⌘9 and ⌘P jumps over 10 terminals and 5 chats: key → first frame of the new item, p95 | ≤ 50 ms | 29.5 ms | 26.7 ms | pass |

"Frame" = frame interval: the time between the ends of two consecutive frames (the paint of the
last element of the window root), recorded for every frame while a phase runs. Frames are
drawn at the display tick, so the p95 of the interval is one display period plus the spread of the
frame's own work and of when the tick arrives; a window that does ≤ 5 ms of work per frame
still measures 18.4–18.5 ms (B4b above, B8a, B9b). That is the floor on this Mac under this load:
B7b, B8a and B9b sit on it, and B7b is 1.2 ms above (its frame work p50/p95 is 3.3/5.4 ms, which
makes the interval p95 ≈ 16.7 + 2 + noise). "Key → frame" counts the handlers plus the frame's own
work, not the wait for the display tick (B4a's definition); the total including the wait is in the
report as `*_key_total_*`.

### What was measured and what was fixed

1. **Sidebar (B9a, B9b): rebuilt and laid out every row on every frame.** `sample(1)` of the
   typing phase: 108 of 167 draw samples in taffy layout of ~300 session rows, plus a clone of
   every `SessionInfo` into new entries in each `render`, every frame (a frame cost 18–48 ms
   with 300 sessions). Fix (`panels/sessions.rs`): the entries are cached and rebuilt only when
   the store notified, the filter, the folds or the settings changed (`LayoutKey`); the rows
   are a `gpui::list` so only the ones in view are built, and `sync_list` splices only the rows
   that changed so the reader keeps their place. Frame work in the drag: p50 18.2 → 3.0 ms;
   filter key p50 32.5 → 7.3 ms. Tests (`tests/it/perf_gpui.rs`):
   `the_sidebar_builds_only_the_rows_in_view` (a frame builds ≤ 40 rows of 150 sessions),
   `a_frame_does_not_lay_the_sidebar_out_again` (no layout without a change, one per filter key).
   The after numbers were taken before the last two touch-ups of the list (a full-width wrapper
   per row; layout probes record their bounds when painted, because a list measures items off
   to the side first); they change no work per frame. A placement badge wider than the dock
   overflows the row (it already did); the list clips to its width, so `tasks_gpui` clicks the
   visible part of the badge.
2. **Hidden timers.** The sessions and history panels redrew the window every 30 s for relative
   ages even when no one could see them; now the tick only notifies a panel that was drawn since
   the last tick (as the Agents, Tasks and Resources panels already did; Resources
   unsubscribes from the daemon while hidden, `resources_gpui`).
3. **Checked, nothing to fix:** the chat transcript is already a `gpui::list` with markdown
   cached per entry and revision (B7: first paint 45–54 ms for 5 603 events, 10–15 MB growth;
   B8: frame work p95 4.6 ms while streaming). Tests: `a_long_transcript_builds_only_the_cards_in_view`
   (300 messages, a frame builds ≤ 40 cards), `markdown_is_parsed_once_per_message_revision`
   (frames that change nothing parse nothing). Image thumbnails of composer chips go through GPUI's
   image asset cache (`img(path)` / `img(Arc<Image>)`), decoded once, not per frame
   [INFERENCE from the source: no benchmark has image chips].

### Misses and follow-ups

- **B3, B3p: GPUI's display link, not Hutan.** With the window visible, a `CVDisplayLink` thread
  fires at 60 Hz and wakes the main thread (`display_link_output_callback` →
  `dispatch_source_merge_data`); the pinned GPUI has no on-demand path on macOS (`frame_waker` is
  `None`, `gpui_macos/src/window.rs`). `sample` of the idle desktop (B3, B3p): every busy sample is
  in that thread or in `window::step` on the main thread; no Hutan code runs. With
  `HUTAN_BENCH_IDLE_FRAMES=1` the idle window traces 0 frames in 20 s (only the two of the start),
  so no timer or notify storm keeps the window drawing. Follow-up (unchanged from B3 above):
  stop the link while no frame is wanted, in the GPUI platform layer; that is outside this slice.
  The number is ~0.2 % higher than the M2 table (0.48 %) with the same method, on a busier
  machine; B3p adds ~0.1 % (more main-thread wakeups, 19 vs 2 per second, none of them frames).
- **B7b, B9b: display floor** (above). Further reduction needs less per-frame work (`sample` of the
  scroll: ~35 % of the busy samples are taffy layout of the whole window tree) and the frames
  draw the whole window each time: proposal, wrap the dock panels, terminal views and chat view in
  `AnyView::cached` so a frame that only scrolls the transcript re-lays out only it. Not done here:
  a cached view reads window state (focus, activity) that is not an entity, so each view needs
  an audit.
- **First-run setup** (found while building the harness): a fresh `HUTAN_HOME` showed the setup sheet
  over the whole window, and it blocked the mouse (the first run is a first screen in the centre now, `docs/design.md` § First run). The B1–B6 numbers above were measured with it
  showing; B3 above and every scenario of this section seed `[app] setup_done = true` (the way
  a user runs Hutan), which is why B3 reads 0.72 % here and 0.48 % in the table above.

### Method

`HUTAN_BENCH_UI=<scenario.json>` (needs `HUTAN_TRACE_LATENCY`; scenarios `transcript`, `stream`,
`sidebar`, `switch`, `idle`) makes the window dispatch GPUI keystrokes, mouse and scroll events
and actions to itself. Operations are timed from dispatch to the end of the first frame that
shows their result (`frames.rs`: `frame` and `op` trace lines, recorded only inside a scenario's
phases). Seeding: projects are real `git init` folders, terminals run `sh`, chats run
`xtask fake-codex`, a stand-in for `codex app-server` (no model, no network, no login) that
plays 9 journal events per unit of `bench:fill` and a paced stream for `bench:stream`. The
trace of every run is kept next to the report (`target/bench/trace-*.jsonl`); `--sample` samples
the desktop during B3p, the sidebar phase and the transcript scroll. A run whose window was
covered, inactive or not presented is repeated.

## Method

`cargo run -p xtask --release -- bench [--only B1,B4] [--runs N] [--json] [--sample]
[--key-interval-ms N]` builds `hutand`, `hutan-desktop` and `hutan` in release, then for every
scenario and run: creates a temporary `HUTAN_HOME`, starts `hutand`, seeds sessions and a
`desktop-state.json`, launches the desktop (the window appears, no screenshots), measures, and
tears down. The report goes to `target/bench/<unix-time>.json` (all runs, every stage metric).

- **B2w.** Same load as B2 (10 sessions × 12 000 coloured lines, 10 000-line scrollback) in 200×50
  terminals with ~190-column lines, no client attached; daemon RSS after the sessions settle.
- **B1/B2/B3 (one scenario).** 10 sessions each print 12 000 coloured 80-column lines (the
  10 000-line scrollback overflows) and idle in `cat`. Daemon RSS is sampled; the desktop opens a
  seeded 2×2 layout of 4 of them; after 10 s idle the desktop and daemon are sampled
  (`proc_pid_rusage`: resident size, physical footprint, user+system CPU time, idle wakeups), and
  again after 30 s. B3 is the CPU time delta of both processes over those 30 s.
- **B4a / B4b.** One `cat`-like session (`printf 'bench> '; exec cat`) in the only pane. The desktop types
  500 keys at one key per 100 ms plus a deterministic 0–17 ms jitter (`HUTAN_BENCH_INPUT`, below).
  Latency is the in-app trace: key event handled → first paint that follows the daemon frame
  replacing the cursor row or moving the cursor. **B4b** is that whole latency. **B4a** is the same
  minus the wait for the display tick (`echo_to_prepaint`): `key_to_sent + sent_to_echo +
  (echo_to_paint − echo_to_prepaint)`, i.e. key handled → the frame with the echo is painted
  (submitted to the renderer). B4b's limit is one display frame + 2 ms (ADR 0011): the desktop
  reports `Window::refresh_interval` (GPUI's cached `maximumFramesPerSecond` of the screen the
  window is on) in the `bench_start`/`bench_done` events, the report records it
  (`b4_refresh_interval_ms`, `b4_refresh_hz`; 60 Hz = 16.67 ms on this run) and the run fails when it
  is missing or changes during the run. A run whose window was inactive or not presented at the
  display rate (median wait for the display tick > 3 frames: a locked, sleeping or occluded
  display) is repeated up to 4 times and then fails the bench; `caffeinate -d -i` keeps the display
  awake while the bench runs. The jitter is needed: a strictly periodic injector
  locks onto one phase of the display period and measures that phase for the whole run.
- **B5.** Two panes: a typing session and a flooded one. The flood is six back-to-back
  `head -c 150000000 /dev/urandom | base64` (about 1.2 GB; a single 203 MB round takes only ~2 s
  and is too short to type through). 600 keys at 20 keys/s (+ jitter) are typed into the first
  pane; latency is taken only for keys that were painted while the flood ran. Daemon growth is
  peak RSS minus RSS before the flood.
- **B6.** Daemon running with 4 prompt sessions; `hutan-desktop` is spawned with the seeded
  layout. Cold start is the wall clock from just before the spawn to the end of the first paint of
  a terminal whose snapshot has text (the prompt).

**Instrumentation** (no cost when off: one atomic load):

- `HUTAN_TRACE_LATENCY=1` and `HUTAN_TRACE_FILE=<path>` enable JSON-lines tracing
  (`terminal_view::trace`): `startup` events (`main`, `window_opened`, `first_frame`, `connected`,
  `first_terminal_painted`), `key` events with the stages `key_to_sent`, `sent_to_echo`,
  `echo_to_paint` (and its first part `echo_to_prepaint`: waiting for the display tick), and once a
  second a `paints` count.
- `HUTAN_BENCH_INPUT=<script.json>` types `{"interval_ms", "start_delay_ms", "jitter_ms",
  "keys": ["a", "enter", …]}` through `Window::dispatch_keystroke`: the real dispatch tree, the
  terminal view's key handler, key encoding and `TerminalHandle::input`. It starts after the first
  terminal painted text.
- `--sample` runs `sample(1)` on the desktop and daemon during B3/B4 (`target/bench/sample-*.txt`).
  Symbolised profiles come from `CARGO_PROFILE_RELEASE_STRIP=none CARGO_PROFILE_RELEASE_DEBUG=1`
  builds.

**Teardown is graceful and checked.** The desktop is stopped with SIGTERM (it saves its layout and
exits 0, leaving the daemon running); the daemon with `hutan daemon stop --stop-sessions`. The run
fails if either had to be SIGKILLed, died of a signal, or left a process behind (descendants of the
daemon included). Verified across three full runs: no stray `hutand`/`hutan-desktop`, no new
`hutan-desktop-*.ips` crash report.

## Fixes (before → after, same harness)

1. **Idle CPU: cursor blink (B3).** Before: 1.58 % (1.57, 1.60, 1.58), ~30 wakeups/s. A `sample`
   of the idle desktop attributed it to whole-window repaints (≈4.5 ms each) twice a second for
   the focused cursor, plus one 530 ms timer per terminal view, focused or not. After: the timer
   exists only while the focused view's cursor blinks and stops ~5 s after the last activity
   (typing, output, focus), leaving a solid cursor (Alacritty's `blinking_timeout` behaviour);
   unfocused views schedule nothing. **0.46 %** (0.44, 0.52, 0.46). Test: the cursor stops blinking
   when idle and typing resumes it.
2. **Typing under load: ack when painted (B4/B5 sensitivity).** The terminal view acked a frame
   one display tick after painting it (a `next_frame` callback), so after every paint the daemon
   (credit 1, ADR 0004) had no credit for a whole frame and echoes of fast typing queued behind the
   ack. Now the ack goes out with the paint; frames are still painted at most once per tick, so a
   flood is still paced by the display, and daemon memory stays bounded (RSS growth unchanged,
   13.5 MB).

   | scenario | before p95 (median of 3) | after p95 |
   |---|---|---|
   | B4 at 33 keys/s (`--key-interval-ms 30`) | 32.3 ms (p50 22.1) | **18.4 ms** (p50 11.2) |
   | B5 at 40 keys/s (`--key-interval-ms 25`) | 61.2 ms (runs 61.3, 32.0, 61.2) | **17.2 ms** |
   | B4 at 10 keys/s (default) | 18.8 ms | 18.0 ms (no real change) |
   | B5 at 20 keys/s (default) | 16.6 ms | 16.9 ms (no real change) |

   Test: `frames_are_acked_when_they_are_painted`.
3. **Tried, no measurable effect, reverted:** skipping the redraw that every keystroke caused
   before the echo arrives (B4 at 10 keys/s 18.8 → 18.3 ms, within noise; at 33 keys/s unchanged
   at 32 ms until fix 2).

## Misses

### B2: fixed by the compact scrollback store (ADR 0013)

Root cause (unchanged, measured earlier): `alacritty_terminal` stores every cell in 24 bytes, so a
10 000-line history is 19.2 MB per session at 80 columns and 48 MB at 200. Fix: the engine keeps
the viewport plus 256 history rows; older rows are spilled after every 2 KiB slice of output into a
per-terminal packed store (text bytes, row index, style spans keyed by byte offset). Result: B2
254.6 → 40.1 MB, B2w 84.6 MB (it was 538 MB with the old store). `b2_daemon_rss_detached_mb`
(before the desktop attached) 37.8 MB; attaching no longer reflows the history, so the desktop adds
only ~2 MB.

What was tried and rejected: spilling only after output pauses (cheaper during floods, but the
engine then holds its whole history during every burst and the process keeps that memory resident:
B2 218.9 MB, B2w 538 MB). macOS does not return freed small allocations to the resident set
(`malloc_zone_pressure_relief` released nothing in a direct test), so the peak, not the idle
live size, decides. Throughput of the mixed 200 MB `flood` example: 138 MB/s with the store vs
154 MB/s before (release; peak RSS 14.9 vs 26.2 MiB).

### B4: split by ADR 0011

Where the time goes (medians over the three runs): key event → bytes sent 0.02 ms; sent → echo
applied 0.3–0.5 ms; applied → start of the paint 9–10 ms (waiting for the display tick); paint
≈ 0.5 ms. B4a (Hutan path) is **1.3 ms p95**, well inside 2 ms. B4b is the display-tick wait on
top: p95 ≈ one frame (16.7 ms) + ~1.3–2.2 ms, against a limit of one frame + 2 ms, so it sits on
the limit: 18.0, 18.5, 18.6, 18.9 ms in the valid runs of two reports. Reducing it further needs
the on-demand present path (roadmap backlog), not Hutan-side work.

### B3: passes, but at the limit

The remaining 0.46 % is not Hutan code: with the window visible, GPUI at the pinned revision keeps
a `CVDisplayLink` running and wakes the main thread at every display tick (~18–30 wakeups/s) to find
nothing to draw; `sample` shows all residual main-thread time under `gpui_macos::window::step` and
`gpui::window` request-frame handling, and the run-to-run spread (0.44–0.52 %) is that cost.
One earlier run measured 0.0 % with 0.03 wakeups/s [INFERENCE: the window was occluded, which
stops the link]. Follow-up: stop the link
while no frame is wanted (the `frame_waker` hook other platforms use) in the GPUI platform layer.
The sessions panel's relative-age refresh (every 30 s) is the only timer Hutan keeps while idle.

## P-3: narrow invalidation, cached views, faster flood

Brief `docs/briefs/P-3.md`; baseline `docs/spikes/2026-10-18-perf-lag.md`.

### What changed

- **Cached view boundaries.** Dock panels, pane items (terminal, chat, changes view) are drawn through
  `AnyView::cached` (`render.rs::cached_view`): GPUI replays their last frame unless their own entity
  notified. The probes (`Region`) of a cached view are owned by it (`workbench/regions.rs`): kept while it
  is replayed, replaced when it paints, dropped when it leaves the tree; probes in deferred draws (menus)
  find their owner through `Window::current_view`. Theme, density and settings changes notify every cached
  view (`Workbench::invalidate_cached_views`). A cached view created in the frame it first renders is not
  followed by GPUI (its entity is not in the accessed set of that render), so a new one asks for one more
  frame (`Regions::take_fresh`).
- **Narrow invalidation.** `SessionsStore` emits `StoreChanged` (connection, sessions, session info,
  workspaces, agents, attention, tasks, active, focus, other) with every notification and does not notify
  when the daemon's state is unchanged. The sessions panel rebuilds its entries only for what it lists,
  History only reacts to connection, scope, workspaces and the session set, the workbench only runs
  `on_sessions_changed` for layout-relevant kinds; terminals are told only about their own session.
  Tests: `perf_gpui::a_store_notification_that_changes_nothing_they_show_renders_no_panel` (render counters
  `SessionsPanel::renders` / `HistoryPanel::renders`).
- **History rows.** No `SessionItem` clone per row; title, meta line and the age tooltip are cached per item
  and minute; home directory and `show_snippets` are read once per frame instead of per row.
- **Emulator flood path** (`crates/terminal`): one state-aware scanner (`sniffer::StreamScanner`) replaces the
  OSC sniffer, the `CSI 16 t` scanner and the history-clear scanner (three passes), skipping the ground state
  eight bytes at a time; whole OSC 7 / OSC 133 strings are not handed to vte (its `unhandled` formats a
  `String` for every one, also with logging off) while their marks keep their exact stream position;
  the blank tail of a row is found with a field-wise compare; the viewport is converted to protocol rows once
  per generation (every attached client asked again); `Block` events are produced from the marks
  as before. Tests: `sniffer::tests::one_pass_equals_the_three_scanners_on_random_streams_and_chunkings`
  (60 random streams, random chunking, against the old scanners kept verbatim in `sniffer/reference.rs`),
  skip-range tests, `render::tests` (blank scan at every width and position), `equivalence::a_huge_read_…`
  (a 60 000-line read at once, in 2 KiB reads and against the full-history engine: identical rows, line bound
  and numbering) and the existing cold-store equivalence suite.

Tried and rejected: settling the scrollback once per read instead of per 2 KiB slice (letting the grid grow
for several slices) measured 2–5× *slower* on a `yes` flood (spilled rows are no longer in cache when they
are converted), so the settle policy is unchanged: per slice that scrolled, once at the end of a read.

### Emulator flood microbench

`cargo run --release -p hutan-terminal --example corpora [-- <MB>]`: engine path only (no PTY), 120×40,
10 000 lines of scrollback, 64 KiB reads, **MB per CPU second of the process** (not wall time: the machine
was shared), best of 3, 12 MB corpora, quiet machine, three alternating runs each:

| Corpus | Before | After | CPU for the same bytes |
|---|---:|---:|---:|
| `yes` (one-cell rows spilled) | 19.7 MB/s | 21.7–21.8 MB/s | −9.5 % |
| colored numbered lines | 104–106 MB/s | 122 MB/s | −14 % |
| prompt markers (OSC 7 / OSC 133 around every command) | 34.2–34.6 MB/s | 58.9–59.4 MB/s | −42 % |

The brief's target (daemon flood CPU −20 % at equal throughput) is **met for the marker corpus only**:
the `yes` flood stays dominated by converting one 120-cell row per line for the cold store
(`RowScratch::fill`, still ~85 % of reader samples after the new scan); going further needs the engine to
expose its occupied-cell count per row (`Row::occ` is `pub(crate)` in `alacritty_terminal` 0.26) or a
row-level spill that does not look at cells at all. Not done here.

### What could not be measured

`just bench` B3, B5, B7 and B9 were **not re-run**: during this slice the console session was locked
(`IOConsoleLocked = Yes` since the afternoon; the bench's desktop draws two frames and then no more, so
`bench_start` times out, as in the earlier `-6661` display-link failures above). The baseline in
`docs/spikes/2026-10-18-perf-lag.md` (B5 p95 55.6 ms, B9 filter incl. display p95 22.5 ms, B7 scroll work
p95 5.1 ms) is therefore **not** compared against an after number, and none of the targets (B5 ≤ 25 ms,
B9 ≤ 18 ms, no History/terminal layout in sidebar interaction samples) is claimed. What is verified instead
is deterministic: a store notification that changes nothing the sessions panel or History shows renders
neither (render counters), a layout change re-renders both, and every cached view keeps its probe regions
(the full workspace suite passes), and `perf_gpui::output_arriving_in_a_visible_cached_terminal_repaints_it`
counts the paints of a terminal in a cached pane (`TerminalView::paints`) while output arrives. To measure: unlock the session and run
`just bench --only B3`, then `B5`, `B7`, `B9` one at a time under `caffeinate -d -u -i`.

The presentation questions of the brief (item 5: GPUI's inactive-window throttling at
`gpui/src/window.rs:1813-1847`, a terminal that receives output requesting one frame) were **not**
investigated beyond reading that the throttle only applies to inactive windows without high-rate input.

### Idle and the display link (GPUI patch, out of scope)

The visible-idle floor (B3) is GPUI's `CVDisplayLink`, which stays subscribed while the window is visible
(`gpui_macos/src/window.rs:836–855`, `display_link.rs:328–343`). The change needed: keep the link stopped
while no frame is wanted, start it when `Window::request_frame`/`invalidator.wake_platform()`/`schedule_frame`
asks for one (what the `platform_waker` hook already signals), and stop it from
`display_link_output_callback` after a tick whose draw had nothing dirty and no next-frame callbacks. The
timers Hutan keeps are unchanged and gated on being drawn (sessions ages, History refresh, Agents ticker).

## Crash found while building the harness

Four `hutan-desktop` crash reports (SIGABRT, main thread) appeared while the harness was being
developed. Root cause: the first version of the bench key injector dispatched keystrokes from
inside a `Workbench` update (`WeakEntity::update_in`); the terminal view's key handling updates
the workbench again (focus, commands), and GPUI panics on re-entrant entity updates
(`cannot update … Workbench while it is already being updated`). The panic happened inside an
AppKit callback, which cannot unwind, so the process aborted and macOS showed its dialog.
Fixed by dispatching through the window context (`AsyncWindowContext::update`) instead; the
regression test (`scripted_keys_reach_the_focused_terminal_without_reentering_the_workbench`) fails
with exactly that panic message when the dispatch is moved back inside the update.
`hutan-desktop` now also installs a panic hook that appends the message, location and backtrace to
`$HUTAN_HOME/logs/desktop.log` (`~/Library/Logs/Hutan/desktop.log` by default, next to
`hutand.log`; one path function, `crash::log_dir`) before the process dies.

## Editor: rope choice and the text model (slice E-1, `hutan-buffer`)

The editor budgets are B11–B15 in `docs/spikes/2026-10-21-editor.md` § 4.8. E-1 measures only the
text-model share of them (no GPUI, no file I/O); the first end-to-end numbers come with the
viewer (E-5). Nothing here is a budget change.

| | |
|---|---|
| Machine | Apple M1 Pro (10 cores, 16 GB), macOS 27.0.1, `rustc 1.98.0`; other agents were building on the machine (load average 6–12), so absolute numbers are pessimistic |
| Build | `cargo bench` profile (release: thin LTO, `codegen-units = 1`) |
| Method | `crates/buffer/benches/rope.rs` and `buffer.rs` (no harness crate): `Instant`, median of 5–9 runs per cell, medians of 3 invocations below. Text: source-like lines of ~40 bytes, every 7th with 2-, 3- and 4-byte characters (so UTF-16 surrogate pairs occur). Run `cargo bench -p hutan-buffer --bench rope` / `--bench buffer` |

### Rope: `crop` 0.4.3 vs `ropey` 1.6.1 (decision: `crop`)

`ropey` 2.0 is still `2.0.0-beta.1` (not released), so 1.6.1 is the comparison. `crop` indexes by
byte and has a UTF-16 metric; `ropey` 1.x indexes by `char`, so its rows include the
`byte_to_char` / `char_to_byte` conversion every byte-based caller (tree-sitter, the buffer API)
pays. Every row is per operation except "open".

| Scenario | crop 1 MB | ropey 1 MB | crop 10 MB | ropey 10 MB |
|---|---|---|---|---|
| open (build from `&str`) | **0.134 ms** | 0.321 ms | **1.32 ms** | 3.43 ms |
| snapshot (clone) | 10 ns | 10 ns | 10 ns | 10 ns |
| random insert (line start) | 352 ns | 359 ns | **675 ns** | 775 ns |
| random delete (≤ 8 chars) | 658 ns | 628 ns | 851 ns | 892 ns |
| typing at a cursor (1 char) | **35 ns** | 96 ns | **46 ns** | 96 ns |
| line → byte | 128 ns | 129 ns | **202 ns** | 235 ns |
| byte → line | **97 ns** | 114 ns | **154 ns** | 207 ns |
| byte → UTF-16 | **97 ns** | 179 ns | **141 ns** | 293 ns |
| UTF-16 → byte | **117 ns** | 239 ns | **189 ns** | 365 ns |

`crop` ties or wins in every row except random delete, where `ropey` is ~5 % faster (628 vs 658 ns
at 1 MB; `crop` is ahead at 10 MB, 851 vs 892 ns). It opens 2.4× faster, does the UTF-16 conversions that
GPUI's `EntityInputHandler` and LSP need about 2× faster, types at a cursor 2.7× faster, and needs
no conversion layer for byte offsets. Its costs: no `char` metric (`Text::byte_to_char` is `O(n)`,
documented; nothing on the editor's hot paths uses it) and a smaller community than `ropey`. Pinned
`=0.4.3` because `Text::utf16_to_byte` relies on how it treats a UTF-16 offset inside a surrogate
pair (rounds down; a test guards it).

### `hutan-buffer` text-model costs

| Scenario | 1 MB | 10 MB | Related budget |
|---|---|---|---|
| open: `Buffer::from_bytes` (NUL scan, UTF-8 validation, line-ending scan, rope) | 1.22 ms | 12.1 ms | B11a ≤ 100 ms, B11b ≤ 500 ms (whole open; this is the model's share) |
| open a CRLF file (extra normalization pass) | 1.86 ms | 18.0 ms | same |
| save: `to_bytes` (LF) | 0.035 ms | 0.42 ms | — |
| keystroke: `Buffer::insert` of 1 char (validate, invert, undo history, rope, 100 tracked anchors) | 0.97 µs | 1.08 µs | B12a ≤ 2 ms (whole key → frame) |
| 1 000 typed characters in one group: undo + redo | 0.001 ms | 0.001 ms | B15 ≤ 2 ms |
| snapshot for a background parse (`Text` clone) | ~10 ns | ~10 ns | B11c, B14 |
| `apply_external`, one line changed (diff included) | 1.51 ms | 15.0 ms | — |
| `apply_external`, 100 lines changed | 7.5 ms | 81 ms | — |
| search, literal absent (scans everything) | 0.048 ms | 0.29 ms | B15 (first match ≤ 50 ms) |
| search, literal, first 1 000 matches | 0.11 ms | 0.15 ms | same |
| search, regex `value_\d+7\b`, first 1 000 matches | 10.6 ms | 10.7 ms | same |
| search, `HÉLLO` ignoring case, first 1 000 matches | 0.13 ms | 0.22 ms | same |

Observations:
- The model's share of every editor budget is orders of magnitude below it; no miss, nothing to fix.
- `apply_external` is dominated by flattening the old text and the line diff (`similar`, Myers, 500 ms
  deadline after which it returns a correct but less minimal diff); common leading and trailing
  lines are skipped before the diff runs, so a one-line change costs a copy and a pass over the lines.
- The regex search runs with `regex-cursor`'s literal prefilter off: version 0.1.5 loses matches
  that straddle a rope chunk boundary inside a multi-byte character (reproduced with a 3-byte
  literal cut after its first byte; regression test `multibyte_literals_straddling_chunks_are_found`).
  Without the prefilter a pure literal still runs at multiple GB/s (table above).
- The undo history is bounded (10 000 groups, 32 MiB, 24 h by default; `HistoryLimits`), pruning is
  `O(1)` per edit.

## Editor: syntax (slice E-2, `hutan-syntax`)

The budgets are B11c/d and B14 in `docs/spikes/2026-10-21-editor.md` § 4.8. E-2 measures the syntax
share (tree-sitter parse, queries, no GPUI, no file I/O); the end-to-end numbers come with the
viewer (E-5). Nothing here changes a budget.

| | |
|---|---|
| Machine | Apple M1 Pro (10 cores, 16 GB), macOS 27.0.1, `rustc 1.98.0`; other agents were building on the machine (load average 9–10), so absolute numbers are pessimistic |
| Build | `cargo bench` profile (release: thin LTO, `codegen-units = 1`); `tree-sitter` 0.27.1 and the grammars in the pinned versions of the root `Cargo.toml` |
| Method | `crates/syntax/benches/syntax.rs` (no harness crate): `Instant`, one untimed warm-up, median of 5–51 runs. Text: 1 MB of synthetic but realistic Rust (structs, impls, `match`, closures, doc comments, strings, attributes; every item distinct), and for the language table each language's test fixture repeated to ~1 MB. Run `cargo bench -p hutan-syntax --bench syntax [-- rust\|cancel\|languages]` |

### 1 MB Rust file

| Scenario | Time | Budget |
|---|---|---|
| language load, first use in the process (grammar check + compile of the highlight, fold and indent queries) | 22.5 ms | off the calling thread: `Syntax` loads on its worker |
| **full parse**, 1 000 434 bytes (≈ 25 000 lines), background thread | **99–101 ms** | B14b ≤ 300 ms ✓ |
| incremental reparse, one character typed in the middle (worker, includes `Tree::edit`) | 2.0 ms | B14a ≤ 50 ms for edit → highlights ✓ |
| `Syntax::edit` + `Syntax::reparse` (what the caller's thread pays per key; the text snapshot is an `Arc`/rope clone) | 2.0 µs | B12a ≤ 2 ms whole key path ✓ |
| edit → new tree published (worker wake-up + parse + publish) | 2.4 ms | B14a ✓ |
| **highlights, 60 lines** (≈ 2 KB, ≈ 450 spans), start / middle / end of the file | **150 / 164 / 146 µs** | B11c, B12a ✓ |
| highlights, 200 lines in the middle | 0.51 ms | |
| highlights, 60 lines, tree one edit behind (old tree queried, spans mapped across the edit) | 177 µs | the "no flash" path while typing |
| highlights, whole file (all 25 000 lines) | 84 ms | not on a hot path |
| folds, 60 lines / whole file | 92 µs / 45 ms | |
| `node_at`, `expand_selection`, `bracket_match` | 0.3 µs, 0.4 µs, 0.6 µs | |
| `indent_level_after` (a query over the cursor's neighbourhood) | 6 µs | |
| cancellation: a newer version submitted 60 ms into the parse of a 20 MB file; time to the new tree | 5.5–8.6 ms (the 20 MB parse is abandoned, not finished) | "cancellable" in B14b ✓ |
| memory: one tree of the 1 MB file, retained | **≈ 37 MB** (RSS grew +37.0, +37.1, +37.1 MB for the 2nd–4th tree held at once, so it is the tree, not allocator slack) | B11d ≤ 40 MB with syntax: **passes with ≈ 2 MB left** (rope ≈ 1 MB + tree ≈ 37 MB) |

### Languages (the test fixture repeated to ~1 MB)

| Language | bytes | full parse | highlights, 60 lines (middle) | first-use load |
|---|---|---|---|---|
| rust | 1 000 050 | 97 ms | 108 µs | 21.5 ms |
| typescript | 1 000 350 | 98 ms | 102 µs | 24.6 ms |
| tsx | 1 000 332 | 93 ms | 125 µs | 26.7 ms |
| javascript | 1 000 076 | 99 ms | 115 µs | 9.7 ms |
| python | 1 000 206 | 125 ms | 109 µs | 6.9 ms |
| go | 1 000 150 | 130 ms | 65 µs | 2.5 ms |
| json | 1 000 036 | 80 ms | 92 µs | 0.1 ms |
| toml | 1 000 230 | 85 ms | 69 µs | 0.3 ms |
| yaml | 1 000 065 | 64 ms | 259 µs | 4.3 ms |
| markdown | 1 000 110 | 236 ms | 413 µs | 4.2 ms |
| bash | 1 000 224 | 127 ms | 64 µs | 7.2 ms |
| html | 1 000 140 | 87 ms | 56 µs | 0.2 ms |
| css | 1 000 109 | 93 ms | 57 µs | 0.6 ms |
| c | 1 000 181 | 123 ms | 77 µs | 5.6 ms |
| cpp | 1 000 377 | 124 ms | 92 µs | 40.2 ms |

Every language is under B14b's 300 ms; markdown is the slowest (the block grammar is heavy and the
inline layer is parsed per highlight request on the visible paragraphs, not for the document: that
is the 413 µs). `yaml`'s repeated fixture ends up with one recovered syntax error at the 1 MB mark
(a `tree-sitter-yaml` quirk with 2 400 documents in one stream; the single fixture parses cleanly).
The first-use load is dominated by compiling the queries (TypeScript, TSX and C++ combine the
JavaScript/C queries with their own); it happens once per language per process, on the parse
worker.

### Binary size and build time

Size of `examples/size_probe` (links every enabled grammar, parses a snippet, runs highlights and
folds), release profile (thin LTO, `strip = "symbols"`), one grammar feature at a time with
`--no-default-features`:

| Features | Binary |
|---|---|
| none (nothing reachable: the linker drops the runtime, so this is not a floor) | 0.34 MB |
| `json` (the runtime, the query machinery and the smallest grammar: **the practical floor**) | 1.72 MB |
| `html` / `toml` / `css` / `yaml` / `go` | 1.74 / 1.74 / 1.86 / 1.90 / 1.94 MB |
| `javascript` / `python` / `c` / `markdown` | 2.14 / 2.19 / 2.35 / 2.50 MB |
| `rust` / `bash` | 2.85 / 3.09 MB |
| `typescript` (TypeScript and TSX) | 4.60 MB |
| `cpp` | 5.19 MB |
| **all 14 grammar features** (15 languages) | **13.45 MB**, i.e. **+13.1 MB** over "none" and +11.7 MB over the floor |

The grammars are compiled in; there is no dynamic loading. If the bundle size matters, the
optional ones to drop first are `cpp` (+3.5 MB over the floor), `typescript` (+2.9), `bash`
(+1.4) and `rust` (+1.1); each is a cargo feature of `hutan-syntax`.

Build time, clean release build of `hutan-syntax` and everything below it (`cargo build --release
-p hutan-syntax --lib`, empty target dir, 4 jobs, load average 9): **19.6 s wall**, 63 s of CPU
summed over units, of which the 14 grammars' C compilation is 13.5 s and the tree-sitter runtime
3.8 s (the rest is `regex` and its dependencies, which the runtime pulls in for query predicates,
and `hutan-buffer`). A change to a query file rebuilds only `hutan-syntax` (1.2 s).

### Observations

- **B11d is met with little room**: ≈ 37 bytes of tree per source byte for token-dense Rust. A
  viewer that keeps trees of background tabs would exceed it quickly; E-5 should drop the tree of
  a file that is not visible (the rope stays) and re-parse on show (≈ 100 ms per MB), or lower
  `large_file_bytes` for syntax.
- Reads never wait for the parser: `Syntax::view` takes the state lock for a pointer copy and a
  copy of the edits since the last tree, and the worker holds the lock only to take a request and
  to publish. The test `reading_never_waits_for_a_parse_in_flight` holds a parse inside its text
  read while reading 200 times.
- A cancelled parse stops at its next read of the text or its next progress check (every 100
  parser operations), whichever comes first. The cancellation row measures the whole round trip:
  abandoning the 20 MB parse, parsing the replacement text and publishing it.
- Incremental parsing equals a fresh parse (same tree, same highlights) after every random edit
  whose result has no syntax errors: 200 chained edits per language in the gate, 3 000 per
  language with `SYNTAX_FUZZ_SEEDS=15`, plus batched edits and 4 × 150 unawaited edits through
  `Syntax`. Where the edited text has syntax errors the two can differ: the first version of the
  test (200 chained garbage edits, no repair) diverged in a C++ file whose tree was mostly
  `ERROR` nodes, because tree-sitter does not promise the same error recovery for an incremental
  and a fresh parse. The gate therefore compares strictly on error-free results (an edit that
  leaves errors is reverted by the next step) and only checks well-formedness on the others.

## Editor: the viewer (slice E-5, `hutan-code-view`)

Budgets B11a–d and B13 of `docs/spikes/2026-10-21-editor.md` § 4.8, end to end through the real
daemon and the release desktop (`just bench --only B11,B13`; scenarios `editor-open` and
`editor-scroll`: `HUTAN_BENCH_UI` `editor_open` / `editor_scroll` in `crates/desktop/src/bench_ui.rs`,
harness in `xtask/src/bench_ui.rs`).

| | |
|---|---|
| Machine | Apple M1 Pro (10 cores, 16 GB), macOS 27.0.1, 120 Hz display (refresh interval 8.33 ms measured by the run); other agents were building (load average 12–20), so the numbers are pessimistic |
| Method | One run (`--runs 1`), 2026-10-21. A seeded project holds generated Rust files: items with doc comments, derives, impls, `match` and strings, every one distinct. They are 1 000 000 and 10 000 000 bytes. The window shows a terminal; the driver dispatches `workbench::OpenFile` (what `ui.run` and `hutan open` send). **Open** is from dispatch to the end of the first frame that draws the file's text: `fs.stat`, the chunked `fs.read`, the rope, the first layout. **Highlighted** is from that frame to the first frame whose lines carry colors from a finished tree of the current text (`CodeView::paint_times`). **RSS** is sampled before the open and 1.5 s after the 1 MB file is highlighted. **Scroll** is 600 wheel steps of 120 px down and 600 back, one per frame. |

| # | Measure | Result | Budget | |
|---|---|---|---|---|
| B11a | open 1 MB, dispatch → first paint of text | **22.2 ms** (Hutan's own handlers and frame: 7.4 ms) | ≤ 100 ms | ✓ |
| B11b | open 10 MB, dispatch → first paint of text | **122.4 ms** (4.1 ms Hutan; the rest is the daemon read and the transfer) | ≤ 500 ms | ✓ |
| B11c | 1 MB: first paint → highlighted viewport | **141 ms** (the background parse, ≈ 100 ms in E-2's bench, plus the wake-up and a frame) | ≤ 400 ms | ✓ |
| — | 10 MB: first paint → highlighted viewport (under `large_file_bytes`, so it is parsed) | 1 146 ms | — | input never waited |
| B11d | desktop RSS growth, one open 1 MB file with syntax | **52.9 MB** (133.5 → 186.4 MB) | ≤ 40 MB | **✗ miss by 12.9 MB** |
| B13 | scroll of the 1 MB file: frame interval p95 | **12.4 ms** (p50 8.33 = every vsync, p99 13.6, max 14.8; 1 198 intervals) | refresh + 2 ms = **10.3 ms** at 120 Hz (the spike's 60 Hz figure: 18.7 ms) | **✗ miss by 2.1 ms** at 120 Hz; ✓ against 18.7 ms |
| B13 | frame work while scrolling (render + layout + paint, Hutan's share) | p50 1.7 ms, **p95 2.5 ms**, max 7.4 ms | — | |
| B13 | lines shaped per frame while scrolling | **5.2** with 39 rows in view | ≤ visible + overscan | ✓ (also the GPUI test `a_frame_shapes_only_the_lines_in_view`) |

**The misses.**
- **B11d.** The tree-sitter tree of the 1 MB file alone is ≈ 37 MB (E-2's measurement above, made outside the desktop). The rest of the growth is allocator high water that RSS does not give back, not data the viewer keeps:
  - the 1 MB text received in chunks and copied into the rope;
  - the language's compiled queries (loaded once per language per process);
  - glyph atlas pages for the new glyphs.

  Nothing in `code_view` holds a whole-file structure besides the rope (≈ 1×). The tree of a tab that is not shown is dropped, so the budget holds per *visible* file, not per open file. The spike marks B11d [INFERENCE], to be replaced by E-2's number; with E-2's 37 MB tree, ≤ 40 MB leaves 3 MB for everything else, which a desktop process cannot meet as RSS. Not relaxed here: the options are a smaller syntax threshold (no tree above e.g. 512 KB until asked), or a re-measurement of the budget as "tree + rope + 8 MB". That is the orchestrator's call.
- **B13 at 120 Hz.** Frames take 2.5 ms (p95) of Hutan's time and shape about 5 lines each. The p95 interval is 12.4 ms because about 1 frame in 15 missed a vsync on a machine with load 12–20. That is the same display floor B7b reports. Against the spike's 60 Hz figure (18.7 ms) it passes.

**Fallback decision (spike § 4.1): stay with B′ (own `code_view`); GPUI Kit is not adopted.**
- The trigger was "the read-only viewer cannot meet B11/B13".
- B11a–c pass with 4×, 4× and 2.8× headroom.
- The two misses are not the viewer's element. B11d is the tree-sitter tree, which `gpui-base` would hold just the same, since its highlighter is tree-sitter too. B13's frame work is 2.5 ms with only the visible lines shaped; the missed intervals are display hiccups under load.
- Replacing `code_view`'s internals would move neither number.
- Re-evaluate at E-6 only if the editing work (IME, selections) is estimated over L, the other half of the trigger.

**Orchestrator decision (2026-10-21) on the misses.** B11d was marked [INFERENCE] in the spike with the instruction to replace it by E-2's measurement before enforcing; the measured tree alone is 37 MB/MB for token-dense Rust. B11d is re-based on measurement, not relaxed silently: **≤ 60 MB desktop RSS growth for one visible 1 MB file with syntax, ≤ 8 MB without**, and the tree is kept only for visible files (already true). A future `syntax_max_bytes` threshold (no tree above it until asked) is the lever if real files exceed it. B13 stays 10.3 ms at 120 Hz: Hutan's frame work is 2.5 ms p95, the missed vsyncs are the display floor under load also seen in B7b; it is re-measured on an idle machine and with GF-1 (on-demand display link) when that spike runs.

## Editor: typing (slice E-6, `hutan-code-view`)

Budget B12a of `docs/spikes/2026-10-21-editor.md` § 4.8, end to end through the real daemon and the release desktop (`just bench --only B12a`; scenario `editor-type`: `HUTAN_BENCH_UI` `editor_type` in `crates/desktop/src/bench_ui.rs`, harness `editor_type` in `xtask/src/bench_ui.rs`).

| | |
|---|---|
| Machine | Apple M1 Pro (10 cores, 16 GB), macOS 27.0.1, 120 Hz display (8.33 ms measured); other agents were building (load average 12–13), so the numbers are pessimistic |
| Method | One run (`--runs 1`), 2026-10-21. The 1 MB generated Rust file of B11 is opened (`workbench::OpenFile`), its tree is waited for, the caret goes to its middle line and 300 letters are typed through `Window::dispatch_keystroke` (the keymap, then the platform text input: `EntityInputHandler::replace_text_in_range`), one every 60 ms with up to 17 ms of jitter. **Hutan path** (B12a) is from the key's dispatch to the end of the frame that shows it, without the wait for the display tick; **on screen** (B12b) includes that wait. |

| # | Measure | Result | Budget | |
|---|---|---|---|---|
| B12a | key handled → frame with the glyph submitted, p95 | **3.12 ms** (p50 1.51, p99 4.56, max 10.0; 300 keys) | ≤ 2 ms | **✗ miss by 1.1 ms** |
| B12a | frame work while typing (render + layout + paint), p95 | 2.57 ms (p50 1.17) | — | |
| B12b | key → on screen, p95 | **9.31 ms** (p50 5.62) | 1 frame + 2 ms = 10.3 ms at 120 Hz | ✓ |
| — | lines shaped per keystroke | 1 (GPUI test `a_keystroke_shapes_only_the_line_it_changed`) | ≤ the lines it changed | ✓ |
| — | parses from scratch while typing | 0 after the first (`edits_reach_the_parser_incrementally`, `SyntaxStats::incremental`) | none | ✓ |

What the key path does: one transaction on the rope (≈ 1 µs, E-1), the parser's `edit` + `reparse` scheduling (≈ 2 µs, E-2), the highlights of the lines in view carried across the edit (≈ 150 µs for 60 lines, E-2), one line shaped, and a frame. The median (1.5 ms) is inside the budget; the p95 is not, under a machine load of 12–13.

**Miss, not relaxed.** The likely cost beyond the editor's own work is that every keystroke also redraws the workbench: the code view's `Changed` event (the status bar's `Ln, Col`) notifies the workbench, which renders its whole tree around the cached views. Follow-up (E-14 or a fix slice): notify only the status item for a caret move and keep the workbench render off the key path, then measure again on an idle machine.

### B12a after the E-8 key-path fix

A symbolized profile (`sample` during `editor_type`) showed where a keystroke's frame goes: the code element's prepare (the highlights of the *whole* view, asked for again after every edit because shaped lines were keyed by the text version, ≈ 28 % of the draw), Taffy layout of the workbench (≈ 17 %), the workbench's own `render` (≈ 7 %), and painting. It also showed that about one key in five arrives while a frame is pending (the parser's new tree), and GPUI draws that frame inside the key's dispatch, so it counts in the key's handling time. Notifying the workbench on a caret move cost little by itself: GPUI renders every ancestor view of a view that changed, so the workbench's `render` runs on each keystroke whatever it is told.

What E-8 changed: shaped lines are found again by line, displayed text and the tree they were colored from (not the text version), and highlights are asked for only the lines that miss, so a keystroke computes the highlights and runs of the one line it changed; `editor.position` and `editor.mode` are cached views of their own that read the focused code view, and the workbench is no longer notified per keystroke (`FileViewEvent::Changed` is gone). Test: `editor_gpui::vim_in_a_file_tab_normal_keys_never_type_and_the_colon_line_saves` asserts the workbench is not notified by a caret move and that `editor.position` follows it.

| # | Measure (3 runs, 2026-10-21, load average 20–25: other agents building) | Before (E-6) | After (E-8) | Budget | |
|---|---|---|---|---|---|
| B12a | key handled → frame submitted, p95 (median of runs) | 3.02 ms (p50 1.51) | **2.80 ms** (runs 2.93, 2.63, 2.80; p50 1.33–1.42) | ≤ 2 ms | **✗ miss by 0.8 ms** |
| B12a | frame work while typing, p95 | 2.26–2.56 ms | 2.15–2.70 ms (p50 1.14–1.21) | — | |
| B12b | key → on screen, p95 | 7.97–9.44 ms | 7.68–8.50 ms | 10.3 ms | ✓ |

**Still a miss, not relaxed**, measured under a machine load twice that of the first number. What remains is the workbench shell (layout of its uncached chrome, its `render`) and the parser-publish frame drawn inside a key's dispatch. Next steps (E-14 or a fix slice): make the status bar and title bar cached views of fixed height, and measure again on an idle machine.

## Editor: Vim in file tabs (slice E-8, B15)

B15 is a non-search Vim Normal command on a 1 MB buffer, key → applied, budget ≤ 2 ms. Run `cargo bench -p hutan-code-view --bench vim` (`crates/code_view/benches/vim.rs`). Each of 200 samples lends a 1 MB rope (`hutan_buffer::Buffer`) to the code view's `BufferHost` with the caret in the middle of the file, runs the command's keys through `hutan_vim::Vim`, and takes the model back, as `CodeView::vim_key` does for each key. Release build, plain `Instant`, 2026-10-21, load average 14–19 (other agents building). The frame that shows the result is B12's.

| Command on 1 MB | p50 | p95 | max | Budget |
|---|---|---|---|---|
| `dd` | 0.005 ms | **0.007 ms** | 0.051 ms | ≤ 2 ms ✓ |
| `w` | 0.004 ms | **0.009 ms** | 0.028 ms | ✓ |
| `ciw`, a letter, Escape | 0.009 ms | **0.012 ms** | 0.026 ms | ✓ |
| `yy` then `p` | 0.013 ms | **0.019 ms** | 0.029 ms | ✓ |
| `G` | 0.001 ms | **0.001 ms** | 0.003 ms | ✓ |
| `x` then `u` | 0.003 ms | **0.004 ms** | 0.013 ms | ✓ |

The host reads the rope in `O(log n)` and copies only what a command returns (a word, a line). This removes the per-command copy of the whole text that `vim.rs:1321` made before E-7a. `:s`, `:g` and `:v` search a snapshot of the rope on a background thread and can be cancelled (`CodeView::ex_running`).

Re-measured after E-7b (the grown layer: command parser, registers, marks that follow every edit, display columns), same bench, 2026-10-21, release, load average 3.5–6: `dd` p95 0.011 ms (max 0.109), `w` 0.008, `ciw`+text+Esc 0.013, `yy p` 0.021, `G` 0.001, `x u` 0.006 — all within B15 (≤ 2 ms). The bench has not yet been extended to the E-7b commands (`%`, `}`, `>>`, `J`, block `I`, a macro); search (`/`, `n`, `*`) is outside B15 and scans line by line from the caret.

## Editor: the Vim layer (slice E-7a, `hutan-vim`)

E-7a moves the V-1 modal layer out of `desktop` into a GPUI-free crate and changes no behaviour, so
no budget moves; the Vim budget (B15, a Normal command on a 1 MB buffer ≤ 2 ms) is measured with the
rope host in E-8. What E-7a measures is what the crate costs to build and to link.

| | |
|---|---|
| Machine | Apple M1 Pro (10 cores, 16 GB), macOS 27.0.1, `rustc 1.98.0`; load average 3–5 |
| Dependencies | none (`proptest` in dev only); no dependency on `hutan-buffer` either: the host trait speaks byte offsets and transactions that a `hutan_buffer::Buffer` host maps one to one |

| Measure | Value |
|---|---|
| Clean release build of `hutan-vim` (`RUSTC_WRAPPER= cargo build --release -p hutan-vim --lib`, empty target dir) | **0.6 s** wall (no dependencies to build) |
| Size of `examples/size_probe` (a complete `String` host, the driver and `Vim`), release, thin LTO, `strip = "symbols"` | **0.41 MB** |
| The same driver without `hutan-vim` and the host (arguments in, string out) | 0.34 MB |
| So the Vim layer and a small host cost | **≈ 0.07 MB** |
| `cargo nextest run -p hutan-vim` (27 tests, two property tests of 256 cases each), warm | 0.1 s |

The desktop's release binary is not rebuilt for this: the code moved (the 2 364-line `vim.rs` became
the crate plus a 130-line adapter), and the state machine is instantiated once, for `TextInput`, so
the linked code is the same functions as before. The adapter's reads are `O(1)` for a character and
`O(line)` for a line lookup on the `String` an input holds; the `Vec<char>` copy of the whole text
that V-1 made for every key is gone (it was `O(n)` per key, spike § 4.6 item 1), which matters for
the 1 MB buffer of the editor host and not for a one-line field.

Equivalence with V-1 was checked differentially while extracting: the old and the new machine fed
the same random keys (159 000 with one seed, 4.7 million with another) over random
multi-byte texts and options agree on outcome, text, caret, selection, mode, register, pending keys
and clipboard after every key. The scaffold is not kept; the V-1 suite (40 tests) runs against the adapter with its
assertions untouched (only the rig changed: it reaches Normal mode by Escape, since the mode is no
longer a field the test can set).

## Checkpoints: snapshot cost (slice CK-1)

The checkpoint engine (`crates/daemon/src/checkpoints/`, ADR 0019) on a generated repository shaped
like the spike's (`docs/spikes/2026-10-21-checkpoints.md` §4.1): 100 directories × 100 `.rs` files
of 3–8 KiB, 20 random 1 MiB binaries, an ignored `node_modules/` of 5 000 files and an ignored
`.env`; committed and `git gc`'d, then one `git status` so the user's index is stat-clean as in
daily use. Measured with

```text
cargo run -p hutan-daemon --release --example checkpoint_bench -- 5
```

which generates the repository in a temporary directory, takes one seeding snapshot, then 5
snapshots per scenario (each run of an editing scenario writes fresh content, so every run hashes
new objects), and prints the table below. `RUST_LOG=hutan_daemon=debug` adds the time of every git
step. Wall time is the whole `Checkpoints::snapshot` call (`rev-parse` to `update-ref`); object
growth is `count-objects -v` (`size` + `size-pack`) before and after.

| | |
|---|---|
| Machine | Apple M1 Pro (10 cores, 16 GB), macOS 27.0.1, APFS, `git version 2.54.0 (Apple Git-157)`, release build |
| Load | other agents building on the machine: 1-minute load average 10–14 during the run (process.md asks for < 20) |

| Scenario | Median of 5 | Runs (ms) | Files hashed | Object growth (median) |
|---|---|---|---|---|
| `git status --porcelain` (reference) | 33 ms | 35, 33, 36, 33, 32 | — | — |
| first snapshot (seeds the private index from the user's) | 225 ms | 1 run | 0 | 4 KiB |
| unchanged | **101 ms** | 101, 109, 103, 99, 101 | 0 | 4 KiB (the commit) |
| 3 edits | **242 ms** | 236, 256, 240, 250, 242 | 3 | 36 KiB |
| 250 edits (200 modified + 50 new files) | **542 ms** | 508, 542, 562, 544, 530 | 250 | 1 416 KiB |
| one new 5 MiB binary | **432 ms** | 432, 440, 486, 374, 396 | 1 | 5 136 KiB |
| 10 000 files touched, same content | **736 ms** | 882, 735, 726, 783, 736 | 0 | 4 KiB |

Where the time goes (debug step timings, same machine and load): every git process costs 15–35 ms
here whatever it does, so the cost follows the number of git steps:
- **Unchanged**: 5 processes — `rev-parse`, `diff-files` (stat only), `ls-files -o`, `commit-tree`,
  `update-ref`. `write-tree` is skipped when nothing was written to the private index (its tree is
  the last snapshot's), and the check of the whole private index for entries that became ignored
  (`ls-files -c -i`, ≈ 35 ms) runs only when an ignore source changed: a `.gitignore` among the
  changes, `info/exclude`, the global excludes file or the configuration naming it.
- **Edits** add `check-ignore` over the stat-dirty paths (so a changed file under a `.gitignore`
  that ignores itself is never read; past 256 paths the one-pass `ls-files -c -i` instead, since
  `check-ignore` costs ≈ 0.1 ms a path), `update-index --refresh` and a second `diff-files` (to
  tell real changes from touched files), `check-attr filter`, `hash-object -w --no-filters`,
  `update-index --index-info` and `write-tree`: ≈ 12 processes for 3 edits. Hashing writes one
  loose object per new blob, ≈ 0.9 ms each (250 files: ≈ 200 ms; not `fsync`: `core.fsync=none`
  measured the same).
- **Touched files** (a branch switch, a formatter that rewrites unchanged files): the refresh
  reads every stat-dirty file once (≈ 600 ms for 10 000 files) and records their stat data, so the
  next snapshot is back at the unchanged cost. Nothing is hashed or written.
- **First snapshot**: the copy of the user's index, `check-attr` over every tracked path (to drop
  `filter=` paths and mark converted ones), and the ignore-source read; once per checkout and
  daemon run.

Against the spike's prototype (§4.2, an unloaded run): unchanged 101 ms vs 117–141 ms; 3 edits
242 ms vs 125 ms (method A, `git add`); 250 edits 542 ms vs 326 ms; 5 MiB binary 432 ms vs
404 ms; touched 10 000 files 736 ms vs 513 ms (persistent private index). The edit scenarios
pay for the steps that keep content raw, capped and private (`lstat` filters, `check-ignore`,
`check-attr`, separate hashing) where `git add` does it in one process. Earlier runs of the same
bench at load 20–27 gave unchanged 107–145 ms and 3 edits 237–272 ms; the numbers move with load
more than with the repository.

The 120 MiB case of the spike is not repeated: such a file is now skipped by `max_file_bytes`
(10 MiB) after an `lstat`, before anything reads it (tested in
`checkpoints::files_over_the_caps_are_skipped_before_anything_is_hashed`).
