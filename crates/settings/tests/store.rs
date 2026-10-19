//! Store semantics over real files in temp dirs (no watcher).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use common::*;
use hutan_settings::{
    AppearanceSettings, Density, SettingsLayer, SettingsPaths, SettingsStore, Severity, keymap,
};
use tempfile::TempDir;

struct Fixture {
    dir: TempDir,
    store: SettingsStore,
}

impl Fixture {
    fn new(user: Option<&str>) -> Self {
        let dir = TempDir::new().unwrap();
        let paths = SettingsPaths::in_dir(dir.path());
        if let Some(user) = user {
            write_file(&paths.user, user);
        }
        let store = SettingsStore::new(paths);
        Self { dir, store }
    }

    fn user_path(&self) -> std::path::PathBuf {
        self.dir.path().join("settings.toml")
    }

    fn workspace(&self, text: &str) -> std::path::PathBuf {
        let root = self.dir.path().join("repo");
        write_file(&root.join(".hutan/settings.toml"), text);
        self.store.set_workspace_root(Some(root.clone()));
        root
    }
}

#[test]
fn defaults_when_nothing_is_configured() {
    let f = Fixture::new(None);
    assert_eq!(*f.store.get::<Terminal>(), Terminal::default());
    assert_eq!(
        *f.store.get::<AppearanceSettings>(),
        AppearanceSettings::default()
    );
    assert!(f.store.errors().is_empty());
}

#[test]
fn user_layer_overrides_defaults_and_merges_deeply() {
    let f = Fixture::new(Some(
        "[terminal]\nfont_size = 15\ntags = [\"x\", \"y\"]\n[terminal.cursor]\nblink = false\n",
    ));
    let t = f.store.get::<Terminal>();
    assert_eq!(t.font_size, 15);
    assert_eq!(t.font_family, "Menlo", "untouched keys keep defaults");
    assert!(!t.cursor.blink);
    assert_eq!(
        t.cursor.style, "block",
        "nested defaults survive a partial nested table"
    );
    assert_eq!(t.tags, ["x", "y"], "arrays replace, not append");
    assert!(f.store.errors().is_empty(), "{:?}", f.store.errors());
}

#[test]
fn precedence_is_defaults_user_workspace_runtime() {
    let f = Fixture::new(Some(
        "[layout]\npreset = \"user\"\n[layout.presets.a]\nsize = 1\npanels = [\"p\"]\n[layout.presets.b]\nsize = 2\n",
    ));
    f.workspace(
        "[layout]\n[layout.presets.a]\nsize = 10\n[layout.presets.c]\nsize = 3\n[panels.sessions]\ngroup_by = \"state\"\n",
    );

    let l = f.store.get::<Layout>();
    assert_eq!(
        l.preset, "user",
        "workspace did not set preset; user wins over default"
    );
    assert_eq!(l.presets["a"].size, 10, "workspace beats user");
    assert_eq!(
        l.presets["a"].panels,
        ["p"],
        "deep merge keeps user keys the workspace did not set"
    );
    assert_eq!(l.presets["b"].size, 2);
    assert_eq!(l.presets["c"].size, 3);
    let s = f.store.get::<Sessions>();
    assert_eq!(s.group_by, "state");
    assert!(s.needs_you);
    assert!(f.store.errors().is_empty(), "{:?}", f.store.errors());

    f.store.set_runtime(Layout {
        preset: "runtime".into(),
        presets: Default::default(),
    });
    assert_eq!(
        f.store.get::<Layout>().preset,
        "runtime",
        "runtime beats workspace and user"
    );
    assert_eq!(
        f.store.get::<Layout>().presets["a"].size,
        10,
        "runtime table merges deeply too"
    );
}

#[test]
fn workspace_override_allowed_for_overridable_namespaces() {
    let f = Fixture::new(Some("[layout]\npreset = \"user\"\n"));
    f.workspace("[layout]\npreset = \"ws\"\n");
    assert_eq!(f.store.get::<Layout>().preset, "ws");
    assert!(f.store.errors().is_empty());

    f.store.set_workspace_root(None);
    assert_eq!(f.store.get::<Layout>().preset, "user");
}

