//! The headless Project source archive: one bounded, fail-closed ZIP reader
//! shared by the CLI that builds an archive and the server that imports one.
//!
//! [`canonicalize`] turns untrusted ZIP bytes into a [`CanonicalTree`] or a
//! [`SourceArchiveError`]; it never executes, extracts to disk, or follows
//! anything. [`write_canonical_zip`] is its inverse for a tree the CLI built,
//! so CLI output and server input go through the same rules.
//!
//! # Bounds
//!
//! Every limit is checked before bytes are inflated past it:
//! [`MAX_ARCHIVE_BYTES`] of input, [`MAX_ENTRIES`] central-directory entries,
//! [`MAX_ENTRY_BYTES`] per file, [`MAX_TOTAL_BYTES`] in all, and an inflation
//! ratio of [`MAX_RATIO`] per entry. Declared sizes are not trusted: reads are
//! capped one byte past the limit so a header that lies still stops.
//!
//! # Path algorithm
//!
//! A name must be printable ASCII with `/` separators only. A backslash, a
//! drive letter, a leading `/`, an empty, `.` or `..` segment, a segment
//! ending in a dot or space, and a Windows device name are refused. Paths are
//! compared case-insensitively, so two names differing only in case collide,
//! and a path that is both a file and another path's directory collides. If
//! every entry sits under one top-level directory and nothing sits at the
//! root, that one wrapper is stripped; the stripped tree must not be empty.
//!
//! # Entry and content policy
//!
//! Encrypted entries, compression other than stored or deflate, symbolic
//! links, and anything that is neither a regular file nor a directory are
//! refused before their data is read. Only the headless roots in
//! [`HEADLESS_ROOTS`] are admitted; dependency and build output directories
//! are refused anywhere; `blobs/` (and the legacy `documents/`) holds only
//! `.yaml` or `.yml` pointers;
//! `.claude/` holds only `settings.json`. Every file must be UTF-8 text with
//! no NUL byte, so legal documents, images, and executables cannot enter a
//! source repository. Credential-shaped paths and contents are refused.
//! Errors name the offending path and rule, never file contents.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read, Write as _};

use sha2::{Digest, Sha256};

/// The most compressed bytes one archive may carry.
pub const MAX_ARCHIVE_BYTES: usize = 16 * 1024 * 1024;
/// The most central-directory entries one archive may declare.
pub const MAX_ENTRIES: usize = 2_000;
/// The most bytes one file may inflate to.
pub const MAX_ENTRY_BYTES: u64 = 2 * 1024 * 1024;
/// The most bytes every file together may inflate to.
pub const MAX_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
/// The most one entry may inflate relative to its compressed size.
pub const MAX_RATIO: u64 = 100;

/// The top-level roots a headless Project source archive may hold: the
/// source-only contract without the retired Portal and Vite application
/// files. Every one is also admitted by `navigator project gate`.
pub const HEADLESS_ROOTS: &[&str] = &[
    ".agents",
    ".claude",
    ".forgejo",
    ".gitattributes",
    ".github",
    ".gitignore",
    "AGENTS.md",
    "LICENSE",
    "LICENSE-APACHE-2",
    "LICENSE-MIT",
    "LICENSE.md",
    "README.md",
    "blobs",
    "documents",
    "drafts",
    "fixtures",
    "navigator.yaml",
    "seeds",
    "templates",
    "tests",
];

/// Directory names that are dependency or build output wherever they appear.
/// The CLI's public snapshot audit (`P001`) refuses the same names in a tracked path.
pub const BUILD_OUTPUT: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
];

/// File names and suffixes that hold credentials, shared with the public snapshot audit.
pub const CREDENTIAL_NAMES: &[&str] = &[
    ".env",
    ".npmrc",
    ".netrc",
    ".pypirc",
    "credentials.json",
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
];
pub const CREDENTIAL_SUFFIXES: &[&str] = &[".pem", ".key", ".p12", ".pfx", ".keystore", ".jks"];
/// Content prefixes of common credentials.
const CREDENTIAL_MARKERS: &[&str] = &[
    "-----BEGIN RSA PRIVATE KEY",
    "-----BEGIN OPENSSH PRIVATE KEY",
    "-----BEGIN PRIVATE KEY",
    "-----BEGIN EC PRIVATE KEY",
    "ghp_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
    "sk_live_",
    "AKIA",
];

