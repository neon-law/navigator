//! Resumable downloads, write-bytes-first — the pattern
//! `crate::projects::docket::ingest` uses for docket attachments, extended
//! with a per-site ledger.
//!
//! Every document downloads into a `.part` file beside its final name while
//! a SHA-256 runs over the bytes. The ledger records each finished document
//! with its hash and size, so a run that dies mid-way resumes from the last
//! byte offset of the interrupted file, skips every document it has already
//! staged, and never re-fetches what it has. Partial bytes never enter the
//! intake: only a `finish`ed download produces a document.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::scraper::adapter::NoticeKey;
use crate::scraper::fetcher::{FetchResponse, HttpFetcher};

/// Why a download could not be staged.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DownloadError {
    #[error("the fetch failed")]
    Fetch(#[from] crate::scraper::fetcher::FetchError),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the download ledger at {path} is corrupt: {detail}")]
    Ledger { path: PathBuf, detail: String },
}

/// What a finished download carries into the ledger and the intake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompletedDownload {
    pub key: NoticeKey,
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
}

/// One finished download as the ledger records it: hash, size, and the
/// staged file path, keyed by the notice's stable [`NoticeKey`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LedgerEntry {
    pub sha256: String,
    pub bytes: u64,
    pub path: PathBuf,
}

/// The per-site ledger of finished downloads, persisted atomically beside
/// the staged files.
#[derive(Debug)]
pub(crate) struct DownloadLedger {
    path: PathBuf,
    completed: BTreeMap<NoticeKey, LedgerEntry>,
}

impl DownloadLedger {
    /// Load the ledger, if one exists; a missing file is a fresh site.
    ///
    /// # Errors
    ///
    /// When the ledger cannot be parsed or the read fails for another
    /// reason.
    pub(crate) fn load(path: PathBuf) -> Result<Self, DownloadError> {
        let completed = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<BTreeMap<NoticeKey, LedgerEntry>>(&bytes)
                .map_err(|error| DownloadError::Ledger {
                    path: path.clone(),
                    detail: error.to_string(),
                })?,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(source) => {
                return Err(DownloadError::Io {
                    path: path.clone(),
                    source,
                });
            }
        };
        Ok(Self { path, completed })
    }

    /// The finished download recorded for `key`, if any.
    #[must_use]
    pub(crate) fn completed(&self, key: &NoticeKey) -> Option<&LedgerEntry> {
        self.completed.get(key)
    }

    /// Forget a ledger row whose staged file no longer exists (staging
    /// directories get cleaned; the ledger alone must not promise bytes).
    ///
    /// # Errors
    ///
    /// When the ledger cannot be re-persisted.
    pub(crate) fn forget(&mut self, key: &NoticeKey) -> Result<(), DownloadError> {
        if self.completed.remove(key).is_some() {
            self.save()?;
        }
        Ok(())
    }

    /// Record a finished download and persist the ledger.
    ///
    /// # Errors
    ///
    /// When the ledger cannot be re-persisted.
    pub(crate) fn record(&mut self, completed: CompletedDownload) -> Result<(), DownloadError> {
        self.completed.insert(
            completed.key.clone(),
            LedgerEntry {
                sha256: completed.sha256,
                bytes: completed.bytes,
                path: completed.path,
            },
        );
        self.save()
    }

    /// Persist the ledger atomically (write beside, rename into place).
    fn save(&self) -> Result<(), DownloadError> {
        let bytes =
            serde_json::to_vec_pretty(&self.completed).map_err(|error| DownloadError::Ledger {
                path: self.path.clone(),
                detail: error.to_string(),
            })?;
        crate::scraper::fs::atomic_write(&self.path, &bytes).map_err(|source| DownloadError::Io {
            path: self.path.clone(),
            source,
        })
    }
}

/// The write-bytes-first staging area for one document: a `.part` file that
/// grows append-only, a running SHA-256, and the byte offset a resumed
/// fetch continues from.
pub(crate) struct DownloadSink {
    key: NoticeKey,
    part_path: PathBuf,
    final_path: PathBuf,
    file: std::fs::File,
    offset: u64,
    hasher: Sha256,
}

