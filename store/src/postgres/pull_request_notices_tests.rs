//! Pull request notice tests: a fresh canonical schema per test, skipped
//! silently when no local PostgreSQL is running.

use uuid::Uuid;

use super::pull_request_notices::{find, record, PullRequestNotice};
use super::PostgresDb;
use crate::postgres::fixtures::{connect, fresh_schema};

async fn postgres() -> Option<PostgresDb> {
    let (options, database) = fresh_schema().await?;
    Some(connect(&options, &database).await)
}

fn notice(ts: &str) -> PullRequestNotice {
    PullRequestNotice {
        channel_id: "C0123456789".to_string(),
        message_ts: ts.to_string(),
    }
}

/// A recorded notice reads back for its own pull request only, and a
/// re-post replaces it rather than adding a second record.
#[tokio::test]
async fn a_pull_request_has_one_recorded_notice() {
    let Some(pg) = postgres().await else { return };
    let project = Uuid::now_v7();
    assert_eq!(find(&pg, project, 12).await.unwrap(), None);

    record(&pg, project, 12, &notice("1700000000.000100"))
        .await
        .unwrap();
    assert_eq!(
        find(&pg, project, 12).await.unwrap(),
        Some(notice("1700000000.000100"))
    );
    assert_eq!(find(&pg, project, 13).await.unwrap(), None);
    assert_eq!(find(&pg, Uuid::now_v7(), 12).await.unwrap(), None);

    record(&pg, project, 12, &notice("1700000500.000200"))
        .await
        .unwrap();
    assert_eq!(
        find(&pg, project, 12).await.unwrap(),
        Some(notice("1700000500.000200"))
    );
}
