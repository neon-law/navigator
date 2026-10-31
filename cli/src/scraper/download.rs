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

fn hex_digest(digest: &[u8]) -> String {
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
