# Motion for the Hutan UI polish pass

Status: research, read-only. No repo code changed. Bracketed numbers link to the Sources list. `[INFERENCE]` marks my extrapolation.

## Conclusions

- Keep the existing rule ([4]: motion explains a state change and ends within 150 ms). Add one 200 ms tier for sheets and toasts only.
- Use four duration tokens: 100, 150, 200, 250 ms. Nothing exceeds 250 ms.
- Exit is shorter than enter, and both use ease-out. No ease-in on UI.
- Do not animate: keyboard-initiated actions, list insert/remove in busy lists, context menus on open, terminal content, theme switches, typing.
- Reduce Motion: drop spatial movement and pulses; keep only short opacity changes, or go instant.
- GPUI can animate opacity, height (via layout), and springs. The pinned checkout exposes no transform (translate/scale), so slides and press-scale must become opacity-only or instant.

## Method

- Apple HIG Motion [6] and the NSWorkspace Reduce Motion property [5].
- Emil Kowalski's review standards [1] (raw `STANDARDS.md`). Its numbers are the most concrete I found.
- Material 3 easing/duration names [2]. The token-spec page returned only metadata, so numeric values come from a third-party mirror in `material-rs` [2] ([INFERENCE] they match the official table).
- Rauno Freiberg, `interfaces` README, Motion section [3]. His "Invisible Details" course was found but not read.
- web.dev compositor article [7].
- Linear design-refresh post [11]. It publishes no motion values; thin.
- Zed issues via read-only `gh search issues --repo zed-industries/zed` with queries "reduce motion", "animations", "motion". Reactions read with `gh api repos/zed-industries/zed/issues/N` (GET) [8][9][10].
- Pinned GPUI `zed-industries/zed@ef376006` [12].

## Duration tokens

| Token | ms | Use |
|---|---|---|
| `motion.instant` | 0 | Keyboard actions, focus ring, context menu open |
| `motion.fast` | 100 | Hover/press colour, popover close, tab content crossfade |
| `motion.base` | 150 | Popover/menu open, collapse/expand, rare list insert |
| `motion.slow` | 200 | Sheet, toast enter |
| `motion.max` | 250 | Hard ceiling; only with explicit design review |

Basis: Emil gives press 100–160 ms, small popovers 125–200 ms, dropdowns 150–250 ms, and "UI animations stay under 300 ms" [1]. Material 3 short2/short3/short4/medium1 are 100/150/200/250 ms [2]. Rauno: "Animation duration should not be more than 200ms" [3]. Hutan's 150 ms cap [4] is the strictest, so the 200 ms tier is limited to sheets and toasts.

