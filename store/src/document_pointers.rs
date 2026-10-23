//! The source-only YAML pointer to one Project document revision chain — a
//! Blob, committed at `blobs/<slug>.yaml` in a Project repository.
//!
//! A pointer is safe to commit because it contains metadata and an `assets`
//! row id, never an object-storage coordinate or legal-document bytes. The
//! repository path below `blobs/`, without `.yaml`, is the document slug; the
//! bytes sit at the Git-ignored `documents/<Matter Folder>/<slug>`, the
//! [`MatterFolder`] named by `folder:` or else by `kind` (LAW-189).

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One committed declaration of a Project document's current revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentPointer {
    /// A Project-document classification from the closed asset-lane
    /// vocabulary. Empty when [`Self::authority_id`] is set: an Authority
    /// capture (`blobs/cases/**`, `blobs/rules/**`) is archived under
    /// a global Authority, never filed on this Project as a document of any
    /// kind, so no asset-lane value describes it (LAW-82).
    pub kind: String,
    pub visibility: String,
    /// The [`MatterFolder`] this document's bytes are downloaded into, when
    /// it is not the one [`MatterFolder::for_kind`] names. Absent on every
    /// pointer written before LAW-189, which every existing pointer keeps
    /// byte for byte.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<MatterFolder>,
    pub current_version: PointerVersion,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_version: Option<Uuid>,
    /// Present only on an Authority capture routed through `site authorities
    /// create` (`blobs/cases/**` or `blobs/rules/**`): the global
    /// Authority — no `project_id` — this capture's bytes were archived
    /// under. Absent for every other document kind, which stays a plain
    /// Project document with no Authority of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_id: Option<Uuid>,
    /// The `DocuSign` envelope this signed document came from (ENG-859).
    /// Required by `project gate` on a `blobs/onboarding/**` or
    /// `blobs/offboarding/**` pointer named `<name>_signed.pdf`; absent
    /// on every other pointer. Validated here only as a well-formed UUID —
    /// `project gate --check` (ENG-863) is the seam that resolves it against
    /// `DocuSign` and confirms the envelope is complete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docusign_envelope_id: Option<Uuid>,
    /// The Xero invoice this pointer's bytes were downloaded from (ENG-859).
    /// Required by `project gate` on every `blobs/invoices/**` pointer.
    /// Validated here only as a well-formed UUID — `project gate --check`
    /// (ENG-863) resolves it against the Firm's Xero org.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xero_invoice_id: Option<Uuid>,
    /// How this executed document was signed: `{ method: wet_ink | docusign,
    /// signed_on: YYYY-MM-DD }`. Optional on every pointer, including a
    /// `_signed` one; no gate requires it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing: Option<crate::documents::Signing>,
    /// The notarial acknowledgments on this executed document, each `{ method:
    /// in_person | remote_online, acknowledged_on, state, county,
    /// commission_expires }`. Optional; never the notary's name or commission
    /// number.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notarization: Vec<crate::documents::Notarization>,
    /// The Service records on this document, each `{ recipient, counsel_for,
    /// method, served_on, address, declarant }`: one served copy × one
    /// recipient × one method × one date. `project sync` writes the live
    /// records here and keeps a hand-recorded list while the live Project
    /// has none; a pleading's certificate of service is composed from it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub service: Vec<crate::project_service::ServiceRecord>,
}

/// One of the five lawyer-facing folders a Project checkout's `documents/`
/// holds a document's bytes in (LAW-189), named the way a case-management
/// system names them. A pointer's `folder:` is one of these, spelled exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MatterFolder {
    /// What the client supplied: exhibits, records, and screenshots.
    #[serde(rename = "Client Production")]
    ClientProduction,
    /// Letters served on, or received from, another party.
    #[serde(rename = "Correspondence (Served)")]
    CorrespondenceServed,
    /// Discovery requests and responses.
    #[serde(rename = "Discovery")]
    Discovery,
    /// Court paper: pleadings, filings, orders, and docket ingest.
    #[serde(rename = "Docket Filings")]
    DocketFilings,
    /// The firm's own work: memos, transcripts, drafts, and everything else.
    #[serde(rename = "Work Product")]
    WorkProduct,
}

