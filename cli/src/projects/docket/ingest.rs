//! `navigator project docket ingest` — file the documents a court's CM/ECF
//! notices point at into this Project.
//!
//! Run from a Project checkout. The Project code and host come from
//! `navigator.yaml`; the cases come from its `dockets:` list (and
//! `matter.case_number`). A notice for any other case is skipped without
//! naming it, so a mailbox that serves several matters cannot leak one into
//! another's output.
//!
//! A free-look link is single-use, so the order of work is fixed: plan
//! offline, then for each document write the bytes under `documents/` before
//! anything else, and only then upload and stage the pointer. A run that
//! fails after the download leaves the bytes on disk, and the next run reuses
//! them rather than spending a second free look.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};

use super::case_timeline;
use super::claims_agent::{self, AgentFetch, ClaimsNotice};
use super::nef::{self, Notice};
use crate::projects::manifest;

/// Where every docket document lands below `documents/`.
const DOCKET_FOLDER: &str = "docket";
/// How many words of the docket text name the file.
const SLUG_WORDS: usize = 6;
const SLUG_MAX_CHARS: usize = 48;

/// What the operator asked for.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub notices: Vec<PathBuf>,
    pub dry_run: bool,
    /// File only these docket entry numbers.
    pub only: Vec<String>,
    /// Skip a notice whose docket text contains any of these, ignoring case.
    pub skip_patterns: Vec<String>,
    /// Replace the suggested kind on every document.
    pub kind: Option<String>,
    pub host: Option<String>,
}

/// One free-look download. The real one is [`HttpFreeLook`]; tests use a fake,
/// so no test spends or even reaches a court's link.
#[async_trait::async_trait]
pub trait FreeLook: Send + Sync {
    async fn fetch(&self, url: &str) -> Result<Vec<u8>>;
}

/// The `%PDF` magic-byte check every download path applies before anything
/// is written: a block page is HTML wearing a `.pdf` name, and only the
/// bytes say so (LAW-159).
pub(super) fn is_pdf(bytes: &[u8]) -> bool {
    bytes.starts_with(b"%PDF")
}

/// Follows a CM/ECF free-look link over HTTPS. A court that answers with a
/// wrapper page is followed once to the PDF that page embeds.
pub struct HttpFreeLook {
    client: reqwest::Client,
}

impl HttpFreeLook {
    /// # Errors
    ///
    /// When the HTTP client cannot be built.
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .context("build free-look client")?,
        })
    }

    async fn get(&self, url: &str) -> Result<Vec<u8>> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .context("follow the free-look link")?;
        let status = response.status();
        if !status.is_success() {
            return Err(anyhow!("the free-look link answered {status}"));
        }
        Ok(response
            .bytes()
            .await
            .context("read free-look bytes")?
            .to_vec())
    }
}

#[async_trait::async_trait]
impl FreeLook for HttpFreeLook {
    async fn fetch(&self, url: &str) -> Result<Vec<u8>> {
        let bytes = self.get(url).await?;
        if is_pdf(&bytes) {
            return Ok(bytes);
        }
        let page = String::from_utf8_lossy(&bytes);
        let embedded = embedded_document(&page).ok_or_else(|| {
            anyhow!(
                "the free-look link answered without a PDF; it may already have been used, \
                 so download this entry from PACER"
            )
        })?;
        let next = url::Url::parse(url)
            .and_then(|base| base.join(&embedded))
            .context("resolve the document the free-look page embeds")?;
        let bytes = self.get(next.as_str()).await?;
        if is_pdf(&bytes) {
            Ok(bytes)
        } else {
            Err(anyhow!(
                "the free-look page embedded something other than a PDF"
            ))
        }
    }
}

/// The `src` of the first `<iframe>` or `<embed>` on a free-look wrapper page.
fn embedded_document(page: &str) -> Option<String> {
    let lower = page.to_ascii_lowercase();
    ["<iframe", "<embed"].iter().find_map(|tag| {
        let start = lower.find(tag)?;
        let end = lower[start..].find('>')? + start;
        let element = &page[start..end];
        let at = element.to_ascii_lowercase().find("src=")? + 4;
        let value = &element[at..];
        let quote = value.chars().next()?;
        let src = if quote == '"' || quote == '\'' {
            value[1..].split(quote).next()?
        } else {
            value.split_whitespace().next()?
        };
        Some(src.replace("&amp;", "&"))
    })
}

/// `navigator project docket ingest`, from the working directory.
pub async fn run(options: Options) -> ExitCode {
    let free_look = match HttpFreeLook::new() {
        Ok(free_look) => free_look,
        Err(error) => {
            eprintln!("navigator project docket ingest: {error:#}");
            return ExitCode::from(2);
        }
    };
    let agent = match claims_agent::HttpAgentFetch::new() {
        Ok(agent) => agent,
        Err(error) => {
            eprintln!("navigator project docket ingest: {error:#}");
            return ExitCode::from(2);
        }
    };
    match ingest(Path::new("."), &options, &free_look, &agent).await {
        Ok(report) if report.failures == 0 => ExitCode::SUCCESS,
        Ok(report) => {
            eprintln!(
                "navigator project docket ingest: {} document(s) failed; their notices can be re-run",
                report.failures
            );
            ExitCode::from(2)
        }
        Err(error) => {
            eprintln!("navigator project docket ingest: {error:#}");
            ExitCode::from(2)
        }
    }
}

/// What one run did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub filed: usize,
    pub duplicates: usize,
    pub skipped_notices: usize,
    pub failures: usize,
    /// Docket entries recorded through `POST /app/api/projects/{id}/docket`,
    /// one per main document and numbered attachment.
    pub entries: usize,
    /// Documents handed to a person: a claims-agent link that no fetch and
    /// no free-look fallback could satisfy, or a link that names no docket
    /// entry number to file under.
    pub manual: usize,
}

/// The Project this checkout declares, and the dockets it owns.
struct ProjectScope {
    code: String,
    host: Option<String>,
    dockets: Vec<String>,
}

fn project_scope(root: &Path) -> Result<ProjectScope> {
    let path = root.join(manifest::FILE);
    let contents = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "read {} — run this from the root of a Project repository",
            manifest::FILE
        )
    })?;
    let parsed = manifest::parse(&contents).map_err(|error| anyhow!(error))?;
    let code = parsed
        .project
        .ok_or_else(|| anyhow!("{} names no Project", manifest::FILE))?;
    let mut dockets = parsed.dockets;
    let matter_case = serde_yaml::from_str::<serde_yaml::Value>(&contents)
        .ok()
        .and_then(|document| {
            document
                .get(crate::projects::matter::KEY)?
                .get("case_number")?
                .as_str()
                .map(str::trim)
                .map(str::to_string)
        })
        .filter(|case| !case.is_empty());
    if let Some(case) = matter_case {
        if !dockets
            .iter()
            .any(|known| known.eq_ignore_ascii_case(&case))
        {
            dockets.push(case);
        }
    }
    if dockets.is_empty() {
        return Err(anyhow!(
            "{} lists no dockets; add the case numbers this Project appears in under `dockets:`",
            manifest::FILE
        ));
    }
    Ok(ProjectScope {
        code,
        host: parsed.host,
        dockets,
    })
}

/// The manifest docket a notice's case number belongs to. A notice prints
/// the judge's initials after the number (`9:26-cv-00042-XYZ`); a manifest
/// entry may leave them off.
fn docket_for<'a>(case_no: &str, dockets: &'a [String]) -> Option<&'a str> {
    let case_no = case_no.to_ascii_lowercase();
    dockets
        .iter()
        .find(|docket| {
            let docket = docket.to_ascii_lowercase();
            case_no == docket || case_no.starts_with(&format!("{docket}-"))
        })
        .map(String::as_str)
}

/// `9:26-cv-00042` → `9-26-cv-00042`: the folder one docket's documents share.
fn case_folder(docket: &str) -> String {
    let mut folder = String::new();
    for c in docket.chars() {
        if c.is_ascii_alphanumeric() {
            folder.push(c.to_ascii_lowercase());
        } else if !folder.is_empty() && !folder.ends_with('-') {
            folder.push('-');
        }
    }
    folder.trim_end_matches('-').to_string()
}

/// The first few words of `text`, starting at the first word that begins
/// with a letter, so the slug never starts with a digit an attachment number
/// could be mistaken for.
fn text_slug(text: &str) -> String {
    let words = text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .skip_while(|word| !word.starts_with(|c: char| c.is_ascii_alphabetic()))
        .take(SLUG_WORDS);
    let mut slug = String::new();
    for word in words {
        if slug.len() + word.len() + 1 > SLUG_MAX_CHARS {
            break;
        }
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(&word);
    }
    if slug.is_empty() {
        "entry".to_string()
    } else {
        slug
    }
}

/// `DKT NNNN[-<att>] - <title>.pdf` (see [`case_timeline::filing_name`]). A
/// docket entry number that is not digits cannot be a DKT Number, so it keeps
/// the older `di-<n>[-<att>]-<slug>.pdf` form.
fn document_filename(doc_no: &str, attachment: Option<u32>, text: &str) -> String {
    let entry = match attachment {
        Some(number) => format!("{doc_no}-{number}"),
        None => doc_no.to_string(),
    };
    case_timeline::filing_name(&entry, text, "pdf").unwrap_or_else(|_| match attachment {
        Some(number) => format!("di-{doc_no}-{number}-{}.pdf", text_slug(text)),
        None => format!("di-{doc_no}-{}.pdf", text_slug(text)),
    })
}

