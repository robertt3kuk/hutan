//! Validation of the value types: what can never reach an argv.

use hutan_container::{AbsPath, ContainerId, EnvVar, ImageRef, Labels, SpecError, VolumeName};

#[test]
fn names_cannot_be_options_or_paths() {
    for ok in ["hutan-s1", "a", "A_b.c-9", "0abc"] {
        assert!(
            ContainerId::new(ok).is_ok() && VolumeName::new(ok).is_ok(),
            "{ok}"
        );
    }
    for bad in [
        "",
        "-rf",
        "--all",
        ".hidden",
        "a b",
        "a/b",
        "a,b",
        "a=b",
        "a:b",
        "ü",
        "a\n",
        &"x".repeat(129),
    ] {
        assert_eq!(
            ContainerId::new(bad),
            Err(SpecError::InvalidName(bad.to_string())),
            "{bad:?}"
        );
        assert!(VolumeName::new(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn absolute_paths_are_plain_and_unambiguous() {
    for ok in [
        "/a",
        "/Users/me/My Projects/repo",
        "/a/b-c_d.e",
        "/with:colon",
        "/with=eq",
        "/ünïcode",
    ] {
        assert!(AbsPath::new(ok).is_ok(), "{ok}");
    }
    for bad in [
        "",
        "relative/path",
        "./a",
        "/",
        "//",
        "/a/",
        "/a//b",
        "/a/./b",
        "/a/../b",
        "/..",
        "/a,b",
        "/a\"b",
        "/a\nb",
        "/a\0b",
        "/a\tb",
    ] {
        assert!(
            matches!(AbsPath::new(bad), Err(SpecError::InvalidPath { .. })),
            "{bad:?}"
        );
    }
    assert!(matches!(
        AbsPath::from_path(std::path::Path::new("rel")),
        Err(SpecError::InvalidPath { .. })
    ));
}

#[test]
fn path_nesting_is_by_component_not_by_prefix() {
    let repo = AbsPath::new("/w/repo").unwrap();
    assert!(AbsPath::new("/w/repo/target").unwrap().is_inside(&repo));
    assert!(AbsPath::new("/w/repo/a/b").unwrap().is_inside(&repo));
    assert!(!AbsPath::new("/w/repo").unwrap().is_inside(&repo));
    assert!(!AbsPath::new("/w/repo2").unwrap().is_inside(&repo));
    assert!(!AbsPath::new("/w").unwrap().is_inside(&repo));
    assert_eq!(repo.depth(), 2);
    assert_eq!(repo.join("hooks").unwrap().as_str(), "/w/repo/hooks");
    assert!(repo.join("..").is_err());
}

#[test]
fn images_are_fully_qualified_and_pinned() {
    for ok in [
        "docker.io/library/alpine:3.20",
        "ghcr.io/hutan/agent:1.0",
        "localhost/agent:dev",
        "localhost:5000/team/agent:2",
        "registry.example.com:5000/a/b@sha256:0123456789abcdef",
        "ghcr.io/hutan/agent:1.0@sha256:0123",
    ] {
        assert!(ImageRef::new(ok).is_ok(), "{ok}");
    }
    for bad in [
        "",
        "alpine",
        "alpine:3.20",
        "library/alpine:3.20",
        "ghcr.io/hutan/agent",
        "localhost:5000/agent",
        "-v/x:1",
        "ghcr.io/a b:1",
        "ghcr.io/a;rm:1",
        "--rm",
        "ghcr.io/a\n:1",
    ] {
        assert!(
            matches!(ImageRef::new(bad), Err(SpecError::InvalidImage { .. })),
            "{bad:?}"
        );
    }
}

#[test]
fn labels_are_hutan_namespaced_and_plain() {
    let mut labels = Labels::default();
    labels.insert("hutan.session", "01J9Z-abc_1.2").unwrap();
    labels.insert("hutan.account", "claude/work@x").unwrap();
    assert_eq!(labels.get("hutan.session"), Some("01J9Z-abc_1.2"));
    assert_eq!(
        labels.iter().map(|(k, _)| k).collect::<Vec<_>>(),
        ["hutan.account", "hutan.session"]
    );
    for (key, value) in [
        ("session", "x"),
        ("hutan.", "x"),
        ("hutan.Session", "x"),
        ("hutan.a b", "x"),
        ("com.docker.x", "x"),
        ("hutan.a", ""),
        ("hutan.a", "a=b"),
        ("hutan.a", "a b"),
        ("hutan.a", "a\nb"),
        ("hutan.a", &"x".repeat(257)),
    ] {
        assert!(
            matches!(
                labels.insert(key, value),
                Err(SpecError::InvalidLabel { .. })
            ),
            "{key}={value:?}"
        );
    }
}

#[test]
fn environment_is_plain_text_never_a_credential() {
    for (name, value) in [
        ("TERM", "xterm"),
        ("A", ""),
        ("_X1", "a=b c"),
        ("GIT_CONFIG_KEY_0", "core.hooksPath"),
        ("LANG", "C.UTF-8"),
    ] {
        let var = EnvVar::new(name, value).unwrap();
        assert_eq!((var.name(), var.value()), (name, value));
    }
    for name in [
        "",
        "1A",
        "A-B",
        "A B",
        "A=B",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "GITHUB_TOKEN",
        "claude_code_oauth_token",
        "DB_PASSWORD",
        "SECRET",
        "AWS_SECRET_ACCESS_KEY",
        "MY_PRIVATE_KEY",
        "STRIPE_KEY",
        "APIKEY",
        "CREDENTIALS",
    ] {
        assert!(
            matches!(EnvVar::new(name, "v"), Err(SpecError::InvalidEnv { .. })),
            "{name}"
        );
    }
    assert!(EnvVar::new("A", "x\0y").is_err());
}
