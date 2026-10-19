# SK-1 — Send key: Return or ⌘Return, switchable everywhere a message is sent

Owner: implementer. Crates: `desktop` (and `settings` only if the legacy-alias helper lives there), docs. No protocol change.

The user (2026-10-20): "let's think about switching between ⌘Enter and Enter so it can be changed". Today `chat.send_on_enter` (bool, default false: Return = newline, ⌘Return = send) exists but is buried in Settings → Chat, applies only to the chat composer, and the other message inputs hard-code ⌘Return.

## Change

1. **Setting.** Replace the bool with an enum `chat.send_key = "cmd-enter" | "enter"` (default `cmd-enter`, unchanged behaviour). Read the old `chat.send_on_enter` as a legacy alias (`true` → `enter`), the same way `appearance.chrome` is handled; writes go to `send_key`. Settings → Chat shows a two-choice dropdown "Send messages with": "⌘Return (Return adds a line)" / "Return (⇧Return adds a line)". Update `settings_labels.rs`, the settings schema/search, and existing tests that pin `send_on_enter` (migrate them, do not duplicate).
2. **One rule for every message composer**, read live from the setting:
   - chat composer (harness chats, direct chats, Mawas Home — `chat/view.rs`, incl. the Vim Normal-mode rule from V-1);
   - Start sheet prompt field (`launcher.rs`: Return in the prompt currently always adds a line, ⌘Return starts);
   - review comment input and "send review" in the Changes tray (`changes/tray.rs`);
   - free-text answers in question cards (`chat/question_card.rs`).
   With `enter`: Return sends, ⇧Return (and ⌥Return) adds a line; ⌘Return still sends (never a dead key). With `cmd-enter`: Return adds a line, ⌘Return sends. IME composition: a Return that commits marked text never sends. Forms that are not messages (merge/delivery commit message, memory editor, settings fields) keep their current keys — list them in the docs.
3. **Quick switch in place.** The composer hint ("⌘↩ send · ↩ newline") becomes a small clickable control; clicking opens a two-item menu (the same two labels) that writes the user setting; every open composer and hint updates at once. Same control on the Start sheet prompt hint. Palette command "Chat: Toggle Send Key" (toggles and shows a short toast "Return now sends" / "⌘Return now sends").
4. **First-run choice.** The setup flow's Mawas step (or the first time a composer is focused, if setup was already done — once, dismissible, persisted) offers the same choice in one line. Keep it unobtrusive; no modal.
5. Docs: `docs/agents.md` § composer, `docs/design.md` (Keyboard / composer rule, list of message composers vs forms), `docs/assistant.md` if it mentions the keys.

## Tests

- Settings: legacy `send_on_enter = true` reads as `enter`; default `cmd-enter`; writing writes `send_key`.
- GPUI: for each composer listed in item 2, with each value: Return sends or adds a line as specified; ⇧Return adds a line under `enter`; ⌘Return sends under both; marked-text Return does not send (chat composer at least).
- GPUI: clicking the hint and choosing the other option flips behaviour in an already open composer without reopening; palette toggle works.

## Acceptance

Gate green with `CARGO_BUILD_JOBS=4` and `cargo nextest run --workspace --locked -j 4` (the user's laptop overheats; never more): fmt, clippy `-D warnings`, nextest, cargo-deny. One commit on `slice/SK-1`. Do not edit `core`, `protocol`, `docs/status.md`, `docs/roadmap.md`. Every `hutan`/`hutand`/`hutan-desktop` you run needs `HUTAN_HOME=<temp dir>` inline; never touch `/Applications/Hutan.app`, `~/Library/Application Support/Hutan`, `~/.omp`, `~/.claude`, `~/.codex` or the Keychain.
