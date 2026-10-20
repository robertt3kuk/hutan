//! Hot reload over real files and the real OS watcher.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::future::Future;
use std::path::Path;
use std::time::Duration;

use common::*;
use hutan_settings::{KEYMAP_NAMESPACE, SettingsPaths, SettingsStore, keymap};
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(15);

/// Re-checks `cond` after every published change until it holds.
async fn until(store: &SettingsStore, what: &str, mut cond: impl FnMut() -> bool) {
    let mut rx = store.changes();
    let result = tokio::time::timeout(WAIT, async {
        loop {
            if cond() {
                return;
            }
            rx.changed().await.unwrap();
        }
    })
    .await;
    assert!(result.is_ok(), "timed out waiting for: {what}");
}

async fn timed<F: Future>(what: &str, f: F) -> F::Output {
    tokio::time::timeout(WAIT, f)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for: {what}"))
}

fn fixture(initial: &str) -> (TempDir, SettingsStore, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let paths = SettingsPaths::in_dir(dir.path());
    write_file(&paths.user, initial);
    let user = paths.user.clone();
    (dir, SettingsStore::new(paths), user)
}

#[tokio::test]
async fn edit_broken_edit_and_fix_cycle() {
    let (_dir, store, user) = fixture("[terminal]\nfont_size = 13\n");
    store.register::<Terminal>();
    store.register::<Layout>();
    assert_eq!(store.get::<Terminal>().font_size, 13);
    let _guard = store.start_watching().unwrap();
    let mut rx = store.changes();
    rx.borrow_and_update();

    // 1. Edit: changes() fires with the namespace.
    write_file(&user, "[terminal]\nfont_size = 20\n");
    let cs = timed("terminal change", async {
        loop {
            rx.changed().await.unwrap();
            let cs = rx.borrow_and_update().clone();
            if cs.contains("terminal") {
                return cs;
            }
        }
    })
    .await;
    assert!(!cs.contains("layout"), "{cs:?}");
    assert_eq!(store.get::<Terminal>().font_size, 20);
    assert!(store.errors().is_empty());

    // 2. Broken edit: error recorded, values unchanged.
    write_file(&user, "[terminal]\nfont_size = \n");
    until(&store, "parse error recorded", || {
        !store.errors().is_empty()
    })
    .await;
    let errors = store.errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].file.as_deref(), Some(user.as_path()));
    assert!(
        errors[0].message.contains("line 2"),
        "{}",
        errors[0].message
    );
    assert_eq!(
        store.get::<Terminal>().font_size,
        20,
        "previous good layer kept"
    );

    // 3. Wrong type is also an error that keeps the value.
    write_file(&user, "[terminal]\nfont_size = \"big\"\n");
    until(&store, "type error recorded", || {
        store
            .errors()
            .iter()
            .any(|e| e.key_path == "terminal.font_size")
    })
    .await;
    assert_eq!(store.get::<Terminal>().font_size, 20);

    // 4. Fixed edit: recovered.
    write_file(&user, "[terminal]\nfont_size = 30\n");
    until(&store, "recovery", || {
        store.get::<Terminal>().font_size == 30 && store.errors().is_empty()
    })
    .await;
}

#[tokio::test]
async fn deleting_and_recreating_the_file() {
    let (_dir, store, user) = fixture("[terminal]\nfont_size = 20\n");
    store.register::<Terminal>();
    let _guard = store.start_watching().unwrap();
    assert_eq!(store.get::<Terminal>().font_size, 20);

    std::fs::remove_file(&user).unwrap();
    until(&store, "revert to defaults", || {
        store.get::<Terminal>().font_size == 13
    })
    .await;

    write_file(&user, "[terminal]\nfont_size = 22\n");
    until(&store, "file back", || {
        store.get::<Terminal>().font_size == 22
    })
    .await;
}

#[tokio::test]
async fn edits_made_between_new_and_start_watching_are_picked_up() {
    let (_dir, store, user) = fixture("[terminal]\nfont_size = 20\n");
    write_file(&user, "[terminal]\nfont_size = 21\n");
    let _guard = store.start_watching().unwrap();
    assert_eq!(store.get::<Terminal>().font_size, 21);
}

#[tokio::test]
async fn missing_settings_directory_is_created_and_watched() {
    let dir = TempDir::new().unwrap();
    let base = dir.path().join("Hutan");
    let store = SettingsStore::new(SettingsPaths::in_dir(&base));
    let _guard = store.start_watching().unwrap();
    assert!(base.is_dir());

    write_file(&base.join("settings.toml"), "[terminal]\nfont_size = 18\n");
    until(&store, "new file", || {
        store.get::<Terminal>().font_size == 18
    })
    .await;
}

