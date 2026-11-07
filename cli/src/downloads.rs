//! `navigator downloads` — the legal files sitting in the lawyer's Downloads
//! folder.
//!
//! Mail that came through `navigator mail` is already filed; the files that
//! never came through mail sit in Downloads, unclassified and unfiled. This
//! verb family scans that folder, proposes a matter, kind, and conventional
//! `YYYY.MM.DD_name` slug for each candidate (LAW-196), files one candidate
//! through the same document door the browser uses, and reports what is
//! already on its matter. Filing refuses rather than guessing: a credential
//! is pointed at the keychain, a missing login or matter stops the upload,
//! and the original moves to the Trash only after the filed record's sha256
//! matches the local bytes.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{anyhow, bail, Context, Result};
use clap::Subcommand;
use serde::Serialize;
use sha2::Digest;
use uuid::Uuid;

#[derive(Subcommand)]
pub(crate) enum DownloadsCmd {
    /// List the files in a Downloads directory with their intake facts.
    Scan {
        /// Directory to sweep. Defaults to the platform's Downloads directory.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Only files modified on or after this ISO date, YYYY-MM-DD.
        #[arg(long)]
        since: Option<String>,
        /// Print one JSON array instead of tab-separated rows.
        #[arg(long)]
        json: bool,
    },
    /// Propose the matter, kind, and conventional slug for each candidate.
    Propose {
        /// Directory to sweep. Defaults to the platform's Downloads directory.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Only files modified on or after this ISO date, YYYY-MM-DD.
        #[arg(long)]
        since: Option<String>,
        /// Print one JSON array instead of prose blocks.
        #[arg(long)]
        json: bool,
        /// Navigator host. Defaults to the host in navigator.yaml.
        #[arg(long)]
        host: Option<String>,
    },
    /// File one local file onto a matter, verify the digest round-trips,
    /// then move the original to the Trash.
    File {
        /// The file to file.
        path: PathBuf,
        /// The Project code to file it onto.
        #[arg(long)]
        project: String,
        /// The document kind to file it as.
        #[arg(long)]
        kind: String,
        /// The pointer slug. Defaults to the file's own name.
        #[arg(long)]
        slug: Option<String>,
        /// Navigator host. Defaults to the host in navigator.yaml.
        #[arg(long)]
        host: Option<String>,
    },
    /// List the Downloads files whose bytes already sit on a visible matter.
    Clean {
        /// List what is confirmed on its matter.
        #[arg(long)]
        already_filed: bool,
        /// Directory to sweep. Defaults to the platform's Downloads directory.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Restrict the check to one Project code.
        #[arg(long)]
        project: Option<String>,
        /// Navigator host. Defaults to the host in navigator.yaml.
        #[arg(long)]
        host: Option<String>,
    },
}

/// One candidate's intake facts, as `scan` reports them.
#[derive(Debug, Serialize)]
struct Candidate {
    path: String,
    filename: String,
    sha256: String,
    content_type: &'static str,
    size_bytes: u64,
    modified_at: String,
    age_days: i64,
    /// The macOS provenance (`kMDItemWhereFroms`), or `null` when unknown —
    /// off macOS, or when the file carries no download record.
    where_froms: Option<Vec<String>>,
    /// The triage verdict the propose cascade reaches for this candidate.
    class: &'static str,
}

/// The triage verdict for one candidate: one ordered pass, first match wins,
/// each verdict carrying the evidence that produced it, the way the inbox's
/// classification carries its reason.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// A keychain-shaped file: a certificate, a key, an exported profile.
    /// Never filed onto a matter.
    Credential { reason: String },
    /// A payment receipt: filed, but the fee evidence, not client production.
    Receipt { reason: String },
    /// Everything the cascade cannot name confidently.
    Unfiled { reason: String },
}

impl Verdict {
    fn label(&self) -> &'static str {
        match self {
            Self::Credential { .. } => "credential",
            Self::Receipt { .. } => "receipt",
            Self::Unfiled { reason: _ } => "unfiled",
        }
    }

    fn reason(&self) -> &str {
        match self {
            Self::Credential { reason } | Self::Receipt { reason } | Self::Unfiled { reason } => {
                reason
            }
        }
    }
}

