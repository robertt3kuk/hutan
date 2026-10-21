# Community opinion: embedded editor, Vim mode, LSP handling (for Hutan)

Scope: Hacker News (Algolia + full thread reads), GitHub (read-only `gh`), product docs, dev forums, and Reddit via `web_search` snippets only. Scores are HN points / comment counts, GitHub reaction counts, or forum like counts. Voice counts are my own tallies from the threads read, not exhaustive. [INFERENCE] marks my reading, not an observed statement.

## Method and limits
- **Reddit is blocked.** `reddit.com/.../search.json` and `old.reddit.com/.../search.json` both returned HTTP 403 via curl. The reader returned "You've been blocked by network security." Reddit evidence is limited to `web_search` snippets, which have no scores. Treat it as anecdotal.
- **HN.** Algolia search `https://hn.algolia.com/api/v1/search?query=...&tags=story|comment`, sorted by date and relevance, for Zed/LSP/download/consent, Vim/Neovim/modal, and agent/terminal/IDE. Full trees read for the threads below.
- **GitHub (read-only).** `gh search issues --sort reactions --order desc --created ">=2025-01-01" --json ...` for Vim and LSP topics; `gh issue view <n> --json reactionGroups` for counts.
- **Docs and blogs.** zed.dev docs (vim, configuring-languages), Zed Decoded: Vim blog, Gram editor docs.
- **Forums.** forum.cursor.com (two topics read in full).

## Findings

### Decision (c): LSP servers are never auto-downloaded

| # | Opinion | Short quote | Link | Score / comments | Agree / disagree |
|---|---|---|---|---|---|
| c1 | Zed downloads Node and npm packages without consent (open GitHub issue) | "Zed downloads NodeJS binary and npm packages from Internet without user's consent" | https://github.com/zed-industries/zed/issues/12589 (opened 2024-06-02, OPEN) | 309 👍, 26 😕, 10 👀, 6 ❤️, 5 🚀, 4 😄, 1 👎; 94 comments | 309 (+) vs 1 👎 [reaction counts] |
| c2 | Silent download is a security/trust problem | "I don't want to be using a language server… most of them execute arbitrary code" (eptcyka) | https://news.ycombinator.com/item?id=40902826 | 239 pts, 173 comments (2024-07-08) | ~12 against silent download (eptcyka, compootr, daghamm, bcardarella, seeknotfind, mxsjoberg, xpe, teo_zero, chillfox, biosboiii, DanielVZ, WuxiFingerHold) vs ~4 for zero-config/anti-popup (theultdev, diego_sandoval, cedws, whazor) |
| c3 | Users want to use system-installed servers, not Zed's copies | "The biggest problem I personally have with Zed design is it downloading it's own binaries for LSP servers instead of using ones already installed in the system." (mrsssnake, 2026-05-01) | https://news.ycombinator.com/item?id=47981277 (in thread 47949027, Zed 1.0) | 2153 pts on the story | 1 explicit (plus xpe's quote of the issue: "Ideally you would be able to turn off auto-download but still be able to use a [language server] if it's already on the users system") |
| c4 | Downloads without asking, reported as a bug in use | "spawned nodejs servers (downloaded without asking for permission!)" (drannex, 2026-02-13) | https://news.ycombinator.com/item?id=47007260 (thread 47002825, Zed wgpu) | 317 pts on story | 1 explicit |
| c5 | Corporate AV blocked adoption | "This broke Zed for me and had to go back to Neovim at my workspace." (DanielVZ) | https://news.ycombinator.com/item?id=40902826 | (same thread) | 1 |
| c6 | Zero-config convenience and anti-popup preference (counter-view) | "Who wants to approve and configure all of their language servers?" (theultdev) and "Popups are one of the reasons I stopped using VSCode." (cedws) | https://news.ycombinator.com/item?id=40902826 | (same thread) | ~4 against consent prompts |
| c7 | Consent as a sign of the right design | "I don't really see the big deal here… is there ever a time you would deny the download?" (theultdev) vs teo_zero "who wants to be asked… " [reply] | https://news.ycombinator.com/item?id=40902826 | (same thread) | The reply side is the stronger view |
| c8 | Zed's own docs: auto-download is the default; opt-out is per server | "Automatic Download: When you open a file with a matching file type, Zed automatically downloads the appropriate language server." and `ignore_system_version` (default false): "Whether to fetch the binary from the internet, or attempt to find locally." | https://zed.dev/docs/configuring-languages | Doc, not a vote | Confirms the default that #12589 objects to |
| c9 | A fork takes the opposite stance | "The Zed editor will do this without asking, but Gram tries not to do anything like downloading binaries from the internet without explicit permission to do so." | https://gram-editor.com/docs/language-servers/ | Doc, no score | One data point (forks are a weak signal) |
| c10 | Claude Code's LSP prompts to install a server, which surprised a user | "I was completely blindsided when my claude asked if I wanted to install a go lsp." (JamesSwift) | https://news.ycombinator.com/item?id=46355165 (Claude Code native LSP, 2025-12-22, 511 pts, 339 comments) | 511 pts, 339 comments | Mixed: the prompt is the consent path; the surprise is the complaint [INFERENCE] |
| c11 | Related Zed issue: json-language-server re-downloaded on every launch | "json-language-server appears to be downloaded every time Zed is opened" | https://github.com/zed-industries/zed/issues/22241 (2024-12-19, CLOSED) | 1 👍, 1 comment | Thin |

