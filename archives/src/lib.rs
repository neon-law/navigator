//! PostgreSQL exports and recovery using Parquet and Iceberg on object storage.
//! Navigator and Forgejo have separate watermarks and a paired completion marker ([`coverage`]).
//!
//! Iceberg metadata authorship (`metadata/v<n>.metadata.json` +
//! manifest Avro over the `<lane>/<table>/metadata/` prefix, `lane` being
//! `application` for a store table or `telemetry` for an `OTel` table) lives
//! in [`iceberg`] (`author_snapshot`), built on the `iceberg` crate via an
//! in-memory `FileIO` so the bytes still persist through
//! `cloud::StorageService`.

pub mod coverage;
pub mod drift;
pub mod generic;
pub mod iceberg;
pub mod parquet_io;
pub mod postgres;
pub mod sent_email_export;
pub mod snapshot;
/// Test doubles for the crate's storage interactions. `pub` so the
/// integration drills (`archives/tests/`) share the exact double the
/// unit tests use.
#[doc(hidden)]
pub mod test_support;
pub mod write_only;

pub use self::iceberg::{
    arrow_schema_to_iceberg, author_iceberg_for_prefix, author_snapshot, AuthoredMetadata,
    DataFileSpec, PriorMetadata, PromotionRequest, SnapshotInput,
};
// The GCP billing-export reader now lives in the `billing` crate
// (`billing::gcp_cost`) so `billing-workflows` can reach it without a
// backwards dependency on `archives`. Re-exported here so `archives`'s
// own call sites — and any downstream that imported `archives::CostRow` —
// keep working unchanged.
pub use billing::gcp_cost::{BillingClient, CostReport, CostRow};
pub use drift::{
    classify, fingerprint_attempt_key, fingerprint_attempt_prefix, fingerprint_key, DriftDecision,
    StoredFingerprint,
};
pub use generic::{batch_from_rows, fingerprint};
pub use parquet_io::encode_parquet;
pub use snapshot::{
    snapshot_key, SnapshotConfig, SnapshotOutcome, APPLICATION_LANE, TELEMETRY_LANE,
};

mod write;

/// Explicit logical native recovery over a separately authorized archive reader.
pub mod recovery;