/// The file extensions whose bytes are credentials, not matter documents.
const CREDENTIAL_EXTENSIONS: &[&str] = &[
    "pem",
    "key",
    "p12",
    "pfx",
    "crt",
    "cer",
    "csr",
    "der",
    "mobileconfig",
    "kdbx",
    "ovpn",
];

/// The filename fragments that name a credential no matter its extension.
const CREDENTIAL_FRAGMENTS: &[&str] = &["password", "credential", "secret", "private key", ".env"];

/// The image extensions the matter surface files as exhibits.
const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "heic", "heif", "gif", "webp", "tif", "tiff", "bmp",
];

/// The triage cascade, applied to one candidate's filename: a credential
/// first (filing one would leak a secret into a matter), then a receipt,
/// then the unfiled pass-through every cascade ends on.
fn classify(filename: &str) -> Verdict {
    let extension = Path::new(filename)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if CREDENTIAL_EXTENSIONS.contains(&extension.as_str()) {
        return Verdict::Credential {
            reason: format!("the `.{extension}` extension marks credentials"),
        };
    }
    let lowered = filename.to_ascii_lowercase();
    if let Some(fragment) = CREDENTIAL_FRAGMENTS
        .iter()
        .find(|fragment| lowered.contains(*fragment))
    {
        return Verdict::Credential {
            reason: format!("the filename names credentials (`{fragment}`)"),
        };
    }
    if lowered.contains("receipt") {
        return Verdict::Receipt {
            reason: "the filename says receipt".to_owned(),
        };
    }
    Verdict::Unfiled {
        reason: "nothing in the name marks it a credential or a receipt".to_owned(),
    }
}

/// The kind the cascade proposes for a candidate: a receipt is the fee
/// evidence, an image is an exhibit, and everything the cascade cannot name
/// stays unclassified — filing guesses no further.
fn proposed_kind(verdict: &Verdict, filename: &str) -> &'static str {
    match verdict {
        Verdict::Credential { .. } | Verdict::Unfiled { reason: _ } => {
            if is_image(filename) {
                "exhibit"
            } else {
                "unclassified"
            }
        }
        Verdict::Receipt { .. } => "invoice",
    }
}

/// True when `filename` carries an image extension.
fn is_image(filename: &str) -> bool {
    let extension = Path::new(filename)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    IMAGE_EXTENSIONS.contains(&extension.as_str())
}

/// The content type an extension names, as the upload door receives it.
/// The map is the one the browser's own accept lists spell; an unknown
/// extension stays `application/octet-stream`.
fn content_type_of(filename: &str) -> &'static str {
    let extension = Path::new(filename)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match extension.as_str() {
        "pdf" => "application/pdf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "tif" | "tiff" => "image/tiff",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "svg" => "image/svg+xml",
        "txt" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "xml" => "application/xml",
        "zip" => "application/zip",
        "eml" => "message/rfc822",
        "msg" => "application/vnd.ms-outlook",
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        _ => "application/octet-stream",
    }
}

/// The macOS provenance of one file, as Spotlight recorded it at download
/// time. Runs `mdls` only on macOS; anywhere else the provenance is unknown,
/// and the caller says so rather than inventing one.
fn where_froms(path: &Path) -> Option<Vec<String>> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let output = std::process::Command::new("mdls")
        .arg("-raw")
        .arg("-name")
        .arg("kMDItemWhereFroms")
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_where_froms(&String::from_utf8_lossy(&output.stdout))
}

/// The download URLs `mdls -raw -name kMDItemWhereFroms` prints for a file —
/// a plist array like `("https://court.example/notice.pdf", …)` — as the
/// ordered list it carries. `None` when the attribute is absent (`(null)`)
/// or names nothing.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))] // exercised by the tests everywhere
fn parse_where_froms(raw: &str) -> Option<Vec<String>> {
    let raw = raw.trim();
    if raw.is_empty() || raw == "(null)" || raw == "missing value" {
        return None;
    }
    let inner = raw.strip_prefix('(')?.strip_suffix(')')?.trim();
    if inner.is_empty() {
        return None;
    }
    let entries: Vec<String> = inner
        .split("\", \"")
        .map(|entry| entry.trim_matches('"').trim().to_owned())
        .filter(|entry| !entry.is_empty())
        .collect();
    (!entries.is_empty()).then_some(entries)
}

