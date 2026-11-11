//! Provision the GCS buckets `web` writes to:
//!
//! - `<project>-assets` — marketing photography and licensed webfonts,
//!   kept private in GCS and served anonymously only through the site's
//!   `/assets/*` application route. Standard.
//! - `<project>-documents` — **private** client documents; holds the
//!   Project-scoped content-addressed `projects/<code>/documents/<sha>`
//!   objects `web` writes. Standard, no public binding. Kept separate from
//!   `-assets` so confidential client data is never co-mingled into the
//!   public bucket.
//! - `<project>-exports` — nightly Parquet/Iceberg archives. Standard.
//! - `<project>-logs` — the log sink export and ship transcripts. Nearline,
//!   with a flat [`super::logging::LOG_RETENTION_DAYS`]-day expiry.
//! - `<project>-postgres-backups` — the PostgreSQL WAL archive and daily base
//!   backups. Standard, enforced-private, with a flat
//!   [`POSTGRES_BACKUPS_RETENTION_DAYS`]-day expiry. Only the
//!   `navigator-postgres-backup` Kubernetes service account is granted on it.
//!
//! Two more exist only when the deployment names them, each in a different
//! project from every bucket above and each carrying a **locked** retention
//! policy ([`LockedLane`], provisioned by [`ensure_locked_archive`]):
//!
//! - the **offsite** Iceberg archive — a bucket in a different GCP project
//!   than the deployment that writes it, carrying a locked
//!   [`OFFSITE_ARCHIVES_RETENTION_SECONDS`] retention policy (25 years).
//!   Enforced-private, no lifecycle rule: the locked policy is the retention
//!   decision, and nothing in the archive path may ever delete a backup.
//! - the **Git bundle** archive (`<deployment>-bundles`) — where
//!   `navigator-deploy ops forgejo bundle archive` and the nightly repository backup
//!   land, carrying a locked [`GIT_BUNDLES_RETENTION_SECONDS`] policy
//!   (7 years). Bundles live apart from the Iceberg tables because the
//!   unlocked Iceberg lane rewrites `metadata/version-hint.text` on every
//!   run, which a bucket-wide retention policy would refuse.
//!
//! Two more exist only when the deployment names them:
//!
//! - `neon-law-archives-<deployment>` — the long-term Iceberg archive of that
//!   deployment's Surreal store. Standard, no lifecycle rule: this is where
//!   long-term storage lives, and an age rule here would answer a retention
//!   question that belongs to a person.
//! - `<deployment>-telemetry` — the landing zone the `OTel` collector writes
//!   Parquet to, before the nightly lane promotes it into the archive.
//!   Standard, with a flat [`TELEMETRY_RETENTION_DAYS`]-day expiry.
//!
//! Every bucket here is private and single-region (location follows
//! `SetupConfig::region`, default `us-west4`), with uniform bucket level
//! access. Storage class is STANDARD except NEARLINE on logs, which are read
//! rarely and mostly written. Archives, telemetry, applications, and PostgreSQL
//! backups additionally enforce `publicAccessPrevention`; the four original
//! kinds inherit the project's
//! setting, because the assets lane's public-read path depends on it. The
//! `-source` (git bundles) bucket is created out-of-band; see
//! `cloud/README.md`.
//!
//! ## Idempotency
//!
//! Re-running `setup` against a project that already has these
//! buckets must succeed. We POST `storage.buckets.insert`
//! unconditionally and treat HTTP **409 Conflict** as success —
//! that's the response GCS returns when a bucket with the same name
//! already exists in the same project. Anything else outside the
//! 2xx range bubbles up as an error.

use serde::Serialize;
use url::Url;

use super::client::{GcpClient, GcpService, Mode};
use super::error::{SetupError, SetupResult};
use super::workload_identity::classify;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BucketKind {
    Assets,
    Documents,
    Exports,
    Logs,
    PostgresBackups,
    Applications,
    Archives,
    Telemetry,
    ArchivedProjects,
    OffsiteArchives,
    GitBundles,
}

impl BucketKind {
    #[must_use]
    pub const fn storage_class(self) -> &'static str {
        match self {
            // Archives is STANDARD, not NEARLINE: the nightly lane reads the
            // prior `v<N>.metadata.json` on every run to chain the snapshot
            // log, so a retrieval-priced class would bill a read per table per
            // night for data that is written just as often.
            // Applications is STANDARD: a Project portal's bundle is read on
            // every client visit, so a retrieval-priced class would bill each
            // asset fetch.
            Self::Assets
            | Self::Documents
            | Self::Exports
            | Self::PostgresBackups
            | Self::Applications
            | Self::Archives
            | Self::ArchivedProjects
            | Self::OffsiteArchives
            | Self::GitBundles
            | Self::Telemetry => "STANDARD",
            Self::Logs => "NEARLINE",
        }
    }

    /// How long objects live before the bucket's own lifecycle rule deletes
    /// them. `None` means the bucket carries no rule at all.
    ///
    /// Archives is where long-term storage lives, and retention there is a
    /// separate, deliberate decision — giving it an age rule here would answer
    /// that question by accident. Logs and PostgreSQL backups both expire, on
    /// windows set independently: see `super::logging` for why.
    #[must_use]
    pub const fn retention_days(self) -> Option<u32> {
        match self {
            Self::Telemetry => Some(TELEMETRY_RETENTION_DAYS),
            Self::Logs => Some(super::logging::LOG_RETENTION_DAYS),
            Self::Applications => Some(APPLICATIONS_RETENTION_DAYS),
            Self::PostgresBackups => Some(POSTGRES_BACKUPS_RETENTION_DAYS),
            _ => None,
        }
    }

    /// `publicAccessPrevention` for this kind, or `None` to inherit the
    /// project's setting.
    #[must_use]
    pub const fn public_access_prevention(self) -> Option<&'static str> {
        match self {
            // Applications is enforced-private like archives and telemetry:
            // its bundles are streamed through Axum, never fetched by a
            // browser directly, so no anonymous grant is ever wanted here.
            // PostgreSQL backups hold the whole database; nothing anonymous may
            // ever read them.
            // Project Recovery Archives are internal only: restores read
            // them through the server, never a browser.
            Self::Applications
            | Self::Archives
            | Self::ArchivedProjects
            | Self::OffsiteArchives
            | Self::GitBundles
            | Self::Telemetry
            | Self::PostgresBackups => Some("enforced"),
            _ => None,
        }
    }

    /// Classify a bucket by its name. Returns `Assets` by default for
    /// unrecognized names — the caller is the source of truth on what it's
    /// creating. Every other kind is matched explicitly so none is silently
    /// treated as `Assets`.
    ///
    /// Archives is matched on a *prefix*, not a suffix: its buckets are named
    /// `neon-law-archives-<deployment>` rather than `<deployment>-archives`, so a
    /// suffix test would classify `neon-law-archives-prod` as `Assets`
    /// and create it with no lifecycle distinction from a public bucket.
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        if name.ends_with(super::LOGS_BUCKET_SUFFIX) {
            Self::Logs
        } else if name.ends_with(super::DOCUMENTS_BUCKET_SUFFIX) {
            Self::Documents
        } else if name.ends_with(super::EXPORTS_BUCKET_SUFFIX) {
            Self::Exports
        } else if name.ends_with(super::POSTGRES_BACKUPS_BUCKET_SUFFIX) {
            Self::PostgresBackups
        } else if name.ends_with(super::APPLICATIONS_BUCKET_SUFFIX) {
            Self::Applications
        } else if name.ends_with(super::TELEMETRY_BUCKET_SUFFIX) {
            Self::Telemetry
        } else if name.ends_with(super::ARCHIVED_PROJECTS_BUCKET_SUFFIX) {
            Self::ArchivedProjects
        } else if name.ends_with(super::OFFSITE_ARCHIVES_BUCKET_SUFFIX) {
            Self::OffsiteArchives
        } else if name.ends_with(super::GIT_BUNDLES_BUCKET_SUFFIX) {
            Self::GitBundles
        } else if name.starts_with(super::ARCHIVES_BUCKET_PREFIX) {
            Self::Archives
        } else {
            Self::Assets
        }
    }
}