#[test]
fn write_workspace_creates_the_workspace_file_and_applies_it() {
    let f = Fixture::new(Some("[layout]\npreset = \"user\"\n"));
    // No workspace folder active: nothing to write to.
    let error = f
        .store
        .write_workspace("layout.preset", toml::Value::String("ws".into()))
        .unwrap_err();
    assert!(error.to_string().contains("no workspace folder"), "{error}");

    let root = f.dir.path().join("repo");
    f.store.set_workspace_root(Some(root.clone()));
    f.store
        .write_workspace("layout.preset", toml::Value::String("ws".into()))
        .unwrap();
    let file = root.join(".hutan/settings.toml");
    assert!(std::fs::read_to_string(&file).unwrap().contains("ws"));
    assert_eq!(f.store.get::<Layout>().preset, "ws");
    // The user's file is untouched.
    assert!(
        std::fs::read_to_string(f.user_path())
            .unwrap()
            .contains("user")
    );
}

#[test]
fn a_workspace_can_be_resolved_without_making_it_the_active_one() {
    let f = Fixture::new(Some(
        "[layout]\npreset = \"user\"\n[layout.presets.a]\nsize = 1\n[layout.presets.b]\nsize = 2\n",
    ));
    let other = f.dir.path().join("other");
    write_file(
        &other.join(".hutan/settings.toml"),
        "[layout]\npreset = \"other\"\n[layout.presets.a]\nsize = 10\n",
    );
    let bare = f.dir.path().join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    let active = f.workspace("[layout]\npreset = \"active\"\n");

    let other_layout = f.store.get_for_roots::<Layout>(Some(&other), None);
    assert_eq!(
        other_layout.preset, "other",
        "that workspace's file wins over the user's"
    );
    assert_eq!(other_layout.presets["a"].size, 10);
    assert_eq!(
        other_layout.presets["b"].size, 2,
        "deep merge over the user layer"
    );
    // The active workspace layer is untouched.
    assert_eq!(f.store.get::<Layout>().preset, "active");
    // A workspace without a file, and no workspace at all.
    assert_eq!(
        f.store.get_for_roots::<Layout>(Some(&bare), None).preset,
        "user"
    );
    assert_eq!(f.store.get_for_roots::<Layout>(None, None).preset, "active");
    // The active root is the cached value.
    assert_eq!(
        f.store.get_for_roots::<Layout>(Some(&active), None).preset,
        "active"
    );

    // The file is read at the time of the call.
    write_file(
        &other.join(".hutan/settings.toml"),
        "[layout]\npreset = \"again\"\n",
    );
    assert_eq!(
        f.store.get_for_roots::<Layout>(Some(&other), None).preset,
        "again"
    );
    // Broken TOML, or a namespace that may not be overridden: the active value.
    write_file(&other.join(".hutan/settings.toml"), "[layout\n");
    assert_eq!(
        f.store.get_for_roots::<Layout>(Some(&other), None).preset,
        "active"
    );
    write_file(
        &other.join(".hutan/settings.toml"),
        "[terminal]\nfont_size = 99\n",
    );
    assert_eq!(
        f.store
            .get_for_roots::<Terminal>(Some(&other), None)
            .font_size,
        13
    );
}

#[test]
fn workspace_override_refused_for_other_namespaces() {
    let f = Fixture::new(Some("[terminal]\nfont_size = 15\n"));
    f.store.register::<Terminal>();
    let root = f.workspace("[terminal]\nfont_size = 99\n");

    assert_eq!(
        f.store.get::<Terminal>().font_size,
        15,
        "workspace value ignored"
    );
    let errors = f.store.errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].severity, Severity::Error);
    assert_eq!(errors[0].key_path, "terminal");
    assert_eq!(
        errors[0].file.as_deref(),
        Some(root.join(".hutan/settings.toml").as_path())
    );
    assert!(
        errors[0].message.contains("workspace"),
        "{}",
        errors[0].message
    );
}