Counter-check: searches for 2025+ Zed issues about a "use system binary / no-download" option returned nothing beyond #12589 and the HN comments. Treat that absence as thin.

**Verdict (c): SUPPORTS.** Strongest signal is #12589 (309 👍 vs 1 👎, open since 2024-06-02). Repeated 2024–2026 HN complaints point the same way (c2, c3, c4). The counter-view (c6) favors zero-config and anti-popup, not silent downloads. It does not defend silent download; it argues for fewer prompts. [INFERENCE] The consent prompt costs convenience, so the design needs batching and a "use system binary" path.

### Decision (a): Built-in editor with first-class Vim mode is a competitive advantage

| # | Opinion | Short quote | Link | Score / comments | Agree / disagree |
|---|---|---|---|---|---|
| a1 | Zed's built-in Vim is praised | "Zed's vim emulator is one of the best I've used in an IDE." (jkl5xx) | https://news.ycombinator.com/item?id=44673096 (thread 44660519, Zed disable AI, 2025-07-23, 577 pts) | 577 pts | 1 explicit; rebuttal to a1-counter |
| a2 | Switched editors for Vim | "Zed Vim Mode" switch from Vim because a background process edits files | https://news.ycombinator.com/item?id=48859439 (Ask HN Vim in AI era, 2026-07-10, 49 pts) | 49 pts, 46 top-level | 1 explicit |
| a3 | Liked the Vim mode on switch | "I love the vim mode too." (xutopia) | https://news.ycombinator.com/item?id=46498735 (VSCode→Zed, 2026-01-05, 453 pts) | 453 pts | 1 explicit |
| a4 | Vim emulation not good enough | "Zed's vim emulation isn't quite up to par yet." (lyall, 2025-05-29) | https://news.ycombinator.com/item?id=44125746 | Not scored | 1 |
| a5 | Modality is an afterthought in Zed | "Zed is not a great modal editor, modality is a strapped on afterthought." (eikenberry) | https://news.ycombinator.com/item?id=44660519 | 577 pts | 1 (rebutted by jkl5xx) |
| a6 | Vim blocker: no .vimrc support | "I have a similar issue with vim support… don't currently support pointing to my own .vimrc. This is unfortunately a dealbreaker for me." (cpt_sobel) | https://news.ycombinator.com/item?id=46498735 | 453 pts | 1 explicit |
| a7 | Desire for a fast, modal editor | "We really need someone to invest in making a Fast, modal editor." (whinvik) | https://news.ycombinator.com/item?id=44660519 | 577 pts | 1 explicit |
| a8 | Vim users keep nvim/tmux beside the agent, not inside the editor | Examples: perplex (Neovim + Claude Code in split), philipportner (tmux + nvim, agent in another window), boriselec (claude CLI inside nvim), anonymid ("prefer my harness inside of nvim") | https://news.ycombinator.com/item?id=48859439 (2026-07-10, 49 pts) | ~15 terminal/nvim-split voices; ~5 non-AI (cryo32, simianwords, Apreche, mtklein) | Most common pattern |
| a9 | AI integration in IDEs is not worth it compared with the CLI | "I never once felt that any AI integration in IDE's was actually worth using over a cli" (quoted by iLemming) | https://news.ycombinator.com/item?id=47319071 (Emacs and Vim in the Age of AI, 2026-03-27, 201 pts) | 201 pts, 49 comments | ~5 terminal-first (47625141: "I prefer cli based coding agents… open neovim on the left, lazygit below"; 47150412; 47537732; 47143754) vs ~1–2 IDE-integrated |
| a10 | Vim is worth learning, and Vim mode in IDEs is common | "vi is part of POSIX"; Vim mode in IDEs is widely used (speedgoose, lyaocean) | https://news.ycombinator.com/item?id=47085255 (Ask HN Is it worth learning Vim in 2026?, 2026-02-20, 35 pts) | 35 pts, 24 top-level | ~12 yes / ~4 no [INFERENCE on the count] |
| a11 | Cursor's agent window keeps disabling the user's Vim extension | Cursor sets `"vim.disableExtension": true`; Cursor says it is "a known bug on our side" | https://forum.cursor.com/t/cursor-keeps-disabling-vim-mode/163875 (2026-06-23, 19 likes, 11 posts, auto-closed 2026-07-30) | 19 likes | Multiple users report it |
| a12 | Cursor CLI Vim is incomplete | Ctrl-A leaks as Emacs binding; `cc` missing; Cursor staff confirm "known issues" | https://forum.cursor.com/t/vim-mode-in-terminal-incomplete-vim-keybinding-support-emacs-bindings-leak-missing-cc-command/151395 (2026-02-10) | Forum, likes not captured | Staff confirm |
| a13 | Vim extension reliability in VS Code | VSCodeVim issues: #9623 "made extension unusable" (14 comments); #9717 lag after ~10 min (open, 15); #9668 Chinese input repeats (31) | https://github.com/VSCodeVim/Vim/issues/9623 and siblings | Comment counts only | Thin [INFERENCE: reliability is the pain] |
| a14 | Zed demand for vim features (GitHub) | #4642 Helix keymap (102 comments, closed); #4930 easymotion vim (39, open); #14801 Flash.nvim-style search (77, open); #4456 vimrc support (17, closed 2023); #30081 Agent Panel vim mode (11, closed, 2025-05-07) | https://github.com/zed-industries/zed/issues/4642 (and others) | Comment counts (reactions not captured for all) | Demand for vim features is real |
| a15 | Reddit, anecdotal | "It's vim mode is like having an entire vim experience out of the box" (r/neovim, 1qmxnpv); "From Neovim to Zed. What I like and what I miss so far." (r/ZedEditor, 1g1yu95, Oct 2024) | https://www.reddit.com/r/neovim/comments/1qmxnpv/ ; https://www.reddit.com/r/ZedEditor/comments/1g1yu95/ | No scores (snippet only) | Thin |

