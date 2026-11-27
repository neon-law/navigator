use super::{all, page, record, NewSentEmail, SentEmailError};
use crate::postgres::{PostgresDb, PostgresError};
use chrono::{DateTime, TimeZone, Utc};
use uuid::Uuid;

#[tokio::test]
async fn unactivated_history_refuses_email_audit_reads_and_writes() {
    let (options, database) = crate::postgres::fixtures::fresh_schema()
        .await
        .expect("required PostgreSQL fixture");
    let pg = crate::postgres::fixtures::connect(&options, &database).await;
    PostgresDb::with_test_db(pg, async {
        let message = NewSentEmail {
            recipient: "synthetic@example.invalid".into(),
            subject: "Synthetic".into(),
            sender: "sender@example.invalid".into(),
            project_id: None,
            person_id: None,
            template_slug: None,
            body: "Synthetic".into(),
            outcome: "failed:synthetic".into(),
            sg_message_id: None,
            sent_at: chrono::Utc::now(),
        };
        assert!(matches!(
            record(&message).await,
            Err(SentEmailError::Postgres(PostgresError::History))
        ));
        assert!(matches!(
            page(1, 10, None).await,
            Err(SentEmailError::Postgres(PostgresError::History))
        ));
        assert!(matches!(
            all().await,
            Err(SentEmailError::Postgres(PostgresError::History))
        ));
    })
    .await;
}

async fn fixture() -> PostgresDb {
    let (options, database) = crate::postgres::fixtures::fresh_schema()
        .await
        .expect("required PostgreSQL fixture");
    let pg = crate::postgres::fixtures::connect(&options, &database).await;
    crate::postgres::fixtures::activate_application(&pg).await;
    pg
}

async fn project() -> Uuid {
    let pg = PostgresDb::from_env().await.unwrap();
    let (_, notation, _) = crate::postgres::fixtures::workflow_fixture(&pg).await;
    sqlx::query_scalar("SELECT project_id FROM notation WHERE id=$1")
        .bind(notation)
        .fetch_one(pg.pool())
        .await
        .unwrap()
}
fn at(minute: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 4, 12, minute, 0).unwrap()
}

fn a_message(recipient: &str, sent_at: DateTime<Utc>) -> NewSentEmail {
    NewSentEmail {
        recipient: recipient.to_string(),
        subject: "Your matter".to_string(),
        sender: "support@neonlaw.com".to_string(),
        project_id: None,
        person_id: None,
        template_slug: Some("welcome".to_string()),
        body: "Hello.".to_string(),
        outcome: "sent".to_string(),
        sg_message_id: Some("sg-1".to_string()),
        sent_at,
    }
}

async fn journal(count: u32) {
    for n in 0..count {
        record(&a_message(&format!("a{n}@example.com"), at(n)))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn a_recorded_message_reads_back_whole() {
    let pg = fixture().await;
    PostgresDb::with_test_db(pg, async {
        let written = record(&a_message("virgo@example.com", at(0)))
            .await
            .unwrap();

        assert_eq!(written.recipient, "virgo@example.com");
        assert_eq!(written.outcome, "sent");
        assert_eq!(written.sent_at, at(0));
        assert_eq!(written.sg_message_id.as_deref(), Some("sg-1"));
        assert_eq!(written.project_id, None);
        assert_eq!(all().await.unwrap(), vec![written]);
    })
    .await;
}

/// ENG-310: a send recorded for a known matter carries it through the
/// round trip untouched.
#[tokio::test]
async fn a_message_scoped_to_a_project_reads_back_with_it() {
    let pg = fixture().await;
    PostgresDb::with_test_db(pg, async {
        let project_id = project().await;
        let mut message = a_message("virgo@example.com", at(0));
        message.project_id = Some(project_id);

        let written = record(&message).await.unwrap();
        assert_eq!(written.project_id, Some(project_id));
    })
    .await;
}

#[tokio::test]
async fn a_failed_send_journals_its_reason_and_no_upstream_id() {
    let pg = fixture().await;
    PostgresDb::with_test_db(pg, async {
        // The audit trail has no closed set of outcomes: it records what
        // the backend actually said.
        let mut failed = a_message("virgo@example.com", at(0));
        failed.outcome = "failed:connection refused".to_string();
        failed.sg_message_id = None;
        failed.template_slug = None;

        let written = record(&failed).await.unwrap();
        assert_eq!(written.outcome, "failed:connection refused");
        assert_eq!(written.sg_message_id, None);
        assert_eq!(written.template_slug, None);
    })
    .await;
}

#[tokio::test]
async fn the_trail_reads_newest_first() {
    let pg = fixture().await;
    PostgresDb::with_test_db(pg, async {
        journal(3).await;
        let recipients: Vec<String> = all()
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.recipient)
            .collect();
        assert_eq!(
            recipients,
            ["a2@example.com", "a1@example.com", "a0@example.com"]
        );
    })
    .await;
}

#[tokio::test]
async fn paging_walks_the_trail_without_dropping_or_repeating_a_row() {
    let pg = fixture().await;
    PostgresDb::with_test_db(pg, async {
        journal(5).await;

        let first = page(1, 2, None).await.unwrap();
        assert_eq!(first.total_pages, 3);
        assert_eq!(first.page, 1);
        assert_eq!(first.rows.len(), 2);

        let second = page(2, 2, None).await.unwrap();
        let third = page(3, 2, None).await.unwrap();
        assert_eq!(third.rows.len(), 1, "the final page is partial");

        let walked: Vec<String> = first
            .rows
            .iter()
            .chain(&second.rows)
            .chain(&third.rows)
            .map(|r| r.recipient.clone())
            .collect();
        let everything: Vec<String> = all()
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.recipient)
            .collect();
        assert_eq!(walked, everything, "paging visits every row exactly once");
    })
    .await;
}