impl Fixture {
    /// Writes `<dir>/<name>/.hutan/settings.toml` and returns the root.
    fn root(&self, name: &str, text: &str) -> std::path::PathBuf {
        let root = self.dir.path().join(name);
        write_file(&root.join(".hutan/settings.toml"), text);
        root
    }
}

#[test]
fn the_project_layer_sits_above_the_workspace_layer() {
    let f = Fixture::new(Some(
        "[layout]\npreset = \"user\"\n[layout.presets.a]\nsize = 1\npanels = [\"p\"]\n",
    ));
    let workspace = f.root(
        "workspace",
        "[layout]\npreset = \"workspace\"\n[layout.presets.a]\nsize = 10\n[layout.presets.w]\nsize = 5\n",
    );
    let project = f.root(
        "project",
        "[layout.presets.a]\nsize = 20\n[layout.presets.p]\nsize = 7\n",
    );

    f.store.set_workspace_root(Some(workspace.clone()));
    assert_eq!(f.store.get::<Layout>().presets["a"].size, 10);
    f.store.set_project_root(Some(project.clone()));
    let l = f.store.get::<Layout>();
    assert_eq!(l.preset, "workspace", "the project did not set the preset");
    assert_eq!(l.presets["a"].size, 20, "project beats workspace");
    assert_eq!(
        l.presets["a"].panels,
        ["p"],
        "deep merge keeps what no layer above the user's touched"
    );
    assert_eq!(l.presets["w"].size, 5, "workspace keys survive");
    assert_eq!(l.presets["p"].size, 7);
    assert!(f.store.errors().is_empty(), "{:?}", f.store.errors());

    // A project outside any workspace layers only its own file.
    f.store.set_workspace_root(None);
    let l = f.store.get::<Layout>();
    assert_eq!(l.preset, "user");
    assert_eq!(l.presets["a"].size, 20);
    assert!(!l.presets.contains_key("w"));

    // Runtime still wins.
    f.store.set_runtime(Layout {
        preset: "runtime".into(),
        presets: Default::default(),
    });
    assert_eq!(f.store.get::<Layout>().preset, "runtime");
    assert_eq!(f.store.get::<Layout>().presets["a"].size, 20);

    f.store.set_project_root(None);
    assert!(!f.store.get::<Layout>().presets.contains_key("p"));
}

#[test]
fn the_project_layer_refuses_keys_that_may_not_be_overridden() {
    let f = Fixture::new(Some("[terminal]\nfont_size = 15\n"));
    f.store.register::<Terminal>();
    let project = f.root("project", "[terminal]\nfont_size = 99\n");
    f.store.set_project_root(Some(project.clone()));

    assert_eq!(f.store.get::<Terminal>().font_size, 15);
    let errors = f.store.errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].severity, Severity::Error);
    assert_eq!(errors[0].key_path, "terminal");
    assert_eq!(
        errors[0].file.as_deref(),
        Some(project.join(".hutan/settings.toml").as_path())
    );
    assert!(
        errors[0].message.contains("per project"),
        "{}",
        errors[0].message
    );
}

#[test]
fn a_broken_project_file_is_reported_with_its_path() {
    let f = Fixture::new(None);
    let project = f.root("project", "[layout]\npreset = \"mine\"\n");
    f.store.set_project_root(Some(project.clone()));
    assert_eq!(f.store.get::<Layout>().preset, "mine");

    write_file(&project.join(".hutan/settings.toml"), "[layout\n");
    f.store.set_project_root(Some(project.clone()));
    let errors = f.store.errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(
        errors[0].file.as_deref(),
        Some(project.join(".hutan/settings.toml").as_path())
    );
}