const WINDOWS_DEVICES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Why an archive was refused. Paths are canonical or raw-escaped; contents
/// are never included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceArchiveError {
    TooLarge,
    Unreadable,
    TooManyEntries,
    EntryTooLarge(String),
    TotalTooLarge,
    RatioTooHigh(String),
    Encrypted(String),
    UnsupportedCompression(String),
    Symlink(String),
    NotRegular(String),
    InvalidPath(String),
    Collision(String),
    Empty,
    OutsideLayout(String),
    BuildOutput(String),
    NotPointer(String),
    NotText(String),
    Credential(String),
}

impl std::fmt::Display for SourceArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => write!(f, "archive exceeds {MAX_ARCHIVE_BYTES} bytes"),
            Self::Unreadable => f.write_str("archive is not a readable ZIP"),
            Self::TooManyEntries => write!(f, "archive declares more than {MAX_ENTRIES} entries"),
            Self::EntryTooLarge(path) => write!(f, "{path} exceeds {MAX_ENTRY_BYTES} bytes"),
            Self::TotalTooLarge => write!(f, "archive inflates past {MAX_TOTAL_BYTES} bytes"),
            Self::RatioTooHigh(path) => write!(f, "{path} inflates more than {MAX_RATIO}x"),
            Self::Encrypted(path) => write!(f, "{path} is encrypted"),
            Self::UnsupportedCompression(path) => write!(f, "{path} uses unsupported compression"),
            Self::Symlink(path) => write!(f, "{path} is a symbolic link"),
            Self::NotRegular(path) => write!(f, "{path} is not a regular file or directory"),
            Self::InvalidPath(path) => write!(f, "{path} is not a canonical source path"),
            Self::Collision(path) => write!(f, "{path} collides with another entry"),
            Self::Empty => f.write_str("archive holds no files"),
            Self::OutsideLayout(path) => {
                write!(f, "{path} is outside the headless Project source layout")
            }
            Self::BuildOutput(path) => write!(f, "{path} is dependency or build output"),
            Self::NotPointer(path) => {
                write!(f, "{path}: blobs/ holds only .yaml document pointers")
            }
            Self::NotText(path) => write!(f, "{path} is not UTF-8 text"),
            Self::Credential(path) => write!(f, "{path} looks like a credential"),
        }
    }
}

impl std::error::Error for SourceArchiveError {}

/// A validated, canonical source tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalTree {
    /// Canonical path to UTF-8 text, ordered by path.
    pub files: BTreeMap<String, String>,
}

impl CanonicalTree {
    /// SHA-256 over every path and its bytes in order: the identity an import
    /// binds its operation to and finds an applied commit by.
    #[must_use]
    pub fn digest(&self) -> String {
        let mut hasher = Sha256::new();
        for (path, text) in &self.files {
            hasher.update((path.len() as u64).to_be_bytes());
            hasher.update(path.as_bytes());
            hasher.update((text.len() as u64).to_be_bytes());
            hasher.update(text.as_bytes());
        }
        hasher
            .finalize()
            .iter()
            .fold(String::with_capacity(64), |mut out, byte| {
                use std::fmt::Write as _;
                let _ = write!(out, "{byte:02x}");
                out
            })
    }

