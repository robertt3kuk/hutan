//! `hutan theme ls|validate|new|export` against the real binary: `--json`, exit statuses, kind
//! disambiguation, no-clobber, the `new` -> `validate` -> select flow, and no daemon: every
//! run gets a temporary `HUTAN_HOME` and a socket path that must stay absent.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{HUTAN, Out, command, finish};
use serde_json::Value;
use tempfile::TempDir;

struct Home {
    dir: TempDir,
    socket: PathBuf,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("never.sock");
        Self { dir, socket }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    async fn hutan(&self, args: &[&str]) -> Out {
        let mut command = command(Path::new(HUTAN), Some(&self.socket), args);
        command
            .env("HUTAN_HOME", self.path())
            .env("HUTAN_SOCKET", &self.socket);
        finish(command).await
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.path().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// Nothing but packs and the settings file may appear, and no daemon file or socket.
    fn assert_no_daemon(&self) {
        assert!(!self.socket.exists(), "the socket was created");
        for entry in fs::read_dir(self.path()).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            assert!(
                matches!(
                    name.as_str(),
                    "themes" | "icon-themes" | "settings.toml" | "out"
                ),
                "unexpected file {name}"
            );
        }
    }
}

const BROKEN: &str = "schema_version = 1\nid = \"broken\"\nname = \"Broken\"\nauthors = [\"t\"]\nlicense = \"MIT\"\nappearance = \"dark\"\nextends = \"builtin:hutan-dark\"\n\n[colors]\n\"ui.border\" = \"oops\"\n";
// A pale accent: valid, but the white text on it is advice-worthy (a warning).
const PALE: &str = "schema_version = 1\nid = \"pale\"\nname = \"Pale\"\nauthors = [\"t\"]\nlicense = \"MIT\"\nappearance = \"dark\"\nextends = \"builtin:hutan-dark\"\n\n[colors]\n\"ui.accent\" = \"#eeeeaa\"\n";