#[test]
fn workspace_and_project_can_be_resolved_without_making_them_active() {
    let f = Fixture::new(Some("[layout]\npreset = \"user\"\n"));
    let workspace = f.root("workspace", "[layout]\npreset = \"workspace\"\n");
    let project = f.root("project", "[layout.presets.p]\nsize = 7\n");
    let loose = f.root("loose", "[layout]\npreset = \"loose\"\n");

    let both = f
        .store
        .get_for_roots::<Layout>(Some(&workspace), Some(&project));
    assert_eq!(both.preset, "workspace");
    assert_eq!(both.presets["p"].size, 7);
    let only_project = f.store.get_for_roots::<Layout>(None, Some(&loose));
    assert_eq!(
        only_project.preset, "loose",
        "a project outside a workspace"
    );
    let only_workspace = f.store.get_for_roots::<Layout>(Some(&workspace), None);
    assert!(!only_workspace.presets.contains_key("p"));
    // Nothing active, nothing asked: the active value.
    assert_eq!(f.store.get_for_roots::<Layout>(None, None).preset, "user");

    // What the active layers say is the cached value.
    f.store.set_workspace_root(Some(workspace.clone()));
    f.store.set_project_root(Some(project.clone()));
    let active = f.store.get::<Layout>();
    assert!(Arc::ptr_eq(
        &active,
        &f.store
            .get_for_roots::<Layout>(Some(&workspace), Some(&project))
    ));
}

#[test]
fn invalid_value_at_startup_falls_back_and_reports_the_key() {
    let f = Fixture::new(Some(
        "[terminal]\nfont_size = \"big\"\nfont_family = \"Fira\"\n",
    ));
    let t = f.store.get::<Terminal>();
    assert_eq!(*t, Terminal::default(), "no last good value yet: defaults");
    let errors = f.store.errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].key_path, "terminal.font_size");
    assert_eq!(errors[0].file.as_deref(), Some(f.user_path().as_path()));
    assert!(errors[0].is_error());
}

#[test]
fn invalid_value_keeps_last_good_value_and_recovers() {
    let f = Fixture::new(Some("[terminal]\nfont_size = 15\n"));
    assert_eq!(f.store.get::<Terminal>().font_size, 15);

    f.store
        .write_user("terminal.font_size", toml::Value::String("huge".into()))
        .unwrap();
    assert_eq!(
        f.store.get::<Terminal>().font_size,
        15,
        "last good value kept"
    );
    let errors = f.store.errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].key_path, "terminal.font_size");

    f.store
        .write_user("terminal.font_size", toml::Value::Integer(17))
        .unwrap();
    assert_eq!(f.store.get::<Terminal>().font_size, 17);
    assert!(f.store.errors().is_empty());
}

#[test]
fn namespace_that_is_not_a_table_is_an_error() {
    let f = Fixture::new(Some("terminal = 5\n"));
    assert_eq!(*f.store.get::<Terminal>(), Terminal::default());
    let errors = f.store.errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].key_path, "terminal");
}

#[test]
fn unknown_keys_warn_without_failing() {
    let f = Fixture::new(Some(
        "[terminal]\nfont_size = 15\nbogus = 1\n[terminal.cursor]\nshape = \"x\"\n\n[nothing_here]\nx = 1\n",
    ));
    assert_eq!(
        f.store.get::<Terminal>().font_size,
        15,
        "value still applies"
    );
    let mut errors = f.store.errors();
    errors.sort_by(|a, b| a.key_path.cmp(&b.key_path));
    let summary: Vec<_> = errors
        .iter()
        .map(|e| (e.severity, e.key_path.as_str()))
        .collect();
    assert_eq!(
        summary,
        [
            (Severity::Warning, "nothing_here"),
            (Severity::Warning, "terminal.bogus"),
            (Severity::Warning, "terminal.cursor.shape"),
        ]
    );
    assert!(
        errors
            .iter()
            .all(|e| e.file.as_deref() == Some(f.user_path().as_path()))
    );
}