    /// Validate an in-memory tree with the same path and content rules.
    ///
    /// # Errors
    /// The first rule a path or file breaks.
    pub fn from_files(files: BTreeMap<String, String>) -> Result<Self, SourceArchiveError> {
        let mut seen = BTreeSet::new();
        for (path, text) in &files {
            let canonical = canonical_path(path)?;
            if &canonical != path {
                return Err(SourceArchiveError::InvalidPath(escape(path.as_bytes())));
            }
            if !seen.insert(canonical.to_ascii_lowercase()) {
                return Err(SourceArchiveError::Collision(canonical));
            }
            check_layout(path)?;
            check_text(path, text.as_bytes())?;
        }
        check_prefix_collisions(files.keys())?;
        if files.is_empty() {
            return Err(SourceArchiveError::Empty);
        }
        Ok(Self { files })
    }
}

fn escape(raw: &[u8]) -> String {
    raw.iter()
        .flat_map(|byte| std::ascii::escape_default(*byte))
        .map(char::from)
        .take(200)
        .collect()
}

/// One canonical path, or why the name is refused.
fn canonical_path(name: &str) -> Result<String, SourceArchiveError> {
    let invalid = || SourceArchiveError::InvalidPath(escape(name.as_bytes()));
    let trimmed = name.strip_suffix('/').unwrap_or(name);
    if trimmed.is_empty()
        || !trimmed.bytes().all(|byte| (0x20..0x7f).contains(&byte))
        || trimmed.contains('\\')
        || trimmed.starts_with('/')
        || trimmed.as_bytes().get(1) == Some(&b':')
    {
        return Err(invalid());
    }
    for segment in trimmed.split('/') {
        let stem = segment
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if segment.is_empty()
            || segment == "."
            || segment == ".."
            || segment.ends_with('.')
            || segment.ends_with(' ')
            || segment.starts_with(' ')
            || WINDOWS_DEVICES.contains(&stem.as_str())
        {
            return Err(invalid());
        }
    }
    Ok(trimmed.to_string())
}

fn check_layout(path: &str) -> Result<(), SourceArchiveError> {
    let segments: Vec<&str> = path.split('/').collect();
    if !HEADLESS_ROOTS.contains(&segments[0]) {
        return Err(SourceArchiveError::OutsideLayout(path.to_string()));
    }
    if segments
        .iter()
        .any(|segment| BUILD_OUTPUT.contains(&segment.to_ascii_lowercase().as_str()))
    {
        return Err(SourceArchiveError::BuildOutput(path.to_string()));
    }
    let file = segments
        .last()
        .copied()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if CREDENTIAL_NAMES.contains(&file.as_str())
        || file.starts_with(".env.")
        || CREDENTIAL_SUFFIXES
            .iter()
            .any(|suffix| file.ends_with(suffix))
    {
        return Err(SourceArchiveError::Credential(path.to_string()));
    }
    let is_pointer = std::path::Path::new(&file)
        .extension()
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("yaml") || extension.eq_ignore_ascii_case("yml")
        });
    if matches!(segments[0], "blobs" | "documents") && segments.len() > 1 && !is_pointer {
        return Err(SourceArchiveError::NotPointer(path.to_string()));
    }
    if segments[0] == ".claude" && path != ".claude/settings.json" {
        return Err(SourceArchiveError::OutsideLayout(path.to_string()));
    }
    Ok(())
}

fn check_text(path: &str, bytes: &[u8]) -> Result<(), SourceArchiveError> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| SourceArchiveError::NotText(path.to_string()))?;
    if text.contains('\0') {
        return Err(SourceArchiveError::NotText(path.to_string()));
    }
    if CREDENTIAL_MARKERS.iter().any(|marker| {
        text.match_indices(marker).any(|(index, _)| {
            // `AKIA` is only a key when sixteen uppercase alphanumerics follow.
            *marker != "AKIA"
                || text[index + 4..]
                    .bytes()
                    .take(16)
                    .filter(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
                    .count()
                    == 16
        })
    }) {
        return Err(SourceArchiveError::Credential(path.to_string()));
    }
    Ok(())
}