/// Objects in a telemetry landing bucket are deleted at 90 days.
///
/// The landing zone is not storage: the nightly lane promotes each day's
/// Parquet into that deployment's archive bucket, and the archive is what
/// keeps it. Ninety days is the window in which a promotion that failed can
/// still be re-run from the original bytes.
pub const TELEMETRY_RETENTION_DAYS: u32 = 90;

/// Objects in a PostgreSQL backups bucket are deleted at 30 days.
///
/// The bucket holds daily base backups plus the continuous WAL archive. An age
/// rule keeps every base backup younger than thirty days and all the WAL after
/// the oldest of them, which satisfies `docs/postgres-kubernetes.md`: keep at
/// least two base backups and all WAL newer than the oldest.
pub const POSTGRES_BACKUPS_RETENTION_DAYS: u32 = 30;

/// Objects in a Project-applications bucket are deleted at ten years.
///
/// The bucket holds client-facing engagement records, so this expiry bounds the
/// accumulation of superseded builds — it is not a recycling window for live
/// ones. Ten years outlives any engagement a portal documents. A thirty-day
/// window did not: it made deletion of a *published* portal the normal outcome
/// of publishing once and then going quiet.
///
/// **A publish must still overwrite every live object unconditionally.** Each
/// publish rewrites `index.html` and every current hashed asset, refreshing
/// their `updateTime`, so the age rule can only ever reach an *orphaned* asset
/// from a superseded build — never a live one. An "optimization" that skips
/// unchanged objects — `gcloud storage rsync` is exactly that — lets a live
/// asset age out and be deleted from under a served portal, and the breakage
/// surfaces long after the change that caused it. Ten years makes that slow
/// rather than imminent; it does not make it safe. Nothing publishes into the
/// bucket any more: Portal publishing is retired.
pub const APPLICATIONS_RETENTION_DAYS: u32 = 3650;

/// Retention on the offsite Iceberg archive: 25 years, in seconds — the unit
/// GCS retention policies are expressed in (25 × 365.25 days). Set at bucket
/// creation and then **locked**: a locked policy cannot be shortened, removed,
/// or replaced, by the deployment's credential, an operator, or a project
/// owner. Locking is irreversible, which is the point.
pub const OFFSITE_ARCHIVES_RETENTION_SECONDS: u64 = 788_940_000;

/// Retention on the Git bundle archive: 7 years, in seconds (7 × 365.25
/// days). Long enough to outlive any matter a repository documents; a locked
/// period can later be lengthened but never shortened, so the shorter of the
/// two defensible windows is the one that gets locked first.
pub const GIT_BUNDLES_RETENTION_SECONDS: u64 = 220_903_200;

/// The two buckets provisioned with a locked retention policy, and how long
/// each locks. Both live in a project other than the deployment's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockedLane {
    /// The offsite Iceberg archive.
    OffsiteArchives,
    /// The Git bundle archive.
    GitBundles,
}

impl LockedLane {
    #[must_use]
    pub const fn kind(self) -> BucketKind {
        match self {
            Self::OffsiteArchives => BucketKind::OffsiteArchives,
            Self::GitBundles => BucketKind::GitBundles,
        }
    }

    #[must_use]
    pub const fn retention_seconds(self) -> u64 {
        match self {
            Self::OffsiteArchives => OFFSITE_ARCHIVES_RETENTION_SECONDS,
            Self::GitBundles => GIT_BUNDLES_RETENTION_SECONDS,
        }
    }

    /// How the lane is named in stage lines and errors.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::OffsiteArchives => "offsite archive",
            Self::GitBundles => "Git bundle archive",
        }
    }
}

/// The exact roles the deployment's workflows-service identity holds on a
/// locked archive bucket, and nothing else: create, read, and the bucket
/// metadata read the retention probe needs
/// ([`super::workload_identity::ARCHIVE_PROBE_ROLE`]), with no delete,
/// overwrite, or administration anywhere in the grant. The writer path in
/// `archives/` never deletes — but even if code changed, IAM has no delete
/// permission to grant, and GCS's locked retention rejects deletion
/// regardless.
const LOCKED_ARCHIVE_ROLES: &[&str] = &[
    "roles/storage.objectCreator",
    "roles/storage.objectViewer",
    super::workload_identity::ARCHIVE_PROBE_ROLE,
];

#[derive(Serialize)]
struct CreateBucketBody<'a> {
    name: &'a str,
    location: &'a str,
    #[serde(rename = "storageClass")]
    storage_class: &'a str,
    #[serde(rename = "iamConfiguration")]
    iam_configuration: IamConfig,
    /// Set only by a locked lane: a retention policy is fixed at
    /// creation there and locked once verified. Omitted everywhere else —
    /// no other bucket kind carries one, and inserting a policy a lane
    /// never asked for would be unrecoverable on exactly the buckets that
    /// overwrite freely.
    #[serde(rename = "retentionPolicy", skip_serializing_if = "Option::is_none")]
    retention_policy: Option<RetentionPolicyBody>,
}

#[derive(Serialize)]
struct RetentionPolicyBody {
    /// GCS expresses the retention period as a count of seconds, serialized
    /// as a JSON string.
    #[serde(rename = "retentionPeriod")]
    retention_period: String,
}

#[derive(Serialize)]
struct IamConfig {
    #[serde(rename = "uniformBucketLevelAccess")]
    uniform_bucket_level_access: UniformAccess,
    /// Omitted for the four original kinds, which inherit the project's
    /// setting. Set to `enforced` for archives and telemetry: neither is ever
    /// served to a browser, and `ensure_public_read` — the one path that wants
    /// an anonymous grant — runs against website buckets, which this call does
    /// not create.
    #[serde(
        rename = "publicAccessPrevention",
        skip_serializing_if = "Option::is_none"
    )]
    public_access_prevention: Option<&'static str>,
}

#[derive(Serialize)]
struct UniformAccess {
    enabled: bool,
}

/// Outcome of a single `ensure_bucket` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnsureOutcome {
    /// Bucket did not exist; we created it.
    Created,
    /// Bucket already existed (HTTP 409 from `buckets.insert`).
    AlreadyExists,
}

/// Idempotently ensure a bucket exists in `project_id` at `location`.
pub async fn ensure_bucket(
    client: &GcpClient,
    project_id: &str,
    name: &str,
    location: &str,
) -> SetupResult<EnsureOutcome> {
    let kind = BucketKind::from_name(name);
    let body = CreateBucketBody {
        name,
        location,
        storage_class: kind.storage_class(),
        iam_configuration: IamConfig {
            uniform_bucket_level_access: UniformAccess { enabled: true },
            public_access_prevention: kind.public_access_prevention(),
        },
        retention_policy: None,
    };
    let body_json = serde_json::to_value(&body).map_err(|source| SetupError::Json {
        what: "create bucket request body",
        source,
    })?;
    let resp = client
        .post_json(
            GcpService::Storage,
            &format!("/storage/v1/b?project={project_id}"),
            &body_json,
        )
        .await?;
    let status = resp.status_u16();
    match status {
        200..=299 => Ok(EnsureOutcome::Created),
        409 => Ok(EnsureOutcome::AlreadyExists),
        other => Err(SetupError::BadStatus {
            operation: format!("create bucket {name}"),
            status: other,
            body: resp.into_text(),
        }),
    }
}