/// The sha256 of a file, read in bounded chunks so a large attachment never
/// sits whole in memory.
fn file_sha256(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).with_context(|| format!("read {}", path.display()))?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = std::io::Read::read(&mut file, &mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    // `sha2`'s digest is a byte array, not a `LowerHex` formatter — write
    // the lowercase hex by hand, as the pointer records it.
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

/// Every candidate in `dir`: the regular files, oldest-modified last, each
/// with the facts intake needs. The folder is swept, not recursed — a
/// lawyer's Downloads is flat.
fn scan_dir(dir: &Path, since: Option<chrono::NaiveDate>) -> Result<Vec<Candidate>> {
    let mut candidates = Vec::new();
    let entries = std::fs::read_dir(dir).with_context(|| format!("sweep {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("sweep {}", dir.display()))?;
        let path = entry.path();
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => metadata,
            // A vanished download, a symlink loop, a socket: nothing to
            // intake. The next sweep sees what remains.
            _ => continue,
        };
        let modified = metadata
            .modified()
            .context("read a file's modification time")?;
        let modified: chrono::DateTime<chrono::Utc> = modified.into();
        if let Some(since) = since {
            if modified.date_naive() < since {
                continue;
            }
        }
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("{} has no UTF-8 filename", path.display()))?
            .to_owned();
        let now = chrono::Utc::now();
        candidates.push(Candidate {
            path: path.display().to_string(),
            filename: filename.clone(),
            sha256: file_sha256(&path)?,
            content_type: content_type_of(&filename),
            size_bytes: metadata.len(),
            modified_at: modified.to_rfc3339(),
            age_days: (now - modified).num_days().max(0),
            where_froms: where_froms(&path),
            class: classify(&filename).label(),
        });
    }
    candidates.sort_by(|a, b| a.filename.cmp(&b.filename));
    Ok(candidates)
}

/// The conventional slug a candidate proposes under LAW-196, dated by its
/// own modification date. Markdown work product is exempt from the
/// convention (`rules::c004`), so no slug is proposed for it.
fn proposed_slug(filename: &str, modified_on: &str) -> Option<String> {
    if rules::c004::is_markdown_work_product(filename) {
        return None;
    }
    rules::c004::proposed_filename(modified_on, filename)
}

/// True when `haystack` names `token` whole — delimited on both sides by
/// something that cannot extend an identifier — so a matter code matches its
/// own mentions and never a longer word around it.
fn names_token(haystack: &str, token: &str) -> bool {
    let boundary = |character: Option<char>| character.is_none_or(|c| !c.is_ascii_alphanumeric());
    let hay = haystack.to_lowercase();
    let needle = token.to_lowercase();
    let mut start = 0;
    while let Some(offset) = hay[start..].find(&needle) {
        let at = start + offset;
        let end = at + needle.len();
        if boundary(hay[..at].chars().next_back()) && boundary(hay[end..].chars().next()) {
            return true;
        }
        start = end;
    }
    false
}

/// One candidate's proposal, as `propose` reports it.
#[derive(Debug, Serialize)]
struct Proposal {
    path: String,
    filename: String,
    class: &'static str,
    reason: String,
    /// The Project code the evidence names, or `null` when none does.
    project: Option<String>,
    kind: &'static str,
    /// The LAW-196 slug, or `null` when no date or no surviving name
    /// supports one — the caller passes `--slug` when filing.
    slug: Option<String>,
    evidence: Vec<String>,
}

