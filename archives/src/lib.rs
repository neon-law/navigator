//! Archives: snapshot `SurrealDB` tables to Parquet files on object
//! storage, post a Slack digest of the run to firm ops, repeat at 05:55 and 17:55 UTC
//! each day, driven by a Restate workflow. Configured PostgreSQL exports cover
//! Navigator and Forgejo with separate watermarks and a paired completion
//! marker ([`coverage`]).
//!
//! Iceberg metadata authorship (`metadata/v<n>.metadata.json` +
//! manifest Avro over the `<lane>/<table>/metadata/` prefix, `lane` being
//! `application` for a store table or `telemetry` for an `OTel` table) lives
//! in [`iceberg`] (`author_snapshot`), built on the `iceberg` crate via an
//! in-memory `FileIO` so the bytes still persist through
//! `cloud::StorageService`.

pub mod coverage;
pub mod digest;
pub mod drift;
pub mod generic;
pub mod iceberg;
pub mod parquet_io;
pub mod postgres;
pub mod runner;
pub mod snapshot;
pub mod tables;
pub mod workflow;

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
pub use digest::{render_archives_slack, DiagnosticReport, SnapshotEntry};
pub use drift::{classify, fingerprint_key, DriftDecision, StoredFingerprint};
pub use generic::{batch_from_rows, fingerprint};
pub use parquet_io::encode_parquet;
pub use runner::{cost_phase, open_resources, snapshot_all, SnapshotSummary, TableFailure};
pub use snapshot::{
    snapshot_key, SnapshotConfig, SnapshotOutcome, APPLICATION_LANE, TELEMETRY_LANE,
};
pub use tables::{fetch_batch, ALL_TABLES};
pub use workflow::{ArchivesService, RunReport, RunRequest};
