//! Fingerprinted, time-bound links to one filed matter document.
//!
//! The document page used to hand the browser a fixed
//! `…/documents/{doc_id}/download` path: the same string on every render, good
//! forever, and naming no particular bytes. A link minted here instead carries
//! three query parameters:
//!
//! - `fp` — the first 16 hex digits of the document's recorded SHA-256, so the
//!   link names the exact bytes it was minted for;
//! - `expires` — a Unix timestamp one hour out ([`LINK_TTL`]);
//! - `sig` — an HMAC-SHA256 over the matter code, document id, fingerprint, and
//!   expiry, keyed by a sub-key derived from `SESSION_SECRET`.
//!
//! The route still sits behind the session, policy, and matter-lens guards, so
//! the signature is never the only credential: it binds the URL to content and
//! a deadline. Because a valid link always serves the same bytes until it
//! expires, the download handler can answer it with a `private, immutable`
//! cache lifetime, and the PDF.js viewer re-fetching a page range never
//! re-downloads the file.
//!
//! The link is same-origin by design. A storage-origin signed URL would need
//! the bucket's CORS policy and the page's `connect-src` widened
//! (`docs/signed-url-delivery-audit.md`, Finding 7); this keeps every
//! directive at `'self'`.

use std::time::Duration;

use base64::Engine as _;
use hmac::digest::KeyInit;
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// How long a minted link stays valid.
pub const LINK_TTL: Duration = Duration::from_hours(1);

/// Hex digits of the SHA-256 a link carries as its fingerprint. Sixty-four bits
/// names a revision unambiguously within one document; the full digest is on
/// the page already.
const FINGERPRINT_HEX_LEN: usize = 16;

/// Domain separator for the sub-key, so a document-link signature can never be
/// replayed as a session cookie signature or the reverse.
const KEY_CONTEXT: &[u8] = b"navigator/document-link/v1";

/// The query parameters of a signed link, as the download route reads them. An
/// `expires` that is present but not an integer arrives as `None` alongside the
/// others, which [`DocumentLinkSigner::verify`] refuses as invalid.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LinkParams {
    pub fp: Option<String>,
    pub expires: Option<i64>,
    pub sig: Option<String>,
}

impl LinkParams {
    /// Whether the request carries any part of a signed link. A request with
    /// none is a plain session-gated download; one with only some of them is
    /// malformed and is refused rather than quietly served unsigned.
    #[must_use]
    pub fn is_present(&self) -> bool {
        self.fp.is_some() || self.expires.is_some() || self.sig.is_some()
    }
}

/// Why a signed link was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkRefusal {
    /// A parameter is missing or the signature does not verify.
    Invalid,
    /// The signature verifies but the deadline has passed.
    Expired,
    /// The link was minted for other bytes than the document now holds.
    FingerprintMismatch,
}

/// Mints and verifies document links. Cheap to clone.
#[derive(Clone)]
pub struct DocumentLinkSigner {
    key: std::sync::Arc<[u8; 32]>,
}

impl std::fmt::Debug for DocumentLinkSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DocumentLinkSigner(..)")
    }
}

impl DocumentLinkSigner {
    /// Derive the link key from the deployment's session secret.
    #[must_use]
    pub fn from_secret(secret: &[u8]) -> Self {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts any key");
        mac.update(KEY_CONTEXT);
        Self {
            key: std::sync::Arc::new(mac.finalize().into_bytes().into()),
        }
    }

    /// The fingerprinted, time-bound `href` for `download_path` (the plain
    /// `…/documents/{doc_id}/download` path), valid until `now + LINK_TTL`.
    /// `extra` query pairs (such as `inline=1`) are appended unsigned: they
    /// select a presentation, not which bytes or for how long.
    #[must_use]
    pub fn href(
        &self,
        download_path: &str,
        project_code: &str,
        doc_id: &str,
        sha256_hex: &str,
        now_unix: i64,
        extra: &str,
    ) -> String {
        let fp = fingerprint(sha256_hex);
        let expires = now_unix + i64::try_from(LINK_TTL.as_secs()).unwrap_or(i64::MAX);
        let sig = self.sign(project_code, doc_id, &fp, expires);
        let extra = if extra.is_empty() {
            String::new()
        } else {
            format!("&{extra}")
        };
        format!("{download_path}?fp={fp}&expires={expires}&sig={sig}{extra}")
    }

    /// Verify a signed link against the document it is being used to fetch,
    /// returning the seconds it has left on success.
    ///
    /// # Errors
    ///
    /// A [`LinkRefusal`] naming why the link may not be served.
    pub fn verify(
        &self,
        params: &LinkParams,
        project_code: &str,
        doc_id: &str,
        sha256_hex: &str,
        now_unix: i64,
    ) -> Result<u64, LinkRefusal> {
        let (Some(fp), Some(expires), Some(sig)) = (&params.fp, params.expires, &params.sig) else {
            return Err(LinkRefusal::Invalid);
        };
        let Ok(sig) = b64().decode(sig) else {
            return Err(LinkRefusal::Invalid);
        };
        let mut mac = self.mac();
        mac.update(&payload(project_code, doc_id, fp, expires));
        // `verify_slice` is constant-time.
        if mac.verify_slice(&sig).is_err() {
            return Err(LinkRefusal::Invalid);
        }
        if expires <= now_unix {
            return Err(LinkRefusal::Expired);
        }
        if *fp != fingerprint(sha256_hex) {
            return Err(LinkRefusal::FingerprintMismatch);
        }
        Ok(u64::try_from(expires - now_unix).unwrap_or(0))
    }

