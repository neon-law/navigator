//! `P001` — the public source snapshot audit.
//!
//! Every tracked file of a tree that carries [`ALLOWLIST`] is read and held to
//! what a public source baseline may contain: source, documentation, firm
//! public facts, and synthetic fixtures. A finding names its [`Class`]:
//!
//! - `credential` — a private key block, a cloud or API token shape, or a URL
//!   with a real password in it. The message names the detector, never the
//!   value.
//! - `email` — an address outside the reserved `example.com`, `.example`,
//!   `.invalid`, `.test`, and `.localhost` domains.
//! - `phone` — a North American number outside the fictional `555` exchange.
//! - `gcp` — a Google Cloud project id, project number, bucket, service-account
//!   address, or Workload Identity provider path.
//! - `env` — a `.env` file, a credential file name, or a SOPS document with a
//!   plaintext value.
//! - `build` — generated build or dependency output.
//! - `binary-metadata` — an image's EXIF, XMP, or text chunk, or a PDF author.
//! - `binary` — a binary format this audit cannot read for metadata.
//!
//! A finding the tree keeps on purpose is listed in [`ALLOWLIST`], one entry
//! per reason. See `docs/public-snapshot-audit.md`.

use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;
use std::process::Command;
use std::sync::LazyLock;

use regex::Regex;
use serde::Deserialize;

/// The rule code every finding carries.
pub const CODE: &str = "P001";

/// The checked-in allowlist. Its presence opts a tree into the audit, so a
/// Project repository or an unrelated directory is never held to it.
pub const ALLOWLIST: &str = ".github/public-snapshot/allowlist.toml";

/// What kind of material a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Class {
    Credential,
    Email,
    Phone,
    Gcp,
    Env,
    Build,
    BinaryMetadata,
    Binary,
}

impl Class {
    fn as_str(self) -> &'static str {
        match self {
            Self::Credential => "credential",
            Self::Email => "email",
            Self::Phone => "phone",
            Self::Gcp => "gcp",
            Self::Env => "env",
            Self::Build => "build",
            Self::BinaryMetadata => "binary-metadata",
            Self::Binary => "binary",
        }
    }
}

impl fmt::Display for Class {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One piece of material that must not enter a public baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Repository-relative, `/`-separated.
    pub path: String,
    /// 1-based; `1` for a finding about the whole file.
    pub line: usize,
    pub class: Class,
    /// What an allowlist `match` is compared against: the address, the ten
    /// phone digits, the cloud identifier, the chunk or detector name, or the
    /// path itself. A credential's value is its detector name.
    pub value: String,
    /// Why it was refused.
    pub detail: String,
}

impl Finding {
    /// The diagnostic message, without the location or code.
    #[must_use]
    pub fn message(&self) -> String {
        format!("{}: {}", self.class, self.detail)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AllowlistFile {
    #[serde(default)]
    allow: Vec<RawEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
    class: Class,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(rename = "match")]
    pattern: Option<String>,
    reason: String,
}

#[derive(Debug)]
struct Entry {
    class: Class,
    paths: Vec<String>,
    pattern: Option<Regex>,
    reason: String,
}

impl Entry {
    fn permits(&self, finding: &Finding) -> bool {
        self.class == finding.class
            && (self.paths.is_empty()
                || self
                    .paths
                    .iter()
                    .any(|path| path_matches(path, &finding.path)))
            && self
                .pattern
                .as_ref()
                .is_none_or(|pattern| pattern.is_match(&finding.value))
    }

    fn describe(&self) -> String {
        let mut scope = Vec::new();
        if self.pattern.is_some() {
            scope.push("value pattern configured".to_owned());
        }
        if !self.paths.is_empty() {
            scope.push(format!("paths {}", self.paths.join(", ")));
        }
        format!("{} ({}): {}", self.class, scope.join("; "), self.reason)
    }
}

/// An exact path, or a directory prefix ending in `/`.
fn path_matches(entry: &str, path: &str) -> bool {
    match entry.strip_suffix('/') {
        Some(_) => path.starts_with(entry),
        None => entry == path,
    }
}

/// The parsed [`ALLOWLIST`].
#[derive(Debug, Default)]
pub struct Allowlist {
    entries: Vec<Entry>,
}

impl Allowlist {
    /// Parse the allowlist. Every entry names a class and a non-empty
    /// `reason`, and narrows itself by `paths`, by `match` (a regular
    /// expression the whole finding value must match), or both.
    pub fn parse(text: &str) -> Result<Self, String> {
        let file: AllowlistFile = toml::from_str(text).map_err(|error| error.to_string())?;
        let mut entries = Vec::with_capacity(file.allow.len());
        for (index, raw) in file.allow.into_iter().enumerate() {
            let number = index + 1;
            if raw.reason.trim().is_empty() {
                return Err(format!("entry {number} ({}) has no reason", raw.class));
            }
            if raw.paths.is_empty() && raw.pattern.is_none() {
                return Err(format!(
                    "entry {number} ({}) needs `paths`, `match`, or both; a whole class is never allowed",
                    raw.class
                ));
            }
            let pattern = raw
                .pattern
                .map(|pattern| {
                    Regex::new(&format!("^(?:{pattern})$"))
                        .map_err(|error| format!("entry {number} ({}): {error}", raw.class))
                })
                .transpose()?;
            entries.push(Entry {
                class: raw.class,
                paths: raw.paths,
                pattern,
                reason: raw.reason,
            });
        }
        Ok(Self { entries })
    }

