//! Immutable archive identities and the Forgejo repository attested by the server.
use super::{Draft, Error, Role};
use crate::postgres::PostgresDb;
use model::deadline_proposals::{Evidence, ForgejoPin, ForgejoRepository};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use uuid::Uuid;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VerifiedForgejo {
    pub project_id: Uuid,
    pub firm_id: Uuid,
    pub source_id: Uuid,
    pub asset_id: Uuid,
    pub sha256_hex: String,
    pub pin: ForgejoPin,
    pub origin: String,
    pub owner: String,
    pub name: String,
    pub repository_id: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceReceipt {
    pub asset_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forgejo: Option<VerifiedForgejo>,
}

impl VerifiedForgejo {
    fn matches(&self, project: Uuid, source: &Evidence, asset: Uuid) -> bool {
        self.project_id == project
            && self.source_id == source.id
            && self.asset_id == asset
            && self.sha256_hex == source.sha256_hex
            && source.forgejo.as_ref() == Some(&self.pin)
            && self.repository_id > 0
            && url::Url::parse(&self.origin).is_ok_and(|url| {
                matches!(url.scheme(), "http" | "https")
                    && url.host().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none()
            })
    }
}

pub(super) async fn capture(
    conn: &mut PgConnection,
    project: Uuid,
    draft: &Draft,
    assets: &[Uuid],
    verified: &[VerifiedForgejo],
) -> Result<Vec<SourceReceipt>, Error> {
    let repo = if draft.evidence.iter().any(|source| source.forgejo.is_some()) {
        sqlx::query_as::<_, (Uuid, String, String)>(
            "SELECT f.id,f.slug,p.code FROM project p JOIN firm f ON p.firm_id=f.id WHERE p.id=$1 FOR SHARE OF p,f",
        )
        .bind(project)
        .fetch_optional(conn)
        .await?
    } else {
        None
    };
    let mut receipts = Vec::with_capacity(assets.len());
    for (source, &asset_id) in draft.evidence.iter().zip(assets) {
        let forgejo = if let Some(pin) = &source.forgejo {
            let (firm, owner, code) = repo.as_ref().ok_or(Error::Invalid(
                "the Project has no owning Forgejo organization",
            ))?;
            let name = match pin.repository {
                ForgejoRepository::Project => code.as_str(),
                ForgejoRepository::FirmLibrary => crate::firm_library::REPOSITORY,
            };
            let receipt = verified
                .iter()
                .find(|check| {
                    check.firm_id == *firm
                        && check.owner == *owner
                        && check.name == name
                        && check.matches(project, source, asset_id)
                })
                .ok_or(Error::Invalid(
                    "the Forgejo citation was not verified for this original and repository",
                ))?;
            Some(receipt.clone())
        } else {
            None
        };
        receipts.push(SourceReceipt { asset_id, forgejo });
    }
    Ok(receipts)
}

pub(super) async fn check(
    conn: &mut PgConnection,
    project: Uuid,
    id: Uuid,
    revision: i64,
    draft: &Draft,
    assets: &[Uuid],
) -> Result<(), Error> {
    let receipts: Option<sqlx::types::Json<Vec<SourceReceipt>>> = sqlx::query_scalar(
        "SELECT source_receipts FROM deadline_proposal WHERE project_id=$1 AND id=$2 AND revision=$3",
    )
    .bind(project)
    .bind(id)
    .bind(revision)
    .fetch_one(&mut *conn)
    .await?;
    let Some(receipts) = receipts else {
        return if draft.evidence.iter().all(|s| s.forgejo.is_none()) {
            Ok(())
        } else {
            Err(Error::Invalid(
                "the Forgejo citation has no verification receipt",
            ))
        };
    };
    let firm: Option<Uuid> = if receipts.iter().any(|r| r.forgejo.is_some()) {
        sqlx::query_scalar("SELECT firm_id FROM project WHERE id=$1 FOR SHARE")
            .bind(project)
            .fetch_one(conn)
            .await?
    } else {
        None
    };
    let matches =
        receipts
            .iter()
            .zip(&draft.evidence)
            .zip(assets)
            .all(|((receipt, source), &asset)| {
                receipt.asset_id == asset
                    && match (&receipt.forgejo, &source.forgejo) {
                        (None, None) => true,
                        (Some(pin), Some(_)) => {
                            Some(pin.firm_id) == firm && pin.matches(project, source, asset)
                        }
                        _ => false,
                    }
            });
    if receipts.len() != assets.len() || !matches {
        return Err(Error::Invalid(
            "the archived original no longer matches the reviewed revision",
        ));
    }
    Ok(())
}

/// Locate the exact archived original for an authorized reader. A tombstone or purge
/// revokes historical access. Legacy Authorities resolve only while their current archive
/// still has the recorded digest.
pub async fn evidence_receipt(
    pg: &PostgresDb,
    project: Uuid,
    id: Uuid,
    revision: i64,
    index: usize,
) -> Result<Option<SourceReceipt>, Error> {
    let Ok(index) = i32::try_from(index) else {
        return Ok(None);
    };
    let row: Option<(
        sqlx::types::Json<Evidence>,
        Option<sqlx::types::Json<SourceReceipt>>,
    )> = sqlx::query_as(
        "SELECT draft->'evidence'->$4::integer,source_receipts->$4::integer
            FROM deadline_proposal WHERE project_id=$1 AND id=$2 AND revision=$3
            AND draft->'evidence'->$4::integer IS NOT NULL",
    )
    .bind(project)
    .bind(id)
    .bind(revision)
    .bind(index)
    .fetch_optional(pg.pool())
    .await?;
    let Some((source, receipt)) = row else {
        return Ok(None);
    };
    let authority = source.role == Role::Authority;
    let asset_id = receipt
        .as_ref()
        .map(|r| r.asset_id)
        .or_else(|| (!authority).then_some(source.id));
    let asset: Option<Uuid> = sqlx::query_scalar(
        "SELECT s.id FROM asset s
            WHERE s.id=COALESCE($1,(SELECT archived_asset_id FROM authority WHERE id=$2))
              AND s.project_id IS NOT DISTINCT FROM $3
              AND s.sha256_hex=$4 AND s.deleted_at IS NULL AND s.purged_at IS NULL AND s.byte_size>0
              AND (NOT $5 OR EXISTS(SELECT FROM authority WHERE id=$2))",
    )
    .bind(asset_id)
    .bind(source.id)
    .bind((!authority).then_some(project))
    .bind(&source.sha256_hex)
    .bind(authority)
    .fetch_optional(pg.pool())
    .await?;
    Ok(asset.map(|asset_id| {
        receipt.map_or(
            SourceReceipt {
                asset_id,
                forgejo: None,
            },
            |r| r.0,
        )
    }))
}