/// Retain a canonical browser-origin CORS rule on the assets bucket.
///
/// The normal website path is same-origin `/assets/*`, so this is not an
/// anonymous-access mechanism. It keeps the bucket portable to an authorized
/// edge proxy that forwards browser CORS semantics later.
pub async fn ensure_assets_cors(
    client: &GcpClient,
    name: &str,
    public_base_url: &str,
) -> SetupResult<()> {
    let font_rule = assets_font_cors_rule(public_base_url)?;
    let response = client
        .get(GcpService::Storage, &format!("/storage/v1/b/{name}"))
        .await?;
    let status = response.status_u16();
    if !(200..=299).contains(&status) {
        return Err(SetupError::BadStatus {
            operation: format!("read CORS for assets bucket {name}"),
            status,
            body: response.into_text(),
        });
    }

    let metadata: serde_json::Value =
        serde_json::from_str(&response.into_text()).map_err(|source| SetupError::Json {
            what: "assets bucket metadata",
            source,
        })?;
    let mut cors = metadata
        .get("cors")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let original_rule_count = cors.len();
    let legacy_font_rule = legacy_assets_font_cors_rule();
    cors.retain(|rule| rule != &legacy_font_rule);
    if cors.contains(&font_rule) && cors.len() == original_rule_count {
        return Ok(());
    }
    if !cors.contains(&font_rule) {
        cors.push(font_rule);
    }

    let body = serde_json::json!({ "cors": cors });
    let resp = client
        .patch_json(GcpService::Storage, &format!("/storage/v1/b/{name}"), &body)
        .await?;
    match resp.status_u16() {
        200..=299 => Ok(()),
        other => Err(SetupError::BadStatus {
            operation: format!("set CORS for assets bucket {name}"),
            status: other,
            body: resp.into_text(),
        }),
    }
}

/// Idempotently set the object-expiry lifecycle rule this bucket's kind calls
/// for, and remove any rule from a kind that calls for none.
///
/// One unconditional PATCH rather than read-then-compare: the desired rule set
/// is a pure function of the kind, so writing it is already idempotent, and a
/// read first would let a rule drift back in between the two calls.
///
/// **The rule is unscoped by prefix, deliberately.** A telemetry bucket holds
/// nothing but telemetry, so an age condition needs no `matchesPrefix` to stay
/// off anything else — which is exactly what a prefix-scoped rule in a shared
/// bucket has to get right, and silently fails to do when a prefix moves.
pub async fn ensure_lifecycle(client: &GcpClient, name: &str) -> SetupResult<()> {
    let rules = match BucketKind::from_name(name).retention_days() {
        Some(days) => serde_json::json!([{
            "action": { "type": "Delete" },
            "condition": { "age": days },
        }]),
        None => serde_json::json!([]),
    };
    let body = serde_json::json!({ "lifecycle": { "rule": rules } });
    let resp = client
        .patch_json(GcpService::Storage, &format!("/storage/v1/b/{name}"), &body)
        .await?;
    match resp.status_u16() {
        200..=299 => Ok(()),
        other => Err(SetupError::BadStatus {
            operation: format!("set lifecycle for bucket {name}"),
            status: other,
            body: resp.into_text(),
        }),
    }
}

/// Ensure a locked archive bucket exists in `project_id` — the **archive**
/// project, never the deployment's — with the lane's locked retention
/// policy, public access prevention enforced, uniform bucket-level access,
/// and no lifecycle rule.
///
/// Locking is irreversible: a locked policy cannot be shortened, removed, or
/// replaced, and a mistaken object lives out the full period. The sequence
/// is therefore create → verify → lock — the policy is fixed at creation,
/// verified against the live bucket, and only then locked. A bucket that
/// already exists with anything but the exact intended policy is refused,
/// never silently locked and never "fixed" by the provisioner: converging an
/// archive bucket by overwriting it is the one write this pipeline must not
/// perform.
///
/// Idempotent in the create (HTTP 409 = already exists) and in the lock (an
/// already-locked bucket with the verified policy skips the lock call). In
/// [`Mode::DryRun`] the synthesized responses carry no policy to verify and
/// no metageneration to lock against, so the plan records the create, the
/// lifecycle clear, and the lock intent without the verification sequence.
pub async fn ensure_locked_archive(
    client: &GcpClient,
    project_id: &str,
    name: &str,
    location: &str,
    lane: LockedLane,
) -> SetupResult<EnsureOutcome> {
    let outcome = create_locked_bucket(client, project_id, name, location, lane).await?;

    if client.mode() == Mode::DryRun {
        clear_locked_lifecycle(client, name, lane).await?;
        client
            .post_json(
                GcpService::Storage,
                &format!("/storage/v1/b/{name}/lockRetentionPolicy"),
                &serde_json::json!({}),
            )
            .await?;
        return Ok(outcome);
    }

    // Ownership before any mutation: bucket names are global, so the 409 on
    // create only means the name is taken. Every later call targets the name
    // alone — confirm the bucket actually lives in the archive project before
    // clearing its lifecycle or locking its policy.
    verify_locked_archive_project(client, name, project_id, lane).await?;
    clear_locked_lifecycle(client, name, lane).await?;

    let metadata = get_bucket_metadata(client, name, lane).await?;
    verify_locked_archive_policy(lane, name, &metadata)?;

    if metadata.pointer("/retentionPolicy/isLocked") == Some(&serde_json::Value::Bool(true)) {
        return Ok(outcome);
    }
    let metageneration = metadata
        .pointer("/metageneration")
        .and_then(serde_json::Value::as_str)
        .expect("invariant: get_bucket_metadata returns a JSON object; a missing or non-string metageneration is a malformed bucket resource");
    lock_locked_policy(client, name, metageneration, lane).await?;
    Ok(outcome)
}

/// Create the locked bucket with its retention policy already in place, or
/// report that it exists. A pre-existing bucket is never rewritten — the
/// verify step below judges the live resource instead.
async fn create_locked_bucket(
    client: &GcpClient,
    project_id: &str,
    name: &str,
    location: &str,
    lane: LockedLane,
) -> SetupResult<EnsureOutcome> {
    let kind = lane.kind();
    let body = CreateBucketBody {
        name,
        location,
        storage_class: kind.storage_class(),
        iam_configuration: IamConfig {
            uniform_bucket_level_access: UniformAccess { enabled: true },
            public_access_prevention: kind.public_access_prevention(),
        },
        retention_policy: Some(RetentionPolicyBody {
            retention_period: lane.retention_seconds().to_string(),
        }),
    };
    let body_json = serde_json::to_value(&body).map_err(|source| SetupError::Json {
        what: "create locked archive bucket request body",
        source,
    })?;
    let resp = client
        .post_json(
            GcpService::Storage,
            &format!("/storage/v1/b?project={project_id}"),
            &body_json,
        )
        .await?;
    match resp.status_u16() {
        200..=299 => Ok(EnsureOutcome::Created),
        409 => Ok(EnsureOutcome::AlreadyExists),
        other => Err(SetupError::BadStatus {
            operation: format!("create {} bucket {name}", lane.label()),
            status: other,
            body: resp.into_text(),
        }),
    }
}

/// A lifecycle rule must never coexist with the locked policy: while
/// retention holds a Delete rule cannot fire, but carrying one is an
/// accident waiting for the day the policy and the rule disagree. Clear
/// unconditionally — the desired rule set is empty, so the write is
/// already idempotent.
async fn clear_locked_lifecycle(
    client: &GcpClient,
    name: &str,
    lane: LockedLane,
) -> SetupResult<()> {
    let resp = client
        .patch_json(
            GcpService::Storage,
            &format!("/storage/v1/b/{name}"),
            &serde_json::json!({ "lifecycle": { "rule": [] } }),
        )
        .await?;
    match resp.status_u16() {
        200..=299 => Ok(()),
        other => Err(SetupError::BadStatus {
            operation: format!("clear lifecycle for {} bucket {name}", lane.label()),
            status: other,
            body: resp.into_text(),
        }),
    }
}

