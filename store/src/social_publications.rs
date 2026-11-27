//! Durable per-Brand, per-provider publication state.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::social::{self as authority, SocialAuthorityError};
use crate::postgres::PostgresDb;

/// A PostgreSQL publication as `row`, its `id` the stable
/// [`publication_key`] rather than the row's own UUID.
macro_rules! pg_publication {
    ($alias:literal) => {
        concat!(
            "to_jsonb(",
            $alias,
            ") || jsonb_build_object('id', ",
            $alias,
            ".post_id::text || '_' || ",
            $alias,
            ".provider) AS row"
        )
    };
}

/// A PostgreSQL write on one publication that returns the written row.
macro_rules! returning {
    ($statement:literal) => {
        concat!(
            "WITH w AS (",
            $statement,
            " RETURNING *) SELECT ",
            pg_publication!("w"),
            " FROM w"
        )
    };
}

pub const SENDING: &str = "sending";
pub const UNKNOWN: &str = "unknown";
pub const FAILED: &str = "failed";
pub const CONFIRMED: &str = "confirmed";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    Admitted(SocialPublication),
    AlreadyInFlight(SocialPublication),
    AlreadyConfirmed(SocialPublication),
    ReconciliationRequired(SocialPublication),
}

#[derive(Debug, thiserror::Error)]
pub enum SocialPublicationError {
    #[error(transparent)]
    Postgres(#[from] SocialAuthorityError),
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
    _db: &ApplicationStore,
    post_id: Uuid,
    provider: &str,
) -> Result<Admission, SocialPublicationError> {
    let pg = authority::authority().await?;
    Box::pin(admit_pg(&pg, post_id, &supported(provider)?)).await
}

/// The provider's lowercase name, when Navigator publishes to it.
fn supported(provider: &str) -> Result<String, SocialPublicationError> {
    let provider = provider.to_ascii_lowercase();
    if crate::social_handles::is_provider(&provider) {
        Ok(provider)
    } else {
        Err(SocialPublicationError::UnsupportedProvider)
    }
}

/// The unique `(post_id, provider)` index decides a race over the first
/// admission: the loser's insert does nothing and it reads the winner's row.
async fn admit_pg(
    pg: &PostgresDb,
    post_id: Uuid,
    provider: &str,
) -> Result<Admission, SocialPublicationError> {
    let first = sqlx::query(returning!(
        "INSERT INTO social_publication (post_id, provider, state, attempt_count) \
         VALUES ($1, $2, 'sending', 1) ON CONFLICT (post_id, provider) DO NOTHING"
    ))
    .bind(post_id)
    .bind(provider.to_owned());
    if let Some(publication) = authority::maybe(pg, first).await? {
        return Ok(Admission::Admitted(publication));
    }
    let retry = sqlx::query(returning!(
        "UPDATE social_publication SET state = 'sending', attempt_count = attempt_count + 1, \
         provider_task_id = NULL, last_error = NULL, updated_at = now() \
         WHERE post_id = $1 AND provider = $2 AND state = 'failed'"
    ))
    .bind(post_id)
    .bind(provider.to_owned());
    if let Some(publication) = authority::maybe(pg, retry).await? {
        return Ok(Admission::Admitted(publication));
    }
    find_pg(pg, post_id, provider)
        .await?
        .map(admission_for_existing)
        .ok_or(SocialPublicationError::NotFound)
}

fn find_pg<'a>(
    pg: &'a PostgresDb,
    post_id: Uuid,
    provider: &str,
) -> authority::Op<'a, Option<SocialPublication>> {
    let query = sqlx::query(concat!(
        "SELECT ",
        pg_publication!("p"),
        " FROM social_publication p WHERE p.post_id = $1 AND p.provider = $2"
    ))
    .bind(post_id)
    .bind(provider.to_ascii_lowercase());
    authority::maybe(pg, query)
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
    _db: &ApplicationStore,
    post_id: Uuid,
    provider: &str,
) -> Result<bool, SocialPublicationError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "UPDATE social_publication SET state = 'unknown', last_error = $3, \
         updated_at = now() WHERE post_id = $1 AND provider = $2 AND state = 'sending'",
    )
    .bind(post_id)
    .bind(provider.to_ascii_lowercase())
    .bind(AWAITING_PROVIDER);
    Ok(authority::execute(&pg, query).await? > 0)
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
    db: &ApplicationStore,
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
    _db: &ApplicationStore,
    post_id: Uuid,
    provider: &str,
    task: &str,
) -> Result<(), SocialPublicationError> {
    if task.trim().is_empty() || task.len() > 256 {
        return Err(SocialPublicationError::InvalidConfirmation);
    }
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "UPDATE social_publication SET provider_task_id = $3, updated_at = now() \
         WHERE post_id = $1 AND provider = $2 AND state = 'unknown' \
         AND (provider_task_id IS NULL OR provider_task_id = $3)",
    )
    .bind(post_id)
    .bind(provider.to_ascii_lowercase())
    .bind(task.to_owned());
    match authority::execute(&pg, query).await? {
        0 => Err(SocialPublicationError::InvalidTransition),
        _ => Ok(()),
    }
}

