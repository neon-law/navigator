//! Durable per-Brand, per-provider publication state.

use chrono::{DateTime, Utc};
use serde::Serialize;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::surreal::{record_id, record_uuid, SurrealDb};

const TABLE: &str = "social_publication";
const SELECT: &str = "id, post_id, provider, state, attempt_count, provider_task_id, provider_post_id, permalink, last_error, inserted_at, updated_at";
pub const SENDING: &str = "sending";
pub const UNKNOWN: &str = "unknown";
pub const FAILED: &str = "failed";
pub const CONFIRMED: &str = "confirmed";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SocialPublication {
    pub id: String,
    pub post_id: Uuid,
    pub provider: String,
    pub state: String,
    pub attempt_count: u32,
    /// Asynchronous provider task receipt; never a public post identity.
    pub provider_task_id: Option<String>,
    pub provider_post_id: Option<String>,
    pub permalink: Option<String>,
    pub last_error: Option<String>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(SurrealValue)]
struct PublicationRow {
    id: surrealdb::types::RecordId,
    post_id: surrealdb::types::RecordId,
    provider: String,
    state: String,
    attempt_count: i64,
    provider_task_id: Option<String>,
    provider_post_id: Option<String>,
    permalink: Option<String>,
    last_error: Option<String>,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

impl PublicationRow {
    fn into_publication(self) -> Option<SocialPublication> {
        let post_id = record_uuid(&self.post_id)?;
        Some(SocialPublication {
            id: publication_key(post_id, &self.provider),
            post_id,
            provider: self.provider,
            state: self.state,
            attempt_count: u32::try_from(self.attempt_count).ok()?,
            provider_task_id: self.provider_task_id,
            provider_post_id: self.provider_post_id,
            permalink: self.permalink,
            last_error: self.last_error,
            inserted_at: self.inserted_at.into(),
            updated_at: self.updated_at.into(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    Admitted(SocialPublication),
    AlreadyInFlight(SocialPublication),
    AlreadyConfirmed(SocialPublication),
    ReconciliationRequired(SocialPublication),
}

#[derive(Debug, thiserror::Error)]
pub enum SocialPublicationError {
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    #[error("unsupported social provider")]
    UnsupportedProvider,
    #[error("publication was not found")]
    NotFound,
    #[error("publication state does not allow this transition")]
    InvalidTransition,
    #[error("provider confirmation must include an HTTPS permalink")]
    InvalidConfirmation,
}

/// A stable identity for a post/provider pair. Provider attempts are never
/// duplicated by a Restate replay or repeated scheduling request.
#[must_use]
pub fn publication_key(post_id: Uuid, provider: &str) -> String {
    format!("{post_id}_{}", provider.to_ascii_lowercase())
}

/// Reserve one provider publication. Confirmed and ambiguous outcomes are
/// never sent again implicitly; only a known failure can be retried.
pub async fn admit(
    db: &SurrealDb,
    post_id: Uuid,
    provider: &str,
) -> Result<Admission, SocialPublicationError> {
    let admission = admit_in_surreal(db, post_id, provider).await?;
    if let Admission::Admitted(publication) = &admission {
        mirror(db, post_id, &publication.provider).await;
    }
    Ok(admission)
}

/// Mirror the publication row into PostgreSQL after a successful write.
async fn mirror(db: &SurrealDb, post_id: Uuid, provider: &str) {
    crate::postgres_mirror::after_write(
        db,
        crate::postgres_mirror::MirrorKey::SocialPublication {
            post_id,
            provider: provider.to_ascii_lowercase(),
        },
    )
    .await;
}

async fn admit_in_surreal(
    db: &SurrealDb,
    post_id: Uuid,
    provider: &str,
) -> Result<Admission, SocialPublicationError> {
    let provider = provider.to_ascii_lowercase();
    if !matches!(
        provider.as_str(),
        "facebook" | "instagram" | "threads" | "youtube" | "linkedin" | "x" | "tiktok"
    ) {
        return Err(SocialPublicationError::UnsupportedProvider);
    }
    let key = publication_key(post_id, &provider);
    let id = surrealdb::types::RecordId::new(TABLE, key.clone());
    match read_by_id(db, &id).await? {
        None => {
            let result = db
                .query(format!(
                    "CREATE $id SET post_id = $post, provider = $provider, state = $state, \
                     attempt_count = 1, inserted_at = time::now(), updated_at = time::now() RETURN {SELECT}"
                ))
                .bind(("id", id.clone()))
                .bind(("post", record_id("post", post_id)))
                .bind(("provider", provider))
                .bind(("state", SENDING.to_string()))
                .await;
            match result {
                Ok(response) => {
                    let mut response = response.check()?;
                    let row: Option<PublicationRow> = response.take(0)?;
                    row.and_then(PublicationRow::into_publication)
                        .map(Admission::Admitted)
                        .ok_or(SocialPublicationError::NotFound)
                }
                Err(error) => {
                    // A concurrent first admission can win the stable record
                    // key. Read its state and fail closed instead of sending
                    // a second request.
                    if let Some(publication) = read_by_id(db, &id).await? {
                        Ok(admission_for_existing(publication))
                    } else {
                        Err(SocialPublicationError::Db(error))
                    }
                }
            }
        }
        Some(publication) if publication.state == FAILED => {
            let mut response = db
                .query(format!(
                    "UPDATE $id SET state = $sending, attempt_count += 1, provider_task_id = NONE, last_error = NONE, updated_at = time::now() \
                     WHERE state = $failed RETURN {SELECT}"
                ))
                .bind(("id", id.clone()))
                .bind(("sending", SENDING.to_string()))
                .bind(("failed", FAILED.to_string()))
                .await?
                .check()?;
            let row: Option<PublicationRow> = response.take(0)?;
            match row.and_then(PublicationRow::into_publication) {
                Some(publication) => Ok(Admission::Admitted(publication)),
                None => match read_by_id(db, &id).await? {
                    Some(publication) => Ok(admission_for_existing(publication)),
                    None => Err(SocialPublicationError::NotFound),
                },
            }
        }
        Some(publication) => Ok(admission_for_existing(publication)),
    }
}

fn admission_for_existing(publication: SocialPublication) -> Admission {
    match publication.state.as_str() {
        CONFIRMED => Admission::AlreadyConfirmed(publication),
        UNKNOWN => Admission::ReconciliationRequired(publication),
        _ => Admission::AlreadyInFlight(publication),
    }
}

/// Claim the one provider write for an admitted publication.
///
/// The row moves from `sending` to `unknown` before the request leaves, so a
/// write interrupted by a crash, or a step replayed after one, is left for
/// reconciliation instead of being sent again. Returns `false` when the write
/// was already claimed and nothing may be sent.
pub async fn begin_provider_write(
    db: &SurrealDb,
    post_id: Uuid,
    provider: &str,
) -> Result<bool, SocialPublicationError> {
    let key = publication_key(post_id, provider);
    let mut response = db
        .query(format!(
            "UPDATE $id SET state = $unknown, last_error = $reason, updated_at = time::now() \
             WHERE state = $sending RETURN {SELECT}"
        ))
        .bind(("id", surrealdb::types::RecordId::new(TABLE, key)))
        .bind(("sending", SENDING.to_string()))
        .bind(("unknown", UNKNOWN.to_string()))
        .bind(("reason", AWAITING_PROVIDER.to_string()))
        .await?
        .check()?;
    let row: Option<PublicationRow> = response.take(0)?;
    if row.is_some() {
        mirror(db, post_id, provider).await;
    }
    Ok(row.is_some())
}

/// The reason an in-flight provider write carries until it settles.
pub const AWAITING_PROVIDER: &str = "awaiting the provider's response";

/// The reason a confirmed publication carries until its public link is read
/// back from the provider.
pub const PERMALINK_PENDING: &str =
    "Receipt recorded; public link pending verification. Check provider processing and visibility.";

/// Settle a publication the provider confirmed. A claimed write
/// ([`begin_provider_write`]) settles from `unknown`.
///
/// `provider_post_id` is the provider's receipt. `permalink` is set only from
/// a URL the provider itself returned; without one the publication is
/// confirmed with its link pending, and [`record_permalink`] completes it. A
/// confirmed publication is never sent again either way.
pub async fn mark_confirmed(
    db: &SurrealDb,
    post_id: Uuid,
    provider: &str,
    provider_post_id: &str,
    permalink: Option<&str>,
) -> Result<SocialPublication, SocialPublicationError> {
    if provider_post_id.trim().is_empty() || permalink.is_some_and(|link| !is_public_link(link)) {
        return Err(SocialPublicationError::InvalidConfirmation);
    }
    transition(
        db,
        post_id,
        provider,
        PublicationTransition {
            expected: &[SENDING, UNKNOWN],
            next: CONFIRMED,
            provider_post_id: Some(provider_post_id),
            permalink,
            error: permalink.is_none().then_some(PERMALINK_PENDING),
        },
    )
    .await
}

/// Save an accepted asynchronous task under the claimed write. A resumed
/// worker polls this receipt; it cannot send another publish request.
///
/// # Errors
/// Returns an error if the write was not claimed or a different task was stored.
pub async fn record_provider_task(
    db: &SurrealDb,
    post_id: Uuid,
    provider: &str,
    task: &str,
) -> Result<(), SocialPublicationError> {
    if task.trim().is_empty() || task.len() > 256 {
        return Err(SocialPublicationError::InvalidConfirmation);
    }
    let id = surrealdb::types::RecordId::new(TABLE, publication_key(post_id, provider));
    let mut result = db.query("UPDATE $id SET provider_task_id = $task, updated_at = time::now() WHERE state = 'unknown' AND (provider_task_id IS NONE OR provider_task_id = $task) RETURN id")
        .bind(("id",id)).bind(("task",task.to_string())).await?.check()?;
    let rows: Vec<surrealdb::types::RecordId> = result.take("id")?;
    if rows.is_empty() {
        return Err(SocialPublicationError::InvalidTransition);
    }
    mirror(db, post_id, provider).await;
    Ok(())
}

/// Record the provider-verified public link of a confirmed publication whose
/// link was pending. It never changes a link already recorded.
pub async fn record_permalink(
    db: &SurrealDb,
    post_id: Uuid,
    provider: &str,
    permalink: &str,
) -> Result<SocialPublication, SocialPublicationError> {
    if !is_public_link(permalink) {
        return Err(SocialPublicationError::InvalidConfirmation);
    }
    let key = publication_key(post_id, provider);
    let mut response = db
        .query(format!(
            "UPDATE $id SET permalink = $permalink, last_error = NONE, updated_at = time::now() \
             WHERE state = $confirmed AND permalink IS NONE RETURN {SELECT}"
        ))
        .bind(("id", surrealdb::types::RecordId::new(TABLE, key)))
        .bind(("confirmed", CONFIRMED.to_string()))
        .bind(("permalink", permalink.to_string()))
        .await?
        .check()?;
    let row: Option<PublicationRow> = response.take(0)?;
    let publication = row
        .and_then(PublicationRow::into_publication)
        .ok_or(SocialPublicationError::InvalidTransition)?;
    mirror(db, post_id, provider).await;
    Ok(publication)
}

fn is_public_link(permalink: &str) -> bool {
    let valid_https = permalink
        .strip_prefix("https://")
        .and_then(|rest| rest.split('/').next())
        .is_some_and(|host| !host.is_empty() && !host.contains('@'));
    valid_https && !permalink.chars().any(char::is_control)
}

pub async fn mark_failed(
    db: &SurrealDb,
    post_id: Uuid,
    provider: &str,
    error: &str,
) -> Result<SocialPublication, SocialPublicationError> {
    let error = bounded_error(error);
    transition(
        db,
        post_id,
        provider,
        PublicationTransition {
            expected: &[SENDING, UNKNOWN],
            next: FAILED,
            provider_post_id: None,
            permalink: None,
            error: Some(&error),
        },
    )
    .await
}

/// Record a known pre-send failure when the destination became unavailable
/// before any provider request was attempted.
pub async fn mark_failed_before_send(
    db: &SurrealDb,
    post_id: Uuid,
    provider: &str,
    error: &str,
) -> Result<SocialPublication, SocialPublicationError> {
    let error = bounded_error(error);
    let key = publication_key(post_id, provider);
    let mut response = db
        .query(format!(
            "UPDATE $id SET state = $failed, last_error = $error, updated_at = time::now() \
             WHERE state = $sending RETURN {SELECT}"
        ))
        .bind(("id", surrealdb::types::RecordId::new(TABLE, key)))
        .bind(("sending", SENDING.to_string()))
        .bind(("failed", FAILED.to_string()))
        .bind(("error", error))
        .await?
        .check()?;
    let row: Option<PublicationRow> = response.take(0)?;
    let publication = row
        .and_then(PublicationRow::into_publication)
        .ok_or(SocialPublicationError::InvalidTransition)?;
    mirror(db, post_id, provider).await;
    Ok(publication)
}

pub async fn mark_unknown(
    db: &SurrealDb,
    post_id: Uuid,
    provider: &str,
    reason: &str,
) -> Result<SocialPublication, SocialPublicationError> {
    let reason = bounded_error(reason);
    transition(
        db,
        post_id,
        provider,
        PublicationTransition {
            expected: &[SENDING, UNKNOWN],
            next: UNKNOWN,
            provider_post_id: None,
            permalink: None,
            error: Some(&reason),
        },
    )
    .await
}

/// Results for the posts visible in one authorized Blog Post catalog.
pub async fn for_posts(
    db: &SurrealDb,
    post_ids: &[Uuid],
) -> Result<Vec<SocialPublication>, SocialPublicationError> {
    if post_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids = post_ids
        .iter()
        .copied()
        .map(|id| record_id("post", id))
        .collect::<Vec<_>>();
    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE post_id IN $posts ORDER BY updated_at DESC"
        ))
        .bind(("posts", ids))
        .await?
        .check()?;
    let rows: Vec<PublicationRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(PublicationRow::into_publication)
        .collect())
}

/// Scheduled Blog Posts whose publication workflow needs to be started or
/// reconciled after a transient ingress failure.
pub async fn due_or_future_scheduled(
    db: &SurrealDb,
    limit: u32,
) -> Result<Vec<(Uuid, DateTime<Utc>)>, SocialPublicationError> {
    #[derive(SurrealValue)]
    struct ScheduledRow {
        id: surrealdb::types::RecordId,
        publish_at: surrealdb::types::Datetime,
    }
    let mut response = db
        .query("SELECT id, publish_at FROM post WHERE status = 'scheduled' AND publish_at IS NOT NONE ORDER BY publish_at LIMIT $limit")
        .bind(("limit", i64::from(limit)))
        .await?
        .check()?;
    let rows: Vec<ScheduledRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(|row| Some((record_uuid(&row.id)?, row.publish_at.into())))
        .collect())
}

fn bounded_error(value: &str) -> String {
    value.chars().take(500).collect()
}

struct PublicationTransition<'a> {
    expected: &'a [&'a str],
    next: &'a str,
    provider_post_id: Option<&'a str>,
    permalink: Option<&'a str>,
    error: Option<&'a str>,
}