#[test]
fn unknown_namespace_warning_disappears_once_registered() {
    let f = Fixture::new(Some("[layout]\npreset = \"x\"\n"));
    f.store.register::<Terminal>();
    assert_eq!(f.store.errors().len(), 1);
    assert_eq!(f.store.errors()[0].key_path, "layout");
    f.store.register::<Layout>();
    assert!(f.store.errors().is_empty());
    assert_eq!(f.store.get::<Layout>().preset, "x");
}

#[test]
fn nested_namespaces_do_not_flag_their_parent_table() {
    let f = Fixture::new(Some(
        "[panels.sessions]\ngroup_by = \"none\"\n[panels.other]\nx = 1\n",
    ));
    f.store.register::<Sessions>();
    assert_eq!(f.store.get::<Sessions>().group_by, "none");
    let errors = f.store.errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].key_path, "panels.other");
}

#[test]
fn invalid_toml_keeps_previous_layer_and_reports_line() {
    let f = Fixture::new(Some("[terminal]\nfont_size = 15\n"));
    assert_eq!(f.store.get::<Terminal>().font_size, 15);
    write_file(&f.user_path(), "[terminal]\nfont_size = \n");
    // No watcher here: a second store over the same broken file starts from defaults + error.
    let again = SettingsStore::new(SettingsPaths::in_dir(f.dir.path()));
    assert_eq!(again.get::<Terminal>().font_size, 13);
    let errors = again.errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].message.contains("line 2"),
        "{}",
        errors[0].message
    );
    assert_eq!(errors[0].key_path, "");
}

#[test]
fn write_user_creates_file_and_private_directory() {
    let dir = TempDir::new().unwrap();
    let base = dir.path().join("nested/Hutan");
    let store = SettingsStore::new(SettingsPaths::in_dir(&base));
    store
        .write_user(
            "appearance.density",
            toml::Value::String("comfortable".into()),
        )
        .unwrap();

    let text = std::fs::read_to_string(base.join("settings.toml")).unwrap();
    assert_eq!(text, "[appearance]\ndensity = \"comfortable\"\n");
    let mode = std::fs::metadata(&base).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
    assert_eq!(
        store.get::<AppearanceSettings>().density,
        Density::Comfortable,
        "applied without waiting for a watcher"
    );
}

#[test]
fn write_user_preserves_comments_and_formatting() {
    const ORIGINAL: &str = "\
# Hutan settings
# Hand-edited; keep my notes.

[terminal]
# the font
font_family = \"Fira Code\"   # not Menlo
font_size = 13 # readable

   [layout]   # trailing on header
   preset = \"default\"
";
    let f = Fixture::new(Some(ORIGINAL));
    f.store
        .write_user("terminal.font_size", toml::Value::Integer(16))
        .unwrap();
    f.store
        .write_user("terminal.cursor.blink", toml::Value::Boolean(false))
        .unwrap();

    let text = std::fs::read_to_string(f.user_path()).unwrap();
    let expected = "\
# Hutan settings
# Hand-edited; keep my notes.

[terminal]
# the font
font_family = \"Fira Code\"   # not Menlo
font_size = 16 # readable

[terminal.cursor]
blink = false

   [layout]   # trailing on header
   preset = \"default\"
";
    // toml_edit places the new sub-table after `[terminal]`'s keys; assert on the exact text so a
    // formatting regression is visible.
    assert_eq!(text, expected);
    assert!(!f.store.get::<Terminal>().cursor.blink);
    assert_eq!(f.store.get::<Terminal>().font_size, 16);
}

#[test]
fn write_user_refuses_to_clobber_a_broken_file() {
    let f = Fixture::new(Some("[terminal\n"));
    let broken = std::fs::read_to_string(f.user_path()).unwrap();
    let err = f
        .store
        .write_user("terminal.font_size", toml::Value::Integer(1))
        .unwrap_err();
    assert!(err.message.contains("invalid TOML"), "{}", err.message);
    assert_eq!(std::fs::read_to_string(f.user_path()).unwrap(), broken);
}

