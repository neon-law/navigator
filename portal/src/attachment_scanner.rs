//! The shared scanner and Portal's deterministic test double.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

pub use cloud::attachment_scanner::{
    AttachmentScanner, ClamdAttachmentScanner, ScanError, ScanVerdict, DEFAULT_MAX_STREAM_BYTES,
};

/// Deterministic scanner used at the Tier-1 trait boundary.
#[derive(Clone)]
pub struct FakeAttachmentScanner {
    result: Result<ScanVerdict, ScanError>,
    calls: Arc<AtomicUsize>,
}

impl FakeAttachmentScanner {
    #[must_use]
    pub fn new(result: Result<ScanVerdict, ScanError>) -> Self {
        Self {
            result,
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    #[must_use]
    pub fn clean() -> Self {
        Self::new(Ok(ScanVerdict::Clean))
    }

    #[must_use]
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl AttachmentScanner for FakeAttachmentScanner {
    async fn scan(&self, _bytes: &[u8]) -> Result<ScanVerdict, ScanError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.result.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fake_counts_each_scan_exactly_once() {
        let scanner = FakeAttachmentScanner::clean();
        scanner.scan(b"a").await.unwrap();
        scanner.scan(b"b").await.unwrap();
        assert_eq!(scanner.calls(), 2);
    }
}
