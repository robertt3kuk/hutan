# libghostty-vt vs alacritty_terminal: re-evaluation (2026-10-18)

Spike by agent GhosttySpike, temp dir outside the repo (deleted). Official Ghostty = github.com/ghostty-org/ghostty (MIT); libghostty-vt from herdr's vendored copy (herdrdev/herdr, 44f2a44df) built with official Zig 0.16.

## Verdict
**Don't swap now. Keep a possible swap for later, behind the existing `terminal` crate boundary, and only once the gates below are met.** The ADR 0004 watch item still isn't met:
- **Binary Snapshot API:** it exists in native C and works in my probe. But no released Rust binding exposes it: `libghostty-vt` 0.2.2 on crates.io doesn't, and herdr's binding has only raw FFI.
- **Scrollback:** the native line cap exists but is approximate. It kept 9,661–9,797 lines for a cap of 10,000.

**The parser isn't Hutan's bottleneck; Hutan's own wrapper is.** Hutan's code takes 32–77 % of `Emulator::feed` time, depending on the corpus. Fix that first.

## Setup
- **Machine and toolchain:** Apple M4, Darwin 25.6.0, rustc 1.98.0, release build with thin LTO and `codegen-units=1`. Ghostty built `ReleaseFast` with SIMD.
- **Runs:** each run parses 64 MiB at 120×40, fed in 32 KiB chunks, in a fresh process. Every number is the median of 3 runs.
- **Engines compared:**
  - **alacritty:** `alacritty_terminal` 0.26.0 with `kitty_keyboard: true` and 10,000 lines of scrollback, run bare (no Hutan wrapper).
  - **Ghostty:** herdr's vendored libghostty-vt 44f2a44df through herdr's `crates/ghostty-vt`, with `SCROLLBACK_MAX_LINES=10000` and the byte cap removed.
  - **Hutan:** a copy of `crates/terminal`, measuring `Emulator::feed`.
- **Corpora:**
  - **flood:** `y\r\n`.
  - **color:** `ls -la`-style lines with 256-colour SGR.
  - **source:** `emulator.rs` with CRLF line endings.
  - **redraw:** alternate screen, 32 frames each wrapped in DEC 2026, with CUP, 256-colour and EL on all 40 rows.
  - **markers:** OSC 7 plus OSC 133 A/B/C/D around each command.
- **Snapshot cost:** time to turn all 40 visible rows into `Vec<hutan_protocol::render::Row>` styled runs, averaged over 1,000 iterations.
  - alacritty uses Hutan's `RowScratch::fill`/`to_row`.
  - Hutan uses the full `Emulator::snapshot`.
  - Ghostty uses `RenderState::update` plus herdr's per-cell iterators. "Naive" calls `basic_data` and grapheme per cell; "fast" checks `is_default_blank` first.

## Numbers

| Corpus | alacritty MB/s | Ghostty MB/s | Hutan `feed` MB/s | Hutan's own share of time | Peak RSS MiB (alac / ghost / hutan) | History kept (alac / ghost / hutan) |
|---|---:|---:|---:|---:|---|---|
| flood | 84.7 | 50.3 (line cap) · 98.6 (16 MiB byte cap) | 19.3 | 77 % | 33.6 / 12.2 (17.6) / 5.6 | 10,000 / 9,797 (15,737) / 10,000 |
| color | 187.0 | 276.3 | 102.8 | 45 % | 33.8 / 12.6 / 6.8 | 10,000 / 9,661 / 10,000 |
| source | 129.7 | 482.4 | 88.5 | 32 % | 33.8 / 12.4 / 7.6 | 10,000 / 9,741 / 10,000 |
| redraw | 224.4 | 465.6 | 146.9 | 35 % | 2.2 / 2.3 / 2.2 | 0 / 0 / 0 |
| markers | 73.6 | 250.5 | 38.3 | 48 % | 33.8 / 12.5 / 8.9 | 10,000 / 9,781 / 10,000 |

| Snapshot µs per frame | flood | color | source | redraw | markers |
|---|---:|---:|---:|---:|---:|
| alacritty + Hutan row code | 6.7 | 10.5 | 8.3 | 8.9 | 6.9 |
| Hutan `Emulator::snapshot` | 8.1 | 12.7 | 10.3 | 10.4 | 8.7 |
| Ghostty, naive per-cell FFI | 114.8 | 128.3 | 115.1 | 126.0 | 112.1 |
| Ghostty, blank fast path | 28.2 | 86.7 | 51.2 | 71.0 | 36.9 |

