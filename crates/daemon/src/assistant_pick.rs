//! Choosing the assistant's model from what a backend offers (AM-1, `docs/assistant.md`
//! § Backends).
//!
//! The assistant is a read-only helper that answers questions, so it must not silently run on the
//! most expensive tier a harness defaults to (OMP's default is often Claude Opus). When no model
//! was stored or configured, [`pick_assistant_model`] takes the catalog's default and, when that
//! default is a *premium* tier, the newest model of the same vendor one tier down. The rules are a
//! fixed table in this file: nothing is learned, fetched or priced at run time.
//!
//! Everything here is pure; the daemon reads the catalog (`ChatService::models`,
//! `provider.models`) and passes it in.

use std::cmp::Ordering;
use std::time::Duration;

use hutan_core::ModelInfo;

/// How long the model catalog of a backend may take when the assistant picks its model: a
/// slow catalog means no pick (the backend's own default runs), never a failed start.
pub(crate) const CATALOG_TIMEOUT: Duration = Duration::from_secs(20);

/// The model the assistant should run on, from the catalog of its backend: the catalog default
/// (else the first model that is not hidden), moved one tier down when it is a premium tier and
/// the catalog has the next tier of the same vendor. `None` for an empty catalog: the harness's
/// own default then runs.
pub fn pick_assistant_model(models: &[ModelInfo]) -> Option<String> {
    let visible: Vec<&ModelInfo> = models.iter().filter(|m| !m.hidden).collect();
    let default = visible
        .iter()
        .copied()
        .find(|m| m.is_default)
        .or_else(|| visible.first().copied())?;
    let chosen = lower_tier(default, &visible).unwrap_or(default);
    Some(chosen.id.clone())
}

/// The newest model one tier below `default`, when `default` is premium and the catalog has one.
fn lower_tier<'a>(default: &ModelInfo, visible: &[&'a ModelInfo]) -> Option<&'a ModelInfo> {
    let others = || {
        visible
            .iter()
            .copied()
            .filter(|m| m.id != default.id && prefix(&m.id) == prefix(&default.id))
    };
    let candidates: Vec<&ModelInfo> = match premium(default)? {
        // Anthropic: Opus costs several times Sonnet and the assistant does not need it.
        Premium::AnthropicOpus => others()
            .filter(|m| anthropic_family(m) == Some("sonnet"))
            .collect(),
        // OpenAI: the `-pro` variants (`o3-pro`, `gpt-5-pro`) are priced far above the base
        // model of the same family; the base model answers questions as well.
        Premium::OpenAiPro => {
            let family = family_key(&default.id);
            others()
                .filter(|m| !words(local(&m.id)).contains(&"pro".to_owned()))
                .filter(|m| family_key(&m.id) == family)
                .collect()
        }
        // Google: Ultra and Deep Think sit above Pro.
        Premium::GoogleUltra => others()
            .filter(|m| is_gemini(m) && !is_google_premium(m))
            .filter(|m| words(local(&m.id)).contains(&"pro".to_owned()))
            .collect(),
    };
    newest(candidates)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Premium {
    AnthropicOpus,
    OpenAiPro,
    GoogleUltra,
}

fn premium(model: &ModelInfo) -> Option<Premium> {
    if anthropic_family(model) == Some("opus") {
        return Some(Premium::AnthropicOpus);
    }
    if is_gemini(model) {
        return is_google_premium(model).then_some(Premium::GoogleUltra);
    }
    // `gemini-…-pro` is Google's ordinary tier, so the OpenAI rule runs only for OpenAI ids.
    let id = words(local(&model.id));
    (is_openai(&id) && id.iter().any(|w| w == "pro")).then_some(Premium::OpenAiPro)
}

/// `opus`, `sonnet` or `haiku` when the model is one of them. The id and the display name decide;
/// the description only when they name no family (Claude Code's ACP adapter exposes `default`
/// whose description says which model it is, while its `opus` description may mention Sonnet).
fn anthropic_family(model: &ModelInfo) -> Option<&'static str> {
    let named = format!("{} {}", model.id, model.display_name).to_lowercase();
    find_family(&named).or_else(|| find_family(&model.description.to_lowercase()))
}

