//! Forgejo push analysis consumption tests, in the established
//! `store/src/postgres/*_tests.rs` pattern: a fresh canonical schema per
//! test, skipped silently when no local PostgreSQL is running, and a
//! wiremock forge whose read expectations are the read-at-most-once
//! assertion.

use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::forgejo_push_analysis::consume_receipt;
use super::forgejo_push_webhook_journal as journal;
use super::forgejo_push_webhook_journal::NewPushReceipt;
use super::PostgresDb;

const SHA: &str = "9f3ba2c7114e0d5689c0b1a2f4e5d6c7b8a9f0e1";
const COMMIT_PATH: &str =
    "/api/v1/repos/acme-law/sample-estate/git/commits/9f3ba2c7114e0d5689c0b1a2f4e5d6c7b8a9f0e1";

fn delivery() -> NewPushReceipt {
    NewPushReceipt {
        delivered_at: chrono::Utc::now(),
        repo_full_name: "acme-law/sample-estate".to_owned(),
        ref_updates: json!([
            {
                "ref": "refs/heads/push/qvrpxyq",
                "before": "0000000000000000000000000000000000000000",
                "after": SHA,
            }
        ]),
        payload: format!(r#"{{"ref":"refs/heads/push/qvrpxyq","after":"{SHA}"}}"#),
    }
}

async fn postgres() -> Option<PostgresDb> {
    let (options, database) = crate::postgres_mirror::tests::fresh_schema().await?;
    Some(crate::postgres_mirror::tests::connect(&options, &database).await)
}

fn forge(server: &MockServer) -> cloud::ForgejoForge {
    cloud::ForgejoForge::new(&server.uri(), "acme-law", "synthetic-token").unwrap()
}

/// The forge's two reads for one analysis, each permitted exactly `times`
/// times — a further read 404s and fails the test.
async fn mount_forge_reads(server: &MockServer, times: u64) {
    Mock::given(method("GET"))
        .and(path(COMMIT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sha": SHA,
            "commit": {
                "message": "Amend the engagement letter's fee schedule\n\nbody\n",
                "author": { "name": "T-bot", "email": "tbot@neonlaw.org" },
            },
            "stats": { "additions": 12, "deletions": 3 },
        })))
        .expect(times)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{COMMIT_PATH}.diff")))
        .respond_with(ResponseTemplate::new(200).set_body_string(concat!(
            "diff --git a/letter.md b/letter.md\n",
            "--- a/letter.md\n",
            "+++ b/letter.md\n",
            "@@ -1,2 +1,2 @@\n",
            "-old\n",
            "+new\n",
        )))
        .expect(times)
        .mount(server)
        .await;
}

/// The consumption round trip: a fresh receipt is analyzed through the
/// forge exactly once, persisted `analyzed`, and a redelivery of the same
/// row — the replay sweep's re-run — answers from the persisted columns
/// with zero further forge reads.
#[tokio::test]
async fn a_receipt_is_analyzed_once_and_redeliveries_reuse_the_row() {
    let Some(pg) = postgres().await else { return };
    let server = MockServer::start().await;
    mount_forge_reads(&server, 1).await;
    let forge = forge(&server);

    let receipt = journal::journal_verified_push(&pg, &delivery())
        .await
        .expect("journal the verified push");

    let first = consume_receipt(&pg, &forge, &receipt)
        .await
        .expect("consume the fresh receipt")
        .expect("a push/* receipt analyzes");
    assert_eq!(first.change_id, "qvrpxyq");
    assert_eq!(first.author, "T-bot (tbot@neonlaw.org)");
    assert_eq!(
        first.description_summary,
        "Amend the engagement letter's fee schedule"
    );
    assert_eq!(
        first.stats,
        json!({"insertions": 12, "deletions": 3, "files": 1})
    );
    assert!(!first.conflict_signal);

    let analyzed = journal::by_event_key(&pg, &receipt.event_key)
        .await
        .expect("read the journal")
        .expect("the receipt persists");
    assert_eq!(analyzed.status, journal::ReceiptStatus::Analyzed);

    // The redelivery's re-run: the same row, answered from its columns —
    // the wiremock expectations above permit exactly one read each, so a
    // second forge read fails this test.
    let again = consume_receipt(&pg, &forge, &analyzed)
        .await
        .expect("consume the redelivery");
    assert_eq!(again, Some(first));
}

/// A verified payload naming no `push/*` bookmark is marked `skipped` —
/// observable, terminal, and never analyzed.
#[tokio::test]
async fn a_push_without_a_push_bookmark_is_skipped() {
    let Some(pg) = postgres().await else { return };
    // No forge reads mounted: any read fails the test.
    let server = MockServer::start().await;
    let forge = forge(&server);
    let other = NewPushReceipt {
        ref_updates: json!([{ "ref": "refs/heads/main", "before": SHA, "after": SHA }]),
        ..delivery()
    };
    let receipt = journal::journal_verified_push(&pg, &other)
        .await
        .expect("journal the verified push");

    assert!(consume_receipt(&pg, &forge, &receipt)
        .await
        .unwrap()
        .is_none());
    let skipped = journal::by_event_key(&pg, &receipt.event_key)
        .await
        .expect("read the journal")
        .expect("the receipt persists");
    assert_eq!(skipped.status, journal::ReceiptStatus::Skipped);
}

/// A terminal receipt re-runs nothing — a delivered event's redelivery
/// answers `None` from the journal without touching the forge.
#[tokio::test]
async fn a_terminal_receipt_re_runs_nothing() {
    let Some(pg) = postgres().await else { return };
    let server = MockServer::start().await;
    let forge = forge(&server);
    let receipt = journal::journal_verified_push(&pg, &delivery())
        .await
        .expect("journal the verified push");
    journal::mark_delivered(&pg, receipt.id)
        .await
        .expect("mark delivered");

    assert!(consume_receipt(&pg, &forge, &receipt)
        .await
        .unwrap()
        .is_none());
}
