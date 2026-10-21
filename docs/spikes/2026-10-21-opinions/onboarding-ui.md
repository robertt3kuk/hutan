# Community opinion: onboarding, permissions, default UI (2025–2026)

Topic: `onboarding-ui`. Read-only research. Only this file was written.

## Method and limits

- **Hacker News (primary, full threads).** Algolia HN API: `search` and `search_by_date`, `tags=story` and `tags=comment`, `numericFilters=created_at_i>1735689600` (2025-01-01 onward). Short tool-name queries worked; long multi-word queries returned nothing. Item trees were read through `/api/v1/items/<id>`, then filtered with a keyword regex. Queries included: onboarding, first run, setup wizard, permissions prompt, login required, no account, plus product names (Warp, Zed, Ghostty, cmux, Cursor, Codex, Claude Code). Scores below are HN story points and total comment counts.
- **Reddit (partial, snippets only).** Direct access is blocked. `www.reddit.com/*.json` returned 403. Thread URLs returned a "Prove your humanity" challenge page. `old.reddit.com` redirects to login. `api.pullpush.io` returned 429. Reddit evidence therefore comes only from `web_search` with `site:reddit.com` snippets. These are not full threads and carry no score or comment counts. They are cited by thread ID.
- **Primary vendor and project sources** were read directly: Warp blog, Ghostty GitHub discussion, Anthropic engineering and product posts.
- **GitHub issues** were searched with `gh search issues` (read-only), restricted to `openai/codex` and `anthropics/claude-code`.
- **Thin areas:** no HN thread on the 2-step first run or on onboarding flows specifically. Almost all onboarding evidence is about login walls, permission prompts, and defaults.
- Dates: the Warp no-login blog is from 2024-11-22, outside the 2025–2026 window. It is included because 2026 threads still cite it.

## Findings

### A. Login walls and account requirements (decision c)

**Complaints (agree: about 8 voices; disagree: 1 vendor voice)**
- egorfine, on Warp open-source (HN 47936264, 2026-04-28, 373 pts / 118 comments): "requiring login (into a console!!) … heaps of telemetry, so a privacy nightmare. This is a terminal that is not designed for users." https://news.ycombinator.com/item?id=47936264
- em-bee, same thread, on the 2026 RPM build: the build "doesn't seem to work without login … stuck at a prompt asking me to log in with no option to skip it." https://news.ycombinator.com/item?id=47936264
- Squarex, same thread: asked for "a single 'turn off all the AI stuff' button." Warp's founder replied that one exists. https://news.ycombinator.com/item?id=47936264
- HN 47937349, same topic (2026-04-28, 237 pts / 173 comments): a second copy of the login and telemetry discussion. https://news.ycombinator.com/item?id=47937349
- Reddit r/commandline, thread 1gxh3jm, title "Warp terminal now useable without login" (snippet only, no score). https://www.reddit.com/comments/1gxh3jm
- Reddit r/devops (snippet only): a login requirement "may collect data … deal breaker." Thread not identified.
- Reddit r/warpdotdev, thread 1v58iv2 (snippet only). Title: "I forked Warp into a local-first terminal: no login, no AI, no telemetry." A community fork built on the no-login premise. https://www.reddit.com/comments/1v58iv2
- Zed, HN 46505816 (2026-01-05, 2 comments): "Does Zed still have an unremovable login button right in the window title?" https://news.ycombinator.com/item?id=46505816
- HN comment 49804792 (reply on the Claude Opus 5.5 thread, story 49803892): a user says Claude Code Desktop keeps "kicking me back to login every week or so, and forgetting whole state." https://news.ycombinator.com/item?id=49804792
- old.reddit.com login wall (HN 49326106, 2026-08-17, 71 pts / 73 comments). shmde: "Such user hostile stuff." suppy123456: "very intrusive login requirement to see content." https://news.ycombinator.com/item?id=49326106

**Praise or neutral (agree: 2; disagree: 0)**
- Warp's founder on the reversal (Warp blog, 2024-11-22): "Starting today, you can use Warp without signing up for an account or logging in first." Warp credits "hundreds of developers who let us know that the login requirement was keeping you from using Warp." https://www.warp.dev/blog/lifting-login-requirement
- zachlloyd, HN 47936264: "we got rid of this requirement a couple of years ago. No login required at all, except for using AI and team features." Also: "hbn: login is no longer required." https://news.ycombinator.com/item?id=47936264
- Zed, HN 44319225 (2025-06-19): "login is only required for internet features." Neutral, but it shows that a login only for optional features does not draw the same complaints. https://news.ycombinator.com/item?id=44319225