/// Lock the verified policy against the metageneration read from the live
/// bucket, so a policy modified between verify and lock fails instead of
/// locking the wrong terms. The lock is irreversible; the response must
/// report the policy as locked.
async fn lock_locked_policy(
    client: &GcpClient,
    name: &str,
    metageneration: &str,
    lane: LockedLane,
) -> SetupResult<()> {
    let resp = client
        .post_json(
            GcpService::Storage,
            &format!(
                "/storage/v1/b/{name}/lockRetentionPolicy?ifMetagenerationMatch={metageneration}"
            ),
            &serde_json::json!({}),
        )
        .await?;
    match resp.status_u16() {
        200..=299 => {
            let locked: serde_json::Value =
                serde_json::from_str(&resp.into_text()).map_err(|source| SetupError::Json {
                    what: "lock retention policy response",
                    source,
                })?;
            if locked.pointer("/retentionPolicy/isLocked") != Some(&serde_json::Value::Bool(true)) {
                return Err(SetupError::Malformed(
                    "lock retention policy response does not report the policy as locked",
                ));
            }
            Ok(())
        }
        other => Err(SetupError::BadStatus {
            operation: format!("lock retention policy on {} bucket {name}", lane.label()),
            status: other,
            body: resp.into_text(),
        }),
    }
}

/// Read the live bucket resource for the verify-then-lock sequence.
async fn get_bucket_metadata(
    client: &GcpClient,
    name: &str,
    lane: LockedLane,
) -> SetupResult<serde_json::Value> {
    let resp = client
        .get(GcpService::Storage, &format!("/storage/v1/b/{name}"))
        .await?;
    let status = resp.status_u16();
    if !(200..=299).contains(&status) {
        return Err(SetupError::BadStatus {
            operation: format!("read {} bucket {name}", lane.label()),
            status,
            body: resp.into_text(),
        });
    }
    serde_json::from_str(&resp.into_text()).map_err(|source| SetupError::Json {
        what: "locked archive bucket metadata",
        source,
    })
}

/// Verify the live bucket carries exactly the policy the lock is about to
/// make permanent: the intended retention period, public access prevention
/// enforced, uniform bucket-level access enabled, and an empty lifecycle —
/// the desired rule set is empty, so a nonempty snapshot means another tool
/// wrote a rule after the clear. Any mismatch refuses the run — the lock is
/// irreversible, so the provisioner must never lock a policy it has not just
/// asserted.
fn verify_locked_archive_policy(
    lane: LockedLane,
    name: &str,
    metadata: &serde_json::Value,
) -> SetupResult<()> {
    let expected_period = lane.retention_seconds().to_string();
    let operation = || format!("verify {} bucket {name}", lane.label());
    let period = metadata
        .pointer("/retentionPolicy/retentionPeriod")
        .and_then(serde_json::Value::as_str);
    if period != Some(expected_period.as_str()) {
        return Err(SetupError::AmbiguousLiveState {
            operation: operation(),
            detail: format!(
                "retention period is {period:?}, expected {expected_period} seconds; \
                 refusing to lock a policy this pipeline did not write"
            ),
        });
    }
    let pap = metadata.pointer("/iamConfiguration/publicAccessPrevention");
    if pap != Some(&serde_json::Value::String("enforced".into())) {
        return Err(SetupError::AmbiguousLiveState {
            operation: operation(),
            detail: format!(
                "public access prevention is {pap:?}, expected \"enforced\"; \
                 refusing to lock a policy this pipeline did not write"
            ),
        });
    }
    let ubla = metadata.pointer("/iamConfiguration/uniformBucketLevelAccess/enabled");
    if ubla != Some(&serde_json::Value::Bool(true)) {
        return Err(SetupError::AmbiguousLiveState {
            operation: operation(),
            detail: format!(
                "uniform bucket-level access is {ubla:?}, expected enabled; \
                 refusing to lock a policy this pipeline did not write"
            ),
        });
    }
    let lifecycle_rules = metadata
        .pointer("/lifecycle/rule")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    if lifecycle_rules != 0 {
        return Err(SetupError::AmbiguousLiveState {
            operation: operation(),
            detail: format!(
                "the bucket carries {lifecycle_rules} lifecycle rule(s), expected an \
                 empty rule set; refusing to lock a policy this pipeline did not write"
            ),
        });
    }
    Ok(())
}

/// Confirm the locked bucket actually lives in the archive project. Bucket
/// names are global, so a 409 on create only means the name is taken — it
/// says nothing about which project owns the bucket — and every later call
/// targets the name alone. Listing the archive project's buckets settles
/// ownership with the same Storage API surface everything else here uses.
async fn verify_locked_archive_project(
    client: &GcpClient,
    name: &str,
    archive_project: &str,
    lane: LockedLane,
) -> SetupResult<()> {
    let resp = client
        .get(
            GcpService::Storage,
            &format!("/storage/v1/b?project={archive_project}&prefix={name}"),
        )
        .await?;
    let status = resp.status_u16();
    if !(200..=299).contains(&status) {
        return Err(SetupError::BadStatus {
            operation: format!("list archive project buckets for {name}"),
            status,
            body: resp.into_text(),
        });
    }
    let listed: serde_json::Value =
        serde_json::from_str(&resp.into_text()).map_err(|source| SetupError::Json {
            what: "archive project bucket listing",
            source,
        })?;
    let owned = listed
        .pointer("/items")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|items| {
            items
                .iter()
                .any(|bucket| bucket.get("name").and_then(serde_json::Value::as_str) == Some(name))
        });
    if owned {
        return Ok(());
    }
    Err(SetupError::AmbiguousLiveState {
        operation: format!("verify {} bucket {name}", lane.label()),
        detail: format!(
            "bucket {name} does not appear in project {archive_project}; a bucket name \
             is global, so refusing to clear or lock a bucket that lives in another project"
        ),
    })
}

/// Grant the deployment's workflows-service identity — the pod service
/// account behind the archive writer — exactly [`LOCKED_ARCHIVE_ROLES`]
/// on a locked bucket: create and read, with no administration and no
/// delete permission anywhere in the grant. The member is named cross-project
/// on the archive bucket itself; `gcloud` merges additive bindings, so
/// re-running setup leaves an existing grant alone.
pub async fn ensure_locked_archive_writer(
    client: &GcpClient,
    deployment_project_id: &str,
    account_id: &str,
    bucket: &str,
) -> SetupResult<()> {
    let gsa = format!("{account_id}@{deployment_project_id}.iam.gserviceaccount.com");
    for role in LOCKED_ARCHIVE_ROLES {
        classify(
            client
                .shell_out(
                    "gcloud",
                    &[
                        "storage",
                        "buckets",
                        "add-iam-policy-binding",
                        &format!("gs://{bucket}"),
                        "--member",
                        &format!("serviceAccount:{gsa}"),
                        "--role",
                        role,
                    ],
                )
                .await?,
            "bind locked archive writer role",
        )?;
    }
    Ok(())
}

/// Reject a `NAV_BASE_URL` that cannot become a CORS origin *before* the
/// pipeline mutates anything. [`ensure_assets_cors`] runs at step 4a, after
/// services, networking, SQL, and the bucket already exist, so parsing only
/// there would leave a malformed value to fail against a half-provisioned
/// project. `run` calls this first so the command refuses upfront instead.
pub fn validate_public_base_url(public_base_url: &str) -> SetupResult<()> {
    assets_font_cors_rule(public_base_url).map(|_| ())
}