async fn transition(
    db: &SurrealDb,
    post_id: Uuid,
    provider: &str,
    transition: PublicationTransition<'_>,
) -> Result<SocialPublication, SocialPublicationError> {
    let key = publication_key(post_id, provider);
    let id = surrealdb::types::RecordId::new(TABLE, key);
    let query = db
        .query(format!(
            "UPDATE $id SET state = $next, provider_post_id = $provider_id, permalink = $permalink, \
             last_error = $error, updated_at = time::now() WHERE state IN $expected RETURN {SELECT}"
        ))
        .bind(("id", id))
        .bind((
            "expected",
            transition
                .expected
                .iter()
                .map(|state| (*state).to_string())
                .collect::<Vec<_>>(),
        ))
        .bind(("next", transition.next.to_string()))
        .bind(("provider_id", transition.provider_post_id.map(str::to_string)))
        .bind(("permalink", transition.permalink.map(str::to_string)))
        .bind(("error", transition.error.map(str::to_string)));
    let mut response = query.await?.check()?;
    let row: Option<PublicationRow> = response.take(0)?;
    let publication = row
        .and_then(PublicationRow::into_publication)
        .ok_or(SocialPublicationError::InvalidTransition)?;
    mirror(db, post_id, provider).await;
    Ok(publication)
}

