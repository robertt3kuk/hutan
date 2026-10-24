//! Turns a raw recording into a committable fixture: no account data, no machine names, no real
//! paths beyond a fixed temp directory, ids replaced by stable counters.
//!
//! Scrubbing is idempotent (scrubbing a fixture changes nothing), and a recording that still
//! looks like it contains something private after scrubbing is refused instead of written.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value, json};

/// Notifications that only carry account state; dropped from fixtures.
const DROPPED_METHODS: &[&str] = &[];
/// The rate-limit notification: kept for what the usage records read (the windows' percentages,
/// lengths and reset times, `docs/agents.md` § Usage) and nothing else of it (credits, names).
const RATE_LIMITS_METHOD: &str = "account/rateLimits/updated";
/// Keys removed wherever they appear: account-specific catalog entries.
const DROPPED_KEYS: &[&str] = &["availableAccessPrograms"];

const FIXTURE_CWD: &str = "/tmp/hutan-fixture";
const FIXTURE_CODEX_HOME: &str = "/tmp/codex-home";
const FIXTURE_HOST: &str = "host.local";
const FIXTURE_ROLLOUT: &str = "/tmp/codex-home/sessions/rollout-fixture.jsonl";
const ZERO_UUID: &str = "00000000-0000-4000-8000-000000000000";
const FIXTURE_UUID_PREFIX: &str = "01a00000-0000-7000-8000-";

#[derive(Debug, thiserror::Error)]
pub enum ScrubError {
    #[error("line {line} is not JSON: {source}")]
    Json {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("line {line} still looks private after scrubbing ({what}): {sample}")]
    Leak {
        line: usize,
        what: &'static str,
        sample: String,
    },
}

fn re(pattern: &str) -> Regex {
    // The patterns are literals of this file and tested.
    #[allow(clippy::expect_used)]
    Regex::new(pattern).expect("a valid regex")
}

static UUID: LazyLock<Regex> = LazyLock::new(|| {
    re(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
});
static FIXTURE_UUID: LazyLock<Regex> = LazyLock::new(|| re(r"^01a00000-0000-7000-8000-(\d{12})$"));
static PROVIDER_ID: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b(msg|rs|fc|call|resp)_[0-9a-fA-F]{16,}"));
static FIXTURE_PROVIDER_ID: LazyLock<Regex> = LazyLock::new(|| re(r"^[a-z]+_fixture_(\d+)$"));
static TEMP_DIR: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?:/private)?/var/folders/[^/\s]+/[^/\s]+/T/[A-Za-z0-9._-]+|/tmp/hutan-rec-[A-Za-z0-9_-]+")
});
static ROLLOUT: LazyLock<Regex> = LazyLock::new(|| re(r"^/.*/sessions/.*rollout-.*\.jsonl$"));
static USER_AGENT: LazyLock<Regex> =
    LazyLock::new(|| re(r"^([A-Za-z0-9_.-]+)/(\d+\.\d+\.\d+[^ ]*) \([^)]*\)(?: .*)?$"));
static EMAIL: LazyLock<Regex> =
    LazyLock::new(|| re(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}"));

/// Strings that mean the recording still holds something private.
static LEAKS: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    vec![
        ("a home directory", re(r"/Users/|/home/[a-z]")),
        (
            "an email address",
            re(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}"),
        ),
        ("an API key", re(r"\bsk-[A-Za-z0-9_-]{10,}")),
        (
            "a bearer or JWT token",
            re(r"\beyJ[A-Za-z0-9_-]{10,}|[Bb]earer [A-Za-z0-9._-]{16,}"),
        ),
        ("a macOS temp directory", re(r"/var/folders/")),
    ]
});

#[derive(Debug, Default)]
struct Renamer {
    uuids: HashMap<String, String>,
    providers: HashMap<String, String>,
    next_uuid: u64,
    next_provider: u64,
}

impl Renamer {
    /// Starts after the highest counter an already scrubbed input uses.
    fn seeded(lines: &[Value]) -> Self {
        let mut renamer = Self::default();
        let mut text = String::new();
        for line in lines {
            text.push_str(&line.to_string());
            text.push('\n');
        }
        for found in UUID.find_iter(&text) {
            if let Some(n) = FIXTURE_UUID
                .captures(found.as_str())
                .and_then(|c| c[1].parse::<u64>().ok())
            {
                renamer.next_uuid = renamer.next_uuid.max(n);
                renamer
                    .uuids
                    .insert(found.as_str().to_owned(), found.as_str().to_owned());
            }
        }
        for found in text.split(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
            if let Some(n) = FIXTURE_PROVIDER_ID
                .captures(found)
                .and_then(|c| c[1].parse::<u64>().ok())
            {
                renamer.next_provider = renamer.next_provider.max(n);
                renamer.providers.insert(found.to_owned(), found.to_owned());
            }
        }
        renamer
    }

