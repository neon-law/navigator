//! Project-scoped [Matter Captions](../../docs/glossary/matter-caption.md)
//! (`matter_caption`): the `matter:` block of a Project repository's
//! `navigator.yaml`, as `navigator project sync` last stored it.
//!
//! The repository is the source of truth; this row is its synced copy, so the
//! matter page's caption and party roster never read git at request time. A
//! Project holds at most one. A sync replaces it whole, and an explicit empty
//! block removes it. The shape is [`word::matter::Matter`], the same value a
//! pleading's caption is composed from.
//!
//! SurrealDB is authoritative. Each successful write or removal mirrors the
//! Project's caption to PostgreSQL when the opt-in mirror is configured
//! ([`crate::postgres_mirror::matter_pages_captions`]).

use surrealdb::types::SurrealValue;
use uuid::Uuid;
use word::matter::{Counsel, Matter, Party};

use crate::surreal::{record_id, record_uuid, SurrealDb};

const TABLE: &str = "matter_caption";

/// A Project's stored `matter:` block.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MatterCaption {
    pub id: Uuid,
    pub project_id: Uuid,
    /// The block exactly as synced.
    pub matter: Matter,
    /// When `project sync` stored it, RFC 3339.
    pub synced_at: String,
}

/// One stored caption party, as SurrealDB holds it.
#[derive(Debug, Clone, SurrealValue)]
pub(crate) struct PartyRow {
    name: String,
    role: String,
}

impl From<PartyRow> for Party {
    fn from(row: PartyRow) -> Self {
        Self {
            name: row.name,
            role: row.role,
        }
    }
}

impl From<&Party> for PartyRow {
    fn from(party: &Party) -> Self {
        Self {
            name: party.name.clone(),
            role: party.role.clone(),
        }
    }
}

/// The stored counsel of record, as SurrealDB holds it.
#[derive(Debug, Clone, SurrealValue)]
pub(crate) struct CounselRow {
    name: String,
    bar_jurisdiction: String,
    bar_number: String,
    firm: String,
    address: Vec<String>,
    telephone: String,
    email: String,
    appears_for: String,
    pro_per: bool,
}

impl From<CounselRow> for Counsel {
    fn from(row: CounselRow) -> Self {
        Self {
            name: row.name,
            bar_jurisdiction: row.bar_jurisdiction,
            bar_number: row.bar_number,
            firm: row.firm,
            address: row.address,
            telephone: row.telephone,
            email: row.email,
            appears_for: row.appears_for,
            pro_per: row.pro_per,
        }
    }
}

impl From<&Counsel> for CounselRow {
    fn from(counsel: &Counsel) -> Self {
        Self {
            name: counsel.name.clone(),
            bar_jurisdiction: counsel.bar_jurisdiction.clone(),
            bar_number: counsel.bar_number.clone(),
            firm: counsel.firm.clone(),
            address: counsel.address.clone(),
            telephone: counsel.telephone.clone(),
            email: counsel.email.clone(),
            appears_for: counsel.appears_for.clone(),
            pro_per: counsel.pro_per,
        }
    }
}

#[derive(SurrealValue)]
struct Row {
    id: surrealdb::types::RecordId,
    project_id: surrealdb::types::RecordId,
    court: Vec<String>,
    case_number: String,
    judicial_officer: String,
    department: String,
    filed_on: String,
    trial_on: Option<String>,
    parties: Vec<PartyRow>,
    counsel: CounselRow,
    synced_at: String,
}

impl Row {
    fn into_caption(self) -> Option<MatterCaption> {
        Some(MatterCaption {
            id: record_uuid(&self.id)?,
            project_id: record_uuid(&self.project_id)?,
            matter: Matter {
                court: self.court,
                case_number: self.case_number,
                judicial_officer: self.judicial_officer,
                department: self.department,
                filed_on: self.filed_on,
                trial_on: self.trial_on,
                parties: self.parties.into_iter().map(Party::from).collect(),
                counsel: self.counsel.into(),
            },
            synced_at: self.synced_at,
        })
    }
}

const SELECT: &str = "id, project_id, court, case_number, judicial_officer, department, \
     filed_on, trial_on, parties, counsel, synced_at";

fn project(project_id: Uuid) -> surrealdb::types::RecordId {
    record_id(crate::projects::PROJECT_TABLE, project_id)
}