Exit: enter 150 ms → exit 100 ms (about 0.67×); enter 200 ms → exit 150 ms [INFERENCE from Emil's "exit faster" guidance; no published ratio].

## Easing

| Name | Curve | Use |
|---|---|---|
| ease-out (default) | `cubic-bezier(0.23, 1, 0.32, 1)` | Enter, exit, popovers, toasts [1] |
| drawer | `cubic-bezier(0.32, 0.72, 0, 1)` | Sheets [1] |
| ease-in-out | `cubic-bezier(0.77, 0, 0.175, 1)` | On-screen movement only (rare here) [1] |
| ease | CSS `ease` | Hover/colour [1] |
| linear | — | Pulses only [1] |

Never `ease-in` on UI [1]. Material's standard curve, `cubic-bezier(0.2, 0, 0, 1)` [2], is an acceptable alternative.

## Per-kind spec

| Kind | Duration | Easing | Animate | Notes |
|---|---|---|---|---|
| Hover fade | 100 | ease | colour/background | No scale; gate to pointer devices [1] |
| Press | 100 | ease-out | colour/background | Web norm is scale 0.96–0.97 [1][3]; scale unavailable in GPUI (below) |
| Focus ring | 0 | — | none | Keyboard-only, instant; keyboard actions don't animate [1] |
| Popover/menu open | 150 | ease-out | opacity | Origin-aware scale unavailable [1] |
| Popover/menu close | 100 | ease-out | opacity | |
| Context menu open | 0 | — | none | macOS right-click menu only animates out [3] |
| Sheet | 200 | drawer | opacity | Modals centred, no origin scale [1] |
| Toast enter | 200 | ease-out | opacity | Toast exit 150 ms; never blocks input [1] |
| List insert/remove (busy lists) | 0 | — | none | Frequent actions: no motion [1][3][6] |
| Rare list insert | 150 | ease-out | opacity | Stagger 30–80 ms if several [1] |
| Tab switch | 100 | ease | content opacity crossfade | No sliding indicator [1] |
| Collapse/expand | 150 | ease-out | height, clipped | Matches [4] "≤150 ms for expanding cards"; instant if content is long |

Shadow/border/colour changes are instant unless listed above.

## What must not animate

- Keyboard-initiated actions (command palette, shortcuts, focus movement) [1].
- Typing, search results, streaming agent text, terminal output and cursor [4].
- Frequent list operations and context menu open [1][3][6].
- Theme switches [3].
- Looping ambient motion, except the existing running-agent glyph. Loops pause off-screen [3][4].
- Anything that moves content the user is reading.

## Exit vs enter and interruptibility

- CSS transitions retarget mid-flight; keyframes restart from zero [1]. Use retargetable motion for hover and toggles.
- GPUI's keyed `with_animation` does not retarget: its phase lives in element state from first mount [12, `elements/animation.rs` L233–240, L407–420]. Use `with_spring` for anything users may reverse mid-flight; it "preserves position and velocity across target changes" [12, `elements/animation.rs` L119–123].
- Spring for a non-bouncy ~150 ms settle: `SpringConfig::new(700, 53, 1)` (ζ≈1, ω₀≈26.5 rad/s) [INFERENCE from the 4/ω₀ settle rule]. Bounce is not appropriate for UI [1].
- Apple: "Let people cancel motion… don't make people wait for an animation to complete" [6]. None of the above may block input.

## Reduce Motion

- Apple: replace motion with cross-fades, highlight fades or colour shifts; avoid large animations [5][6].
- Emil: keep opacity/colour, drop transform motion; "fewer and gentler animations, not zero" [1].
- Hutan reads the setting differently:
  - `NSWorkspace.accessibilityDisplayShouldReduceMotion` is the documented property. Observe `accessibilityDisplayOptionsDidChangeNotification` for changes [5].
  - Hutan shells to `defaults read com.apple.universalaccess reduceMotion` once per process (`crates/desktop/src/theme.rs` L495–507). A runtime toggle is missed until relaunch. Recommend the NSWorkspace property plus the notification.
- Under Reduce Motion: 100–150 ms opacity changes may stay; spatial movement, collapse height animation, pulses and sheet fades go instant. GPUI's `with_animation` snaps oneshot animations to end state when `cx.reduce_motion()` is set [12, `elements/animation.rs` L407–412], so opacity fades disappear too. Render the end state directly under Reduce Motion instead of relying on the wrapper.
- Hutan's running-block gutter and agent/task pulses are already steady under Reduce Motion (`crates/terminal_view/src/element.rs` L454–456; `crates/desktop/src/workbench/panels/agents.rs` L381; `docs/design.md` ~L635).

## Frame budget

- 60 Hz: 16.7 ms per frame. 120 Hz: 8.3 ms per frame [arithmetic].
- web.dev targets roughly 4–5 ms of compositing during transitions [7]. Only transform and opacity are compositor-only; other properties trigger layout/paint [7].
- Zed #50392 "Presence of animated elements generate layout recalculation and repaint": open, 7 reactions (+1), 6 comments [8]. Zed #55949 (iGPU 47–50% render busy for an agent spinner): open, 0 reactions, 4 comments [9]. Both support keeping pulses cheap and avoiding layout-driven motion.
- Hutan's terminal block animation re-renders the whole `TerminalView` on every 140 ms tick (`crates/terminal_view/src/view.rs` L93, L2009–2034). Keep it to running blocks and measure at 120 Hz [INFERENCE on cost].

## GPUI mapping and limits (pinned `ef376006`)

- Hutan does not use `with_animation` or `Animation::new` anywhere today; all motion is manual (timer ticks or wall-clock modulo). This section is prospective.
- `AnimationExt::with_animation(id, Animation::new(duration).with_easing(f), |el, t| …)` drives the animator with eased `t` each frame [12, `elements/animation.rs` L15–28, L60–63, L84–99].
- Built-in easings: `linear`, `quadratic`, `ease_in_out`, `ease_out_quint`, `bounce`, `pulsating_between` [12, `elements/animation.rs` L506–545]. No cubic-bezier helper; use `ease_out_quint()` as a close default, or write a small solver (`Fn(f32) -> f32`) [INFERENCE on cost].
- Opacity: `Styled::opacity` exists [12, `styled.rs` L746]. My grep of `styled.rs`, `style.rs`, `element.rs`, `elements/div.rs` and `window/` found no `transform`, `translate` or `scale` setter. Treat press scale, origin-aware popovers and sheet slides as unavailable.
- Layout animation: none. The animator returns a new element each frame, so changing height re-runs taffy layout each frame [12, `elements/animation.rs` L407–470]. Use it for collapse only, with a clip.
- Frame scheduling: `request_animation_frame` each frame, or `Animation::with_max_fps` to throttle decorative loops [12, `elements/animation.rs` L65–72; `window.rs` L2726–2733]. Use `with_max_fps` for running pulses.
- Reduce Motion: `App::reduce_motion()` is set only through `cx.set_reduce_motion`. Zed derives it from its own setting [12, `crates/zed/src/zed.rs` L2075–2078; `settings_content/src/settings_content.rs` L164–170], not from macOS. Hutan must call `set_reduce_motion` from the NSWorkspace flag.
- Interruptibility: use `with_spring` (above).

## Gaps

- Linear, Raycast and Arc published no motion values I could read. Raycast's lack of an open/close animation comes via Emil [1]. Treat competitor claims as thin.
- No 120 Hz measurements exist in the sources; the frame budget is arithmetic.
- The Material token page was unreadable; values come from the mirror [2].

## Sources

1. Emil Kowalski, animation review standards: https://github.com/emilkowalski/skills/blob/main/skills/review-animations/STANDARDS.md ; also https://emilkowal.ski/ui/great-animations
2. Material 3, easing and duration: https://m3.material.io/styles/motion/easing-and-duration ; numeric values from https://docs.rs/material-rs/0.1.2/src/material_rs/theme/motion.rs.html (third-party mirror)
3. Rauno Freiberg, interfaces (Motion section): https://github.com/raunofreiberg/interfaces
4. Hutan: `docs/spikes/2026-10-21-clean-aesthetic.md` §3.1 principle 7; `docs/design.md` ~L286, ~L500, ~L524, ~L635
5. Apple, NSWorkspace.accessibilityDisplayShouldReduceMotion: https://developer.apple.com/documentation/appkit/nsworkspace/accessibilitydisplayshouldreducemotion
6. Apple HIG, Motion: https://developer.apple.com/design/human-interface-guidelines/motion
7. web.dev, compositor-only properties: https://web.dev/articles/stick-to-compositor-only-properties-and-manage-layer-count
8. Zed #50392: https://github.com/zed-industries/zed/issues/50392 (open; 7 reactions, 6 comments)
9. Zed #55949: https://github.com/zed-industries/zed/issues/55949 (open; 0 reactions, 4 comments)
10. Zed #53369: https://github.com/zed-industries/zed/issues/53369 (open; 1 reaction, 8 comments); not cited in text above, listed for the iGPU search
11. Linear, "A calmer interface for a product in motion" (2026-03-12): https://linear.app/now/behind-the-latest-design-refresh
12. Pinned GPUI, zed-industries/zed@ef376006: `crates/gpui/src/elements/animation.rs`, `spring.rs`, `app.rs`, `window.rs`, `styled.rs`; `crates/zed/src/zed.rs`; `crates/settings_content/src/settings_content.rs`