    fn uuid(&mut self, original: &str) -> String {
        let key = original.to_ascii_lowercase();
        if let Some(known) = self.uuids.get(&key) {
            return known.clone();
        }
        self.next_uuid += 1;
        let renamed = format!("{FIXTURE_UUID_PREFIX}{:012}", self.next_uuid);
        self.uuids.insert(key, renamed.clone());
        renamed
    }

    fn provider(&mut self, original: &str) -> String {
        if let Some(known) = self.providers.get(original) {
            return known.clone();
        }
        self.next_provider += 1;
        let prefix = original.split('_').next().unwrap_or("id");
        let renamed = format!("{prefix}_fixture_{}", self.next_provider);
        self.providers.insert(original.to_owned(), renamed.clone());
        renamed
    }

    fn string(&mut self, text: &str) -> String {
        if ROLLOUT.is_match(text) {
            return FIXTURE_ROLLOUT.to_owned();
        }
        if let Some(caps) = USER_AGENT.captures(text) {
            return format!("{}/{} (Mac OS; arm64)", &caps[1], &caps[2]);
        }
        let text = TEMP_DIR.replace_all(text, FIXTURE_CWD);
        let text = EMAIL.replace_all(&text, "user@example.com");
        // `replace_all` with a closure cannot borrow `self` twice, so collect first.
        let uuids: Vec<String> = UUID
            .find_iter(&text)
            .map(|m| m.as_str().to_owned())
            .collect();
        let mut text = text.into_owned();
        for found in uuids {
            let renamed = self.uuid(&found);
            text = text.replace(&found, &renamed);
        }
        let providers: Vec<String> = PROVIDER_ID
            .find_iter(&text)
            .map(|m| m.as_str().to_owned())
            .collect();
        for found in providers {
            let renamed = self.provider(&found);
            text = text.replace(&found, &renamed);
        }
        text
    }

    fn value(&mut self, value: Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.string(&text)),
            Value::Array(items) => Value::Array(items.into_iter().map(|v| self.value(v)).collect()),
            Value::Object(map) => {
                let mut out = Map::new();
                // `serverName` is the machine's name in the remote-control status (which also
                // has the installation id); in an MCP elicitation it names an MCP server.
                let host_status = map.contains_key("installationId");
                for (key, inner) in map {
                    if DROPPED_KEYS.contains(&key.as_str()) {
                        continue;
                    }
                    let inner = match key.as_str() {
                        "serverName" if host_status && inner.is_string() => json!(FIXTURE_HOST),
                        "installationId" if inner.is_string() => json!(ZERO_UUID),
                        "codexHome" if inner.is_string() => json!(FIXTURE_CODEX_HOME),
                        "planType" => Value::Null,
                        _ => self.value(inner),
                    };
                    out.insert(key, inner);
                }
                Value::Object(out)
            }
            other => other,
        }
    }
}

/// Scrubs a raw transcript (one `{"dir", "msg"}` object per line; `t` timestamps are dropped).
pub fn scrub(raw: &str) -> Result<String, ScrubError> {
    let mut lines = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line).map_err(|source| ScrubError::Json {
            line: index + 1,
            source,
        })?;
        lines.push(value);
    }
    let mut renamer = Renamer::seeded(&lines);
    let mut out = String::new();
    for (index, mut line) in lines.into_iter().enumerate() {
        if let Some(object) = line.as_object_mut() {
            object.remove("t");
            let method = object
                .get("msg")
                .and_then(|m| m.get("method"))
                .and_then(Value::as_str);
            if method.is_some_and(|m| DROPPED_METHODS.contains(&m)) {
                continue;
            }
            if method == Some(RATE_LIMITS_METHOD) && !keep_rate_windows(object) {
                continue;
            }
        }
        // The installation id is a stable machine identifier even as a UUID.
        let scrubbed = renamer.value(line);
        check_leaks(&scrubbed, index + 1)?;
        out.push_str(&scrubbed.to_string());
        out.push('\n');
    }
    Ok(out)
}

/// Reduces a rate-limit notification to its windows; false when it has none.
fn keep_rate_windows(line: &mut Map<String, Value>) -> bool {
    let Some(params) = line.get_mut("msg").and_then(|m| m.get_mut("params")) else {
        return false;
    };
    let mut kept = Map::new();
    kept.insert("limitId".to_owned(), json!("codex"));
    if let Some(limits) = params.get("rateLimits").and_then(Value::as_object) {
        for key in ["primary", "secondary"] {
            let Some(window) = limits.get(key).and_then(Value::as_object) else {
                continue;
            };
            let mut keep = Map::new();
            for field in ["usedPercent", "windowDurationMins", "resetsAt"] {
                if let Some(value) = window.get(field).filter(|v| v.is_number()) {
                    keep.insert(field.to_owned(), value.clone());
                }
            }
            if keep.contains_key("usedPercent") {
                kept.insert(key.to_owned(), Value::Object(keep));
            }
        }
    }
    if kept.len() == 1 {
        return false;
    }
    *params = json!({ "rateLimits": kept });
    true
}