#[tokio::test]
async fn an_out_of_range_page_is_clamped_to_the_last_one() {
    let pg = fixture().await;
    PostgresDb::with_test_db(pg, async {
        journal(3).await;

        // The pager renders `page` against `total_pages`; an unclamped
        // request would fetch nothing while the label still claimed a
        // real page.
        let beyond = page(99, 2, None).await.unwrap();
        assert_eq!(beyond.total_pages, 2);
        assert_eq!(beyond.page, 2, "clamped to the final page");
        assert_eq!(beyond.rows.len(), 1);
    })
    .await;
}

#[tokio::test]
async fn an_empty_trail_is_one_empty_page() {
    let pg = fixture().await;
    PostgresDb::with_test_db(pg, async {
        let empty = page(1, 50, None).await.unwrap();
        assert!(empty.rows.is_empty());
        assert_eq!(
            empty.total_pages, 1,
            "an empty table reads as Page 1 of 1, never Page 1 of 0"
        );
        assert_eq!(empty.page, 1);
    })
    .await;
}

#[tokio::test]
async fn a_full_page_boundary_does_not_invent_an_empty_last_page() {
    let pg = fixture().await;
    PostgresDb::with_test_db(pg, async {
        journal(4).await;
        let exact = page(1, 2, None).await.unwrap();
        assert_eq!(exact.total_pages, 2, "4 rows at 2 per page is 2 pages");
    })
    .await;
}

/// ENG-310: scoping to a project set filters both the count and the
/// fetch, so the pagination math describes the scoped set rather than
/// the whole table.
#[tokio::test]
async fn a_scoped_page_counts_and_fetches_only_its_own_projects() {
    let pg = fixture().await;
    PostgresDb::with_test_db(pg, async {
        let visible = project().await;
        let other = project().await;

        for n in 0..3 {
            let mut message = a_message(&format!("a{n}@example.com"), at(n));
            message.project_id = Some(visible);
            record(&message).await.unwrap();
        }
        for n in 3..5 {
            let mut message = a_message(&format!("a{n}@example.com"), at(n));
            message.project_id = Some(other);
            record(&message).await.unwrap();
        }
        // A firm-wide send with no matter at all must not leak into either
        // caller's scoped page.
        record(&a_message("firmwide@example.com", at(5)))
            .await
            .unwrap();

        let scoped = page(1, 10, Some(&[visible])).await.unwrap();
        assert_eq!(scoped.total_pages, 1);
        assert_eq!(scoped.rows.len(), 3, "only the visible project's rows");
        assert!(scoped
            .rows
            .iter()
            .all(|row| row.project_id == Some(visible)));

        let unscoped = page(1, 10, None).await.unwrap();
        assert_eq!(unscoped.rows.len(), 6, "Owner/Admin still see every row");
    })
    .await;
}

#[tokio::test]
async fn tied_timestamps_have_stable_disjoint_pages() {
    let pg = fixture().await;
    PostgresDb::with_test_db(pg, async {
        let mut ids = Vec::new();
        for n in 0..5 {
            ids.push(
                record(&a_message(&format!("tie-{n}@example.invalid"), at(0)))
                    .await
                    .unwrap()
                    .id,
            );
        }
        ids.sort_unstable_by(|left, right| right.cmp(left));
        let mut walked = Vec::new();
        for number in 1..=3 {
            let result = page(number, 2, None).await.unwrap();
            assert_eq!((result.page, result.total_pages), (number, 3));
            walked.extend(result.rows.into_iter().map(|row| row.id));
        }
        assert_eq!(walked, ids);
        assert_eq!(
            all()
                .await
                .unwrap()
                .into_iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            ids
        );
    })
    .await;
}

#[tokio::test]
async fn native_person_links_survive_and_project_scope_excludes_other_people_and_firm_mail() {
    let pg = fixture().await;
    PostgresDb::with_test_db(pg, async {
        let visible = project().await;
        let hidden = project().await;
        let pg = PostgresDb::from_env().await.unwrap();
        let mut written = Vec::new();
        for project in [visible, hidden] {
            let person = sqlx::query_scalar("SELECT person_id FROM notation WHERE project_id=$1")
                .bind(project)
                .fetch_one(pg.pool())
                .await
                .unwrap();
            let mut message = a_message("recipient@example.invalid", at(0));
            message.project_id = Some(project);
            message.person_id = Some(person);
            let row = record(&message).await.unwrap();
            assert_eq!(
                (row.person_id, row.project_id),
                (Some(person), Some(project))
            );
            written.push(row);
        }
        record(&a_message("firm@example.invalid", at(0)))
            .await
            .unwrap();
        let scoped = page(1, 10, Some(&[visible, visible])).await.unwrap();
        assert_eq!(scoped.rows, vec![written.remove(0)]);
        let denied = page(99, 0, Some(&[])).await.unwrap();
        assert!(denied.rows.is_empty());
        assert_eq!((denied.page, denied.total_pages), (1, 1));
        assert_eq!(all().await.unwrap().len(), 3);
    })
    .await;
}
