use super::*;
use crate::postgres_mirror::tests::{connect, fresh_schema};

async fn captured_lead(source: &SurrealDb, lead: Uuid) {
    source.query("CREATE $id SET email='synthetic@example.invalid',brand_key='synthetic',source_path='/synthetic',consent_version='v1',consented_at=$at")
        .bind(("id", crate::surreal::record_id("lead",lead)))
        .bind(("at", Datetime::from("2026-10-21T12:00:00.123456100Z".parse::<DateTime<Utc>>().unwrap())))
        .await.unwrap().check().unwrap();
}

async fn captured_consent(source: &SurrealDb, lead: Uuid, id: Uuid, at: &str) {
    source.query("CREATE $id SET lead_id=$lead,consented_at=$at,source_path='/synthetic',sms_consent_version='sms-v1',sms_policy_version='policy-v1',sms_opt_in=true")
        .bind(("id", crate::surreal::record_id("lead_consent",id)))
        .bind(("lead",crate::surreal::record_id("lead",lead)))
        .bind(("at",Datetime::from(at.parse::<DateTime<Utc>>().unwrap())))
        .await.unwrap().check().unwrap();
}

#[tokio::test]
async fn captured_lead_consent_import_preserves_native_drift_and_missing_source_rows() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let source = crate::surreal::test_support::mem().await;
    let lead = Uuid::now_v7();
    let consent = Uuid::now_v7();
    captured_lead(&source, lead).await;
    captured_consent(&source, lead, consent, "2026-10-21T12:00:00.123456900Z").await;
    super::super::during_initial_copy(async {
        for key in [MirrorKey::Lead(lead), MirrorKey::LeadConsent(consent)] {
            assert_eq!(mirror(&source, &pg, &key).await.unwrap(), Mirrored::Written);
            assert_eq!(
                mirror(&source, &pg, &key).await.unwrap(),
                Mirrored::Unchanged
            );
        }
        assert!(parity(&source, &pg).await.unwrap().is_clean());
        sqlx::query("UPDATE lead SET status='unsubscribed' WHERE id=$1")
            .bind(lead)
            .execute(pg.pool())
            .await
            .unwrap();
        assert!(!reconcile(&source, &pg).await.unwrap().is_clean());
        source
            .query("DELETE $id")
            .bind(("id", crate::surreal::record_id("lead", lead)))
            .await
            .unwrap()
            .check()
            .unwrap();
        assert!(!reconcile(&source, &pg).await.unwrap().is_clean());
        let status: String = sqlx::query_scalar("SELECT status FROM lead WHERE id=$1")
            .bind(lead)
            .fetch_one(pg.pool())
            .await
            .unwrap();
        assert_eq!(status, "unsubscribed");
        let at: DateTime<Utc> =
            sqlx::query_scalar("SELECT consented_at FROM lead_consent WHERE id=$1")
                .bind(consent)
                .fetch_one(pg.pool())
                .await
                .unwrap();
        assert_eq!(at.timestamp_subsec_nanos(), 123_456_000);
        sqlx::query("UPDATE lead_consent SET sms_opt_in=false WHERE id=$1")
            .bind(consent)
            .execute(pg.pool())
            .await
            .unwrap_err();
    })
    .await;
    pg.pool().close().await;
}

#[tokio::test]
async fn consent_precision_ties_keep_uuid_order_and_missing_parent_cannot_create_evidence() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let source = crate::surreal::test_support::mem().await;
    let lead = Uuid::now_v7();
    captured_lead(&source, lead).await;
    let early = Uuid::parse_str("ffffffff-ffff-7fff-bfff-ffffffffffff").unwrap();
    let late = Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
    captured_consent(&source, lead, early, "2026-10-21T12:00:00.123456100Z").await;
    captured_consent(&source, lead, late, "2026-10-21T12:00:00.123456900Z").await;
    super::super::during_initial_copy(async {
        for id in [early, late] {
            mirror(&source, &pg, &MirrorKey::LeadConsent(id))
                .await
                .unwrap();
        }
        let stored: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM lead_consent WHERE lead_id=$1 ORDER BY consented_at,id",
        )
        .bind(lead)
        .fetch_all(pg.pool())
        .await
        .unwrap();
        assert_eq!(stored, vec![late, early]);
        let repaired = Uuid::now_v7();
        captured_consent(&source,lead,repaired,"2026-10-22T12:00:00Z").await;
        assert!(reconcile(&source,&pg).await.unwrap().is_clean());
        let extra=Uuid::now_v7();
        sqlx::query("INSERT INTO lead_consent(id,lead_id,consented_at,source_path,sms_consent_version,sms_policy_version,sms_opt_in) VALUES($1,$2,now(),'/native','native-v1','native-policy',false)")
            .bind(extra).bind(lead).execute(pg.pool()).await.unwrap();
        assert!(!reconcile(&source,&pg).await.unwrap().is_clean());
        let retained:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM lead_consent WHERE id=$1)")
            .bind(extra).fetch_one(pg.pool()).await.unwrap();
        assert!(retained);

        let orphan = Uuid::now_v7();
        captured_consent(&source, Uuid::now_v7(), orphan, "2026-10-21T12:00:00Z").await;
        assert!(matches!(
            mirror(&source, &pg, &MirrorKey::LeadConsent(orphan)).await,
            Err(MirrorError::Source)
        ));
        let present: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM lead_consent WHERE id=$1)")
                .bind(orphan)
                .fetch_one(pg.pool())
                .await
                .unwrap();
        assert!(!present);
    })
    .await;
    pg.pool().close().await;
}