fn find_family(text: &str) -> Option<&'static str> {
    ["opus", "sonnet", "haiku"]
        .into_iter()
        .filter_map(|family| text.find(family).map(|at| (at, family)))
        .min()
        .map(|(_, family)| family)
}

fn is_gemini(model: &ModelInfo) -> bool {
    format!("{} {}", model.id, model.display_name)
        .to_lowercase()
        .contains("gemini")
}

fn is_google_premium(model: &ModelInfo) -> bool {
    let text = words(&format!("{} {}", model.id, model.display_name));
    text.iter().any(|w| w == "ultra" || w == "deepthink")
        || text
            .windows(2)
            .any(|pair| pair[0] == "deep" && pair[1] == "think")
}

/// `gpt-…`, `o1`/`o3`/`o4-…`, `codex`: OpenAI's naming.
fn is_openai(words: &[String]) -> bool {
    words.iter().any(|w| {
        w.starts_with("gpt")
            || w == "codex"
            || (w.len() >= 2 && w.starts_with('o') && w[1..].bytes().all(|b| b.is_ascii_digit()))
    })
}

/// The provider part of an OMP-style `provider/model` id: replacements never cross it.
fn prefix(id: &str) -> Option<&str> {
    id.split_once('/').map(|(prefix, _)| prefix)
}

/// The id without its provider prefix.
fn local(id: &str) -> &str {
    id.split_once('/').map_or(id, |(_, rest)| rest)
}