fn check_prefix_collisions<'a>(
    paths: impl Iterator<Item = &'a String> + Clone,
) -> Result<(), SourceArchiveError> {
    let files: BTreeSet<String> = paths
        .clone()
        .map(|path| path.to_ascii_lowercase())
        .collect();
    for path in paths {
        let mut prefix = String::new();
        for segment in path
            .split('/')
            .collect::<Vec<_>>()
            .split_last()
            .map_or(&[][..], |(_, rest)| rest)
        {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(&segment.to_ascii_lowercase());
            if files.contains(&prefix) {
                return Err(SourceArchiveError::Collision(path.clone()));
            }
        }
    }
    Ok(())
}

/// One entry's canonical name and bytes, read within every bound.
struct Entry {
    path: String,
    bytes: Vec<u8>,
}

/// Validate untrusted ZIP bytes into a canonical tree.
///
/// # Errors
/// [`SourceArchiveError`] naming the first bound, path, entry, or content rule
/// broken. Nothing is written anywhere.
pub fn canonicalize(bytes: &[u8]) -> Result<CanonicalTree, SourceArchiveError> {
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(SourceArchiveError::TooLarge);
    }
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| SourceArchiveError::Unreadable)?;
    if archive.len() > MAX_ENTRIES {
        return Err(SourceArchiveError::TooManyEntries);
    }
    let mut entries = Vec::new();
    let mut total: u64 = 0;
    for index in 0..archive.len() {
        let file = archive
            .by_index_raw(index)
            .map_err(|_| SourceArchiveError::Unreadable)?;
        let raw = escape(file.name_raw());
        let name = std::str::from_utf8(file.name_raw())
            .map_err(|_| SourceArchiveError::InvalidPath(raw.clone()))?
            .to_string();
        if file.encrypted() {
            return Err(SourceArchiveError::Encrypted(raw));
        }
        if file.is_symlink() {
            return Err(SourceArchiveError::Symlink(raw));
        }
        if let Some(mode) = file.unix_mode() {
            let kind = mode & 0o170_000;
            if kind != 0 && kind != 0o100_000 && kind != 0o040_000 {
                return Err(SourceArchiveError::NotRegular(raw));
            }
        }
        if file.is_dir() {
            canonical_path(&name)?;
            continue;
        }
        let compression = file.compression();
        if !matches!(
            compression,
            zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated
        ) {
            return Err(SourceArchiveError::UnsupportedCompression(raw));
        }
        let path = canonical_path(&name)?;
        let declared = file.size();
        if declared > MAX_ENTRY_BYTES {
            return Err(SourceArchiveError::EntryTooLarge(path));
        }
        if declared > file.compressed_size().max(1).saturating_mul(MAX_RATIO) {
            return Err(SourceArchiveError::RatioTooHigh(path));
        }
        drop(file);
        let mut file = archive
            .by_index(index)
            .map_err(|_| SourceArchiveError::Unreadable)?;
        let compressed = file.compressed_size().max(1);
        let mut data = Vec::new();
        file.by_ref()
            .take(MAX_ENTRY_BYTES + 1)
            .read_to_end(&mut data)
            .map_err(|_| SourceArchiveError::Unreadable)?;
        let actual = data.len() as u64;
        if actual > MAX_ENTRY_BYTES {
            return Err(SourceArchiveError::EntryTooLarge(path));
        }
        if actual > compressed.saturating_mul(MAX_RATIO) {
            return Err(SourceArchiveError::RatioTooHigh(path));
        }
        total = total.saturating_add(actual);
        if total > MAX_TOTAL_BYTES {
            return Err(SourceArchiveError::TotalTooLarge);
        }
        entries.push(Entry { path, bytes: data });
    }
    strip_wrapper(&mut entries)?;
    let mut files = BTreeMap::new();
    let mut lowered = BTreeSet::new();
    for entry in entries {
        if !lowered.insert(entry.path.to_ascii_lowercase()) {
            return Err(SourceArchiveError::Collision(entry.path));
        }
        check_layout(&entry.path)?;
        check_text(&entry.path, &entry.bytes)?;
        let text = String::from_utf8(entry.bytes)
            .map_err(|_| SourceArchiveError::NotText(entry.path.clone()))?;
        files.insert(entry.path, text);
    }
    check_prefix_collisions(files.keys())?;
    if files.is_empty() {
        return Err(SourceArchiveError::Empty);
    }
    Ok(CanonicalTree { files })
}

