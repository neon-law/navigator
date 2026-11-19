//! `project_pull_request_notice` — where each Project pull request's Slack
//! notice was posted. The first report of a pull request records the message;
//! every later report reads it back and edits that message in place.

use sqlx::Row;
use uuid::Uuid;

use super::PostgresDb;

/// One pull request's posted notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestNotice {
    pub channel_id: String,
    pub message_ts: String,
}

/// The notice recorded for `(project, pull request number)`, if any.
///
/// # Errors
/// [`sqlx::Error`] if the read fails.
pub async fn find(
    db: &PostgresDb,
    project_id: Uuid,
    number: u32,
) -> Result<Option<PullRequestNotice>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT channel_id, message_ts FROM project_pull_request_notice \
         WHERE project_id = $1 AND number = $2",
    )
    .bind(project_id)
    .bind(i64::from(number))
    .fetch_optional(db.pool())
    .await?;
    row.map(|row| {
        Ok(PullRequestNotice {
            channel_id: row.try_get("channel_id")?,
            message_ts: row.try_get("message_ts")?,
        })
    })
    .transpose()
}

/// Record where the pull request's notice now lives, replacing any earlier
/// record: a message deleted in Slack is posted again under a new timestamp.
///
/// # Errors
/// [`sqlx::Error`] if the write fails.
pub async fn record(
    db: &PostgresDb,
    project_id: Uuid,
    number: u32,
    notice: &PullRequestNotice,
) -> Result<(), sqlx::Error> {
    let now = chrono::Utc::now();
    sqlx::query(
        "INSERT INTO project_pull_request_notice \
             (project_id, number, channel_id, message_ts, inserted_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $5) \
         ON CONFLICT (project_id, number) DO UPDATE SET \
             channel_id = EXCLUDED.channel_id, \
             message_ts = EXCLUDED.message_ts, \
             updated_at = EXCLUDED.updated_at",
    )
    .bind(project_id)
    .bind(i64::from(number))
    .bind(&notice.channel_id)
    .bind(&notice.message_ts)
    .bind(now)
    .execute(db.pool())
    .await?;
    Ok(())
}
