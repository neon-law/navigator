//! Grants to individual filed revisions. A grant never confers Project participation.

use crate::postgres::PostgresDb;
use chrono::{DateTime, Utc};
use sqlx::{postgres::PgRow, FromRow, Row};
use uuid::Uuid;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct MediaRevision {
    pub id: Uuid,
    pub sha256: String,
    pub name: String,
    pub at: DateTime<Utc>,
    pub original_time: bool,
}

pub struct EvidenceShare {
    pub id: Uuid,
    pub project_id: Uuid,
    pub asset_id: Uuid,
    pub sha256: String,
    pub recipient_email: String,
    pub terms: String,
    pub accepted_at: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub opens: i64,
    pub downloads: i64,
    pub media: Vec<MediaRevision>,
}

impl FromRow<'_, PgRow> for EvidenceShare {
    fn from_row(row: &PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            project_id: row.try_get("project_id")?,
            asset_id: row.try_get("asset_id")?,
            sha256: row.try_get("sha256")?,
            recipient_email: row.try_get("recipient_email")?,
            terms: row.try_get("terms")?,
            accepted_at: row.try_get("accepted_at")?,
            expires_at: row.try_get("expires_at")?,
            revoked_at: row.try_get("revoked_at")?,
            opens: row.try_get("opens")?,
            downloads: row.try_get("downloads")?,
            media: row
                .try_get::<sqlx::types::Json<Vec<MediaRevision>>, _>("media")?
                .0,
        })
    }
}

pub async fn create(
    db: &PostgresDb,
    project: Uuid,
    asset: Uuid,
    sha256: &str,
    recipient: &str,
    actor: Uuid,
    terms: &str,
) -> Result<Uuid, sqlx::Error> {
    create_with_media(db, project, asset, sha256, recipient, actor, terms, &[]).await
}

#[allow(clippy::too_many_arguments)]
pub async fn create_with_media(
    db: &PostgresDb,
    project: Uuid,
    asset: Uuid,
    sha256: &str,
    recipient: &str,
    actor: Uuid,
    terms: &str,
    media: &[MediaRevision],
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO evidence_share (id, project_id, asset_id, sha256, recipient_email, created_by, terms, media)
         VALUES ($1, $2, $3, $4, lower($5), $6, $7, $8)",
    )
    .bind(id)
    .bind(project)
    .bind(asset)
    .bind(sha256)
    .bind(recipient)
    .bind(actor)
    .bind(terms)
    .bind(sqlx::types::Json(media))
    .execute(db.pool())
    .await?;
    Ok(id)
}

pub async fn list(
    db: &PostgresDb,
    project: Uuid,
    asset: Uuid,
) -> Result<Vec<EvidenceShare>, sqlx::Error> {
    sqlx::query_as(
        "SELECT * FROM evidence_share WHERE project_id = $1 AND asset_id = $2
         ORDER BY created_at DESC, id DESC",
    )
    .bind(project)
    .bind(asset)
    .fetch_all(db.pool())
    .await
}

pub async fn revoke(
    db: &PostgresDb,
    id: Uuid,
    project: Uuid,
    asset: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE evidence_share SET revoked_at = now()
         WHERE id = $1 AND project_id = $2 AND asset_id = $3 AND revoked_at IS NULL",
    )
    .bind(id)
    .bind(project)
    .bind(asset)
    .execute(db.pool())
    .await?;
    Ok(())
}

pub async fn for_recipient(
    db: &PostgresDb,
    id: Uuid,
    email: &str,
    person: Uuid,
) -> Result<Option<EvidenceShare>, sqlx::Error> {
    sqlx::query_as(
        "SELECT * FROM evidence_share WHERE id = $1 AND recipient_email = lower($2)
         AND (accepted_by IS NULL OR accepted_by = $3) AND revoked_at IS NULL AND expires_at > now()",
    )
    .bind(id)
    .bind(email)
    .bind(person)
    .fetch_optional(db.pool())
    .await
}

pub async fn accept(
    db: &PostgresDb,
    id: Uuid,
    email: &str,
    person: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE evidence_share SET accepted_at = COALESCE(accepted_at, now()), accepted_by = $3
         WHERE id = $1 AND recipient_email = lower($2)
         AND (accepted_by IS NULL OR accepted_by = $3) AND revoked_at IS NULL AND expires_at > now()",
    )
    .bind(id)
    .bind(email)
    .bind(person)
    .execute(db.pool())
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Recheck the grant immediately before returning content and count the allowed request.
pub async fn record_access(
    db: &PostgresDb,
    id: Uuid,
    email: &str,
    person: Uuid,
    download: bool,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "WITH granted AS (
             UPDATE evidence_share SET opens = opens + CASE WHEN $4 THEN 0 ELSE 1 END,
             downloads = downloads + CASE WHEN $4 THEN 1 ELSE 0 END
             WHERE id = $1 AND recipient_email = lower($2) AND accepted_by = $3
             AND revoked_at IS NULL AND expires_at > now() RETURNING id
         )
         INSERT INTO evidence_share_usage (day, opens, downloads)
         SELECT (now() AT TIME ZONE 'UTC')::date,
                CASE WHEN $4 THEN 0 ELSE 1 END, CASE WHEN $4 THEN 1 ELSE 0 END
         FROM granted
         ON CONFLICT (day) DO UPDATE SET
             opens = evidence_share_usage.opens + EXCLUDED.opens,
             downloads = evidence_share_usage.downloads + EXCLUDED.downloads",
    )
    .bind(id)
    .bind(email)
    .bind(person)
    .bind(download)
    .execute(db.pool())
    .await?;
    Ok(result.rows_affected() == 1)
}

