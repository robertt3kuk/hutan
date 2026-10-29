//! What GitHub's releases list says, and which release is the update (ADR 0024 decision 2).
//!
//! Pure functions over the JSON of `GET /repos/robertt3kuk/hutan/releases`: parsing is
//! forgiving (an entry that does not look like a release is skipped, never an error), selection
//! is strict (a semver above the running one, in the channel, with the app's archive attached).

use hutan_protocol::update::{
    ASSETS_MAX, NOTES_MAX_BYTES, UpdateAsset, UpdateChannel, UpdateRelease,
};
use semver::Version;
use serde::Deserialize;
use serde_json::Value;

/// The repository whose releases are checked.
pub const REPOSITORY: &str = "robertt3kuk/hutan";

/// One release as GitHub lists it, reduced to what the check reads.
#[derive(Debug, Deserialize)]
struct RawRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    published_at: Option<String>,
    html_url: Option<String>,
    body: Option<String>,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

#[derive(Debug, Deserialize)]
struct RawAsset {
    name: String,
    #[serde(default)]
    size: u64,
    /// `sha256:<hex>`, as GitHub computes it for an uploaded asset.
    digest: Option<String>,
    /// `uploaded` once the upload finished; absent in older answers.
    state: Option<String>,
}

/// The release list, with every entry that does not parse left out. `Err` when the answer is
/// not a JSON array at all (an error object, an HTML page).
pub fn parse_list(body: &[u8]) -> Result<Vec<Release>, String> {
    let value: Value =
        serde_json::from_slice(body).map_err(|error| format!("the answer is not JSON: {error}"))?;
    let Value::Array(entries) = value else {
        return Err("the answer is not a list of releases".to_owned());
    };
    Ok(entries
        .into_iter()
        .filter_map(|entry| serde_json::from_value::<RawRelease>(entry).ok())
        .filter_map(Release::from_raw)
        .collect())
}

/// A release with a version Hutan can order.
#[derive(Debug)]
pub struct Release {
    version: Version,
    raw: RawRelease,
}

impl Release {
    fn from_raw(raw: RawRelease) -> Option<Self> {
        let version = parse_version(&raw.tag_name)?;
        Some(Self { version, raw })
    }

    /// Marked pre-release on GitHub, or a semver pre-release (`0.4.0-rc.1`).
    fn is_prerelease(&self) -> bool {
        self.raw.prerelease || !self.version.pre.is_empty()
    }

    /// The archive of the app this release must carry to be an update, uploaded.
    fn has_app_archive(&self) -> bool {
        let wanted = app_archive_name(&self.version);
        self.raw
            .assets
            .iter()
            .any(|a| a.name == wanted && a.state.as_deref().is_none_or(|s| s == "uploaded"))
    }

    fn into_update(self) -> UpdateRelease {
        let version = self.version.to_string();
        let notes_url = self
            .raw
            .html_url
            .filter(|url| url.starts_with("https://"))
            .unwrap_or_else(|| {
                format!(
                    "https://github.com/{REPOSITORY}/releases/tag/{}",
                    self.raw.tag_name
                )
            });
        let (notes, notes_truncated) = bounded(self.raw.body.unwrap_or_default(), NOTES_MAX_BYTES);
        UpdateRelease {
            prerelease: self.raw.prerelease || !self.version.pre.is_empty(),
            version,
            tag: self.raw.tag_name,
            published: self.raw.published_at,
            notes_url,
            notes,
            notes_truncated,
            assets: self
                .raw
                .assets
                .into_iter()
                .take(ASSETS_MAX)
                .map(|a| UpdateAsset {
                    sha256: a.digest.as_deref().and_then(sha256_of),
                    name: a.name,
                    size: a.size,
                })
                .collect(),
        }
    }
}

/// `Hutan-<version>-arm64.zip`, the file the cask and the in-place update install.
pub fn app_archive_name(version: &Version) -> String {
    format!("Hutan-{version}-arm64.zip")
}

/// `v0.3.2` and `0.3.2` are both 0.3.2; anything else that is not semver is no release of ours.
pub fn parse_version(tag: &str) -> Option<Version> {
    Version::parse(tag.trim().trim_start_matches('v')).ok()
}

/// The newest release of `channel` above `running` that carries the app's archive.
pub fn newest(
    releases: Vec<Release>,
    running: &Version,
    channel: UpdateChannel,
) -> Option<UpdateRelease> {
    releases
        .into_iter()
        .filter(|release| !release.raw.draft)
        .filter(|release| channel == UpdateChannel::Prerelease || !release.is_prerelease())
        .filter(|release| release.version > *running)
        .filter(Release::has_app_archive)
        .max_by(|a, b| a.version.cmp(&b.version))
        .map(Release::into_update)
}

/// `text` cut to at most `max` bytes at a character boundary, and whether it was cut.
pub fn bounded(mut text: String, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    (text, true)
}

