use super::*;
use crate::{
    leads::{self, LeadStatus, NewConsentEvent, NewLead},
    postgres_mirror::tests::{connect, fresh_schema, test_postgres},
    test_support::mem_surreal,
};
use chrono::{TimeZone, Utc};
use std::process::Command;

#[test]
fn lead_mirror_keys_keep_source_ids_and_table_names() {
    let id = Uuid::now_v7();
    assert_eq!(MirrorKey::Lead(id).table(), "lead");
    assert_eq!(MirrorKey::LeadConsent(id).table(), "lead_consent");
    assert_eq!(
        MirrorKey::LeadConsent(id).to_string(),
        format!("lead_consent:{id}")
    );
}

fn new_lead(email: &str) -> NewLead {
    NewLead {
        email: email.into(),
        phone: None,
        brand_key: "synthetic-brand".into(),
        source_path: "/contact".into(),
        consent_version: "email-v1".into(),
        consented_at: Utc.with_ymd_and_hms(2026, 10, 17, 12, 0, 0).unwrap(),
    }
}

fn event(at: DateTime<Utc>, phone: Option<&str>, opt_in: bool) -> NewConsentEvent {
    NewConsentEvent {
        consented_at: at,
        phone: phone.map(str::to_owned),
        source_path: "/contact".into(),
        sms_consent_version: "sms-v1".into(),
        sms_policy_version: "policy-v3".into(),
        sms_opt_in: opt_in,
    }
}

