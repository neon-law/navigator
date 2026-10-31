//! Project-repository `navigator.yaml` — one accepted key set, one reader.
//!
//! `navigator project gate` and `navigator project drift` share this list so
//! a key one side honours cannot be a key the other refuses. Unknown keys are
//! refused by name. There is no per-repository exemption key.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The filename a Project repository declares itself in.
pub const FILE: &str = "navigator.yaml";
/// The retired `.yml` spelling. A file of this name is a rename, not a second
/// schema.
pub const RETIRED_FILE: &str = "navigator.yml";

/// `Y004` — `host` must be a hostname.
pub const HOST_CODE: &str = "Y004";
/// `Y005` — `project` must be a Navigator Project code.
pub const PROJECT_CODE: &str = "Y005";
/// `Y006` — a top-level key outside the accepted set.
pub const UNKNOWN_KEY_CODE: &str = "Y006";
/// `Y007` — `no_live_row` must be a non-empty reason string.
pub const ROWLESS_CODE: &str = "Y007";
/// `Y008` — the manifest is `navigator.yaml`, not `navigator.yml`.
pub const RENAME_CODE: &str = "Y008";
/// `Y011` — a Project manifest must not carry YAML comment tokens.
pub const COMMENT_CODE: &str = "Y011";
/// `Y012` — `version` must be an exact release tag.
pub const VERSION_CODE: &str = "Y012";
/// `Y013` — the flat manifest shape is deprecated.
pub const DEPRECATED_CODE: &str = "Y013";
/// `Y015` — a `skills:` entry must carry `jurisdiction`, `practice_area`, and `version`.
pub const SKILLS_CODE: &str = "Y015";
/// `Y016` — required Project coordinates must be present and populated.
pub const HANDLE_CODE: &str = "Y016";
/// `Y020` — `dockets:` must be a list of non-empty case numbers.
pub const DOCKETS_CODE: &str = "Y020";
/// `Y021` — `pages:` must be a list of well-shaped entries with unique,
/// URL-safe slugs.
pub const PAGES_CODE: &str = "Y021";
/// `Y023` — a section declared in `pages:` reads a data source the live
/// matter holds no rows of. A warning: a new matter's empty state is valid.
/// The live half of `navigator project gate` reports it
/// (`projects::gate::section_data_findings`).
pub const SECTION_DATA_CODE: &str = "Y023";

/// Every top-level key `navigator.yaml` may carry.
///
/// Drift's reader deserializes this same set (and only this set of *named*
/// fields). Adding a key means adding it here, in [`Manifest`], and in the
/// covering test that asserts the two agree.
pub const ACCEPTED_KEYS: &[&str] = &[
    "allowed_hosts",
    "allowed_links",
    "allowed_prefixes",
    "dockets",
    "host",
    "matter",
    "no_live_row",
    "pages",
    "project",
    "skills",
    "version",
];

const PROJECT_KEYS: &[&str] = &[
    "calendar_id",
    "client_dri",
    "drive_folder_id",
    "host",
    "lawyer_dri",
    "name",
    "private_notion_page",
    "private_slack_channel",
    "shared_notion_page",
    "shared_slack_channel",
    "xero_customer",
];

const HANDLE_KEYS: &[&str] = &[
    "lawyer_dri",
    "client_dri",
    "private_slack_channel",
    "private_notion_page",
    "shared_slack_channel",
    "shared_notion_page",
    "xero_customer",
    "calendar_id",
    "drive_folder_id",
];

const REQUIRED_HANDLE_KEYS: &[&str] = &[
    "lawyer_dri",
    "client_dri",
    "private_slack_channel",
    "private_notion_page",
];

/// The literal `project.private_slack_channel`/`project.private_notion_page`
/// value a manifest writes when the Project genuinely has no live channel or
/// page yet — a real, common resting state, not an oversight (Slack and
/// Notion are optional integrations; not every matter runs either one).
///
/// This is not a placeholder: [`is_placeholder`] catches "someone forgot to
/// fill this in" (`todo`, `<INSERT ...>`); this sentinel means "there is
/// nothing to fill in." `Y016` (`lint_handle`) accepts it as a valid shape
/// for these two keys only — `lawyer_dri`/`client_dri` stay a hard
/// requirement, because every matter has both.
///
/// Accepting the word alone would let a manifest claim "not provisioned"
/// forever, even after Slack or Notion is set up live, so nothing here
/// checks it against reality — that is
/// `cli::projects::gate::check_live_handles`'s job. Its
/// `handle_matches_live` only accepts this sentinel when the live
/// coordinate is actually absent, so the live gate — not this offline
/// lint — is what keeps the claim honest.
pub const NOT_PROVISIONED: &str = "not provisioned";

/// Whether `value` is the [`NOT_PROVISIONED`] sentinel, ignoring case and
/// surrounding whitespace the way [`is_placeholder`] does for its own set of
/// words.
#[must_use]
pub fn is_not_provisioned(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case(NOT_PROVISIONED)
}

/// One `skills:` entry — a Project Skill catalog `(jurisdiction,
/// practice_area)` pinned at a specific `version`, recorded by `navigator
/// project skill use` and read back by `navigator project skill status` and
/// `navigator project gate --check`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillPin {
    pub jurisdiction: String,
    pub practice_area: String,
    pub version: String,
}

/// A Project repository's root manifest.
///
/// Unknown keys are not stored: [`lint`] refuses them before a caller reads
/// this struct. The `no_live_row` field stays untyped so `true` cannot coerce
/// into a plausible reason.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct Manifest {
    pub version: Option<String>,
    pub host: Option<String>,
    pub project: Option<String>,
    pub lawyer_dri: Option<String>,
    pub client_dri: Option<String>,
    pub private_slack_channel: Option<String>,
    pub private_notion_page: Option<String>,
    pub shared_slack_channel: Option<String>,
    pub shared_notion_page: Option<String>,
    pub xero_customer: Option<String>,
    /// The Project's shared Google Calendar id. Optional: `navigator project
    /// setup` creates the calendar and `navigator project set --calendar`
    /// records it here.
    pub calendar_id: Option<String>,
    /// The Project's Drive ingest folder id. Optional: `navigator project
    /// setup` creates or adopts the folder and `navigator project set
    /// --drive-folder` records it here.
    pub drive_folder_id: Option<String>,
    pub no_live_row: Option<serde_yaml::Value>,
    pub allowed_hosts: BTreeMap<String, String>,
    pub allowed_links: BTreeMap<String, String>,
    pub allowed_prefixes: BTreeMap<String, String>,
    pub skills: Vec<SkillPin>,
    /// The case numbers whose court notices belong to this Project, read by
    /// `navigator project docket ingest`.
    pub dockets: Vec<String>,
    /// The matter pages this Project declares, in order. `navigator project
    /// sync` stores them with the Project, and the matter page renders them.
    pub pages: Vec<rules::pages::PageEntry>,
}

/// One manifest finding, with a Y-family rule code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestFinding {
    pub path: PathBuf,
    pub line: usize,
    pub code: &'static str,
    pub message: String,
    pub warning: bool,
}

impl ManifestFinding {
    pub(crate) fn at(
        path: &Path,
        line: usize,
        code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            path: path.to_path_buf(),
            line,
            code,
            message: message.into(),
            warning: false,
        }
    }

    fn warning(path: &Path, line: usize, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            path: path.to_path_buf(),
            line,
            code,
            message: message.into(),
            warning: true,
        }
    }
}

/// Lint the Project manifest at `root`, if one is present.
///
/// A missing `navigator.yaml` is not a finding here — layout reports that
/// when the tree is a Project repository. A root `navigator.yml` is always
/// a rename finding.
#[must_use]
pub fn lint(root: &Path) -> Vec<ManifestFinding> {
    let mut findings = Vec::new();
    let retired = root.join(RETIRED_FILE);
    if retired.is_file() {
        findings.push(ManifestFinding::at(
            &retired,
            1,
            RENAME_CODE,
            "the manifest is navigator.yaml, rename it",
        ));
    }
    let path = root.join(FILE);
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return findings;
    };
    findings.extend(lint_contents(&path, &contents));
    findings.extend(lint_draft_targets(root, &path, &contents));
    findings
}