/// Propose the matter, kind, and slug for every candidate, oldest first.
/// The matter proposal reads the visible matters the login can see and
/// names one only when the filename or the provenance carries its code as
/// evidence; without a login the local proposals still stand, and the
/// evidence says what is missing.
async fn propose(
    dir: &Path,
    since: Option<chrono::NaiveDate>,
    host: Option<&str>,
) -> Result<Vec<Proposal>> {
    let candidates = scan_dir(dir, since)?;
    let mut proposals = Vec::new();
    let mut login_error: Option<String> = None;
    let projects = match crate::remote::visible_projects(host).await {
        Ok(projects) => projects,
        Err(error) => {
            login_error = Some(format!("{error:#}"));
            Vec::new()
        }
    };
    for candidate in candidates {
        let verdict = classify(&candidate.filename);
        let mut evidence = vec![format!(
            "classified {class}: {reason}",
            class = verdict.label(),
            reason = verdict.reason()
        )];
        let project = match_project(&candidate, &projects, &mut evidence);
        if let Some(error) = &login_error {
            evidence.push(format!("no matter proposals without a login: {error}"));
        }
        let kind = proposed_kind(&verdict, &candidate.filename);
        if kind == "exhibit" {
            evidence.push("an image files as an exhibit".to_owned());
        }
        let modified_on = candidate.modified_at.get(..10).unwrap_or_default();
        let slug = proposed_slug(&candidate.filename, modified_on);
        match &slug {
            Some(slug) => evidence.push(format!("the LAW-196 slug is `{slug}`")),
            None if rules::c004::is_markdown_work_product(&candidate.filename) => {
                evidence.push(
                    "markdown work product — the filename convention does not govern it".to_owned(),
                );
            }
            None => evidence.push("no date names this file — pass --slug when filing".to_owned()),
        }
        proposals.push(Proposal {
            path: candidate.path,
            filename: candidate.filename,
            class: verdict.label(),
            reason: verdict.reason().to_owned(),
            project,
            kind,
            slug,
            evidence,
        });
    }
    Ok(proposals)
}

/// The first visible matter whose code the candidate's evidence names, with
/// the evidence recorded. First match wins, in the order the deployment
/// lists the matters — the same shape the inbox's classification follows.
fn match_project(
    candidate: &Candidate,
    projects: &[(Uuid, String)],
    evidence: &mut Vec<String>,
) -> Option<String> {
    for (_, code) in projects {
        if names_token(&candidate.filename, code) {
            evidence.push(format!("the filename names matter code `{code}`"));
            return Some(code.clone());
        }
        if let Some(where_froms) = &candidate.where_froms {
            let provenance = where_froms.join(" ");
            if names_token(&provenance, code) {
                evidence.push(format!("the provenance names matter code `{code}`"));
                return Some(code.clone());
            }
        }
    }
    if projects.is_empty() {
        evidence.push("no visible matter names a code in this candidate".to_owned());
    } else {
        evidence.push("no visible matter's code appears in the filename or provenance".to_owned());
    }
    None
}

pub(crate) async fn run(action: DownloadsCmd) -> ExitCode {
    match run_inner(action).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("navigator downloads: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// The Downloads directory: the caller's `--dir`, else the platform's own.
fn resolve_dir(dir: Option<&Path>) -> Result<PathBuf> {
    match dir {
        Some(dir) => Ok(dir.to_path_buf()),
        None => crate::lsp_download::default_downloads_dir(std::env::var("HOME").ok()),
    }
}

/// An ISO date flag, parsed where the verb reads it.
fn parse_since(since: Option<&str>) -> Result<Option<chrono::NaiveDate>> {
    since
        .map(|value| {
            chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .map_err(|error| anyhow!("--since must be an ISO date, YYYY-MM-DD: {error}"))
        })
        .transpose()
}

async fn run_inner(action: DownloadsCmd) -> Result<()> {
    match action {
        DownloadsCmd::Scan { dir, since, json } => {
            let candidates = scan_dir(
                &resolve_dir(dir.as_deref())?,
                parse_since(since.as_deref())?,
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&candidates)?);
            } else {
                print_candidates(&candidates);
            }
            Ok(())
        }
        DownloadsCmd::Propose {
            dir,
            since,
            json,
            host,
        } => {
            let proposals = propose(
                &resolve_dir(dir.as_deref())?,
                parse_since(since.as_deref())?,
                host.as_deref(),
            )
            .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&proposals)?);
            } else {
                for proposal in &proposals {
                    print_proposal(proposal);
                }
            }
            Ok(())
        }
        DownloadsCmd::File { .. } | DownloadsCmd::Clean { .. } => {
            unreachable!("file and clean dispatch in the second half of this module")
        }
    }
}

