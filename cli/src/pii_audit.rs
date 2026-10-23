//! Repository PII checks through an isolated, loopback-only Presidio analyzer.
//!
//! The upstream analyzer supplies regex, checksum and spaCy name recognition.
//! Repository traversal, allowlists and content-free diagnostics stay in Rust.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use futures::{StreamExt, TryStreamExt};
use regex::Regex;
use serde::{Deserialize, Serialize};

/// Diagnostic code emitted by `navigator pii`.
pub const CODE: &str = "P002";
const ALLOWLIST: &str = ".github/pii/allowlist.toml";
const ENTITIES: [&str; 7] = [
    "PERSON",
    "EMAIL_ADDRESS",
    "PHONE_NUMBER",
    "US_SSN",
    "US_ITIN",
    "CREDIT_CARD",
    "IBAN_CODE",
];
const MAX_LINE_CHARS: usize = 16_000;
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const NAME_CANARY: &str = "My name is John Smith.";

/// A diagnostic carries location and category, never the detected content.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Finding {
    pub path: String,
    pub line: usize,
    pub entity: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Group {
    reason: String,
    values: Vec<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Allowlist {
    paths: Vec<Group>,
    values: Vec<Group>,
    names: Vec<Group>,
    name_tokens: Vec<Group>,
    non_name_words: Vec<Group>,
    email_domains: Vec<Group>,
    phones: Vec<Group>,
}

impl Allowlist {
    fn parse(text: &str) -> Result<Self> {
        let allow: Self = toml::from_str(text).context("invalid PII allowlist")?;
        for group in [
            &allow.paths,
            &allow.values,
            &allow.names,
            &allow.name_tokens,
            &allow.non_name_words,
            &allow.email_domains,
            &allow.phones,
        ]
        .into_iter()
        .flatten()
        {
            ensure!(
                !group.reason.trim().is_empty()
                    && !group.values.is_empty()
                    && group.values.iter().all(|v| !v.trim().is_empty()),
                "PII exceptions require values and a reason"
            );
        }
        Ok(allow)
    }

    fn contains(groups: &[Group], value: &str) -> bool {
        groups
            .iter()
            .flat_map(|g| &g.values)
            .any(|v| v.trim().to_lowercase() == value.to_lowercase())
    }

    fn skips(&self, path: &str) -> Result<bool> {
        let path = path.to_lowercase();
        for pattern in self.paths.iter().flat_map(|g| &g.values) {
            // Existing path exceptions use shell '*'/'?' over the whole path.
            let escaped = regex::escape(&pattern.to_lowercase())
                .replace(r"\*", ".*")
                .replace(r"\?", ".");
            if Regex::new(&format!("^{escaped}$"))?.is_match(&path) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn allows(&self, entity: &str, value: &str, patterns: &Patterns) -> bool {
        if Self::contains(&self.values, value.trim()) {
            return true;
        }
        match entity {
            "PERSON" => {
                let words = name_tokens(value);
                self.names
                    .iter()
                    .flat_map(|group| &group.values)
                    .any(|name| name_tokens(name) == words)
                    || words
                        .iter()
                        .any(|t| Self::contains(&self.non_name_words, t))
                    || (!words.is_empty()
                        && words.iter().all(|t| Self::contains(&self.name_tokens, t)))
            }
            "EMAIL_ADDRESS" => {
                let domain = value.rsplit('@').next().unwrap_or_default().to_lowercase();
                patterns.reserved.is_match(value)
                    || self.email_domains.iter().flat_map(|g| &g.values).any(|d| {
                        domain == d.to_lowercase()
                            || domain.ends_with(&format!(".{}", d.to_lowercase()))
                    })
            }
            "PHONE_NUMBER" => {
                patterns.fictional_phone.is_match(value)
                    || self
                        .phones
                        .iter()
                        .flat_map(|g| &g.values)
                        .any(|v| phone_digits(v) == phone_digits(value))
            }
            _ => false,
        }
    }
}

fn phone_digits(value: &str) -> String {
    let digits: String = value.chars().filter(char::is_ascii_digit).collect();
    digits
        .chars()
        .skip(digits.len().saturating_sub(10))
        .collect()
}

fn name_tokens(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .map(|v| {
            v.trim_matches(['\'', '’', '.', ','])
                .trim_end_matches("'s")
                .trim_end_matches("’s")
                .to_lowercase()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

struct Patterns {
    candidate: Regex,
    string: Regex,
    comment: Regex,
    reserved: Regex,
    fictional_phone: Regex,
    phone: Regex,
    tax_id: Regex,
    name: Regex,
}

impl Patterns {
    fn new() -> Result<Self> {
        Ok(Self {
            candidate: Regex::new(
                r"\b[A-Z][a-z'’]+(?:\s+[A-Z]\.)?\s+[A-Z][a-zA-Z'’-]+|@|\d(?:[\s().-]*\d){6,}",
            )?,
            string: Regex::new(r#""((?:[^"\\]|\\.){3,})""#)?,
            comment: Regex::new(r"(?://|#|--)\s?(.*)$")?,
            reserved: Regex::new(
                r"(?i)@(?:[a-z0-9-]+\.)*(?:example|test|invalid|localhost)(?:\.[a-z]+)?$|@example\.(?:com|org|net)$",
            )?,
            fictional_phone: Regex::new(r"(?:^|\D)555(?:\D|$)|555\D?01\d\d")?,
            phone: Regex::new(
                r"^(?:\+?1[\s.-]?)?\(?[2-9]\d{2}\)?[\s.-]\d{3}[\s.-]\d{4}$|^\+\d[\d\s().-]{8,}\d$",
            )?,
            tax_id: Regex::new(r"^\d{3}-\d{2}-\d{4}$|^\d{3} \d{2} \d{4}$")?,
            name: Regex::new(r"^[A-Z][a-zA-Z'’.-]*$")?,
        })
    }

    fn plausible(&self, entity: &str, value: &str) -> bool {
        match entity {
            "EMAIL_ADDRESS" => !value.contains(['/', '{', '}', '=']),
            "PHONE_NUMBER" => self.phone.is_match(value),
            "US_SSN" | "US_ITIN" => self.tax_id.is_match(value),
            "PERSON" => {
                let words: Vec<_> = value.split_whitespace().collect();
                (2..=4).contains(&words.len())
                    && words
                        .iter()
                        .all(|word| self.name.is_match(word.trim_matches(['\'', '’', '.', ','])))
            }
            _ => true,
        }
    }

    fn human_text(&self, path: &str, line: &str) -> String {
        let p = Path::new(path);
        let extension = p
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_lowercase();
        if matches!(
            extension.as_str(),
            "md" | "markdown"
                | "txt"
                | "yaml"
                | "yml"
                | "json"
                | "jsonl"
                | "feature"
                | "html"
                | "csv"
                | "eml"
                | "fields"
                | "tex"
                | "typ"
                | "toml"
        ) || matches!(
            p.file_name().and_then(|s| s.to_str()),
            Some("NOTICE" | "LICENSE")
        ) {
            return line.to_owned();
        }
        if !matches!(
            extension.as_str(),
            "rs" | "py"
                | "sh"
                | "js"
                | "ts"
                | "tsx"
                | "jsx"
                | "css"
                | "surql"
                | "sql"
                | "rego"
                | "tf"
                | "dockerfile"
        ) {
            return String::new();
        }
        let mut parts: Vec<_> = self
            .string
            .captures_iter(line)
            .filter_map(|c| c.get(1).map(|m| m.as_str()))
            .collect();
        if !line.trim_start().starts_with(['"']) && !line.trim_start().starts_with("r#") {
            if let Some(comment) = self.comment.captures(line).and_then(|c| c.get(1)) {
                parts.push(comment.as_str());
            }
        }
        parts.join("  ")
    }
}

#[derive(Deserialize)]
struct Detection {
    entity_type: String,
    start: usize,
    end: usize,
    score: f64,
}
#[derive(Serialize)]
struct AnalyzeRequest<'a> {
    text: &'a str,
    language: &'static str,
    entities: [&'static str; 7],
    score_threshold: f64,
}

/// Permit only explicit loopback IPs, without credentials, paths or query data.
fn endpoint(value: &str) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(value).context("invalid analyzer URL")?;
    let local = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    };
    ensure!(
        url.scheme() == "http" && local,
        "analyzer must use an explicit loopback IP"
    );
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path() == "/",
        "analyzer must be a plain local origin"
    );
    url.set_path("/");
    Ok(url)
}

async fn response<T: serde::de::DeserializeOwned>(request: reqwest::RequestBuilder) -> Result<T> {
    let mut reply = request
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("local PII analyzer request failed"))?;
    ensure!(
        reply.status().is_success(),
        "local PII analyzer returned a failure status"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = reply
        .chunk()
        .await
        .map_err(|_| anyhow::anyhow!("local PII analyzer response failed"))?
    {
        ensure!(
            bytes.len() + chunk.len() <= MAX_RESPONSE_BYTES,
            "PII analyzer response exceeds its limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("local PII analyzer returned invalid JSON"))
}

async fn analyze(
    client: &reqwest::Client,
    base: &reqwest::Url,
    text: &str,
) -> Result<Vec<Detection>> {
    response(client.post(base.join("analyze")?).json(&AnalyzeRequest {
        text,
        language: "en",
        entities: ENTITIES,
        score_threshold: 0.4,
    }))
    .await
}

struct Scanner<'a> {
    client: &'a reqwest::Client,
    base: &'a reqwest::Url,
    allow: &'a Allowlist,
    patterns: &'a Patterns,
}
impl Scanner<'_> {
    async fn scan_line(&self, text: &str, path: &str, line: usize) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        let chars: Vec<char> = text.chars().collect();
        for found in analyze(self.client, self.base, text).await? {
            ensure!(
                ENTITIES.contains(&found.entity_type.as_str())
                    && found.score.is_finite()
                    && (0.0..=1.0).contains(&found.score)
                    && found.start < found.end
                    && found.end <= chars.len(),
                "PII analyzer returned an invalid detection"
            );
            let value: String = chars[found.start..found.end].iter().collect();
            if found.score < 0.4
                || !self.patterns.plausible(&found.entity_type, &value)
                || self.allow.allows(&found.entity_type, &value, self.patterns)
            {
                continue;
            }
            findings.push(Finding {
                path: path.to_owned(),
                line,
                entity: found.entity_type,
            });
        }
        Ok(findings)
    }
}

/// Scan tracked repository text using the configured local analyzer.
/// Analyzer failures fail closed. Requests cannot follow redirects or proxies.
pub async fn audit_tree(root: &Path, analyzer: &str) -> Result<Vec<Finding>> {
    let base = endpoint(analyzer)?;
    let allow = Allowlist::parse(
        &std::fs::read_to_string(root.join(ALLOWLIST)).context("read PII allowlist")?,
    )?;
    let patterns = Patterns::new()?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(60))
        .build()?;
    let supported: Vec<String> = response(
        client
            .get(base.join("supportedentities")?)
            .query(&[("language", "en")]),
    )
    .await?;
    ensure!(
        ENTITIES.iter().all(|e| supported.iter().any(|s| s == e)),
        "PII analyzer lacks required recognizers"
    );
    ensure!(
        analyze(&client, &base, NAME_CANARY)
            .await?
            .iter()
            .any(|d| d.entity_type == "PERSON" && d.start == 11 && d.end == 21 && d.score >= 0.4),
        "PII analyzer failed its name-recognition canary"
    );
    let output = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        .context("enumerate tracked PII inputs")?;
    ensure!(output.status.success(), "PII scan requires a Git checkout");
    let paths = std::str::from_utf8(&output.stdout).context("tracked PII paths must be UTF-8")?;
    let scanner = Scanner {
        client: &client,
        base: &base,
        allow: &allow,
        patterns: &patterns,
    };
    let mut inputs = Vec::new();
    for path in paths.split('\0').filter(|p| !p.is_empty()) {
        if allow.skips(path)? {
            continue;
        }
        let source = root.join(path);
        ensure!(
            !std::fs::symlink_metadata(&source)?.file_type().is_symlink(),
            "PII input cannot be a symlink"
        );
        let bytes = std::fs::read(&source).context("read tracked PII input")?;
        let Ok(contents) = std::str::from_utf8(&bytes) else {
            continue;
        };
        for (index, line) in contents.lines().enumerate() {
            if line.contains("pii-scan: allow") {
                continue;
            }
            let chunk = patterns.human_text(path, line);
            let chunk = chunk.trim();
            if chunk.len() < 5 || !patterns.candidate.is_match(chunk) {
                continue;
            }
            let size = chunk.chars().count();
            ensure!(
                size <= MAX_LINE_CHARS,
                "PII input line exceeds the analyzer limit"
            );
            inputs.push((chunk.to_owned(), path.to_owned(), index + 1));
        }
    }
    let batches: Vec<Vec<Finding>> = futures::stream::iter(
        inputs
            .iter()
            .map(|(text, path, line)| scanner.scan_line(text, path, *line)),
    )
    .buffer_unordered(8)
    .try_collect()
    .await?;
    Ok(batches
        .into_iter()
        .flatten()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}

#[cfg(test)]
#[path = "pii_audit_tests.rs"]
mod tests;