    fn permitting(&self, finding: &Finding) -> Option<usize> {
        self.entries.iter().position(|entry| entry.permits(finding))
    }
}

/// The result of auditing one tree.
#[derive(Debug, Default)]
pub struct Audit {
    pub files_scanned: usize,
    /// Findings no allowlist entry permits; each one fails the run.
    pub findings: Vec<Finding>,
    /// Allowlist entries that permitted nothing, described for a warning.
    pub unused_entries: Vec<String>,
}

/// Audit `root` when it carries [`ALLOWLIST`]; `Ok(None)` when it does not.
///
/// The files are the ones `git ls-files` tracks, or every file outside `.git/`
/// when `root` is not a Git work tree.
pub fn audit_tree(root: &Path) -> Result<Option<Audit>, String> {
    let allowlist_path = root.join(ALLOWLIST);
    if !allowlist_path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&allowlist_path)
        .map_err(|error| format!("read {ALLOWLIST}: {error}"))?;
    let allowlist = Allowlist::parse(&text).map_err(|error| format!("{ALLOWLIST}: {error}"))?;
    let files = tracked_files(root)?;
    let mut audit = Audit::default();
    let mut used = BTreeSet::new();
    for relative in files {
        if relative == ALLOWLIST {
            continue;
        }
        let path = root.join(&relative);
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let bytes = std::fs::read(&path).map_err(|error| format!("read {relative}: {error}"))?;
        audit.files_scanned += 1;
        for finding in scan_file(&relative, &bytes) {
            match allowlist.permitting(&finding) {
                Some(index) => {
                    used.insert(index);
                }
                None => audit.findings.push(finding),
            }
        }
    }
    audit.unused_entries = allowlist
        .entries
        .iter()
        .enumerate()
        .filter(|(index, _)| !used.contains(index))
        .map(|(_, entry)| entry.describe())
        .collect();
    Ok(Some(audit))
}

fn tracked_files(root: &Path) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z", "--cached"])
        .output();
    let listed = output
        .ok()
        .filter(|output| output.status.success() && root.join(".git").exists());
    if let Some(output) = listed {
        let mut files: Vec<String> = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|name| !name.is_empty())
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .collect();
        files.sort();
        return Ok(files);
    }
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| entry.file_name() != ".git")
    {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry.file_type().is_file() {
            continue;
        }
        if let Ok(relative) = entry.path().strip_prefix(root) {
            files.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    files.sort();
    Ok(files)
}

/// Every finding in one file, before the allowlist is applied.
#[must_use]
pub fn scan_file(path: &str, bytes: &[u8]) -> Vec<Finding> {
    let mut findings = path_findings(path);
    let extension = path
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    if BINARY_FORMATS.contains(&extension.as_str()) || bytes.contains(&0) {
        findings.extend(binary_findings(path, &extension, bytes));
    } else {
        let text = String::from_utf8_lossy(bytes);
        findings.extend(text_findings(path, &text));
    }
    findings
}

/// Formats read as binary whatever their first bytes hold.
const BINARY_FORMATS: &[&str] = &[
    "png", "jpg", "jpeg", "webp", "pdf", "ttf", "otf", "woff", "woff2",
];
/// Fonts carry their own copyright and licence names, which the notices preserve.
const FONT_FORMATS: &[&str] = &["ttf", "otf", "woff", "woff2"];
/// File names that are build, coverage, or operating-system output.
const BUILD_FILES: &[&str] = &[".ds_store", "thumbs.db", "lcov.info"];
const BUILD_SUFFIXES: &[&str] = &[
    ".pyc", ".pyo", ".o", ".obj", ".a", ".so", ".dylib", ".dll", ".exe", ".rlib", ".rmeta",
    ".class", ".profraw", ".wasm",
];
/// `.env` templates that document variable names without values.
const ENV_TEMPLATES: &[&str] = &[".env.example", ".env.sample", ".env.template"];

fn whole_file(path: &str, class: Class, detail: &str) -> Finding {
    Finding {
        path: path.to_string(),
        line: 1,
        class,
        value: path.to_string(),
        detail: detail.to_string(),
    }
}

fn path_findings(path: &str) -> Vec<Finding> {
    let segments: Vec<&str> = path.split('/').collect();
    let (name, directories) = segments.split_last().unwrap_or((&"", &[]));
    let name = name.to_ascii_lowercase();
    let mut findings = Vec::new();
    let build_directory = directories.iter().any(|directory| {
        rules::source_archive::BUILD_OUTPUT.contains(&directory.to_ascii_lowercase().as_str())
    });
    if build_directory
        || BUILD_FILES.contains(&name.as_str())
        || BUILD_SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
    {
        findings.push(whole_file(
            path,
            Class::Build,
            "generated build or dependency output",
        ));
    }
    let env_file =
        name == ".env" || (name.starts_with(".env.") && !ENV_TEMPLATES.contains(&name.as_str()));
    if env_file
        || rules::source_archive::CREDENTIAL_NAMES.contains(&name.as_str())
        || rules::source_archive::CREDENTIAL_SUFFIXES
            .iter()
            .any(|suffix| name.ends_with(suffix))
        || name.contains(".decrypted.")
    {
        findings.push(whole_file(path, Class::Env, "a secret-bearing file name"));
    }
    findings
}

static PRIVATE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----(?:\s|\\n)*(?:[A-Za-z-]+:[^\n]*(?:\s|\\n)+)*[A-Za-z0-9+/=]{40,}")
        .expect("private key pattern")
});

