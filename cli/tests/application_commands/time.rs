//! `navigator project time` end to end: the CLI binary, the portal's real
//! handlers, and PostgreSQL (LAW-583, LAW-584).
use chrono::{Duration, Utc};
use serde_json::Value;
use uuid::Uuid;

use crate::live::LiveSite;

async fn priced(site: &LiveSite) {
    store::attorney_rates::set_hourly_rate(
        &site.pg,
        store::attorney_rates::NewAttorneyRate {
            person_id: site.lawyer,
            project_id: None,
            hourly_rate_cents: 24_000,
            effective_on: Utc::now().date_naive() - Duration::days(365),
        },
    )
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backfilled_time_is_added_edited_approved_and_billed_once() {
    store::test_support::with_postgres(async {
        let site = LiveSite::start(None, None).await;
        priced(&site).await;
        let last_week = (Utc::now().date_naive() - Duration::days(7)).to_string();
        let request = Uuid::now_v7().to_string();
        let add = [
            "time",
            "add",
            "--performed-on",
            &last_week,
            "--minutes",
            "30",
            "--narrative",
            "Synthetic call with opposing counsel",
            "--request-id",
            &request,
        ];
        let added = site.json(&add);
        assert_eq!(added["status"], "pending_review");
        assert_eq!(added["backfilled"], true);
        let id = added["id"].as_str().unwrap().to_owned();

        let edited = site.json(&["time", "edit", &id, "--minutes", "18"]);
        assert_eq!(edited["minutes"], 18);
        let approved = site.json(&["time", "approve", &id]);
        assert_eq!(approved["status"], "approved");
        let entry = approved["entry_id"].as_str().unwrap().to_owned();

        // The retried add finds the first row; nothing new is recorded.
        assert_eq!(site.json(&add)["id"], id.as_str());
        let billing = site.json(&["billing"]);
        let time = billing["project_time"].as_array().unwrap();
        assert_eq!(time.len(), 1, "{billing:#}");
        assert_eq!(time[0]["id"], entry.as_str());
        assert_eq!(time[0]["minutes"], 18);
        assert_eq!(time[0]["backfilled"], true);
        assert_eq!(time[0]["status"], "unbilled");
        let text = site.navigator(&["billing"]).assert().success();
        let text = String::from_utf8_lossy(&text.get_output().stdout).into_owned();
        assert!(
            text.contains("18 min") && text.contains("backfilled"),
            "{text}"
        );

        let unbilled = site.json(&["time", "list", "--status", "unbilled"]);
        assert_eq!(unbilled.as_array().unwrap().len(), 1);
        site.navigator(&["time", "edit", &id, "--minutes", "6"])
            .assert()
            .failure()
            .stderr(predicates::str::contains("only a pending"));
        site.navigator(&[
            "time",
            "add",
            "--performed-on",
            "2999-01-01",
            "--minutes",
            "6",
            "--narrative",
            "Future",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("future"));
    })
    .await;
}

async fn seed_record(site: &LiveSite) {
    let project = site.project.id;
    let now = Utc::now();
    for (channel, direction, subject, call) in [
        (
            "call",
            "internal",
            Some("Synthetic strategy call"),
            Some(
                serde_json::json!({"direction": "outbound", "duration_minutes": 25, "recorded": "no"}),
            ),
        ),
        (
            "email_outbound",
            "outbound",
            Some("Synthetic status letter"),
            None,
        ),
        ("email_inbound", "inbound", Some("Synthetic reply"), None),
    ] {
        sqlx::query(
            "INSERT INTO communication (id,project_id,channel,direction,author_person_id,subject,body,
             occurred_at,call,inserted_at,updated_at) VALUES ($1,$2,$3,$4,$5,$6,'Synthetic body',$7,$8,$7,$7)",
        )
        .bind(Uuid::new_v4())
        .bind(project)
        .bind(channel)
        .bind(direction)
        .bind(site.lawyer)
        .bind(subject)
        .bind(now - Duration::days(3))
        .bind(call)
        .execute(site.pg.pool())
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO asset (storage_key,content_type,byte_size,sha256_hex,project_id,filename,kind,source,slug,inserted_at)
         VALUES ('synthetic/answer','application/pdf',4,$2,$1,'answer.pdf','pleading','upload','2026.11.20_answer.pdf',$3)",
    )
    .bind(project)
    .bind("1".repeat(64))
    .bind(now - Duration::days(4))
    .execute(site.pg.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO project_deadlines (id,project_id,due_at,authority_kind,authority,source,status,
         calendar_role,inserted_at,updated_at,title) VALUES ($1,$2,$3,'internal','Synthetic rule','lawyer','complete','firm',$3,$3,'Synthetic answer due')",
    )
    .bind(Uuid::new_v4())
    .bind(project)
    .bind(now - Duration::days(2))
    .execute(site.pg.pool())
    .await
    .unwrap();
}

fn sources(value: &Value) -> Vec<String> {
    let mut sources = value["proposals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|proposal| proposal["source"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    sources.sort();
    sources
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backfill_previews_writes_and_never_repeats_a_source() {
    store::test_support::with_postgres(async {
        let site = LiveSite::start(None, None).await;
        seed_record(&site).await;

        let preview = site.json(&["time", "backfill", "--dry-run"]);
        assert_eq!(
            sources(&preview),
            [
                "communication",
                "communication",
                "communication",
                "deadline",
                "document"
            ]
        );
        assert!(preview["default_minutes"].as_array().unwrap().len() >= 5);
        let text = site
            .navigator(&["time", "backfill", "--dry-run"])
            .assert()
            .success();
        let text = String::from_utf8_lossy(&text.get_output().stdout).into_owned();
        assert!(
            text.contains("Default minutes") && text.contains("Would propose: 5"),
            "{text}"
        );
        assert!(site.json(&["time", "list"]).as_array().unwrap().is_empty());

        let written = site.json(&["time", "backfill"]);
        assert_eq!(written["proposals"].as_array().unwrap().len(), 5);
        let pending = site.json(&["time", "list", "--status", "pending-review"]);
        assert_eq!(pending.as_array().unwrap().len(), 5);
        assert!(pending
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["status"] == "pending_review" && row["source_id"].is_string()));
        assert!(site
            .json(&["time", "list", "--status", "approved"])
            .as_array()
            .unwrap()
            .is_empty());

        let again = site.json(&["time", "backfill"]);
        assert!(
            again["proposals"].as_array().unwrap().is_empty(),
            "{again:#}"
        );
        let billing = site.json(&["billing"]);
        assert!(billing["needs_my_action"]
            .as_array()
            .unwrap()
            .iter()
            .any(|action| action["pending_proposals"] == 5));
    })
    .await;
}