/// The slug of a filing: its flat `DKT` name, which sits directly in Docket
/// Filings, or the older `docket/<case>/di-…` path.
fn filing_slug(case_folder: &str, filename: &str) -> String {
    if case_timeline::parse_filing_name(filename).is_some() {
        filename.to_string()
    } else {
        format!("{DOCKET_FOLDER}/{case_folder}/{filename}")
    }
}

/// An entry number without leading zeros, so `0029` and `29` compare equal.
fn unpadded(doc_no: &str) -> String {
    let trimmed = doc_no.trim_start_matches('0');
    if trimmed.is_empty() { "0" } else { trimmed }.to_string()
}

/// The `(docket entry, attachment)` a docket filename names.
pub(super) fn docket_key(filename: &str) -> Option<(String, Option<u32>)> {
    let rest = filename.strip_prefix("di-")?;
    let (doc_no, rest) = rest.split_once('-')?;
    if doc_no.is_empty() || !doc_no.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let attachment = rest
        .split_once('-')
        .filter(|(head, _)| !head.is_empty() && head.chars().all(|c| c.is_ascii_digit()))
        .and_then(|(head, _)| head.parse().ok());
    Some((doc_no.to_string(), attachment))
}

/// The kind a docket document most likely is. The operator overrides it with
/// `--kind`.
fn suggested_kind(docket_text: &str, attachment_description: Option<&str>) -> &'static str {
    if let Some(description) = attachment_description {
        if description.to_ascii_lowercase().contains("exhibit") {
            return "exhibit";
        }
    }
    if docket_text
        .split_whitespace()
        .next()
        .is_some_and(|word| word.eq_ignore_ascii_case("transcript"))
    {
        return "transcript";
    }
    "pleading"
}

/// What this checkout already files, read from its committed pointers.
#[derive(Default)]
struct Filed {
    /// `(case folder, entry, attachment)` → slug.
    /// The case folder is `None` for a flat `DKT NNNN - <title>` filing,
    /// whose name carries no case.
    keyed: BTreeMap<(Option<String>, String, Option<u32>), String>,
    /// Lowercased document filename → slug, for a filing uploaded from the
    /// drafts lane under its own name.
    named: BTreeMap<String, String>,
}

impl Filed {
    fn read(root: &Path) -> Result<Self> {
        let mut filed = Self::default();
        for relative in crate::document_read::discover_pointers(root)? {
            let slash = crate::document_sync::slash_path(&relative)?;
            let Some(slug) = slash
                .strip_prefix(&format!("{}/", crate::document_sync::POINTER_DIRECTORY))
                .and_then(crate::document_sync::strip_pointer_extension)
            else {
                continue;
            };
            filed.record(slug);
        }
        Ok(filed)
    }

    fn record(&mut self, slug: &str) {
        let mut parts = slug.rsplit('/');
        let Some(filename) = parts.next() else {
            return;
        };
        self.named
            .entry(filename.to_ascii_lowercase())
            .or_insert_with(|| slug.to_string());
        let folder = parts.next();
        let docket_root = parts.next();
        match (folder, docket_root, parts.next()) {
            (None, _, _) => {
                if let Some((doc_no, attachment)) = case_timeline::parse_filing_name(filename) {
                    self.keyed
                        .entry((None, doc_no, attachment))
                        .or_insert_with(|| slug.to_string());
                }
            }
            (Some(folder), Some(DOCKET_FOLDER), None) => {
                if let Some((doc_no, attachment)) = docket_key(filename) {
                    self.keyed
                        .entry((Some(folder.to_string()), unpadded(&doc_no), attachment))
                        .or_insert_with(|| slug.to_string());
                }
            }
            _ => {}
        }
    }

    fn existing(&self, item: &Item) -> Option<&str> {
        let doc_no = unpadded(&item.doc_no);
        self.keyed
            .get(&(Some(item.folder.clone()), doc_no.clone(), item.attachment))
            .or_else(|| self.keyed.get(&(None, doc_no, item.attachment)))
            .or_else(|| {
                item.original_filename
                    .as_ref()
                    .and_then(|name| self.named.get(&name.to_ascii_lowercase()))
            })
            .map(String::as_str)
    }
}

/// One document a notice points at.
#[derive(Debug, Clone)]
struct Item {
    case_no: String,
    folder: String,
    doc_no: String,
    attachment: Option<u32>,
    filename: String,
    title: String,
    kind: String,
    free_look_url: Option<String>,
    /// The claims-agent download link, when the notice came from a claims
    /// agent. Blocked by the agent's site before any PDF exists.
    agent_url: Option<String>,
    original_filename: Option<String>,
    description: String,
}

impl Item {
    fn slug(&self) -> String {
        filing_slug(&self.folder, &self.filename)
    }

    fn label(&self) -> String {
        match self.attachment {
            Some(number) => format!("D.I. {}-{number}", self.doc_no),
            None => format!("D.I. {}", self.doc_no),
        }
    }
}

/// The documents one notice points at.
fn items_for(notice: &Notice, docket: &str, kind_override: Option<&str>) -> Vec<Item> {
    let Some(doc_no) = notice.doc_no.clone() else {
        return Vec::new();
    };
    let folder = case_folder(docket);
    let mut items = vec![Item {
        case_no: notice.case_no.clone(),
        folder: folder.clone(),
        doc_no: doc_no.clone(),
        attachment: None,
        filename: document_filename(&doc_no, None, &notice.docket_text),
        title: notice.docket_text.clone(),
        kind: kind_override
            .unwrap_or_else(|| suggested_kind(&notice.docket_text, None))
            .to_string(),
        free_look_url: notice.free_look_url.clone(),
        agent_url: None,
        original_filename: notice.original_filename.clone(),
        description: format!(
            "D.I. {doc_no} in {}: {}",
            notice.case_no,
            truncate(&notice.docket_text, 400)
        ),
    }];
    for attachment in &notice.attachments {
        items.push(Item {
            case_no: notice.case_no.clone(),
            folder: folder.clone(),
            doc_no: doc_no.clone(),
            attachment: Some(attachment.number),
            filename: document_filename(&doc_no, Some(attachment.number), &attachment.description),
            title: attachment.description.clone(),
            kind: kind_override
                .unwrap_or_else(|| {
                    suggested_kind(&notice.docket_text, Some(&attachment.description))
                })
                .to_string(),
            free_look_url: attachment.free_look_url.clone(),
            agent_url: None,
            original_filename: attachment.original_filename.clone(),
            description: format!(
                "D.I. {doc_no}-{} in {}: {}",
                attachment.number, notice.case_no, attachment.description
            ),
        });
    }
    items
}

fn truncate(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((at, _)) => &text[..at],
        None => text,
    }
}

/// The documents a claims-agent notice links. Only links whose label names a
/// docket entry number can be filed automatically; a link without one is
/// handed to a person in [`accept`].
fn items_for_claims(claims: &ClaimsNotice, docket: &str, kind_override: Option<&str>) -> Vec<Item> {
    let folder = case_folder(docket);
    claims
        .links
        .iter()
        .filter_map(|link| {
            let doc_no = claims_agent::docket_entry_number(&link.label)?;
            let title = claims_agent::link_title(&link.label);
            let case_no = claims.case_no.clone().unwrap_or_default();
            Some(Item {
                case_no: case_no.clone(),
                folder: folder.clone(),
                doc_no: doc_no.clone(),
                attachment: None,
                filename: document_filename(&doc_no, None, &title),
                title: title.clone(),
                kind: kind_override
                    .unwrap_or_else(|| suggested_kind(&title, None))
                    .to_string(),
                free_look_url: None,
                agent_url: Some(link.url.clone()),
                original_filename: None,
                description: format!("D.I. {doc_no} in {case_no}: {}", truncate(&title, 400)),
            })
        })
        .collect()
}

/// Why one document could not be fetched at all. The handoff is a reported
/// outcome, not a failure: the run keeps going, and the Docket Entry is
/// recorded source pending so the gap shows on the matter page.
#[derive(Debug, thiserror::Error)]
enum Handoff {
    #[error(
        "manual download needed: D.I. {doc_no} could not be fetched from {url} — download it in a \
         browser and file it with `navigator site document upload`; the Docket Entry is recorded \
         as source pending"
    )]
    Manual { doc_no: String, url: String },
}

/// The two notices a docket ingest reads: a court's CM/ECF notice of
/// electronic filing and a claims agent's notice of the same case.
enum ParsedNotice {
    Nef(Notice),
    Claims(ClaimsNotice),
}

impl ParsedNotice {
    fn nef(&self) -> Option<&Notice> {
        match self {
            Self::Nef(notice) => Some(notice),
            Self::Claims(_) => None,
        }
    }

    /// The case number the notice prints, judge initials included.
    fn case_no(&self) -> Option<&str> {
        match self {
            Self::Nef(notice) => Some(notice.case_no.as_str()),
            Self::Claims(claims) => claims.case_no.as_deref(),
        }
    }