static TOKENS: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    [
        ("an AWS access key id", r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"),
        (
            "a GitHub token",
            r"\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{60,})",
        ),
        ("a Slack token", r"\bxox[abposr]-[0-9A-Za-z-]{10,}"),
        ("a Stripe live key", r"\b[rs]k_live_[0-9A-Za-z]{16,}"),
        ("a Google API key", r"\bAIza[0-9A-Za-z_-]{35}\b"),
        (
            "a SendGrid API key",
            r"\bSG\.[A-Za-z0-9_-]{22}\.[A-Za-z0-9_-]{43}\b",
        ),
        (
            "a service-account key id",
            r#""private_key_id"\s*:\s*"[0-9a-f]{40}""#,
        ),
    ]
    .into_iter()
    .map(|(name, pattern)| (name, Regex::new(pattern).expect("token pattern")))
    .collect()
});

static URL_PASSWORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\b[a-z][a-z0-9+.-]*://([^\s/:@"'<>]+):([^\s/@"'<>]+)@([A-Za-z0-9.-]+)"#)
        .expect("url password pattern")
});

/// Passwords that name their own role rather than holding a secret.
const PLACEHOLDER_PASSWORDS: &[&str] = &[
    "password",
    "pass",
    "passwd",
    "secret",
    "token",
    "changeme",
    "redacted",
    "x",
    "xxx",
    "user",
    "postgres",
    "root",
    "admin",
    "test",
    "dev",
    "navigator",
];

static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}").expect("email pattern")
});

/// Top-level labels a file name ends in, which an `@2x.png`-style name is not an address at.
const FILE_EXTENSION_TLDS: &[&str] = &["png", "jpg", "svg", "webp", "gif", "js", "css", "rs", "md"];

static PHONE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?P<cc>\+?1[ .-]?)?(?P<open>\()?(?P<a>[2-9]\d{2})(?P<close>\))?(?P<s1>[ .-]?)(?P<b>[2-9]\d{2})(?P<s2>[ .-])(?P<c>\d{4})",
    )
    .expect("phone pattern")
});

/// The separator-free E.164 form, `+1NXXNXXXXXX`.
static PHONE_E164: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\+1(?P<a>[2-9]\d{2})(?P<b>[2-9]\d{2})(?P<c>\d{4})").expect("e164 pattern")
});

/// A GCP project id: 6–30 characters, lowercase letters, digits, and hyphens.
const PROJECT_ID: &str = r"[a-z][a-z0-9-]{4,28}[a-z0-9]";

static GCP: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    [
        (
            "a service-account address",
            r"\b(?P<id>[a-z][a-z0-9-]*@(?P<project>[a-z][a-z0-9-]*)\.iam\.gserviceaccount\.com)\b".to_string(),
        ),
        (
            "a default compute service account",
            r"\b(?P<id>(?P<number>\d{6,})-compute@developer\.gserviceaccount\.com)\b".to_string(),
        ),
        (
            "a Workload Identity pool path",
            r"(?P<id>projects/(?P<number>\d+)/locations/global/workloadIdentityPools/[A-Za-z0-9_-]+(?:/providers/[A-Za-z0-9_-]+)?)".to_string(),
        ),
        ("a Cloud Storage bucket", r"gs://(?P<id>[a-z0-9][a-z0-9._-]{1,61}[a-z0-9])".to_string()),
        (
            "a project id in a resource path",
            format!(
                r"\bprojects/(?P<id>{PROJECT_ID})/(?:locations|secrets|serviceAccounts|global|zones|regions|topics|subscriptions|databases|instances|buckets|keyRings|repositories|roles|sinks|logs|metrics|queries|datasets|jobs)\b"
            ),
        ),
        (
            "a project id in a project variable",
            format!(
                r#"\b(?:CLOUDSDK_CORE_PROJECT|GOOGLE_CLOUD_PROJECT|GCLOUD_PROJECT|GCP_PROJECT(?:_ID)?|GOOGLE_PROJECT_ID)["']?\s*[:=]\s*["']?(?P<id>{PROJECT_ID})\b"#
            ),
        ),
        (
            "a project id or bucket in a deployment variable",
            format!(
                r#"\b[A-Z][A-Z0-9_]*(?:_PROJECT|_PROJECT_ID|_BUCKET|_SERVICE_ACCOUNT|_SERVICE_ACCOUNT_ID)["']?\s*[:=]\s*["']?(?P<id>{PROJECT_ID})\b"#
            ),
        ),
        (
            "a project id in a registry path",
            format!(r"\b(?:[a-z0-9-]+-docker\.pkg\.dev|gcr\.io)/(?P<id>{PROJECT_ID})/"),
        ),
    ]
    .into_iter()
    .map(|(name, pattern)| (name, Regex::new(&pattern).expect("gcp pattern")))
    .collect()
});

