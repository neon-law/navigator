//! The retention purge: expunge a soft-deleted document once its window closes.
//!
//! A soft deletion (`store::assets::soft_delete`) leaves a document readable by
//! lawyers for [`store::assets::RETENTION_DAYS`]. This handler is what ends the
//! window. For every chain whose `purge_after` has passed it runs the
//! governed-expunge primitive ([`crate::expunge::expunge`]), so the audit trail
//! is the same one a lawyer-authorized expunge leaves, with the
//! `retention` category, and then marks the rows purged.
//!
//! Each chain needs an explicit authority: the admin who asked
//! ([`PurgeAuthority::Admin`]), or, for the unattended schedule, the person a
//! deployment configured for the chain's own Firm ([`PurgeAuthority::ByFirm`]),
//! who must still hold that Firm's recovery-archive capability. A chain with
//! no valid authority is reported and left alone. No age rule on a bucket ever
//! stands in for this.
//!
//! Three things stop a purge, and none is an error:
//!
//! - a **Legal Hold** on the chain, rechecked from fresh rows immediately
//!   before the expunge, so a hold placed during a run still protects it;
//! - a Project that is **closed but not yet recorded**: neither its
//!   repository's `closed_repository` document nor a committed Project
//!   Recovery Archive cut after the close exists yet; and
//! - **no authority** for the chain's Firm.
//!
//! An object another unpurged row still needs — a sibling chain on the same
//! matter sharing the content-addressed bytes, or a row on another matter — is
//! kept. With the retention lanes configured, each purge is recorded under its
//! chain and window so a retry after a crash joins it, and the chain's
//! archived copies leave with the active ones unless another live or held row
//! needs them. A purged chain carries `purged_at` and is never selected again.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use cloud::StorageService;
use serde::Serialize;
use uuid::Uuid;

/// One document identity that was expunged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PurgedDocument {
    pub project_id: Uuid,
    /// The slug, or the filename of a document that has none.
    pub path: String,
    pub revisions: usize,
    /// The `expunge_record` the primitive wrote.
    pub expunge_record_id: Uuid,
}

/// What one purge run did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PurgeReport {
    pub purged: Vec<PurgedDocument>,
    /// Chains past their window that a Legal Hold keeps.
    pub held: usize,
    /// Chains past their window on a Project that is closed but not archived.
    pub awaiting_archive: usize,
    /// Chains past their window with no valid Firm-scoped authority.
    pub unauthorized: usize,
    /// Chains whose expunge failed; they stay soft-deleted for the next run.
    pub failed: Vec<String>,
}

/// Who authorizes a purge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PurgeAuthority {
    /// An admin who asked through the purge door; the primitive checks the
    /// tier and records the person.
    Admin(Uuid),
    /// The person each Firm configured for its unattended purge, by Firm id.
    ByFirm(BTreeMap<Uuid, Uuid>),
}

impl PurgeAuthority {
    /// Parse `NAVIGATOR_RETENTION_PURGE_AUTHORITIES`: comma-separated
    /// `<firm-uuid>=<person-uuid>` pairs. A malformed entry fails closed.
    ///
    /// # Errors
    /// The first entry that is not a pair of UUIDs.
    pub fn by_firm_from(value: &str) -> Result<Self, String> {
        let mut map = BTreeMap::new();
        for entry in value
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
        {
            let (firm, person) = entry
                .split_once('=')
                .ok_or_else(|| format!("`{entry}` is not <firm>=<person>"))?;
            let firm =
                Uuid::parse_str(firm.trim()).map_err(|_| format!("`{firm}` is not a UUID"))?;
            let person =
                Uuid::parse_str(person.trim()).map_err(|_| format!("`{person}` is not a UUID"))?;
            map.insert(firm, person);
        }
        Ok(Self::ByFirm(map))
    }
}

/// The optional lanes that make a purge durable and archive-aware.
#[derive(Clone, Copy, Default)]
pub struct RetentionLanes<'a> {
    pub pg: Option<&'a store::postgres::PostgresDb>,
    pub archive: Option<&'a Arc<dyn StorageService>>,
}