/// One publication by its Post and provider, if it exists.
///
/// # Errors
///
/// A database failure.
pub async fn find(
    db: &SurrealDb,
    post_id: Uuid,
    provider: &str,
) -> Result<Option<SocialPublication>, SocialPublicationError> {
    let id = surrealdb::types::RecordId::new(TABLE, publication_key(post_id, provider));
    read_by_id(db, &id).await
}

/// Confirmed receipts whose public links still need provider verification.
///
/// # Errors
///
/// A database failure.
pub async fn pending_links(
    db: &SurrealDb,
    provider: &str,
    limit: usize,
) -> Result<Vec<SocialPublication>, SocialPublicationError> {
    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE provider = $provider AND state = $confirmed \
             AND provider_post_id IS NOT NONE AND permalink IS NONE ORDER BY updated_at ASC LIMIT $limit"
        ))
        .bind(("provider", provider.to_string()))
        .bind(("confirmed", CONFIRMED.to_string()))
        .bind(("limit", limit.min(100)))
        .await?
        .check()?;
    let rows: Vec<PublicationRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(PublicationRow::into_publication)
        .collect())
}

/// Confirmed receipts to poll for inbound activity, newest first.
///
/// Unlike [`pending_links`], a `permalink` may already be verified: comment
/// polling reads every confirmed video, not only links awaiting verification.
///
/// # Errors
///
/// A database failure.
pub async fn confirmed_links(
    db: &SurrealDb,
    provider: &str,
    limit: usize,
) -> Result<Vec<SocialPublication>, SocialPublicationError> {
    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE provider = $provider AND state = $confirmed \
             AND provider_post_id IS NOT NONE ORDER BY updated_at DESC LIMIT $limit"
        ))
        .bind(("provider", provider.to_string()))
        .bind(("confirmed", CONFIRMED.to_string()))
        .bind(("limit", limit.min(100)))
        .await?
        .check()?;
    let rows: Vec<PublicationRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(PublicationRow::into_publication)
        .collect())
}