/// Strip one wrapper directory when every file sits beneath it and nothing
/// sits at the root.
fn strip_wrapper(entries: &mut [Entry]) -> Result<(), SourceArchiveError> {
    if entries.is_empty() {
        return Err(SourceArchiveError::Empty);
    }
    let first = entries[0]
        .path
        .split('/')
        .next()
        .unwrap_or_default()
        .to_string();
    let wrapped = entries.iter().all(|entry| {
        entry
            .path
            .split_once('/')
            .is_some_and(|(top, _)| top == first)
    });
    if wrapped && !HEADLESS_ROOTS.contains(&first.as_str()) {
        for entry in entries.iter_mut() {
            let (_, rest) = entry.path.split_once('/').unwrap_or_default();
            entry.path = rest.to_string();
        }
    }
    Ok(())
}

/// Write a canonical tree as a deterministic ZIP: sorted paths, deflate, and
/// a fixed timestamp, so the same tree is always the same bytes.
///
/// # Errors
/// [`SourceArchiveError::Unreadable`] if the writer fails.
pub fn write_canonical_zip(tree: &CanonicalTree) -> Result<Vec<u8>, SourceArchiveError> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::default())
        .unix_permissions(0o644);
    for (path, text) in &tree.files {
        writer
            .start_file(path.as_str(), options)
            .map_err(|_| SourceArchiveError::Unreadable)?;
        writer
            .write_all(text.as_bytes())
            .map_err(|_| SourceArchiveError::Unreadable)?;
    }
    writer
        .finish()
        .map(Cursor::into_inner)
        .map_err(|_| SourceArchiveError::Unreadable)
}

#[cfg(test)]
mod tests {
    use super::{canonicalize, write_canonical_zip, CanonicalTree, SourceArchiveError};
    use std::collections::BTreeMap;
    use std::io::{Cursor, Write as _};

    fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in entries {
            if name.ends_with('/') {
                writer.add_directory(*name, options).unwrap();
            } else {
                writer.start_file(*name, options).unwrap();
                writer.write_all(bytes).unwrap();
            }
        }
        writer.finish().unwrap().into_inner()
    }

    fn tree(entries: &[(&str, &str)]) -> CanonicalTree {
        CanonicalTree::from_files(
            entries
                .iter()
                .map(|(path, text)| ((*path).to_string(), (*text).to_string()))
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn a_wrapped_source_tree_canonicalizes_and_round_trips() {
        let bytes = zip(&[
            ("matter/", b""),
            ("matter/navigator.yaml", b"version: 1\n"),
            ("matter/drafts/onboarding__letter.md", b"# Draft\n"),
            ("matter/documents/engagement.pdf.yml", b"slug: engagement\n"),
        ]);
        let tree = canonicalize(&bytes).unwrap();
        assert_eq!(
            tree.files.keys().cloned().collect::<Vec<_>>(),
            [
                "documents/engagement.pdf.yml",
                "drafts/onboarding__letter.md",
                "navigator.yaml"
            ]
        );
        let again = canonicalize(&write_canonical_zip(&tree).unwrap()).unwrap();
        assert_eq!(again, tree);
        assert_eq!(again.digest(), tree.digest());
        assert_eq!(
            write_canonical_zip(&tree).unwrap(),
            write_canonical_zip(&again).unwrap(),
            "the same tree is the same bytes"
        );
    }

    #[test]
    fn hostile_paths_are_refused() {
        for name in [
            "../navigator.yaml",
            "/navigator.yaml",
            "C:/navigator.yaml",
            "drafts\\x.md",
            "drafts/./x.md",
            "drafts//x.md",
            "drafts/x.md.",
            "drafts/x.md ",
            "drafts/CON.md",
            "drafts/caf\u{e9}.md",
            "drafts/a\u{1}.md",
        ] {
            let result = canonicalize(&zip(&[(name, b"x")]));
            assert!(
                matches!(result, Err(SourceArchiveError::InvalidPath(_))),
                "{name}: {result:?}"
            );
        }
    }

    #[test]
    fn case_and_file_directory_collisions_are_refused() {
        assert!(matches!(
            canonicalize(&zip(&[("drafts/A.md", b"a"), ("drafts/a.md", b"b")])),
            Err(SourceArchiveError::Collision(_))
        ));
        assert!(matches!(
            canonicalize(&zip(&[("drafts/x", b"a"), ("drafts/x/y.md", b"b")])),
            Err(SourceArchiveError::Collision(_))
        ));
    }

    #[test]
    fn layout_build_output_pointers_text_and_credentials_are_enforced() {
        let cases: &[(&str, &[u8])] = &[
            ("portal/index.html", b"<html>"),
            ("package.json", b"{}"),
            ("tests/node_modules/x.js", b"x"),
            ("documents/engagement.pdf", b"%PDF"),
            ("drafts/logo.png", b"\x89PNG\r\n\x1a\n\0"),
            ("fixtures/.env", b"TOKEN=x"),
            ("fixtures/deploy.pem", b"x"),
            ("drafts/note.md", b"-----BEGIN OPENSSH PRIVATE KEY-----"),
            ("drafts/note2.md", b"token ghp_abcdefghijklmnop"),
            // Split so secret scanners do not flag the synthetic key itself.
            (
                "drafts/note3.md",
                concat!("key AKIA", "ABCDEFGHIJKLMNOP").as_bytes(),
            ),
            (".claude/agents.json", b"{}"),
        ];
        for (name, bytes) in cases {
            let result = canonicalize(&zip(&[(name, bytes)]));
            assert!(
                matches!(
                    result,
                    Err(SourceArchiveError::OutsideLayout(_)
                        | SourceArchiveError::BuildOutput(_)
                        | SourceArchiveError::NotPointer(_)
                        | SourceArchiveError::NotText(_)
                        | SourceArchiveError::Credential(_))
                ),
                "{name}: {result:?}"
            );
            // The refusal names the path and never the content.
            let message = result.unwrap_err().to_string();
            assert!(
                !message.contains("ghp_") && !message.contains("BEGIN"),
                "{message}"
            );
        }
        // A word that merely starts like a key is ordinary text.
        assert!(canonicalize(&zip(&[("drafts/a.md", b"AKIAs are cloud keys")])).is_ok());
    }

    #[test]
    fn bombs_and_oversize_inputs_stop_before_inflating() {
        let bomb = vec![b'a'; usize::try_from(super::MAX_ENTRY_BYTES + 10).unwrap()];
        assert!(matches!(
            canonicalize(&zip(&[("drafts/bomb.md", &bomb)])),
            Err(SourceArchiveError::EntryTooLarge(_) | SourceArchiveError::RatioTooHigh(_))
        ));
        let ratio = vec![b'a'; 1_000_000];
        assert!(matches!(
            canonicalize(&zip(&[("drafts/ratio.md", &ratio)])),
            Err(SourceArchiveError::RatioTooHigh(_))
        ));
        assert_eq!(
            canonicalize(&vec![0; super::MAX_ARCHIVE_BYTES + 1]),
            Err(SourceArchiveError::TooLarge)
        );
        assert_eq!(
            canonicalize(b"not a zip"),
            Err(SourceArchiveError::Unreadable)
        );
        assert_eq!(
            canonicalize(&zip(&[("drafts/", b"")])),
            Err(SourceArchiveError::Empty)
        );
    }

    #[test]
    fn symlinks_and_special_files_are_refused_before_reading() {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        writer
            .add_symlink("drafts/link.md", "../../etc/passwd", options)
            .unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        assert!(matches!(
            canonicalize(&bytes),
            Err(SourceArchiveError::Symlink(_))
        ));

        // Writers normalize file types, so mark a FIFO in the central
        // directory's external attributes by hand.
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                "drafts/fifo",
                zip::write::SimpleFileOptions::default().unix_permissions(0o644),
            )
            .unwrap();
        let mut bytes = writer.finish().unwrap().into_inner();
        let central = bytes
            .windows(4)
            .position(|window| window == [0x50, 0x4b, 0x01, 0x02])
            .unwrap();
        bytes[central + 38..central + 42].copy_from_slice(&(0o010_644_u32 << 16).to_le_bytes());
        assert!(matches!(
            canonicalize(&bytes),
            Err(SourceArchiveError::NotRegular(_))
        ));
    }

    #[test]
    fn an_in_memory_tree_obeys_the_same_rules() {
        assert!(CanonicalTree::from_files(BTreeMap::new()).is_err());
        let built = tree(&[("navigator.yaml", "version: 1\n"), ("drafts/a.md", "a")]);
        assert_eq!(built.files.len(), 2);
        assert!(CanonicalTree::from_files(
            [("../x".to_string(), String::new())].into_iter().collect()
        )
        .is_err());
        assert_eq!(
            CanonicalTree::from_files(
                [("drafts/".to_string(), String::new())]
                    .into_iter()
                    .collect()
            ),
            Err(SourceArchiveError::InvalidPath("drafts/".into())),
            "an in-memory path must already be canonical"
        );
        assert_eq!(
            CanonicalTree::from_files(
                [
                    ("drafts/A.md".to_string(), "a".to_string()),
                    ("drafts/a.md".to_string(), "a".to_string()),
                ]
                .into_iter()
                .collect()
            ),
            Err(SourceArchiveError::Collision("drafts/a.md".into()))
        );
    }

    #[test]
    fn oversized_or_unreadable_bytes_are_refused_before_parsing() {
        assert_eq!(
            canonicalize(&vec![0; super::MAX_ARCHIVE_BYTES + 1]),
            Err(SourceArchiveError::TooLarge)
        );
        assert_eq!(
            canonicalize(b"not a zip"),
            Err(SourceArchiveError::Unreadable)
        );
    }

    /// Every refusal names its rule and, at most, a path: never contents.
    #[test]
    fn every_refusal_reads_as_one_line_naming_its_rule() {
        let path = || "drafts/a.md".to_string();
        for (error, expected) in [
            (SourceArchiveError::TooLarge, "archive exceeds"),
            (SourceArchiveError::Unreadable, "not a readable ZIP"),
            (SourceArchiveError::TooManyEntries, "more than"),
            (
                SourceArchiveError::EntryTooLarge(path()),
                "drafts/a.md exceeds",
            ),
            (SourceArchiveError::TotalTooLarge, "inflates past"),
            (
                SourceArchiveError::RatioTooHigh(path()),
                "inflates more than",
            ),
            (SourceArchiveError::Encrypted(path()), "is encrypted"),
            (
                SourceArchiveError::UnsupportedCompression(path()),
                "unsupported compression",
            ),
            (SourceArchiveError::Symlink(path()), "symbolic link"),
            (SourceArchiveError::NotRegular(path()), "not a regular file"),
            (SourceArchiveError::InvalidPath(path()), "not a canonical"),
            (SourceArchiveError::Collision(path()), "collides"),
            (SourceArchiveError::Empty, "holds no files"),
            (
                SourceArchiveError::OutsideLayout(path()),
                "outside the headless",
            ),
            (SourceArchiveError::BuildOutput(path()), "build output"),
            (SourceArchiveError::NotPointer(path()), "document pointers"),
            (SourceArchiveError::NotText(path()), "not UTF-8 text"),
            (SourceArchiveError::Credential(path()), "credential"),
        ] {
            let message = error.to_string();
            assert!(message.contains(expected), "{message}");
            assert!(!message.contains('\n'), "{message}");
        }
    }
}