/// What can stop the purge as a whole.
#[derive(Debug, thiserror::Error)]
pub enum PurgeError {
    #[error("asset: {0}")]
    Asset(#[from] store::assets::AssetError),
    #[error("project: {0}")]
    Project(#[from] store::projects::ProjectStoreError),
}

/// One chain past its window, by Project and chain key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize, PartialOrd, Ord)]
pub struct DueChain {
    pub project_id: Uuid,
    pub chain_key: String,
}

/// Every chain whose `purge_after` is at or before `now`.
///
/// # Errors
/// [`PurgeError`] when the due rows cannot be read.
pub async fn due_chains(
    surreal: &store::surreal::SurrealDb,
    now: DateTime<Utc>,
) -> Result<Vec<DueChain>, PurgeError> {
    let mut chains: Vec<DueChain> = store::assets::due_for_purge(surreal, now)
        .await?
        .into_iter()
        .filter_map(|row| {
            Some(DueChain {
                project_id: row.project_id?,
                chain_key: store::document_retention::chain_key(&row),
            })
        })
        .collect();
    chains.sort();
    chains.dedup();
    Ok(chains)
}

/// Expunge every soft-deleted chain whose `purge_after` is at or before `now`,
/// authorized by one admin. The purge door's entry point.
///
/// # Errors
/// [`PurgeError`] when the due rows or a Project cannot be read. A chain whose
/// expunge fails is reported in [`PurgeReport::failed`], not raised.
pub async fn purge_expired_documents(
    surreal: &store::surreal::SurrealDb,
    storage: &Arc<dyn StorageService>,
    authorized_by: Uuid,
    now: DateTime<Utc>,
) -> Result<PurgeReport, PurgeError> {
    purge_expired_documents_with(
        surreal,
        storage,
        RetentionLanes::default(),
        &PurgeAuthority::Admin(authorized_by),
        now,
    )
    .await
}

/// [`purge_expired_documents`] with an explicit authority and lanes.
///
/// # Errors
/// As [`purge_expired_documents`].
pub async fn purge_expired_documents_with(
    surreal: &store::surreal::SurrealDb,
    storage: &Arc<dyn StorageService>,
    lanes: RetentionLanes<'_>,
    authority: &PurgeAuthority,
    now: DateTime<Utc>,
) -> Result<PurgeReport, PurgeError> {
    let mut report = PurgeReport::default();
    for chain in due_chains(surreal, now).await? {
        purge_chain(surreal, storage, lanes, authority, &chain, now, &mut report).await?;
    }
    Ok(report)
}

/// The outcome of purging one chain, for a caller that runs chains one at a
/// time (the DBOS schedule).
///
/// # Errors
/// As [`purge_expired_documents`].
pub async fn purge_one(
    surreal: &store::surreal::SurrealDb,
    storage: &Arc<dyn StorageService>,
    lanes: RetentionLanes<'_>,
    authority: &PurgeAuthority,
    chain: &DueChain,
    now: DateTime<Utc>,
) -> Result<PurgeReport, PurgeError> {
    let mut report = PurgeReport::default();
    purge_chain(surreal, storage, lanes, authority, chain, now, &mut report).await?;
    Ok(report)
}

/// Resolve and check the authority for one chain's Firm.
async fn authorizer(
    surreal: &store::surreal::SurrealDb,
    authority: &PurgeAuthority,
    firm_id: Option<Uuid>,
) -> Option<Uuid> {
    match authority {
        PurgeAuthority::Admin(person) => Some(*person),
        PurgeAuthority::ByFirm(map) => {
            let firm_id = firm_id?;
            let person_id = *map.get(&firm_id)?;
            let person = store::persons::find_by_id(surreal, person_id)
                .await
                .ok()??;
            if !person.role.is_admin_tier() {
                return None;
            }
            store::project_archives::authorize(surreal, person.role, person_id, firm_id)
                .await
                .ok()
                .map(|_| person_id)
        }
    }
}

#[allow(clippy::too_many_lines)]
async fn purge_chain(
    surreal: &store::surreal::SurrealDb,
    storage: &Arc<dyn StorageService>,
    lanes: RetentionLanes<'_>,
    authority: &PurgeAuthority,
    chain: &DueChain,
    now: DateTime<Utc>,
    report: &mut PurgeReport,
) -> Result<(), PurgeError> {
    let project_id = chain.project_id;
    let rows: Vec<store::assets::Asset> =
        store::assets::for_project_including_deleted(surreal, project_id)
            .await?
            .into_iter()
            .filter(|row| store::document_retention::chain_key(row) == chain.chain_key)
            .collect();
    // Recheck eligibility from fresh rows: a restore, a hold, or a purge that
    // landed since selection changes the answer.
    let eligible = !rows.is_empty()
        && rows.iter().all(|row| {
            row.deleted_at.is_some()
                && row.purged_at.is_none()
                && row.purge_after.is_some_and(|after| after <= now)
        });
    if !eligible {
        return Ok(());
    }
    if rows.iter().any(|row| row.legal_hold_at.is_some()) {
        report.held += 1;
        return Ok(());
    }
    if awaiting_archive(surreal, lanes.pg, project_id).await? {
        report.awaiting_archive += 1;
        return Ok(());
    }
    let firm_id = store::projects::find_by_id(surreal, project_id)
        .await?
        .and_then(|project| project.firm_id);
    let Some(authorized_by) = authorizer(surreal, authority, firm_id).await else {
        tracing::warn!(%project_id, "retention purge skipped: no Firm-scoped authority");
        report.unauthorized += 1;
        return Ok(());
    };
    let path = rows[0]
        .filename
        .clone()
        .or_else(|| rows[0].slug.clone())
        .unwrap_or_default();
    let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    let purge_after = rows.iter().filter_map(|row| row.purge_after).max();

    // With the durable lane, a retry after a crash joins the recorded run.
    let mut recorded: Option<Uuid> = None;
    if let (Some(pg), Some(firm_id), Some(purge_after)) = (lanes.pg, firm_id, purge_after) {
        match start_run(pg, chain, purge_after, firm_id, authorized_by, now).await {
            Ok(existing) => recorded = existing,
            Err(error) => {
                report.failed.push(format!("{path}: {error}"));
                return Ok(());
            }
        }
    }

    let expunge_record_id = if let Some(existing) = recorded {
        existing
    } else {
        let mut storage_keys = Vec::new();
        for row in &rows {
            for key in super::storage_keys_for_asset(row) {
                if !storage_keys.contains(&key)
                    && !store::assets::storage_key_needed_elsewhere(surreal, &key, &ids).await?
                {
                    storage_keys.push(key);
                }
            }
        }
        storage_keys.sort();
        let note = format!(
            "retention purge of {} revision(s), soft-deleted {}",
            rows.len(),
            rows[0]
                .deleted_at
                .map_or_else(String::new, |at| at.to_rfc3339()),
        );
        match super::expunge(
            surreal,
            storage,
            super::ExpungeRequest {
                project_id,
                path: &path,
                category: store::expunge_records::CATEGORY_RETENTION,
                authorized_by,
                storage_keys,
                note: Some(&note),
            },
        )
        .await
        {
            Ok(id) => {
                if let (Some(pg), Some(purge_after)) = (lanes.pg, purge_after) {
                    if let Err(error) = record_expunge(pg, chain, purge_after, id).await {
                        tracing::error!(%error, %project_id, "retention purge: run record failed");
                    }
                }
                id
            }
            Err(error) => {
                tracing::error!(%error, %project_id, %path, "retention purge: expunge failed");
                report.failed.push(format!("{path}: {error}"));
                return Ok(());
            }
        }
    };
    store::assets::mark_purged(surreal, &ids, now).await?;
    if let (Some(pg), Some(archive), Some(firm_id)) = (lanes.pg, lanes.archive, firm_id) {
        let stores = store::document_retention::RetentionStores {
            surreal,
            pg,
            documents: storage,
            archive,
        };
        if let Err(error) = store::document_retention::purge_archived_copies(
            &stores,
            firm_id,
            project_id,
            &rows,
            Some(expunge_record_id),
            now,
        )
        .await
        {
            report
                .failed
                .push(format!("{path}: archived copies remain: {error}"));
        }
    }
    if let (Some(pg), Some(purge_after)) = (lanes.pg, purge_after) {
        let _ = finish_run(pg, chain, purge_after, now).await;
    }
    report.purged.push(PurgedDocument {
        project_id,
        path,
        revisions: rows.len(),
        expunge_record_id,
    });
    Ok(())
}

/// Record (or join) one chain's purge run; returns an already-recorded expunge.
async fn start_run(
    pg: &store::postgres::PostgresDb,
    chain: &DueChain,
    purge_after: DateTime<Utc>,
    firm_id: Uuid,
    authorized_by: Uuid,
    now: DateTime<Utc>,
) -> Result<Option<Uuid>, String> {
    store::document_retention::start_purge_run(
        pg,
        chain.project_id,
        &chain.chain_key,
        purge_after,
        firm_id,
        authorized_by,
        now,
    )
    .await
    .map_err(|error| error.to_string())
}

async fn record_expunge(
    pg: &store::postgres::PostgresDb,
    chain: &DueChain,
    purge_after: DateTime<Utc>,
    expunge_record_id: Uuid,
) -> Result<(), String> {
    store::document_retention::record_purge_expunge(
        pg,
        chain.project_id,
        &chain.chain_key,
        purge_after,
        expunge_record_id,
    )
    .await
    .map_err(|error| error.to_string())
}

async fn finish_run(
    pg: &store::postgres::PostgresDb,
    chain: &DueChain,
    purge_after: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), String> {
    store::document_retention::finish_purge_run(
        pg,
        chain.project_id,
        &chain.chain_key,
        purge_after,
        now,
    )
    .await
    .map_err(|error| error.to_string())
}

/// Whether `project_id` is closed with no record of it yet: neither a
/// `closed_repository` archive nor, with the durable lane, a committed Project
/// Recovery Archive cut after the close.
async fn awaiting_archive(
    surreal: &store::surreal::SurrealDb,
    pg: Option<&store::postgres::PostgresDb>,
    project_id: Uuid,
) -> Result<bool, PurgeError> {
    let Some(project) = store::projects::find_by_id(surreal, project_id).await? else {
        return Ok(false);
    };
    let Some(closed_at) = project.closed_at.as_deref() else {
        return Ok(false);
    };
    let archive = store::assets::latest_of_kind(
        surreal,
        project_id,
        rules::kind::Kind::ClosedRepository.as_str(),
    )
    .await?;
    if archive.is_some() {
        return Ok(false);
    }
    let closed_at = DateTime::parse_from_rfc3339(closed_at)
        .map(|at| at.with_timezone(&Utc))
        .ok();
    let recovered = match (pg, closed_at) {
        (Some(pg), Some(closed_at)) => store::project_archives::latest_committed(pg, project_id)
            .await
            .ok()
            .flatten()
            .and_then(|generation| generation.cutoff_at)
            .is_some_and(|cutoff| cutoff >= closed_at),
        _ => false,
    };
    Ok(!recovered)
}

/// The governed purge as the DBOS retention schedule's source.
pub struct ScheduledPurge {
    pub surreal: store::surreal::SurrealDb,
    pub storage: Arc<dyn StorageService>,
    pub pg: Option<store::postgres::PostgresDb>,
    pub archive: Option<Arc<dyn StorageService>>,
    pub authority: PurgeAuthority,
}

#[async_trait::async_trait]
impl workflows::dbos_engine::retention_purge::RetentionSource for ScheduledPurge {
    async fn due(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<(Uuid, String)>, workflows::dbos_engine::retention_purge::PurgeQueueError> {
        due_chains(&self.surreal, now)
            .await
            .map(|chains| {
                chains
                    .into_iter()
                    .map(|chain| (chain.project_id, chain.chain_key))
                    .collect()
            })
            .map_err(|_| workflows::dbos_engine::retention_purge::PurgeQueueError::Queue)
    }

    async fn purge(&self, project_id: Uuid, chain_key: &str, now: DateTime<Utc>) -> String {
        let chain = DueChain {
            project_id,
            chain_key: chain_key.to_string(),
        };
        let lanes = RetentionLanes {
            pg: self.pg.as_ref(),
            archive: self.archive.as_ref(),
        };
        match purge_one(
            &self.surreal,
            &self.storage,
            lanes,
            &self.authority,
            &chain,
            now,
        )
        .await
        {
            Ok(report) => outcome(&report),
            Err(error) => format!("failed: {error}"),
        }
    }
}

/// One word for what a single-chain report did.
fn outcome(report: &PurgeReport) -> String {
    if !report.purged.is_empty() {
        "purged".into()
    } else if report.held > 0 {
        "held".into()
    } else if report.awaiting_archive > 0 {
        "awaiting closed-repository archive".into()
    } else if report.unauthorized > 0 {
        "no Firm-scoped authority configured".into()
    } else if let Some(failure) = report.failed.first() {
        format!("failed: {failure}")
    } else {
        "no longer eligible".into()
    }
}

#[cfg(test)]
mod tests {
    use super::{outcome, PurgeAuthority, PurgeReport, PurgedDocument, ScheduledPurge};
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use uuid::Uuid;
    use workflows::dbos_engine::retention_purge::RetentionSource as _;

    #[test]
    fn the_authority_map_parses_firm_person_pairs_and_fails_closed() {
        let firm = Uuid::new_v4();
        let person = Uuid::new_v4();
        assert_eq!(
            PurgeAuthority::by_firm_from(&format!(" {firm} = {person} , ,")).unwrap(),
            PurgeAuthority::ByFirm(BTreeMap::from([(firm, person)]))
        );
        assert_eq!(
            PurgeAuthority::by_firm_from("").unwrap(),
            PurgeAuthority::ByFirm(BTreeMap::new())
        );
        assert!(PurgeAuthority::by_firm_from("no-pair")
            .unwrap_err()
            .contains("<firm>=<person>"));
        assert!(PurgeAuthority::by_firm_from(&format!("firm={person}"))
            .unwrap_err()
            .contains("not a UUID"));
        assert!(PurgeAuthority::by_firm_from(&format!("{firm}=person"))
            .unwrap_err()
            .contains("not a UUID"));
        // One bad entry refuses the whole map, even after a good one.
        assert!(PurgeAuthority::by_firm_from(&format!("{firm}={person},{firm}")).is_err());
    }

    #[test]
    fn each_single_chain_report_reads_as_one_outcome() {
        let mut report = PurgeReport::default();
        assert_eq!(outcome(&report), "no longer eligible");
        report.failed.push("drafts/a.md: storage down".into());
        assert_eq!(outcome(&report), "failed: drafts/a.md: storage down");
        report.unauthorized = 1;
        assert_eq!(outcome(&report), "no Firm-scoped authority configured");
        report.awaiting_archive = 1;
        assert_eq!(outcome(&report), "awaiting closed-repository archive");
        report.held = 1;
        assert_eq!(outcome(&report), "held");
        report.purged.push(PurgedDocument {
            project_id: Uuid::new_v4(),
            path: "drafts/a.md".into(),
            revisions: 2,
            expunge_record_id: Uuid::new_v4(),
        });
        assert_eq!(outcome(&report), "purged");
    }

    /// With nothing soft-deleted the schedule finds no chain, and a chain it
    /// is handed anyway is no longer eligible: nothing is purged on a stale
    /// selection.
    #[tokio::test]
    async fn the_schedule_source_purges_nothing_it_cannot_reselect() {
        let dir = tempfile::tempdir().unwrap();
        let storage: Arc<dyn cloud::StorageService> =
            Arc::new(cloud::FsStorage::new(dir.path()).await.unwrap());
        let source = ScheduledPurge {
            surreal: store::surreal::test_support::mem().await,
            storage,
            pg: None,
            archive: None,
            authority: PurgeAuthority::ByFirm(BTreeMap::new()),
        };
        let now = chrono::Utc::now();
        assert!(source.due(now).await.unwrap().is_empty());
        assert_eq!(
            source.purge(Uuid::new_v4(), "drafts/a.md", now).await,
            "no longer eligible"
        );
    }
}