/// Record the provider-verified public link of a confirmed publication whose
/// link was pending. It never changes a link already recorded.
pub async fn record_permalink(
    _db: &ApplicationStore,
    post_id: Uuid,
    provider: &str,
    permalink: &str,
) -> Result<SocialPublication, SocialPublicationError> {
    if !is_public_link(permalink) {
        return Err(SocialPublicationError::InvalidConfirmation);
    }
    let pg = authority::authority().await?;
    let query = sqlx::query(returning!(
        "UPDATE social_publication SET permalink = $3, last_error = NULL, updated_at = now() \
         WHERE post_id = $1 AND provider = $2 AND state = 'confirmed' AND permalink IS NULL"
    ))
    .bind(post_id)
    .bind(provider.to_ascii_lowercase())
    .bind(permalink.to_owned());
    authority::maybe(&pg, query)
        .await?
        .ok_or(SocialPublicationError::InvalidTransition)
}

fn is_public_link(permalink: &str) -> bool {
    let valid_https = permalink
        .strip_prefix("https://")
        .and_then(|rest| rest.split('/').next())
        .is_some_and(|host| !host.is_empty() && !host.contains('@'));
    valid_https && !permalink.chars().any(char::is_control)
}

pub async fn mark_failed(
    db: &ApplicationStore,
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
    _db: &ApplicationStore,
    post_id: Uuid,
    provider: &str,
    error: &str,
) -> Result<SocialPublication, SocialPublicationError> {
    let error = bounded_error(error);
    let pg = authority::authority().await?;
    let query = sqlx::query(returning!(
        "UPDATE social_publication SET state = 'failed', last_error = $3, updated_at = now() \
         WHERE post_id = $1 AND provider = $2 AND state = 'sending'"
    ))
    .bind(post_id)
    .bind(provider.to_ascii_lowercase())
    .bind(error);
    authority::maybe(&pg, query)
        .await?
        .ok_or(SocialPublicationError::InvalidTransition)
}

pub async fn mark_unknown(
    db: &ApplicationStore,
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
    _db: &ApplicationStore,
    post_ids: &[Uuid],
) -> Result<Vec<SocialPublication>, SocialPublicationError> {
    if post_ids.is_empty() {
        return Ok(Vec::new());
    }
    let pg = authority::authority().await?;
    let query = sqlx::query(concat!(
        "SELECT ",
        pg_publication!("p"),
        " FROM social_publication p WHERE p.post_id = ANY($1) ORDER BY p.updated_at DESC"
    ))
    .bind(post_ids.to_vec());
    Ok(authority::rows(&pg, query).await?)
}

/// Scheduled Blog Posts whose publication workflow needs to be started or
/// reconciled after a transient ingress failure.
pub async fn due_or_future_scheduled(
    _db: &ApplicationStore,
    limit: u32,
) -> Result<Vec<(Uuid, DateTime<Utc>)>, SocialPublicationError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT jsonb_build_array(id, publish_at) AS row FROM post \
         WHERE status = 'scheduled' AND publish_at IS NOT NULL ORDER BY publish_at LIMIT $1",
    )
    .bind(i64::from(limit));
    Ok(authority::rows(&pg, query).await?)
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
    _db: &ApplicationStore,
    post_id: Uuid,
    provider: &str,
    transition: PublicationTransition<'_>,
) -> Result<SocialPublication, SocialPublicationError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(returning!(
        "UPDATE social_publication SET state = $3, provider_post_id = $4, permalink = $5, \
         last_error = $6, updated_at = now() \
         WHERE post_id = $1 AND provider = $2 AND state = ANY($7)"
    ))
    .bind(post_id)
    .bind(provider.to_ascii_lowercase())
    .bind(transition.next.to_owned())
    .bind(transition.provider_post_id.map(str::to_owned))
    .bind(transition.permalink.map(str::to_owned))
    .bind(transition.error.map(str::to_owned))
    .bind(
        transition
            .expected
            .iter()
            .map(|state| (*state).to_owned())
            .collect::<Vec<_>>(),
    );
    authority::maybe(&pg, query)
        .await?
        .ok_or(SocialPublicationError::InvalidTransition)
}

/// One publication by its Post and provider, if it exists.
///
/// # Errors
///
/// A database failure.
pub async fn find(
    _db: &ApplicationStore,
    post_id: Uuid,
    provider: &str,
) -> Result<Option<SocialPublication>, SocialPublicationError> {
    let pg = authority::authority().await?;
    Ok(find_pg(&pg, post_id, provider).await?)
}