**Counter-position (1 vendor voice)**
- Warp blog: "We still think the best experience comes from logging in" and the login exists for team and AI features. It was lifted because of user pressure, not because the company dropped the idea. https://www.warp.dev/blog/lifting-login-requirement

**Local-first and no-cloud as a positive (market signals, not opinions)**
- Gander, "an Android file viewer that asks for no permissions" (HN 49119425, 2026-07-31, 211 pts / 79 comments). This is a launch, not an opinion. It shows that a "no permissions" pitch can trend. [INFERENCE] This is a weak signal. https://news.ycombinator.com/item?id=49119425
- TERMy, "A fast terminal assistant that does not use LLMs" (HN 49562219, 2026-09-04, 225 pts / 45 comments). mpalmer: "this flavor of specialization gives the user a win on privacy and speed." https://news.ycombinator.com/item?id=49562219

### B. Permission prompts and approval fatigue (decision a)

**Primary data (vendor-run, self-reported)**
- Anthropic engineering post, 2026-03-25: "By default, Claude Code asks users for approval before running commands or modifying files… Manual prompts sit in the middle, and in practice users accept 93% of them anyway." https://www.anthropic.com/research/claude-code-auto-mode
- Anthropic product post, 2026-08-07: "users approve 97% of permission prompts in Claude Code." It also reports that for plan approval, users "reject 39% of them," but for individual permission requests "the rejection rate is only 3%." https://claude.com/resources/articles/auto-mode-default-in-claude-code
- Same post, from a 1,053-tester controlled study run by a research vendor: "human review caught just 13.6% of dangerous commands, while auto mode caught 89%." Human block rate reportedly fell from about 17% to about 5% after 50 or more prompts. https://claude.com/resources/articles/auto-mode-default-in-claude-code
- Anthropic made auto mode the default for Pro, Max, and Team plans from 2026-08-14. Source: https://claude.com/blog/auto-mode-default-in-claude-code [INFERENCE] The vendor has a commercial interest in this claim. Treat the numbers as company-reported, not independently verified.

**Community complaints about per-action prompts (agree: about 7 voices, all 2025–2026; disagree: 0 found)**
- HN 48102087 (2026-05-11, 4 pts / 15 comments). A product-maker's notes: users want the tool to "not ping you every 10 seconds asking permission to exe[cute]." https://news.ycombinator.com/item?id=48102087
- Reddit r/ClaudeAI 1puqrvc, "Claude code annoyingly asking for permissions" (snippet only). https://www.reddit.com/comments/1puqrvc
- Reddit r/ClaudeCode 1rvo79u (snippet only): "Even when I select 'Always allow for local actions', Claude still repeatedly asks for permission… This slows down the workflow." https://www.reddit.com/comments/1rvo79u
- Reddit r/ClaudeCode 1sn512w (snippet only): "This is driving me a little crazy. Every time Claude tries to edit something…" https://www.reddit.com/comments/1sn512w
- Reddit r/codex 1u9ln9e (snippet only): "Codex asking for approval even with FULL ACCESS." https://www.reddit.com/comments/1u9ln9e
- Reddit r/OpenaiCodex 1ncwkws (snippet only): "You either have to painfully select 'allow' over and over again OR you have to let it run in yolo mode." https://www.reddit.com/comments/1ncwkws
- Reddit r/ClaudeCode 1sfr7fk and 1tvnpow: the same pattern ("prompts for permission on every single file write"; "constantly asks for a confirmation to run commands"). Snippets only.

**Consent before silent or background behavior (agree: about 4 voices; disagree: 1)**
- HN 50060694 "Codex CLI installs a persistent, self-updating background daemon by default" (2026-10-12, 609 pts / 287 comments). This is the largest 2026 thread on this topic. Top comments (depth-1 only, 39 total):
  - wtallis: "Serious tools shouldn't do this." Opt-in is fine, but forced "stable" updates that change UI and behavior are not acceptable. https://news.ycombinator.com/item?id=50060694
  - swatcoder: "Auto-update systems have value but are not a suitable default for professional tools." https://news.ycombinator.com/item?id=50060694
  - whimblepop: "There aren't any documented settings to disable this, but there is a feature flag it currently sits behind." Undocumented opt-out. https://news.ycombinator.com/item?id=50060694
  - dllu: the daemon "would randomly ruin the permissions and block the goal as .git became read-only." https://news.ycombinator.com/item?id=50060694
  - Counter-voice, dingaling911: "This stuff is moving fast, should update automatically. This is kind of standard now." https://news.ycombinator.com/item?id=50060694