/// Every publication, for the PostgreSQL parity check.
pub(crate) async fn all(db: &SurrealDb) -> Result<Vec<SocialPublication>, SocialPublicationError> {
    let mut response = db
        .query(format!("SELECT {SELECT} FROM {TABLE}"))
        .await?
        .check()?;
    let rows: Vec<PublicationRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(PublicationRow::into_publication)
        .collect())
}

async fn read_by_id(
    db: &SurrealDb,
    id: &surrealdb::types::RecordId,
) -> Result<Option<SocialPublication>, SocialPublicationError> {
    let mut response = db
        .query(format!("SELECT {SELECT} FROM ONLY $id"))
        .bind(("id", id.clone()))
        .await?
        .check()?;
    let row: Option<PublicationRow> = response.take(0)?;
    Ok(row.and_then(PublicationRow::into_publication))
}

#[cfg(test)]
mod tests {
    use super::{admit, for_posts, mark_confirmed, mark_failed, mark_unknown, Admission};
    use crate::surreal::{record_id, SurrealDb};
    use uuid::Uuid;

    #[tokio::test]
    async fn pending_links_selects_only_retained_unlinked_receipts_for_the_provider() {
        let db = crate::test_support::mem_surreal().await;
        let pending = post(&db).await;
        let linked = post(&db).await;
        let unknown = post(&db).await;
        let other_provider = post(&db).await;
        for id in [pending, linked, unknown] {
            admit(&db, id, "youtube").await.unwrap();
        }
        admit(&db, other_provider, "instagram").await.unwrap();
        mark_confirmed(&db, pending, "youtube", "synthetic-video", None)
            .await
            .unwrap();
        mark_confirmed(
            &db,
            linked,
            "youtube",
            "synthetic-linked",
            Some("https://www.youtube.com/watch?v=synthetic-linked"),
        )
        .await
        .unwrap();
        mark_unknown(&db, unknown, "youtube", "uncertain upload")
            .await
            .unwrap();
        mark_confirmed(&db, other_provider, "instagram", "synthetic-reel", None)
            .await
            .unwrap();
        let rows = super::pending_links(&db, "youtube", 100).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].post_id, pending);
        assert_eq!(rows[0].provider_post_id.as_deref(), Some("synthetic-video"));
        assert!(super::pending_links(&db, "youtube", 0)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn accepted_task_receipt_is_historical_safe_and_prevents_resending() {
        let db = crate::test_support::mem_surreal().await;
        let id = post(&db).await;
        admit(&db, id, "tiktok").await.unwrap();
        let historical = for_posts(&db, &[id]).await.unwrap();
        assert!(historical[0].provider_task_id.is_none());
        assert!(super::begin_provider_write(&db, id, "tiktok")
            .await
            .unwrap());
        super::record_provider_task(&db, id, "tiktok", "synthetic-task")
            .await
            .unwrap();
        super::record_provider_task(&db, id, "tiktok", "synthetic-task")
            .await
            .unwrap();
        assert!(
            super::record_provider_task(&db, id, "tiktok", "different-task")
                .await
                .is_err()
        );
        let resumed = admit(&db, id, "tiktok").await.unwrap();
        let Admission::ReconciliationRequired(publication) = resumed else {
            panic!("accepted task cannot be sent again")
        };
        assert_eq!(
            publication.provider_task_id.as_deref(),
            Some("synthetic-task")
        );
        assert!(publication.provider_post_id.is_none());
        assert!(!super::begin_provider_write(&db, id, "tiktok")
            .await
            .unwrap());
        mark_confirmed(&db, id, "tiktok", "synthetic-public-post", None)
            .await
            .unwrap();
        let rows = for_posts(&db, &[id]).await.unwrap();
        assert_eq!(rows[0].provider_task_id.as_deref(), Some("synthetic-task"));
        assert_eq!(
            rows[0].provider_post_id.as_deref(),
            Some("synthetic-public-post")
        );
    }

    async fn post(db: &SurrealDb) -> Uuid {
        let id = Uuid::now_v7();
        db.query("CREATE $id SET brand_key = 'synthetic', slug = $slug, title = 'Synthetic', description = '', body_html = '', status = 'draft', inserted_at = time::now(), updated_at = time::now()")
            .bind(("id", record_id("post", id)))
            .bind(("slug", id.to_string()))
            .await
            .unwrap()
            .check()
            .unwrap();
        id
    }

    #[tokio::test]
    async fn ambiguous_provider_outcome_requires_reconciliation_before_retry() {
        let db = crate::test_support::mem_surreal().await;
        let post_id = post(&db).await;
        let first = admit(&db, post_id, "instagram").await.unwrap();
        assert!(
            matches!(first, Admission::Admitted(publication) if publication.attempt_count == 1)
        );
        let uncertain = mark_unknown(&db, post_id, "instagram", "connection timed out")
            .await
            .unwrap();
        assert_eq!(uncertain.state, "unknown");
        assert!(matches!(
            admit(&db, post_id, "instagram").await.unwrap(),
            Admission::ReconciliationRequired(publication) if publication.attempt_count == 1
        ));
    }

    #[tokio::test]
    async fn a_claimed_provider_write_is_never_claimed_twice() {
        let db = crate::test_support::mem_surreal().await;
        let post_id = post(&db).await;
        admit(&db, post_id, "threads").await.unwrap();
        assert!(super::begin_provider_write(&db, post_id, "threads")
            .await
            .unwrap());
        // A replayed step finds the write already claimed.
        assert!(!super::begin_provider_write(&db, post_id, "threads")
            .await
            .unwrap());
        assert!(matches!(
            admit(&db, post_id, "threads").await.unwrap(),
            Admission::ReconciliationRequired(publication)
                if publication.last_error.as_deref() == Some(super::AWAITING_PROVIDER)
        ));
        let confirmed = mark_confirmed(
            &db,
            post_id,
            "threads",
            "thread-1",
            Some("https://www.threads.net/@synthetic/post/abc"),
        )
        .await
        .unwrap();
        assert_eq!(confirmed.state, "confirmed");
        assert_eq!(confirmed.last_error, None);
    }

    #[tokio::test]
    async fn a_pending_link_keeps_the_receipt_and_never_reopens_the_send() {
        let db = crate::test_support::mem_surreal().await;
        let post_id = post(&db).await;
        admit(&db, post_id, "instagram").await.unwrap();
        let confirmed = mark_confirmed(&db, post_id, "instagram", "17890000000000001", None)
            .await
            .unwrap();
        assert_eq!(confirmed.state, "confirmed");
        assert_eq!(
            confirmed.provider_post_id.as_deref(),
            Some("17890000000000001")
        );
        assert_eq!(confirmed.permalink, None);
        assert_eq!(
            confirmed.last_error.as_deref(),
            Some(super::PERMALINK_PENDING)
        );
        assert!(matches!(
            admit(&db, post_id, "instagram").await.unwrap(),
            Admission::AlreadyConfirmed(publication) if publication.permalink.is_none()
        ));
        assert!(
            super::record_permalink(&db, post_id, "instagram", "javascript:alert(1)")
                .await
                .is_err()
        );
        let linked = super::record_permalink(
            &db,
            post_id,
            "instagram",
            "https://www.instagram.com/reel/SyntheticCode/",
        )
        .await
        .unwrap();
        assert_eq!(
            linked.permalink.as_deref(),
            Some("https://www.instagram.com/reel/SyntheticCode/")
        );
        assert_eq!(
            linked.provider_post_id.as_deref(),
            Some("17890000000000001")
        );
        assert_eq!(linked.last_error, None);
        // A recorded link is never replaced.
        assert!(super::record_permalink(
            &db,
            post_id,
            "instagram",
            "https://www.instagram.com/reel/Other/",
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn known_failure_can_retry_but_confirmed_publication_cannot() {
        let db = crate::test_support::mem_surreal().await;
        let post_id = post(&db).await;
        admit(&db, post_id, "youtube").await.unwrap();
        mark_failed(&db, post_id, "youtube", "temporary rejection")
            .await
            .unwrap();
        let retry = admit(&db, post_id, "youtube").await.unwrap();
        assert!(
            matches!(retry, Admission::Admitted(publication) if publication.attempt_count == 2)
        );
        let confirmed = mark_confirmed(
            &db,
            post_id,
            "youtube",
            "provider-video-id",
            Some("https://youtube.com/watch?v=provider-video-id"),
        )
        .await
        .unwrap();
        assert_eq!(confirmed.state, "confirmed");
        assert_eq!(
            confirmed.provider_post_id.as_deref(),
            Some("provider-video-id")
        );
        assert!(matches!(
            admit(&db, post_id, "youtube").await.unwrap(),
            Admission::AlreadyConfirmed(publication) if publication.permalink.as_deref() == Some("https://youtube.com/watch?v=provider-video-id")
        ));
        let results = for_posts(&db, &[post_id]).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].state, "confirmed");
    }

    #[tokio::test]
    async fn scheduled_posts_are_listed_and_pre_send_failure_is_recorded() {
        let db = crate::test_support::mem_surreal().await;
        let post_id = post(&db).await;
        let publish_at = chrono::Utc::now() + chrono::Duration::hours(1);
        db.query("UPDATE $id SET status = 'scheduled', publish_at = $when")
            .bind(("id", record_id("post", post_id)))
            .bind(("when", surrealdb::types::Datetime::from(publish_at)))
            .await
            .unwrap()
            .check()
            .unwrap();
        let scheduled = super::due_or_future_scheduled(&db, 10).await.unwrap();
        assert_eq!(scheduled.len(), 1);
        assert_eq!(scheduled[0].0, post_id);
        admit(&db, post_id, "facebook").await.unwrap();
        let failed = super::mark_failed_before_send(&db, post_id, "facebook", "account revoked")
            .await
            .unwrap();
        assert_eq!(failed.state, "failed");
        assert_eq!(failed.last_error.as_deref(), Some("account revoked"));
        assert!(super::for_posts(&db, &[]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn unsupported_provider_and_untrusted_confirmation_are_rejected() {
        let db = crate::test_support::mem_surreal().await;
        let post_id = post(&db).await;
        assert!(admit(&db, post_id, "unsupported").await.is_err());
        admit(&db, post_id, "facebook").await.unwrap();
        assert!(mark_confirmed(
            &db,
            post_id,
            "facebook",
            "provider-id",
            Some("javascript:alert(1)"),
        )
        .await
        .is_err());
    }
}