impl DownloadSink {
    /// Open (or re-open) the staging area for `key`. An existing `.part`
    /// file resumes: the offset and the running hash are rebuilt from the
    /// bytes already on disk.
    ///
    /// # Errors
    ///
    /// When the staging directory cannot be created or the part file cannot
    /// be read or opened.
    pub(crate) fn open(
        staging_dir: &std::path::Path,
        final_path: PathBuf,
        key: NoticeKey,
    ) -> Result<Self, DownloadError> {
        std::fs::create_dir_all(staging_dir).map_err(|source| DownloadError::Io {
            path: staging_dir.to_path_buf(),
            source,
        })?;
        let part_path = staging_dir.join(format!("{}.part", staging_name(&key)));
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&part_path)
            .map_err(|source| DownloadError::Io {
                path: part_path.clone(),
                source,
            })?;
        // Rebuild the resume offset and the running hash from the bytes the
        // previous attempt already landed.
        let mut offset = 0u64;
        let mut hasher = Sha256::new();
        for chunk in read_chunks(&mut file)? {
            hasher.update(&chunk);
            offset += chunk.len() as u64;
        }
        Ok(Self {
            key,
            part_path,
            final_path,
            file,
            offset,
            hasher,
        })
    }

    /// The byte offset the next fetch should ask the server for.
    #[must_use]
    pub(crate) fn resume_offset(&self) -> u64 {
        self.offset
    }

    /// Append bytes to the part file, advancing the offset and the hash.
    ///
    /// # Errors
    ///
    /// When the part file cannot be written.
    #[cfg_attr(not(test), allow(dead_code))] // adapters stream bytes through this
    pub(crate) fn extend(&mut self, bytes: &[u8]) -> Result<(), DownloadError> {
        std::io::Write::write_all(&mut self.file, bytes).map_err(|source| DownloadError::Io {
            path: self.part_path.clone(),
            source,
        })?;
        self.hasher.update(bytes);
        self.offset += bytes.len() as u64;
        Ok(())
    }

    /// Discard everything staged so far and restart the document from zero.
    ///
    /// # Errors
    ///
    /// When the part file cannot be truncated.
    #[cfg_attr(not(test), allow(dead_code))] // adapters restart on a checksum mismatch
    pub(crate) fn restart(&mut self) -> Result<(), DownloadError> {
        self.file.set_len(0).map_err(|source| DownloadError::Io {
            path: self.part_path.clone(),
            source,
        })?;
        self.offset = 0;
        self.hasher = Sha256::new();
        Ok(())
    }

    /// Flush and rename the part file into its final staged name. Only a
    /// finished download yields bytes — until this, a crash leaves a
    /// `.part` file that the next run resumes, and the intake has seen
    /// nothing.
    ///
    /// # Errors
    ///
    /// When the flush, rename, or hashing fails.
    pub(crate) fn finish(self) -> Result<CompletedDownload, DownloadError> {
        let mut file = self.file;
        std::io::Write::flush(&mut file).map_err(|source| DownloadError::Io {
            path: self.part_path.clone(),
            source,
        })?;
        file.sync_all().map_err(|source| DownloadError::Io {
            path: self.part_path.clone(),
            source,
        })?;
        drop(file);
        std::fs::rename(&self.part_path, &self.final_path).map_err(|source| DownloadError::Io {
            path: self.part_path.clone(),
            source,
        })?;
        let digest = self.hasher.finalize();
        Ok(CompletedDownload {
            key: self.key,
            path: self.final_path,
            sha256: hex_digest(&digest),
            bytes: self.offset,
        })
    }
}

