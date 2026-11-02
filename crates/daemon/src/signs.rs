//! What the review packet and the findings engine both recognise (`agents.review`,
//! `agents.findings`; `docs/assistant.md` § Checks): test files, snapshot files, test-like
//! commands, a message that claims "done" or "tests pass", a diff that weakens tests, and the
//! signs of one turn the packet has always carried (a lockfile changed, a file deleted, a path
//! outside the folder, a secret-like path, a large diff, no test command, a failed command and
//! then "done").
//!
//! Everything here is a pure function of recorded facts — paths, command lines with their exit
//! codes, the agent's final message, a unified patch. A recognition by name or pattern is a
//! heuristic and every caller says so; an exit code is a fact. Nothing here reads a file, runs a
//! command or calls a model.

use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use hutan_index::redact::redact;
use hutan_protocol::checkpoint::CheckpointFileStatus;
use hutan_protocol::review::{
    REVIEW_EVIDENCE_CHARS, REVIEW_EVIDENCE_MAX, REVIEW_LARGE_DIFF_LINES, ReviewCommand,
    ReviewSignal, ReviewSignalId, ReviewToolStatus,
};
use regex::Regex;

use crate::checkpoints::caps::{DEFAULT_EXCLUDE, excluded};
use crate::context::{Keep, bound};

// Text helpers ------------------------------------------------------------------------------------

/// `text` on one line, redacted by the caller, at most `max` characters; a cut ends in `…`.
pub fn line(text: &str, max: usize) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut = bound(&joined, max, Keep::Start);
    if cut.truncated {
        format!("{}…", cut.text)
    } else {
        cut.text
    }
}

/// A line of evidence: redacted, on one line, at most [`REVIEW_EVIDENCE_CHARS`] characters.
pub fn evidence(text: impl AsRef<str>) -> String {
    line(&redact(text.as_ref()), REVIEW_EVIDENCE_CHARS)
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

#[allow(clippy::expect_used)] // The patterns are constants covered by the unit tests.
fn compile(pattern: &str) -> Regex {
    Regex::new(pattern).expect("constant pattern")
}

// Files -------------------------------------------------------------------------------------------

/// File names of lockfiles and package manifests.
const DEPENDENCY_FILES: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "package.json",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lock",
    "bun.lockb",
    "go.mod",
    "go.sum",
    "Gemfile",
    "Gemfile.lock",
    "pyproject.toml",
    "poetry.lock",
    "uv.lock",
    "Pipfile",
    "Pipfile.lock",
    "requirements.txt",
    "composer.json",
    "composer.lock",
    "build.gradle",
    "build.gradle.kts",
    "pom.xml",
    "Package.swift",
    "Package.resolved",
    "Podfile",
    "Podfile.lock",
    "flake.lock",
    "mix.exs",
    "mix.lock",
];

/// The name of `path` is a lockfile or a package manifest.
pub fn is_dependency_file(path: &str) -> bool {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| DEPENDENCY_FILES.contains(&name))
}

/// What a path is when it belongs to a project's tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileRole {
    /// A file with tests in it.
    Test,
    /// A snapshot, a golden file or a fixture: what a test compares its result with.
    Snapshot,
}

/// Extensions of files that hold code (a test file is code; a README in `tests/` is not).
const CODE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts", "py", "go", "rb", "java", "kt",
    "kts", "swift", "c", "cc", "cpp", "cxx", "h", "hpp", "m", "mm", "cs", "php", "scala", "ex",
    "exs", "erl", "hs", "lua", "sh", "bash", "zsh", "dart", "clj", "vue", "svelte",
];

/// Directories that hold tests.
const TEST_DIRS: &[&str] = &["test", "tests", "__tests__", "spec", "specs", "e2e"];
/// Directories that hold what a test compares with, wherever they are.
const SNAPSHOT_DIRS: &[&str] = &[
    "__snapshots__",
    "__fixtures__",
    "golden",
    "goldens",
    "testdata",
    "cassettes",
];
/// Directories that hold fixtures only inside a test directory.
const FIXTURE_DIRS: &[&str] = &["fixtures", "snapshots", "snapshot"];
/// Extensions of snapshot files.
const SNAPSHOT_EXTENSIONS: &[&str] = &["snap", "golden", "ambr"];