/// The timeline reads the original export in memory. Restrict the feature to
/// bounded text revisions and check the bytes before creating a grant.
#[must_use]
pub fn eligible(asset: &crate::assets::Asset) -> bool {
    asset.deleted_at.is_none()
        && asset.purged_at.is_none()
        && (1..=5 * 1024 * 1024).contains(&asset.byte_size)
        && asset.filename.as_deref().is_some_and(|name| {
            name.rsplit_once('.')
                .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("txt"))
        })
}

/// Read only live, bounded raster images from the specified Project.
#[must_use]
pub fn image_content_type(asset: &crate::assets::Asset, project: Uuid) -> Option<&'static str> {
    if asset.project_id != Some(project)
        || asset.deleted_at.is_some()
        || asset.purged_at.is_some()
        || !(1..=10 * 1024 * 1024).contains(&asset.byte_size)
    {
        return None;
    }
    match asset.content_type.as_str() {
        "image/jpeg" => Some("image/jpeg"),
        "image/png" => Some("image/png"),
        _ => None,
    }
}

/// Resolve the ordered declaration to an immutable list of image revisions.
pub async fn resolve_media(
    db: &crate::application_store::ApplicationStore,
    project: Uuid,
    declared: &[rules::pages::TimelineMedia],
) -> Result<Vec<MediaRevision>, crate::assets::AssetError> {
    let mut images = Vec::with_capacity(declared.len());
    for item in declared {
        let Ok(id) = item.document_revision.parse() else {
            continue;
        };
        let Some(asset) = crate::assets::find_by_id(db, id).await? else {
            continue;
        };
        if image_content_type(&asset, project).is_some() {
            images.push(MediaRevision {
                id,
                sha256: asset.sha256_hex,
                name: asset.filename.unwrap_or_else(|| "Filed image".to_string()),
                at: item
                    .created_at
                    .map_or(asset.inserted_at, |date| date.to_utc()),
                original_time: item.created_at.is_some(),
            });
        }
    }
    Ok(images)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::postgres::fixtures::fresh_schema;

    #[tokio::test]
    async fn evidence_share_requires_acceptance_and_stops_counting_after_revocation() {
        let Some((options, schema)) = fresh_schema().await else {
            return;
        };
        let db = PostgresDb::connect(options.database(&schema))
            .await
            .unwrap();
        let project = Uuid::now_v7();
        let asset = Uuid::now_v7();
        let actor = Uuid::now_v7();
        let guest = Uuid::now_v7();
        let stranger = Uuid::now_v7();
        let email = "recipient@example.test";
        let id = create(
            &db,
            project,
            asset,
            "example-hash",
            email,
            actor,
            "Terms v1",
        )
        .await
        .unwrap();

        assert!(for_recipient(&db, id, "other@example.test", stranger)
            .await
            .unwrap()
            .is_none());
        assert!(!record_access(&db, id, email, guest, true).await.unwrap());
        assert!(accept(&db, id, "Recipient@Example.test", guest)
            .await
            .unwrap());
        assert!(!accept(&db, id, email, stranger).await.unwrap());
        assert!(for_recipient(&db, id, email, stranger)
            .await
            .unwrap()
            .is_none());
        assert!(record_access(&db, id, email, guest, false).await.unwrap());
        assert!(record_access(&db, id, email, guest, true).await.unwrap());

        revoke(&db, id, Uuid::now_v7(), asset).await.unwrap();
        assert!(for_recipient(&db, id, email, guest)
            .await
            .unwrap()
            .is_some());
        revoke(&db, id, project, asset).await.unwrap();
        assert!(!record_access(&db, id, email, guest, true).await.unwrap());
        assert!(for_recipient(&db, id, email, guest)
            .await
            .unwrap()
            .is_none());

        let rows = list(&db, project, asset).await.unwrap();
        assert_eq!((rows[0].opens, rows[0].downloads), (1, 1));
        let totals: (i64, i64) =
            sqlx::query_as("SELECT opens, downloads FROM evidence_share_usage")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(totals, (1, 1));

        let expired = create(
            &db,
            project,
            asset,
            "example-hash",
            email,
            actor,
            "Terms v1",
        )
        .await
        .unwrap();
        sqlx::query("UPDATE evidence_share SET expires_at = now() WHERE id = $1")
            .bind(expired)
            .execute(db.pool())
            .await
            .unwrap();
        assert!(!accept(&db, expired, email, guest).await.unwrap());
    }
}