#[test]
fn write_user_follows_symlinked_settings_file() {
    let dir = TempDir::new().unwrap();
    let real = dir.path().join("dotfiles/settings.toml");
    write_file(&real, "# mine\n[terminal]\nfont_size = 12\n");
    let base = dir.path().join("app");
    std::fs::create_dir_all(&base).unwrap();
    std::os::unix::fs::symlink(&real, base.join("settings.toml")).unwrap();

    let store = SettingsStore::new(SettingsPaths::in_dir(&base));
    store
        .write_user("terminal.font_size", toml::Value::Integer(14))
        .unwrap();
    assert!(
        base.join("settings.toml")
            .symlink_metadata()
            .unwrap()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_to_string(&real).unwrap(),
        "# mine\n[terminal]\nfont_size = 14\n"
    );
}

#[test]
fn changes_report_changed_namespaces_only() {
    let f = Fixture::new(None);
    f.store.register::<Terminal>();
    f.store.register::<Layout>();
    let mut rx = f.store.changes();
    assert_eq!(rx.borrow_and_update().generation, 0);

    f.store
        .write_user("terminal.font_size", toml::Value::Integer(20))
        .unwrap();
    assert!(rx.has_changed().unwrap());
    let cs = rx.borrow_and_update().clone();
    assert_eq!(cs.generation, 1);
    assert_eq!(cs.namespaces.iter().collect::<Vec<_>>(), ["terminal"]);

    // Same value again: nothing changed, nothing published.
    f.store
        .write_user("terminal.font_size", toml::Value::Integer(20))
        .unwrap();
    assert!(!rx.has_changed().unwrap());

    f.store.set_runtime(Layout {
        preset: "r".into(),
        presets: Default::default(),
    });
    let cs = rx.borrow_and_update().clone();
    assert_eq!(cs.generation, 2);
    assert_eq!(cs.namespaces.iter().collect::<Vec<_>>(), ["layout"]);
}

#[test]
fn get_returns_a_shared_arc_until_something_changes() {
    let f = Fixture::new(None);
    let a = f.store.get::<Terminal>();
    let b = f.store.get::<Terminal>();
    assert!(Arc::ptr_eq(&a, &b));
    f.store
        .write_user("layout.preset", toml::Value::String("x".into()))
        .unwrap();
    assert!(
        Arc::ptr_eq(&a, &f.store.get::<Terminal>()),
        "unrelated namespace untouched"
    );
}

#[test]
fn schema_contains_registered_namespaces() {
    let f = Fixture::new(None);
    f.store.register::<Terminal>();
    f.store.register::<Layout>();
    f.store.register::<Sessions>();
    f.store.register::<AppearanceSettings>();
    let schema = f.store.schema();
    let namespaces = schema["namespaces"].as_object().unwrap();
    let mut names: Vec<_> = namespaces.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["appearance", "layout", "panels.sessions", "terminal"]
    );

    assert_eq!(namespaces["terminal"]["workspace_overridable"], false);
    assert_eq!(namespaces["layout"]["workspace_overridable"], true);
    let props = &namespaces["terminal"]["schema"]["properties"];
    assert!(props["font_size"].is_object(), "{props}");
    assert!(props["font_family"].is_object());
    let density = &namespaces["appearance"]["schema"];
    assert!(density.to_string().contains("comfortable"), "{density}");
}

#[test]
fn conflicting_namespace_registration_is_reported_and_ignored() {
    let f = Fixture::new(None);
    f.store.register::<Terminal>();
    f.store.register::<Impostor>();
    assert_eq!(
        f.store.get::<Impostor>().x,
        0,
        "impostor only ever sees defaults"
    );
    assert_eq!(f.store.get::<Terminal>().font_size, 13);
    let errors = f.store.errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].message.contains("already registered"));
}