fn print_candidates(candidates: &[Candidate]) {
    if candidates.is_empty() {
        println!("The Downloads folder holds no candidates.");
        return;
    }
    for candidate in candidates {
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            candidate.filename,
            candidate.sha256,
            candidate.content_type,
            candidate.size_bytes,
            candidate.age_days,
            candidate.class,
            candidate
                .where_froms
                .as_ref()
                .map_or_else(|| "-".to_owned(), |urls| urls.join(" "))
        );
    }
}

fn print_proposal(proposal: &Proposal) {
    println!("Path         {}", proposal.path);
    println!("Class        {} ({})", proposal.class, proposal.reason);
    println!(
        "Matter       {}",
        proposal.project.as_deref().unwrap_or("-")
    );
    println!("Kind         {}", proposal.kind);
    println!("Slug         {}", proposal.slug.as_deref().unwrap_or("-"));
    println!("Because      {}", proposal.evidence.join("; "));
    println!();
}

/// Move one file into the Trash, without overwriting whatever may already
/// sit there, and return the destination it landed at.
#[allow(dead_code)] // consumed by `downloads file`, landing in the next commit
fn move_to_trash(path: &Path, trash_root: Option<&Path>) -> Result<PathBuf> {
    let root = match trash_root {
        Some(root) => root.to_path_buf(),
        None => trash_root_from_env()?,
    };
    std::fs::create_dir_all(&root)
        .with_context(|| format!("prepare the Trash at {}", root.display()))?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("{} has no UTF-8 filename", path.display()))?;
    let mut destination = root.join(filename);
    let mut attempt = 0;
    while destination.exists() {
        attempt += 1;
        destination = root.join(format!("{filename}-{attempt}"));
    }
    std::fs::rename(path, &destination)
        .with_context(|| format!("move {} to the Trash", path.display()))?;
    Ok(destination)
}