/// `--project <id>`, read only on a line that runs a Google Cloud tool, since
/// Navigator's own `--project` names a Project code.
static GCLOUD_PROJECT_FLAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r#"--project[= ]["']?(?P<id>{PROJECT_ID})\b"#))
        .expect("project flag pattern")
});
static GCLOUD_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:gcloud|gsutil|bq)\b").expect("gcloud line pattern"));

static SOPS_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<key>[A-Z][A-Z0-9_]*):[ \t]*(?P<value>\S.*)$").expect("sops value pattern")
});

/// Line starts, so a byte offset maps to a 1-based line.
struct Lines(Vec<usize>);

impl Lines {
    fn new(text: &str) -> Self {
        Self(
            std::iter::once(0)
                .chain(text.match_indices('\n').map(|(index, _)| index + 1))
                .collect(),
        )
    }

    fn of(&self, offset: usize) -> usize {
        self.0.partition_point(|start| *start <= offset)
    }
}

/// Collects one file's findings, numbering each by the line its offset falls on.
struct Sink<'a> {
    path: &'a str,
    lines: Lines,
    findings: Vec<Finding>,
}

impl Sink<'_> {
    fn push(&mut self, offset: usize, class: Class, value: String, detail: &str) {
        self.findings.push(Finding {
            path: self.path.to_string(),
            line: self.lines.of(offset),
            class,
            value,
            detail: detail.to_string(),
        });
    }
}

fn text_findings(path: &str, text: &str) -> Vec<Finding> {
    let mut sink = Sink {
        path,
        lines: Lines::new(text),
        findings: Vec::new(),
    };
    credential_findings(&mut sink, text);
    contact_findings(&mut sink, text);
    gcp_findings(&mut sink, text);
    if is_sops_document(path) {
        sops_findings(&mut sink, text);
    }
    sink.findings
}

fn credential_findings(sink: &mut Sink<'_>, text: &str) {
    for found in PRIVATE_KEY.find_iter(text) {
        sink.push(
            found.start(),
            Class::Credential,
            "private-key".to_string(),
            "a private key block",
        );
    }
    for (name, pattern) in TOKENS.iter() {
        for found in pattern.find_iter(text) {
            sink.push(found.start(), Class::Credential, (*name).to_string(), name);
        }
    }
    for captures in URL_PASSWORD.captures_iter(text) {
        let (user, password, host) = (&captures[1], &captures[2], &captures[3]);
        if !placeholder_password(user, password) && !local_or_reserved_host(host) {
            let offset = captures.get(0).map_or(0, |found| found.start());
            sink.push(
                offset,
                Class::Credential,
                "url-password".to_string(),
                "a URL with a password",
            );
        }
    }
}

fn contact_findings(sink: &mut Sink<'_>, text: &str) {
    for found in EMAIL.find_iter(text) {
        let address = found.as_str().trim_end_matches(['.', '-']);
        let Some((local, domain)) = address.rsplit_once('@') else {
            continue;
        };
        let domain = domain.to_ascii_lowercase();
        if domain.ends_with(".gserviceaccount.com")
            || reserved_domain(&domain)
            || local == "git"
            || FILE_EXTENSION_TLDS.contains(&domain.rsplit('.').next().unwrap_or_default())
        {
            continue;
        }
        sink.push(
            found.start(),
            Class::Email,
            address.to_string(),
            "an email address outside the reserved domains",
        );
    }
    for captures in PHONE.captures_iter(text) {
        let Some(found) = captures.get(0) else {
            continue;
        };
        if let Some(digits) = dialable_number(text, &captures, found.start(), found.end()) {
            sink.push(
                found.start(),
                Class::Phone,
                digits,
                "a dialable phone number",
            );
        }
    }
    for captures in PHONE_E164.captures_iter(text) {
        let Some(found) = captures.get(0) else {
            continue;
        };
        let (area, exchange) = (&captures["a"], &captures["b"]);
        let continues = text[found.end()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit());
        if !continues && !fictional_number(area, exchange) {
            sink.push(
                found.start(),
                Class::Phone,
                format!("{area}{exchange}{}", &captures["c"]),
                "a dialable phone number",
            );
        }
    }
}

/// The `555` exchange and area code, and the `N11` service codes, reach no one.
fn fictional_number(area: &str, exchange: &str) -> bool {
    area == "555" || exchange == "555" || area.ends_with("11")
}

fn gcp_findings(sink: &mut Sink<'_>, text: &str) {
    for (name, pattern) in GCP.iter() {
        for captures in pattern.captures_iter(text) {
            let Some(id) = captures.name("id") else {
                continue;
            };
            let placeholder = captures.name("project").map_or_else(
                || placeholder_cloud_name(id.as_str()),
                |project| placeholder_cloud_name(project.as_str()),
            ) || captures
                .name("number")
                .is_some_and(|number| placeholder_number(number.as_str()));
            if !placeholder {
                sink.push(id.start(), Class::Gcp, id.as_str().to_string(), name);
            }
        }
    }
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if GCLOUD_LINE.is_match(line) {
            for captures in GCLOUD_PROJECT_FLAG.captures_iter(line) {
                let Some(id) = captures.name("id") else {
                    continue;
                };
                if !placeholder_cloud_name(id.as_str()) {
                    sink.push(
                        offset + id.start(),
                        Class::Gcp,
                        id.as_str().to_string(),
                        "a project id in a gcloud flag",
                    );
                }
            }
        }
        offset += line.len();
    }
}