/// Fetch `url` into `sink`, resuming: the first request carries no `Range`
/// header, a resumed request carries `bytes=<offset>-`, and a server that
/// answers a range request with a full `200` body discards the stale part
/// and restarts. Adapter `download` implementations fill their sink through
/// this helper.
///
/// # Errors
///
/// Fetch failures and staging I/O; see [`DownloadError`].
#[cfg_attr(not(test), allow(dead_code))] // adapters fetch through this helper
pub(crate) async fn fetch_into(
    sink: &mut DownloadSink,
    fetcher: &dyn HttpFetcher,
    url: &str,
) -> Result<(), DownloadError> {
    let resume_offset = sink.resume_offset();
    let request = crate::scraper::fetcher::FetchRequest {
        url: url.to_owned(),
        range_start: (resume_offset > 0).then_some(resume_offset),
        headers: std::collections::BTreeMap::new(),
    };
    let FetchResponse { status, body } = fetcher.fetch(&request).await?;
    // 206 Partial Content continues the staged bytes; anything else answers
    // the whole document, so the stale part must go.
    if resume_offset > 0 && status != 206 {
        sink.restart()?;
    }
    sink.extend(&body)
}

/// The part/final file name for one notice key: a stable digest of the key
/// itself, so court titles and entry numbers never reach the filesystem.
pub(crate) fn staging_name(key: &NoticeKey) -> String {
    let digest = Sha256::digest(key.as_str().as_bytes());
    hex_digest(&digest)
}

pub(crate) fn hex_digest(digest: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        // Infallible on a `String`.
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Read a file back in 64 KiB chunks — how a resumed sink rebuilds its
/// running hash over the bytes a previous attempt landed.
fn read_chunks(file: &mut std::fs::File) -> Result<Vec<Vec<u8>>, DownloadError> {
    use std::io::Read;
    let mut chunks = Vec::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|source| DownloadError::Io {
            path: PathBuf::from("part file"),
            source,
        })?;
        if read == 0 {
            return Ok(chunks);
        }
        chunks.push(buffer[..read].to_vec());
    }
}

#[cfg(test)]
mod tests {
    use super::{
        fetch_into, hex_digest, staging_name, CompletedDownload, DownloadError, DownloadLedger,
        DownloadSink,
    };
    use crate::scraper::adapter::{NoticeKey, SiteKey};
    use crate::scraper::fetcher::{FetchRequest, FetchResponse};
    use crate::scraper::testing::MockFetcher;
    use sha2::{Digest, Sha256};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn site() -> SiteKey {
        SiteKey::new("la-superior").expect("valid key")
    }

    fn key() -> NoticeKey {
        NoticeKey::new(&site(), "0004", "7")
    }

    /// A private staging area per test: ledger path, staging dir, and the
    /// final staged file path, all under one throwaway root.
    struct Scratch {
        root: PathBuf,
    }

    impl Scratch {
        fn new(label: &str) -> Self {
            Self {
                root: std::env::temp_dir().join(format!(
                    "navigator-scraper-dl-{label}-{}",
                    uuid::Uuid::now_v7()
                )),
            }
        }

        fn staging(&self) -> PathBuf {
            self.root.join("staging")
        }

        fn final_path(&self) -> PathBuf {
            self.staging().join("DKT 0004 - Notice of Hearing.pdf")
        }

        fn ledger(&self) -> PathBuf {
            self.root.join("ledger.json")
        }

        fn on_disk(path: &std::path::Path) -> Vec<u8> {
            std::fs::read(path).expect("the staged file exists")
        }
    }

    /// A handler that answers every fetch with `status` and `body`.
    fn static_handler(status: u16, body: Vec<u8>) -> MockFetcher {
        MockFetcher::with_handler(Arc::new(move |_: &FetchRequest| {
            Ok(FetchResponse {
                status,
                body: body.clone(),
            })
        }))
    }