    fn sign(&self, project_code: &str, doc_id: &str, fp: &str, expires: i64) -> String {
        let mut mac = self.mac();
        mac.update(&payload(project_code, doc_id, fp, expires));
        b64().encode(mac.finalize().into_bytes())
    }

    fn mac(&self) -> Hmac<Sha256> {
        Hmac::<Sha256>::new_from_slice(self.key.as_slice()).expect("HMAC accepts any key")
    }
}

/// The current Unix time in seconds.
#[must_use]
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}

fn fingerprint(sha256_hex: &str) -> String {
    sha256_hex
        .chars()
        .take(FINGERPRINT_HEX_LEN)
        .collect::<String>()
        .to_ascii_lowercase()
}

/// The signed bytes. Fields are newline-separated; none of them may contain a
/// newline (a matter code, a UUID, hex, and an integer), so the encoding is
/// unambiguous.
fn payload(project_code: &str, doc_id: &str, fp: &str, expires: i64) -> Vec<u8> {
    format!("{project_code}\n{doc_id}\n{fp}\n{expires}").into_bytes()
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "ABCDEF0123456789ffffffffffffffffffffffffffffffffffffffffffffffff";
    const PATH: &str = "/app/projects/sample-estate/documents/D/download";
    const NOW: i64 = 1_800_000_000;

    fn signer() -> DocumentLinkSigner {
        DocumentLinkSigner::from_secret(b"test-session-secret")
    }

    fn params_of(href: &str) -> LinkParams {
        let query = href.split_once('?').expect("query").1;
        let mut params = LinkParams::default();
        for pair in query.split('&') {
            let (k, v) = pair.split_once('=').expect("pair");
            match k {
                "fp" => params.fp = Some(v.to_string()),
                "expires" => params.expires = Some(v.parse().expect("int")),
                "sig" => params.sig = Some(v.to_string()),
                _ => {}
            }
        }
        params
    }

    #[test]
    fn a_minted_link_carries_the_fingerprint_and_an_hour() {
        let href = signer().href(PATH, "sample-estate", "D", SHA, NOW, "inline=1");
        assert!(
            href.starts_with(&format!("{PATH}?fp=abcdef0123456789&expires=")),
            "{href}"
        );
        assert!(href.ends_with("&inline=1"), "{href}");
        let params = params_of(&href);
        assert_eq!(params.expires, Some(NOW + 3600));
    }

    #[test]
    fn a_fresh_link_verifies_with_its_remaining_lifetime() {
        let href = signer().href(PATH, "sample-estate", "D", SHA, NOW, "");
        let left = signer().verify(&params_of(&href), "sample-estate", "D", SHA, NOW + 600);
        assert_eq!(left, Ok(3000));
    }

    #[test]
    fn an_expired_link_is_refused_as_expired() {
        let href = signer().href(PATH, "sample-estate", "D", SHA, NOW, "");
        let verdict = signer().verify(&params_of(&href), "sample-estate", "D", SHA, NOW + 3600);
        assert_eq!(verdict, Err(LinkRefusal::Expired));
    }

    #[test]
    fn a_link_for_other_bytes_is_refused() {
        let href = signer().href(PATH, "sample-estate", "D", SHA, NOW, "");
        let other = "0000000000000000ffffffffffffffffffffffffffffffffffffffffffffffff";
        let verdict = signer().verify(&params_of(&href), "sample-estate", "D", other, NOW);
        assert_eq!(verdict, Err(LinkRefusal::FingerprintMismatch));
    }

    #[test]
    fn a_link_moved_to_another_document_or_matter_is_refused() {
        let href = signer().href(PATH, "sample-estate", "D", SHA, NOW, "");
        let params = params_of(&href);
        assert_eq!(
            signer().verify(&params, "sample-estate", "E", SHA, NOW),
            Err(LinkRefusal::Invalid)
        );
        assert_eq!(
            signer().verify(&params, "sample-litigation", "D", SHA, NOW),
            Err(LinkRefusal::Invalid)
        );
    }

    #[test]
    fn an_extended_deadline_breaks_the_signature() {
        let href = signer().href(PATH, "sample-estate", "D", SHA, NOW, "");
        let mut params = params_of(&href);
        params.expires = params.expires.map(|e| e + 86_400);
        assert_eq!(
            signer().verify(&params, "sample-estate", "D", SHA, NOW),
            Err(LinkRefusal::Invalid)
        );
    }

    #[test]
    fn another_deployments_secret_does_not_verify() {
        let href = signer().href(PATH, "sample-estate", "D", SHA, NOW, "");
        let other = DocumentLinkSigner::from_secret(b"another-secret");
        assert_eq!(
            other.verify(&params_of(&href), "sample-estate", "D", SHA, NOW),
            Err(LinkRefusal::Invalid)
        );
    }

    #[test]
    fn a_partial_link_is_invalid_and_none_is_absent() {
        assert!(!LinkParams::default().is_present());
        let partial = LinkParams {
            fp: Some("abcdef0123456789".to_string()),
            ..LinkParams::default()
        };
        assert!(partial.is_present());
        assert_eq!(
            signer().verify(&partial, "sample-estate", "D", SHA, NOW),
            Err(LinkRefusal::Invalid)
        );
    }
}