- GitHub openai/codex issue 51763 (2026-10-07, 1 comment): "Background daemon auto-update drains active session and leaves daemon incompatible with current Codex client." https://github.com/openai/codex/issues/51763 (corroboration; few comments)
- HN 47886763 "Codex macOS app switches to Fast speed after update without asking" (2026-04-24, 22 pts / 1 comment). Small, but the same complaint: a silent state change. https://news.ycombinator.com/item?id=47886763
- HN 43132313 "Why is Cursor IDE accessing all my env vars?" (2025-02-21, 15 pts / 25 comments). jimsmart: "I don't understand why they might want to serialize/access all of my env vars." Users saw a firewall alert on first launch. https://news.ycombinator.com/item?id=43132313 (2025, outside the main window, but still relevant)
- Zed, HN 46498735 and 47949027: a GitHub sign-in prompt asking to "act on behalf" of the user. mfontani: "That seems risky." https://news.ycombinator.com/item?id=47949027

### C. Default layout, clutter, progressive disclosure (decision b)

**Supports clean, minimal defaults (agree: about 5; disagree: 0 in these threads)**
- Ghostty maintainer mitchellh, "Ghostty's 'Zero Config' Defaults" (GitHub Discussion #2363, opened 2024-10-02, closed 2025-07-14, 36 comments / 106 replies): "Ghostty **already feels really good** out of the box compared to other terminal emulators." He describes his own personal config as three lines and says he wants Ghostty to be acceptable "such that a lot of people can install Ghostty, run it, and not feel a need to customize anything." https://github.com/ghostty-org/ghostty/discussions/2363 (Note: the discussion is from 2024, but the defaults shipped with 1.0 in 2025.)
- Ghostty, HN 46568794 (2026-01-10): geon, "feels just like the native terminal." https://news.ycombinator.com/item?id=46568794
- Zed, HN 46498735 "I switched from VSCode to Zed" (2026-01-05, 453 pts / 99 comments): drcongo, "The UI design is perfect … like a really ergonomic pair of slippers." giancarlostoro: "VS Code but fully native." https://news.ycombinator.com/item?id=46498735
- Warp, HN 47937349: alokedesai (Warp) says they added controls "to configure how much (or little) UI." This is a vendor example of progressive disclosure. https://news.ycombinator.com/item?id=47937349

**Contradicts forced or cluttered layouts (agree: about 6; disagree: 2 defenders)**
- Zed 1.0, HN 47949027 (2026-04-29, 2153 pts / 187 comments): "agent first layout which I cannot undo." The reply was "collapse the sidebar." https://news.ycombinator.com/item?id=47949027
- Same thread, a search that opens a new tab (Aldipower). Counter: pastel8739 likes it. https://news.ycombinator.com/item?id=47949027
- Same thread, bmandale: "There are no 'defaults that work for everyone'." https://news.ycombinator.com/item?id=47949027
- Reddit r/ZedEditor 1su6w0z "Turn of agentic layout?": "Since they changed the defaults…" (snippet only). https://www.reddit.com/comments/1su6w0z
- Reddit r/ZedEditor 1r8cpck "Zed UI/UX clarity issues?" (snippet only): low contrast, panels hard to tell apart. https://www.reddit.com/comments/1r8cpck
- Reddit r/ZedEditor 1thf63f (snippet only): a debate over moving the agent sidebar left. https://www.reddit.com/comments/1thf63f
- Warp, HN 47937349: Esophagus4 says the UI "has changed a lot recently … more intimidating." pzo says it is "too flat." https://news.ycombinator.com/item?id=47937349
- cmux, HN 47079718 (2026-02-19, 198 pts / 77 comments): "Can't switch top tabs" (twostorytower) and "a more basic UI" (james2doyle). Praise is for vertical tabs and notifications. https://news.ycombinator.com/item?id=47079718
- HN 49743291 "Why does cmux need 200 settings?" (2 pts). Thin. https://news.ycombinator.com/item?id=49743291

**Config burden (mixed)**
- Ghostty config, HN 47206009 (2026-03-01, 863 pts / 67 comments). KingMob: "a config page" would help, and debugging config errors was frustrating. saberience: it can't be configured to the level of iTerm. https://news.ycombinator.com/item?id=47206009
- Ghostty GTK rewrite, HN 44905808 (2025-08-14, 428 pts / 21 comments). corsica: "The only UI Ghostty has is tabs and the context menu." Neutral description, not a complaint. https://news.ycombinator.com/item?id=44905808

### D. Performance and RAM (adjacent to onboarding and default UI)

- Ghostty memory leak, "Finding and fixing Ghostty's largest memory leak" (HN 46568794, 2026-01-10, 632 pts / 25 comments). Users reported 20GB+; neobrain reported an OOM. https://news.ycombinator.com/item?id=46568794
- Zed, wgpu switch (HN 47002825, 2026-02-13, 317 pts / 28 comments). nu11ptr: the native renderers on macOS and Windows were "less memory intensive." https://news.ycombinator.com/item?id=47002825
- Zed 1.0 thread: bicepjai says Claude Code in Zed "ballooned to all 128GB RAM." https://news.ycombinator.com/item?id=47949027
- Codex app, bundling LibreOffice (HN 49527396, 2026-09-01, 495 pts / 32 comments). cpursley: "heavy and dog slow." https://news.ycombinator.com/item?id=49527396
- HN 48102087 (2026-05-11): jdabney wants tools that don't "take up GBs of RAM for only having a few windows open" and prefers something "not based on Electron." dlcarrier: "Mostly the bloat." https://news.ycombinator.com/item?id=48102087
- Reddit r/codex 1u3o9eg (snippet only): "Codex app overloads the Mac… One session can grow to a gigabyte or more." https://www.reddit.com/comments/1u3o9eg
- Reddit r/ClaudeCode 1v4fwjv (snippet only): "Switched to the Claude Code desktop app. It lags the same as the browser." https://www.reddit.com/comments/1v4fwjv

## Verdicts

| Decision | Verdict | Evidence count (agree / disagree) |
|---|---|---|
| (a) 2-step first run with just-in-time permission asks | **mixed** | Against frequent per-action prompts: about 7 Reddit threads, 1 HN thread, and Anthropic's company-reported 93–97% approval data (2 posts). For consent before silent or background behavior: about 4 HN voices, plus 1 GitHub issue, versus 1 counter-voice. No evidence found on a 2-step first run itself. |
| (b) Clean default layout, progressive disclosure | **mixed** | Supports minimal defaults: about 4 voices (Ghostty maintainer, Zed UI praise, Warp configurable UI). Contradicts forced or cluttered layouts: about 9 voices (Zed agent-first layout, Warp "intimidating"/"too flat", cmux "basic UI", Zed "defaults that work for everyone"). |
| (c) Local-first, no account as a selling point | **supports** | About 10 voices complain about login walls and account requirements, including Warp's own reversal. About 2 praise or neutral voices. Counter-voice: 1 vendor (Warp, "the best experience comes from logging in"). |

## Five takeaways for Hutan

1. **Make permission prompts rare and meaningful.** Anthropic's own data says users approve 93–97% of per-action prompts. Its plan approvals were rejected 39% of the time, versus 3% for individual prompts. Ask once for a plan or a scoped permission, not on every action. [INFERENCE: this applies to Hutan's agent-approval queue.]
2. **Never change behavior silently.** The largest 2026 HN thread on agent tools (Codex daemon, 609 pts) and the Codex Fast-speed complaint both reject silent defaults. Any background process, auto-update, or setting change needs an explicit, documented opt-out.
3. **No account on first run; make login a feature gate, not a wall.** Warp had to drop its login requirement after "hundreds of developers" complained. Zed's login is tolerated only for internet features. Hutan's local-first stance is a real selling point for this audience.
4. **Keep the default UI quiet, but never force a layout change on existing users.** The Zed 1.0 backlash ("agent first layout which I cannot undo") was about forced changes, not about minimalism. Offer a one-step way back (collapse, or a "classic" toggle) and say so on first run.
5. **Be lean on memory and RAM, and say so.** Several threads mention Electron, RAM, and "bloat" as reasons to switch. A native Rust/GPUI app is a concrete advantage here, but only if it is measured and reported.

## Search log (reproducibility)

- HN Algolia: `search` and `search_by_date`, tags `story` and `comment`, `numericFilters=created_at_i>1735689600`. Queries: onboarding, first run, setup wizard, permissions prompt, too many prompts, asks permission, login required, no account, Warp, Zed, Ghostty, cmux, Cursor, Codex, Claude Code.
- HN item tree: `https://hn.algolia.com/api/v1/items/<id>`, filtered by keyword regex.
- Reddit: `site:reddit.com` `web_search` queries, e.g. "Claude Code desktop app permission prompts annoying onboarding login", "Codex app macOS permissions prompts too many approvals first run setup", "Zed UI clarity", "Warp no login local-first". Direct Reddit access was blocked (see Method).
- GitHub (read-only `gh search issues`): `daemon` in `openai/codex`; `permission prompts` in `anthropics/claude-code`.
- Primary pages read: warp.dev/blog/lifting-login-requirement; github.com/ghostty-org/ghostty/discussions/2363; anthropic.com/research/claude-code-auto-mode; claude.com/resources/articles/auto-mode-default-in-claude-code.
