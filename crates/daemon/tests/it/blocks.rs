//! Terminal blocks end to end (protocol 0.5): a real daemon on a temporary `HUTAN_HOME`, real
//! zsh, bash 3.2 (`/bin/bash` on macOS) and fish sessions with automatic shell integration,
//! driven over the daemon's socket. Shells that are not installed are skipped with a message.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use common::*;
use hutan_client::{BlockList, Client};
use hutan_core::TerminalId;
use hutan_protocol::ErrorCode;
use hutan_protocol::blocks::{Block, BlockPart, BlockState, TerminalBlocksResult};
use hutan_protocol::messages::{Event, TerminalSpec};
use tokio::time::sleep;

const PROMPT: &str = "HUTAN-TEST-PROMPT>";

fn have(path: &str) -> bool {
    Path::new(path).exists()
}

async fn blocks(client: &Client, terminal: TerminalId) -> Vec<Block> {
    let mut list = client
        .terminal_blocks(terminal, None, 500)
        .await
        .unwrap()
        .blocks;
    list.reverse();
    list
}

/// Polls `terminal.blocks` until `pred` holds for the (oldest first) blocks.
async fn wait_blocks(
    client: &Client,
    terminal: TerminalId,
    what: &str,
    pred: impl Fn(&[Block]) -> bool,
) -> Vec<Block> {
    let deadline = Instant::now() + WAIT;
    loop {
        let now = blocks(client, terminal).await;
        if pred(&now) {
            return now;
        }
        assert!(
            Instant::now() < deadline,
            "{what}: timed out; blocks: {now:#?}"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

fn finished(blocks: &[Block]) -> usize {
    blocks
        .iter()
        .filter(|b| b.state == BlockState::Finished)
        .count()
}

fn last_is_prompt(blocks: &[Block]) -> bool {
    blocks.last().is_some_and(|b| b.state == BlockState::Prompt)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Zsh,
    Bash,
    Fish,
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

/// The user's startup files: a plain prompt and nothing else.
fn user_files(kind: Kind, home: &Path) {
    match kind {
        Kind::Zsh => write(
            &home.join(".zshrc"),
            &format!("PS1='{PROMPT} '\nunsetopt PROMPT_SP\n"),
        ),
        Kind::Bash => {
            write(&home.join(".bash_profile"), ". \"$HOME/.bashrc\"\n");
            write(&home.join(".bashrc"), &format!("PS1='{PROMPT} '\n"));
        }
        Kind::Fish => write(
            &home.join(".config/fish/config.fish"),
            &format!("function fish_prompt\n    echo -n '{PROMPT} '\nend\n"),
        ),
    }
}

async fn run_shell(kind: Kind, shell: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap().join("home");
    fs::create_dir_all(&home).unwrap();
    user_files(kind, &home);
    let t = TestDaemon::start().await;
    let (client, mut events) = t.connect_subscribed().await;
    assert!(client.supports_blocks());
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    for (key, value) in [
        ("HOME", home.to_string_lossy().into_owned()),
        ("SHELL", shell.to_owned()),
        ("ZDOTDIR", home.to_string_lossy().into_owned()),
        (
            "XDG_CONFIG_HOME",
            home.join(".config").to_string_lossy().into_owned(),
        ),
        (
            "XDG_DATA_HOME",
            home.join(".local/share").to_string_lossy().into_owned(),
        ),
        (
            "XDG_CACHE_HOME",
            home.join(".cache").to_string_lossy().into_owned(),
        ),
        (
            "HISTFILE",
            home.join(".hist").to_string_lossy().into_owned(),
        ),
    ] {
        env.insert(key.to_owned(), value);
    }
    let session = create(
        &client,
        TerminalSpec {
            env,
            cwd: Some(home.clone()),
            ..TerminalSpec::default()
        },
    )
    .await;
    let terminal = terminal_of(&session);

    let now = wait_blocks(&client, terminal, "the first prompt", last_is_prompt).await;
    assert_eq!(now.len(), 1, "{kind:?}: one prompt block: {now:#?}");
    let first_prompt = now[0].clone();
    assert_eq!(first_prompt.id, 1);
    assert_eq!(
        first_prompt.cwd.as_deref(),
        Some(home.as_path()),
        "{kind:?}"
    );

    for (index, line) in ["true", "false", "printf 'a\\nb\\n'"]
        .into_iter()
        .enumerate()
    {
        let wanted = index + 1;
        client
            .send_input(terminal, format!("{line}\n").as_bytes())
            .await
            .unwrap();
        wait_blocks(&client, terminal, line, |b| {
            finished(b) == wanted && last_is_prompt(b)
        })
        .await;
    }
    let now = blocks(&client, terminal).await;
    let done: Vec<&Block> = now
        .iter()
        .filter(|b| b.state == BlockState::Finished)
        .collect();
    assert_eq!(done.len(), 3, "{kind:?}: {now:#?}");
    assert_eq!(
        now.len(),
        4,
        "{kind:?}: three commands and the open prompt: {now:#?}"
    );
    assert_eq!(
        done.iter()
            .map(|b| b.command.as_deref())
            .collect::<Vec<_>>(),
        [Some("true"), Some("false"), Some("printf 'a\\nb\\n'")],
        "{kind:?}"
    );
    assert_eq!(
        done.iter().map(|b| b.exit_code).collect::<Vec<_>>(),
        [Some(0), Some(1), Some(0)],
        "{kind:?}"
    );
    assert_eq!(
        done.iter().map(|b| b.id).collect::<Vec<_>>(),
        [1, 2, 3],
        "{kind:?}: the prompt block that was open becomes the first command"
    );
    for block in &done {
        assert_eq!(block.cwd.as_deref(), Some(home.as_path()), "{kind:?}");
        assert!(block.finished_at_ms.unwrap() >= block.started_at_ms);
        assert!(block.prompt_line <= block.output_start.unwrap());
        assert!(block.output_start.unwrap() <= block.end_line.unwrap());
    }
    // Ranges follow each other.
    assert!(done[0].end_line.unwrap() <= done[1].prompt_line, "{kind:?}");
    assert!(done[1].end_line.unwrap() <= done[2].prompt_line, "{kind:?}");

    let output = client
        .terminal_block_text(terminal, done[2].id, BlockPart::Output, 1 << 16)
        .await
        .unwrap();
    assert_eq!(output.text, "a\nb", "{kind:?}");
    assert!(!output.truncated);
    let none = client
        .terminal_block_text(terminal, done[0].id, BlockPart::Output, 1 << 16)
        .await
        .unwrap();
    assert_eq!(none.text, "", "{kind:?}");
    let command = client
        .terminal_block_text(terminal, done[2].id, BlockPart::Command, 1 << 16)
        .await
        .unwrap();
    assert_eq!(command.text, "printf 'a\\nb\\n'", "{kind:?}");

    // The same list through the client's model, and paging.
    let list = BlockList::fetch(&client, terminal).await.unwrap();
    assert_eq!(list.iter().cloned().collect::<Vec<_>>(), now, "{kind:?}");
    let page: TerminalBlocksResult = client.terminal_blocks(terminal, Some(3), 1).await.unwrap();
    assert_eq!(page.blocks.iter().map(|b| b.id).collect::<Vec<_>>(), [2]);

    // Events: every state change, in order.
    let mut seen: Vec<(u64, BlockState)> = Vec::new();
    let mut applied = BlockList::new();
    while let Ok(Ok((_, event))) =
        tokio::time::timeout(Duration::from_millis(200), events.recv()).await
    {
        if let Event::TerminalBlock { terminal_id, block } = event {
            assert_eq!(terminal_id, terminal);
            if seen.last() != Some(&(block.id, block.state)) {
                seen.push((block.id, block.state));
            }
            applied.apply(block);
        }
    }
    use BlockState::{Finished, Prompt, Running};
    assert_eq!(
        seen,
        [
            (1, Prompt),
            (1, Running),
            (1, Finished),
            (2, Prompt),
            (2, Running),
            (2, Finished),
            (3, Prompt),
            (3, Running),
            (3, Finished),
            (4, Prompt),
        ],
        "{kind:?}"
    );
    assert_eq!(applied, list, "{kind:?}: events build the same list");

    client.send_input(terminal, b"exit\n").await.unwrap();
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn zsh_blocks() {
    if !have("/bin/zsh") {
        eprintln!("skipping: zsh is not installed");
        return;
    }
    run_shell(Kind::Zsh, "/bin/zsh").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bash_3_2_blocks() {
    // macOS ships bash 3.2 as /bin/bash; on other systems this is whatever /bin/bash is.
    if !have("/bin/bash") {
        eprintln!("skipping: /bin/bash is not installed");
        return;
    }
    run_shell(Kind::Bash, "/bin/bash").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fish_blocks() {
    let fish = [
        "/opt/homebrew/bin/fish",
        "/usr/local/bin/fish",
        "/usr/bin/fish",
    ]
    .into_iter()
    .find(|path| have(path));
    let Some(fish) = fish else {
        eprintln!("skipping: fish is not installed");
        return;
    };
    run_shell(Kind::Fish, fish).await;
}

// A scripted terminal: the marks come from printf, so every number is exact. -----------------

/// `$ cmd` with exit 3 and two lines of output, then a prompt, then `cat` keeps it alive.
fn scripted() -> TerminalSpec {
    sh(
        "printf '\\033]133;A\\007$ \\033]133;B\\007cmd\\r\\n\\033]133;C\\007out1\\r\\nout2\\r\\n\\033]133;D;3\\007\\033]133;A\\007$ \\033]133;B\\007'; exec cat",
    )
}

async fn scripted_terminal(client: &Client) -> TerminalId {
    let session = create(client, scripted()).await;
    let terminal = terminal_of(&session);
    wait_blocks(client, terminal, "scripted blocks", |b| {
        b.len() == 2 && b[0].state == BlockState::Finished
    })
    .await;
    terminal
}

#[tokio::test(flavor = "multi_thread")]
async fn blocks_have_exact_lines_and_texts() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let terminal = scripted_terminal(&client).await;
    let all = blocks(&client, terminal).await;
    let first = &all[0];
    assert_eq!(
        (
            first.id,
            first.prompt_line,
            first.command_line,
            first.output_start,
            first.end_line,
            first.command.as_deref(),
            first.exit_code
        ),
        (1, 0, Some(0), Some(1), Some(3), Some("cmd"), Some(3))
    );
    assert_eq!((all[1].id, all[1].prompt_line), (2, 3));
    assert_eq!(all[1].state, BlockState::Prompt);
    let result = client.terminal_blocks(terminal, None, 10).await.unwrap();
    assert_eq!(result.viewport_top, 0);
    let out = client
        .terminal_block_text(terminal, 1, BlockPart::Output, 1024)
        .await
        .unwrap();
    assert_eq!((out.text.as_str(), out.truncated), ("out1\nout2", false));
    let cut = client
        .terminal_block_text(terminal, 1, BlockPart::Output, 6)
        .await
        .unwrap();
    assert_eq!((cut.text.as_str(), cut.truncated), ("out1\no", true));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn renders_carry_the_same_viewport_top_as_the_blocks_answer() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    // 100 lines scroll the 24-row screen, then one block and a prompt on the last rows.
    let session = create(
        &client,
        sh("seq 1 100; printf '\\033]133;A\\007$ \\033]133;B\\007cmd\\r\\n\\033]133;C\\007out\\r\\n\\033]133;D;0\\007\\033]133;A\\007$ \\033]133;B\\007'; exec cat"),
    )
    .await;
    let terminal = terminal_of(&session);
    wait_blocks(&client, terminal, "the block", |b| {
        b.len() == 2 && b[0].state == BlockState::Finished
    })
    .await;
    let mut view = View::attach(&client, terminal, None).await;
    let first = view.screen.viewport_top;
    view.wait_for("the screen to settle", |s| s.contains("$ "))
        .await;
    let answer = client.terminal_blocks(terminal, None, 10).await.unwrap();
    assert!(
        answer.viewport_top > 70,
        "the screen scrolled: {}",
        answer.viewport_top
    );
    assert_eq!(
        view.screen.viewport_top, answer.viewport_top,
        "snapshot {first}"
    );
    // The block's lines map onto rows with it: the prompt line is a row of the screen.
    let blocks = blocks(&client, terminal).await;
    let row = blocks[0].prompt_line - answer.viewport_top;
    assert_eq!(
        view.screen.lines[usize::try_from(row).unwrap()].trim_end(),
        "$ cmd"
    );
    // More output moves the number in the frames, and the answer follows.
    client.send_input(terminal, b"x\n").await.unwrap();
    view.wait_for("the screen to scroll", |s| {
        s.viewport_top > answer.viewport_top
    })
    .await;
    let later = client.terminal_blocks(terminal, None, 10).await.unwrap();
    assert_eq!(later.viewport_top, view.screen.viewport_top);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_terminals_and_blocks_are_not_found_and_limits_are_clamped() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let terminal = scripted_terminal(&client).await;
    assert_eq!(
        remote_code(client.terminal_blocks(TerminalId(9999), None, 5).await),
        ErrorCode::NotFound
    );
    assert_eq!(
        remote_code(
            client
                .terminal_block_text(TerminalId(9999), 1, BlockPart::Output, 5)
                .await
        ),
        ErrorCode::NotFound
    );
    assert_eq!(
        remote_code(
            client
                .terminal_block_text(terminal, 77, BlockPart::Output, 5)
                .await
        ),
        ErrorCode::NotFound
    );
    // limit 0 returns nothing, a huge limit is clamped to the protocol's 500.
    assert!(
        client
            .terminal_blocks(terminal, None, 0)
            .await
            .unwrap()
            .blocks
            .is_empty()
    );
    assert_eq!(
        client
            .terminal_blocks(terminal, None, u32::MAX)
            .await
            .unwrap()
            .blocks
            .len(),
        2
    );
    assert!(
        client
            .terminal_blocks(terminal, Some(1), 10)
            .await
            .unwrap()
            .blocks
            .is_empty()
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_output_of_a_block_in_the_alternate_screen_is_unavailable_until_it_leaves() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    // The script enters the alternate screen after the marks and leaves when it reads a line.
    let session = create(
        &client,
        sh("printf '\\033]133;A\\007$ \\033]133;B\\007cmd\\r\\n\\033]133;C\\007out1\\r\\nout2\\r\\n\\033]133;D;0\\007\\033[?1049h'; read x; printf '\\033[?1049l'; exec cat"),
    )
    .await;
    let terminal = terminal_of(&session);
    wait_blocks(&client, terminal, "the finished block", |b| {
        b.len() == 1 && b[0].state == BlockState::Finished
    })
    .await;
    let deadline = Instant::now() + WAIT;
    loop {
        match client
            .terminal_block_text(terminal, 1, BlockPart::Output, 100)
            .await
        {
            Err(error) => {
                assert_eq!(remote_code::<()>(Err(error)), ErrorCode::InvalidState);
                break;
            }
            Ok(_) => {
                assert!(Instant::now() < deadline, "never saw the alternate screen");
                sleep(Duration::from_millis(25)).await;
            }
        }
    }
    // The command text is stored with the block and stays readable.
    let command = client
        .terminal_block_text(terminal, 1, BlockPart::Command, 100)
        .await
        .unwrap();
    assert_eq!(command.text, "cmd");
    client.send_input(terminal, b"go\n").await.unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        if let Ok(out) = client
            .terminal_block_text(terminal, 1, BlockPart::Output, 100)
            .await
        {
            assert_eq!(out.text, "out1\nout2");
            break;
        }
        assert!(Instant::now() < deadline, "never left the alternate screen");
        sleep(Duration::from_millis(25)).await;
    }
    t.stop().await;
}
