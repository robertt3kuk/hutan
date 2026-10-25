# ACCOUNTS-MULTI — several accounts per harness, without touching tokens

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/ACCOUNTS-MULTI`

**Contract ownership:** **delegated**: additive protocol minor if needed (account id on session create / harness account list), own commit; ADR 0015 amendment written by you first and reviewed at merge.

## Context
Claude Code #18435 (1183 👍), #36151 (1065): people with work + personal subscriptions want to switch accounts per project/session (`docs/spikes/2026-10-25-user-needs/agents-cli.md`). ADR 0015: subscription logins are used only through harnesses; Hutan lists accounts and starts the harness's own login, never reads, stores or forwards tokens. Harnesses keep credentials in their config home: Codex `CODEX_HOME`, Claude Code `CLAUDE_CONFIG_DIR`, OMP its config dir (check `omp --help`/docs for the env var); Claude Code on macOS also uses the Keychain item named per config dir [verify].

## Items (in commit order)
1. **Design note + ADR 0015 amendment** (`docs/adr/0015-*.md` § Amendment: multiple accounts): an account = a named harness config home Hutan creates under its data dir (`accounts/<harness>/<name>/`) and passes via the harness's env var; Hutan never opens its contents except the harness's documented non-secret files it already reads; login = the harness's own login command run with that env var in a visible terminal; logout/remove = delete the home after confirm. Verify each harness's env var and where its credentials actually go (file vs Keychain) from docs and `--help` only, never by reading credential files. Record findings with sources in `docs/agents.md` § Accounts.
2. **Daemon**: account registry (name, harness, home path, label, created; no secrets), harness launch with the account's env var; default account per harness and per project/workspace (`[harness.<h>] account`, project override).
3. **UI**: Settings → Agents → Accounts: list per harness, *Add account…* (name → runs login in a terminal), *Set as default*, *Remove…*; Start agent sheet gets an account picker when a harness has 2+; the agent chat header shows the account label; USAGE-LIMITS windows are per account (its summary already keeps accounts — show each).
4. CLI `hutan account list|add|remove|default`, `hutan new --account`.
5. Docs: `docs/agents.md` § Accounts, `docs/integrations.md`.

## Acceptance
Daemon tests with fake harness binaries asserting the env var and that Hutan never reads the account home beyond an allowlist; registry persistence; defaults precedence (session > project > harness). GPUI tests: Accounts page, picker, header label. No real logins in tests; a manual check only with the user's consent (never on this machine's real harness homes).

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(ACCOUNTS-MULTI)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/accountsmulti/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