/// Resolve draft references against the working tree and Git index. Shape-only
/// consumers can validate unsaved manifests without a repository via `lint_contents`.
fn lint_draft_targets(root: &Path, path: &Path, contents: &str) -> Vec<ManifestFinding> {
    let Ok(manifest) = parse(contents) else {
        return Vec::new();
    };
    manifest.pages.iter().filter_map(|page| {
        let draft = page.draft.as_deref()?;
        let relative = Path::new(draft);
        let mut components = relative.components();
        let admitted = matches!(components.next(), Some(std::path::Component::Normal(name))
            if name == "drafts" || name == "templates")
            && components.clone().next().is_some()
            && components.all(|part| matches!(part, std::path::Component::Normal(_)))
            && !draft.contains('\\');
        let target = root.join(relative);
        let within_root = root.canonicalize().ok().zip(target.canonicalize().ok())
            .is_some_and(|(root, target)| {
                target.starts_with(root.join("drafts")) || target.starts_with(root.join("templates"))
            });
        let tracked = admitted && std::process::Command::new("git")
            .args(["--literal-pathspecs", "ls-files", "--error-unmatch", "--", draft])
            .current_dir(root)
            .output()
            .is_ok_and(|output| output.status.success());
        let regular_file = target.symlink_metadata().is_ok_and(|metadata| metadata.is_file());
        if admitted && regular_file && within_root && tracked {
            return None;
        }
        let line = rules::pages::declared_pages(contents).iter()
            .find(|declared| declared.slug == page.slug)
            .map_or(1, |declared| declared.line_of(contents, "draft"));
        Some(ManifestFinding::at(path, line, "Y026", format!(
            "`pages:` draft `{draft}` must resolve to an existing tracked regular file under drafts/ or templates/"
        )))
    }).collect()
}

/// YAML comment tokens, not a `#` grep: quoted and block scalars keep their
/// hashes as content. Reasons belong on the pull request and in the contract.
fn lint_comments(path: &Path, contents: &str) -> Vec<ManifestFinding> {
    yaml_comment_lines(contents)
        .into_iter()
        .map(|line| {
            ManifestFinding::at(
                path,
                line,
                COMMENT_CODE,
                "navigator.yaml must not contain comments; record the reason \
                 in the pull request that adds the entry and in the repository \
                 contract",
            )
        })
        .collect()
}

/// 1-based lines that a YAML scanner treats as comments.
fn yaml_comment_lines(contents: &str) -> Vec<usize> {
    let contents = contents.strip_prefix('\u{feff}').unwrap_or(contents);
    let bytes = contents.as_bytes();
    let mut i = 0;
    let mut line = 1;
    let mut lines = Vec::new();
    while i < bytes.len() {
        match bytes[i] {
            b'\'' if is_quote_start(bytes, i) => skip_single_quoted(bytes, &mut i, &mut line),
            b'"' if is_quote_start(bytes, i) => skip_double_quoted(bytes, &mut i, &mut line),
            b'|' | b'>' if is_block_scalar_header(bytes, i) => {
                if let Some(comment_line) = comment_on_rest_of_line(bytes, i, line) {
                    lines.push(comment_line);
                }
                skip_block_scalar(bytes, &mut i, &mut line);
            }
            b'#' if is_comment_start(bytes, i) => {
                lines.push(line);
                skip_to_eol(bytes, &mut i, &mut line);
            }
            b'\n' => {
                line += 1;
                i += 1;
            }
            b'\r' => {
                i += 1;
                if i < bytes.len() && bytes[i] == b'\n' {
                    i += 1;
                }
                line += 1;
            }
            _ => i += 1,
        }
    }
    lines
}

fn is_comment_start(bytes: &[u8], i: usize) -> bool {
    i == 0
        || bytes[i - 1] == b' '
        || bytes[i - 1] == b'\t'
        || bytes[i - 1] == b'\n'
        || bytes[i - 1] == b'\r'
}

/// A quote opens a scalar only at a token boundary. An apostrophe inside a
/// plain value (`the author's closed file`) is content; treating it as a
/// quote start would skip the rest of the document and hide a later comment.
fn is_quote_start(bytes: &[u8], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    matches!(
        bytes[i - 1],
        b':' | b'[' | b'{' | b',' | b' ' | b'\t' | b'\n' | b'\r'
    )
}

fn comment_on_rest_of_line(bytes: &[u8], mut i: usize, line: usize) -> Option<usize> {
    while i < bytes.len() {
        match bytes[i] {
            b'\n' | b'\r' => return None,
            b'#' if is_comment_start(bytes, i) => return Some(line),
            _ => i += 1,
        }
    }
    None
}

fn is_block_scalar_header(bytes: &[u8], i: usize) -> bool {
    let Ok(rest) = std::str::from_utf8(&bytes[i..]) else {
        return false;
    };
    let line = rest
        .split('\n')
        .next()
        .unwrap_or(rest)
        .trim_end_matches('\r');
    let mut chars = line.chars();
    match chars.next() {
        Some('|' | '>') => {}
        _ => return false,
    }
    for ch in chars.by_ref() {
        match ch {
            '0'..='9' | '+' | '-' | ' ' | '\t' => {}
            '#' => return true,
            _ => return false,
        }
    }
    true
}

fn skip_to_eol(bytes: &[u8], i: &mut usize, line: &mut usize) {
    while *i < bytes.len() {
        match bytes[*i] {
            b'\n' => {
                *line += 1;
                *i += 1;
                return;
            }
            b'\r' => {
                *i += 1;
                if *i < bytes.len() && bytes[*i] == b'\n' {
                    *i += 1;
                }
                *line += 1;
                return;
            }
            _ => *i += 1,
        }
    }
}

fn skip_single_quoted(bytes: &[u8], i: &mut usize, line: &mut usize) {
    *i += 1;
    while *i < bytes.len() {
        match bytes[*i] {
            b'\'' => {
                *i += 1;
                if *i < bytes.len() && bytes[*i] == b'\'' {
                    *i += 1;
                    continue;
                }
                return;
            }
            b'\n' => {
                *line += 1;
                *i += 1;
            }
            b'\r' => {
                *i += 1;
                if *i < bytes.len() && bytes[*i] == b'\n' {
                    *i += 1;
                }
                *line += 1;
            }
            _ => *i += 1,
        }
    }
}

fn skip_double_quoted(bytes: &[u8], i: &mut usize, line: &mut usize) {
    *i += 1;
    while *i < bytes.len() {
        match bytes[*i] {
            b'\\' => {
                *i += 1;
                if *i < bytes.len() {
                    if bytes[*i] == b'\n' {
                        *line += 1;
                    } else if bytes[*i] == b'\r' {
                        *i += 1;
                        if *i < bytes.len() && bytes[*i] == b'\n' {
                            *i += 1;
                        }
                        *line += 1;
                        continue;
                    }
                    *i += 1;
                }
            }
            b'"' => {
                *i += 1;
                return;
            }
            b'\n' => {
                *line += 1;
                *i += 1;
            }
            b'\r' => {
                *i += 1;
                if *i < bytes.len() && bytes[*i] == b'\n' {
                    *i += 1;
                }
                *line += 1;
            }
            _ => *i += 1,
        }
    }
}

fn skip_block_scalar(bytes: &[u8], i: &mut usize, line: &mut usize) {
    skip_to_eol(bytes, i, line);
    let mut content_indent: Option<usize> = None;
    while *i < bytes.len() {
        let start = *i;
        let indent = leading_indent(bytes, *i);
        if !line_is_blank(bytes, *i) {
            match content_indent {
                None => content_indent = Some(indent),
                Some(min) if indent < min => {
                    *i = start;
                    return;
                }
                Some(_) => {}
            }
        }
        skip_to_eol(bytes, i, line);
    }
}

fn leading_indent(bytes: &[u8], mut i: usize) -> usize {
    let mut n = 0;
    while i < bytes.len() {
        match bytes[i] {
            b' ' | b'\t' => {
                n += 1;
                i += 1;
            }
            _ => break,
        }
    }
    n
}