impl MatterFolder {
    /// Every matter folder, in the order a Finder window lists them.
    pub const ALL: [Self; 5] = [
        Self::ClientProduction,
        Self::CorrespondenceServed,
        Self::Discovery,
        Self::DocketFilings,
        Self::WorkProduct,
    ];

    /// The folder's name on disk and in a pointer's `folder:` key.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClientProduction => "Client Production",
            Self::CorrespondenceServed => "Correspondence (Served)",
            Self::Discovery => "Discovery",
            Self::DocketFilings => "Docket Filings",
            Self::WorkProduct => "Work Product",
        }
    }

    /// The folder named exactly `name`, or `None`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|folder| folder.as_str() == name)
    }

    /// The folder a document of `kind` lands in when its pointer names none.
    /// No kind means discovery, so [`Self::Discovery`] is reached only by an
    /// explicit `folder:`.
    #[must_use]
    pub fn for_kind(kind: &str) -> Self {
        match kind {
            "exhibit" => Self::ClientProduction,
            "letter" => Self::CorrespondenceServed,
            // A court's order is neither a pleading nor a filing, but it is
            // docket paper all the same (LAW-189).
            "pleading" | "filing" | "order" => Self::DocketFilings,
            _ => Self::WorkProduct,
        }
    }

    /// The kind a file dropped into this folder is uploaded with when its
    /// path names no more specific one.
    #[must_use]
    pub const fn default_kind(self) -> &'static str {
        match self {
            Self::ClientProduction => "exhibit",
            Self::CorrespondenceServed => "letter",
            Self::DocketFilings => "pleading",
            Self::Discovery | Self::WorkProduct => "unclassified",
        }
    }
}

impl DocumentPointer {
    /// The folder this document's bytes belong in: `folder:` when the
    /// pointer names one, else the one its `kind` maps to.
    #[must_use]
    pub fn matter_folder(&self) -> MatterFolder {
        self.folder
            .unwrap_or_else(|| MatterFolder::for_kind(&self.kind))
    }
}

/// The immutable facts copied from the current `assets` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PointerVersion {
    pub version: usize,
    pub asset_id: Uuid,
    pub created_at: String,
    pub sha256: String,
    pub size_bytes: i64,
    /// Where this revision was captured from, and when — carried only by an
    /// Authority capture's pointer (see [`DocumentPointer::authority_id`]),
    /// so the portal can render a real citation/link instead of an opaque
    /// file. Absent for every other document kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_on: Option<String>,
}

/// Why a pointer cannot name a valid document revision.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PointerError {
    #[error("kind `{0}` is not an accepted document kind")]
    Kind(String),
    /// An Authority-backed pointer (`authority_id` set) is never a Project
    /// document of any kind, so its `kind` must be empty (LAW-82).
    #[error("an authority-backed pointer's kind must be empty, not `{0}`")]
    KindOnAuthority(String),
    #[error("visibility must be `internal` or `client`")]
    Visibility,
    #[error("current_version.version must be a positive integer")]
    Version,
    #[error("current_version.created_at must be an RFC 3339 timestamp in UTC")]
    CreatedAt,
    #[error("current_version.sha256 must be exactly 64 lowercase hexadecimal characters")]
    Sha256,
    #[error("current_version.size_bytes must be a positive integer")]
    Size,
    #[error("previous_version must be absent at version 1 and present after version 1")]
    PreviousVersion,
    #[error(transparent)]
    Notarization(#[from] crate::documents::NotarizationError),
}