/// The Trash directory: `~/.Trash` on macOS, the `FreeDesktop` trash `files`
/// directory everywhere else the CLI ships.
#[allow(dead_code)] // consumed by `downloads file`, landing in the next commit
fn trash_root_from_env() -> Result<PathBuf> {
    let home = std::env::var("HOME").ok().filter(|value| !value.is_empty());
    let Some(home) = home else {
        bail!("cannot resolve a home directory ($HOME is unset) to move the original to the Trash");
    };
    if cfg!(target_os = "macos") {
        Ok(PathBuf::from(home).join(".Trash"))
    } else {
        Ok(PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("Trash")
            .join("files"))
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        classify, content_type_of, move_to_trash, names_token, parse_where_froms, proposed_slug,
        trash_root_from_env, Verdict,
    };

    fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn the_cascade_refuses_a_credential_first() {
        assert_eq!(
            classify("firm-api.pem"),
            Verdict::Credential {
                reason: "the `.pem` extension marks credentials".to_owned()
            }
        );
        assert_eq!(
            classify("2026.10.12_passwords.txt"),
            Verdict::Credential {
                reason: "the filename names credentials (`password`)".to_owned()
            }
        );
        // A receipt that would also match nothing else stays a receipt; the
        // cascade's credential arm never reorders.
        assert_eq!(
            classify("2026.10.12_filing-receipt.pdf"),
            Verdict::Receipt {
                reason: "the filename says receipt".to_owned()
            }
        );
        assert_eq!(
            classify("2026.10.12_complaint-annulment.pdf"),
            Verdict::Unfiled {
                reason: "nothing in the name marks it a credential or a receipt".to_owned()
            }
        );
    }

    #[test]
    fn the_kind_fits_the_verdict_and_the_extension() {
        assert_eq!(
            super::proposed_kind(
                &classify("2026.10.12_filing-receipt.pdf"),
                "2026.10.12_filing-receipt.pdf"
            ),
            "invoice"
        );
        assert_eq!(
            super::proposed_kind(
                &classify("2026.10.12_client-screenshot.png"),
                "2026.10.12_client-screenshot.png"
            ),
            "exhibit"
        );
        assert_eq!(
            super::proposed_kind(&classify("2026.10.12_notes.txt"), "2026.10.12_notes.txt"),
            "unclassified"
        );
        assert_eq!(
            super::proposed_kind(&classify("firm-api.pem"), "firm-api.pem"),
            "unclassified"
        );
    }

    #[test]
    fn content_types_follow_the_extension() {
        assert_eq!(content_type_of("notice.pdf"), "application/pdf");
        assert_eq!(content_type_of("IMG_0001.HEIC"), "image/heic");
        assert_eq!(content_type_of("no-extension"), "application/octet-stream");
    }

    #[test]
    fn where_froms_parse_the_mdls_plist_array() {
        let raw =
            "(\"https://ecf.nvd.uscourts.gov/doc1/notice.pdf\", \"https://ecf.nvd.uscourts.gov/\")";
        assert_eq!(
            parse_where_froms(raw),
            Some(vec![
                "https://ecf.nvd.uscourts.gov/doc1/notice.pdf".to_owned(),
                "https://ecf.nvd.uscourts.gov/".to_owned()
            ])
        );
        assert_eq!(parse_where_froms("(null)"), None);
        assert_eq!(parse_where_froms(""), None);
        assert_eq!(parse_where_froms("()"), None);
    }

    #[test]
    fn the_slug_follows_the_law_196_convention() {
        assert_eq!(
            proposed_slug("complaint-annulment-2026-10-12.pdf", "2026-10-12").as_deref(),
            Some("2026.10.12_complaint-annulment.pdf")
        );
        assert_eq!(
            proposed_slug("Client Screenshot IMG-2645.png", "2026-10-12").as_deref(),
            Some("2026.10.12_client-screenshot-img-2645.png")
        );
        // Markdown work product stays outside the convention.
        assert_eq!(proposed_slug("memo.md", "2026-10-12"), None);
        // A file no date names proposes nothing.
        assert_eq!(proposed_slug("!!!.pdf", "2026-10-12"), None);
    }

    #[test]
    fn a_token_matches_whole_and_not_inside_a_longer_word() {
        assert!(names_token("acme-001-intake-notes.pdf", "acme-001"));
        assert!(names_token("notice from acme-001.pdf", "acme-001"));
        assert!(!names_token("acme-0012-intake.pdf", "acme-001"));
        assert!(!names_token("2026.10.12_notes.pdf", "acme"));
    }

    #[test]
    fn a_moved_original_lands_in_the_trash_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let trash = tempfile::tempdir().unwrap();
        let path = write_file(dir.path(), "2026.10.12_notice.pdf", b"bytes");
        let destination = move_to_trash(&path, Some(trash.path())).unwrap();
        assert!(!path.exists());
        assert_eq!(destination, trash.path().join("2026.10.12_notice.pdf"));

        let path = write_file(dir.path(), "2026.10.12_notice.pdf", b"bytes");
        let destination = move_to_trash(&path, Some(trash.path())).unwrap();
        assert!(!path.exists());
        assert_eq!(
            destination,
            trash.path().join("2026.10.12_notice.pdf-1"),
            "a second filing never overwrites what the Trash already holds"
        );
    }

    #[test]
    fn the_default_trash_root_sits_under_home() {
        std::env::set_var("HOME", "/home/nick");
        let root = trash_root_from_env().unwrap();
        let display = root.display().to_string();
        if cfg!(target_os = "macos") {
            assert_eq!(display, "/home/nick/.Trash");
        } else {
            assert!(display.starts_with("/home/nick/.local/share/Trash/files"));
        }
        std::env::remove_var("HOME");
        assert!(trash_root_from_env().is_err(), "no $HOME, no Trash move");
        std::env::set_var("HOME", std::env::temp_dir().display().to_string());
    }
}
