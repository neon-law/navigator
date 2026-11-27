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
//! PostgreSQL is authoritative whenever its host is configured. The Project/Firm
//! ownership constraint applies to every replacement, and a source mirror cannot
//! overwrite or remove an authoritative caption.

use crate::postgres::{captions, matter as authority};
use uuid::Uuid;
use word::matter::{Counsel, Matter, Party};

use crate::application_store::ApplicationStore;

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
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
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

/// Replace a Project's stored `matter:` block with `matter`. `None`, or a
/// block that declares nothing, removes it.
///
/// # Errors
///
/// When the write fails.
pub async fn replace_for_project(
    _application_store: &ApplicationStore,
    project_id: Uuid,
    matter: Option<&Matter>,
) -> Result<Option<MatterCaption>, crate::postgres::matter::MatterAuthorityError> {
    let pg = authority::authority_for(TABLE).await?;
    if let Some(matter) = matter.filter(|matter| !matter.is_empty()) {
        captions::replace(&pg, project_id, matter).await.map(Some)
    } else {
        captions::delete(&pg, project_id).await?;
        Ok(None)
    }
}

/// A Project's stored `matter:` block, if any.
///
/// # Errors
///
/// When the read fails.
pub async fn for_project(
    _application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<Option<MatterCaption>, crate::postgres::matter::MatterAuthorityError> {
    let pg = authority::authority_for(TABLE).await?;
    captions::find(&pg, project_id).await
}

/// Remove a Project's stored `matter:` block, for an explicit empty block and
/// the matter-delete cascade.
///
/// # Errors
///
/// When the delete fails.
pub async fn delete_for_project(
    _application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<(), crate::postgres::matter::MatterAuthorityError> {
    let pg = authority::authority_for(TABLE).await?;
    captions::delete(&pg, project_id).await
}

#[cfg(test)]
mod tests {
    use super::{for_project, replace_for_project};
    use word::matter::{Counsel, Matter, Party};

    pub(super) fn matter(case_number: &str) -> Matter {
        Matter {
            court: vec!["SUPERIOR COURT".into(), "COUNTY OF SAMPLE".into()],
            case_number: case_number.into(),
            judicial_officer: "Hon. Sample Jurist".into(),
            department: "12".into(),
            filed_on: "2026-02-02".into(),
            trial_on: Some("2027-01-11".into()),
            signed_on: None,
            in_re: String::new(),
            page_count: false,
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
        use crate::application_store::ApplicationStore;
        use crate::postgres::fixtures::{activate_family, firm_for_notation, workflow_fixture};
        use crate::postgres::fixtures::{connect, fresh_schema};
        use crate::postgres::history::Family;
        use crate::postgres::{matter::with_test_db, PostgresDb};

        let Some((options, database)) = fresh_schema().await else {
            panic!("PostgreSQL-backed caption tests require the disposable test server");
        };
        let pg: PostgresDb = connect(&options, &database).await;
        activate_family(&pg, Family::Satellites).await;
        let (_, notation, _) = workflow_fixture(&pg).await;
        let (_, project_id) = firm_for_notation(&pg, notation).await;
        let unavailable = ApplicationStore::uninitialized();

        Box::pin(with_test_db(pg.clone(), async {
            assert!(for_project(&unavailable, project_id)
                .await
                .unwrap()
                .is_none());

            let stored = replace_for_project(&unavailable, project_id, Some(&matter("00CV000001")))
                .await
                .unwrap()
                .expect("a declared block is stored");
            assert_eq!(stored.matter, matter("00CV000001"));
            assert!(!stored.synced_at.is_empty());

            replace_for_project(&unavailable, project_id, Some(&matter("00CV000002")))
                .await
                .unwrap();
            let read = for_project(&unavailable, project_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(read.matter.case_number, "00CV000002", "one row per Project");

            let partial = Matter {
                case_number: "00CV000003".into(),
                ..Matter::default()
            };
            let stored = replace_for_project(&unavailable, project_id, Some(&partial))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(stored.matter, partial);

            // LAW-242: the signing date survives a sync round trip, including
            // the decision to declare none.
            let mut dated = matter("00CV000005");
            dated.signed_on = Some("2026-11-11".into());
            let stored = replace_for_project(&unavailable, project_id, Some(&dated))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(stored.matter.signed_on.as_deref(), Some("2026-11-11"));
            replace_for_project(&unavailable, project_id, Some(&matter("00CV000005")))
                .await
                .unwrap();
            let read = for_project(&unavailable, project_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                read.matter.signed_on, None,
                "an unsigned matter stores no date"
            );

            replace_for_project(&unavailable, project_id, Some(&matter("00CV000004")))
                .await
                .unwrap();
            super::delete_for_project(&unavailable, project_id)
                .await
                .unwrap();
            assert!(for_project(&unavailable, project_id)
                .await
                .unwrap()
                .is_none());

            let cleared = replace_for_project(&unavailable, project_id, Some(&Matter::default()))
                .await
                .unwrap();
            assert!(cleared.is_none());
            assert!(for_project(&unavailable, project_id)
                .await
                .unwrap()
                .is_none());
        }))
        .await;
        pg.pool().close().await;
    }
}

#[cfg(test)]
#[path = "matter_captions/compatibility_tests.rs"]
mod compatibility_tests;