#[test]
fn keymap_file_errors_surface_and_keymap_merges_with_defaults() {
    let f = Fixture::new(None);
    let keymap_path = f.dir.path().join("keymap.toml");
    write_file(
        &keymap_path,
        "[[binding]]\nkeys = \"cmd-d\"\naction = \"none\"\n[[binding]]\nkeys = \"cmd-wat\"\naction = \"x::Y\"\n",
    );
    let store = SettingsStore::new(SettingsPaths::in_dir(f.dir.path()));
    let defaults = vec![
        keymap::BindingSpec::new("cmd-d", "workbench::SplitRight", None).unwrap(),
        keymap::BindingSpec::new("cmd-t", "workbench::NewTerminal", None).unwrap(),
    ];
    let loaded = store.keymap(&defaults);
    assert_eq!(loaded.bindings.len(), 1);
    assert_eq!(loaded.bindings[0].action, "workbench::NewTerminal");
    let errors = store.errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].key_path, "binding[1].keys");
    assert_eq!(errors[0].file.as_deref(), Some(Path::new(&keymap_path)));
}

#[test]
fn paths_resolve_from_hutan_home_or_app_support() {
    let p = SettingsPaths::resolve(Some("/h".into()), Some("/Users/u".into())).unwrap();
    assert_eq!(p.user, Path::new("/h/settings.toml"));
    assert_eq!(p.keymap, Path::new("/h/keymap.toml"));
    let p = SettingsPaths::resolve(Some("".into()), Some("/Users/u".into())).unwrap();
    assert_eq!(
        p.user,
        Path::new("/Users/u/Library/Application Support/Hutan/settings.toml")
    );
    assert!(SettingsPaths::resolve(None, None).is_err());
}

#[test]
fn application_notes_join_the_diagnostics_and_publish_a_change() {
    let fixture = Fixture::new(None);
    let mut changes = fixture.store.changes();
    let before = changes.borrow_and_update().generation;
    assert!(fixture.store.errors().is_empty());

    fixture.store.set_notes(
        "check",
        vec![hutan_settings::SettingsError::warning(
            None,
            "app.on_last_window_close",
            "needs app.menu_bar",
        )],
    );
    let errors = fixture.store.errors();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].severity, Severity::Warning);
    assert!(errors[0].message.contains("app.menu_bar"));
    assert!(changes.has_changed().unwrap());
    let published = changes.borrow_and_update().clone();
    assert!(published.generation > before);
    assert!(
        published.namespaces.is_empty(),
        "only the diagnostics changed"
    );

    // The same notes again change nothing; an empty list clears them.
    fixture.store.set_notes(
        "check",
        vec![hutan_settings::SettingsError::warning(
            None,
            "app.on_last_window_close",
            "needs app.menu_bar",
        )],
    );
    assert!(!changes.has_changed().unwrap());
    fixture.store.set_notes("check", Vec::new());
    assert!(fixture.store.errors().is_empty());
    assert!(changes.has_changed().unwrap());
}