fn has_extension(path: &Path, list: &[&str]) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| list.contains(&ext.to_ascii_lowercase().as_str()))
}

/// Whether `path` is a test file or a snapshot, by its directories and its name (Rust, JS/TS,
/// Python, Go, Ruby, Java/Kotlin, Swift). A heuristic: a test inside a source file (`#[cfg(test)]`
/// in a module) is not recognised by a path.
pub fn file_role(path: &str) -> Option<FileRole> {
    let path = Path::new(path);
    let mut dirs: Vec<String> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(part) => part.to_str().map(str::to_lowercase),
            _ => None,
        })
        .collect();
    let name = dirs.pop()?;
    let in_test_dir = dirs.iter().any(|d| TEST_DIRS.contains(&d.as_str()));
    let named = name.as_str();
    if has_extension(path, SNAPSHOT_EXTENSIONS)
        || named.contains(".approved.")
        || named.contains(".received.")
        || named.ends_with(".snap.new")
        || dirs.iter().any(|d| SNAPSHOT_DIRS.contains(&d.as_str()))
        || (in_test_dir && dirs.iter().any(|d| FIXTURE_DIRS.contains(&d.as_str())))
    {
        return Some(FileRole::Snapshot);
    }
    if !has_extension(path, CODE_EXTENSIONS) {
        return None;
    }
    let original = path.file_name().and_then(|n| n.to_str()).unwrap_or(named);
    let stem = Path::new(original)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(original);
    let lower = stem.to_lowercase();
    let by_name = lower.starts_with("test_")
        || lower.ends_with("_test")
        || lower.ends_with("_tests")
        || lower.ends_with(".test")
        || lower.ends_with(".spec")
        || lower.ends_with("_spec")
        || matches!(lower.as_str(), "test" | "tests" | "conftest")
        // CamelCase names: FooTest.java, FooTests.swift, FooSpec.kt, FooIT.java.
        || ["Test", "Tests", "Spec", "IT"]
            .iter()
            .any(|suffix| stem.len() > suffix.len() && stem.ends_with(suffix));
    (in_test_dir || by_name).then_some(FileRole::Test)
}

/// A file with code that is neither a test nor a snapshot.
pub fn is_source_code(path: &str) -> bool {
    file_role(path).is_none() && has_extension(Path::new(path), CODE_EXTENSIONS)
}

/// A path as the agent gave it that leaves the folders of the session.
pub fn outside(path: &str, roots: &[PathBuf]) -> bool {
    let path = Path::new(path);
    if path.is_absolute() {
        return !roots.is_empty() && !roots.iter().any(|root| path.starts_with(root));
    }
    matches!(path.components().next(), Some(Component::ParentDir))
}

/// A file as the turn's records give it, before the list is cut.
#[derive(Debug, Clone)]
pub struct SeenFile {
    pub path: String,
    pub status: CheckpointFileStatus,
    pub added: Option<u64>,
    pub removed: Option<u64>,
    pub binary: bool,
    /// Only the journal named it.
    pub journal_only: bool,
}

// Commands ----------------------------------------------------------------------------------------

/// Words that make a command look like a test run.
const TEST_WORDS: &[&str] = &[
    "test", "tests", "pytest", "jest", "vitest", "nextest", "mocha", "rspec", "phpunit", "tox",
    "unittest", "xctest", "ctest",
];

/// Words that make a command look like a build, a type check or a lint.
const BUILD_WORDS: &[&str] = &[
    "build",
    "check",
    "clippy",
    "lint",
    "tsc",
    "compile",
    "typecheck",
    "mypy",
    "eslint",
    "ruff",
    "vet",
    "make",
    "cmake",
    "gradle",
    "mvn",
    "xcodebuild",
];

/// Whether the words of `command` look like a test run (a test, pytest, jest, nextest …).
pub fn looks_like_a_test(command: &str) -> bool {
    words(command)
        .iter()
        .any(|word| TEST_WORDS.contains(&word.as_str()))
}