fn sops_findings(sink: &mut Sink<'_>, text: &str) {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if let Some(captures) = SOPS_VALUE.captures(line.trim_end()) {
            if !captures["value"].starts_with("ENC[") {
                sink.push(
                    offset,
                    Class::Env,
                    captures["key"].to_string(),
                    "a SOPS document carries a plaintext value for",
                );
            }
        }
        offset += line.len();
    }
}

fn is_sops_document(path: &str) -> bool {
    let name = path
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let yaml = Path::new(&name)
        .extension()
        .is_some_and(|extension| extension == "yaml" || extension == "yml");
    (name.contains(".enc.") || name.contains(".sops.")) && yaml
}

fn reserved_domain(domain: &str) -> bool {
    let tld = domain.rsplit('.').next().unwrap_or_default();
    matches!(tld, "example" | "invalid" | "test" | "localhost")
        || domain == "localhost"
        || ["example.com", "example.org", "example.net"]
            .iter()
            .any(|reserved| domain == *reserved || domain.ends_with(&format!(".{reserved}")))
}

fn local_or_reserved_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    !host.contains('.')
        || matches!(
            host.as_str(),
            "127.0.0.1" | "0.0.0.0" | "host.docker.internal"
        )
        || reserved_domain(&host)
}

fn placeholder_password(user: &str, password: &str) -> bool {
    password.starts_with(['$', '<', '{', '%', '*'])
        || password == user
        || PLACEHOLDER_PASSWORDS.contains(&password.to_ascii_lowercase().as_str())
}

/// A cloud name no real deployment holds: too short to be a project id, or
/// spelled as a placeholder.
fn placeholder_cloud_name(name: &str) -> bool {
    name.len() < 6
        || PLACEHOLDER_CLOUD_WORDS
            .iter()
            .any(|word| name.contains(word))
        || ["my-", "your-", "test-proj", "proj-"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        || PUBLIC_PROJECTS.contains(&name)
}

/// Words that mark a cloud name as a fixture rather than a deployment's.
const PLACEHOLDER_CLOUD_WORDS: &[&str] = &[
    "example",
    "placeholder",
    "invented",
    "fake",
    "dummy",
    "synthetic",
    "fixture",
    "sample",
];

/// Google's own public image projects, which every self-hosted build pulls.
const PUBLIC_PROJECTS: &[&str] = &["distroless", "google-containers", "cloudsdktool"];

/// A project number spelled as a placeholder: one repeated digit, or a run
/// of `123…`.
fn placeholder_number(number: &str) -> bool {
    let first = number.as_bytes()[0];
    number.bytes().all(|digit| digit == first) || "12345678901234567890".starts_with(number)
}

fn dialable_number(
    text: &str,
    captures: &regex::Captures<'_>,
    start: usize,
    end: usize,
) -> Option<String> {
    let before = text[..start].chars().next_back();
    if before.is_some_and(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '/' | '+')) {
        return None;
    }
    let mut after = text[end..].chars();
    let next = after.next();
    if next.is_some_and(|c| c.is_alphanumeric() || c == '_')
        || (next == Some('.') && after.next().is_some_and(|c| c.is_ascii_digit()))
    {
        return None;
    }
    let parenthesized = match (captures.name("open"), captures.name("close")) {
        (Some(_), Some(_)) => true,
        (None, None) => false,
        _ => return None,
    };
    let s1 = captures.name("s1").map_or("", |m| m.as_str());
    let s2 = &captures["s2"];
    let has_country = captures.name("cc").is_some();
    if !parenthesized && (s1.is_empty() || s1 != s2) {
        return None;
    }
    if !parenthesized && !has_country && s2 == " " {
        return None;
    }
    let (area, exchange) = (&captures["a"], &captures["b"]);
    if fictional_number(area, exchange) {
        return None;
    }
    Some(format!("{area}{exchange}{}", &captures["c"]))
}

fn binary_findings(path: &str, extension: &str, bytes: &[u8]) -> Vec<Finding> {
    let chunks = match extension {
        "png" => png_metadata(bytes),
        "jpg" | "jpeg" => jpeg_metadata(bytes),
        "webp" => webp_metadata(bytes),
        "pdf" => pdf_metadata(bytes),
        _ if FONT_FORMATS.contains(&extension) => Vec::new(),
        _ => {
            return vec![whole_file(
                path,
                Class::Binary,
                "a binary format the audit cannot read for embedded metadata",
            )];
        }
    };
    chunks
        .into_iter()
        .map(|chunk| Finding {
            path: path.to_string(),
            line: 1,
            class: Class::BinaryMetadata,
            value: chunk.to_string(),
            detail: "embedded metadata".to_string(),
        })
        .collect()
}