fn ids(doc: &Value) -> Vec<String> {
    doc["themes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap_or("").to_owned())
        .collect()
}

#[tokio::test]
async fn ls_lists_embedded_user_and_invalid_packs_as_versioned_json() {
    let home = Home::new();
    home.write("themes/pale/theme.toml", PALE);
    home.write("themes/broken/theme.toml", BROKEN);
    home.write("settings.toml", "[appearance]\ndark_theme = \"pale\"\n");
    let out = home.hutan(&["theme", "ls", "--json"]).await.ok();
    let doc = out.json();
    assert_eq!(doc["version"], 1);
    assert_eq!(doc["kind"], "color");
    let themes = doc["themes"].as_array().unwrap();
    assert_eq!(
        ids(&doc),
        ["hutan-dark", "hutan-light", "pale", ""],
        "the invalid pack has no id yet"
    );
    let dark = &themes[0];
    assert_eq!(dark["origin"], "embedded");
    assert_eq!(dark["appearance"], "dark");
    assert_eq!(dark["valid"], true);
    assert_eq!(dark["selected_as"], serde_json::json!([]));
    assert_eq!(themes[1]["selected_as"], serde_json::json!(["light"]));
    let pale = &themes[2];
    assert_eq!(pale["origin"], "user");
    assert_eq!(pale["selected_as"], serde_json::json!(["dark"]));
    assert!(
        pale["diagnostics"][0]["severity"] == "warning"
            && pale["diagnostics"][0]["key"] == "contrast"
    );
    let broken = &themes[3];
    assert_eq!(broken["valid"], false);
    assert_eq!(broken["id"], Value::Null);
    assert_eq!(broken["name"], "broken", "named after its folder");
    let error = &broken["diagnostics"][0];
    assert_eq!(error["severity"], "error");
    assert_eq!(error["key"], "colors.ui.border");
    assert!(
        error["file"]
            .as_str()
            .unwrap()
            .ends_with("themes/broken/theme.toml")
    );
    assert!(error["message"].as_str().unwrap().contains("#rrggbb"));
    assert_eq!(out.stderr, "");

    // Human output: a table with the problem spelled out.
    let text = home.hutan(&["theme", "ls"]).await.ok().stdout;
    assert!(
        text.contains("hutan-dark") && text.contains("embedded"),
        "{text}"
    );
    assert!(text.contains("invalid (1 error)"), "{text}");
    assert!(text.contains("colors.ui.border"), "{text}");

    // The other kind: Phosphor only.
    let icons = home
        .hutan(&["theme", "ls", "--kind", "icon", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(icons["kind"], "icon");
    assert_eq!(ids(&icons), ["phosphor"]);
    assert_eq!(
        icons["themes"][0]["selected_as"],
        serde_json::json!(["icon"])
    );
    home.assert_no_daemon();
}

#[tokio::test]
async fn validate_exit_statuses_follow_errors_warnings_and_strict() {
    let home = Home::new();
    home.write("themes/pale/theme.toml", PALE);
    home.write("themes/broken/theme.toml", BROKEN);

    let ok = home.hutan(&["theme", "validate", "hutan-dark"]).await;
    assert_eq!(ok.code, 0, "{ok:?}");
    assert!(ok.stdout.contains("ok"), "{}", ok.stdout);

    // Warnings are exit 0 ...
    let warn = home.hutan(&["theme", "validate", "pale", "--json"]).await;
    assert_eq!(warn.code, 0, "{warn:?}");
    let doc = warn.json();
    assert_eq!(
        (doc["valid"].clone(), doc["ok"].clone()),
        (true.into(), true.into())
    );
    assert_eq!(doc["warnings"], 1);
    assert_eq!(doc["id"], "pale");
    // ... and 1 with --strict.
    let strict = home
        .hutan(&["theme", "validate", "pale", "--strict", "--json"])
        .await;
    assert_eq!(strict.code, 1, "{strict:?}");
    let doc = strict.json();
    assert_eq!(
        (doc["valid"].clone(), doc["ok"].clone()),
        (true.into(), false.into())
    );
    assert_eq!(strict.stderr, "", "the report is on stdout, not repeated");

    // Errors: 1, with the file, the key and the reason on one line.
    let bad = home.hutan(&["theme", "validate", "broken"]).await;
    assert_eq!(bad.code, 1, "{bad:?}");
    assert!(
        bad.stdout.contains("colors.ui.border") && bad.stdout.contains("themes/broken/theme.toml"),
        "{}",
        bad.stdout
    );
    let by_path = home
        .hutan(&[
            "theme",
            "validate",
            home.path().join("themes/broken").to_str().unwrap(),
            "--json",
        ])
        .await;
    assert_eq!(by_path.code, 1);
    assert_eq!(by_path.json()["errors"], 1);

    // Not found and unreadable: 1. Invalid invocations: 2.
    assert_eq!(home.hutan(&["theme", "validate", "nope"]).await.code, 1);
    assert_eq!(home.hutan(&["theme", "validate", "./nope"]).await.code, 1);
    assert_eq!(home.hutan(&["theme", "validate"]).await.code, 2);
    assert_eq!(
        home.hutan(&["theme", "validate", "pale", "--kind", "font"])
            .await
            .code,
        2
    );
    assert_eq!(home.hutan(&["theme"]).await.code, 2);
    assert_eq!(
        home.hutan(&["theme", "import", "x.json"]).await.code,
        2,
        "import is TH-4"
    );
    home.assert_no_daemon();
}

#[tokio::test]
async fn the_kind_decides_what_an_id_or_a_folder_means() {
    let home = Home::new();
    home.write("themes/pale/theme.toml", PALE);
    let made = home
        .hutan(&[
            "theme",
            "new",
            "lines",
            "--kind",
            "icon",
            "--name",
            "Lines",
            "--author",
            "me",
            "--license",
            "MIT",
        ])
        .await;
    assert_eq!(made.code, 0, "{made:?}");

    // `pale` is a color theme and `lines` an icon theme: the wrong --kind says so.
    let wrong = home.hutan(&["theme", "validate", "lines"]).await;
    assert_eq!(wrong.code, 1);
    assert!(wrong.stderr.contains("--kind icon"), "{}", wrong.stderr);
    let wrong = home
        .hutan(&["theme", "validate", "pale", "--kind", "icon"])
        .await;
    assert_eq!(wrong.code, 1);
    assert!(wrong.stderr.contains("--kind color"), "{}", wrong.stderr);
    let folder = home.path().join("icon-themes/lines");
    let wrong = home
        .hutan(&["theme", "validate", folder.to_str().unwrap()])
        .await;
    assert_eq!(wrong.code, 1);
    assert!(wrong.stderr.contains("icon pack"), "{}", wrong.stderr);
    assert_eq!(
        home.hutan(&["theme", "validate", "lines", "--kind", "icon"])
            .await
            .code,
        0
    );
    assert_eq!(
        home.hutan(&[
            "theme",
            "validate",
            folder.to_str().unwrap(),
            "--kind",
            "icon"
        ])
        .await
        .code,
        0
    );
    home.assert_no_daemon();
}

#[tokio::test]
async fn new_validate_select_export_is_one_flow_without_a_daemon() {
    let home = Home::new();
    let new = home
        .hutan(&[
            "theme",
            "new",
            "canopy-night",
            "--name",
            "Canopy Night",
            "--author",
            "A. Author",
            "--license",
            "MIT OR Apache-2.0",
            "--json",
        ])
        .await;
    assert_eq!(new.code, 0, "{new:?}");
    let created = new.json();
    assert_eq!(created["version"], 1);
    let manifest = PathBuf::from(created["manifest"].as_str().unwrap());
    assert!(manifest.ends_with("themes/canopy-night/theme.toml"));
    let text = fs::read_to_string(&manifest).unwrap();
    assert!(
        text.contains("extends = \"builtin:hutan-dark\"")
            && text.contains("authors = [\"A. Author\"]"),
        "{text}"
    );

    // It validates as created ...
    let verdict = home
        .hutan(&["theme", "validate", "canopy-night", "--strict", "--json"])
        .await;
    assert_eq!(verdict.code, 0, "{verdict:?}");
    assert_eq!(verdict.json()["diagnostics"], serde_json::json!([]));

    // ... refuses to be created over ...
    fs::write(&manifest, format!("{text}\n# mine\n")).unwrap();
    let again = home
        .hutan(&[
            "theme",
            "new",
            "canopy-night",
            "--name",
            "X",
            "--author",
            "y",
            "--license",
            "MIT",
        ])
        .await;
    assert_eq!(again.code, 1, "{again:?}");
    assert!(again.stderr.contains("already exists"), "{}", again.stderr);
    assert!(
        fs::read_to_string(&manifest).unwrap().ends_with("# mine\n"),
        "not clobbered"
    );

    // ... is selected the way Settings selects it (the key it writes) ...
    home.write(
        "settings.toml",
        "[appearance]\ndark_theme = \"canopy-night\"\n",
    );
    let ls = home.hutan(&["theme", "ls", "--json"]).await.ok().json();
    let row = ls["themes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "canopy-night")
        .unwrap()
        .clone();
    assert_eq!(row["selected_as"], serde_json::json!(["dark"]));
    assert_eq!(row["origin"], "user");

    // ... and is exported as an editable pack, plain and resolved, that validates elsewhere.
    let out = home.path().join("out");
    let plain = home
        .hutan(&[
            "theme",
            "export",
            "canopy-night",
            "--output",
            out.to_str().unwrap(),
            "--json",
        ])
        .await;
    assert_eq!(plain.code, 0, "{plain:?}");
    let exported = out.join("canopy-night/theme.toml");
    assert_eq!(
        fs::read_to_string(&exported).unwrap(),
        fs::read_to_string(&manifest).unwrap()
    );
    let refused = home
        .hutan(&[
            "theme",
            "export",
            "canopy-night",
            "--output",
            out.to_str().unwrap(),
        ])
        .await;
    assert_eq!(refused.code, 1);
    assert!(
        refused.stderr.contains("already exists"),
        "{}",
        refused.stderr
    );

    let flat = home.path().join("out/flat");
    let resolved = home
        .hutan(&[
            "theme",
            "export",
            "canopy-night",
            "--resolved",
            "--output",
            flat.to_str().unwrap(),
        ])
        .await;
    assert_eq!(resolved.code, 0, "{resolved:?}");
    let flat_text = fs::read_to_string(flat.join("canopy-night/theme.toml")).unwrap();
    assert!(
        flat_text.contains("root = true") && !flat_text.contains("extends"),
        "{flat_text}"
    );
    let check = home
        .hutan(&[
            "theme",
            "validate",
            flat.join("canopy-night").to_str().unwrap(),
            "--strict",
        ])
        .await;
    assert_eq!(check.code, 0, "{check:?}");
    assert_eq!(
        home.hutan(&[
            "theme",
            "export",
            "nope",
            "--output",
            flat.to_str().unwrap()
        ])
        .await
        .code,
        1
    );
    assert_eq!(
        home.hutan(&["theme", "export", "canopy-night"]).await.code,
        2,
        "--output is required"
    );
    home.assert_no_daemon();
}

#[tokio::test]
async fn the_two_icon_example_pack_goes_through_the_same_flow() {
    let home = Home::new();
    let new = home
        .hutan(&[
            "theme",
            "new",
            "canopy-lines",
            "--kind",
            "icon",
            "--name",
            "Canopy Lines",
            "--author",
            "A. Author",
            "--license",
            "MIT OR Apache-2.0",
            "--json",
        ])
        .await;
    assert_eq!(new.code, 0, "{new:?}");
    assert_eq!(
        new.json()["files"],
        serde_json::json!(["icon-theme.toml", "icons/close.svg"])
    );
    // The author adds the second icon of the example.
    let dir = home.path().join("icon-themes/canopy-lines");
    fs::write(
        dir.join("icons/search.svg"),
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.75\" stroke-linecap=\"round\" stroke-linejoin=\"round\"><circle cx=\"10.5\" cy=\"10.5\" r=\"6.5\"/><path d=\"M15.5 15.5 21 21\"/></svg>",
    )
    .unwrap();
    let manifest = fs::read_to_string(dir.join("icon-theme.toml")).unwrap();
    fs::write(
        dir.join("icon-theme.toml"),
        format!("{manifest}\"nav.search\" = \"icons/search.svg\"\n"),
    )
    .unwrap();
    let verdict = home
        .hutan(&[
            "theme",
            "validate",
            "canopy-lines",
            "--kind",
            "icon",
            "--strict",
            "--json",
        ])
        .await;
    assert_eq!(verdict.code, 0, "{verdict:?}");

    // A broken asset is one actionable diagnostic: file, key, reason.
    fs::write(
        dir.join("icons/search.svg"),
        "<svg viewBox=\"0 0 24 24\"><script/></svg>",
    )
    .unwrap();
    let bad = home
        .hutan(&[
            "theme",
            "validate",
            "canopy-lines",
            "--kind",
            "icon",
            "--json",
        ])
        .await;
    assert_eq!(bad.code, 0, "an asset falls back per id: a warning");
    let doc = bad.json();
    assert_eq!(doc["warnings"], 1, "{doc}");
    let warning = &doc["diagnostics"][0];
    assert_eq!(warning["key"], "icons.nav.search");
    assert!(
        warning["file"]
            .as_str()
            .unwrap()
            .ends_with("icon-themes/canopy-lines/icon-theme.toml")
    );
    assert!(
        warning["message"].as_str().unwrap().contains("script"),
        "{warning}"
    );
    assert_eq!(
        home.hutan(&[
            "theme",
            "validate",
            "canopy-lines",
            "--kind",
            "icon",
            "--strict"
        ])
        .await
        .code,
        1
    );
    fs::write(
        dir.join("icons/search.svg"),
        "<svg viewBox=\"0 0 24 24\"><path d=\"M1 1L5 5\" stroke=\"currentColor\"/></svg>",
    )
    .unwrap();

    let out = home.path().join("out");
    let exported = home
        .hutan(&[
            "theme",
            "export",
            "canopy-lines",
            "--kind",
            "icon",
            "--resolved",
            "--output",
            out.to_str().unwrap(),
            "--json",
        ])
        .await;
    assert_eq!(exported.code, 0, "{exported:?}");
    let files = exported.json()["files"].as_array().unwrap().len();
    assert!(files > 60, "every icon of the catalog is written: {files}");
    assert!(out.join("canopy-lines/LICENSE-phosphor").is_file());
    let check = home
        .hutan(&[
            "theme",
            "validate",
            out.join("canopy-lines").to_str().unwrap(),
            "--kind",
            "icon",
            "--strict",
        ])
        .await;
    assert_eq!(check.code, 0, "{check:?}");
    home.assert_no_daemon();
}

#[tokio::test]
async fn new_with_a_bad_invocation_exits_2_and_writes_nothing() {
    let home = Home::new();
    let args = |extra: &[&'static str]| {
        let mut all = vec!["theme", "new"];
        all.extend_from_slice(extra);
        all
    };
    for bad in [
        // an id that is not a pack id, a reserved one, a missing required option, a wrong base
        args(&["Bad_Id", "--name", "X", "--author", "y", "--license", "MIT"]),
        args(&["auto", "--name", "X", "--author", "y", "--license", "MIT"]),
        args(&["ok", "--author", "y", "--license", "MIT"]),
        args(&[
            "ok",
            "--name",
            "X",
            "--author",
            "y",
            "--license",
            "MIT",
            "--extends",
            "builtin:nope",
        ]),
        args(&[
            "ok",
            "--name",
            "X",
            "--author",
            "y",
            "--license",
            "MIT",
            "--extends",
            "builtin:phosphor",
        ]),
        args(&["ok", "--name", "X\nY", "--author", "y", "--license", "MIT"]),
    ] {
        let out = home.hutan(&bad).await;
        assert_eq!(out.code, 2, "{bad:?}: {out:?}");
    }
    assert!(!home.path().join("themes").exists());
    home.assert_no_daemon();
}

#[tokio::test]
async fn a_resolved_export_carries_the_users_overrides() {
    let home = Home::new();
    home.write(
        "settings.toml",
        "[appearance.theme_overrides.\"hutan-dark\".colors]\n\"ui.accent\" = \"#336699\"\n",
    );
    let out = home.path().join("out");
    let plain = home
        .hutan(&[
            "theme",
            "export",
            "hutan-dark",
            "--output",
            out.to_str().unwrap(),
        ])
        .await;
    assert_eq!(plain.code, 0, "{plain:?}");
    assert!(
        !fs::read_to_string(out.join("hutan-dark/theme.toml"))
            .unwrap()
            .contains("#336699")
    );
    let flat = out.join("flat");
    let resolved = home
        .hutan(&[
            "theme",
            "export",
            "hutan-dark",
            "--resolved",
            "--output",
            flat.to_str().unwrap(),
        ])
        .await;
    assert_eq!(resolved.code, 0, "{resolved:?}");
    assert!(resolved.stdout.contains("override"), "{}", resolved.stdout);
    assert!(
        fs::read_to_string(flat.join("hutan-dark/theme.toml"))
            .unwrap()
            .contains("\"ui.accent\" = \"#336699\"")
    );
    home.assert_no_daemon();
}