impl DocumentPointer {
    /// Parse YAML and enforce the document-pointer contract.
    ///
    /// # Errors
    /// A YAML shape error or a semantic [`PointerError`].
    pub fn from_yaml(raw: &str) -> anyhow::Result<Self> {
        let pointer: Self = serde_yaml::from_str(raw)?;
        pointer.validate()?;
        Ok(pointer)
    }

    /// Serialize the stable committed representation.
    ///
    /// # Errors
    /// A YAML serialization failure.
    pub fn to_yaml(&self) -> anyhow::Result<String> {
        Ok(serde_yaml::to_string(self)?)
    }

    /// Enforce constraints serde's field types cannot express.
    ///
    /// # Errors
    /// The first semantic pointer violation.
    pub fn validate(&self) -> Result<(), PointerError> {
        if self.authority_id.is_some() {
            if !self.kind.is_empty() {
                return Err(PointerError::KindOnAuthority(self.kind.clone()));
            }
        } else if !rules::kind::Kind::parse(&self.kind)
            .is_some_and(|kind| kind.valid_for(rules::kind::Lane::Asset))
        {
            return Err(PointerError::Kind(self.kind.clone()));
        }
        if !matches!(self.visibility.as_str(), "internal" | "client") {
            return Err(PointerError::Visibility);
        }
        if self.current_version.version == 0 {
            return Err(PointerError::Version);
        }
        let created_at = DateTime::parse_from_rfc3339(&self.current_version.created_at)
            .map_err(|_| PointerError::CreatedAt)?;
        if created_at.offset() != &FixedOffset::east_opt(0).ok_or(PointerError::CreatedAt)? {
            return Err(PointerError::CreatedAt);
        }
        let sha = self.current_version.sha256.as_bytes();
        if sha.len() != 64
            || !sha
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        {
            return Err(PointerError::Sha256);
        }
        if self.current_version.size_bytes <= 0 {
            return Err(PointerError::Size);
        }
        if (self.current_version.version == 1) != self.previous_version.is_none() {
            return Err(PointerError::PreviousVersion);
        }
        for notarization in &self.notarization {
            notarization.validate()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pointer() -> DocumentPointer {
        DocumentPointer {
            kind: "agreement".into(),
            visibility: "internal".into(),
            folder: None,
            current_version: PointerVersion {
                version: 1,
                asset_id: Uuid::now_v7(),
                created_at: "2026-09-05T12:00:00Z".into(),
                sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
                size_bytes: 42,
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
        }
    }

    fn evidence_pointer() -> DocumentPointer {
        let mut pointer = pointer();
        pointer.kind = String::new();
        pointer.authority_id = Some(Uuid::now_v7());
        pointer.current_version.canonical_url = Some("https://example.test/opinion".into());
        pointer.current_version.checked_on = Some("2026-09-05".into());
        pointer
    }

    #[test]
    fn round_trips_the_committed_yaml_shape() {
        let expected = pointer();
        let yaml = expected.to_yaml().unwrap();
        assert_eq!(DocumentPointer::from_yaml(&yaml).unwrap(), expected);
    }

    #[test]
    fn rejects_a_non_utc_timestamp_and_a_broken_chain() {
        let mut invalid = pointer();
        invalid.current_version.created_at = "2026-09-05T08:00:00-04:00".into();
        assert_eq!(invalid.validate(), Err(PointerError::CreatedAt));
        invalid.current_version.created_at = "2026-09-05T12:00:00Z".into();
        invalid.current_version.version = 2;
        assert_eq!(invalid.validate(), Err(PointerError::PreviousVersion));
    }

    /// `current_version.created_at` is required, not merely
    /// validated-when-present: a committed pointer YAML that omits the key
    /// entirely fails to parse at all, so `project gate --check` (which
    /// calls `from_yaml` on every committed pointer) fails it too.
    #[test]
    fn a_pointer_missing_created_at_entirely_fails_to_parse() {
        let yaml = "kind: agreement\n\
                     visibility: internal\n\
                     current_version:\n  \
                       version: 1\n  \
                       asset_id: 018f3b1a-0000-7000-8000-000000000000\n  \
                       sha256: 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n  \
                       size_bytes: 42\n";
        let error = DocumentPointer::from_yaml(yaml).unwrap_err();
        assert!(
            error.to_string().contains("created_at"),
            "the parse failure should name the missing field: {error}"
        );
    }

    /// An evidence capture's pointer (LAW-50) reuses this same struct rather
    /// than a parallel shape: `authority_id` on the identity, `canonical_url`
    /// and `checked_on` on the revision. It round-trips and validates like
    /// any other pointer — the new fields are additive, not a second schema.
    #[test]
    fn an_authority_backed_pointer_round_trips_and_validates() {
        let expected = evidence_pointer();
        assert!(expected.validate().is_ok());
        let yaml = expected.to_yaml().unwrap();
        assert!(yaml.contains("authority_id"));
        assert!(yaml.contains("canonical_url"));
        assert!(yaml.contains("checked_on"));
        assert_eq!(DocumentPointer::from_yaml(&yaml).unwrap(), expected);
    }

    /// A pointer with no Authority — every non-evidence document kind —
    /// serializes none of the three new fields, so an older reader (or a
    /// byte-for-byte diff) never sees noise it does not understand.
    #[test]
    fn a_plain_document_pointer_serializes_no_authority_fields() {
        let plain = pointer();
        let yaml = plain.to_yaml().unwrap();
        assert!(!yaml.contains("authority_id"));
        assert!(!yaml.contains("canonical_url"));
        assert!(!yaml.contains("checked_on"));
    }

    /// LAW-82: an Authority-backed pointer is never a Project document of any
    /// kind, so a non-empty `kind` (even `exhibit`) alongside `authority_id`
    /// is rejected, not silently accepted.
    #[test]
    fn an_authority_pointer_with_a_nonempty_kind_is_rejected() {
        let mut invalid = evidence_pointer();
        invalid.kind = "exhibit".into();
        assert_eq!(
            invalid.validate(),
            Err(PointerError::KindOnAuthority("exhibit".into()))
        );
    }

    /// `docusign_envelope_id` and `xero_invoice_id` (ENG-859) are additive
    /// and optional like `authority_id`: absent by default, round-trip when
    /// set, and `deny_unknown_fields` still catches a typo of either key.
    #[test]
    fn docusign_and_xero_ids_are_optional_and_round_trip() {
        let mut plain = pointer();
        let plain_yaml = plain.to_yaml().unwrap();
        assert!(!plain_yaml.contains("docusign_envelope_id"));
        assert!(!plain_yaml.contains("xero_invoice_id"));
        assert!(DocumentPointer::from_yaml(&plain_yaml)
            .unwrap()
            .validate()
            .is_ok());

        plain.kind = "onboarding".into();
        plain.docusign_envelope_id = Some(Uuid::now_v7());
        plain.xero_invoice_id = Some(Uuid::now_v7());
        assert!(plain.validate().is_ok());
        let yaml = plain.to_yaml().unwrap();
        assert!(yaml.contains("docusign_envelope_id"));
        assert!(yaml.contains("xero_invoice_id"));
        assert_eq!(DocumentPointer::from_yaml(&yaml).unwrap(), plain);
    }

    /// LAW-189: `folder:` is optional, absent from a pointer that names
    /// none, round-trips one of the five names, and refuses any other.
    #[test]
    fn folder_is_optional_round_trips_and_defaults_from_kind() {
        let mut plain = pointer();
        assert!(!plain.to_yaml().unwrap().contains("folder"));
        assert_eq!(plain.matter_folder(), MatterFolder::WorkProduct);

        plain.folder = Some(MatterFolder::Discovery);
        let yaml = plain.to_yaml().unwrap();
        assert!(yaml.contains("folder: Discovery\n"), "{yaml}");
        assert_eq!(DocumentPointer::from_yaml(&yaml).unwrap(), plain);

        let invalid = yaml.replace("folder: Discovery", "folder: Discoveries");
        assert!(DocumentPointer::from_yaml(&invalid).is_err());

        for (kind, folder) in [
            ("exhibit", MatterFolder::ClientProduction),
            ("letter", MatterFolder::CorrespondenceServed),
            ("pleading", MatterFolder::DocketFilings),
            ("filing", MatterFolder::DocketFilings),
            ("order", MatterFolder::DocketFilings),
            ("memo", MatterFolder::WorkProduct),
            ("transcript", MatterFolder::WorkProduct),
            ("unclassified", MatterFolder::WorkProduct),
        ] {
            assert_eq!(MatterFolder::for_kind(kind), folder, "{kind}");
        }
        for folder in MatterFolder::ALL {
            assert_eq!(MatterFolder::parse(folder.as_str()), Some(folder));
        }
    }

    /// A hand-written `signing` block parses, round-trips, and stays
    /// optional; a method outside the closed list or a malformed date fails
    /// to parse.
    #[test]
    fn signing_is_optional_and_round_trips() {
        let base = "kind: agreement\n\
                    visibility: internal\n\
                    current_version:\n  \
                      version: 1\n  \
                      asset_id: 018f3b1a-0000-7000-8000-000000000000\n  \
                      created_at: 2026-09-05T12:00:00Z\n  \
                      sha256: 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n  \
                      size_bytes: 42\n";
        assert_eq!(DocumentPointer::from_yaml(base).unwrap().signing, None);

        let signed = format!("{base}signing:\n  method: wet_ink\n  signed_on: 2026-09-04\n");
        let pointer = DocumentPointer::from_yaml(&signed).unwrap();
        assert_eq!(
            pointer.signing,
            Some(crate::documents::Signing {
                method: crate::documents::SigningMethod::WetInk,
                signed_on: chrono::NaiveDate::from_ymd_opt(2026, 9, 4).unwrap(),
            })
        );
        let yaml = pointer.to_yaml().unwrap();
        assert!(yaml.contains("method: wet_ink"), "{yaml}");
        assert_eq!(DocumentPointer::from_yaml(&yaml).unwrap(), pointer);

        for bad in [
            "signing:\n  method: notarized\n  signed_on: 2026-09-04\n",
            "signing:\n  method: docusign\n  signed_on: 09/04/2026\n",
            "signing:\n  method: docusign\n",
            "signing:\n  method: docusign\n  signed_on: 2026-09-04\n  signer: someone\n",
        ] {
            assert!(
                DocumentPointer::from_yaml(&format!("{base}{bad}")).is_err(),
                "{bad}"
            );
        }
    }

    /// A hand-written `notarization` list parses, round-trips, and stays
    /// optional. A notary's name, an unknown method, a bad state code, or a
    /// commission that lapsed before the acknowledgment is refused.
    #[test]
    fn notarization_is_an_optional_list_that_round_trips() {
        let base = "kind: will\n\
                    visibility: internal\n\
                    current_version:\n  \
                      version: 1\n  \
                      asset_id: 018f3b1a-0000-7000-8000-000000000000\n  \
                      created_at: 2026-09-05T12:00:00Z\n  \
                      sha256: 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n  \
                      size_bytes: 42\n";
        let plain = DocumentPointer::from_yaml(base).unwrap();
        assert!(plain.notarization.is_empty());
        assert!(!plain.to_yaml().unwrap().contains("notarization"));

        let entry = |method: &str, state: &str, expires: &str| {
            format!(
                "- method: {method}\n  acknowledged_on: 2026-09-04\n  state: {state}\n  \
                 county: Clark\n  commission_expires: {expires}\n"
            )
        };
        let two = format!(
            "{base}notarization:\n{}{}",
            entry("in_person", "NV", "2029-01-31"),
            entry("remote_online", "CA", "2027-06-30"),
        );
        let pointer = DocumentPointer::from_yaml(&two).unwrap();
        assert_eq!(pointer.notarization.len(), 2);
        assert_eq!(
            pointer.notarization[1].method,
            crate::documents::NotarizationMethod::RemoteOnline
        );
        assert_eq!(
            DocumentPointer::from_yaml(&pointer.to_yaml().unwrap()).unwrap(),
            pointer
        );

        for bad in [
            entry("by_mail", "NV", "2029-01-31"),
            entry("in_person", "Nevada", "2029-01-31"),
            entry("in_person", "nv", "2029-01-31"),
            entry("in_person", "NV", "2026-09-03"),
            format!(
                "{}  notary_name: Someone\n",
                entry("in_person", "NV", "2029-01-31")
            ),
            format!(
                "{}  commission_number: \"12345\"\n",
                entry("in_person", "NV", "2029-01-31")
            ),
        ] {
            assert!(
                DocumentPointer::from_yaml(&format!("{base}notarization:\n{bad}")).is_err(),
                "{bad}"
            );
        }
    }

    /// A hand-written `service` list parses, round-trips, and stays
    /// optional; an unknown method, a malformed date, or a recipient id
    /// (the pointer is names-only) fails to parse.
    #[test]
    fn service_is_an_optional_list_that_round_trips() {
        let base = "kind: pleading\n\
                    visibility: internal\n\
                    current_version:\n  \
                      version: 1\n  \
                      asset_id: 018f3b1a-0000-7000-8000-000000000000\n  \
                      created_at: 2026-09-05T12:00:00Z\n  \
                      sha256: 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n  \
                      size_bytes: 42\n";
        let plain = DocumentPointer::from_yaml(base).unwrap();
        assert!(plain.service.is_empty());
        assert!(!plain.to_yaml().unwrap().contains("service"));

        let entry = |method: &str, served_on: &str| {
            format!(
                "- recipient: Example Counsel\n  counsel_for: Defendant Example Co.\n  \
                 method: {method}\n  served_on: {served_on}\n  \
                 address: 200 Sample Street\n  declarant: Pat Sample\n"
            )
        };
        let two = format!(
            "{base}service:\n{}{}",
            entry("mail", "2026-03-05"),
            entry("e_service", "2026-03-06"),
        );
        let pointer = DocumentPointer::from_yaml(&two).unwrap();
        assert_eq!(pointer.service.len(), 2);
        assert_eq!(
            pointer.service[1].method,
            crate::project_service::ServiceMethod::EService
        );
        assert_eq!(
            DocumentPointer::from_yaml(&pointer.to_yaml().unwrap()).unwrap(),
            pointer
        );

        for bad in [
            entry("fax", "2026-03-05"),
            entry("mail", "03/05/2026"),
            format!(
                "{}  recipient_person_id: someone\n",
                entry("mail", "2026-03-05")
            ),
        ] {
            assert!(
                DocumentPointer::from_yaml(&format!("{base}service:\n{bad}")).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn an_unknown_pointer_key_is_rejected() {
        let yaml = "kind: invoice\n\
                     visibility: internal\n\
                     current_version:\n  \
                       version: 1\n  \
                       asset_id: 018f3b1a-0000-7000-8000-000000000000\n  \
                       created_at: 2026-09-05T12:00:00Z\n  \
                       sha256: 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n  \
                       size_bytes: 42\n\
                     xero_invoic_id: 018f3b1a-0000-7000-8000-000000000000\n";
        let error = DocumentPointer::from_yaml(yaml).unwrap_err();
        assert!(
            error.to_string().contains("unknown field"),
            "a typo'd key must fail to parse, not silently vanish: {error}"
        );
    }
}