    /// The text `--skip-pattern` reads: the docket text of a notice, the
    /// joined link labels of a claims notice.
    fn filter_text(&self) -> String {
        match self {
            Self::Nef(notice) => notice.docket_text.clone(),
            Self::Claims(claims) => claims
                .links
                .iter()
                .map(|link| link.label.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        }
    }

    fn items(&self, docket: &str, kind_override: Option<&str>) -> Vec<Item> {
        match self {
            Self::Nef(notice) => items_for(notice, docket, kind_override),
            Self::Claims(claims) => items_for_claims(claims, docket, kind_override),
        }
    }
}

/// One notice that belongs to this Project and passed the filters.
struct Accepted {
    parsed: ParsedNotice,
    /// The manifest docket the notice's case number matched.
    docket: String,
    items: Vec<Item>,
}

/// Parse and filter every notice. Nothing here touches the network.
#[allow(clippy::too_many_lines)]
fn accept(scope: &ProjectScope, options: &Options, report: &mut Report) -> Vec<Accepted> {
    let patterns: Vec<String> = options
        .skip_patterns
        .iter()
        .map(|pattern| pattern.to_ascii_lowercase())
        .collect();
    let mut accepted = Vec::new();
    for path in &options.notices {
        let bytes = match std::fs::read(path).with_context(|| format!("read {}", path.display())) {
            Ok(bytes) => bytes,
            Err(error) => {
                println!("skipped {}: {error:#}", path.display());
                report.skipped_notices += 1;
                continue;
            }
        };
        let parsed = match nef::parse_eml(&bytes).map_err(anyhow::Error::from) {
            Ok(notice) => ParsedNotice::Nef(notice),
            Err(_) => match claims_agent::parse(&bytes) {
                Some(claims) => ParsedNotice::Claims(claims),
                None => {
                    println!(
                        "skipped {}: not a CM/ECF notice or a claims-agent notice",
                        path.display()
                    );
                    report.skipped_notices += 1;
                    continue;
                }
            },
        };
        let Some(case_no) = parsed.case_no().map(str::to_string) else {
            println!(
                "skipped {}: no case number — match it to a docket by hand",
                path.display()
            );
            report.skipped_notices += 1;
            continue;
        };
        let Some(docket) = docket_for(&case_no, &scope.dockets) else {
            println!("skipped {}: not a docket of this Project", path.display());
            report.skipped_notices += 1;
            continue;
        };
        let text = parsed.filter_text().to_ascii_lowercase();
        if let Some(pattern) = patterns
            .iter()
            .find(|pattern| text.contains(pattern.as_str()))
        {
            println!(
                "skipped {}: docket text matches --skip-pattern {pattern:?}",
                path.display()
            );
            report.skipped_notices += 1;
            continue;
        }
        let mut items = parsed.items(docket, options.kind.as_deref());
        match &parsed {
            ParsedNotice::Claims(claims) => {
                // A link that names no docket entry number cannot be filed
                // automatically under a DKT name: hand it to a person.
                items.retain(|item| options.only.is_empty() || options.only.contains(&item.doc_no));
                for link in &claims.links {
                    if claims_agent::docket_entry_number(&link.label).is_none() {
                        println!(
                            "{} · manual download needed: {} names no docket entry number — \
                             download {} in a browser and file it with \
                             `navigator site document upload`",
                            case_no,
                            truncate(&link.label, 80),
                            link.url
                        );
                        report.manual += 1;
                    }
                }
            }
            ParsedNotice::Nef(notice) => {
                if !options.only.is_empty()
                    && !notice
                        .doc_no
                        .as_ref()
                        .is_some_and(|doc_no| options.only.contains(doc_no))
                {
                    report.skipped_notices += 1;
                    continue;
                }
            }
        }
        accepted.push(Accepted {
            parsed,
            docket: docket.to_string(),
            items,
        });
    }
    accepted
}

/// Plan, then (unless `--dry-run`) download, upload, and stage each document.
///
/// # Errors
///
/// A missing or malformed manifest, a missing login, or an unreadable
/// `documents/` tree. A failure on one document is counted in
/// [`Report::failures`] and the run continues with the next.
#[allow(clippy::too_many_lines)]
pub async fn ingest(
    root: &Path,
    options: &Options,
    free_look: &dyn FreeLook,
    agent: &dyn AgentFetch,
) -> Result<Report> {
    let scope = project_scope(root)?;
    crate::document_sync::refuse_legacy_layout(root)?;
    let mut report = Report::default();
    let accepted = accept(&scope, options, &mut report);
    let mut filed = Filed::read(root)?;
    let mut planned: Vec<Item> = Vec::new();
    let mut seen: BTreeSet<(String, String, Option<u32>)> = BTreeSet::new();

    for Accepted { parsed, items, .. } in &accepted {
        if let Some(notice) = parsed.nef() {
            if notice.doc_no.is_none() {
                println!(
                    "{} · no document attached · {}",
                    notice.case_no,
                    truncate(&notice.docket_text, 80)
                );
            }
        }
        for item in items {
            let key = (item.folder.clone(), item.doc_no.clone(), item.attachment);
            if let Some(existing) = filed.existing(item) {
                println!(
                    "{} · {} · already filed as {existing}",
                    item.case_no,
                    item.label()
                );
                report.duplicates += 1;
                continue;
            }
            if !seen.insert(key) {
                report.duplicates += 1;
                continue;
            }
            let on_disk = docket_bytes(root, &item.slug()).is_file()
                || crate::document_sync::document_path_for_slug(root, Path::new(&item.slug()))
                    .is_file();
            if item.free_look_url.is_none() && item.agent_url.is_none() && !on_disk {
                println!(
                    "{} · {} · no free-look link in the notice; download it from {}'s document page",
                    item.case_no,
                    item.label(),
                    format_args!("D.I. {}", item.doc_no)
                );
                continue;
            }
            println!(
                "{} · {} · {} · {}{}",
                item.case_no,
                item.label(),
                item.kind,
                item.slug(),
                if on_disk {
                    " (bytes already on disk)"
                } else {
                    ""
                }
            );
            planned.push(item.clone());
        }
    }

    let free_looks: BTreeMap<String, String> = accepted
        .iter()
        .filter_map(|accepted| accepted.parsed.nef())
        .flat_map(free_look_index)
        .collect();
    let service_list = ServiceList::from_notices(
        &scope.code,
        accepted.iter().filter_map(|accepted| accepted.parsed.nef()),
    );
    service_list.print();

    let entries = entry_bodies(&accepted, &filed);
    for entry in &entries {
        println!(
            "{} · D.I. {} · docket entry · {}",
            entry["docket_number"].as_str().unwrap_or_default(),
            entry["entry_number"].as_str().unwrap_or_default(),
            entry["kind"].as_str().unwrap_or_default()
        );
    }

    if options.dry_run {
        println!(
            "dry run: {} document(s) would be filed into {} and {} docket entr{} recorded; nothing was downloaded",
            planned.len(),
            scope.code,
            entries.len(),
            if entries.len() == 1 { "y" } else { "ies" }
        );
        return Ok(report);
    }

    let host = options.host.as_deref().or(scope.host.as_deref());
    if planned.is_empty() {
        println!("nothing to file into {}", scope.code);
    } else {
        file_planned(
            root,
            host,
            &scope.code,
            free_look,
            agent,
            &free_looks,
            &planned,
            &mut filed,
            &mut report,
        )
        .await?;
    }
    let entries = entry_bodies(&accepted, &filed);
    if !entries.is_empty() {
        record_entries(root, host, &scope.code, &entries, &mut report).await;
    }
    if !service_list.is_empty() {
        match service_list.persist(host).await {
            Ok((people, entries)) => println!(
                "service list: {} new person(s), {} new relationship log row(s)",
                people.created, entries.created
            ),
            Err(error) => {
                eprintln!("service list: {error:#}");
                report.failures += 1;
            }
        }
    }
    Ok(report)
}

/// The docket entry one notice records (LAW-133): the entry number, the
/// kind read from the docket text, the text itself as the title, the filer
/// as the party, the filed date, and the main document by the slug the
/// filing step wrote. A text-only entry (no document number) records nothing.
fn docket_entry_body(notice: &Notice, docket: &str) -> Option<serde_json::Value> {
    let doc_no = notice.doc_no.as_deref()?;
    let mut body = serde_json::json!({
        "docket_number": docket,
        "entry_number": doc_no,
        "kind": entry_kind(&notice.docket_text),
        "title": notice.docket_text,
        "slug": filing_slug(
            &case_folder(docket),
            &document_filename(doc_no, None, &notice.docket_text),
        ),
    });
    if let Some(caption) = notice.case_name.as_deref().filter(|name| !name.is_empty()) {
        body["caption"] = serde_json::json!(caption);
    }
    if !notice.court.is_empty() {
        body["forum"] = serde_json::json!(notice.court);
    }
    if let Some(party) = notice
        .filed_by
        .as_deref()
        .or(notice.filer_attorney.as_deref())
    {
        body["party"] = serde_json::json!(party);
    }
    if let Some(filed) = notice.filed_at {
        body["filed_or_served_on"] = serde_json::json!(filed.format("%Y-%m-%d").to_string());
    }
    Some(body)
}

/// Every court-numbered document has its own linked entry. An unavailable
/// document remains source pending; retained identities are linked before being renamed.
fn docket_entry_bodies(notice: &Notice, docket: &str, filed: &Filed) -> Vec<serde_json::Value> {
    let Some(base) = docket_entry_body(notice, docket) else {
        return Vec::new();
    };
    items_for(notice, docket, None)
        .into_iter()
        .map(|item| {
            let mut body = base.clone();
            body["entry_number"] = serde_json::json!(match item.attachment {
                Some(number) => format!("{}-{number}", item.doc_no),
                None => item.doc_no.clone(),
            });
            body["title"] = serde_json::json!(item.title);
            body["kind"] = serde_json::json!(entry_kind(&item.title));
            if let Some(slug) = filed.existing(&item) {
                body["slug"] = serde_json::json!(slug);
            } else if let Some(object) = body.as_object_mut() {
                object.remove("slug");
            }
            body
        })
        .collect()
}

/// The docket entries a run records: a court notice's entries carry the
/// notice's caption, forum, party, and filed date; a claims-agent notice's
/// entries carry the link label as the title. A document that never files
/// leaves the entry source pending — no `slug` — so the gap shows on the
/// matter page (LAW-159).
fn entry_bodies(accepted: &[Accepted], filed: &Filed) -> Vec<serde_json::Value> {
    accepted
        .iter()
        .flat_map(|accepted| match &accepted.parsed {
            ParsedNotice::Nef(notice) => docket_entry_bodies(notice, &accepted.docket, filed),
            ParsedNotice::Claims(_) => {
                claims_entry_bodies(&accepted.items, &accepted.docket, filed)
            }
        })
        .collect()
}

/// The source-pending-able entries of a claims-agent notice, one per link
/// that names a docket entry number.
fn claims_entry_bodies(items: &[Item], docket: &str, filed: &Filed) -> Vec<serde_json::Value> {
    items
        .iter()
        .map(|item| {
            let mut body = serde_json::json!({
                "docket_number": docket,
                "entry_number": item.doc_no,
                "kind": entry_kind(&item.title),
                "title": item.title,
            });
            if let Some(slug) = filed.existing(item) {
                body["slug"] = serde_json::json!(slug);
            }
            body
        })
        .collect()
}

/// The free-look link of every document a court notice names, keyed the way
/// a claims-agent item names its entry: unpadded for the main document,
/// `entry-attachment` for a numbered attachment. A claims-agent link that
/// the agent's site blocks falls back to these.
fn free_look_index(notice: &Notice) -> Vec<(String, String)> {
    let mut index = Vec::new();
    if let (Some(doc_no), Some(url)) = (&notice.doc_no, &notice.free_look_url) {
        index.push((unpadded(doc_no), url.clone()));
    }
    for attachment in &notice.attachments {
        if let (Some(doc_no), Some(url)) = (&notice.doc_no, &attachment.free_look_url) {
            index.push((
                format!("{}-{}", unpadded(doc_no), attachment.number),
                url.clone(),
            ));
        }
    }
    index
}

/// The docket entry kind a notice's text names: its first word, when that
/// word is a kind the spine knows; `other` when it is not. A hearing or
/// trial is never recorded from a notice, because the notice does not carry
/// the date the court set.
fn entry_kind(docket_text: &str) -> &'static str {
    let first = docket_text
        .split(|c: char| !c.is_ascii_alphabetic())
        .find(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match first.as_str() {
        "motion" => "motion",
        "opposition" | "response" => "opposition",
        "reply" => "reply",
        "order" => "order",
        "notice" => "notice",
        "stipulation" => "stipulation",
        "subpoena" => "subpoena",
        "complaint" | "answer" | "petition" | "counterclaim" => "pleading",
        _ => "other",
    }
}

/// Record each planned entry through `POST /app/api/projects/{id}/docket`.
/// The door answers an entry number already on the case with the existing
/// row, so a re-run records nothing twice.
async fn record_entries(
    root: &Path,
    host: Option<&str>,
    code: &str,
    entries: &[serde_json::Value],
    report: &mut Report,
) {
    let client = match crate::remote::DocumentClient::connect(host, code).await {
        Ok(client) => client,
        Err(error) => {
            eprintln!("docket entries: {error:#}");
            report.failures += entries.len();
            return;
        }
    };
    let coordinates = super::entries::Coordinates {
        base: client.base().to_string(),
        token: client.token().to_string(),
        project_id: client.project_id(),
        project_code: code.to_string(),
    };
    for entry in entries {
        let recorded = async {
            let row = super::entries::post_entry(&coordinates, entry).await?;
            super::renumber::name_linked_document(&coordinates, &row, root).await?;
            super::timeline_write::write_after(root, &coordinates.project_code, &row)?;
            Ok::<_, anyhow::Error>(row)
        }
        .await;
        match recorded {
            Ok(row) => {
                report.entries += 1;
                println!(
                    "recorded docket entry D.I. {} ({})",
                    row["entry_number"].as_str().unwrap_or_default(),
                    row["id"].as_str().unwrap_or_default()
                );
            }
            Err(error) => {
                eprintln!(
                    "docket entry D.I. {}: {error:#}",
                    entry["entry_number"].as_str().unwrap_or_default()
                );
                report.failures += 1;
            }
        }
    }
}

async fn file_planned(
    root: &Path,
    host: Option<&str>,
    code: &str,
    free_look: &dyn FreeLook,
    agent: &dyn AgentFetch,
    free_looks: &BTreeMap<String, String>,
    planned: &[Item],
    filed: &mut Filed,
    report: &mut Report,
) -> Result<()> {
    let client = crate::remote::DocumentClient::connect(host, code).await?;
    crate::document_sync::ensure_documents_ignored(root)?;
    for item in planned {
        match file_one(root, &client, free_look, agent, free_looks, item, filed).await {
            Ok(pointer_path) => {
                println!("staged {}", pointer_path.display());
                report.filed += 1;
            }
            Err(error) if error.downcast_ref::<Handoff>().is_some() => {
                eprintln!("{} · {} · {error:#}", item.case_no, item.label());
                report.manual += 1;
            }
            Err(error) => {
                eprintln!("{} · {} · {error:#}", item.case_no, item.label());
                report.failures += 1;
            }
        }
    }
    println!(
        "{} document(s) filed into {code}, {} already filed",
        report.filed, report.duplicates
    );
    Ok(())
}

/// Everyone the accepted notices were mailed to, as the two seed documents
/// that record them: a Person per primary email, and a Relationship Log row
/// per person per filing. Firm-wide conflicts data — each log row names the
/// Project whose filing produced it, and nothing grants a role on it.
#[derive(Debug, Default)]
struct ServiceList {
    people: Vec<serde_json::Value>,
    entries: Vec<serde_json::Value>,
}

impl ServiceList {
    fn from_notices<'a>(project: &str, notices: impl Iterator<Item = &'a Notice>) -> Self {
        let mut emails = BTreeSet::new();
        let mut rows = BTreeSet::new();
        let mut list = Self::default();
        for notice in notices {
            let source = match &notice.doc_no {
                Some(doc_no) => format!("per D.I. {doc_no}"),
                None => match notice.filed_at {
                    Some(date) => format!("per the notice filed {date}"),
                    None => "per a notice of electronic filing".to_string(),
                },
            };
            for entry in &notice.service_list {
                let Some(email) = entry.primary_email() else {
                    continue;
                };
                if emails.insert(email.to_string()) {
                    list.people.push(serde_json::json!({
                        "email": email,
                        "name": entry.name,
                    }));
                }
                let (action, detail) = match &entry.party {
                    Some(party) => (
                        "appears_for",
                        format!(
                            "{} appears for {party} in {}, {source}",
                            entry.name, notice.case_no
                        ),
                    ),
                    None => (
                        "on_service_list",
                        format!(
                            "{} is on the service list in {}, {source}",
                            entry.name, notice.case_no
                        ),
                    ),
                };
                if rows.insert((email.to_string(), detail.clone())) {
                    list.entries.push(serde_json::json!({
                        "subject": { "email": email },
                        "action": action,
                        "detail": detail,
                        "project": { "code": project },
                    }));
                }
            }
        }
        list
    }