#[tokio::test]
async fn consent_uuid_ties_match_postgres_without_rewriting_source_instants() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let lead = leads::record(&surreal, &new_lead("history@example.test"))
        .await
        .unwrap();
    assert_eq!(lead.id.get_version_num(), 4);
    mirror(&surreal, &pg, &MirrorKey::Lead(lead.id))
        .await
        .unwrap();
    let older_id = Uuid::parse_str("ffffffff-ffff-7fff-bfff-ffffffffffff").unwrap();
    let newer_id = Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
    let older_at = "2026-10-21T12:00:00.123456100Z"
        .parse::<DateTime<Utc>>()
        .unwrap();
    let newer_at = "2026-10-21T12:00:00.123456900Z"
        .parse::<DateTime<Utc>>()
        .unwrap();
    for (id, at) in [(older_id, older_at), (newer_id, newer_at)] {
        surreal.query("CREATE $id SET lead_id=$lead, consented_at=$at, phone=NONE, source_path='/contact', sms_consent_version='synthetic-disclosure', sms_policy_version='synthetic-policy', sms_opt_in=true")
            .bind(("id", crate::surreal::record_id("lead_consent", id)))
            .bind(("lead", crate::surreal::record_id("lead", lead.id)))
            .bind(("at", surrealdb::types::Datetime::from(at)))
            .await.unwrap().check().unwrap();
        let key = MirrorKey::LeadConsent(id);
        assert_eq!(
            mirror(&surreal, &pg, &key).await.unwrap(),
            Mirrored::Written
        );
        assert_eq!(
            mirror(&surreal, &pg, &key).await.unwrap(),
            Mirrored::Unchanged
        );
    }
    let source = leads::consent_history(&surreal, lead.id).await.unwrap();
    let stored: Vec<(Uuid, DateTime<Utc>)> = sqlx::query_as(
        "SELECT id, consented_at FROM lead_consent WHERE lead_id=$1 ORDER BY consented_at, id",
    )
    .bind(lead.id)
    .fetch_all(&pg.0)
    .await
    .unwrap();
    assert_eq!(
        source.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![newer_id, older_id]
    );
    assert_eq!(
        source.iter().map(|row| row.id).collect::<Vec<_>>(),
        stored.iter().map(|row| row.0).collect::<Vec<_>>()
    );
    assert_eq!(source[0].consented_at, newer_at);
    assert_eq!(source[1].consented_at, older_at);
    assert_eq!(stored[0].1.timestamp_micros(), newer_at.timestamp_micros());
    assert_eq!(stored[1].1.timestamp_micros(), older_at.timestamp_micros());
    let latest: Uuid = sqlx::query_scalar(
        "SELECT id FROM lead_consent WHERE lead_id=$1 ORDER BY consented_at DESC, id DESC LIMIT 1",
    )
    .bind(lead.id)
    .fetch_one(&pg.0)
    .await
    .unwrap();
    assert_eq!(
        leads::latest_consent_event(&surreal, lead.id)
            .await
            .unwrap()
            .unwrap()
            .id,
        latest
    );
    assert_eq!(latest, older_id);
    assert!(super::parity(&surreal, &pg).await.unwrap().is_clean());
    pg.0.close().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn leads_and_consent_history_mirror_replay_and_reconcile_without_rewriting_evidence() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let first_at = Utc.with_ymd_and_hms(2026, 10, 17, 12, 0, 0).unwrap();
    let mut first = new_lead("visitor@example.test");
    first.phone = Some("(555) 010-4444".into());
    let lead = leads::record_consent_event(
        &surreal,
        &first,
        &event(first_at, first.phone.as_deref(), true),
    )
    .await
    .unwrap();
    let history = leads::consent_history(&surreal, lead.id).await.unwrap();
    let consent_id = history[0].id;
    let lead_key = MirrorKey::Lead(lead.id);
    let consent_key = MirrorKey::LeadConsent(consent_id);

    assert_eq!(
        mirror(&surreal, &pg, &lead_key).await.unwrap(),
        Mirrored::Written
    );
    assert_eq!(
        mirror(&surreal, &pg, &consent_key).await.unwrap(),
        Mirrored::Written
    );
    assert_eq!(
        mirror(&surreal, &pg, &consent_key).await.unwrap(),
        Mirrored::Unchanged
    );
    assert!(super::parity(&surreal, &pg).await.unwrap().is_clean());

    let mut other_brand = new_lead("visitor@example.test");
    other_brand.brand_key = "synthetic-other-brand".into();
    let separate = leads::record(&surreal, &other_brand).await.unwrap();
    assert_ne!(separate.id, lead.id);
    mirror(&surreal, &pg, &MirrorKey::Lead(separate.id))
        .await
        .unwrap();
    let copies: i64 =
        sqlx::query_scalar("SELECT count(*) FROM lead WHERE email_lower='visitor@example.test'")
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(
        copies, 2,
        "email remains a non-unique lookup key across Brands"
    );

    let second_at = first_at + chrono::Duration::microseconds(123_456);
    let mut repeat = new_lead("VISITOR@example.test");
    repeat.phone = Some("(555) 010-5555".into());
    let updated = leads::record_consent_event(
        &surreal,
        &repeat,
        &event(second_at, repeat.phone.as_deref(), false),
    )
    .await
    .unwrap();
    assert_eq!(updated.id, lead.id);
    assert_eq!(updated.submissions, 2);
    leads::set_status(&surreal, lead.id, LeadStatus::Unsubscribed)
        .await
        .unwrap();
    mirror(&surreal, &pg, &lead_key).await.unwrap();
    let second = leads::consent_history(&surreal, lead.id).await.unwrap();
    assert_eq!(second.len(), 2);
    assert_eq!(second[0].id, consent_id);
    assert_eq!(second[0].consented_at, first_at);
    assert_eq!(second[1].consented_at, second_at);
    assert_eq!(second[0].phone.as_deref(), Some("(555) 010-4444"));
    assert!(!second[1].sms_opt_in);

    sqlx::query("UPDATE lead_consent SET sms_opt_in=false WHERE id=$1")
        .bind(consent_id)
        .execute(&pg.0)
        .await
        .unwrap_err();
    sqlx::query("DROP TRIGGER lead_consent_immutable ON lead_consent")
        .execute(&pg.0)
        .await
        .unwrap();
    sqlx::query("UPDATE lead_consent SET sms_opt_in=false WHERE id=$1")
        .bind(consent_id)
        .execute(&pg.0)
        .await
        .unwrap();
    let report = super::reconcile(&surreal, &pg).await.unwrap();
    assert!(
        !report.is_clean(),
        "changed consent history remains visible to parity"
    );
    let stored: bool = sqlx::query_scalar("SELECT sms_opt_in FROM lead_consent WHERE id=$1")
        .bind(consent_id)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert!(!stored, "reconcile must not overwrite consent evidence");
    assert_eq!(
        mirror(&surreal, &pg, &consent_key).await.unwrap(),
        Mirrored::Unchanged
    );
    let after_replay: bool = sqlx::query_scalar("SELECT sms_opt_in FROM lead_consent WHERE id=$1")
        .bind(consent_id)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert!(
        !after_replay,
        "a replay must preserve changed consent evidence"
    );
    pg.0.close().await;
}