fn check_leaks(value: &Value, line: usize) -> Result<(), ScrubError> {
    match value {
        Value::String(text) => {
            for (what, pattern) in LEAKS.iter() {
                // The one address the scrubber writes itself.
                let text = text.replace("user@example.com", "");
                if let Some(found) = pattern.find(&text) {
                    return Err(ScrubError::Leak {
                        line,
                        what,
                        sample: found.as_str().to_owned(),
                    });
                }
            }
            Ok(())
        }
        Value::Array(items) => items.iter().try_for_each(|v| check_leaks(v, line)),
        Value::Object(map) => map.values().try_for_each(|v| check_leaks(v, line)),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &str = r#"
{"dir":"in","t":1.5,"msg":{"id":1,"result":{"userAgent":"hutan/0.162.1 (Mac OS 26.6.2; arm64) Orca/1.4 (hutan; 0.1.0)","codexHome":"/private/tmp/m4a/codex-home","platformOs":"macos"}}}
{"dir":"in","t":1.6,"msg":{"method":"remoteControl/status/changed","params":{"serverName":"workies-MacBook-Air.local","installationId":"5bdf5e69-6440-473e-9530-369290aacf28"}}}
{"dir":"in","t":1.7,"msg":{"method":"account/rateLimits/updated","params":{"rateLimits":{}}}}
{"dir":"in","t":1.75,"msg":{"method":"account/rateLimits/updated","params":{"rateLimits":{"limitId":"codex","limitName":"Codex of me@example.org","primary":{"usedPercent":7,"windowDurationMins":300,"resetsAt":1792024800},"credits":{"hasCredits":true,"unlimited":false,"balance":"12.50"},"planType":"plus"}}}}
{"dir":"in","t":1.8,"msg":{"method":"account/updated","params":{"authMode":"chatgpt","planType":"plus"}}}
{"dir":"in","t":1.9,"msg":{"method":"item/started","params":{"threadId":"01a13bfd-6208-7f30-ab9c-2a84706d72d8","item":{"id":"msg_0f75b2f5d2998cd5016acfde949f9c8191","cwd":"/var/folders/f8/abc/T/hutan-rec-s2gy8xlb","text":"mail me@example.org"},"path":"/Users/x/.codex/sessions/2026/10/15/rollout-2026-10-15T00-57-03-01a13bfd-6208-7f30-ab9c-2a84706d72d8.jsonl"}}}
"#;

    #[test]
    fn scrubbing_removes_account_machine_and_path_details() {
        let out = scrub(RAW).unwrap();
        // The rate limits keep their windows and nothing else; one without windows is dropped.
        assert_eq!(out.matches("\"rateLimits\":{").count(), 1, "{out}");
        assert!(out.contains(r#""usedPercent":7"#), "{out}");
        assert!(!out.contains("12.50") && !out.contains("Codex of me"), "{out}");
        assert!(!out.contains("workies"), "{out}");
        assert!(!out.contains("5bdf5e69"), "{out}");
        assert!(!out.contains("plus"), "{out}");
        assert!(!out.contains("/Users/"), "{out}");
        assert!(!out.contains("/var/folders"), "{out}");
        assert!(!out.contains("me@example.org"), "{out}");
        assert!(!out.contains("\"t\""), "timestamps are dropped: {out}");
        assert!(out.contains("hutan/0.162.1 (Mac OS; arm64)"), "{out}");
        assert!(out.contains(FIXTURE_CWD), "{out}");
        assert!(out.contains("msg_fixture_1"), "{out}");
        assert!(
            out.contains("01a00000-0000-7000-8000-000000000001"),
            "{out}"
        );
    }

    #[test]
    fn scrubbing_twice_changes_nothing() {
        let once = scrub(RAW).unwrap();
        assert_eq!(scrub(&once).unwrap(), once);
    }

    #[test]
    fn something_private_that_survives_is_refused() {
        let raw =
            r#"{"dir":"in","msg":{"method":"x","params":{"note":"token sk-abcdefghijklmnop"}}}"#;
        let error = scrub(raw).unwrap_err();
        assert!(
            matches!(
                error,
                ScrubError::Leak {
                    what: "an API key",
                    ..
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn a_line_that_is_not_json_is_an_error_with_its_number() {
        let error = scrub("{}\nnot json").unwrap_err();
        assert!(matches!(error, ScrubError::Json { line: 2, .. }));
    }
}