fn line_is_blank(bytes: &[u8], mut i: usize) -> bool {
    while i < bytes.len() {
        match bytes[i] {
            b' ' | b'\t' => i += 1,
            b'\n' | b'\r' => return true,
            _ => return false,
        }
    }
    true
}

/// Lint already-read YAML. Used by tests and by [`lint`].
#[must_use]
pub fn lint_contents(path: &Path, contents: &str) -> Vec<ManifestFinding> {
    let mut findings = lint_comments(path, contents);
    let document: serde_yaml::Value = match serde_yaml::from_str(contents) {
        Ok(document) => document,
        Err(error) => {
            findings.push(ManifestFinding::at(
                path,
                1,
                UNKNOWN_KEY_CODE,
                format!("navigator.yaml is not valid YAML: {error}"),
            ));
            return findings;
        }
    };
    let Some(mapping) = document.as_mapping() else {
        findings.push(ManifestFinding::at(
            path,
            1,
            UNKNOWN_KEY_CODE,
            format!(
                "navigator.yaml must be a mapping of {}; got a non-mapping document",
                accepted_keys_phrase()
            ),
        ));
        return findings;
    };
    findings.extend(lint_keys(path, mapping));
    findings.extend(lint_version(path, mapping));
    match mapping.get("project") {
        Some(serde_yaml::Value::Mapping(project)) => {
            findings.extend(lint_nested_project(path, project));
        }
        Some(serde_yaml::Value::String(_)) => {
            if mapping.contains_key("version") {
                findings.push(ManifestFinding::at(
                    path,
                    1,
                    PROJECT_CODE,
                    "`project` must be a map with `host` and `name` when `version` is present",
                ));
            } else {
                findings.push(ManifestFinding::warning(
                    path,
                    1,
                    DEPRECATED_CODE,
                    "flat `host:`/`project:` is deprecated; use `version:` and `project.host`/`project.name`",
                ));
                findings.extend(lint_flat_host(path, mapping));
                findings.extend(lint_flat_project(path, mapping));
            }
        }
        Some(_) => findings.push(ManifestFinding::at(
            path,
            1,
            PROJECT_CODE,
            "`project` must be a Project code or a map with `host` and `name`",
        )),
        None => {
            findings.extend(lint_flat_host(path, mapping));
            findings.extend(lint_flat_project(path, mapping));
        }
    }
    findings.extend(lint_rowless(path, mapping));
    findings.extend(lint_skills(path, mapping));
    findings.extend(lint_dockets(path, mapping));
    findings.extend(lint_pages(path, contents));
    findings.extend(lint_matter(path, mapping));
    findings.extend(lint_allowlist_shapes(path, mapping));
    if let Ok(manifest) = parse(contents) {
        findings.extend(lint_allowlists(path, &manifest));
    }
    findings
}

/// `Y019` — a `matter:` block must name every field a pleading's caption needs.
fn lint_matter(path: &Path, mapping: &serde_yaml::Mapping) -> Vec<ManifestFinding> {
    let Some(block) = mapping.get(super::matter::KEY) else {
        return Vec::new();
    };
    let message = match serde_yaml::from_value::<word::matter::Matter>(block.clone()) {
        Ok(matter) => {
            let missing = matter.missing();
            if missing.is_empty() {
                return Vec::new();
            }
            format!("missing or malformed {}", missing.join(", "))
        }
        Err(error) => format!("`matter:` is malformed: {error}"),
    };
    vec![ManifestFinding::at(
        path,
        1,
        super::matter::CODE,
        format!("a pleading's matter is incomplete: {message}"),
    )]
}

/// `Y015` — each `skills:` entry must be a map carrying non-empty
/// `jurisdiction`, `practice_area`, and `version` text. Whether the pair
/// still resolves against the compiled-in catalog is a separate question —
/// `navigator project gate --check` (not this offline lint) answers it,
/// because only the CLI binary carries the catalog.
fn lint_skills(path: &Path, mapping: &serde_yaml::Mapping) -> Vec<ManifestFinding> {
    let Some(value) = mapping.get("skills") else {
        return Vec::new();
    };
    let Some(items) = value.as_sequence() else {
        return vec![ManifestFinding::at(
            path,
            1,
            SKILLS_CODE,
            "`skills:` must be a list of {jurisdiction, practice_area, version} entries",
        )];
    };
    let mut findings = Vec::new();
    for item in items {
        let Some(entry) = item.as_mapping() else {
            findings.push(ManifestFinding::at(
                path,
                1,
                SKILLS_CODE,
                "each `skills:` entry must be a map of jurisdiction, practice_area, and version",
            ));
            continue;
        };
        for key in ["jurisdiction", "practice_area", "version"] {
            if entry.get(key).and_then(scalar_string).is_none() {
                findings.push(ManifestFinding::at(
                    path,
                    1,
                    SKILLS_CODE,
                    format!("each `skills:` entry must carry a non-empty `{key}`"),
                ));
            }
        }
    }
    findings
}

/// `Y020` — `dockets:` is a list of non-empty case numbers.
fn lint_dockets(path: &Path, mapping: &serde_yaml::Mapping) -> Vec<ManifestFinding> {
    match parse_dockets(mapping) {
        Ok(_) => Vec::new(),
        Err(message) => vec![ManifestFinding::at(path, 1, DOCKETS_CODE, message)],
    }
}

/// `Y021` for the shape of `pages:`, then the D-family for each entry's
/// composition against its kind's skeleton — the same rules the LSP would
/// show, so a page and its validator cannot disagree.
fn lint_pages(path: &Path, contents: &str) -> Vec<ManifestFinding> {
    let mut findings: Vec<ManifestFinding> = rules::pages::shape_problems(contents)
        .into_iter()
        .map(|(line, message)| ManifestFinding::at(path, line, PAGES_CODE, message))
        .collect();
    let file = rules::SourceFile {
        path: path.to_path_buf(),
        contents: contents.to_string(),
    };
    findings.extend(rules::lint_pages(&file).into_iter().map(|violation| {
        ManifestFinding::at(path, violation.line, violation.code, violation.message)
    }));
    findings
}

/// Each allowlist is a map of `host: reason`. A list, or any other shape,
/// fails here rather than leaving every later reader of the manifest unable
/// to parse it.
fn lint_allowlist_shapes(path: &Path, mapping: &serde_yaml::Mapping) -> Vec<ManifestFinding> {
    ["allowed_hosts", "allowed_links", "allowed_prefixes"]
        .into_iter()
        .filter_map(|key| parse_reason_map(mapping, key).err())
        .map(|message| ManifestFinding::at(path, 1, UNKNOWN_KEY_CODE, message))
        .collect()
}

/// Allowlist map values are claims: each host or prefix needs a non-empty
/// reason. The maps themselves are what the origin scanner reads, keyed by
/// bare host for `allowed_hosts` and `allowed_links`.
fn lint_allowlists(path: &Path, manifest: &Manifest) -> Vec<ManifestFinding> {
    let mut findings = Vec::new();
    for (key, map) in [
        ("allowed_hosts", &manifest.allowed_hosts),
        ("allowed_links", &manifest.allowed_links),
    ] {
        for entry in map.keys() {
            if let Some(host) = url_host(entry) {
                findings.push(ManifestFinding::at(
                    path,
                    1,
                    UNKNOWN_KEY_CODE,
                    format!("`{key}` entry `{entry}` is a URL; list its host `{host}` instead"),
                ));
            }
        }
    }
    for (host, reason) in &manifest.allowed_hosts {
        if reason.trim().is_empty() {
            findings.push(ManifestFinding::at(
                path,
                1,
                UNKNOWN_KEY_CODE,
                format!("`allowed_hosts` entry `{host}` must give a reason"),
            ));
        }
    }
    for (prefix, reason) in &manifest.allowed_prefixes {
        if reason.trim().is_empty() {
            findings.push(ManifestFinding::at(
                path,
                1,
                UNKNOWN_KEY_CODE,
                format!("`allowed_prefixes` entry `{prefix}` must give a reason"),
            ));
        }
    }
    for (host, reason) in &manifest.allowed_links {
        if reason.trim().is_empty() {
            findings.push(ManifestFinding::at(
                path,
                1,
                UNKNOWN_KEY_CODE,
                format!("`allowed_links` entry `{host}` must give a reason"),
            ));
        }
    }
    let _ = manifest.host.as_deref();
    findings
}

