# Buttons: best-practice evidence for the Hutan UI polish pass

Status: research only. No repo code changed. Numbers marked **[INFERENCE]** are my extrapolation for Hutan's compact desktop UI, not a source's claim.

## Hutan baseline (read, not changed)

- `crates/desktop/src/workbench/ui.rs` (lines 239–363): `ButtonKind {Primary, Secondary, Ghost}`; height `tokens.control_height`; horizontal padding `12 px`; radius `tokens.radius` (6); primary = accent fill, hover accent at 0.88 opacity, text `on_accent`; secondary = 1 px border, hover `surface_hover`; ghost = no border, hover `surface_hover`; disabled = `faint` text + `opacity(0.7)` (round buttons `opacity(0.45)`). `approval_button_kind`: only AllowOnce is Primary.
- `docs/spikes/2026-10-21-clean-aesthetic.md`: one primary per surface (A2), accent budget ≤5 marks per screen (§3.2), quit dialog's destructive confirm was accent-filled (L3), focus ring keyboard-only (D-C3).
- `docs/design.md`: "Native macOS chrome"; close-session dialog "Stop and close / Cancel" (CL-1).

## 1. Hierarchy

| Role | Rule | Source |
|---|---|---|
| Primary | One per surface. Apple: "Keep the number of prominent buttons to one or two per view." Carbon: "Each page should have only one primary button." | [Apple Buttons](https://developer.apple.com/design/human-interface-guidelines/buttons); [Carbon Button guidelines](https://www.carbondesignsystem.com/building-blocks/core/components/button/guidelines) |
| Secondary | 1 px border, medium emphasis. Carbon: secondary is "the negative action of the set, such as 'Cancel' or 'Back'" and should not be used in isolation. | Carbon guidelines (above) |
| Ghost / tertiary | Least pronounced; no fill, no border. Carbon: "often used in conjunction with a primary button." | Carbon guidelines (above); [Carbon specifications](https://www.carbondesignsystem.com/building-blocks/core/components/button/specifications) |
| Destructive | Never primary/default. Apple: "Don't assign the primary role to a button that performs a destructive action." Carbon offers Danger primary, tertiary (border + text), ghost. | [Apple Buttons](https://developer.apple.com/design/human-interface-guidelines/buttons); [Carbon specifications](https://www.carbondesignsystem.com/building-blocks/core/components/button/specifications) |
| Link | Navigation is a link, not a button. Carbon: use links "when the desired action is to take the user to a new page." | Carbon guidelines (above) |
| Icon-only | Icon must "accurately represent what the button does"; needs tooltip + accessible name. Apple: "prefer symbols"; help button circular, one per window. Zed: "Nearly all interactable elements should have a tooltip." | Carbon guidelines; [Apple Buttons](https://developer.apple.com/design/human-interface-guidelines/buttons); [Zed button_like.rs](https://raw.githubusercontent.com/zed-industries/zed/main/crates/ui/src/components/button/button_like.rs); [shadcn Button](https://ui.shadcn.com/docs/components/radix/button) (icon buttons need `aria-label`) |

**Hutan rule [INFERENCE from sources]:** keep Hutan's three kinds; add a `Destructive` styling (Carbon danger tertiary: border + text in danger colour, no fill) so destructive confirms never use `Primary`.

## 2. Sizes for compact desktop

| Source | Heights | Horizontal padding | Type |
|---|---|---|---|
| Carbon ([specs](https://www.carbondesignsystem.com/building-blocks/core/components/button/specifications)) | xs 24, sm 32, md 40, lg 48 px | 16 px (icon 16×16, 8 px gap at ghost-with-icon) | label 14 px |
| Zed ([button_like.rs](https://raw.githubusercontent.com/zed-industries/zed/main/crates/ui/src/components/button/button_like.rs)) | Large 32, Medium 28, Default 22, Compact 18, None 16 px | Large/Medium 8 px; Default/Compact 4 px | — |
| VS Code ([button.css](https://raw.githubusercontent.com/microsoft/vscode/main/src/vs/base/browser/ui/button/button.css)) | short-label 28 px; line-height 16 | 4 px × 8 px; small 3 × 6 px | 12 px (small 11 px) |
| Radix ([docs](https://www.radix-ui.com/themes/docs/components/button)) | sizes 1–4, default "2" | — | — |

Apple's "hit region of at least 44x44 pt" is stated for touch/visionOS contexts ([Apple Buttons](https://developer.apple.com/design/human-interface-guidelines/buttons)); I did not find a desktop-pointer equivalent in Apple's pages, so I use the desktop precedents above.

**Recommended Hutan values [INFERENCE]:**

- Default control (toolbar, dialog, approval): height **28 px**, horizontal padding **12 px** (keeps the current value; on the 4-px base), label **12–13 px** medium.
- Compact control (inline, card footers): height **24 px**, padding **8 px**, label **12 px**.
- Icon-only button: **24 px** square visual, **28 px** hit region, glyph **16 px**; circular only for the pill style (`radius.pill`).
- Spacing: 8 px between related buttons, 12 px in dialog footers; never mix sizes inside one group (Carbon: "We do not recommend mixing different button sizes").
- Radius: `radius.control` 6 for rectangular buttons; `radius.pill` 999 for icon circles (matches `design.md`).

## 3. States

| State | Recommendation | Source |
|---|---|---|
| Enabled | Primary: solid accent + `on-accent` ink (`#1f1e1d`, already decided in clean-aesthetic). Secondary: 1 px `ui.border`. Ghost: no border. | clean-aesthetic §2.4 |
| Hover | Primary: accent at 88% (matches code). Secondary/ghost: `surface_hover`. NN/g: a "slight darkening" and a 150–200 ms delay before hover changes, so pointer drift does not flash. | [NN/g Button States](https://www.nngroup.com/articles/button-states-communicate-interaction/) (Apr 25, 2025); Zed: Subtle hover = ghost hover |
| Pressed | Apple: "Always include a press state for a custom button." NN/g: feedback within 100–150 ms. **[INFERENCE]** Hutan: one step darker than hover (accent 80%; secondary `surface_active` = hover + one step). | [Apple Buttons](https://developer.apple.com/design/human-interface-guidelines/buttons); NN/g (above); Zed `element_active` |
| Focus-visible | Keyboard only. NN/g: keep an outline; do not rely on colour alone. Carbon: `$focus` 2 px border + inset. Hutan: 2 px `ui.focus` ring with 1 px `bg` gap (D-C3). Appears ~100–150 ms after keyboard input. | [NN/g](https://www.nngroup.com/articles/button-states-communicate-interaction/); [Carbon specs](https://www.carbondesignsystem.com/building-blocks/core/components/button/specifications); Zed: outlined focus changes border to `border_focused` |
| Disabled | Lighter, lower contrast, but readable; NN/g: set `aria-disabled` so it stays focusable. Hutan: text `faint`, container/label opacity **0.45** (replaces 0.7 for rectangles; matches round buttons). Cursor default. VS Code uses 0.4. | NN/g (above); [VS Code button.css](https://raw.githubusercontent.com/microsoft/vscode/main/src/vs/base/browser/ui/button/button.css); [NN/g video, Aug 25, 2025](https://www.nngroup.com/videos/why-disabled-buttons-hurt-ux-and-how-to-fix-them/): "use them sparingly", "clearly explain why" |
| Loading | NN/g: spinner **left of label**, enabled look. Radix: `loading` keeps size and disables. **Hutan:** replace the leading icon with a 12–14 px spinner, keep width, disable re-click; no scrim. I found **no primary source for a loading overlay percentage**; if one is wanted, **[INFERENCE]** 0.5 opacity on the label only. | NN/g; [Radix](https://www.radix-ui.com/themes/docs/components/button) |
| Border | Only secondary (1 px `ui.border`) and destructive-outline (1 px danger). Radius 6 everywhere. | Carbon; Zed Outlined |
| Cursor | Pointer on enabled. shadcn's Tailwind v4 default is `cursor: default` and its docs add a rule to restore pointer. Disabled: default arrow [INFERENCE]; Zed uses `not-allowed`. | [shadcn](https://ui.shadcn.com/docs/components/radix/button); Zed |

**Evidence that focus/hover drift is a real defect:** Zed issue [#56533](https://github.com/zed-industries/zed/issues/56533) "Dialog button focus and hover states are visually indistinct across themes" (created 2026-05-12, closed, 8 comments, 0 reactions; read via `gh api`). Thin evidence: one issue, no reactions.

## 4. Labels

- **Sentence case.** Carbon: "sentence case, with only the first word in a phrase and any proper nouns capitalized." Hutan keeps this (clean-aesthetic L5). Apple's alert buttons use title style; Hutan diverges on purpose.
- **Verb + object**, specific. NN/g: labels should "clearly describe what the button will do," avoid generic "Continue" or "Next" ([NN/g](https://www.nngroup.com/articles/button-states-communicate-interaction/)).
- **"Cancel"** always for cancel; Apple: "Always use 'Cancel'" ([Apple Alerts](https://developer.apple.com/design/human-interface-guidelines/alerts.md)).
- **"OK"** only for purely informational alerts (same source).
- Ellipsis "…" when the button opens another window, view or app ([Apple Buttons](https://developer.apple.com/design/human-interface-guidelines/buttons)).

## 5. Placement

- Primary **trailing** (right) in dialogs and toolbars: Apple Toolbars: "Only specify one primary action, and put it on the trailing side" ([Apple Toolbars](https://developer.apple.com/design/human-interface-guidelines/toolbars.md)); Apple Alerts: default trailing, Cancel leading. Carbon: right-justified in dialogs and wizards.
- Cancel **leading**, secondary.
- Destructive: not default, not accent; placed on the far side away from Return (Apple Alerts: destructive style only for actions people "didn't deliberately choose", so use it sparingly).
- Toolbar: one `.prominent` action max; others "without borders" ([Apple Toolbars](https://developer.apple.com/design/human-interface-guidelines/toolbars.md)). Standard Back/Close icons, no label.

## 6. Destructive confirmation

- Stop-and-quit / stop-and-close dialogs (L3, CL-1): confirm is **Destructive** (danger border + text, no fill); Cancel is **Secondary** and the Esc target; no button is `Primary`. Apple: "you don't want to make a Cancel button the default button" ([Apple Alerts](https://developer.apple.com/design/human-interface-guidelines/alerts.md)), so Return should not activate the destructive action.
- **[INFERENCE]:** if one button must be default, make it Cancel, and mark it with the Apple-style default ring only on keyboard focus.

## 7. Keyboard

| Key | Behaviour | Source |
|---|---|---|
| Return | Activates the **default** (primary) button. | [Apple Buttons](https://developer.apple.com/design/human-interface-guidelines/buttons) |
| Esc | Activates Cancel. | [Apple Alerts](https://developer.apple.com/design/human-interface-guidelines/alerts.md) |
| Space | Activates the focused button. | **[INFERENCE]** (standard native behaviour; no primary source fetched) |
| Tab | Moves focus; disabled buttons stay focusable with `aria-disabled` (NN/g). Zed removes disabled from tab order instead. Pick one; Hutan: `aria-disabled` [INFERENCE]. | NN/g; Zed |

## Sources and method

Every primary source above was read directly; each link sits beside the claim it supports.

Search method:
- `gh search issues` on zed-industries/zed ("button focus", "focus ring") and microsoft/vscode ("focus visible button"); reactions via `gh api`. Read-only.
- `web_search` for Material 3 button specs, NN/g button labels, Linear button design.

Thin or unverified (stated, not padded):
- **Material 3**: `m3.material.io` pages returned no body (JS-rendered). A search summary gave default button 40 dp, state-layer opacities hover 8%, focus/press 10%. Not verified; not used in recommendations.
- **NN/g "button labels" and Apple push-buttons pages**: both 404; rules taken from the pages above.
- **Linear, Raycast, Arc**: no first-party button guidance found. Only third-party mirrors of "DESIGN.md" appeared in search; not cited.
- **Loading overlay %**: no primary source. Recommendation above is [INFERENCE].
- **Label style**: Carbon's sentence-case rule is from its typography section; Apple uses title style for alerts.
