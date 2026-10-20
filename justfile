# Quality gate and dev entry points. `just check` must be green before every merge.

default: check

# Full gate: format, lint, tests, licenses/advisories.
check: fmt-check clippy test deny

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all --check

# The gate runs at nice 10 so the editor, terminal and agents stay responsive while it builds.
clippy:
    nice -n 10 cargo clippy --workspace --all-targets --locked -- -D warnings

test:
    nice -n 10 cargo build --workspace --bins --locked
    nice -n 10 cargo nextest run --workspace --locked --no-tests=pass

deny:
    cargo deny --locked check

# Rewrite protocol golden fixtures after an intentional wire change (bump protocol version first).
update-golden:
    UPDATE_GOLDEN=1 cargo nextest run -p hutan-protocol --locked

# Measure the performance budgets B1-B6, B3p, B7-B10 (docs/perf.md); not part of the gate. Extra args: `just bench --only B1,B4`.
bench *args:
    cargo run -p xtask --release --locked -- bench {{args}}

# Assemble and ad-hoc sign target/bundle/Hutan.app (docs/packaging.md). Never launches or registers
# it; not part of the gate (the xtask bundle test covers it). Extra args: `just bundle --release`.
bundle *args:
    cargo run -p xtask --locked -- bundle {{args}}

# Render assets/macos/Hutan.icns from the brand icon SVG (docs/packaging.md).
icon:
    cargo run -p xtask --locked -- icon

# Render the menu bar glyph assets/macos/MenuBarIcon.png and @2x from the brand favicon.
menubar-icon:
    cargo run -p xtask --locked -- menubar-icon

# Build a release Hutan.app and install it into /Applications (docs/packaging.md). Extra args:
# `just install --dest ~/Applications`, `just install --debug`.
install *args:
    cargo run -p xtask --locked -- install {{args}}

# Free disk without losing the build: drop incremental caches (rebuilt on demand) and print what
# the target dir and slice worktrees still take.
sweep:
    rm -rf target/debug/incremental target/release/incremental
    -du -sh target ../hutan-wt/*/target 2>/dev/null