What the numbers show:
- **Parsing:** Ghostty parses 1.5–3.7× faster than alacritty on real-looking output.
  - On `yes` floods with the line cap it is 0.6× as fast. With a byte cap it is 1.16× as fast, but then the history isn't line-bounded. [INFERENCE: line-cap page pruning is what costs the time.]
- **Memory:** Ghostty uses about 2.7× less RSS than bare alacritty. Hutan's compact cold store is already below both.
- **Snapshots:** Ghostty extraction through per-cell FFI is 4–14× slower than Hutan's. That is still under 1 % of an 8 ms frame budget. The bulk `ROW_DATA_CELLS_RAW` API wasn't measured.
- **Earlier markers run discarded:** in my first markers run Ghostty showed 51.6 MiB RSS. That was my harness never draining herdr's `take_pwd_changes` buffer, not Ghostty. The table uses the corrected runs.

## Feature probe (Ghostty, 80×24, run)
- **DA/DSR/kitty replies:** `ESC[?1u`, `ESC[?62;22c`, `ESC[>1;0;0c`, `ESC[0n`, `ESC[1;3R`. Kitty flags read back as 1.
- **OSC 7:** the raw bytes `file://localhost/tmp/project` arrive through the callback.
- **OSC 8:** the hyperlink URI is kept on the cell.
- **DEC 2026:** the mode flag is visible, but text lands on the grid immediately. The embedder has to hold back presentation itself, as herdr does.
- **Binary snapshot:**
  - Without continuation tracking, encoding while `ESC[31` is unfinished fails with `INVALID_VALUE` (-2).
  - With `CONTINUATION_MAX_BYTES=65536` the snapshot is 1,387 bytes. After restoring, the alternate screen plus the pending CSI (`mX`) matched, and so did the primary screen after `?1049l`.
  - This fixes the formatter-restore failures from the Oct-13 spike.

## Feature matrix