**Verdict (a): MIXED.** Editor-level Vim praise exists (a1–a3). Complaints also exist (a4–a7). The largest group keeps Vim outside the agent tool (a8, roughly 15 voices), and terminal-first voices dominate AI discussions (a9). [INFERENCE] Vim users do not cite the built-in Vim as a reason to adopt an IDE; they cite .vimrc and keeping nvim. The strongest advantage signal is the failure of other agent-IDEs' Vim (a11, a12) and the .vimrc gap (a6). That supports building Vim into an agent-first tool, not treating it as a headline feature.

### Decision (b): Vim mode is off by default, with a first-run question

| # | Opinion | Short quote | Link | Score / comments | Agree / disagree |
|---|---|---|---|---|---|
| b1 | Zed asks on the welcome screen, default off | "you'll see a checkbox on the welcome screen that allows you to enable vim mode" | https://zed.dev/docs/vim | Doc | Product precedent, not a vote |
| b2 | Zed's Vim-adjacent extras are not on by default | "not enabled by default" (AnyQuotes / MiniQuotes) | https://zed.dev/docs/vim | Doc | Product precedent |
| b3 | Users want toggles and per-filetype control | "Setting to enable Vim Editor only for certain filetypes" (VSCodeVim #6151, 2020, old): "It is annoying to enable/disable…" | https://github.com/VSCodeVim/Vim/issues/6151 | Old (2020) | Thin, old |
| b4 | Product overrides a user's Vim setting | Cursor disables the Vim extension in the agent window (a11) | https://forum.cursor.com/t/cursor-keeps-disabling-vim-mode/163875 | 19 likes, 11 posts | Multiple users |
| b5 | Default-on Vim is not discussed | No direct comment found on the default state of Vim mode in any read thread | — | — | Absence |

**Verdict (b): MIXED [INFERENCE].** There is no direct community evidence about the default. Indirect signals:
- Zed's own opt-in welcome checkbox (b1, b2) is the nearest precedent.
- Users complain when a host silently changes Vim state (b4), which favors a stable and explicit setting over automatic toggling.
- Users ask for toggles (b3), but that is an old thread.

Net: a first-run question plus a stable setting fits what is visible. It is not proven that default-off is what users prefer.

## Takeaways for Hutan
1. **Do not auto-download LSP binaries.** Silent download is the most repeated trust complaint about Zed (#12589, 309 👍 vs 1 👎; HN 40902826, 173 comments; HN 47981277 and 47007260 in 2026). Prefer a system binary when one exists, and ask before any download.
2. **Keep consent prompts rare.** The convenience camp (c6) says popups are the cost. Batch the prompt per workspace or language and offer "use installed server" as the default path.
3. **Vim must read `.vimrc`-level config, or it loses users.** One explicit dealbreaker (a6) and repeated "not up to par" comments (a4, a5) point this way. [INFERENCE: this is a small sample.]
4. **Keep Vim state stable in agent surfaces.** Cursor's failures (a11, a12) are the failure mode to avoid. Hutan's embedded editor should own its Vim state and not let an agent pane disable it.
5. **Position the editor as a secondary surface.** The terminal-first and nvim-beside-agent pattern (a8, a9) is the most common stance. Hutan's differentiation is the needs-you queue and review, not the editor. Use Vim to keep the power-user audience, not as the headline.

## Verdicts (per decision)
- **(a) Built-in editor with first-class Vim mode is a competitive advantage: MIXED.** Support: a1, a2, a3 (3 explicit). Against: a4, a5, a6, a7 (4 explicit), plus a8 (~15 keep nvim outside the agent) and a9 (~5 terminal-first).
- **(b) Vim mode is off by default, with a first-run question: MIXED [INFERENCE].** Support: b1, b2 (Zed docs precedent). Against or unclear: b3 (old), b4 (silent overrides). Direct community evidence on the default: 0 voices.
- **(c) LSP servers are never auto-downloaded: SUPPORTS.** Support: c1 (309 👍 vs 1 👎), c2 (~12 against silent download vs ~4 for convenience), c3, c4, c9 (Gram fork). Against: c6 (~4 anti-popup voices), which argues for fewer prompts, not silent downloads.

## Thin-evidence notes
- Reddit: no scores, snippet-only.
- Cursor Vim: bugs and reliability, not sentiment. Two forum topics only.
- Zed system-LSP option: no 2025+ GitHub issue asking for a system-only option was found. The `ignore_system_version` setting exists (c8).
- Vim default state: no direct evidence found.