    #[tokio::test]
    async fn the_first_fetch_has_no_range_header_and_lands_the_whole_document() {
        let scratch = Scratch::new("first-fetch");
        let body: Vec<u8> = (0u8..=255).cycle().take(10_000).collect();

        let mut ledger = DownloadLedger::load(scratch.ledger()).expect("fresh ledger");
        let mut sink = DownloadSink::open(&scratch.staging(), scratch.final_path(), key())
            .expect("fresh sink");
        assert_eq!(sink.resume_offset(), 0, "nothing staged yet");

        fetch_into(
            &mut sink,
            &static_handler(200, body.clone()),
            "https://portal.example.com/doc",
        )
        .await
        .expect("the fetch completes");
        let completed = sink.finish().expect("the download finalizes");

        assert_eq!(completed.bytes, body.len() as u64);
        assert_eq!(
            completed.sha256,
            hex_digest(&Sha256::digest(&body)),
            "the recorded hash covers exactly the staged bytes"
        );
        assert_eq!(Scratch::on_disk(&completed.path), body);
        ledger.record(completed).expect("the ledger persists");
        assert!(ledger.completed(&key()).is_some());
    }

    #[tokio::test]
    async fn a_resumed_fetch_carries_the_staged_offset_and_completes_the_document() {
        let scratch = Scratch::new("resume");
        let body: Vec<u8> = (0u8..=255).cycle().take(10_000).collect();
        // Owned copies: the mock handler is `'static` and must not borrow
        // the local `body`.
        let staged = body[..4_021].to_vec();
        let remainder = body[4_021..].to_vec();
        let staged_len = staged.len() as u64;

        // First attempt stages a prefix, then "crashes" (the sink drops).
        {
            let mut sink =
                DownloadSink::open(&scratch.staging(), scratch.final_path(), key()).expect("sink");
            sink.extend(&staged).expect("the prefix stages");
        }

        // The next run reopens the sink over the surviving bytes and asks
        // the server for the rest.
        let mut sink = DownloadSink::open(&scratch.staging(), scratch.final_path(), key())
            .expect("the sink reopens");
        assert_eq!(
            sink.resume_offset(),
            staged.len() as u64,
            "the resume offset is rebuilt from the part file"
        );

        let fetcher = MockFetcher::with_handler(Arc::new(move |request: &FetchRequest| {
            assert_eq!(
                request.range_start,
                Some(staged_len),
                "the resumed request carries the staged offset"
            );
            Ok(FetchResponse {
                status: 206,
                body: remainder.clone(),
            })
        }));
        fetch_into(&mut sink, &fetcher, "https://portal.example.com/doc")
            .await
            .expect("the resumed fetch completes");
        let completed = sink.finish().expect("the download finalizes");

        assert_eq!(completed.bytes, body.len() as u64);
        assert_eq!(completed.sha256, hex_digest(&Sha256::digest(&body)));
        assert_eq!(Scratch::on_disk(&completed.path), body);
        assert_eq!(
            fetcher.request_count("portal.example.com"),
            1,
            "resuming never re-fetches the staged prefix"
        );
    }

    #[tokio::test]
    async fn a_full_200_answer_to_a_range_request_restarts_the_sink() {
        let scratch = Scratch::new("stale-part");
        let body = b"the current document, in full".to_vec();

        // Stage bytes from a stale attempt, then reopen for resume.
        {
            let mut sink =
                DownloadSink::open(&scratch.staging(), scratch.final_path(), key()).expect("sink");
            sink.extend(b"stale bytes from a superseded document")
                .expect("stage");
        }
        let mut sink = DownloadSink::open(&scratch.staging(), scratch.final_path(), key())
            .expect("the sink reopens");
        assert!(sink.resume_offset() > 0);

        // The server does not honor the range: a 200 means the whole
        // document is in the body and the staged bytes are stale.
        fetch_into(
            &mut sink,
            &static_handler(200, body.clone()),
            "https://portal.example.com/doc",
        )
        .await
        .expect("the fetch completes");
        let completed = sink.finish().expect("the download finalizes");

        assert_eq!(
            completed.bytes,
            body.len() as u64,
            "the stale bytes are gone"
        );
        assert_eq!(Scratch::on_disk(&completed.path), body);
        assert_eq!(completed.sha256, hex_digest(&Sha256::digest(&body)));
    }