| | alacritty 0.26 (+ Hutan) | libghostty-vt (44f2a44 / main 431126c2) |
|---|---|---|
| Release | crates.io, pure Rust | No tagged libghostty release; README says API signatures are still in flux. App is at v1.3.1 |
| Scrollback contract | Exact 10,000 lines (Hutan's settle and cold store) | Byte cap or physical-line cap, page-granular, first limit hit wins (`terminal.h:1391-1435`). Can undershoot: `PageList.zig:11220-11251` |
| Snapshot / restore | Owned DTOs only; engine state can't be restored | Native binary snapshot (`snapshot.h`), CRC-checked, READY then history pages. Format v1 has no compatibility guarantee; continuation tracking must be on from the start |
| Render state | Hutan's `RowScratch` | `RenderState` with dirty rows, row/cell iterators, a bulk raw-cells view; two-phase update for lock splitting |
| OSC 133 / OSC 7 | Hutan's sniffer (vte ignores them) | Native semantic prompts plus a PWD callback |
| Kitty keyboard, DA/DSR, OSC 8, DEC 2026 | Yes; 2026 handled by the parser with a 150 ms timeout | Yes; 2026 is a mode flag only, the embedder schedules |
| Threading | `Send + Sync` | C API creates no threads; callbacks run inside `vt_write`. crates.io 0.2.2 is `!Send/!Sync`; herdr adds `unsafe impl Send` for `Terminal`, `RenderState`, `RowIterator`, `RowCells`, `KeyEncoder` (lib.rs:2304, 3023, 3133, 3282, 3434), no `Sync` |
| Rust binding | — | crates.io `libghostty-vt` 0.2.2 (2026-09-28) is owned by Uzaaft/pluiedev, not ghostty-org, and has no snapshot module. herdr's crate is `publish=false` 0.0.0, Apache-2.0, and its name `ghostty-vt` is taken on crates.io by an unrelated project (gpui-cn) |

## Build (run)
- **Zig 0.17.0 fails on both** herdr's vendored copy and official main `431126c215dc`:
  - `build.zig:27/28: no field named 'build_root' in struct 'Build'`
  - `src/build/zig.zig:13: Your Zig version v0.17.0 does not meet the required build version of v0.16.0`
  - The version guard requires the same major.minor, so moving to official tip doesn't help, and CI can't use Homebrew's 0.17.
- **Workaround that worked:** the official `zig-aarch64-macos-0.16.0.tar.xz` from ziglang.org, unpacked into the temp dir and used via `ZIG=<path>`.
  - SHA256 `b23d70de…401489` matches `index.json`. I did not verify the minisign signature.
  - herdr's `build.rs` then built cleanly with the stock Xcode SDK. The Oct-13 SDK 15.4 workaround is no longer needed.
  - Cold cargo build including the native library: about 80 s.
- **Stale docs:** ghostty.org/docs/install/build still lists tip → Zig 0.15.2.
- **Platform targets:** herdr's `build.rs` maps Linux gnu/musl, macOS and Windows **MSVC only**. Cross-building for Windows needs the Microsoft SDK (xwin, with licence acceptance). Linux and Windows builds were not exercised here.
- **herdr's patches:** 0004 (Windows Wuffs libc) and 0008 (hyperlink lifetime crash) fix correctness bugs. A swap would need those fixes too, from upstream or as Hutan's own patches.

## Integration cost
- **Threading:** fits without a dedicated thread per terminal if Hutan uses a herdr-style `Send` wrapper under the existing `Mutex<Emulator>` (`terminal.rs:128-145`). The reader, the sync timer and the render/history paths all lock it. With the `!Send` crates.io crate, every snapshot, frame and history call would become a message round-trip to an owning thread.
- **Scrollback:** the hard part. An approximate native cap means Hutan's exact 10k contract, `abs_top` and block numbering need either a Hutan-side exact layer or a deliberate contract change.
- **Restart:** the native snapshot is useful only for a same-build handoff. It doesn't keep the PTY, and ADR 0009 respawns terminals anyway. It would also need Hutan's block, link and title state carried alongside.
- **Rewrite scope:** `render.rs` and `emulator.rs` need a new extraction path (colours, wide/combining cells, link interning, wrap, cursor, modes). The DEC 2026 presentation hold, Zig 0.16 pinned in CI on every OS, and Windows MSVC SDK plumbing come with it.
- **Licensing** [INFERENCE, not legal advice]: Ghostty is MIT, so it's fine. Hutan (MIT OR Apache-2.0) *can* include herdr's Apache-2.0 binding, but those files stay Apache-2.0 only and their notices must be kept. The cleaner option is Hutan's own thin binding generated from Ghostty's MIT headers.

## Optimize in Hutan first (measured on the copied crate)
1. **`RowScratch::fill` blank-tail scan** (`render.rs:203-207`, `rposition(|c| *c != blank)`).
   - On the 120-column flood it takes **1,405 of 1,644 `feed` samples (85 %)**; the parser takes about 10 %.
   - A field-wise check (`c != ' '` first, then flags/extra/fg/bg, same meaning) took the flood from **18.6 → 23.6 MB/s (+28 %)**. Markers didn't change.
   - The root cost is converting every scrolled row cell by cell. Alacritty's `Row::occ` would make it cheap, but it's `pub(crate)`, so that needs an upstream accessor or a smarter spill.
2. **vte's unhandled-OSC formatting.** `osc_dispatch::unhandled` (vte-0.15.0 `ansi.rs:1332-1341`) builds a `String` for every OSC 133/7 even with logging off. That is **277 of 848 samples (~33 %)** on the markers corpus.
   - Fix either by keeping the already-sniffed OSC 133/7 bytes away from the engine (sequence ordering must be preserved) or by an upstream `log_enabled!` guard.
3. **`apply_signal` and `Block` cloning:** about 224 of 848 markers samples.
4. **Extraction** (from the scout review): cache converted rows once per generation across all attachments. The three scan passes (`emulator.rs:307-323`) were measurable but small in these corpora.
5. **The lag itself is in the UI:** the existing perf-lag doc finds daemon echo p95 at 0.128 ms, so whole-window invalidation in the desktop is what to fix. Changing engines doesn't touch it.

**Gates before a future swap:**
- A released Rust binding, or Hutan's own, with a safe snapshot API.
- An exact-or-accepted scrollback contract.
- Zig pinned in CI on all three OSes.
- Conformance corpus parity.
- An end-to-end benchmark that includes Hutan's own work.

[INFERENCE] These are Hutan's own costs, so fixing items 1–3 above also shrinks what a swap could gain.