fn png_metadata(bytes: &[u8]) -> Vec<&'static str> {
    let mut found = Vec::new();
    let mut offset = 8;
    while offset + 8 <= bytes.len() {
        let length = u32::from_be_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]);
        let kind = &bytes[offset + 4..offset + 8];
        for name in ["eXIf", "tEXt", "zTXt", "iTXt"] {
            if kind == name.as_bytes() {
                found.push(name);
            }
        }
        offset = offset.saturating_add(12).saturating_add(length as usize);
    }
    found
}

fn jpeg_metadata(bytes: &[u8]) -> Vec<&'static str> {
    let mut found = Vec::new();
    let mut offset = 2;
    while offset + 4 <= bytes.len() && bytes[offset] == 0xFF {
        let marker = bytes[offset + 1];
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        let length = usize::from(u16::from_be_bytes([bytes[offset + 2], bytes[offset + 3]]));
        match marker {
            0xE1 => found.push("APP1 (Exif or XMP)"),
            0xED => found.push("APP13 (IPTC)"),
            0xFE => found.push("COM"),
            _ => {}
        }
        offset += 2 + length;
    }
    found
}

fn webp_metadata(bytes: &[u8]) -> Vec<&'static str> {
    let mut found = Vec::new();
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let kind = &bytes[offset..offset + 4];
        let length = u32::from_le_bytes([
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
            bytes[offset + 7],
        ]) as usize;
        if kind == b"EXIF" {
            found.push("EXIF");
        } else if kind == b"XMP " {
            found.push("XMP");
        }
        offset = offset
            .saturating_add(8)
            .saturating_add(length + (length & 1));
    }
    found
}