#[tokio::test]
async fn a_consent_event_with_a_missing_source_lead_is_reported_without_a_parent_or_delete() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let missing_lead = Uuid::now_v7();
    let event_id = Uuid::now_v7();
    let consented_at = Utc.with_ymd_and_hms(2026, 10, 17, 12, 0, 0).unwrap()
        + chrono::Duration::microseconds(123_456);
    surreal.query("CREATE $id SET lead_id=$lead, consented_at=$at, phone=NONE, source_path='/contact', sms_consent_version='v1', sms_policy_version='p1', sms_opt_in=true")
        .bind(("id", crate::surreal::record_id("lead_consent", event_id)))
        .bind(("lead", crate::surreal::record_id("lead", missing_lead)))
        .bind(("at", surrealdb::types::Datetime::from(consented_at)))
        .await.unwrap().check().unwrap();
    let consent_key = MirrorKey::LeadConsent(event_id);
    assert!(
        mirror(&surreal, &pg, &consent_key).await.is_err(),
        "a dangling source parent fails closed"
    );
    assert!(super::reconcile(&surreal, &pg)
        .await
        .unwrap()
        .divergent()
        .any(|divergent| divergent == &consent_key));
    let parents: i64 = sqlx::query_scalar("SELECT count(*) FROM lead WHERE id=$1")
        .bind(missing_lead)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(parents, 0, "reconcile does not fabricate a missing Lead");
    pg.0.close().await;
}

#[tokio::test]
async fn reconcile_keeps_extra_consent_events_and_repairs_missing_events() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let lead = leads::record_consent_event(
        &surreal,
        &new_lead("visitor@example.test"),
        &event(Utc::now(), None, true),
    )
    .await
    .unwrap();
    let event = leads::consent_history(&surreal, lead.id)
        .await
        .unwrap()
        .remove(0);
    mirror(&surreal, &pg, &MirrorKey::Lead(lead.id))
        .await
        .unwrap();
    sqlx::query("INSERT INTO lead_consent (id,lead_id,consented_at,phone,source_path,sms_consent_version,sms_policy_version,sms_opt_in) VALUES ($1,$2,$3,NULL,'/old','old','old',false)")
        .bind(Uuid::now_v7()).bind(lead.id).bind(Utc::now()).execute(&pg.0).await.unwrap();
    let historical_lead = Uuid::now_v7();
    sqlx::query("INSERT INTO lead (id,email,email_lower,brand_key,source_path,consent_version,consented_at,status,submissions,inserted_at,updated_at) VALUES ($1,'stale@example.test','stale@example.test','synthetic-brand','/contact','email-v1',$2,'new',1,$2,$2)")
        .bind(historical_lead).bind(Utc::now()).execute(&pg.0).await.unwrap();
    sqlx::query("INSERT INTO lead_consent (id,lead_id,consented_at,source_path,sms_consent_version,sms_policy_version,sms_opt_in) VALUES ($1,$2,$3,'/contact','v1','v1',true)")
        .bind(Uuid::now_v7()).bind(historical_lead).bind(Utc::now()).execute(&pg.0).await.unwrap();
    let report = super::reconcile(&surreal, &pg).await.unwrap();
    assert!(!report.is_clean(), "extra event remains reportable");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM lead WHERE id=$1")
            .bind(historical_lead)
            .fetch_one(&pg.0)
            .await
            .unwrap(),
        0,
        "reconcile removes a stale Lead projection"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM lead_consent WHERE lead_id=$1")
            .bind(historical_lead)
            .fetch_one(&pg.0)
            .await
            .unwrap(),
        1,
        "reconcile never cascades away audit history"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM lead_consent WHERE lead_id=$1")
            .bind(lead.id)
            .fetch_one(&pg.0)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        mirror(&surreal, &pg, &MirrorKey::LeadConsent(event.id))
            .await
            .unwrap(),
        Mirrored::Unchanged
    );
    assert!(super::parity(&surreal, &pg)
        .await
        .unwrap()
        .tables
        .iter()
        .any(|t| t.table == "lead_consent" && !t.divergent.is_empty()));
    pg.0.close().await;
}