fn assets_font_cors_rule(public_base_url: &str) -> SetupResult<serde_json::Value> {
    let url = Url::parse(public_base_url).map_err(|_| {
        SetupError::InvalidPublicBaseUrl(
            "must be an absolute HTTPS URL with no path, query, or fragment".into(),
        )
    })?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(SetupError::InvalidPublicBaseUrl(
            "must be an absolute HTTPS URL with no path, query, or fragment".into(),
        ));
    }

    Ok(serde_json::json!({
        "origin": [url.origin().ascii_serialization()],
        "method": ["GET", "HEAD"],
        "responseHeader": ["Content-Type"],
        "maxAgeSeconds": 3600
    }))
}

fn legacy_assets_font_cors_rule() -> serde_json::Value {
    serde_json::json!({
        "origin": ["*"],
        "method": ["GET", "HEAD"],
        "responseHeader": ["Content-Type"],
        "maxAgeSeconds": 3600
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use wiremock::matchers::{body_partial_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::super::client::{GcpClient, GcpService, StaticToken};
    use super::{
        assets_font_cors_rule, ensure_assets_cors, ensure_bucket, ensure_lifecycle,
        ensure_locked_archive, ensure_locked_archive_writer, legacy_assets_font_cors_rule,
        BucketKind, CreateBucketBody, EnsureOutcome, IamConfig, LockedLane, UniformAccess,
        APPLICATIONS_RETENTION_DAYS, GIT_BUNDLES_RETENTION_SECONDS,
        OFFSITE_ARCHIVES_RETENTION_SECONDS, POSTGRES_BACKUPS_RETENTION_DAYS,
        TELEMETRY_RETENTION_DAYS,
    };

    const PUBLIC_BASE_URL: &str = "https://www.neonlaw.com";

    fn client_pointed_at(server: &MockServer) -> GcpClient {
        GcpClient::new(Arc::new(StaticToken("t".into())))
            .with_base_url(GcpService::Storage, server.uri())
    }

    #[tokio::test]
    async fn creates_bucket_when_post_returns_2xx() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/storage/v1/b"))
            .and(query_param("project", "proj"))
            .and(body_partial_json(json!({
                "name": "proj-assets",
                "location": "us-west4",
                "storageClass": "STANDARD",
                "iamConfiguration": {
                    "uniformBucketLevelAccess": { "enabled": true }
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"name": "proj-assets"})))
            .expect(1)
            .mount(&server)
            .await;

        let client = client_pointed_at(&server);
        let outcome = ensure_bucket(&client, "proj", "proj-assets", "us-west4")
            .await
            .unwrap();
        assert_eq!(outcome, EnsureOutcome::Created);
    }

    #[test]
    fn archives_is_classified_by_prefix_not_suffix() {
        // `neon-law-archives-prod` ends in `-production`, so every
        // suffix test in `from_name` misses it. Without the prefix arm it
        // falls through to `Assets` — created with no public-access
        // prevention, and never given the archive lane's treatment.
        assert_eq!(
            BucketKind::from_name("neon-law-archives-prod"),
            BucketKind::Archives
        );
        assert_eq!(
            BucketKind::from_name("neon-law-archives-prod"),
            BucketKind::Archives
        );
        assert_eq!(
            BucketKind::from_name("neon-law-test-telemetry"),
            BucketKind::Telemetry
        );
        // The four original kinds keep classifying exactly as before.
        assert_eq!(BucketKind::from_name("proj-assets"), BucketKind::Assets);
        assert_eq!(
            BucketKind::from_name("proj-documents"),
            BucketKind::Documents
        );
        assert_eq!(BucketKind::from_name("proj-exports"), BucketKind::Exports);
        assert_eq!(BucketKind::from_name("proj-logs"), BucketKind::Logs);
        // The applications bucket is classified on its own suffix so it gets
        // the 30-day expiry and enforced-private access rather than the
        // Assets default.
        assert_eq!(
            BucketKind::from_name("neon-law-test-applications"),
            BucketKind::Applications
        );
    }

    #[test]
    fn archived_projects_is_private_and_never_expires_by_age() {
        let kind = BucketKind::from_name("neon-law-stg-archived-projects");
        assert_eq!(kind, BucketKind::ArchivedProjects);
        // Retention belongs to the governed purge, which rechecks Legal
        // Holds; a bucket age rule would bypass them.
        assert_eq!(kind.retention_days(), None);
        assert_eq!(kind.public_access_prevention(), Some("enforced"));
        assert_eq!(kind.storage_class(), "STANDARD");
    }

    #[test]
    fn telemetry_and_applications_expire_but_archives_never_does() {
        // The whole point of the archive/telemetry split: the landing zone is
        // disposable and the archive is not. An age rule on the archive would
        // silently answer a retention question nobody asked. The applications
        // bucket expires too, at ten years — long enough that the expiry only
        // ever reaches orphaned assets from superseded builds, and never the
        // live bundle of an engagement still on foot.
        assert_eq!(
            BucketKind::Telemetry.retention_days(),
            Some(TELEMETRY_RETENTION_DAYS)
        );
        assert_eq!(
            BucketKind::Applications.retention_days(),
            Some(APPLICATIONS_RETENTION_DAYS)
        );
        // Ten years, in days. A short window here deletes published client
        // portals; ENG-273 raised it from thirty days for exactly that reason.
        assert_eq!(APPLICATIONS_RETENTION_DAYS, 3650);
        // The applications bundle is streamed through Axum, never served to a
        // browser directly, so it is enforced-private like archives/telemetry.
        assert_eq!(
            BucketKind::Applications.public_access_prevention(),
            Some("enforced")
        );
        assert_eq!(BucketKind::Applications.storage_class(), "STANDARD");
        assert_eq!(BucketKind::Archives.retention_days(), None);
        for kind in [
            BucketKind::Assets,
            BucketKind::Documents,
            BucketKind::Exports,
        ] {
            assert_eq!(kind.retention_days(), None, "{kind:?} must not expire");
        }
        assert_eq!(
            BucketKind::Logs.retention_days(),
            Some(super::super::logging::LOG_RETENTION_DAYS)
        );
    }

    #[tokio::test]
    async fn archive_and_telemetry_buckets_are_created_with_public_access_prevented() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/storage/v1/b"))
            .and(query_param("project", "neon-law"))
            .and(body_partial_json(json!({
                "name": "neon-law-archives-prod",
                "storageClass": "STANDARD",
                "iamConfiguration": {
                    "uniformBucketLevelAccess": { "enabled": true },
                    "publicAccessPrevention": "enforced"
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"name": "b"})))
            .expect(1)
            .mount(&server)
            .await;

        let client = client_pointed_at(&server);
        let outcome = ensure_bucket(&client, "neon-law", "neon-law-archives-prod", "us-west4")
            .await
            .unwrap();
        assert_eq!(outcome, EnsureOutcome::Created);
    }

    #[tokio::test]
    async fn assets_bucket_create_still_omits_public_access_prevention() {
        // The four original kinds inherit the project's setting. Enforcing it
        // here would break `ensure_public_read`, which the marketing website
        // path depends on.
        let body = serde_json::to_value(&CreateBucketBody {
            name: "proj-assets",
            location: "us-west4",
            storage_class: BucketKind::Assets.storage_class(),
            iam_configuration: IamConfig {
                uniform_bucket_level_access: UniformAccess { enabled: true },
                public_access_prevention: BucketKind::Assets.public_access_prevention(),
            },
            retention_policy: None,
        })
        .unwrap();
        assert!(
            body.get("publicAccessPrevention").is_none(),
            "assets create body must not carry publicAccessPrevention: {body}"
        );
    }

    #[tokio::test]
    async fn telemetry_lifecycle_sets_a_flat_ninety_day_delete() {
        // Flat — no `matchesPrefix`. A dedicated bucket needs no prefix scoping,
        // which is precisely the thing that goes stale in a shared one.
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/storage/v1/b/neon-law-test-telemetry"))
            .and(body_partial_json(json!({
                "lifecycle": {
                    "rule": [{
                        "action": { "type": "Delete" },
                        "condition": { "age": 90 }
                    }]
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"name": "b"})))
            .expect(1)
            .mount(&server)
            .await;

        let client = client_pointed_at(&server);
        ensure_lifecycle(&client, "neon-law-test-telemetry")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn archive_lifecycle_clears_any_rule_rather_than_setting_one() {
        // Idempotent in the direction that matters: if someone attaches an age
        // rule to an archive bucket by hand, the next setup takes it back off.
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/storage/v1/b/neon-law-archives-prod"))
            .and(body_partial_json(json!({ "lifecycle": { "rule": [] } })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"name": "b"})))
            .expect(1)
            .mount(&server)
            .await;

        let client = client_pointed_at(&server);
        ensure_lifecycle(&client, "neon-law-archives-prod")
            .await
            .unwrap();
    }

    #[test]
    fn postgres_backups_are_private_standard_and_expire_at_thirty_days() {
        // Daily base backups plus WAL: a 30-day age rule always leaves more
        // than two base backups and every WAL segment after the oldest one.
        assert_eq!(
            BucketKind::from_name("neon-law-stg-postgres-backups"),
            BucketKind::PostgresBackups
        );
        assert_eq!(BucketKind::PostgresBackups.storage_class(), "STANDARD");
        assert_eq!(
            BucketKind::PostgresBackups.retention_days(),
            Some(POSTGRES_BACKUPS_RETENTION_DAYS)
        );
        assert_eq!(POSTGRES_BACKUPS_RETENTION_DAYS, 30);
        assert_eq!(
            BucketKind::PostgresBackups.public_access_prevention(),
            Some("enforced")
        );
    }

    #[tokio::test]
    async fn postgres_backups_bucket_is_created_with_public_access_prevented() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/storage/v1/b"))
            .and(query_param("project", "neon-law-stg"))
            .and(body_partial_json(json!({
                "name": "neon-law-stg-postgres-backups",
                "location": "us-west4",
                "storageClass": "STANDARD",
                "iamConfiguration": {
                    "uniformBucketLevelAccess": { "enabled": true },
                    "publicAccessPrevention": "enforced"
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"name": "b"})))
            .expect(1)
            .mount(&server)
            .await;

        let client = client_pointed_at(&server);
        let outcome = ensure_bucket(
            &client,
            "neon-law-stg",
            "neon-law-stg-postgres-backups",
            "us-west4",
        )
        .await
        .unwrap();
        assert_eq!(outcome, EnsureOutcome::Created);
    }

    #[tokio::test]
    async fn postgres_backups_lifecycle_sets_a_flat_thirty_day_delete() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/storage/v1/b/neon-law-stg-postgres-backups"))
            .and(body_partial_json(json!({
                "lifecycle": {
                    "rule": [{
                        "action": { "type": "Delete" },
                        "condition": { "age": 30 }
                    }]
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"name": "b"})))
            .expect(1)
            .mount(&server)
            .await;

        let client = client_pointed_at(&server);
        ensure_lifecycle(&client, "neon-law-stg-postgres-backups")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn treats_409_conflict_as_already_exists() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/storage/v1/b"))
            .respond_with(ResponseTemplate::new(409).set_body_json(json!({
                "error": { "code": 409, "message": "You already own this bucket." }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = client_pointed_at(&server);
        let outcome = ensure_bucket(&client, "proj", "proj-assets", "us-west4")
            .await
            .unwrap();
        assert_eq!(outcome, EnsureOutcome::AlreadyExists);
    }

    #[tokio::test]
    async fn assets_bucket_cors_allows_cross_origin_font_fetches() {
        let server = MockServer::start().await;
        let existing_rule = json!({
            "origin": ["https://operator.example.test"],
            "method": ["GET"],
            "responseHeader": ["ETag"],
            "maxAgeSeconds": 600
        });
        Mock::given(method("GET"))
            .and(path("/storage/v1/b/proj-assets"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "cors": [existing_rule] })),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/storage/v1/b/proj-assets"))
            .and(body_partial_json(json!({
                "cors": [
                    existing_rule,
                    assets_font_cors_rule(PUBLIC_BASE_URL).unwrap()
                ]
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        ensure_assets_cors(&client_pointed_at(&server), "proj-assets", PUBLIC_BASE_URL)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn assets_bucket_cors_does_not_duplicate_the_font_rule() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/storage/v1/b/proj-assets"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({ "cors": [assets_font_cors_rule(PUBLIC_BASE_URL).unwrap()] }),
            ))
            .expect(1)
            .mount(&server)
            .await;

        ensure_assets_cors(&client_pointed_at(&server), "proj-assets", PUBLIC_BASE_URL)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn assets_bucket_cors_replaces_the_legacy_wildcard_rule() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/storage/v1/b/proj-assets"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "cors": [legacy_assets_font_cors_rule()] })),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/storage/v1/b/proj-assets"))
            .and(body_partial_json(json!({
                "cors": [assets_font_cors_rule(PUBLIC_BASE_URL).unwrap()]
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        ensure_assets_cors(&client_pointed_at(&server), "proj-assets", PUBLIC_BASE_URL)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn assets_bucket_cors_surfaces_a_failed_metadata_read() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/storage/v1/b/proj-assets"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .expect(1)
            .mount(&server)
            .await;

        let err = ensure_assets_cors(&client_pointed_at(&server), "proj-assets", PUBLIC_BASE_URL)
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("read CORS for assets bucket proj-assets"),
            "{message}"
        );
        assert!(message.contains("403"), "{message}");
    }

    #[tokio::test]
    async fn assets_bucket_cors_surfaces_unparseable_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/storage/v1/b/proj-assets"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .expect(1)
            .mount(&server)
            .await;

        let err = ensure_assets_cors(&client_pointed_at(&server), "proj-assets", PUBLIC_BASE_URL)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("assets bucket metadata"), "{err}");
    }

    #[tokio::test]
    async fn assets_bucket_cors_surfaces_a_failed_patch() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/storage/v1/b/proj-assets"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/storage/v1/b/proj-assets"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .expect(1)
            .mount(&server)
            .await;

        let err = ensure_assets_cors(&client_pointed_at(&server), "proj-assets", PUBLIC_BASE_URL)
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("set CORS for assets bucket proj-assets"),
            "{message}"
        );
        assert!(message.contains("500"), "{message}");
    }

    #[test]
    fn assets_bucket_cors_uses_the_canonical_https_origin() {
        let rule = assets_font_cors_rule("https://www.neonlaw.com/").unwrap();
        assert_eq!(rule["origin"], json!(["https://www.neonlaw.com"]));
    }

    #[test]
    fn assets_bucket_cors_rejects_a_non_origin_base_url() {
        let err = assets_font_cors_rule("https://www.neonlaw.com/services")
            .expect_err("a path must not become a CORS origin");
        assert!(err.to_string().contains("NAV_BASE_URL"), "{err}");
    }

    #[tokio::test]
    async fn second_run_is_idempotent() {
        let server = MockServer::start().await;
        // First POST: bucket doesn't exist → 200.
        // Second POST: bucket exists → 409.
        // Wiremock matchers are FIFO when stacked under
        // `.up_to_n_times`, which is exactly the cadence we want
        // to model a real second run.
        Mock::given(method("POST"))
            .and(path("/storage/v1/b"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"name": "proj-assets"})))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/storage/v1/b"))
            .respond_with(ResponseTemplate::new(409))
            .mount(&server)
            .await;

        let client = client_pointed_at(&server);
        let first = ensure_bucket(&client, "proj", "proj-assets", "us-west4")
            .await
            .unwrap();
        let second = ensure_bucket(&client, "proj", "proj-assets", "us-west4")
            .await
            .unwrap();
        assert_eq!(first, EnsureOutcome::Created);
        assert_eq!(second, EnsureOutcome::AlreadyExists);
    }

    #[tokio::test]
    async fn logs_bucket_uses_nearline_storage_class() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/storage/v1/b"))
            .and(body_partial_json(json!({
                "name": "proj-logs",
                "storageClass": "NEARLINE"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"name": "proj-logs"})))
            .expect(1)
            .mount(&server)
            .await;

        let client = client_pointed_at(&server);
        ensure_bucket(&client, "proj", "proj-logs", "us-west4")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn documents_bucket_uses_standard_storage_class() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/storage/v1/b"))
            .and(body_partial_json(json!({
                "name": "proj-documents",
                "storageClass": "STANDARD",
                "iamConfiguration": {
                    "uniformBucketLevelAccess": { "enabled": true }
                }
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"name": "proj-documents"})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = client_pointed_at(&server);
        ensure_bucket(&client, "proj", "proj-documents", "us-west4")
            .await
            .unwrap();
    }

    #[test]
    fn from_name_classifies_each_suffix() {
        use super::BucketKind;
        assert_eq!(BucketKind::from_name("proj-assets"), BucketKind::Assets);
        assert_eq!(
            BucketKind::from_name("proj-documents"),
            BucketKind::Documents
        );
        assert_eq!(BucketKind::from_name("proj-exports"), BucketKind::Exports);
        assert_eq!(BucketKind::from_name("proj-logs"), BucketKind::Logs);
        // Unknown names default to Assets (STANDARD).
        assert_eq!(BucketKind::from_name("proj-whatever"), BucketKind::Assets);
    }

    #[tokio::test]
    async fn unexpected_status_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/storage/v1/b"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;
        let client = client_pointed_at(&server);
        let err = ensure_bucket(&client, "p", "x", "us-west4")
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("500"), "got {err}");
    }

    #[test]
    fn offsite_archive_is_classified_by_suffix_and_carries_no_lifecycle_rule() {
        assert_eq!(
            BucketKind::from_name("example-deployment-archives-offsite"),
            BucketKind::OffsiteArchives
        );
        assert_eq!(BucketKind::OffsiteArchives.storage_class(), "STANDARD");
        // Retention belongs to the locked policy, never to a lifecycle age
        // rule: the lock is the 25-year decision, made once and verified.
        assert_eq!(BucketKind::OffsiteArchives.retention_days(), None);
        assert_eq!(
            BucketKind::OffsiteArchives.public_access_prevention(),
            Some("enforced")
        );
        // 25 × 365.25 days in seconds. Locked GCS retention cannot be
        // shortened or removed, so this constant is permanent the moment
        // provisioning locks a bucket with it.
        assert_eq!(OFFSITE_ARCHIVES_RETENTION_SECONDS, 788_940_000);
    }

    /// ENG-1292: Git bundles get their own locked bucket, apart from the
    /// Iceberg lane whose `version-hint.text` rewrite a bucket-wide policy
    /// would refuse.
    #[test]
    fn git_bundle_archive_is_classified_by_suffix_and_locked_for_seven_years() {
        assert_eq!(
            BucketKind::from_name("example-deployment-bundles"),
            BucketKind::GitBundles
        );
        assert_eq!(BucketKind::GitBundles.storage_class(), "STANDARD");
        assert_eq!(BucketKind::GitBundles.retention_days(), None);
        assert_eq!(
            BucketKind::GitBundles.public_access_prevention(),
            Some("enforced")
        );
        // 7 × 365.25 days in seconds. A locked period can be lengthened later
        // and never shortened, so the shorter defensible window locks first.
        assert_eq!(GIT_BUNDLES_RETENTION_SECONDS, 220_903_200);
        assert_eq!(
            LockedLane::GitBundles.retention_seconds(),
            GIT_BUNDLES_RETENTION_SECONDS
        );
        assert_eq!(LockedLane::GitBundles.kind(), BucketKind::GitBundles);
    }

    /// The dry-run plan for the bundle lane: created with its retention
    /// policy, lifecycle cleared, lock intent recorded — the shape a locked
    /// bucket the writer cannot delete from takes before any object lands.
    #[tokio::test]
    async fn git_bundle_archive_plan_creates_with_retention_and_locks() {
        let client = GcpClient::new(Arc::new(StaticToken("t".into()))).with_dry_run();
        ensure_locked_archive(
            &client,
            "example-archive",
            "example-deployment-bundles",
            "us-west4",
            LockedLane::GitBundles,
        )
        .await
        .unwrap();
        let calls = client.recorded_calls();
        assert_eq!(calls.len(), 3, "{calls:?}");
        let create = calls[0].body.as_deref().unwrap_or_default();
        assert!(
            create.contains("\"retentionPeriod\":\"220903200\"")
                && create.contains("\"publicAccessPrevention\":\"enforced\""),
            "{create}"
        );
        assert!(calls[1]
            .url
            .ends_with("/storage/v1/b/example-deployment-bundles"));
        assert!(calls[2]
            .url
            .ends_with("/storage/v1/b/example-deployment-bundles/lockRetentionPolicy"));
    }

    /// The ownership check lists the archive project's buckets; `owns`
    /// selects whether the offsite bucket is among them.
    async fn mount_archive_project_listing(server: &MockServer, owns: bool) {
        let items = if owns {
            json!({"items": [{"name": "example-deployment-archives-offsite"}]})
        } else {
            json!({"items": []})
        };
        Mock::given(method("GET"))
            .and(path("/storage/v1/b"))
            .and(query_param("project", "example-archive"))
            .and(query_param("prefix", "example-deployment-archives-offsite"))
            .respond_with(ResponseTemplate::new(200).set_body_json(items))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn offsite_archive_is_created_verified_then_locked() {
        let server = MockServer::start().await;
        mount_archive_project_listing(&server, true).await;
        Mock::given(method("POST"))
            .and(path("/storage/v1/b"))
            .and(query_param("project", "example-archive"))
            .and(body_partial_json(json!({
                "name": "example-deployment-archives-offsite",
                "storageClass": "STANDARD",
                "retentionPolicy": { "retentionPeriod": "788940000" },
                "iamConfiguration": {
                    "uniformBucketLevelAccess": { "enabled": true },
                    "publicAccessPrevention": "enforced"
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"name": "b"})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/storage/v1/b/example-deployment-archives-offsite"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "metageneration": "1",
                "retentionPolicy": {
                    "retentionPeriod": "788940000",
                    "isLocked": false
                },
                "iamConfiguration": {
                    "uniformBucketLevelAccess": { "enabled": true },
                    "publicAccessPrevention": "enforced"
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/storage/v1/b/example-deployment-archives-offsite"))
            .and(body_partial_json(json!({ "lifecycle": { "rule": [] } })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        // The lock carries the metageneration guard read from the verified
        // bucket, and reports the policy back as locked.
        Mock::given(method("POST"))
            .and(path(
                "/storage/v1/b/example-deployment-archives-offsite/lockRetentionPolicy",
            ))
            .and(query_param("ifMetagenerationMatch", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "retentionPolicy": {
                    "retentionPeriod": "788940000",
                    "isLocked": true
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let outcome = ensure_locked_archive(
            &client_pointed_at(&server),
            "example-archive",
            "example-deployment-archives-offsite",
            "us-west4",
            LockedLane::OffsiteArchives,
        )
        .await
        .unwrap();
        assert_eq!(outcome, EnsureOutcome::Created);
    }

    #[tokio::test]
    async fn already_locked_offsite_archive_skips_the_lock_call() {
        let server = MockServer::start().await;
        mount_archive_project_listing(&server, true).await;
        Mock::given(method("POST"))
            .and(path("/storage/v1/b"))
            .respond_with(ResponseTemplate::new(409).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/storage/v1/b/example-deployment-archives-offsite"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "metageneration": "7",
                "retentionPolicy": {
                    "retentionPeriod": "788940000",
                    "isLocked": true
                },
                "iamConfiguration": {
                    "uniformBucketLevelAccess": { "enabled": true },
                    "publicAccessPrevention": "enforced"
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/storage/v1/b/example-deployment-archives-offsite"))
            .and(body_partial_json(json!({ "lifecycle": { "rule": [] } })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        // An already-locked, verified bucket is a no-op: a lock call here
        // would fail the test, and so would a policy rewrite.
        Mock::given(method("POST"))
            .and(path(
                "/storage/v1/b/example-deployment-archives-offsite/lockRetentionPolicy",
            ))
            .respond_with(ResponseTemplate::new(500).set_body_string("lock must not be called"))
            .expect(0)
            .mount(&server)
            .await;

        let outcome = ensure_locked_archive(
            &client_pointed_at(&server),
            "example-archive",
            "example-deployment-archives-offsite",
            "us-west4",
            LockedLane::OffsiteArchives,
        )
        .await
        .unwrap();
        assert_eq!(outcome, EnsureOutcome::AlreadyExists);
    }

    #[tokio::test]
    async fn offsite_archive_refuses_to_lock_a_policy_it_did_not_write() {
        let server = MockServer::start().await;
        mount_archive_project_listing(&server, true).await;
        Mock::given(method("POST"))
            .and(path("/storage/v1/b"))
            .respond_with(ResponseTemplate::new(409).set_body_json(json!({})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/storage/v1/b/example-deployment-archives-offsite"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "metageneration": "1",
                "retentionPolicy": {
                    "retentionPeriod": "31536000",
                    "isLocked": false
                },
                "iamConfiguration": {
                    "uniformBucketLevelAccess": { "enabled": true },
                    "publicAccessPrevention": "enforced"
                }
            })))
            .mount(&server)
            .await;
        // A bucket that pre-exists with a different retention period is
        // refused, never locked and never silently converged. The lifecycle
        // clear still runs — the desired rule set is always empty.
        Mock::given(method("PATCH"))
            .and(path("/storage/v1/b/example-deployment-archives-offsite"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"name": "example-deployment-archives-offsite"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(
                "/storage/v1/b/example-deployment-archives-offsite/lockRetentionPolicy",
            ))
            .respond_with(ResponseTemplate::new(500).set_body_string("lock must not be called"))
            .expect(0)
            .mount(&server)
            .await;

        let err = ensure_locked_archive(
            &client_pointed_at(&server),
            "example-archive",
            "example-deployment-archives-offsite",
            "us-west4",
            LockedLane::OffsiteArchives,
        )
        .await
        .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("verify offsite archive bucket"),
            "{message}"
        );
        assert!(message.contains("788940000"), "{message}");
    }

    #[tokio::test]
    async fn offsite_archive_refuses_a_bucket_outside_the_archive_project() {
        let server = MockServer::start().await;
        // The name exists somewhere — the create 409s — but the archive
        // project's own listing does not contain it, so the bucket lives in
        // some other project. Nothing may be cleared or locked.
        mount_archive_project_listing(&server, false).await;
        Mock::given(method("POST"))
            .and(path("/storage/v1/b"))
            .respond_with(ResponseTemplate::new(409).set_body_json(json!({})))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/storage/v1/b/example-deployment-archives-offsite"))
            .respond_with(
                ResponseTemplate::new(500).set_body_string("lifecycle must not be cleared"),
            )
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(
                "/storage/v1/b/example-deployment-archives-offsite/lockRetentionPolicy",
            ))
            .respond_with(ResponseTemplate::new(500).set_body_string("lock must not be called"))
            .expect(0)
            .mount(&server)
            .await;

        let err = ensure_locked_archive(
            &client_pointed_at(&server),
            "example-archive",
            "example-deployment-archives-offsite",
            "us-west4",
            LockedLane::OffsiteArchives,
        )
        .await
        .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("example-archive"), "{message}");
        assert!(message.contains("another project"), "{message}");
    }

    #[tokio::test]
    async fn offsite_archive_refuses_to_lock_over_a_restored_lifecycle_rule() {
        let server = MockServer::start().await;
        mount_archive_project_listing(&server, true).await;
        Mock::given(method("POST"))
            .and(path("/storage/v1/b"))
            .respond_with(ResponseTemplate::new(409).set_body_json(json!({})))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/storage/v1/b/example-deployment-archives-offsite"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"name": "b"})))
            .mount(&server)
            .await;
        // The verified policy is right, but the metadata snapshot read for
        // the lock carries a lifecycle rule another tool restored after the
        // clear — the lock must refuse it.
        Mock::given(method("GET"))
            .and(path("/storage/v1/b/example-deployment-archives-offsite"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "metageneration": "7",
                "lifecycle": { "rule": [{ "action": { "type": "Delete" }, "condition": { "age": 30 } }] },
                "retentionPolicy": {
                    "retentionPeriod": "788940000",
                    "isLocked": false
                },
                "iamConfiguration": {
                    "uniformBucketLevelAccess": { "enabled": true },
                    "publicAccessPrevention": "enforced"
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(
                "/storage/v1/b/example-deployment-archives-offsite/lockRetentionPolicy",
            ))
            .respond_with(ResponseTemplate::new(500).set_body_string("lock must not be called"))
            .expect(0)
            .mount(&server)
            .await;

        let err = ensure_locked_archive(
            &client_pointed_at(&server),
            "example-archive",
            "example-deployment-archives-offsite",
            "us-west4",
            LockedLane::OffsiteArchives,
        )
        .await
        .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("lifecycle rule"), "{message}");
    }

    #[tokio::test]
    async fn locked_archive_writer_grant_is_creator_viewer_and_bucket_probe() {
        let client = GcpClient::new(Arc::new(StaticToken("t".into()))).with_dry_run();
        ensure_locked_archive_writer(
            &client,
            "example-deployment",
            "navigator-web",
            "example-deployment-archives-offsite",
        )
        .await
        .unwrap();

        let calls = client.recorded_calls();
        assert_eq!(
            calls.len(),
            3,
            "exactly the creator, viewer, and bucket-probe bindings: {calls:?}"
        );
        for call in &calls {
            let command = call.url.as_str();
            assert!(
                command.contains(
                    "gcloud storage buckets add-iam-policy-binding \
                     gs://example-deployment-archives-offsite"
                ),
                "{command}"
            );
            assert!(
                command.contains(
                    "serviceAccount:navigator-web@example-deployment.iam.gserviceaccount.com"
                ),
                "{command}"
            );
        }
        let plan = calls
            .iter()
            .map(|call| call.url.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(plan.contains("roles/storage.objectCreator"), "{plan}");
        assert!(plan.contains("roles/storage.objectViewer"), "{plan}");
        // The writer probes the bucket's retention policy before every write;
        // without `storage.buckets.get` every write fails (ENG-1289).
        assert!(plan.contains("roles/storage.legacyBucketReader"), "{plan}");
        // No administration, no overwrite, and no deletion anywhere in the
        // grant — the IAM layer of the write-only guarantee.
        for forbidden in [
            "roles/storage.objectAdmin",
            "roles/storage.objectUser",
            "storage.objects.delete",
            "storage.objects.update",
            "storage.buckets.delete",
        ] {
            assert!(
                !plan.contains(forbidden),
                "{forbidden} in the grant: {plan}"
            );
        }
    }
}