fn lint_keys(path: &Path, mapping: &serde_yaml::Mapping) -> Vec<ManifestFinding> {
    let mut findings = Vec::new();
    for key in mapping.keys() {
        let Some(name) = key.as_str() else {
            findings.push(ManifestFinding::at(
                path,
                1,
                UNKNOWN_KEY_CODE,
                format!(
                    "navigator.yaml top-level keys must be strings; expected one of {}",
                    accepted_keys_phrase()
                ),
            ));
            continue;
        };
        if !ACCEPTED_KEYS.contains(&name) {
            findings.push(ManifestFinding::at(
                path,
                1,
                UNKNOWN_KEY_CODE,
                format!(
                    "unknown key `{name}`; expected one of {}",
                    accepted_keys_phrase()
                ),
            ));
        }
    }
    findings
}

fn lint_version(path: &Path, mapping: &serde_yaml::Mapping) -> Vec<ManifestFinding> {
    match mapping.get("version") {
        None => Vec::new(),
        Some(value) => match scalar_string(value) {
            Some(version) if crate::devx::registry::is_release_tag(&version) => Vec::new(),
            Some(version) => vec![ManifestFinding::at(
                path,
                1,
                VERSION_CODE,
                format!("version must be an exact release tag, not '{version}'."),
            )],
            None => vec![ManifestFinding::at(
                path,
                1,
                VERSION_CODE,
                "version must be an exact release tag, not ''.",
            )],
        },
    }
}

fn lint_flat_host(path: &Path, mapping: &serde_yaml::Mapping) -> Vec<ManifestFinding> {
    match mapping.get("host") {
        Some(host) => match host_string(host) {
            None => vec![ManifestFinding::at(
                path,
                1,
                HOST_CODE,
                "`host` must be a hostname",
            )],
            Some(host) if !is_hostname(&host) => vec![ManifestFinding::at(
                path,
                1,
                HOST_CODE,
                format!("`host: {host}` is not a hostname"),
            )],
            Some(_) => Vec::new(),
        },
        None => vec![ManifestFinding::at(
            path,
            1,
            HOST_CODE,
            "`host` is required and must be a hostname",
        )],
    }
}

fn lint_flat_project(path: &Path, mapping: &serde_yaml::Mapping) -> Vec<ManifestFinding> {
    match mapping.get("project").and_then(scalar_string) {
        Some(code) if store::projects::is_valid_code(&code) => Vec::new(),
        Some(code) => vec![ManifestFinding::at(
            path,
            1,
            PROJECT_CODE,
            format!("`project: {code}` is not a valid Project code"),
        )],
        None if mapping.contains_key("project") => vec![ManifestFinding::at(
            path,
            1,
            PROJECT_CODE,
            "`project` must be a Project code",
        )],
        None => vec![ManifestFinding::at(
            path,
            1,
            PROJECT_CODE,
            "`project` is required and must be a valid Project code",
        )],
    }
}

fn lint_nested_project(path: &Path, project: &serde_yaml::Mapping) -> Vec<ManifestFinding> {
    let mut findings = Vec::new();
    for key in project.keys() {
        let Some(name) = key.as_str() else {
            findings.push(ManifestFinding::at(
                path,
                1,
                UNKNOWN_KEY_CODE,
                "navigator.yaml project-map keys must be strings",
            ));
            continue;
        };
        if !PROJECT_KEYS.contains(&name) {
            findings.push(ManifestFinding::at(
                path,
                1,
                UNKNOWN_KEY_CODE,
                format!(
                    "unknown project key `{name}`; expected one of {}",
                    PROJECT_KEYS.join(", ")
                ),
            ));
        }
    }

    let Some(host) = project.get("host").and_then(scalar_string) else {
        findings.push(ManifestFinding::at(
            path,
            1,
            HOST_CODE,
            "`project.host` is required and must be a hostname",
        ));
        return findings;
    };
    if !is_hostname(&host) {
        findings.push(ManifestFinding::at(
            path,
            1,
            HOST_CODE,
            format!("`project.host: {host}` is not a hostname"),
        ));
    }

    match project.get("name").and_then(scalar_string) {
        Some(name) if store::projects::is_valid_code(&name) => {}
        Some(name) => findings.push(ManifestFinding::at(
            path,
            1,
            PROJECT_CODE,
            format!("`project.name: {name}` is not a valid Project code"),
        )),
        None => findings.push(ManifestFinding::at(
            path,
            1,
            PROJECT_CODE,
            "`project.name` is required and must be a Project code",
        )),
    }

    for key in REQUIRED_HANDLE_KEYS {
        if !project.contains_key(*key) {
            findings.push(ManifestFinding::at(
                path,
                1,
                HANDLE_CODE,
                format!(
                    "`project.{key}` is required; populate it with `navigator project set --{}`",
                    key.replace('_', "-")
                ),
            ));
        }
    }
    for key in HANDLE_KEYS {
        if let Some(value) = project.get(*key) {
            findings.extend(lint_handle(path, key, value));
        }
    }
    findings
}

fn lint_handle(path: &Path, key: &str, value: &serde_yaml::Value) -> Vec<ManifestFinding> {
    let Some(value) = scalar_string(value) else {
        return vec![ManifestFinding::at(
            path,
            1,
            HANDLE_CODE,
            format!("`project.{key}` must be non-empty text"),
        )];
    };
    if is_placeholder(&value) {
        return vec![ManifestFinding::at(
            path,
            1,
            HANDLE_CODE,
            format!(
                "`project.{key}` is a placeholder; populate it with `navigator project set --{}`",
                key.replace('_', "-")
            ),
        )];
    }
    if matches!(key, "private_slack_channel" | "private_notion_page") && is_not_provisioned(&value)
    {
        return Vec::new();
    }
    let valid = match key {
        "lawyer_dri" | "client_dri" => is_email(&value),
        "private_slack_channel"
        | "private_notion_page"
        | "shared_slack_channel"
        | "shared_notion_page" => is_https_url(&value),
        "xero_customer" => true,
        "calendar_id" => store::projects::is_valid_calendar_id(&value),
        "drive_folder_id" => store::projects::is_valid_drive_folder_id(&value),
        _ => false,
    };
    if valid {
        Vec::new()
    } else {
        vec![ManifestFinding::at(
            path,
            1,
            HANDLE_CODE,
            format!("`project.{key}` has the wrong shape"),
        )]
    }
}

fn is_placeholder(value: &str) -> bool {
    let value = value.trim();
    let normalized = value.to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "todo" | "tbd" | "placeholder" | "insert"
    ) || normalized.starts_with("insert ")
        || (value.starts_with('<') && value.ends_with('>'))
}

fn is_email(value: &str) -> bool {
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.is_empty()
        && !value.chars().any(char::is_whitespace)
        && domain.contains('.')
}

fn is_https_url(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https" && url.host_str().is_some_and(|host| !host.is_empty())
    })
}

fn lint_rowless(path: &Path, mapping: &serde_yaml::Mapping) -> Vec<ManifestFinding> {
    match mapping.get("no_live_row") {
        None => Vec::new(),
        Some(serde_yaml::Value::String(reason)) if !reason.trim().is_empty() => Vec::new(),
        Some(serde_yaml::Value::String(_)) => vec![ManifestFinding::at(
            path,
            1,
            ROWLESS_CODE,
            "`no_live_row:` must give the reason this repository has no live row",
        )],
        Some(_) => vec![ManifestFinding::at(
            path,
            1,
            ROWLESS_CODE,
            "`no_live_row:` must be the reason this repository has no live row, written as text",
        )],
    }
}

/// Parse a manifest that has already passed [`lint_contents`] (or a fixture
/// known to be well-shaped). Unknown keys are ignored here the way serde
/// default-denies nothing: callers that need the closed set call [`lint`].
/// Read and parse `navigator.yaml` from `dir`. `None` covers both a missing
/// file and one that fails to parse — callers that only want an optional
/// fallback (a default `--host` or Project code) treat both the same way,
/// the same as `project gate`'s own live-status check does.
pub fn read(dir: &Path) -> Option<Manifest> {
    let contents = std::fs::read_to_string(dir.join(FILE)).ok()?;
    parse(&contents).ok()
}