/// The hex of a `sha256:<hex>` digest, lowercase; `None` for anything else.
fn sha256_of(digest: &str) -> Option<String> {
    let hex = digest.strip_prefix("sha256:")?;
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| hex.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const DIGEST: &str = "sha256:9F86D081884C7D659A2FEAA0C55AD015A3BF4F1B2B0B822CD15D6C15B0F00A08";

    fn release(tag: &str, prerelease: bool, archive: bool) -> Value {
        let version = tag.trim_start_matches('v');
        let mut assets = vec![json!({"name": format!("Hutan-{version}-source.tar.gz"), "size": 5})];
        if archive {
            assets.push(json!({
                "name": format!("Hutan-{version}-arm64.zip"),
                "size": 1234,
                "digest": DIGEST,
                "state": "uploaded",
            }));
        }
        json!({
            "tag_name": tag,
            "draft": false,
            "prerelease": prerelease,
            "published_at": "2026-10-29T10:00:00Z",
            "html_url": format!("https://github.com/robertt3kuk/hutan/releases/tag/{tag}"),
            "body": format!("notes of {tag}"),
            "assets": assets,
        })
    }

    fn pick(list: &[Value], running: &str, channel: UpdateChannel) -> Option<UpdateRelease> {
        let body = serde_json::to_vec(list).unwrap();
        newest(
            parse_list(&body).unwrap(),
            &Version::parse(running).unwrap(),
            channel,
        )
    }

    #[test]
    fn the_highest_version_above_the_running_one_wins() {
        let list = [
            release("v0.3.2", true, true),
            release("v0.3.10", true, true),
            release("v0.3.1", true, true),
            release("v0.4.0", true, false),
        ];
        let found = pick(&list, "0.3.1", UpdateChannel::Prerelease).unwrap();
        assert_eq!(found.version, "0.3.10", "semver order, not text order");
        assert_eq!(found.tag, "v0.3.10");
        assert!(found.prerelease);
        assert_eq!(found.notes, "notes of v0.3.10");
        assert_eq!(
            found.notes_url,
            "https://github.com/robertt3kuk/hutan/releases/tag/v0.3.10"
        );
        let archive = found
            .assets
            .iter()
            .find(|a| a.name == "Hutan-0.3.10-arm64.zip")
            .unwrap();
        assert_eq!(
            archive.sha256.as_deref(),
            Some(&DIGEST["sha256:".len()..].to_ascii_lowercase()[..])
        );
        assert_eq!(archive.size, 1234);
    }

    #[test]
    fn older_equal_and_archiveless_releases_are_ignored() {
        let list = [
            release("v0.3.0", true, true),
            release("v0.3.1", true, true),
            release("v0.3.2", true, false),
        ];
        assert_eq!(pick(&list, "0.3.1", UpdateChannel::Prerelease), None);
    }

    #[test]
    fn the_stable_channel_ignores_prereleases() {
        let list = [
            release("v0.3.2", true, true),
            release("v0.3.3-rc.1", false, true),
        ];
        assert_eq!(
            pick(&list, "0.3.1", UpdateChannel::Stable),
            None,
            "rc is a pre-release"
        );
        let list = [
            release("v0.3.2", true, true),
            release("v0.3.4", false, true),
        ];
        assert_eq!(
            pick(&list, "0.3.1", UpdateChannel::Stable).unwrap().version,
            "0.3.4"
        );
        assert_eq!(
            pick(&list, "0.3.1", UpdateChannel::Prerelease)
                .unwrap()
                .version,
            "0.3.4"
        );
    }

    #[test]
    fn a_draft_and_an_upload_in_progress_do_not_count() {
        let mut draft = release("v0.3.2", true, true);
        draft["draft"] = json!(true);
        assert_eq!(pick(&[draft], "0.3.1", UpdateChannel::Prerelease), None);
        let mut starting = release("v0.3.2", true, true);
        starting["assets"][1]["state"] = json!("pending");
        assert_eq!(pick(&[starting], "0.3.1", UpdateChannel::Prerelease), None);
    }

    #[test]
    fn entries_that_are_not_releases_are_skipped_and_a_non_list_is_an_error() {
        let list = [
            json!({"message": "odd"}),
            json!(7),
            release("v0.3.2", true, true),
        ];
        assert_eq!(
            pick(&list, "0.3.1", UpdateChannel::Prerelease)
                .unwrap()
                .version,
            "0.3.2"
        );
        let tagless = [json!({"tag_name": "nightly", "assets": []})];
        assert_eq!(pick(&tagless, "0.3.1", UpdateChannel::Prerelease), None);
        assert!(parse_list(br#"{"message":"Not Found"}"#).is_err());
        assert!(parse_list(b"<html>").is_err());
    }

    #[test]
    fn a_digest_is_kept_only_when_it_is_a_sha256() {
        assert_eq!(sha256_of("sha256:abcd"), None);
        assert_eq!(sha256_of(&format!("sha1:{}", "a".repeat(40))), None);
        assert_eq!(
            sha256_of(DIGEST).unwrap(),
            DIGEST["sha256:".len()..].to_ascii_lowercase()
        );
    }

    #[test]
    fn notes_are_cut_at_a_character_boundary() {
        let (text, cut) = bounded("é".repeat(10), 5);
        assert!(cut);
        assert_eq!(text, "éé");
        let (text, cut) = bounded("short".to_owned(), 5);
        assert!(!cut);
        assert_eq!(text, "short");
        let mut big = release("v0.3.2", true, true);
        big["body"] = json!("x".repeat(NOTES_MAX_BYTES + 100));
        let found = pick(&[big], "0.3.1", UpdateChannel::Prerelease).unwrap();
        assert_eq!(found.notes.len(), NOTES_MAX_BYTES);
        assert!(found.notes_truncated);
    }

    #[test]
    fn a_page_that_is_not_https_is_replaced_by_the_tag_page() {
        let mut odd = release("v0.3.2", true, true);
        odd["html_url"] = json!("javascript:alert(1)");
        let found = pick(&[odd], "0.3.1", UpdateChannel::Prerelease).unwrap();
        assert_eq!(
            found.notes_url,
            "https://github.com/robertt3kuk/hutan/releases/tag/v0.3.2"
        );
    }
}