/// Whether `command` looks like a build, a type check, a linter or a test run.
pub fn looks_like_a_check(command: &str) -> bool {
    looks_like_a_test(command)
        || words(command)
            .iter()
            .any(|word| BUILD_WORDS.contains(&word.as_str()))
}

/// The command failed: its status says so or its exit code is not 0.
pub fn is_failed(command: &ReviewCommand) -> bool {
    command.status == ReviewToolStatus::Failed || command.exit_code.is_some_and(|code| code != 0)
}

/// The command finished and its exit code is 0: the only evidence that it passed.
pub fn is_passed(command: &ReviewCommand) -> bool {
    command.status == ReviewToolStatus::Completed && command.exit_code == Some(0)
}

/// The command finished and nothing says how: neither a pass nor a failure.
pub fn is_unknown(command: &ReviewCommand) -> bool {
    command.status == ReviewToolStatus::Completed && command.exit_code.is_none()
}

// Messages ----------------------------------------------------------------------------------------

/// Words with which an agent says it is finished.
const DONE_WORDS: &[&str] = &[
    "done",
    "complete",
    "completed",
    "finished",
    "fixed",
    "implemented",
    "works",
    "working",
    "passes",
    "passing",
    "passed",
    "ready",
    "resolved",
    "all set",
];

/// Whether `message` sounds finished (a word like done, fixed, passes).
pub fn sounds_done(message: &str) -> bool {
    let lower = message.to_lowercase();
    let tokens = words(message);
    DONE_WORDS.iter().any(|word| {
        if word.contains(' ') {
            lower.contains(word)
        } else {
            tokens.iter().any(|token| token == word)
        }
    })
}

/// What a message says passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimKind {
    /// The tests (or "everything") pass.
    Tests,
    /// The build, the type check, the lints pass.
    Build,
}

/// A sentence of a message that says tests or a build pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub kind: ClaimKind,
    pub sentence: String,
}

static CLAIM: LazyLock<Regex> = LazyLock::new(|| {
    compile(
        r"\b(tests?|test suite|specs?|everything|all checks?|checks?|builds?|ci|clippy|lints?|type-?checks?|typecheck|compilation|cargo test|pytest|jest|nextest)\b[^.;\n]{0,60}?\b(pass(?:es|ed|ing)?|green|succe(?:ed|eds|eded|ssful|ssfully)|clean|ok|compil(?:es|ed))\b",
    )
});
static CLAIM_REVERSED: LazyLock<Regex> = LazyLock::new(|| {
    compile(
        r"\b(?:pass(?:es|ed|ing)?|green)\b[^.;\n]{0,24}\b(tests?|test suite|builds?|checks?|ci)\b",
    )
});
static NO_FAILURES: LazyLock<Regex> = LazyLock::new(|| {
    compile(
        r"\b(?:no|zero|0|without|any)\s+(?:test\s+|build\s+)?(?:failures?|failing|errors?|warnings?|regressions?)\b",
    )
});
static NEGATION: LazyLock<Regex> = LazyLock::new(|| {
    compile(
        r"\b(?:not|never|cannot|unable|could\s?not|fails?|failed|failing|failures?|errors?|broken|todo|yet|unless|should|would|until|wait(?:ing)?|once|when|if)\b|n't",
    )
});

/// The first sentence of `message` that says tests or a build pass, when there is one. A
/// heuristic on the words: a sentence with a negation, a condition or a wish ("should pass",
/// "if the tests pass") is not a claim.
pub fn claims_pass(message: &str) -> Option<Claim> {
    for sentence in sentences(message) {
        let lower = sentence.to_lowercase();
        let cleaned = NO_FAILURES.replace_all(&lower, " ");
        if NEGATION.is_match(&cleaned) {
            continue;
        }
        let noun = CLAIM
            .captures(&cleaned)
            .or_else(|| CLAIM_REVERSED.captures(&cleaned))
            .and_then(|caps| caps.get(1).map(|m| m.as_str().to_owned()));
        let Some(noun) = noun else {
            continue;
        };
        let build = ["build", "ci", "clippy", "lint", "check", "compil", "type"]
            .iter()
            .any(|word| noun.contains(word))
            && !noun.contains("test");
        return Some(Claim {
            kind: if build {
                ClaimKind::Build
            } else {
                ClaimKind::Tests
            },
            sentence: sentence.to_owned(),
        });
    }
    None
}