    fn is_empty(&self) -> bool {
        self.people.is_empty()
    }

    fn print(&self) {
        if self.is_empty() {
            return;
        }
        println!(
            "service list: {} person(s), {} relationship log row(s)",
            self.people.len(),
            self.entries.len()
        );
        for entry in &self.entries {
            println!(
                "  {} · {}",
                entry["subject"]["email"].as_str().unwrap_or_default(),
                entry["detail"].as_str().unwrap_or_default()
            );
        }
    }

    /// People first, so every log row's subject exists when it is written.
    async fn persist(
        &self,
        host: Option<&str>,
    ) -> Result<(crate::remote::SeedCounts, crate::remote::SeedCounts)> {
        let people =
            crate::remote::import_records(host, "person", &["email"], &self.people).await?;
        let entries = crate::remote::import_records(
            host,
            "relationship_log",
            &["subject_id", "action", "detail"],
            &self.entries,
        )
        .await?;
        Ok((people, entries))
    }
}

/// Write `bytes` beside `path` and rename into place, so an interrupted run
/// never leaves a partial PDF that a later run would mistake for the download.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let temp = PathBuf::from(format!("{}.tmp-{}", path.display(), uuid::Uuid::now_v7()));
    std::fs::write(&temp, bytes).with_context(|| format!("write {}", temp.display()))?;
    std::fs::rename(&temp, path).with_context(|| format!("publish {}", path.display()))
}