#[test]
fn lead_write_hooks_are_isolated_from_disabled_and_unreachable_postgres() {
    let mut cases = vec!["disabled", "unreachable"];
    let database = test_postgres().map(|_| {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(fresh_schema())
            .unwrap()
    });
    if database.is_some() {
        cases.push("mirrored");
    }
    for case in cases {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "postgres_mirror::leads::tests::lead_write_hook_probe",
                "--nocapture",
            ])
            .env("NAVIGATOR_LEAD_HOOK_TEST_CASE", case)
            .env(crate::postgres_mirror::MIRROR_ENV, "1")
            .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
            .env("NAVIGATOR_POSTGRES_PORT", "1")
            .env("NAVIGATOR_POSTGRES_USER", "postgres")
            .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
            .env("NAVIGATOR_POSTGRES_DATABASE", "synthetic")
            .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
            .env_remove("NAVIGATOR_POSTGRES_ROOT_CERT");
        match case {
            "disabled" => {
                child.env_remove("NAVIGATOR_POSTGRES_HOST");
            }
            "mirrored" => {
                let (options, name) = database.as_ref().unwrap();
                child
                    .env("NAVIGATOR_POSTGRES_PORT", options.get_port().to_string())
                    .env("NAVIGATOR_POSTGRES_DATABASE", name);
            }
            _ => {}
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "lead hook case {case}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[tokio::test]
async fn lead_write_hook_probe() {
    let Ok(case) = std::env::var("NAVIGATOR_LEAD_HOOK_TEST_CASE") else {
        return;
    };
    let surreal = mem_surreal().await;
    let at = Utc::now();
    let lead = leads::record(&surreal, &new_lead("hook@example.test"))
        .await
        .unwrap();
    let with_event = leads::record_consent_event(
        &surreal,
        &new_lead("hook@example.test"),
        &event(at, None, true),
    )
    .await
    .unwrap();
    assert_eq!(lead.id, with_event.id);
    let unsubscribed = leads::set_status(&surreal, lead.id, LeadStatus::Unsubscribed)
        .await
        .unwrap();
    assert_eq!(unsubscribed.status, "unsubscribed");
    let converted = leads::convert(&surreal, lead.id).await.unwrap();
    assert_eq!(converted.status, "converted");
    let linked_person = crate::persons::create(
        &surreal,
        &crate::persons::NewPerson::new("Existing", "linked@example.test"),
    )
    .await
    .unwrap();
    let linked_lead = leads::record(&surreal, &new_lead("linked@example.test"))
        .await
        .unwrap();
    let linked = leads::link_person(&surreal, linked_lead.id).await.unwrap();
    assert_eq!(linked.person_id, Some(linked_person.id));
    if case == "mirrored" {
        let port: u16 = std::env::var("NAVIGATOR_POSTGRES_PORT")
            .unwrap()
            .parse()
            .unwrap();
        let options = sqlx::postgres::PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port)
            .username("postgres")
            .password("synthetic")
            .ssl_mode(sqlx::postgres::PgSslMode::Disable);
        let pg = connect(
            &options,
            &std::env::var("NAVIGATOR_POSTGRES_DATABASE").unwrap(),
        )
        .await;
        let stored: (i64, String, i64, Option<Uuid>) = sqlx::query_as("SELECT l.submissions,l.status,(SELECT count(*) FROM lead_consent c WHERE c.lead_id=l.id),l.person_id FROM lead l WHERE l.id=$1")
            .bind(lead.id).fetch_one(&pg.0).await.unwrap();
        assert_eq!(stored, (2, "converted".into(), 1, converted.person_id));
        let linked_status: (String, Option<Uuid>) =
            sqlx::query_as("SELECT status,person_id FROM lead WHERE id=$1")
                .bind(linked_lead.id)
                .fetch_one(&pg.0)
                .await
                .unwrap();
        assert_eq!(linked_status, ("converted".into(), Some(linked_person.id)));
        assert!(super::parity(&surreal, &pg).await.unwrap().is_clean());
        pg.0.close().await;
    }
}