/// The sentences and lines of `text`, trimmed.
fn sentences(text: &str) -> impl Iterator<Item = &str> {
    text.split(['\n', '.', '!', '?', ';'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

// Diffs -------------------------------------------------------------------------------------------

static SKIPPED: LazyLock<Regex> = LazyLock::new(|| {
    compile(
        r#"#\[ignore\b|\b(?:it|test|describe|context|specify)\.(?:skip|only)\b|\b(?:xit|xtest|xdescribe|xcontext|fdescribe)\s*\(|\bfit\s*\(\s*['"`]|@pytest\.mark\.(?:skip|skipif|xfail)\b|\bpytest\.(?:skip|xfail)\s*\(|@unittest\.(?:skip|skipIf|skipUnless|expectedFailure)\b|\.skipTest\s*\(|\b[tb]\.Skip(?:f|Now)?\s*\(|^\s*(?:skip|pending)\s+['"]|^\s*(?:skip|pending)\s*$|@Disabled\b|@Ignore\b|@Test\s*\(\s*enabled\s*=\s*false|\bXCTSkip"#,
    )
});
static ASSERTION: LazyLock<Regex> = LazyLock::new(|| {
    compile(
        r"(?:^|[^.\w])(?:assert\w*|debug_assert\w*|expect\s*\(|expect_\w+|XCTAssert\w*|assertThat|require\.\w+|t\.(?:Error|Errorf|Fatal|Fatalf))|\.should\b|\.to(?:Be|Equal|Have|Match|Throw|Contain|StrictEqual|Be\w+)\s*\(|\.must\b|\bshould\s*[.(]",
    )
});

/// What a patch to a test file takes out of the test.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Weakening {
    /// Added lines that skip, ignore or focus a test, with what they said; a line that only
    /// replaced another that did the same is not counted.
    pub skips: Vec<String>,
    /// Lines that assert, removed and added.
    pub removed_assertions: usize,
    pub added_assertions: usize,
}

impl Weakening {
    /// More assertions went than came, or a test is skipped that was not.
    pub fn is_weakened(&self) -> bool {
        !self.skips.is_empty() || self.removed_assertions > self.added_assertions
    }
}

fn is_comment(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("//")
        || line.starts_with("/*")
        || line.starts_with('*')
        || line.starts_with("# ")
        || line == "#"
        || line.starts_with("--")
}

/// The weakening in a unified patch of one test file: the added lines that skip a test and the
/// assertions that were removed against those that were added. Comment lines count for nothing.
pub fn weakening(patch: &str) -> Weakening {
    let mut out = Weakening::default();
    let (mut skips_added, mut skips_removed) = (Vec::new(), 0usize);
    for raw in patch.lines() {
        let (added, text) = if let Some(text) = raw.strip_prefix('+') {
            if raw.starts_with("+++") {
                continue;
            }
            (true, text)
        } else if let Some(text) = raw.strip_prefix('-') {
            if raw.starts_with("---") {
                continue;
            }
            (false, text)
        } else {
            continue;
        };
        if is_comment(text) {
            continue;
        }
        if SKIPPED.is_match(text) {
            if added {
                skips_added.push(text.trim().to_owned());
            } else {
                skips_removed += 1;
            }
        }
        if ASSERTION.is_match(text) {
            if added {
                out.added_assertions += 1;
            } else {
                out.removed_assertions += 1;
            }
        }
    }
    let net = skips_added.len().saturating_sub(skips_removed);
    skips_added.truncate(net);
    out.skips = skips_added;
    out
}

/// The files of a multi-file unified patch (`diff --git a/x b/x`), each with its own part.
pub fn split_patch(patch: &str) -> Vec<(String, &str)> {
    let mut starts: Vec<(usize, String)> = Vec::new();
    let mut offset = 0;
    for raw in patch.split_inclusive('\n') {
        if let Some(rest) = raw.strip_prefix("diff --git ") {
            // `a/<path> b/<path>`: the path of the new side (the same for a change in place).
            let path = rest
                .trim_end()
                .rsplit_once(" b/")
                .map_or_else(|| rest.trim().to_owned(), |(_, path)| path.to_owned());
            starts.push((offset, path));
        }
        offset += raw.len();
    }
    let mut out = Vec::with_capacity(starts.len());
    for (index, (start, path)) in starts.iter().enumerate() {
        let end = starts.get(index + 1).map_or(patch.len(), |(next, _)| *next);
        out.push((path.clone(), &patch[*start..end]));
    }
    out
}

// Signs -------------------------------------------------------------------------------------------

/// The signs of one turn the review packet carries: what changed, what ran, what the final
/// message said. Each is a heuristic with a name that points at evidence and never decides.
pub fn signals(
    seen: &[SeenFile],
    commands: &[ReviewCommand],
    message: Option<&str>,
    roots: &[PathBuf],
    totals: Option<(u64, u64)>,
) -> Vec<ReviewSignal> {
    let mut out: Vec<ReviewSignal> = Vec::new();
    let mut push = |id: ReviewSignalId, label: &str, mut lines: Vec<String>| {
        lines.truncate(REVIEW_EVIDENCE_MAX);
        out.push(ReviewSignal {
            id,
            label: label.to_owned(),
            evidence: lines,
        });
    };

    let deps: Vec<String> = seen
        .iter()
        .filter(|f| is_dependency_file(&f.path))
        .map(|f| evidence(&f.path))
        .collect();
    if !deps.is_empty() {
        push(
            ReviewSignalId::DependencyFileChanged,
            "a lockfile or a package manifest changed (by file name)",
            deps,
        );
    }
    let deleted: Vec<String> = seen
        .iter()
        .filter(|f| f.status == CheckpointFileStatus::Deleted)
        .map(|f| evidence(&f.path))
        .collect();
    if !deleted.is_empty() {
        push(ReviewSignalId::FileDeleted, "a file was deleted", deleted);
    }
    // Only the journal knows a path outside the checkout: the checkpoint's are relative to it.
    let away: Vec<String> = seen
        .iter()
        .filter(|f| f.journal_only && outside(&f.path, roots))
        .map(|f| evidence(&f.path))
        .collect();
    if !away.is_empty() {
        push(
            ReviewSignalId::PathOutsideFolder,
            "the agent changed a path outside the session's folders",
            away,
        );
    }
    let patterns: Vec<String> = DEFAULT_EXCLUDE.iter().map(|p| (*p).to_owned()).collect();
    let secret: Vec<String> = seen
        .iter()
        .filter(|f| excluded(f.path.as_bytes(), &patterns))
        .map(|f| evidence(&f.path))
        .collect();
    if !secret.is_empty() {
        push(
            ReviewSignalId::SecretLikePath,
            "a changed path has the name of a likely secret (.env, *.pem, *.key, id_rsa …)",
            secret,
        );
    }
    if let Some((added, removed)) = totals
        && added + removed > REVIEW_LARGE_DIFF_LINES
    {
        push(
            ReviewSignalId::LargeDiff,
            &format!("more than {REVIEW_LARGE_DIFF_LINES} lines added plus removed"),
            vec![format!("+{added} −{removed} over {} files", seen.len())],
        );
    }
    // The commands of a journaled turn are all recorded (none is a fact too).
    if !seen.is_empty() && !commands.iter().any(|c| looks_like_a_test(&c.text)) {
        push(
            ReviewSignalId::NoTestCommand,
            "files changed and no command that looks like a test run (a test, pytest, jest, nextest … in its words)",
            vec![format!(
                "{} files changed; {} command(s), none a test run",
                seen.len(),
                commands.len()
            )],
        );
    }
    let unrecovered = unrecovered_failures(commands);
    if !unrecovered.is_empty()
        && let Some(message) = message
        && sounds_done(message)
    {
        let mut lines: Vec<String> = unrecovered
            .iter()
            .map(|c| {
                let code = c
                    .exit_code
                    .map_or_else(|| "failed".to_owned(), |code| format!("exit {code}"));
                evidence(format!("{code}: {}", c.text))
            })
            .collect();
        lines.push(evidence(format!("final message: {message}")));
        push(
            ReviewSignalId::FailedCommandThenDone,
            "a command failed and was not run again with success, and the final message sounds done (a word like done, fixed, passes)",
            lines,
        );
    }
    out
}

/// The commands that failed and were not run again, word for word, with success.
pub fn unrecovered_failures(commands: &[ReviewCommand]) -> Vec<&ReviewCommand> {
    commands
        .iter()
        .enumerate()
        .filter(|(at, command)| {
            is_failed(command)
                && command.status != ReviewToolStatus::Declined
                && !commands[at + 1..].iter().any(|later| {
                    later.text == command.text
                        && !is_failed(later)
                        && later.status == ReviewToolStatus::Completed
                })
        })
        .map(|(_, command)| command)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_files_are_recognised_per_ecosystem() {
        for path in [
            "crates/daemon/tests/it/stats.rs",
            "src/parser_test.rs",
            "src/tests.rs",
            "web/src/app.test.tsx",
            "web/src/app.spec.ts",
            "web/__tests__/app.js",
            "pkg/test_parser.py",
            "pkg/parser_test.py",
            "tests/conftest.py",
            "internal/store/store_test.go",
            "spec/models/user_spec.rb",
            "test/user_test.rb",
            "app/src/test/java/com/acme/FooTest.java",
            "app/src/main/kotlin/FooTests.kt",
            "Tests/AppTests/ParserTests.swift",
        ] {
            assert_eq!(file_role(path), Some(FileRole::Test), "{path}");
        }
        for path in [
            "src/main.rs",
            "src/latest.rs",
            "src/contest.py",
            "src/protest.js",
            "README.md",
            "tests/README.md",
            "src/testing.rs",
            "docs/testing.md",
            "Cargo.toml",
        ] {
            assert_eq!(file_role(path), None, "{path}");
        }
    }

    #[test]
    fn snapshots_goldens_and_fixtures_are_recognised() {
        for path in [
            "web/__snapshots__/app.test.tsx.snap",
            "tests/snapshots/parser__basic.snap",
            "crates/protocol/tests/fixtures/v0/request_x.json",
            "pkg/testdata/in.txt",
            "tests/golden/out.txt",
            "a/b.golden",
            "tests/__snapshots__/x.ambr",
            "x.approved.txt",
        ] {
            assert_eq!(file_role(path), Some(FileRole::Snapshot), "{path}");
        }
        // A fixtures directory outside any test directory is not one.
        assert_eq!(file_role("db/fixtures/users.json"), None);
        assert_eq!(file_role("src/snapshots/mod.rs"), None);
    }

    #[test]
    fn source_code_is_code_that_is_not_a_test() {
        assert!(is_source_code("src/lib.rs"));
        assert!(is_source_code("web/app.tsx"));
        assert!(!is_source_code("tests/it/a.rs"));
        assert!(!is_source_code("README.md"));
        assert!(!is_source_code("Cargo.lock"));
    }

    #[test]
    fn a_command_looks_like_a_test_by_its_words() {
        for command in [
            "cargo test -p x",
            "cargo nextest run",
            "npm test",
            "pnpm run test:unit",
            "pytest -q",
            "go test ./...",
            "make test",
            "bundle exec rspec",
        ] {
            assert!(looks_like_a_test(command), "{command}");
        }
        for command in ["cargo build", "ls -la", "git status", "rg contest"] {
            assert!(!looks_like_a_test(command), "{command}");
        }
        assert!(looks_like_a_check("cargo clippy --all"));
        assert!(looks_like_a_check("npm run build"));
        assert!(!looks_like_a_check("git diff"));
    }

    #[test]
    fn claims_of_passing_tests_or_builds_are_found_and_wishes_and_negations_are_not() {
        let claim = |text: &str| claims_pass(text).map(|c| c.kind);
        assert_eq!(claim("Done. All tests pass."), Some(ClaimKind::Tests));
        assert_eq!(
            claim("I ran the suite and the tests are green"),
            Some(ClaimKind::Tests)
        );
        assert_eq!(
            claim("The build passes and clippy is clean."),
            Some(ClaimKind::Build)
        );
        assert_eq!(claim("Everything passes now"), Some(ClaimKind::Tests));
        assert_eq!(
            claim("all tests pass with no failures"),
            Some(ClaimKind::Tests)
        );
        assert_eq!(claim("Tests passed: 41 ok"), Some(ClaimKind::Tests));
        assert_eq!(claim("tests don't pass yet"), None);
        assert_eq!(claim("two tests still fail"), None);
        assert_eq!(claim("this should make the tests pass"), None);
        assert_eq!(claim("if the tests pass, merge it"), None);
        assert_eq!(claim("I could not run the tests"), None);
        assert_eq!(claim("added a health endpoint"), None);
        assert_eq!(
            claims_pass("Fixed the bug.\nThe test suite passes.").map(|c| c.sentence),
            Some("The test suite passes".to_owned())
        );
    }

    #[test]
    fn a_patch_that_skips_a_test_or_removes_assertions_weakens_it() {
        let rust = "--- a/t.rs\n+++ b/t.rs\n@@ -1,4 +1,5 @@\n+#[ignore = \"flaky\"]\n #[test]\n fn a() {\n-    assert_eq!(f(), 2);\n }\n";
        let w = weakening(rust);
        assert_eq!(w.skips, vec!["#[ignore = \"flaky\"]".to_owned()]);
        assert_eq!((w.removed_assertions, w.added_assertions), (1, 0));
        assert!(w.is_weakened());

        for added in [
            "+  it.skip('x', () => {})",
            "+describe.only('x', () => {})",
            "+xit('x', () => {})",
            "+@pytest.mark.skip(reason='later')",
            "+@pytest.mark.xfail",
            "+    t.Skip(\"later\")",
            "+  skip \"later\"",
            "+@Disabled",
            "+    XCTSkip(\"later\")",
        ] {
            let patch = format!("@@ -1 +1,2 @@\n context\n{added}\n");
            assert_eq!(weakening(&patch).skips.len(), 1, "{added}");
        }
    }

    #[test]
    fn what_does_not_weaken_a_test_is_left_alone() {
        // An assertion rewritten in place, a comment about ignoring, a skip that was already there.
        let rewritten = "@@ -1,3 +1,3 @@\n-    assert_eq!(f(), 2);\n+    assert_eq!(f(), 3);\n";
        assert!(!weakening(rewritten).is_weakened());
        let comment = "@@ -1 +1,2 @@\n+// do not #[ignore] this\n+# pytest.mark.skip was removed\n";
        assert!(!weakening(comment).is_weakened());
        let moved = "@@ -1,2 +1,2 @@\n-#[ignore = \"a\"]\n+#[ignore = \"b\"]\n";
        assert!(weakening(moved).skips.is_empty());
        let more = "@@ -1 +1,3 @@\n+    assert!(a);\n+    assert!(b);\n-    assert!(a);\n";
        assert!(!weakening(more).is_weakened());
        // `.expect(` on a result is not an assertion.
        let expect =
            "@@ -1 +1 @@\n-    let v = run().expect(\"ran\");\n+    let v = run().unwrap();\n";
        assert_eq!(weakening(expect).removed_assertions, 0);
        // expect( of a JS test is.
        let js = "@@ -1,2 +1 @@\n-  expect(x).toBe(1);\n";
        assert_eq!(weakening(js).removed_assertions, 1);
    }

    #[test]
    fn a_multi_file_patch_is_split_by_file() {
        let patch = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-x\n+y\ndiff --git a/tests/b.rs b/tests/b.rs\n--- a/tests/b.rs\n+++ b/tests/b.rs\n@@ -1 +1 @@\n-p\n+q\n";
        let parts = split_patch(patch);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].0, "src/a.rs");
        assert!(parts[0].1.contains("+y") && !parts[0].1.contains("+q"));
        assert_eq!(parts[1].0, "tests/b.rs");
        assert!(parts[1].1.contains("+q"));
        assert!(split_patch("").is_empty());
    }
}