    #[tokio::test]
    async fn restart_discards_the_staged_bytes_and_rebuilds_the_hash() {
        let scratch = Scratch::new("restart");
        let body = b"a fresh copy after a checksum mismatch".to_vec();

        let mut sink =
            DownloadSink::open(&scratch.staging(), scratch.final_path(), key()).expect("sink");
        sink.extend(b"corrupt staging").expect("stage");
        sink.restart().expect("the sink restarts");
        assert_eq!(sink.resume_offset(), 0, "restart discards the staged bytes");

        fetch_into(
            &mut sink,
            &static_handler(200, body.clone()),
            "https://portal.example.com/doc",
        )
        .await
        .expect("the fetch completes");
        let completed = sink.finish().expect("the download finalizes");
        assert_eq!(Scratch::on_disk(&completed.path), body);
        assert_eq!(completed.sha256, hex_digest(&Sha256::digest(&body)));
    }

    #[test]
    fn a_completed_download_survives_a_ledger_reload() {
        let scratch = Scratch::new("ledger-reload");
        let completed = CompletedDownload {
            key: key(),
            path: scratch.final_path(),
            sha256: "abc123".to_owned(),
            bytes: 5_000,
        };

        let mut ledger = DownloadLedger::load(scratch.ledger()).expect("fresh ledger");
        ledger
            .record(completed.clone())
            .expect("the ledger persists");
        drop(ledger);

        let reloaded = DownloadLedger::load(scratch.ledger()).expect("the ledger reloads");
        let entry = reloaded.completed(&key()).expect("the row survives");
        assert_eq!(entry.sha256, "abc123");
        assert_eq!(entry.bytes, 5_000);
        assert_eq!(entry.path, scratch.final_path());
    }

    #[test]
    fn forget_drops_a_row_whose_staged_file_is_gone() {
        let scratch = Scratch::new("forget");
        let mut ledger = DownloadLedger::load(scratch.ledger()).expect("fresh ledger");
        ledger
            .record(CompletedDownload {
                key: key(),
                path: scratch.final_path(),
                sha256: "abc123".to_owned(),
                bytes: 10,
            })
            .expect("the ledger persists");

        ledger.forget(&key()).expect("the forget persists");
        assert!(
            ledger.completed(&key()).is_none(),
            "the forgotten row is gone from the live ledger"
        );
        assert!(
            DownloadLedger::load(scratch.ledger())
                .expect("the ledger reloads")
                .completed(&key())
                .is_none(),
            "the forget survives the reload"
        );

        // Forgetting a key that was never recorded is a harmless no-op.
        let other = NoticeKey::new(&site(), "0004", "8");
        ledger
            .forget(&other)
            .expect("an unknown key forgets cleanly");
    }

    #[test]
    fn a_corrupt_ledger_is_a_named_error_not_a_panic() {
        let scratch = Scratch::new("corrupt");
        std::fs::create_dir_all(&scratch.root).expect("scratch dir");
        std::fs::write(scratch.ledger(), b"this is not a ledger").expect("write corrupt bytes");

        let outcome = DownloadLedger::load(scratch.ledger());
        assert!(
            matches!(outcome, Err(DownloadError::Ledger { .. })),
            "the corrupt ledger fails with the typed error"
        );
    }

    #[test]
    fn the_staging_name_is_a_stable_hex_digest_of_the_key() {
        let first = staging_name(&key());
        let second = staging_name(&NoticeKey::new(&site(), "0004", "7"));
        assert_eq!(first, second, "the same key always maps to the same name");
        assert_eq!(first.len(), 64, "a SHA-256 digest renders as 64 hex chars");
        assert!(
            first.chars().all(|c| c.is_ascii_hexdigit()),
            "the name is filesystem-safe hex"
        );
        let other = staging_name(&NoticeKey::new(&site(), "0004", "8"));
        assert_ne!(first, other, "a different notice maps to a different name");
    }

    #[test]
    fn hex_digest_renders_lowercase_hex() {
        // Known SHA-256 vector: the empty string.
        let empty = Sha256::digest(b"");
        assert_eq!(
            hex_digest(&empty),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
