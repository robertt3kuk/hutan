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
- **First-run setup** (found while building the harness): a fresh `HUTAN_HOME` shows the setup sheet
  over the whole window, and it blocks the mouse. The B1–B6 numbers above were measured with it
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
