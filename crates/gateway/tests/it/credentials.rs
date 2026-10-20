//! The credential stores. The memory store runs in the gate; the Keychain test is `#[ignore]`d
//! and only ever touches a temporary keychain file it creates (never the login keychain):
//!
//! ```sh
//! cargo test -p hutan-gateway --test it credentials:: -- --ignored
//! ```
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hutan_gateway::{CredentialStore, KEYCHAIN_SERVICE, MemoryStore, Secret, keychain_ref};

#[test]
fn the_memory_store_sets_replaces_and_deletes() {
    let store = MemoryStore::new();
    assert_eq!(store.get("openai").unwrap(), None);
    store.set("openai", &Secret::new("one")).unwrap();
    store.set("anthropic", &Secret::new("two")).unwrap();
    assert_eq!(store.get("openai").unwrap().unwrap().expose(), "one");
    store.set("openai", &Secret::new("three")).unwrap();
    assert_eq!(store.get("openai").unwrap().unwrap().expose(), "three");
    store.delete("openai").unwrap();
    store.delete("openai").unwrap();
    assert_eq!(store.get("openai").unwrap(), None);
    assert_eq!(store.get("anthropic").unwrap().unwrap().expose(), "two");
}

#[test]
fn secrets_and_stores_never_print_their_content() {
    let store = MemoryStore::new();
    store.set("p", &Secret::new("very-secret-value")).unwrap();
    let secret = store.get("p").unwrap().unwrap();
    assert!(!format!("{secret:?} {store:?}").contains("very-secret-value"));
    assert_eq!(keychain_ref("openai"), format!("{KEYCHAIN_SERVICE}/openai"));
}

#[cfg(target_os = "macos")]
mod keychain {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use hutan_gateway::{CredentialStore, KEYCHAIN_SERVICE, KeychainStore, Secret};

    const PASSWORD: &str = "hutan-test-keychain-password";

    fn security(args: &[&str]) -> std::process::Output {
        Command::new("security")
            .args(args)
            .output()
            .expect("running /usr/bin/security")
    }

    fn search_list() -> String {
        String::from_utf8_lossy(&security(&["list-keychains", "-d", "user"]).stdout).into_owned()
    }

    /// Deletes the keychain file (and takes it out of the search list again) however the test
    /// ends.
    struct TempKeychain {
        dir: tempfile::TempDir,
        path: PathBuf,
    }

    impl TempKeychain {
        fn create() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("hutan-test.keychain-db");
            let created = security(&["create-keychain", "-p", PASSWORD, path.to_str().unwrap()]);
            assert!(
                created.status.success(),
                "create-keychain: {}",
                String::from_utf8_lossy(&created.stderr)
            );
            let keychain = Self { dir, path };
            let unlocked = security(&[
                "unlock-keychain",
                "-p",
                PASSWORD,
                keychain.path.to_str().unwrap(),
            ]);
            assert!(unlocked.status.success());
            keychain
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempKeychain {
        fn drop(&mut self) {
            let _ = security(&["delete-keychain", self.path.to_str().unwrap_or_default()]);
            let _ = std::fs::remove_dir_all(self.dir.path());
        }
    }

    #[test]
    #[ignore = "creates and deletes a temporary keychain via /usr/bin/security; run manually"]
    fn the_keychain_store_round_trips_in_a_temporary_keychain() {
        let before = search_list();
        let account = format!("hutan-test-{}", std::process::id());
        {
            let keychain = TempKeychain::create();
            let store = KeychainStore::open_file(keychain.path(), PASSWORD).unwrap();

            assert_eq!(store.get(&account).unwrap(), None);
            store.set(&account, &Secret::new("first-secret")).unwrap();
            assert_eq!(
                store.get(&account).unwrap().unwrap().expose(),
                "first-secret"
            );
            store.set(&account, &Secret::new("second-secret")).unwrap();
            assert_eq!(
                store.get(&account).unwrap().unwrap().expose(),
                "second-secret"
            );

            // The item is in the temporary keychain only: the default search list (which holds
            // the login keychain) does not know it.
            let found = security(&[
                "find-generic-password",
                "-s",
                KEYCHAIN_SERVICE,
                "-a",
                &account,
            ]);
            assert!(
                !found.status.success(),
                "the credential leaked into a keychain on the search list"
            );

            store.delete(&account).unwrap();
            store.delete(&account).unwrap();
            assert_eq!(store.get(&account).unwrap(), None);
        }
        assert_eq!(
            search_list(),
            before,
            "the keychain search list was restored"
        );
    }
}