/// Confirmed receipts of one provider, `pending` only those whose public
/// link is still unverified, ordered by `updated_at` (`ascending` or not).
fn confirmed_pg<'a>(
    pg: &'a PostgresDb,
    provider: &str,
    limit: usize,
    pending: bool,
) -> authority::Op<'a, Vec<SocialPublication>> {
    let query = sqlx::query(concat!(
        "SELECT ",
        pg_publication!("p"),
        " FROM social_publication p WHERE p.provider = $1 AND p.state = 'confirmed' \
         AND p.provider_post_id IS NOT NULL AND (NOT $2 OR p.permalink IS NULL) \
         ORDER BY CASE WHEN $2 THEN p.updated_at END ASC, p.updated_at DESC LIMIT $3"
    ))
    .bind(provider.to_owned())
    .bind(pending)
    .bind(i64::try_from(limit.min(100)).unwrap_or(100));
    authority::rows(pg, query)
}

/// Confirmed receipts whose public links still need provider verification.
///
/// # Errors
///
/// A database failure.
pub async fn pending_links(
    _db: &ApplicationStore,
    provider: &str,
    limit: usize,
) -> Result<Vec<SocialPublication>, SocialPublicationError> {
    let pg = authority::authority().await?;
    Ok(confirmed_pg(&pg, provider, limit, true).await?)
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
    _db: &ApplicationStore,
    provider: &str,
    limit: usize,
) -> Result<Vec<SocialPublication>, SocialPublicationError> {
    let pg = authority::authority().await?;
    Ok(confirmed_pg(&pg, provider, limit, false).await?)
}

#[cfg(test)]
mod tests {
    use super::{admit, for_posts, mark_confirmed, mark_failed, mark_unknown, Admission};
    use crate::application_store::ApplicationStore;
    use crate::test_support;
    use uuid::Uuid;

    #[tokio::test]
    async fn pending_links_selects_only_retained_unlinked_receipts_for_the_provider() {
        test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
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
        })
        .await;
    }

    #[tokio::test]
    async fn accepted_task_receipt_is_historical_safe_and_prevents_resending() {
        test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
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
        })
        .await;
    }

    async fn post(db: &ApplicationStore) -> Uuid {
        test_support::seed_compiled_brands_postgres(db).await;
        let pg = crate::postgres::PostgresDb::application().await.unwrap();
        let id = Uuid::now_v7();
        sqlx::query("INSERT INTO post (id,brand_key,slug,title,description,status) VALUES ($1,'neon',$2,'Synthetic','','draft')")
            .bind(id).bind(id.to_string()).execute(pg.pool()).await.unwrap();
        id
    }

    #[tokio::test]
    async fn ambiguous_provider_outcome_requires_reconciliation_before_retry() {
        test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
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
        })
        .await;
    }

    #[tokio::test]
    async fn a_claimed_provider_write_is_never_claimed_twice() {
        test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
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
        })
        .await;
    }

    #[tokio::test]
    async fn a_pending_link_keeps_the_receipt_and_never_reopens_the_send() {
        test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
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
        })
        .await;
    }

    #[tokio::test]
    async fn known_failure_can_retry_but_confirmed_publication_cannot() {
        test_support::with_required_postgres(async {
        let db = ApplicationStore::uninitialized();
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
            }).await;
    }

    #[tokio::test]
    async fn scheduled_posts_are_listed_and_pre_send_failure_is_recorded() {
        test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
            let post_id = post(&db).await;
            let publish_at = chrono::Utc::now() + chrono::Duration::hours(1);
            let pg = crate::postgres::PostgresDb::application().await.unwrap();
            sqlx::query("UPDATE post SET status='scheduled', publish_at=$2 WHERE id=$1")
                .bind(post_id)
                .bind(publish_at)
                .execute(pg.pool())
                .await
                .unwrap();
            let scheduled = super::due_or_future_scheduled(&db, 10).await.unwrap();
            assert_eq!(scheduled.len(), 1);
            assert_eq!(scheduled[0].0, post_id);
            admit(&db, post_id, "facebook").await.unwrap();
            let failed =
                super::mark_failed_before_send(&db, post_id, "facebook", "account revoked")
                    .await
                    .unwrap();
            assert_eq!(failed.state, "failed");
            assert_eq!(failed.last_error.as_deref(), Some("account revoked"));
            assert!(super::for_posts(&db, &[]).await.unwrap().is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn unsupported_provider_and_untrusted_confirmation_are_rejected() {
        test_support::with_required_postgres(async {
            let db = ApplicationStore::uninitialized();
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
        })
        .await;
    }
}