fn pdf_metadata(bytes: &[u8]) -> Vec<&'static str> {
    let mut found = Vec::new();
    let mut search = 0;
    while let Some(position) = find(&bytes[search..], b"/Author") {
        let rest = &bytes[search + position + b"/Author".len()..];
        let value = rest.iter().skip_while(|byte| byte.is_ascii_whitespace());
        let mut value = value.copied();
        let filled = match value.next() {
            Some(b'(') => value.next().is_some_and(|byte| byte != b')'),
            Some(b'<') => value.next().is_some_and(|byte| byte != b'>'),
            _ => false,
        };
        if filled {
            found.push("Author");
            break;
        }
        search += position + 1;
    }
    if find(bytes, b"<dc:creator>").is_some() {
        found.push("dc:creator");
    }
    found
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes(path: &str, text: &str) -> Vec<(Class, String)> {
        scan_file(path, text.as_bytes())
            .into_iter()
            .map(|finding| (finding.class, finding.value))
            .collect()
    }

    fn has(path: &str, text: &str, class: Class) -> bool {
        scan_file(path, text.as_bytes())
            .iter()
            .any(|finding| finding.class == class)
    }

    // Credential-shaped samples are assembled at run time so neither this
    // file nor the repository's secret scanner carries one whole.

    #[test]
    fn contact_diagnostics_do_not_echo_detected_values() {
        let value = ["private", "person", "@", "mail", ".com"].concat();
        let finding = Finding {
            path: "fixture.md".into(),
            line: 1,
            class: Class::Email,
            value: value.clone(),
            detail: "personal contact".into(),
        };
        assert!(!finding.message().contains(&value));
    }

    #[test]
    fn a_private_key_block_is_a_credential() {
        let body = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7".repeat(2);
        let text = format!("{}\n{body}\n", concat!("-----BEGIN ", "PRIVATE KEY-----"));
        let findings = scan_file("notes/key.md", text.as_bytes());
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].class, Class::Credential);
        assert!(!findings[0].message().contains(&body));
    }

    #[test]
    fn a_documented_private_key_header_with_no_body_is_not_a_credential() {
        let text = concat!("  -----BEGIN RSA ", "PRIVATE KEY-----\n  ...\n");
        assert!(!has("docs/setup.md", text, Class::Credential));
    }

    #[test]
    fn cloud_and_api_token_shapes_are_credentials() {
        let samples = [
            format!("key = {}{}", "AKIA", "Q7ZD4MPLWX2KR9TB"),
            format!("token: {}{}", "ghp_", "a".repeat(36)),
            format!("{}{}", "xoxb-", "1234567890-abcdefghij"),
            format!("{}{}", "sk_live_", "4eC39HqLyjWDarjtT1zdp7dc"),
            format!("{}{}", "AIza", "SyD-9tSrke72PouQMnMX-a7eZSW0jkFMBWY"),
            format!(
                r#""private_key_id": "{}""#,
                "0123456789abcdef".repeat(2) + "01234567"
            ),
        ];
        for sample in samples {
            assert!(has("src/config.rs", &sample, Class::Credential), "{sample}");
        }
    }

    #[test]
    fn a_url_with_a_real_password_is_a_credential_and_a_local_one_is_not() {
        let real = format!("postgres://app:{}@db.internal.neonlaw.com/app", "Zq81vLx0");
        assert!(has("src/db.rs", &real, Class::Credential));
        assert!(!has(
            "src/db.rs",
            "postgres://postgres:postgres@localhost:5432/app",
            Class::Credential
        ));
        assert!(!has(
            "src/db.rs",
            "https://user:token@www.example.com/page",
            Class::Credential
        ));
    }

    #[test]
    fn an_address_outside_the_reserved_domains_is_an_email() {
        let address = format!("{}@{}", "intake", "invented-firm.biz");
        assert_eq!(
            classes("docs/contact.md", &format!("Write to {address}.")),
            vec![(Class::Email, address)]
        );
    }

    #[test]
    fn reserved_domains_and_git_transport_users_are_not_emails() {
        let text = "a@example.com b@mail.example.org c@client.test d@host.invalid \
                    e@site.example f@localhost git@github.com:neon-law/navigator";
        assert!(!has("docs/contact.md", text, Class::Email));
    }

    #[test]
    fn a_dialable_number_is_a_phone_and_a_fictional_one_is_not() {
        let number = format!("+1 (415) {}-{}", "823", "4410");
        assert_eq!(
            classes("docs/contact.md", &format!("Call {number}.")),
            vec![(Class::Phone, "4158234410".to_string())]
        );
        assert!(!has("docs/contact.md", "Call 702-555-0134.", Class::Phone));
        let compact = format!("NAVIGATOR_SMS_NUMBER = \"+1{}{}\"", "415823", "4410");
        assert_eq!(
            classes("deploy/config.toml", &compact),
            vec![(Class::Phone, "4158234410".to_string())]
        );
        assert!(!has("src/sms.rs", "from: \"+15555550100\"", Class::Phone));
    }

    #[test]
    fn digit_runs_that_are_not_phone_numbers_are_ignored() {
        let text = "for w in 400 800 1200; do\nd=\"M-2.228.408.5608\"\nv 26.10.19.1234\n";
        assert!(!has("docs/assets.md", text, Class::Phone));
    }

    #[test]
    fn cloud_coordinates_are_gcp_findings() {
        let samples = [
            (
                "web@acme-prod-4821.iam.gserviceaccount.com",
                "web@acme-prod-4821.iam.gserviceaccount.com",
            ),
            ("gs://acme-prod-archive/x", "acme-prod-archive"),
            ("projects/acme-prod-4821/secrets/db", "acme-prod-4821"),
            (
                "gcloud run deploy --project acme-prod-4821",
                "acme-prod-4821",
            ),
            ("CLOUDSDK_CORE_PROJECT=acme-prod-4821", "acme-prod-4821"),
            (
                "NAVIGATOR_DOCUMENTS_BUCKET = \"acme-prod-documents\"",
                "acme-prod-documents",
            ),
            ("NAVIGATOR_GCP_PROJECT_ID: acme-prod-4821", "acme-prod-4821"),
            (
                "projects/918273645/locations/global/workloadIdentityPools/gh/providers/repo",
                "projects/918273645/locations/global/workloadIdentityPools/gh/providers/repo",
            ),
            (
                "us-west4-docker.pkg.dev/acme-prod-4821/images/web",
                "acme-prod-4821",
            ),
        ];
        for (text, value) in samples {
            assert!(
                classes("deploy/run.sh", text).contains(&(Class::Gcp, value.to_string())),
                "{text}: {:?}",
                classes("deploy/run.sh", text)
            );
        }
    }

    #[test]
    fn a_service_account_address_is_not_also_an_email() {
        assert!(!has(
            "deploy/run.sh",
            "web@acme-prod-4821.iam.gserviceaccount.com",
            Class::Email
        ));
    }

    #[test]
    fn placeholder_cloud_coordinates_are_not_findings() {
        let text = "gs://example-bucket projects/my-project/secrets/x \
                    svc@example-deployment.iam.gserviceaccount.com --project=$PROJECT \
                    gcr.io/distroless/cc NAVIGATOR_DOCUMENTS_BUCKET=sample-documents";
        assert!(
            !has("docs/deploy.md", text, Class::Gcp),
            "{:?}",
            classes("docs/deploy.md", text)
        );
    }

    #[test]
    fn env_files_and_credential_file_names_are_env_findings() {
        for path in [
            ".env",
            "app/.env.production",
            "deploy/key.pem",
            "ci/credentials.json",
        ] {
            assert!(has(path, "X=1\n", Class::Env), "{path}");
        }
        assert!(!has(".env.example", "X=\n", Class::Env));
    }

    #[test]
    fn a_sops_document_with_a_plaintext_value_is_an_env_finding() {
        let sealed = "API_TOKEN: ENC[AES256_GCM,data:abc,type:str]\nsops:\n  version: 3.9.0\n";
        assert!(!has("deploy/secrets.enc.yaml", sealed, Class::Env));
        let open = "API_TOKEN: plaintext-value\nsops:\n  version: 3.9.0\n";
        let findings = scan_file("deploy/secrets.enc.yaml", open.as_bytes());
        assert!(
            findings
                .iter()
                .any(|finding| finding.class == Class::Env && finding.line == 1),
            "{findings:?}"
        );
    }

    #[test]
    fn generated_build_output_is_a_build_finding() {
        for path in [
            "target/debug/navigator.d",
            "web/node_modules/x/index.js",
            "portal/dist/index.html",
            "scripts/__pycache__/scan.cpython-312.pyc",
            "docs/.DS_Store",
            "lcov.info",
        ] {
            assert!(has(path, "x", Class::Build), "{path}");
        }
        assert!(!has("distributions/readme.md", "x", Class::Build));
    }

    fn png(chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        for (kind, data) in chunks {
            bytes.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
            bytes.extend_from_slice(*kind);
            bytes.extend_from_slice(data);
            bytes.extend_from_slice(&[0, 0, 0, 0]);
        }
        bytes
    }

    #[test]
    fn png_text_and_exif_chunks_are_binary_metadata() {
        let tagged = png(&[
            (b"IHDR", &[0; 13]),
            (b"tEXt", b"Author\0Jane"),
            (b"IEND", b""),
        ]);
        let findings = scan_file("public/logo.png", &tagged);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].class, Class::BinaryMetadata);
        assert_eq!(findings[0].value, "tEXt");
        let clean = png(&[(b"IHDR", &[0; 13]), (b"IDAT", &[0; 4]), (b"IEND", b"")]);
        assert!(scan_file("public/logo.png", &clean).is_empty());
    }

    #[test]
    fn jpeg_exif_is_binary_metadata() {
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x08];
        jpeg.extend_from_slice(b"Exif\0\0");
        jpeg.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02, 0x00]);
        let findings = scan_file("public/photo.jpg", &jpeg);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].class, Class::BinaryMetadata);
    }

    #[test]
    fn a_pdf_author_is_binary_metadata() {
        let pdf = b"%PDF-1.7\n\0\n1 0 obj << /Author (Jane Roe) /Producer (navigator) >> endobj\n";
        let findings = scan_file("testdata/brief.pdf", pdf);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].class, Class::BinaryMetadata);
        assert!(scan_file("testdata/brief.pdf", b"%PDF-1.7\n\0\n/Author ()\n").is_empty());
    }

    #[test]
    fn an_unknown_binary_format_is_a_binary_finding() {
        assert!(has("tools/blob.bin", "\0\x01\x02", Class::Binary));
        assert!(!has("fonts/serif.woff2", "\0\x01\x02", Class::Binary));
    }

    #[test]
    fn an_allowlist_entry_needs_a_reason_and_a_scope() {
        assert!(Allowlist::parse(
            "[[allow]]\nclass = \"email\"\nmatch = \"a@b\\\\.c\"\nreason = \"\"\n"
        )
        .is_err());
        assert!(Allowlist::parse("[[allow]]\nclass = \"email\"\nreason = \"why\"\n").is_err());
        assert!(Allowlist::parse(
            "[[allow]]\nclass = \"nope\"\npaths = [\"x\"]\nreason = \"why\"\n"
        )
        .is_err());
    }

    #[test]
    fn an_allowlist_entry_permits_only_its_class_paths_and_match() {
        let allowlist = Allowlist::parse(
            "[[allow]]\nclass = \"email\"\nmatch = '''[a-z]+@firm\\.example'''\npaths = [\"docs/\"]\nreason = \"firm\"\n",
        )
        .unwrap();
        let finding = |path: &str, class: Class, value: &str| Finding {
            path: path.to_string(),
            line: 1,
            class,
            value: value.to_string(),
            detail: String::new(),
        };
        assert!(allowlist
            .permitting(&finding("docs/a.md", Class::Email, "intake@firm.example"))
            .is_some());
        assert!(allowlist
            .permitting(&finding("src/a.rs", Class::Email, "intake@firm.example"))
            .is_none());
        assert!(allowlist
            .permitting(&finding(
                "docs/a.md",
                Class::Email,
                "intake@firm.example.invalid"
            ))
            .is_none());
        assert!(allowlist
            .permitting(&finding("docs/a.md", Class::Phone, "intake@firm.example"))
            .is_none());
    }

    #[test]
    fn this_repository_carries_the_allowlist_and_audits_clean() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let audit = audit_tree(&root)
            .unwrap()
            .expect("the repository root carries the public snapshot allowlist");
        let findings: Vec<String> = audit
            .findings
            .iter()
            .map(|finding| format!("{}:{} {}", finding.path, finding.line, finding.message()))
            .collect();
        assert!(findings.is_empty(), "{}", findings.join("\n"));
    }

    #[test]
    fn a_tree_without_the_allowlist_is_not_audited() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "TOKEN=x\n").unwrap();
        assert!(audit_tree(dir.path()).unwrap().is_none());
    }

    #[test]
    fn a_tree_with_the_allowlist_reports_unpermitted_findings_and_unused_entries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".github/public-snapshot")).unwrap();
        std::fs::write(
            dir.path().join(ALLOWLIST),
            "[[allow]]\nclass = \"env\"\npaths = [\"gone/.env\"]\nreason = \"left over\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join(".env"), "TOKEN=x\n").unwrap();
        let audit = audit_tree(dir.path()).unwrap().unwrap();
        assert_eq!(audit.files_scanned, 1);
        assert_eq!(audit.findings.len(), 1, "{:?}", audit.findings);
        assert_eq!(audit.findings[0].path, ".env");
        assert_eq!(audit.unused_entries.len(), 1);
    }
}