/// Where docket ingest saves a filing's bytes: Docket Filings, whatever the
/// entry's kind (LAW-189).
fn docket_bytes(root: &Path, slug: &str) -> PathBuf {
    root.join(crate::document_sync::DOCUMENT_DIRECTORY).join(
        crate::document_layout::bytes_relative(
            Path::new(slug),
            store::document_pointers::MatterFolder::DocketFilings,
        ),
    )
}

/// The bytes of one planned document: from the claims agent's site when the
/// notice is one of theirs, from the free look when it is the court's. An
/// agent link the site blocks falls back to the same entry's free look;
/// when neither yields a PDF, the document is handed to a person.
async fn fetch_document(
    free_look: &dyn FreeLook,
    agent: &dyn AgentFetch,
    free_looks: &BTreeMap<String, String>,
    item: &Item,
) -> Result<(Vec<u8>, &'static str)> {
    if let Some(agent_url) = item.agent_url.as_deref() {
        match agent.fetch(agent_url).await {
            Ok(bytes) => return Ok((bytes, "from the claims agent")),
            Err(error) => {
                eprintln!(
                    "{} · {} · claims-agent link refused: {error:#}",
                    item.case_no,
                    item.label()
                );
                if let Some(url) = free_looks.get(&unpadded(&item.doc_no)) {
                    println!(
                        "{} · {} · falling back to the CM/ECF free look for D.I. {}",
                        item.case_no,
                        item.label(),
                        item.doc_no
                    );
                    match free_look.fetch(url).await {
                        Ok(bytes) => return Ok((bytes, "from the free-look fallback")),
                        Err(fallback_error) => {
                            eprintln!(
                                "{} · {} · the free-look fallback failed too: {fallback_error:#}",
                                item.case_no,
                                item.label()
                            );
                        }
                    }
                }
                return Err(Handoff::Manual {
                    doc_no: item.doc_no.clone(),
                    url: agent_url.to_string(),
                }
                .into());
            }
        }
    }
    let url = item
        .free_look_url
        .as_deref()
        .ok_or_else(|| anyhow!("no free-look link"))?;
    free_look
        .fetch(url)
        .await
        .map(|bytes| (bytes, "from the free-look link"))
}

async fn file_one(
    root: &Path,
    client: &crate::remote::DocumentClient,
    free_look: &dyn FreeLook,
    agent: &dyn AgentFetch,
    free_looks: &BTreeMap<String, String>,
    item: &Item,
    filed: &mut Filed,
) -> Result<PathBuf> {
    let slug = item.slug();
    let target = docket_bytes(root, &slug);
    // A download saved before the matter folders sits at the slug itself; a
    // free-look link is single-use, so those bytes are read, not refetched.
    let legacy = crate::document_sync::document_path_for_slug(root, Path::new(&slug));
    let bytes = if target.is_file() {
        std::fs::read(&target).with_context(|| format!("read {}", target.display()))?
    } else if legacy.is_file() {
        std::fs::read(&legacy).with_context(|| format!("read {}", legacy.display()))?
    } else {
        let (bytes, source) = fetch_document(free_look, agent, free_looks, item).await?;
        let parent = target
            .parent()
            .ok_or_else(|| anyhow!("{} has no parent", target.display()))?;
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        crate::document_sync::ensure_managed_copy_target(root, &target)?;
        // The belt under every fetcher's own check: nothing that is not a
        // PDF is ever saved under a `.pdf` name (LAW-159).
        if !is_pdf(&bytes) {
            return Err(anyhow!(
                "the download is not a PDF; it may be a block page saved under a PDF name"
            ));
        }
        write_atomically(&target, &bytes)?;
        println!("saved {} ({source})", target.display());
        bytes
    };
    let metadata = serde_json::json!({
        "source": if item.agent_url.is_some() {
            "claims_agent_notice"
        } else {
            "cm_ecf_notice"
        },
        "case_number": item.case_no,
        "docket_entry": item.doc_no,
        "attachment": item.attachment,
        "original_filename": item.original_filename,
    });
    let mut pointer = client
        .upload_bytes(
            &item.filename,
            &bytes,
            &item.kind,
            None,
            Some(&item.description),
            Some("application/pdf"),
            Some(&slug),
            Some(metadata),
        )
        .await?;
    pointer.folder = crate::document_layout::recorded_folder(
        &pointer.kind,
        store::document_pointers::MatterFolder::DocketFilings,
    );
    let pointer_path = crate::document_sync::write_uploaded_pointer(root, &slug, &pointer)?;
    filed.record(&slug);
    Ok(pointer_path)
}

#[cfg(test)]
mod tests {
    use super::claims_agent::AgentFetchError;
    use super::{
        case_folder, docket_for, docket_key, document_filename, embedded_document, entry_kind,
        filing_slug, ingest, is_pdf, suggested_kind, text_slug, AgentFetch, FreeLook, Options,
        Report,
    };

    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::LazyLock;

    use crate::credentials::{self, Credentials, HostCredential};
    use uuid::Uuid;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// The flat Docket Filings name the bankruptcy fixture files under.
    const FILED: &str = "DKT 0029 - Transfer of Claim. Transfer Agreement 3001 (e) 2 Transferor- Acme Lending Co (Claim No. 7) To Harbor Recovery Fund. File.pdf";

    /// Serves fixed bytes for any link and counts how often it was asked.
    struct FakeFreeLook {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl FreeLook for FakeFreeLook {
        async fn fetch(&self, url: &str) -> anyhow::Result<Vec<u8>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(format!("%PDF-1.7 synthetic {url}").into_bytes())
        }
    }

    /// Serves PDFs or a CloudFront block page, and counts how often the
    /// agent's site was asked.
    struct FakeAgentFetch {
        calls: AtomicUsize,
        /// `true` answers every link with a PDF; `false` answers with the
        /// block page the agent sites put in front of a scripted fetch.
        pdfs: bool,
        /// `true` makes every link fail outright, as a blocked request does
        /// before any bytes arrive.
        fails: bool,
    }

