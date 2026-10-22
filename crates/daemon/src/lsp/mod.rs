//! Language servers in the daemon (protocol 0.39, `docs/lsp.md`).
//!
//! - [`catalog`]: the servers Hutan knows (built in, merged with `[lsp.servers.<id>]`) and the
//!   language of a file name. Servers are found on the login `PATH`, never fetched.
//! - [`roots`]: the topmost root marker above a file, within its project, with a per-project
//!   override.
//! - [`trust`]: what the user decided about folders; a server never starts in an untrusted one.
//! - [`rustup`]: a `rust-analyzer` that is rustup's proxy without the component counts as not
//!   installed (`rustup component add rust-analyzer`), never a crash loop.
//! - [`convert`]: positions and diagnostics between a server's encoding and the wire.
//! - [`hub`]: [`LspHub`], one server per (server, root) shared by every client, the mirrors of
//!   the open documents, idle stop, `max_servers`, restart and the requests of the protocol.

mod catalog;
mod convert;
mod hub;
mod roots;
mod rustup;
mod trust;

pub use hub::{Inputs, LspHub, LspTuning};

#[cfg(test)]
mod tests {
    /// The queues of the hub, of the client library it drives and of the daemon's side are
    /// bounded: no unbounded channel anywhere in them (`AGENTS.md`: bounded everything).
    #[test]
    fn no_channel_of_the_language_servers_is_unbounded() {
        let needle = ["unbounded", "_channel"].concat();
        let needle_type = ["Unbounded", "Sender"].concat();
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut checked = 0;
        for dir in ["src/lsp", "../lsp_client/src"] {
            for entry in std::fs::read_dir(manifest.join(dir)).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let source = std::fs::read_to_string(&path).unwrap();
                // The test itself spells the needles out of pieces.
                assert!(!source.contains(&needle), "{}", path.display());
                assert!(!source.contains(&needle_type), "{}", path.display());
                checked += 1;
            }
        }
        assert!(checked >= 15, "{checked}");
    }
}