#[tokio::test]
async fn keymap_changes_are_reported_and_validated() {
    let dir = TempDir::new().unwrap();
    let paths = SettingsPaths::in_dir(dir.path());
    let keymap_path = paths.keymap.clone();
    let store = SettingsStore::new(paths);
    let _guard = store.start_watching().unwrap();
    let defaults = vec![keymap::BindingSpec::new("cmd-d", "workbench::SplitRight", None).unwrap()];
    assert_eq!(store.keymap(&defaults).bindings, defaults);

    let mut rx = store.changes();
    rx.borrow_and_update();
    write_file(
        &keymap_path,
        "[[binding]]\nkeys = \"cmd-d\"\naction = \"workbench::SplitDown\"\n",
    );
    timed("keymap change", async {
        loop {
            rx.changed().await.unwrap();
            if rx.borrow_and_update().contains(KEYMAP_NAMESPACE) {
                return;
            }
        }
    })
    .await;
    assert_eq!(
        store.keymap(&defaults).bindings[0].action,
        "workbench::SplitDown"
    );

    // Broken keymap: error recorded, previous bindings stay.
    write_file(&keymap_path, "[[binding]\n");
    until(&store, "keymap error", || !store.errors().is_empty()).await;
    assert_eq!(
        store.keymap(&defaults).bindings[0].action,
        "workbench::SplitDown"
    );
    assert_eq!(
        store.errors()[0].file.as_deref(),
        Some(keymap_path.as_path())
    );

    write_file(&keymap_path, "");
    until(&store, "keymap fixed", || {
        store.errors().is_empty() && store.keymap(&defaults).bindings == defaults
    })
    .await;
}

#[tokio::test]
async fn workspace_file_created_after_watching_started() {
    let dir = TempDir::new().unwrap();
    let store = SettingsStore::new(SettingsPaths::in_dir(dir.path().join("app")));
    store.register::<Layout>();
    store.register::<Terminal>();
    let _guard = store.start_watching().unwrap();

    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    store.set_workspace_root(Some(root.clone()));
    assert_eq!(store.get::<Layout>().preset, "default");

    write_file(
        &root.join(".hutan/settings.toml"),
        "[layout]\npreset = \"focus\"\n[terminal]\nfont_size = 99\n",
    );
    until(&store, "workspace layer", || {
        store.get::<Layout>().preset == "focus"
    })
    .await;
    assert_eq!(
        store.get::<Terminal>().font_size,
        13,
        "non-overridable stays put"
    );
    until(&store, "refusal reported", || {
        store.errors().iter().any(|e| e.key_path == "terminal")
    })
    .await;

    // Edits to the now-existing .hutan dir keep hot reloading.
    write_file(
        &root.join(".hutan/settings.toml"),
        "[layout]\npreset = \"overview\"\n",
    );
    until(&store, "workspace edit", || {
        store.get::<Layout>().preset == "overview"
    })
    .await;
    assert!(store.errors().is_empty(), "{:?}", store.errors());

    // Switching roots drops the old layer.
    let other = dir.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    store.set_workspace_root(Some(other));
    assert_eq!(store.get::<Layout>().preset, "default");
}

#[tokio::test]
async fn the_project_file_hot_reloads_above_the_workspace_file() {
    let dir = TempDir::new().unwrap();
    let store = SettingsStore::new(SettingsPaths::in_dir(dir.path().join("app")));
    store.register::<Layout>();
    let _guard = store.start_watching().unwrap();

    let workspace = dir.path().join("workspace");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    write_file(
        &workspace.join(".hutan/settings.toml"),
        "[layout]\npreset = \"workspace\"\n",
    );
    store.set_workspace_root(Some(workspace));
    store.set_project_root(Some(project.clone()));
    assert_eq!(store.get::<Layout>().preset, "workspace");

    // The project's file appears after watching started (its `.hutan` did not exist).
    write_file(
        &project.join(".hutan/settings.toml"),
        "[layout]\npreset = \"focus\"\n",
    );
    until(&store, "project layer", || {
        store.get::<Layout>().preset == "focus"
    })
    .await;

    // Edits keep hot reloading.
    write_file(
        &project.join(".hutan/settings.toml"),
        "[layout]\npreset = \"overview\"\n",
    );
    until(&store, "project edit", || {
        store.get::<Layout>().preset == "overview"
    })
    .await;

    // Removing the file falls back to the workspace layer.
    std::fs::remove_file(project.join(".hutan/settings.toml")).unwrap();
    until(&store, "project file removed", || {
        store.get::<Layout>().preset == "workspace"
    })
    .await;

    // Switching the project drops its layer.
    store.set_project_root(None);
    assert_eq!(store.get::<Layout>().preset, "workspace");
}

#[tokio::test]
async fn dropping_the_guard_stops_reloading() {
    let (_dir, store, user) = fixture("[terminal]\nfont_size = 20\n");
    store.register::<Terminal>();
    let guard = store.start_watching().unwrap();
    drop(guard);
    write_file(&user, "[terminal]\nfont_size = 25\n");
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(store.get::<Terminal>().font_size, 20);

    // And a new guard can be started afterwards.
    let _guard = store.start_watching().unwrap();
    assert_eq!(store.get::<Terminal>().font_size, 25);
}

#[test]
fn second_guard_while_watching_is_refused() {
    let dir = TempDir::new().unwrap();
    let store = SettingsStore::new(SettingsPaths::in_dir(Path::new(dir.path())));
    let _guard = store.start_watching().unwrap();
    assert!(store.start_watching().is_err());
}