/// Replace a Project's stored `matter:` block with `matter`. `None`, or a
/// block that declares nothing, removes it.
///
/// # Errors
///
/// When the write fails.
pub async fn replace_for_project(
    surreal: &SurrealDb,
    project_id: Uuid,
    matter: Option<&Matter>,
) -> Result<Option<MatterCaption>, surrealdb::Error> {
    let Some(matter) = matter.filter(|matter| !matter.is_empty()) else {
        delete_for_project(surreal, project_id).await?;
        return Ok(None);
    };
    surreal
        .query(format!(
            "BEGIN TRANSACTION; DELETE {TABLE} WHERE project_id = $project; \
             CREATE $id SET project_id = $project, court = $court, \
             case_number = $case_number, judicial_officer = $judicial_officer, \
             department = $department, filed_on = $filed_on, trial_on = $trial_on, \
             parties = $parties, counsel = $counsel, synced_at = $now; \
             COMMIT TRANSACTION;"
        ))
        .bind(("id", record_id(TABLE, Uuid::now_v7())))
        .bind(("project", project(project_id)))
        .bind(("court", matter.court.clone()))
        .bind(("case_number", matter.case_number.clone()))
        .bind(("judicial_officer", matter.judicial_officer.clone()))
        .bind(("department", matter.department.clone()))
        .bind(("filed_on", matter.filed_on.clone()))
        .bind(("trial_on", matter.trial_on.clone()))
        .bind((
            "parties",
            matter
                .parties
                .iter()
                .map(PartyRow::from)
                .collect::<Vec<_>>(),
        ))
        .bind(("counsel", CounselRow::from(&matter.counsel)))
        .bind(("now", chrono::Utc::now().to_rfc3339()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    mirror(surreal, project_id).await;
    for_project(surreal, project_id).await
}

/// A Project's stored `matter:` block, if any.
///
/// # Errors
///
/// When the read fails.
pub async fn for_project(
    surreal: &SurrealDb,
    project_id: Uuid,
) -> Result<Option<MatterCaption>, surrealdb::Error> {
    let mut response = surreal
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE project_id = $project LIMIT 1"
        ))
        .bind(("project", project(project_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<Row> = response.take(0)?;
    Ok(rows.into_iter().find_map(Row::into_caption))
}

/// Remove a Project's stored `matter:` block, for an explicit empty block and
/// the matter-delete cascade.
///
/// # Errors
///
/// When the delete fails.
pub async fn delete_for_project(
    surreal: &SurrealDb,
    project_id: Uuid,
) -> Result<(), surrealdb::Error> {
    surreal
        .query(format!("DELETE {TABLE} WHERE project_id = $project"))
        .bind(("project", project(project_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    mirror(surreal, project_id).await;
    Ok(())
}

/// Mirror the Project's caption, or its removal, to PostgreSQL when opted in.
async fn mirror(surreal: &SurrealDb, project_id: Uuid) {
    crate::postgres_mirror::after_write(
        surreal,
        crate::postgres_mirror::MirrorKey::MatterCaption(project_id),
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::{for_project, replace_for_project};
    use word::matter::{Counsel, Matter, Party};

    fn matter(case_number: &str) -> Matter {
        Matter {
            court: vec!["SUPERIOR COURT".into(), "COUNTY OF SAMPLE".into()],
            case_number: case_number.into(),
            judicial_officer: "Hon. Sample Jurist".into(),
            department: "12".into(),
            filed_on: "2026-02-02".into(),
            trial_on: Some("2027-01-11".into()),
            parties: vec![
                Party {
                    name: "A CORP".into(),
                    role: "Plaintiff".into(),
                },
                Party {
                    name: "B CORP".into(),
                    role: "Defendant".into(),
                },
            ],
            counsel: Counsel {
                name: "Pat Sample".into(),
                bar_jurisdiction: "California".into(),
                bar_number: "000000".into(),
                firm: "SAMPLE LAW".into(),
                address: vec!["1 Example Way".into()],
                telephone: "(000) 555-0100".into(),
                email: "pat@example.invalid".into(),
                appears_for: "Plaintiff A CORP".into(),
                pro_per: false,
            },
        }
    }

    #[tokio::test]
    async fn a_sync_replaces_the_block_and_an_empty_block_removes_it() {
        let surreal = crate::surreal::test_support::mem().await;
        let project_id = crate::test_support::seed_project_surreal(&surreal, "caption").await;
        assert!(for_project(&surreal, project_id).await.unwrap().is_none());

        let stored = replace_for_project(&surreal, project_id, Some(&matter("00CV000001")))
            .await
            .unwrap()
            .expect("a declared block is stored");
        assert_eq!(stored.matter, matter("00CV000001"));
        assert!(!stored.synced_at.is_empty());

        replace_for_project(&surreal, project_id, Some(&matter("00CV000002")))
            .await
            .unwrap();
        let read = for_project(&surreal, project_id).await.unwrap().unwrap();
        assert_eq!(read.matter.case_number, "00CV000002", "one row per Project");

        let cleared = replace_for_project(&surreal, project_id, Some(&Matter::default()))
            .await
            .unwrap();
        assert!(cleared.is_none());
        assert!(for_project(&surreal, project_id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_partial_block_round_trips_with_its_gaps() {
        let surreal = crate::surreal::test_support::mem().await;
        let project_id = crate::test_support::seed_project_surreal(&surreal, "partial").await;
        let partial = Matter {
            case_number: "00CV000003".into(),
            ..Matter::default()
        };
        let stored = replace_for_project(&surreal, project_id, Some(&partial))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.matter, partial);
    }

    #[tokio::test]
    async fn the_matter_delete_cascade_removes_the_caption() {
        let surreal = crate::surreal::test_support::mem().await;
        let project_id = crate::test_support::seed_project_surreal(&surreal, "cascade").await;
        replace_for_project(&surreal, project_id, Some(&matter("00CV000004")))
            .await
            .unwrap();
        super::delete_for_project(&surreal, project_id)
            .await
            .unwrap();
        assert!(for_project(&surreal, project_id).await.unwrap().is_none());
    }
}