#[test]
fn layers_say_where_a_value_comes_from_and_remove_falls_back_to_the_layer_below() {
    let f = Fixture::new(Some("# mine\n[layout]\npreset = \"user\"\n"));
    f.store.register::<Layout>();
    f.store.register::<Terminal>();
    assert_eq!(f.store.layers().len(), 1, "only the user's file at first");
    assert_eq!(
        f.store.source_of("layout.preset"),
        Some(SettingsLayer::User)
    );
    assert_eq!(f.store.source_of("layout.presets"), None);

    let workspace = f.root("workspace", "[layout]\npreset = \"workspace\"\n");
    let project = f.root("project", "");
    f.store.set_workspace_root(Some(workspace.clone()));
    f.store.set_project_root(Some(project.clone()));
    let layers = f.store.layers();
    assert_eq!(
        layers.iter().map(|l| l.layer).collect::<Vec<_>>(),
        [
            SettingsLayer::User,
            SettingsLayer::Workspace,
            SettingsLayer::Project
        ]
    );
    assert_eq!(layers[2].root.as_deref(), Some(project.as_path()));
    assert_eq!(
        f.store.source_of("layout.preset"),
        Some(SettingsLayer::Workspace)
    );

    // The project layer is writable and applies at once.
    f.store
        .write(
            SettingsLayer::Project,
            "layout.preset",
            toml::Value::String("project".into()),
        )
        .unwrap();
    assert_eq!(f.store.get::<Layout>().preset, "project");
    assert_eq!(
        f.store.source_of("layout.preset"),
        Some(SettingsLayer::Project)
    );
    assert!(
        std::fs::read_to_string(project.join(".hutan/settings.toml"))
            .unwrap()
            .contains("project")
    );
    assert_eq!(
        f.store.layer_value(SettingsLayer::User, "layout.preset"),
        Some(toml::Value::String("user".into()))
    );

    // Removing falls back one layer at a time, and the file really loses the key.
    assert!(
        f.store
            .remove(SettingsLayer::Project, "layout", "preset")
            .unwrap()
    );
    assert_eq!(f.store.get::<Layout>().preset, "workspace");
    assert!(
        !std::fs::read_to_string(project.join(".hutan/settings.toml"))
            .unwrap()
            .contains("preset")
    );
    assert!(
        f.store
            .remove(SettingsLayer::Workspace, "layout", "preset")
            .unwrap()
    );
    assert_eq!(f.store.get::<Layout>().preset, "user");
    assert!(
        !f.store
            .remove(SettingsLayer::Workspace, "layout", "preset")
            .unwrap()
    );
    assert!(
        f.store
            .remove(SettingsLayer::User, "layout", "preset")
            .unwrap()
    );
    assert_eq!(f.store.get::<Layout>().preset, "default");
    assert_eq!(f.store.source_of("layout.preset"), None);
    assert!(
        std::fs::read_to_string(f.user_path())
            .unwrap()
            .starts_with("# mine"),
        "comments stay"
    );

    // Not overridable: the workspace file does not count as the source.
    write_file(
        &workspace.join(".hutan/settings.toml"),
        "[terminal]\nfont_size = 20\n",
    );
    f.store.set_workspace_root(Some(workspace.clone()));
    assert_eq!(f.store.source_of("terminal.font_size"), None);
}

#[test]
fn removing_from_a_layer_that_does_not_exist_is_an_error_and_a_missing_file_is_not() {
    let f = Fixture::new(None);
    f.store.register::<Layout>();
    let error = f
        .store
        .remove(SettingsLayer::Project, "layout", "preset")
        .unwrap_err();
    assert!(error.to_string().contains("no project folder"), "{error}");
    assert!(
        !f.store
            .remove(SettingsLayer::User, "layout", "preset")
            .unwrap()
    );
    let error = f
        .store
        .write(
            SettingsLayer::Workspace,
            "layout.preset",
            toml::Value::String("x".into()),
        )
        .unwrap_err();
    assert!(error.to_string().contains("no workspace folder"), "{error}");
}

#[test]
fn reload_reads_the_files_again() {
    let f = Fixture::new(Some("[layout]\npreset = \"a\"\n"));
    f.store.register::<Layout>();
    assert_eq!(f.store.get::<Layout>().preset, "a");
    write_file(&f.user_path(), "[layout]\npreset = \"b\"\n");
    f.store.reload();
    assert_eq!(f.store.get::<Layout>().preset, "b");
}

#[test]
fn an_upgrade_hook_maps_an_old_key_in_each_layer_and_never_touches_the_defaults() {
    use common::Renamed;
    let f = Fixture::new(Some("[renamed]\nold_size = 12\n"));
    assert_eq!(f.store.get::<Renamed>().size, "large");
    assert!(f.store.errors().is_empty(), "{:?}", f.store.errors());

    // A layer that sets the new key itself wins over the old one beside it.
    write_file(
        &f.user_path(),
        "[renamed]\nold_size = 12\nsize = \"tiny\"\n",
    );
    f.store.reload();
    assert_eq!(f.store.get::<Renamed>().size, "tiny");

    // A layer that sets neither leaves the default alone.
    write_file(&f.user_path(), "[renamed]\n");
    f.store.reload();
    assert_eq!(f.store.get::<Renamed>().size, "medium");
}