/// Trim a manifest field to `None` if it is missing or blank, so a
/// `host: ""` or all-whitespace value can never masquerade as declared.
pub fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub fn parse(contents: &str) -> Result<Manifest, String> {
    let document: serde_yaml::Value = serde_yaml::from_str(contents)
        .map_err(|error| format!("navigator.yaml is not valid YAML: {error}"))?;
    let mapping = document
        .as_mapping()
        .ok_or_else(|| "navigator.yaml must be a mapping".to_string())?;
    let version = rules::project_manifest::pinned_version_in(mapping)?;
    let (host, project, project_map) = match mapping.get("project") {
        Some(serde_yaml::Value::Mapping(project)) => (
            Some(required_text(project, "host")?),
            Some(required_text(project, "name")?),
            Some(project),
        ),
        Some(value) => (
            mapping.get("host").map(parse_text).transpose()?,
            Some(required_text_value(value, "project")?),
            None,
        ),
        None => (mapping.get("host").map(parse_text).transpose()?, None, None),
    };
    let values = |key: &str| {
        project_map
            .and_then(|project| project.get(key))
            .map(parse_text)
            .transpose()
    };
    Ok(Manifest {
        version,
        host,
        project,
        lawyer_dri: values("lawyer_dri")?,
        client_dri: values("client_dri")?,
        private_slack_channel: values("private_slack_channel")?,
        private_notion_page: values("private_notion_page")?,
        shared_slack_channel: values("shared_slack_channel")?,
        shared_notion_page: values("shared_notion_page")?,
        xero_customer: values("xero_customer")?,
        calendar_id: values("calendar_id")?,
        drive_folder_id: values("drive_folder_id")?,
        no_live_row: mapping.get("no_live_row").cloned(),
        allowed_hosts: parse_reason_map(mapping, "allowed_hosts")?,
        allowed_links: parse_reason_map(mapping, "allowed_links")?,
        allowed_prefixes: parse_reason_map(mapping, "allowed_prefixes")?,
        skills: parse_skills(mapping)?,
        dockets: parse_dockets(mapping)?,
        pages: rules::pages::entries(mapping)?,
    })
}

fn parse_dockets(mapping: &serde_yaml::Mapping) -> Result<Vec<String>, String> {
    let Some(value) = mapping.get("dockets") else {
        return Ok(Vec::new());
    };
    value
        .as_sequence()
        .ok_or_else(|| "navigator.yaml dockets must be a list of case numbers".to_string())?
        .iter()
        .map(|item| {
            scalar_string(item)
                .map(|case| case.trim().to_string())
                .filter(|case| !case.is_empty())
                .ok_or_else(|| {
                    "each navigator.yaml dockets entry must be a case number".to_string()
                })
        })
        .collect()
}

fn parse_skills(mapping: &serde_yaml::Mapping) -> Result<Vec<SkillPin>, String> {
    let Some(value) = mapping.get("skills") else {
        return Ok(Vec::new());
    };
    let items = value
        .as_sequence()
        .ok_or_else(|| "navigator.yaml skills must be a list".to_string())?;
    items
        .iter()
        .map(|item| {
            let entry = item
                .as_mapping()
                .ok_or_else(|| "navigator.yaml skills entry must be a map".to_string())?;
            Ok(SkillPin {
                jurisdiction: required_text(entry, "jurisdiction")?,
                practice_area: required_text(entry, "practice_area")?,
                version: required_text(entry, "version")?,
            })
        })
        .collect()
}

/// Pin `(jurisdiction, practice_area)` at `version` in a `navigator.yaml`'s
/// `skills:` list, matching an existing entry case-insensitively. Returns
/// the rewritten contents and whether anything changed — a caller writes
/// the file only when it did, so re-pinning the same version is a byte-for-
/// byte no-op rather than a reformat.
///
/// # Errors
///
/// A string error if `contents` is not valid YAML, is not a mapping, or
/// already carries a non-list `skills:` key.
pub fn pin_skill(
    contents: &str,
    jurisdiction: &str,
    practice_area: &str,
    version: &str,
) -> Result<(String, bool), String> {
    let mut document: serde_yaml::Value = serde_yaml::from_str(contents)
        .map_err(|error| format!("navigator.yaml is not valid YAML: {error}"))?;
    let mapping = document
        .as_mapping_mut()
        .ok_or_else(|| "navigator.yaml must be a mapping".to_string())?;

    let mut skills: Vec<serde_yaml::Value> = match mapping.get("skills") {
        Some(serde_yaml::Value::Sequence(items)) => items.clone(),
        Some(_) => return Err("navigator.yaml `skills` must be a list".to_string()),
        None => Vec::new(),
    };

    let mut changed = false;
    let mut found = false;
    for item in &mut skills {
        let Some(entry) = item.as_mapping_mut() else {
            continue;
        };
        let entry_jurisdiction = entry
            .get("jurisdiction")
            .and_then(scalar_string)
            .unwrap_or_default();
        let entry_practice_area = entry
            .get("practice_area")
            .and_then(scalar_string)
            .unwrap_or_default();
        if entry_jurisdiction.eq_ignore_ascii_case(jurisdiction)
            && entry_practice_area.eq_ignore_ascii_case(practice_area)
        {
            found = true;
            let current_version = entry
                .get("version")
                .and_then(scalar_string)
                .unwrap_or_default();
            if current_version != version {
                entry.insert(
                    serde_yaml::Value::String("version".to_string()),
                    serde_yaml::Value::String(version.to_string()),
                );
                changed = true;
            }
        }
    }
    if !found {
        let mut entry = serde_yaml::Mapping::new();
        entry.insert(
            serde_yaml::Value::String("jurisdiction".to_string()),
            serde_yaml::Value::String(jurisdiction.to_string()),
        );
        entry.insert(
            serde_yaml::Value::String("practice_area".to_string()),
            serde_yaml::Value::String(practice_area.to_string()),
        );
        entry.insert(
            serde_yaml::Value::String("version".to_string()),
            serde_yaml::Value::String(version.to_string()),
        );
        skills.push(serde_yaml::Value::Mapping(entry));
        changed = true;
    }

    if !changed {
        return Ok((contents.to_string(), false));
    }
    mapping.insert(
        serde_yaml::Value::String("skills".to_string()),
        serde_yaml::Value::Sequence(skills),
    );
    let serialized = serde_yaml::to_string(&document)
        .map_err(|error| format!("serialize navigator.yaml: {error}"))?;
    Ok((serialized, true))
}

fn parse_text(value: &serde_yaml::Value) -> Result<String, String> {
    scalar_string(value).ok_or_else(|| "value must be non-empty text".to_string())
}

fn required_text(mapping: &serde_yaml::Mapping, key: &str) -> Result<String, String> {
    mapping
        .get(key)
        .ok_or_else(|| format!("navigator.yaml project.{key} is required"))
        .and_then(parse_text)
}

fn required_text_value(value: &serde_yaml::Value, key: &str) -> Result<String, String> {
    parse_text(value).map_err(|_| format!("navigator.yaml {key} must be non-empty text"))
}

fn parse_reason_map(
    mapping: &serde_yaml::Mapping,
    key: &str,
) -> Result<BTreeMap<String, String>, String> {
    mapping.get(key).map_or_else(
        || Ok(BTreeMap::new()),
        |value| {
            value
                .as_mapping()
                .ok_or_else(|| {
                    let entry = if key == "allowed_prefixes" {
                        "prefix"
                    } else {
                        "host"
                    };
                    format!("navigator.yaml {key} must be a map of {entry}: reason")
                })?
                .iter()
                .map(|(key, value)| {
                    let key = parse_text(key)?;
                    Ok((key, parse_text(value)?))
                })
                .collect()
        },
    )
}

/// The host of an allowlist entry written as a URL, or `None` for a bare host.
fn url_host(entry: &str) -> Option<String> {
    let parsed = url::Url::parse(entry).ok()?;
    parsed.host_str().map(str::to_string)
}