    #[async_trait::async_trait]
    impl AgentFetch for FakeAgentFetch {
        async fn fetch(&self, url: &str) -> Result<Vec<u8>, AgentFetchError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fails {
                return Err(AgentFetchError::Other(anyhow::anyhow!(
                    "HTTP 403 Forbidden"
                )));
            }
            if self.pdfs {
                Ok(format!("%PDF-1.7 agent {url}").into_bytes())
            } else {
                Ok(b"<html>ERROR: The request could not be satisfied.</html>".to_vec())
            }
        }
    }

    /// The default agent for tests that never reach the agent's site.
    static NO_AGENT: FakeAgentFetch = FakeAgentFetch {
        calls: AtomicUsize::new(0),
        pdfs: true,
        fails: false,
    };

    /// A free look that always fails, as an expired CM/ECF link does.
    struct FailingFreeLook;

    #[async_trait::async_trait]
    impl FreeLook for FailingFreeLook {
        async fn fetch(&self, _url: &str) -> anyhow::Result<Vec<u8>> {
            Err(anyhow::anyhow!("free look expired"))
        }
    }

    // `NAVIGATOR_CREDENTIALS_FILE` is process-global env, so tests that set
    // it must not run concurrently — the discipline `cli::remote` uses.
    static CREDENTIALS_ENV_LOCK: LazyLock<tokio::sync::Mutex<()>> =
        LazyLock::new(|| tokio::sync::Mutex::new(()));

    struct CredentialsEnv {
        _dir: tempfile::TempDir,
    }

    impl CredentialsEnv {
        fn logged_in(base: &str) -> Self {
            Self::with_token(base, "test-token")
        }

        fn with_token(base: &str, token: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("navigator.json");
            let mut creds = Credentials::default();
            creds.set(
                base,
                HostCredential {
                    token: token.into(),
                    person_email: Some("lawyer@neonlaw.com".into()),
                    role: Some("lawyer".into()),
                    expires_at: i64::MAX,
                },
            );
            credentials::save(&path, &creds).unwrap();
            std::env::set_var("NAVIGATOR_CREDENTIALS_FILE", path);
            Self { _dir: dir }
        }
    }

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/nef")
            .join(name)
    }

    fn checkout(manifest: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("navigator.yaml"), manifest).unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        assert!(init.success());
        dir
    }

    #[test]
    fn a_notice_case_number_matches_its_docket_with_or_without_initials() {
        let dockets = vec!["9:26-cv-00042".to_string(), "26-10042-ZZZ".to_string()];
        assert_eq!(
            docket_for("9:26-cv-00042-XYZ", &dockets),
            Some("9:26-cv-00042")
        );
        assert_eq!(docket_for("26-10042-zzz", &dockets), Some("26-10042-ZZZ"));
        assert_eq!(docket_for("9:26-cv-00043-XYZ", &dockets), None);
        assert_eq!(docket_for("9:26-cv-000421", &dockets), None);
        assert_eq!(case_folder("9:26-cv-00042"), "9-26-cv-00042");
    }

    #[test]
    fn a_document_is_named_by_its_entry_attachment_and_text() {
        assert_eq!(
            document_filename("12", None, "MOTION to Dismiss for Failure to State a Claim"),
            "DKT 0012 - MOTION to Dismiss for Failure to State a Claim.pdf"
        );
        assert_eq!(
            document_filename("12", Some(1), "Exhibit A: Supply/Agreement"),
            "DKT 0012-1 - Exhibit A- Supply-Agreement.pdf"
        );
        assert_eq!(
            document_filename("x", None, "Motion"),
            "di-x-motion.pdf",
            "a non-numeric entry keeps the older form"
        );
        assert_eq!(
            filing_slug("9-26-cv-1", "DKT 0012 - A.pdf"),
            "DKT 0012 - A.pdf"
        );
        assert_eq!(
            filing_slug("9-26-cv-1", "di-12-a.pdf"),
            "docket/9-26-cv-1/di-12-a.pdf"
        );
        assert_eq!(text_slug("3 (three) copies"), "three-copies");
        assert_eq!(text_slug("12 34"), "entry");
        assert_eq!(docket_key("di-12-motion.pdf"), Some(("12".into(), None)));
        assert_eq!(
            docket_key("di-12-1-exhibit-a.pdf"),
            Some(("12".into(), Some(1)))
        );
        assert_eq!(docket_key("motion.pdf"), None);
    }

    #[test]
    fn the_suggested_kind_reads_the_entry() {
        assert_eq!(suggested_kind("MOTION to Dismiss", None), "pleading");
        assert_eq!(suggested_kind("ORDER granting", None), "pleading");
        assert_eq!(
            suggested_kind("TRANSCRIPT of Proceedings", None),
            "transcript"
        );
        assert_eq!(suggested_kind("MOTION", Some("Exhibit A")), "exhibit");
        assert_eq!(suggested_kind("MOTION", Some("Proposed Order")), "pleading");
    }

    #[test]
    fn a_free_look_wrapper_page_names_its_pdf() {
        assert_eq!(
            embedded_document(
                "<html><iframe src=\"/cgi-bin/show_temp.pl?file=a.pdf&amp;type=1\"></iframe>"
            )
            .as_deref(),
            Some("/cgi-bin/show_temp.pl?file=a.pdf&type=1")
        );
        assert_eq!(embedded_document("<p>expired</p>"), None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_dry_run_plans_without_downloading_and_skips_other_dockets() {
        let dir =
            checkout("host: navigator.example.com\nproject: acme\ndockets:\n  - 9:26-cv-00042\n");
        let free_look = FakeFreeLook {
            calls: AtomicUsize::new(0),
        };
        let report = ingest(
            dir.path(),
            &Options {
                notices: vec![
                    fixture("district-motion.eml"),
                    fixture("bankruptcy-transfer.eml"),
                ],
                dry_run: true,
                ..Options::default()
            },
            &free_look,
            &NO_AGENT,
        )
        .await
        .unwrap();

        assert_eq!(
            report,
            Report {
                skipped_notices: 1,
                ..Report::default()
            }
        );
        assert_eq!(free_look.calls.load(Ordering::SeqCst), 0);
        assert!(!dir.path().join("documents").exists());
    }

    #[test]
    fn the_entry_kind_reads_the_first_word_of_the_docket_text() {
        assert_eq!(entry_kind("MOTION to Dismiss"), "motion");
        assert_eq!(entry_kind("ORDER granting"), "order");
        assert_eq!(entry_kind("Transfer of Claim."), "other");
        assert_eq!(entry_kind("Notice of hearing"), "notice");
        assert_eq!(entry_kind("ANSWER to complaint"), "pleading");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_skip_pattern_and_only_filter_notices() {
        let dir = checkout(
            "host: navigator.example.com\nproject: acme\ndockets:\n  - 9:26-cv-00042\n  - 26-10042\n",
        );
        let free_look = FakeFreeLook {
            calls: AtomicUsize::new(0),
        };
        let notices = vec![
            fixture("district-motion.eml"),
            fixture("bankruptcy-transfer.eml"),
        ];
        let skipped = ingest(
            dir.path(),
            &Options {
                notices: notices.clone(),
                dry_run: true,
                skip_patterns: vec!["TRANSFER OF CLAIM".into()],
                ..Options::default()
            },
            &free_look,
            &NO_AGENT,
        )
        .await
        .unwrap();
        assert_eq!(skipped.skipped_notices, 1);

        let only = ingest(
            dir.path(),
            &Options {
                notices,
                dry_run: true,
                only: vec!["29".into()],
                ..Options::default()
            },
            &free_look,
            &NO_AGENT,
        )
        .await
        .unwrap();
        assert_eq!(only.skipped_notices, 1);
    }

    /// Mount the Project lookup and an upload that answers with a fresh
    /// pointer per request.
    async fn upload_server(project_id: Uuid) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": project_id, "code": "acme"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/app/api/projects/{project_id}/documents")))
            .respond_with(move |request: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                ResponseTemplate::new(201).set_body_json(serde_json::json!({
                    "kind": body["kind"],
                    "visibility": "internal",
                    "current_version": {
                        "version": 1,
                        "asset_id": Uuid::now_v7(),
                        "created_at": "2026-03-04T12:00:00Z",
                        "sha256": "0".repeat(64),
                        "size_bytes": 10
                    }
                }))
            })
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/app/api/seed"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "model": "person", "created": 1, "updated": 0, "unchanged": 0, "records": []
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/app/api/projects/{project_id}/docket")))
            .respond_with(move |request: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                ResponseTemplate::new(201).set_body_json(serde_json::json!({
                    "id": Uuid::now_v7(),
                    "entry_number": body["entry_number"],
                    "kind": body["kind"],
                    "title": body["title"],
                }))
            })
            .mount(&server)
            .await;
        server
    }

    /// The docket entries a run recorded, as posted.
    async fn docket_entries(server: &MockServer) -> Vec<serde_json::Value> {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.url.path().ends_with("/docket"))
            .map(|request| serde_json::from_slice(&request.body).unwrap())
            .collect()
    }

    /// The seed documents a run posted, as `(model, yaml)`.
    async fn seeds(server: &MockServer) -> Vec<(String, String)> {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.url.path() == "/app/api/seed")
            .map(|request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                (
                    body["model"].as_str().unwrap().to_string(),
                    body["yaml"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn apply_mode_saves_bytes_files_the_document_and_a_rerun_skips_it() {
        let project_id = Uuid::now_v7();
        let server = upload_server(project_id).await;
        let dir = checkout("host: navigator.example.com\nproject: acme\ndockets:\n  - 26-10042\n");
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let _env = CredentialsEnv::logged_in(&server.uri());
        let free_look = FakeFreeLook {
            calls: AtomicUsize::new(0),
        };
        let options = Options {
            notices: vec![fixture("bankruptcy-transfer.eml")],
            host: Some(server.uri()),
            ..Options::default()
        };

        let report = ingest(dir.path(), &options, &free_look, &NO_AGENT)
            .await
            .unwrap();
        assert_eq!(report.filed, 1, "{report:?}");
        assert_eq!(report.failures, 0, "{report:?}");
        assert_eq!(report.entries, 1, "one docket entry per notice: {report:?}");
        assert_eq!(free_look.calls.load(Ordering::SeqCst), 1);
        let entries = docket_entries(&server).await;
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0]["docket_number"], "26-10042");
        assert_eq!(entries[0]["entry_number"], "29");
        assert_eq!(entries[0]["kind"], "other");
        assert_eq!(entries[0]["party"], "Harbor Recovery Fund");
        assert_eq!(entries[0]["filed_or_served_on"], "2026-03-05");
        assert_eq!(entries[0]["slug"], FILED);
        assert!(entries[0]["title"]
            .as_str()
            .unwrap()
            .starts_with("Transfer of Claim"));
        let bytes = dir.path().join(format!("documents/Docket Filings/{FILED}"));
        assert!(std::fs::read(&bytes).unwrap().starts_with(b"%PDF"));
        assert!(dir.path().join(format!("blobs/{FILED}.yaml")).is_file());
        let uploads: Vec<serde_json::Value> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.url.path().ends_with("/documents"))
            .map(|request| serde_json::from_slice(&request.body).unwrap())
            .collect();
        assert_eq!(uploads.len(), 1);
        assert_eq!(uploads[0]["kind"], "pleading");
        assert_eq!(uploads[0]["slug"], FILED);
        assert_eq!(uploads[0]["content_type"], "application/pdf");

        let posted = seeds(&server).await;
        let models: Vec<&str> = posted.iter().map(|(model, _)| model.as_str()).collect();
        assert_eq!(models, vec!["person", "relationship_log"]);
        assert!(posted[0].1.contains("email: quentin.quill@example.com"));
        assert!(posted[1].1.contains(
            "Quentin Quill appears for Creditor Harbor Recovery Fund in 26-10042-ZZZ, per D.I. 29"
        ));
        assert!(
            !posted[0].1.contains("acme"),
            "the person seed never names the Project"
        );
        assert!(
            posted[1].1.contains("project:\n    code: acme"),
            "each relationship_log row names the Project it came from: {}",
            posted[1].1
        );

        let rerun = ingest(dir.path(), &options, &free_look, &NO_AGENT)
            .await
            .unwrap();
        assert_eq!(rerun.filed, 0);
        assert_eq!(rerun.duplicates, 1);
        assert_eq!(
            free_look.calls.load(Ordering::SeqCst),
            1,
            "no second free look"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bytes_left_by_a_failed_run_are_reused_not_downloaded_again() {
        let project_id = Uuid::now_v7();
        let server = upload_server(project_id).await;
        let dir = checkout("host: navigator.example.com\nproject: acme\ndockets:\n  - 26-10042\n");
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let _env = CredentialsEnv::logged_in(&server.uri());
        let folder = dir.path().join("documents");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(FILED), b"%PDF-1.7 left behind").unwrap();
        let free_look = FakeFreeLook {
            calls: AtomicUsize::new(0),
        };

        let report = ingest(
            dir.path(),
            &Options {
                notices: vec![fixture("bankruptcy-transfer.eml")],
                host: Some(server.uri()),
                ..Options::default()
            },
            &free_look,
            &NO_AGENT,
        )
        .await
        .unwrap();
        assert_eq!(report.filed, 1);
        assert_eq!(free_look.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    #[expect(
        clippy::too_many_lines,
        reason = "real upload, retained identity, docket, metadata, and checkout gate form one regression"
    )]
    async fn attachments_and_a_retained_filing_pass_the_real_scoped_integrity_gate() {
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let db = store::test_support::mem_surreal().await;
        let project = store::projects::create(
            &db,
            &store::projects::NewProject {
                code: "acme".into(),
                name: "Acme".into(),
                status: "open".into(),
                entity_id: store::test_support::seed_entity(&db).await,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let lawyer = store::persons::create(
            &db,
            &store::persons::NewPerson::with_role(
                "Synthetic Lawyer",
                "lawyer@example.com",
                store::persons::Role::Lawyer,
            ),
        )
        .await
        .unwrap();
        store::projects::designate_dri_in_surreal(
            &db,
            project.id,
            lawyer.id,
            store::projects::DriSide::Lawyer,
        )
        .await
        .unwrap();
        let mut state = portal::test_support::app_state(db.clone()).await;
        state.policy = portal::policy::PolicyClient::embedded().unwrap();
        let mut session =
            portal::SessionData::fresh("synthetic-lawyer", store::persons::Role::Lawyer);
        session.person_id = Some(lawyer.id);
        session.email = Some("lawyer@example.com".into());
        session.source = portal::SessionSource::Cli;
        let token = state.sessions.encode(&session);
        let mut ci_session = session.clone();
        ci_session.source = portal::SessionSource::Ci;
        ci_session.scope = Some(portal::session::SessionScope::Document(
            portal::session::DocumentScope {
                project_id: project.id,
                project_code: "acme".into(),
            },
        ));
        let ci_token = state.sessions.encode(&ci_session);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, portal::router(state)).await.unwrap();
        });
        let _env = CredentialsEnv::with_token(&base, &token);
        let dir = checkout(&format!(
            "host: {base}\nproject: acme\ndockets:\n  - 9:26-cv-00042\n"
        ));
        let client =
            crate::remote::DocumentClient::with_credential(base.clone(), token.clone(), "acme")
                .await
                .unwrap();
        let retained_slug = "docket/9-26-cv-00042/di-12-motion.pdf";
        let pointer = client
            .upload_bytes(
                "motion.pdf",
                b"%PDF synthetic main",
                "pleading",
                None,
                None,
                Some("application/pdf"),
                Some(retained_slug),
                None,
            )
            .await
            .unwrap();
        crate::document_sync::write_uploaded_pointer(dir.path(), retained_slug, &pointer).unwrap();
        let retained_asset = pointer.current_version.asset_id;
        let notice =
            super::nef::parse_eml(&std::fs::read(fixture("district-motion.eml")).unwrap()).unwrap();
        let case = store::cases::open_case(
            &db,
            &store::cases::NewCase {
                project_id: project.id,
                caption: "Synthetic caption",
                forum: None,
                jurisdiction: None,
                docket_number: Some("9:26-cv-00042"),
                judge: None,
                posture: "plaintiff",
            },
        )
        .await
        .unwrap();
        store::cases::record_entry(
            &db,
            &store::cases::NewDocketEntry {
                case_id: case.id,
                entry_number: "12",
                kind: store::cases::EntryKind::Motion,
                title: &notice.docket_text,
                party: None,
                filed_or_served_on: None,
                scheduled_on: None,
                supersedes: None,
                document_asset_id: None,
                notation_id: None,
            },
        )
        .await
        .unwrap();
        let items = super::items_for(&notice, "9:26-cv-00042", None);
        assert_eq!(items.len(), 3);
        for item in items.iter().skip(1) {
            let target = super::docket_bytes(dir.path(), &item.slug());
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, b"%PDF identical synthetic attachment bytes").unwrap();
        }
        let free_look = FakeFreeLook {
            calls: AtomicUsize::new(0),
        };
        let report = ingest(
            dir.path(),
            &Options {
                notices: vec![fixture("district-motion.eml")],
                host: Some(base.clone()),
                ..Options::default()
            },
            &free_look,
            &NO_AGENT,
        )
        .await
        .unwrap();
        assert_eq!(report.entries, 3, "{report:?}");
        assert_eq!(report.failures, 0, "{report:?}");
        assert_eq!(
            report.filed, 2,
            "identical bytes on distinct entries need distinct Document Identities: {report:?}"
        );
        assert_eq!(free_look.calls.load(Ordering::SeqCst), 0);
        let entries = store::cases::docket_for_project(&db, project.id)
            .await
            .unwrap();
        assert_eq!(entries.len(), 3);
        assert!(entries
            .iter()
            .any(|entry| entry.entry_number == "12"
                && entry.document_asset_id == Some(retained_asset)));
        let ci_client =
            crate::remote::DocumentClient::with_credential(base.clone(), ci_token, "acme")
                .await
                .unwrap();
        let integrity = ci_client.integrity(false).await.unwrap();
        assert_eq!(integrity.assets.len(), 3);
        assert!(
            integrity
                .assets
                .iter()
                .all(|asset| asset.docket_name_matches == Some(true)),
            "{:#?}",
            integrity.assets
        );
        let metadata:Vec<_> = integrity.assets.iter().map(|asset| serde_json::json!({"asset_id":asset.asset_id,"slug":asset.slug,"docket_name_matches":asset.docket_name_matches})).collect();
        super::super::renumber::validate_checkout(dir.path(), &metadata).unwrap();
        assert!(!dir
            .path()
            .join("blobs")
            .join(format!("{retained_slug}.yaml"))
            .exists());
        let ids: std::collections::BTreeSet<_> = integrity
            .assets
            .iter()
            .map(|asset| asset.asset_id)
            .collect();
        let rerun = ingest(
            dir.path(),
            &Options {
                notices: vec![fixture("district-motion.eml")],
                host: Some(base),
                ..Options::default()
            },
            &free_look,
            &NO_AGENT,
        )
        .await
        .unwrap();
        assert_eq!(rerun.filed, 0, "{rerun:?}");
        assert_eq!(rerun.entries, 3, "{rerun:?}");
        assert_eq!(rerun.failures, 0, "{rerun:?}");
        let after = ci_client.integrity(false).await.unwrap();
        assert_eq!(
            after
                .assets
                .iter()
                .map(|asset| asset.asset_id)
                .collect::<std::collections::BTreeSet<_>>(),
            ids
        );
        assert!(after
            .assets
            .iter()
            .all(|asset| asset.docket_name_matches == Some(true)));
        let metadata:Vec<_> = after.assets.iter().map(|asset| serde_json::json!({"asset_id":asset.asset_id,"slug":asset.slug,"docket_name_matches":asset.docket_name_matches})).collect();
        super::super::renumber::validate_checkout(dir.path(), &metadata).unwrap();
        server.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn our_own_filing_from_the_drafts_lane_is_recognised_named() {
        let dir = checkout("host: navigator.example.com\nproject: acme\ndockets:\n  - 26-10042\n");
        let pointer = store::document_pointers::DocumentPointer {
            kind: "pleading".into(),
            visibility: "internal".into(),
            folder: None,
            current_version: store::document_pointers::PointerVersion {
                version: 1,
                asset_id: Uuid::now_v7(),
                created_at: "2026-03-05T12:00:00Z".into(),
                sha256: "1".repeat(64),
                size_bytes: 10,
                canonical_url: None,
                checked_on: None,
            },
            previous_version: None,
            authority_id: None,
            docusign_envelope_id: None,
            xero_invoice_id: None,
            signing: None,
            notarization: Vec::new(),
            service: Vec::new(),
            evidence: None,
        };
        crate::document_sync::write_uploaded_pointer(
            dir.path(),
            "filings/transfer-of-claim-7.pdf",
            &pointer,
        )
        .unwrap();
        let free_look = FakeFreeLook {
            calls: AtomicUsize::new(0),
        };

        let report = ingest(
            dir.path(),
            &Options {
                notices: vec![fixture("bankruptcy-transfer.eml")],
                ..Options::default()
            },
            &free_look,
            &NO_AGENT,
        )
        .await
        .unwrap();
        assert_eq!(report.duplicates, 1);
        assert_eq!(free_look.calls.load(Ordering::SeqCst), 0);
    }

    /// A pointer filed under either name for entry 29 stops a re-filing.
    async fn a_filing_under_this_name_is_not_filed_again(slug: &str) {
        let dir = checkout("host: navigator.example.com\nproject: acme\ndockets:\n  - 26-10042\n");
        let pointer = store::document_pointers::DocumentPointer {
            kind: "pleading".into(),
            visibility: "internal".into(),
            folder: None,
            current_version: store::document_pointers::PointerVersion {
                version: 1,
                asset_id: Uuid::now_v7(),
                created_at: "2026-03-05T12:00:00Z".into(),
                sha256: "2".repeat(64),
                size_bytes: 10,
                canonical_url: None,
                checked_on: None,
            },
            previous_version: None,
            authority_id: None,
            docusign_envelope_id: None,
            xero_invoice_id: None,
            signing: None,
            notarization: Vec::new(),
            service: Vec::new(),
            evidence: None,
        };
        crate::document_sync::write_uploaded_pointer(dir.path(), slug, &pointer).unwrap();
        let free_look = FakeFreeLook {
            calls: AtomicUsize::new(0),
        };
        let report = ingest(
            dir.path(),
            &Options {
                notices: vec![fixture("bankruptcy-transfer.eml")],
                ..Options::default()
            },
            &free_look,
            &NO_AGENT,
        )
        .await
        .unwrap();
        assert_eq!(report.duplicates, 1, "{slug}: {report:?}");
        assert_eq!(report.filed, 0);
        assert_eq!(free_look.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_filing_made_under_the_older_di_name_is_recognised() {
        a_filing_under_this_name_is_not_filed_again(
            "docket/26-10042/di-29-transfer-of-claim-transfer-agreement-3001.pdf",
        )
        .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_filing_made_under_a_dkt_name_is_recognised_whatever_its_title() {
        a_filing_under_this_name_is_not_filed_again("DKT 0029 - Retitled by hand.pdf").await;
    }

    #[test]
    fn only_pdf_bytes_pass_the_magic_byte_check() {
        assert!(is_pdf(b"%PDF-1.7"));
        assert!(!is_pdf(
            b"<html>ERROR: The request could not be satisfied.</html>"
        ));
        assert!(!is_pdf(b""));
    }

    /// A claims-agent notice files every link that names a docket entry,
    /// records its Docket Entries through the same door as a court notice,
    /// and hands the link that names no entry to a person.
    #[tokio::test(flavor = "current_thread")]
    async fn a_claims_agent_notice_files_its_docket_links_and_hands_the_rest_to_a_person() {
        let project_id = Uuid::now_v7();
        let server = upload_server(project_id).await;
        let dir = checkout("host: navigator.example.com\nproject: acme\ndockets:\n  - 26-10042\n");
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let _env = CredentialsEnv::logged_in(&server.uri());
        let agent = FakeAgentFetch {
            calls: AtomicUsize::new(0),
            pdfs: true,
            fails: false,
        };

        let report = ingest(
            dir.path(),
            &Options {
                notices: vec![fixture("claims-agent-notice.eml")],
                host: Some(server.uri()),
                ..Options::default()
            },
            &FakeFreeLook {
                calls: AtomicUsize::new(0),
            },
            &agent,
        )
        .await
        .unwrap();

        assert_eq!(
            report,
            Report {
                filed: 2,
                manual: 1,
                entries: 2,
                ..Report::default()
            }
        );
        assert_eq!(agent.calls.load(Ordering::SeqCst), 2);
        for name in [
            "DKT 0029 - Notice of Transfer of Claim.pdf",
            "DKT 0033 - Notice of Amendments.pdf",
        ] {
            let saved = dir.path().join(format!("documents/Docket Filings/{name}"));
            assert!(
                std::fs::read(&saved).unwrap().starts_with(b"%PDF"),
                "{name}"
            );
            let blob = dir.path().join(format!("blobs/{name}.yaml"));
            assert!(blob.is_file(), "{name}: {blob:?}");
        }
        let uploads: Vec<serde_json::Value> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| {
                request.method == "POST" && request.url.path().ends_with("/documents")
            })
            .map(|request| serde_json::from_slice(&request.body).unwrap())
            .collect();
        assert_eq!(uploads.len(), 2, "{uploads:?}");
        let metadata = &uploads[0]["metadata"];
        assert_eq!(metadata["source"], "claims_agent_notice");
        let entries = docket_entries(&server).await;
        assert_eq!(entries.len(), 2, "{entries:?}");
        assert_eq!(entries[0]["entry_number"], "29");
        assert_eq!(entries[0]["kind"], "notice");
        assert!(
            entries[0].get("slug").is_some(),
            "a filed document is linked to its entry"
        );
        assert_eq!(entries[1]["entry_number"], "33");
    }

    /// When no download succeeds, the run still records the Docket Entries —
    /// source pending, no `slug` — and hands both links to a person. Nothing
    /// is a failure: the gap shows on the matter page.
    #[tokio::test(flavor = "current_thread")]
    async fn a_claims_agent_notice_records_source_pending_entries_when_every_download_fails() {
        let project_id = Uuid::now_v7();
        let server = upload_server(project_id).await;
        let dir = checkout("host: navigator.example.com\nproject: acme\ndockets:\n  - 26-10042\n");
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let _env = CredentialsEnv::logged_in(&server.uri());
        let agent = FakeAgentFetch {
            calls: AtomicUsize::new(0),
            pdfs: false,
            fails: true,
        };

        let report = ingest(
            dir.path(),
            &Options {
                notices: vec![fixture("claims-agent-notice.eml")],
                host: Some(server.uri()),
                ..Options::default()
            },
            &FakeFreeLook {
                calls: AtomicUsize::new(0),
            },
            &agent,
        )
        .await
        .unwrap();

        assert_eq!(
            report,
            Report {
                manual: 3,
                entries: 2,
                ..Report::default()
            }
        );
        assert_eq!(agent.calls.load(Ordering::SeqCst), 2);
        assert!(!dir.path().join("documents/Docket Filings").exists());
        let entries = docket_entries(&server).await;
        assert_eq!(entries.len(), 2, "{entries:?}");
        for entry in &entries {
            assert!(
                entry.get("slug").is_none(),
                "an unfetched document leaves its entry source pending: {entry}"
            );
        }
        assert_eq!(entries[0]["entry_number"], "29");
        assert_eq!(entries[1]["entry_number"], "33");
    }

    /// A link the agent's site blocks falls back to the same entry's
    /// CM/ECF free look, and an entry with no free look is handed to a
    /// person. A document the free look already filed is not filed twice.
    #[tokio::test(flavor = "current_thread")]
    async fn a_blocked_agent_link_falls_back_to_the_free_look_of_the_same_entry() {
        let project_id = Uuid::now_v7();
        let server = upload_server(project_id).await;
        let dir = checkout("host: navigator.example.com\nproject: acme\ndockets:\n  - 26-10042\n");
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let _env = CredentialsEnv::logged_in(&server.uri());
        let agent = FakeAgentFetch {
            calls: AtomicUsize::new(0),
            pdfs: false,
            fails: true,
        };
        let free_look = FakeFreeLook {
            calls: AtomicUsize::new(0),
        };

        let report = ingest(
            dir.path(),
            &Options {
                notices: vec![
                    fixture("claims-agent-notice.eml"),
                    fixture("bankruptcy-transfer.eml"),
                ],
                host: Some(server.uri()),
                ..Options::default()
            },
            &free_look,
            &agent,
        )
        .await
        .unwrap();

        assert_eq!(
            report,
            Report {
                filed: 1,
                duplicates: 1,
                manual: 2,
                entries: 3,
                ..Report::default()
            },
            "the court's own notice names the same entry 29: the second item of the pair is a duplicate"
        );
        assert_eq!(agent.calls.load(Ordering::SeqCst), 2);
        assert_eq!(free_look.calls.load(Ordering::SeqCst), 1);
        assert!(dir
            .path()
            .join("documents/Docket Filings/DKT 0029 - Notice of Transfer of Claim.pdf")
            .is_file());
        let entries = docket_entries(&server).await;
        assert_eq!(entries.len(), 3, "{entries:?}");
        assert_eq!(entries[0]["entry_number"], "29");
        assert_eq!(entries[2]["entry_number"], "29");
    }

    /// An expired CM/ECF link is the ordinary case: the agent's link
    /// fails AND the entry's own free look fails. Both entries still
    /// land — source pending — and every link is handed to a person.
    #[tokio::test(flavor = "current_thread")]
    async fn when_the_free_look_fails_too_the_entries_stay_source_pending() {
        let project_id = Uuid::now_v7();
        let server = upload_server(project_id).await;
        let dir = checkout("host: navigator.example.com\nproject: acme\ndockets:\n  - 26-10042\n");
        let _lock = CREDENTIALS_ENV_LOCK.lock().await;
        let _env = CredentialsEnv::logged_in(&server.uri());
        let agent = FakeAgentFetch {
            calls: AtomicUsize::new(0),
            pdfs: false,
            fails: true,
        };

        let report = ingest(
            dir.path(),
            &Options {
                notices: vec![
                    fixture("claims-agent-notice.eml"),
                    fixture("bankruptcy-transfer.eml"),
                ],
                host: Some(server.uri()),
                ..Options::default()
            },
            &FailingFreeLook,
            &agent,
        )
        .await
        .unwrap();

        assert_eq!(report.filed, 0, "{report:?}");
        assert!(
            !dir.path().join("documents/Docket Filings").exists(),
            "nothing is saved when every source fails"
        );
        let entries = docket_entries(&server).await;
        assert_eq!(entries.len(), 3, "{entries:?}");
        for entry in &entries {
            assert!(
                entry.get("slug").is_none(),
                "an unfetched document leaves its entry source pending: {entry}"
            );
        }
        assert_eq!(entries[0]["entry_number"], "29");
        assert_eq!(entries[1]["entry_number"], "33");
        assert_eq!(entries[2]["entry_number"], "29");
    }
}
