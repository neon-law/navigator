//! Completed-envelope receipts: one worker per receipt, review and resume
//! scoped to the Firm, and a cursor that never moves backwards.
use super::super::{fixtures, PostgresDb};
use crate::docusign_completion::{
    checkpoint, claim, claim_notice, cursor, enqueue, finish, finish_notice, resume_adopted,
    review_queue, Outcome,
};
use chrono::TimeZone as _;
use uuid::Uuid;

async fn firm(pg: &PostgresDb) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO firm(id, name, status, slug) VALUES ($1, 'Synthetic', 'active', $2)")
        .bind(id)
        .bind(id.to_string())
        .execute(pg.pool())
        .await
        .unwrap();
    id
}

async fn status(pg: &PostgresDb, firm: Uuid, envelope: Uuid) -> (String, Option<Uuid>, bool) {
    sqlx::query_as(
        "SELECT status, project_id, next_attempt_at > now() FROM docusign_completion \
         WHERE firm_id = $1 AND envelope_id = $2",
    )
    .bind(firm)
    .bind(envelope)
    .fetch_one(pg.pool())
    .await
    .unwrap()
}

async fn notices_retry_until_sent(
    pg: &PostgresDb,
    firm: Uuid,
    account: Uuid,
    first: Uuid,
    project: Uuid,
) {
    // A failed notice is retried later; a sent one is never repeated.
    let mut tx = pg.pool().begin().await.unwrap();
    assert_eq!(
        claim_notice(&mut tx).await.unwrap(),
        Some((firm, account, first, project))
    );
    finish_notice(&mut tx, firm, account, first, false)
        .await
        .unwrap();
    assert!(claim_notice(&mut tx).await.unwrap().is_none());
    tx.commit().await.unwrap();
    sqlx::query("UPDATE docusign_completion SET next_attempt_at = now()")
        .execute(pg.pool())
        .await
        .unwrap();
    let mut tx = pg.pool().begin().await.unwrap();
    assert!(claim_notice(&mut tx).await.unwrap().is_some());
    finish_notice(&mut tx, firm, account, first, true)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sqlx::query("UPDATE docusign_completion SET next_attempt_at = now()")
        .execute(pg.pool())
        .await
        .unwrap();
    let mut tx = pg.pool().begin().await.unwrap();
    assert!(claim_notice(&mut tx).await.unwrap().is_none());
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn a_receipt_has_one_worker_and_review_stays_inside_its_firm() {
    crate::test_support::with_required_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let (_, notation, _) = fixtures::workflow_fixture(&pg).await;
        let (firm, project) = fixtures::firm_for_notation(&pg, notation).await;
        let other_firm = self::firm(&pg).await;
        let account = Uuid::new_v4();
        let mut envelopes = [Uuid::new_v4(), Uuid::new_v4()];
        envelopes.sort();
        let [first, second] = envelopes;

        // A repeated poll adds nothing; another Firm's receipt is its own row.
        enqueue(&pg, firm, account, &envelopes).await.unwrap();
        enqueue(&pg, firm, account, &envelopes).await.unwrap();
        enqueue(&pg, other_firm, account, &[first]).await.unwrap();

        // Three workers each hold a different receipt; a fourth finds none.
        let mut one = pg.pool().begin().await.unwrap();
        let mut two = pg.pool().begin().await.unwrap();
        let mut three = pg.pool().begin().await.unwrap();
        let mut four = pg.pool().begin().await.unwrap();
        let claimed_first = claim(&mut one).await.unwrap().unwrap();
        let claimed_second = claim(&mut two).await.unwrap().unwrap();
        let claimed_other = claim(&mut three).await.unwrap().unwrap();
        assert!(claim(&mut four).await.unwrap().is_none());
        four.rollback().await.unwrap();
        assert_eq!(
            (claimed_first.firm_id, claimed_first.envelope_id),
            (firm, first)
        );
        assert_eq!(claimed_first.account_id, account);
        assert_eq!(claimed_first.project_id, None);
        assert_eq!(claimed_second.envelope_id, second);
        assert_eq!(claimed_other.firm_id, other_firm);

        finish(&mut one, &claimed_first, Outcome::Assigned(project))
            .await
            .unwrap();
        finish(&mut two, &claimed_second, Outcome::Review("no_match"))
            .await
            .unwrap();
        finish(&mut three, &claimed_other, Outcome::Retry)
            .await
            .unwrap();
        one.commit().await.unwrap();
        two.commit().await.unwrap();
        three.commit().await.unwrap();
        assert_eq!(
            status(&pg, firm, first).await,
            ("pending".into(), Some(project), false)
        );
        assert_eq!(
            status(&pg, other_firm, first).await,
            ("pending".into(), None, true),
            "a retry waits before its next attempt"
        );

        // The assigned receipt comes back carrying its Project, then files.
        let mut tx = pg.pool().begin().await.unwrap();
        let assigned = claim(&mut tx).await.unwrap().unwrap();
        assert_eq!(assigned.project_id, Some(project));
        finish(&mut tx, &assigned, Outcome::Filed(project))
            .await
            .unwrap();
        assert!(claim(&mut tx).await.unwrap().is_none());
        tx.commit().await.unwrap();

        // Review names only provider IDs, and only to the owning Firm.
        assert_eq!(
            review_queue(&pg, firm).await.unwrap(),
            vec![(second, "no_match".to_owned())]
        );
        assert!(review_queue(&pg, other_firm).await.unwrap().is_empty());
        resume_adopted(&pg, other_firm, account, second, project)
            .await
            .unwrap();
        assert_eq!(status(&pg, firm, second).await.0, "review");
        resume_adopted(&pg, firm, account, second, project)
            .await
            .unwrap();
        assert!(review_queue(&pg, firm).await.unwrap().is_empty());
        assert_eq!(
            status(&pg, firm, second).await,
            ("pending".into(), Some(project), false)
        );
        // Adoption resumes a reviewed receipt only; a filed one stays filed.
        resume_adopted(&pg, firm, account, first, project)
            .await
            .unwrap();
        assert_eq!(status(&pg, firm, first).await.0, "filed");

        notices_retry_until_sent(&pg, firm, account, first, project).await;
    })
    .await;
}

#[tokio::test]
async fn the_polling_cursor_is_per_firm_account_and_never_moves_back() {
    crate::test_support::with_required_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let (firm, other_firm) = (self::firm(&pg).await, self::firm(&pg).await);
        let account = Uuid::new_v4();
        let at = |hour| {
            chrono::Utc
                .with_ymd_and_hms(2026, 1, 2, hour, 0, 0)
                .unwrap()
        };

        assert_eq!(cursor(&pg, firm, account).await.unwrap(), None);
        checkpoint(&pg, firm, account, at(5)).await.unwrap();
        checkpoint(&pg, firm, account, at(3)).await.unwrap();
        assert_eq!(cursor(&pg, firm, account).await.unwrap(), Some(at(5)));
        checkpoint(&pg, firm, account, at(7)).await.unwrap();
        assert_eq!(cursor(&pg, firm, account).await.unwrap(), Some(at(7)));

        assert_eq!(cursor(&pg, other_firm, account).await.unwrap(), None);
        assert_eq!(cursor(&pg, firm, Uuid::new_v4()).await.unwrap(), None);
    })
    .await;
}