fn accepted_keys_phrase() -> String {
    ACCEPTED_KEYS.join(", ")
}

fn scalar_string(value: &serde_yaml::Value) -> Option<String> {
    value
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn host_string(value: &serde_yaml::Value) -> Option<String> {
    scalar_string(value)
}

/// A hostname: labels of letters, digits, and hyphens, joined by dots, with
/// no empty first (or any) label. No scheme, port, or path.
pub fn is_hostname(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    if host.starts_with('.') || host.ends_with('.') {
        return false;
    }
    if host.contains('/') || host.contains(':') || host.contains(' ') {
        return false;
    }
    host.split('.').all(is_dns_label)
}

fn is_dns_label(label: &str) -> bool {
    let bytes = label.as_bytes();
    if bytes.is_empty() || bytes.len() > 63 {
        return false;
    }
    let first = bytes[0];
    let last = bytes[bytes.len() - 1];
    if !first.is_ascii_alphanumeric() || !last.is_ascii_alphanumeric() {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn codes(contents: &str) -> Vec<&'static str> {
        lint_contents(Path::new("navigator.yaml"), contents)
            .into_iter()
            .map(|f| f.code)
            .collect()
    }

    #[test]
    fn accepted_keys_and_drift_reader_agree() {
        // Drift deserializes these named fields on `drift::Manifest`. Keep the
        // lists identical so a new key cannot land on one side only.
        const DRIFT_READER_KEYS: &[&str] = &[
            "allowed_hosts",
            "allowed_links",
            "allowed_prefixes",
            "dockets",
            "host",
            "matter",
            "no_live_row",
            "pages",
            "project",
            "skills",
            "version",
        ];
        assert_eq!(
            BTreeSet::from_iter(ACCEPTED_KEYS.iter().copied()),
            BTreeSet::from_iter(DRIFT_READER_KEYS.iter().copied())
        );
        let yaml = concat!(
            "host: staging.neonlaw.com\n",
            "project: acme\n",
            "no_live_row: the matter closed\n",
            "allowed_hosts:\n",
            "  www.w3.org: XML namespace\n",
            "allowed_links:\n",
            "  courts.example: civil-procedure citation\n",
            "allowed_prefixes:\n",
            "  \"https://react.dev/errors/\": React minified errors\n",
        );
        let parsed = parse(yaml).expect("accepted keys deserialize");
        assert_eq!(parsed.host.as_deref(), Some("staging.neonlaw.com"));
        assert_eq!(parsed.project.as_deref(), Some("acme"));
        assert_eq!(parsed.allowed_hosts.len(), 1);
        assert_eq!(parsed.allowed_links.len(), 1);
        assert_eq!(parsed.allowed_prefixes.len(), 1);
        assert_eq!(codes(yaml), vec![DEPRECATED_CODE]);
    }

    const NESTED: &str = "version: 26.10.6\nproject:\n  host: h.example.com\n  name: acme\n";

    /// A `pages:` fixture shared with the rules crate's acceptance test.
    fn pages_fixture(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../rules/tests/fixtures/pages")
            .join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    }

    fn pages_codes(name: &str) -> Vec<&'static str> {
        let body = pages_fixture(name);
        let yaml = if body.starts_with("pages:") {
            format!("{NESTED}{body}")
        } else {
            body
        };
        codes(&yaml)
            .into_iter()
            .filter(|code| *code == PAGES_CODE || code.starts_with('D'))
            .collect()
    }

    #[test]
    fn a_valid_pages_block_parses_and_lints_clean() {
        let manifest = parse(&pages_fixture("valid.yaml")).expect("pages deserialize");
        assert_eq!(manifest.pages.len(), 2);
        assert_eq!(manifest.pages[0].slug, "timeline");
        assert_eq!(
            manifest.pages[1].draft.as_deref(),
            Some("drafts/engagement.md")
        );
        assert!(pages_codes("valid.yaml").is_empty());
    }

    #[test]
    fn a_duplicate_slug_is_y021() {
        assert_eq!(pages_codes("y021_duplicate_slug.yaml"), [PAGES_CODE]);
    }

    #[test]
    fn page_compositions_surface_their_d_codes_through_the_manifest_pass() {
        assert_eq!(pages_codes("d002_out_of_catalog.yaml"), ["D002"]);
        assert_eq!(pages_codes("d003_missing_boundary_note.yaml"), ["D003"]);
        assert_eq!(pages_codes("d005_lawyer_only_on_client.yaml"), ["D005"]);
    }

    #[test]
    fn a_pages_finding_points_at_the_entry_line() {
        let yaml = format!(
            "{NESTED}pages:\n  - slug: Bad_Slug\n    kind: authority_library\n    title: T\n    \
             lenses:\n      lawyer: [authority_table, boundary_note, provenance_statement]\n"
        );
        let findings = lint_contents(Path::new("navigator.yaml"), &yaml);
        let finding = findings
            .iter()
            .find(|f| f.code == PAGES_CODE)
            .expect("a malformed slug is Y021");
        assert_eq!(finding.line, 6);
        assert!(!finding.warning);
    }

    #[test]
    fn a_listed_allowlist_fails_offline_with_its_shape() {
        let yaml = format!("{NESTED}allowed_links:\n  - https://www.example.com\n");
        let findings = lint_contents(Path::new("navigator.yaml"), &yaml);
        let shape = findings
            .iter()
            .find(|finding| finding.message.contains("allowed_links"))
            .expect("a listed allowed_links is a finding, not a silent skip");
        assert_eq!(shape.code, UNKNOWN_KEY_CODE);
        assert!(!shape.warning);
        assert!(shape.message.contains("host: reason"), "{}", shape.message);
    }

    #[test]
    fn an_allowlist_key_names_a_host_not_a_url() {
        let yaml = format!("{NESTED}allowed_links:\n  https://www.example.com: citation\n");
        let findings = lint_contents(Path::new("navigator.yaml"), &yaml);
        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("`www.example.com`")),
            "{findings:?}"
        );
        let host_only = format!("{NESTED}allowed_links:\n  www.example.com: citation\n");
        assert!(
            !codes(&host_only).contains(&UNKNOWN_KEY_CODE),
            "{:?}",
            codes(&host_only)
        );
    }

    #[test]
    fn dockets_is_a_list_of_case_numbers() {
        let parsed = parse("host: h.example.com\nproject: acme\ndockets:\n  - 9:26-cv-00042\n")
            .expect("a list of case numbers parses");
        assert_eq!(parsed.dockets, vec!["9:26-cv-00042".to_string()]);
        assert!(
            !codes("host: h.example.com\nproject: acme\ndockets:\n  - 26-10042\n")
                .contains(&DOCKETS_CODE)
        );
        for malformed in [
            "dockets: 26-10042\n",
            "dockets:\n  - \"\"\n",
            "dockets:\n  - [a]\n",
        ] {
            let yaml = format!("host: h.example.com\nproject: acme\n{malformed}");
            assert!(
                codes(&yaml).contains(&DOCKETS_CODE),
                "{malformed:?} must be Y020"
            );
        }
    }

    #[test]
    fn a_matter_block_is_checked_field_by_field() {
        let complete = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/synthetic_matter/navigator.yaml"),
        )
        .unwrap();
        assert!(!codes(&complete).contains(&"Y019"));
        let without_case = complete.replace("  case_number: 00CV000000\n", "");
        let findings = lint_contents(Path::new("navigator.yaml"), &without_case);
        let finding = findings.iter().find(|f| f.code == "Y019").unwrap();
        assert!(
            finding.message.contains("matter.case_number"),
            "{finding:?}"
        );
        let typo = complete.replace("case_number:", "case_numbr:");
        assert!(codes(&typo).contains(&"Y019"));
    }

    #[test]
    fn nested_manifest_reads_release_and_handles() {
        let yaml = concat!(
            "version: 26.9.14\n",
            "project:\n",
            "  host: staging.neonlaw.com\n",
            "  name: acme\n",
            "  lawyer_dri: lawyer@example.com\n",
            "  client_dri: client@example.com\n",
            "  private_slack_channel: https://slack.example.com/private\n",
            "  private_notion_page: https://notion.example.com/private\n",
            "  shared_slack_channel: https://slack.example.com/shared\n",
            "  shared_notion_page: https://notion.example.com/shared\n",
            "  xero_customer: customer-1\n",
            "  calendar_id: c_0123@group.calendar.example.com\n",
        );
        let parsed = parse(yaml).expect("nested manifest deserializes");
        assert_eq!(
            parsed.calendar_id.as_deref(),
            Some("c_0123@group.calendar.example.com")
        );
        assert_eq!(parsed.version.as_deref(), Some("26.9.14"));
        assert_eq!(parsed.host.as_deref(), Some("staging.neonlaw.com"));
        assert_eq!(parsed.project.as_deref(), Some("acme"));
        assert_eq!(parsed.lawyer_dri.as_deref(), Some("lawyer@example.com"));
        assert_eq!(parsed.xero_customer.as_deref(), Some("customer-1"));
        assert!(lint_contents(Path::new(FILE), yaml).is_empty());
    }

    #[test]
    fn required_project_handles_cannot_be_missing() {
        let findings = lint_contents(
            Path::new(FILE),
            "version: 26.9.14\nproject:\n  host: staging.neonlaw.com\n  name: acme\n",
        );
        for key in [
            "lawyer_dri",
            "client_dri",
            "private_slack_channel",
            "private_notion_page",
        ] {
            assert!(
                findings.iter().any(|finding| finding
                    .message
                    .contains(&format!("`project.{key}` is required"))),
                "missing `{key}` must be an error: {findings:?}"
            );
        }
        assert!(findings.iter().all(|finding| !finding.warning));
    }

    #[test]
    fn required_project_handles_reject_placeholders_and_wrong_shapes() {
        let findings = lint_contents(
            Path::new(FILE),
            concat!(
                "version: 26.9.14\nproject:\n",
                "  host: staging.neonlaw.com\n  name: acme\n",
                "  lawyer_dri: INSERT LAWYER DRI EMAIL\n",
                "  client_dri: todo\n",
                "  private_slack_channel: not-a-url\n",
                "  private_notion_page: INSERT PRIVATE NOTION PAGE URL\n",
            ),
        );
        for key in [
            "lawyer_dri",
            "client_dri",
            "private_slack_channel",
            "private_notion_page",
        ] {
            assert!(
                findings
                    .iter()
                    .any(|finding| finding.message.contains(&format!("`project.{key}`"))),
                "invalid `{key}` must be an error: {findings:?}"
            );
        }
        for key in ["lawyer_dri", "client_dri", "private_notion_page"] {
            assert!(
                findings.iter().any(|finding| {
                    finding.message.contains(&format!("`project.{key}`"))
                        && finding.message.contains("placeholder")
                }),
                "placeholder `{key}` must be identified: {findings:?}"
            );
        }
        assert!(findings.iter().all(|finding| !finding.warning));
    }

    /// `Y016`: a Project that genuinely has no live Slack channel or Notion
    /// page yet may say so explicitly, rather than being forced to invent a
    /// URL or leave the key missing. `lawyer_dri`/`client_dri` get no such
    /// out — every matter has both.
    #[test]
    fn not_provisioned_is_accepted_for_slack_and_notion_but_not_for_dri() {
        let yaml = concat!(
            "version: 26.9.14\nproject:\n",
            "  host: staging.neonlaw.com\n  name: acme\n",
            "  lawyer_dri: lawyer@example.com\n",
            "  client_dri: client@example.com\n",
            "  private_slack_channel: Not Provisioned\n",
            "  private_notion_page: not provisioned\n",
        );
        assert!(
            lint_contents(Path::new(FILE), yaml).is_empty(),
            "{:?}",
            lint_contents(Path::new(FILE), yaml)
        );
        let parsed = parse(yaml).expect("parses");
        assert_eq!(
            parsed.private_slack_channel.as_deref(),
            Some("Not Provisioned")
        );

        let dri_sentinel = concat!(
            "version: 26.9.14\nproject:\n",
            "  host: staging.neonlaw.com\n  name: acme\n",
            "  lawyer_dri: not provisioned\n",
            "  client_dri: client@example.com\n",
            "  private_slack_channel: not provisioned\n",
            "  private_notion_page: not provisioned\n",
        );
        assert!(
            codes(dri_sentinel).contains(&HANDLE_CODE),
            "`lawyer_dri` must still require a real email: {:?}",
            lint_contents(Path::new(FILE), dri_sentinel)
        );
    }

    /// `Y016`: `calendar_id` is optional, but when present it must be a
    /// Google Calendar id, never a URL or a placeholder.
    #[test]
    fn calendar_id_is_optional_and_must_be_a_calendar_id() {
        let base = concat!(
            "version: 26.9.14\nproject:\n",
            "  host: staging.neonlaw.com\n  name: acme\n",
            "  lawyer_dri: lawyer@example.com\n",
            "  client_dri: client@example.com\n",
            "  private_slack_channel: not provisioned\n",
            "  private_notion_page: not provisioned\n",
        );
        assert!(lint_contents(Path::new(FILE), base).is_empty());
        for bad in [
            "https://calendar.google.com/calendar/r?cid=abc",
            "INSERT CALENDAR ID",
            "two words",
        ] {
            let yaml = format!("{base}  calendar_id: {bad}\n");
            let findings = lint_contents(Path::new(FILE), &yaml);
            assert!(
                findings.iter().any(|finding| finding.code == HANDLE_CODE
                    && finding.message.contains("`project.calendar_id`")),
                "`{bad}` must be refused: {findings:?}"
            );
        }
    }

    #[test]
    fn malformed_nested_manifest_reports_one_manifest_finding() {
        let findings = lint_contents(
            Path::new(FILE),
            "version: 26.9.14\nproject:\n  host: [staging.neonlaw.com]\n  name: acme\n",
        );
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].path, Path::new(FILE));
        assert_eq!(findings[0].code, HOST_CODE);
    }

    #[test]
    fn version_must_be_an_exact_release_tag() {
        for version in ["latest", "main", "HEAD", ""] {
            let yaml = format!(
                "version: \"{version}\"\nproject:\n  host: staging.neonlaw.com\n  name: acme\n"
            );
            let finding = lint_contents(Path::new(FILE), &yaml)
                .into_iter()
                .find(|finding| finding.code == VERSION_CODE)
                .expect("invalid version finding");
            assert!(finding
                .message
                .contains("version must be an exact release tag"));
        }
    }

    #[test]
    fn host_must_be_a_hostname() {
        assert!(codes("host: https://staging.neonlaw.com\nproject: acme\n").contains(&HOST_CODE));
        assert!(codes("host: staging.neonlaw.com/app\nproject: acme\n").contains(&HOST_CODE));
        assert!(codes("host: .neonlaw.com\nproject: acme\n").contains(&HOST_CODE));
        assert!(codes("project: acme\n").contains(&HOST_CODE));
        assert!(!codes("host: staging.neonlaw.com\nproject: acme\n").contains(&HOST_CODE));
        assert!(is_hostname("staging.neonlaw.com"));
        assert!(is_hostname("localhost"));
        assert!(!is_hostname(".test"));
        assert!(!is_hostname("example.com:443"));
    }

    #[test]
    fn project_must_be_a_valid_code() {
        assert!(codes("host: staging.neonlaw.com\nproject: Not A Code\n").contains(&PROJECT_CODE));
        assert!(codes("host: staging.neonlaw.com\n").contains(&PROJECT_CODE));
        assert!(!codes("host: staging.neonlaw.com\nproject: acme\n").contains(&PROJECT_CODE));
    }

    #[test]
    fn unknown_key_names_the_known_set() {
        let findings = lint_contents(
            Path::new("navigator.yaml"),
            "host: staging.neonlaw.com\nproject: acme\nexempt_roots: [docs]\n",
        );
        let unknown = findings
            .iter()
            .find(|f| f.code == UNKNOWN_KEY_CODE)
            .expect("unknown key");
        assert!(unknown.message.contains("exempt_roots"));
        assert!(unknown.message.contains("host"));
        assert!(unknown.message.contains("project"));
        assert!(unknown.message.contains("no_live_row"));
        assert!(unknown.message.contains("allowed_hosts"));
        assert!(unknown.message.contains("allowed_links"));
        assert!(unknown.message.contains("allowed_prefixes"));
    }

    #[test]
    fn no_live_row_must_be_a_non_empty_reason() {
        let boolean = lint_contents(
            Path::new("navigator.yaml"),
            "host: staging.neonlaw.com\nproject: acme\nno_live_row: true\n",
        );
        assert!(boolean
            .iter()
            .any(|f| f.code == ROWLESS_CODE && f.message.contains("written as text")));
        let empty = lint_contents(
            Path::new("navigator.yaml"),
            "host: staging.neonlaw.com\nproject: acme\nno_live_row: \"\"\n",
        );
        assert!(empty
            .iter()
            .any(|f| f.code == ROWLESS_CODE && f.message.contains("must give the reason")));
        assert!(lint_contents(
            Path::new("navigator.yaml"),
            "host: staging.neonlaw.com\nproject: acme\nno_live_row: the matter closed\n"
        )
        .iter()
        .all(|finding| finding.warning));
    }

    #[test]
    fn a_comment_token_is_refused_and_a_hash_in_a_quoted_scalar_is_not() {
        let commented = lint_contents(
            Path::new("navigator.yaml"),
            "host: staging.neonlaw.com\n# record the exemption in the pull request\nproject: acme\n",
        );
        let finding = commented
            .iter()
            .find(|f| f.code == COMMENT_CODE)
            .expect("comment token");
        assert_eq!(finding.line, 2);
        assert!(
            finding.message.contains("pull request"),
            "{}",
            finding.message
        );
        assert!(
            finding.message.contains("repository contract"),
            "{}",
            finding.message
        );
        let trailing = lint_contents(
            Path::new("navigator.yaml"),
            "host: staging.neonlaw.com  # hostname of the deployment\nproject: acme\n",
        );
        assert!(
            trailing
                .iter()
                .any(|f| f.code == COMMENT_CODE && f.line == 1),
            "{trailing:?}"
        );
        let quoted = concat!(
            "host: staging.neonlaw.com\n",
            "project: acme\n",
            "allowed_prefixes:\n",
            "  \"https://react.dev/errors/#\": React minified errors\n",
        );
        assert!(
            !codes(quoted).contains(&COMMENT_CODE),
            "{:?}",
            lint_contents(Path::new("navigator.yaml"), quoted)
        );
        let quoted_value = "host: staging.neonlaw.com\nproject: \"acme # synthetic\"\n";
        assert!(!codes(quoted_value).contains(&COMMENT_CODE));
        let single_quoted = "host: staging.neonlaw.com\nproject: 'acme # synthetic'\n";
        assert!(!codes(single_quoted).contains(&COMMENT_CODE));
        let flow = concat!(
            "host: staging.neonlaw.com\n",
            "project: acme\n",
            "allowed_prefixes: [\"https://react.dev/errors/#\", 'https://doc.rust-lang.org/error_codes/#']\n",
        );
        assert!(
            !codes(flow).contains(&COMMENT_CODE),
            "{:?}",
            lint_contents(Path::new("navigator.yaml"), flow)
        );
        let apostrophe_then_comment = concat!(
            "host: staging.neonlaw.com\n",
            "project: acme\n",
            "no_live_row: the author's closed file # record the reason\n",
        );
        assert!(
            codes(apostrophe_then_comment).contains(&COMMENT_CODE),
            "{:?}",
            lint_contents(Path::new("navigator.yaml"), apostrophe_then_comment)
        );
        let bom = concat!(
            "\u{feff}",
            "# record the exemption in the pull request\n",
            "host: staging.neonlaw.com\n",
            "project: acme\n",
        );
        let bom_findings = lint_contents(Path::new("navigator.yaml"), bom);
        assert!(
            bom_findings
                .iter()
                .any(|f| f.code == COMMENT_CODE && f.line == 1),
            "{bom_findings:?}"
        );
        let block = concat!(
            "host: staging.neonlaw.com\n",
            "project: acme\n",
            "no_live_row: |\n",
            "  the matter closed; # this hash is scalar content\n",
        );
        assert!(!codes(block).contains(&COMMENT_CODE));
    }

    #[test]
    fn yml_spelling_is_a_rename() {
        let dir = std::env::temp_dir().join(format!(
            "navigator-manifest-yml-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(RETIRED_FILE),
            "host: staging.neonlaw.com\nproject: acme\n",
        )
        .unwrap();
        let findings = lint(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(findings
            .iter()
            .any(|f| f.code == RENAME_CODE
                && f.message == "the manifest is navigator.yaml, rename it"));
    }

    #[test]
    fn skills_entries_parse_into_pins() {
        let yaml = concat!(
            "host: staging.neonlaw.com\n",
            "project: acme\n",
            "skills:\n",
            "  - jurisdiction: NV\n",
            "    practice_area: estates\n",
            "    version: \"1\"\n",
            "  - jurisdiction: TX\n",
            "    practice_area: probate\n",
            "    version: \"1\"\n",
        );
        assert!(
            lint_contents(Path::new(FILE), yaml)
                .iter()
                .all(|f| f.warning),
            "{:?}",
            lint_contents(Path::new(FILE), yaml)
        );
        let parsed = parse(yaml).expect("skills parse");
        assert_eq!(parsed.skills.len(), 2);
        assert_eq!(parsed.skills[0].jurisdiction, "NV");
        assert_eq!(parsed.skills[0].practice_area, "estates");
        assert_eq!(parsed.skills[0].version, "1");
    }

    #[test]
    fn a_skills_entry_missing_a_field_is_flagged() {
        let yaml = concat!(
            "host: staging.neonlaw.com\n",
            "project: acme\n",
            "skills:\n",
            "  - jurisdiction: NV\n",
            "    version: \"1\"\n",
        );
        let findings = lint_contents(Path::new(FILE), yaml);
        assert!(
            findings
                .iter()
                .any(|f| f.code == SKILLS_CODE && f.message.contains("practice_area")),
            "{findings:?}"
        );
    }

    #[test]
    fn a_non_list_skills_key_is_flagged() {
        let yaml = "host: staging.neonlaw.com\nproject: acme\nskills: nv/estates\n";
        let findings = lint_contents(Path::new(FILE), yaml);
        assert!(findings.iter().any(|f| f.code == SKILLS_CODE));
    }

    #[test]
    fn pin_skill_appends_a_new_entry() {
        let yaml = "host: staging.neonlaw.com\nproject: acme\n";
        let (updated, changed) = pin_skill(yaml, "NV", "estates", "1").expect("pin");
        assert!(changed);
        let parsed = parse(&updated).expect("parses");
        assert_eq!(parsed.skills.len(), 1);
        assert_eq!(parsed.skills[0].jurisdiction, "NV");
        assert_eq!(parsed.skills[0].practice_area, "estates");
        assert_eq!(parsed.skills[0].version, "1");
    }

    #[test]
    fn pin_skill_is_a_no_op_when_already_pinned_at_the_same_version() {
        let yaml = "host: staging.neonlaw.com\nproject: acme\n";
        let (once, _) = pin_skill(yaml, "NV", "estates", "1").expect("pin");
        let (twice, changed) = pin_skill(&once, "nv", "estates", "1").expect("pin again");
        assert!(!changed, "re-pinning the same version must be a no-op");
        assert_eq!(once, twice);
    }

    #[test]
    fn pin_skill_updates_the_version_on_an_existing_pin() {
        let yaml = "host: staging.neonlaw.com\nproject: acme\n";
        let (once, _) = pin_skill(yaml, "NV", "estates", "1").expect("pin");
        let (updated, changed) = pin_skill(&once, "NV", "estates", "2").expect("re-pin");
        assert!(changed);
        let parsed = parse(&updated).expect("parses");
        assert_eq!(
            parsed.skills.len(),
            1,
            "a version bump updates in place, not a duplicate"
        );
        assert_eq!(parsed.skills[0].version, "2");
    }
}