/// Lowercase alphanumeric words of `text`, without bracketed variants (`sonnet[1m]`).
fn words(text: &str) -> Vec<String> {
    let mut plain = String::with_capacity(text.len());
    let mut depth = 0u32;
    for c in text.chars() {
        match c {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => plain.push(c),
            _ => {}
        }
    }
    plain
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// A calendar date inside an id: `20250929`, `250929`, or `2025-09-29` as three words.
fn date_at(words: &[String]) -> Option<(u64, usize)> {
    let first = words.first()?;
    let digits = |w: &str, len: usize| w.len() == len && w.bytes().all(|b| b.is_ascii_digit());
    if digits(first, 8) || digits(first, 6) {
        return Some((first.parse().ok()?, 1));
    }
    if digits(first, 4) && (first.starts_with("19") || first.starts_with("20")) {
        let (month, day) = (words.get(1)?, words.get(2)?);
        if digits(month, 2) && digits(day, 2) {
            let value = first.parse::<u64>().ok()? * 10_000
                + month.parse::<u64>().ok()? * 100
                + day.parse::<u64>().ok()?;
            return Some((value, 3));
        }
    }
    None
}

/// What orders models of one family: the numbers of the version (`4-5` → `[4, 5]`) and then the
/// date. Dates and the context sizes of variants (`1m`, `200k`) are not part of the version.
fn version_key(id: &str) -> (Vec<u64>, u64) {
    let words = words(local(id));
    let mut version = Vec::new();
    let mut date = 0;
    let mut at = 0;
    while at < words.len() {
        if let Some((value, taken)) = date_at(&words[at..]) {
            date = date.max(value);
            at += taken;
            continue;
        }
        // `4o`, `1m`, `70b` mix letters in and are no version; `06` is a padded month.
        if let Some(number) = version_number(&words[at]) {
            version.push(number);
        }
        at += 1;
    }
    (version, date)
}

/// A word that is a version number: `5`, or `o3` (a one-letter family name with its number).
fn version_number(word: &str) -> Option<u64> {
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    if digits(word) {
        return if word.len() > 1 && word.starts_with('0') {
            None
        } else {
            word.parse().ok()
        };
    }
    let mut chars = word.chars();
    let letter = chars.next()?;
    let rest = chars.as_str();
    (letter.is_ascii_alphabetic() && digits(rest))
        .then(|| rest.parse().ok())
        .flatten()
}

/// The words of an id that name the family: without the provider prefix, `pro` and dates.
fn family_key(id: &str) -> Vec<String> {
    let words = words(local(id));
    let mut key = Vec::new();
    let mut at = 0;
    while at < words.len() {
        if let Some((_, taken)) = date_at(&words[at..]) {
            at += taken;
            continue;
        }
        if words[at] != "pro" {
            key.push(words[at].clone());
        }
        at += 1;
    }
    key
}

/// The newest of `models`: the highest version, then the latest date; the first in catalog order
/// on a tie.
fn newest(models: Vec<&ModelInfo>) -> Option<&ModelInfo> {
    models.into_iter().reduce(|best, next| {
        if version_key(&next.id).cmp(&version_key(&best.id)) == Ordering::Greater {
            next
        } else {
            best
        }
    })
}

/// Whether `base_url` (a validated `http(s)` URL) points at this machine: a provider there needs
/// no API key (Ollama, LM Studio, other local servers).
pub fn is_loopback_url(base_url: &str) -> bool {
    let rest = base_url
        .split_once("://")
        .map_or(base_url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = match authority.strip_prefix('[') {
        Some(bracketed) => bracketed.split(']').next().unwrap_or(bracketed),
        None => authority.split(':').next().unwrap_or(authority),
    };
    let host = host.to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str, name: &str, description: &str, is_default: bool) -> ModelInfo {
        ModelInfo {
            id: id.to_owned(),
            display_name: name.to_owned(),
            description: description.to_owned(),
            is_default,
            hidden: false,
            default_reasoning_effort: None,
            reasoning_efforts: Vec::new(),
        }
    }

    /// An OMP-style entry: the id is `provider/model`, the description the provider.
    fn omp(id: &str, is_default: bool) -> ModelInfo {
        let provider = id.split('/').next().unwrap_or_default();
        model(id, id, provider, is_default)
    }

    fn pick(models: &[ModelInfo]) -> Option<String> {
        pick_assistant_model(models)
    }

    #[test]
    fn an_omp_catalog_with_an_opus_default_moves_to_the_newest_sonnet_of_that_provider() {
        let catalog = [
            omp("anthropic/claude-haiku-4-5", false),
            omp("anthropic/claude-opus-4-1", true),
            omp("anthropic/claude-sonnet-4", false),
            omp("anthropic/claude-sonnet-4-5", false),
            omp("anthropic/claude-sonnet-4-1", false),
            // Newer, but another provider's: never chosen.
            omp("openrouter/anthropic/claude-sonnet-9", false),
            omp("amazon-bedrock/claude-sonnet-9", false),
            omp("openai/gpt-5", false),
        ];
        assert_eq!(
            pick(&catalog).as_deref(),
            Some("anthropic/claude-sonnet-4-5")
        );
    }

    #[test]
    fn the_gitlab_duo_catalog_of_omp_picks_its_newest_sonnet_and_stays_in_that_provider() {
        // The real option list of `omp acp` 18.11 (fixtures/acp-omp-18.11), current model Opus 4.6.
        let ids = [
            "gitlab-duo/claude-haiku-4-5-20251001",
            "gitlab-duo/claude-opus-4-5-20251101",
            "gitlab-duo/claude-sonnet-4-5-20250929",
            "gitlab-duo/duo-chat-gpt-5-1",
            "gitlab-duo/duo-chat-opus-4-5",
            "gitlab-duo/duo-chat-opus-4-6",
            "gitlab-duo/duo-chat-sonnet-4-5",
            "gitlab-duo/duo-chat-sonnet-4-6",
            "gitlab-duo-agent/claude_sonnet_4_7_vertex",
        ];
        let catalog: Vec<ModelInfo> = ids
            .iter()
            .map(|id| omp(id, *id == "gitlab-duo/duo-chat-opus-4-6"))
            .collect();
        assert_eq!(
            pick(&catalog).as_deref(),
            Some("gitlab-duo/duo-chat-sonnet-4-6")
        );
    }

    #[test]
    fn versions_and_dates_decide_what_is_newest() {
        let key = version_key;
        assert!(key("a/claude-sonnet-4-5") > key("a/claude-sonnet-4-1"));
        assert!(key("a/claude-sonnet-4-1") > key("a/claude-sonnet-4"));
        assert!(key("a/claude-sonnet-4-20250929") > key("a/claude-sonnet-4-20250514"));
        // A newer version beats an older one with a later date.
        assert!(key("a/claude-sonnet-4-5-20250101") > key("a/claude-sonnet-4-20250514"));
        // Dashed dates are dates, not versions.
        assert_eq!(key("openai/gpt-5-2025-08-07"), (vec![5], 20_250_807));
        // Context-size variants do not count as versions.
        assert_eq!(key("sonnet[1m]"), (vec![], 0));
        assert_eq!(key("claude-3-5-sonnet-20241022"), (vec![3, 5], 20_241_022));
        assert_eq!(key("openai/o3-pro"), (vec![3], 0));
        let catalog = [
            omp("anthropic/claude-opus-4-1", true),
            omp("anthropic/claude-sonnet-4-20250514", false),
            omp("anthropic/claude-sonnet-4-5-20250929", false),
            omp("anthropic/claude-sonnet-4-5-20250101", false),
        ];
        assert_eq!(
            pick(&catalog).as_deref(),
            Some("anthropic/claude-sonnet-4-5-20250929")
        );
        // A tie keeps the catalog's order.
        let catalog = [
            omp("anthropic/claude-opus-4-1", true),
            omp("anthropic/claude-sonnet-4-5", false),
            omp("anthropic/claude-sonnet-4-5", false),
        ];
        assert_eq!(
            pick(&catalog).as_deref(),
            Some("anthropic/claude-sonnet-4-5")
        );
    }

    #[test]
    fn claude_codes_default_described_as_opus_becomes_sonnet() {
        let catalog = [
            model(
                "default",
                "Default (recommended)",
                "Opus 4.5 · Most capable for complex work",
                true,
            ),
            // Its description mentions Sonnet, but its id says what it is.
            model(
                "opus",
                "Opus",
                "Opus 4.5 · Better than Sonnet at depth",
                false,
            ),
            model(
                "sonnet",
                "Sonnet",
                "Sonnet 4.5 · Best for everyday tasks",
                false,
            ),
            model(
                "haiku",
                "Haiku",
                "Haiku 4.5 · Fastest for quick answers",
                false,
            ),
        ];
        assert_eq!(pick(&catalog).as_deref(), Some("sonnet"));
        // A default that already is Sonnet stays.
        let catalog = [
            model(
                "default",
                "Default",
                "Sonnet 4.5 · Best for everyday tasks",
                true,
            ),
            model("opus", "Opus", "Opus 4.5", false),
            model("sonnet", "Sonnet", "Sonnet 4.5", false),
        ];
        assert_eq!(pick(&catalog).as_deref(), Some("default"));
        // Context variants do not beat the plain model.
        let catalog = [
            model("default", "Default", "Opus 4.5", true),
            model("sonnet", "Sonnet", "Sonnet 4.5", false),
            model(
                "sonnet[1m]",
                "Sonnet (1M context)",
                "Sonnet 4.5 · 1M",
                false,
            ),
        ];
        assert_eq!(pick(&catalog).as_deref(), Some("sonnet"));
    }

    #[test]
    fn a_codex_catalog_keeps_its_default_and_replaces_a_pro_default_by_the_base_model() {
        let codex = |id: &str, name: &str, default: bool| {
            model(id, name, "Optimized for professional coding work", default)
        };
        let catalog = [
            codex("gpt-5.1-codex-max", "GPT-5.1 Codex Max", false),
            codex("gpt-5-codex", "GPT-5 Codex", true),
            codex("gpt-5", "GPT-5", false),
        ];
        assert_eq!(pick(&catalog).as_deref(), Some("gpt-5-codex"));

        let catalog = [
            codex("gpt-5-pro-2025-10-06", "GPT-5 Pro", false),
            codex("gpt-5-pro", "GPT-5 Pro", true),
            codex("gpt-5-mini", "GPT-5 mini", false),
            codex("gpt-5-codex", "GPT-5 Codex", false),
            codex("gpt-5-2025-08-07", "GPT-5", false),
            codex("gpt-5.1", "GPT-5.1", false),
        ];
        assert_eq!(pick(&catalog).as_deref(), Some("gpt-5-2025-08-07"));

        let catalog = [
            omp("openai/o3-pro", true),
            omp("openai/o3-mini", false),
            omp("openai/o3", false),
            omp("openrouter/o3", false),
        ];
        assert_eq!(pick(&catalog).as_deref(), Some("openai/o3"));
    }

    #[test]
    fn google_ultra_and_deep_think_move_to_pro_and_pro_itself_stays() {
        let catalog = [
            omp("google/gemini-2.5-flash", false),
            omp("google/gemini-2.5-pro", false),
            omp("google/gemini-3-pro", false),
            omp("google/gemini-3-ultra", true),
        ];
        assert_eq!(pick(&catalog).as_deref(), Some("google/gemini-3-pro"));
        let catalog = [
            model("gemini-2.5-deep-think", "Gemini 2.5 Deep Think", "", true),
            model("gemini-2.5-pro", "Gemini 2.5 Pro", "", false),
        ];
        assert_eq!(pick(&catalog).as_deref(), Some("gemini-2.5-pro"));
        // Pro is Google's ordinary tier, not a premium one.
        let catalog = [
            omp("google/gemini-2.5-pro", true),
            omp("google/gemini-2.5-flash", false),
        ];
        assert_eq!(pick(&catalog).as_deref(), Some("google/gemini-2.5-pro"));
    }

    #[test]
    fn without_a_lower_tier_the_default_stays() {
        let catalog = [
            omp("anthropic/claude-haiku-4-5", false),
            omp("anthropic/claude-opus-4-1", true),
            // The lower tier of another provider does not count.
            omp("openrouter/claude-sonnet-4-5", false),
        ];
        assert_eq!(pick(&catalog).as_deref(), Some("anthropic/claude-opus-4-1"));
        let catalog = [
            omp("openai/gpt-5-pro", true),
            omp("openai/gpt-5-mini", false),
        ];
        assert_eq!(pick(&catalog).as_deref(), Some("openai/gpt-5-pro"));
    }

    #[test]
    fn an_empty_catalog_has_no_pick_and_hidden_models_are_skipped() {
        assert_eq!(pick(&[]), None);
        let mut hidden = omp("anthropic/claude-sonnet-4-5", false);
        hidden.hidden = true;
        assert_eq!(pick(std::slice::from_ref(&hidden)), None);

        // A hidden default is not used: the first visible model is.
        let mut default = omp("anthropic/claude-opus-4-1", true);
        default.hidden = true;
        let catalog = [default, omp("anthropic/claude-haiku-4-5", false)];
        assert_eq!(
            pick(&catalog).as_deref(),
            Some("anthropic/claude-haiku-4-5")
        );

        // A hidden Sonnet is not a replacement.
        let catalog = [omp("anthropic/claude-opus-4-1", true), hidden];
        assert_eq!(pick(&catalog).as_deref(), Some("anthropic/claude-opus-4-1"));
    }

    #[test]
    fn without_a_flagged_default_the_first_visible_model_is_the_start() {
        let catalog = [
            omp("anthropic/claude-opus-4-1", false),
            omp("anthropic/claude-sonnet-4-5", false),
        ];
        assert_eq!(
            pick(&catalog).as_deref(),
            Some("anthropic/claude-sonnet-4-5")
        );
        let catalog = [omp("anthropic/claude-haiku-4-5", false)];
        assert_eq!(
            pick(&catalog).as_deref(),
            Some("anthropic/claude-haiku-4-5")
        );
    }

    #[test]
    fn loopback_urls_need_no_key() {
        for url in [
            "http://127.0.0.1:1234/v1",
            "http://localhost:11434",
            "http://LocalHost/v1",
            "http://[::1]:8080/v1",
            "http://127.1.2.3:9",
            "https://llm.localhost/v1",
        ] {
            assert!(is_loopback_url(url), "{url}");
        }
        for url in [
            "https://api.openai.com/v1",
            "http://192.168.1.5:1234/v1",
            "http://localhost.example.com/v1",
            "http://example.com:127/v1",
            "http://0.0.0.0:1234",
        ] {
            assert!(!is_loopback_url(url), "{url}");
        }
    }
}
