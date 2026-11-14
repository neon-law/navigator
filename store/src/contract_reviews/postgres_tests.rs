//! Opt-in PostgreSQL copies follow successful Contract Review commands.

use super::*;
use crate::postgres_mirror::tests::fresh_schema;
use chrono::{DateTime, SubsecRound, Utc};
use serde_json::Value;
use sqlx::Row;
use tokio::process::Command;
use uuid::Uuid;

const PROBE: &str = "NAV_CONTRACT_REVIEW_MIRROR_PROBE";
const TEST: &str = "contract_reviews::postgres_tests::lifecycle_probe";

#[tokio::test]
async fn command_lifecycle_is_mirrored_in_an_isolated_process() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST, "--nocapture"])
        .env(PROBE, "connected")
        .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
        .env("NAVIGATOR_POSTGRES_PORT", options.get_port().to_string())
        .env("NAVIGATOR_POSTGRES_USER", "postgres")
        .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
        .env("NAVIGATOR_POSTGRES_DATABASE", database)
        .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
        .env_remove("NAVIGATOR_POSTGRES_ROOT_CERT")
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(&format!("test {TEST} ... ok")));
}

async fn assert_projection(pg: &crate::postgres::PostgresDb, source: &ContractReview) {
    let row = sqlx::query("SELECT * FROM contract_review WHERE id=$1")
        .bind(source.id)
        .fetch_optional(&pg.0)
        .await
        .unwrap()
        .expect("the command must mirror its committed review before returning");
    assert_eq!(row.get::<Uuid, _>("notation_id"), source.notation_id);
    assert_eq!(row.get::<Uuid, _>("playbook_id"), source.playbook_id);
    assert_eq!(row.get::<Option<Uuid>, _>("asset_id"), source.asset_id);
    assert_eq!(row.get::<Value, _>("findings"), source.findings);
    assert_eq!(row.get::<String, _>("status"), source.status);
    assert_eq!(
        row.get::<Option<String>, _>("risk_summary"),
        source.risk_summary
    );
    assert_eq!(
        row.get::<DateTime<Utc>, _>("inserted_at"),
        source.inserted_at.trunc_subsecs(6)
    );
    assert_eq!(
        row.get::<DateTime<Utc>, _>("updated_at"),
        source.updated_at.trunc_subsecs(6)
    );
}

async fn review_fixture(surreal: &crate::surreal::SurrealDb) -> NewContractReview {
    let notation_id = crate::test_support::seed_notation(surreal).await;
    let entity_id = crate::test_support::seed_entity(surreal).await;
    let playbook_id = crate::playbooks::create(
        surreal,
        &crate::playbooks::NewPlaybook {
            entity_id,
            name: "Synthetic review playbook",
            positions: &[],
        },
    )
    .await
    .unwrap();
    NewContractReview {
        notation_id,
        playbook_id,
        asset_id: None,
    }
}

#[tokio::test]
async fn lifecycle_probe() {
    let mode = std::env::var(PROBE);
    if mode.as_deref() == Ok("unavailable") {
        source_commands_survive_mirror_outage().await;
        return;
    }
    if mode.as_deref() != Ok("connected") {
        return;
    }
    let pg = crate::postgres::PostgresDb::from_env().await.unwrap();
    let surreal = crate::test_support::mem_surreal().await;
    let first = review_fixture(&surreal).await;
    let mut second = review_fixture(&surreal).await;
    second.asset_id =
        Some(crate::test_support::seed_body_asset(&surreal, b"Synthetic contract").await);
    let id = create(&surreal, &first).await.unwrap();
    let other = create(&surreal, &second).await.unwrap();
    let original = by_id(&surreal, id).await.unwrap().unwrap();
    assert_projection(&pg, &original).await;
    let other_original = by_id(&surreal, other).await.unwrap().unwrap();
    assert_projection(&pg, &other_original).await;
    let mut findings = vec![Finding {
        clause_ref: "Synthetic section".into(),
        deviation: "Synthetic deviation".into(),
        severity: crate::playbooks::SEVERITY_HIGH.into(),
        suggested_redline: Some("Synthetic revision".into()),
        attorney_note: None,
        accepted: false,
    }];
    record_analysis(&surreal, id, "Synthetic analysis", &findings)
        .await
        .unwrap();
    let analyzed = by_id(&surreal, id).await.unwrap().unwrap();
    assert_projection(&pg, &analyzed).await;
    assert_eq!(analyzed.status, STATUS_ANALYZED);
    assert_eq!(analyzed.inserted_at, original.inserted_at);
    findings[0].attorney_note = Some("Synthetic attorney note".into());
    findings[0].accepted = true;
    update_findings(&surreal, id, &findings).await.unwrap();
    assert_projection(&pg, &by_id(&surreal, id).await.unwrap().unwrap()).await;
    update_risk_summary(&surreal, id, "Synthetic edited summary")
        .await
        .unwrap();
    assert_projection(&pg, &by_id(&surreal, id).await.unwrap().unwrap()).await;
    set_status(&surreal, id, STATUS_APPROVED).await.unwrap();
    let approved = by_id(&surreal, id).await.unwrap().unwrap();
    assert_projection(&pg, &approved).await;
    assert_eq!(approved.status, STATUS_APPROVED);
    assert_eq!(approved.inserted_at, original.inserted_at);
    assert_projection(&pg, &other_original).await;
    assert_eq!(
        latest_for_notation(&surreal, first.notation_id)
            .await
            .unwrap()
            .unwrap()
            .id,
        id
    );
    assert_eq!(
        latest_for_notation(&surreal, second.notation_id)
            .await
            .unwrap()
            .unwrap()
            .id,
        other
    );
    let missing = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO contract_review (id, notation_id, playbook_id, asset_id, status, risk_summary, findings, inserted_at, updated_at) \
         SELECT $2, notation_id, playbook_id, asset_id, status, risk_summary, findings, inserted_at, updated_at FROM contract_review WHERE id=$1",
    ).bind(approved.id).bind(missing).execute(&pg.0).await.unwrap();
    let mut sentinel = approved.clone();
    sentinel.id = missing;
    assert!(
        matches!(record_analysis(&surreal, missing, "Synthetic", &findings).await, Err(ContractReviewError::NotFound(found)) if found == missing)
    );
    assert!(
        matches!(update_findings(&surreal, missing, &findings).await, Err(ContractReviewError::NotFound(found)) if found == missing)
    );
    assert!(
        matches!(update_risk_summary(&surreal, missing, "Synthetic").await, Err(ContractReviewError::NotFound(found)) if found == missing)
    );
    assert!(
        matches!(set_status(&surreal, missing, STATUS_APPROVED).await, Err(ContractReviewError::NotFound(found)) if found == missing)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM contract_review")
            .fetch_one(&pg.0)
            .await
            .unwrap(),
        3
    );
    assert!(by_id(&surreal, missing).await.unwrap().is_none());
    assert_projection(&pg, &sentinel).await;
    assert_projection(&pg, &approved).await;
    pg.0.close().await;
}

#[tokio::test]
async fn unavailable_mirror_does_not_change_successful_source_writes() {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST, "--nocapture"])
        .env(PROBE, "unavailable")
        .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
        .env("NAVIGATOR_POSTGRES_PORT", "1")
        .env("NAVIGATOR_POSTGRES_USER", "synthetic")
        .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
        .env("NAVIGATOR_POSTGRES_DATABASE", "synthetic")
        .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
        .env_remove("NAVIGATOR_POSTGRES_ROOT_CERT")
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(&format!("test {TEST} ... ok")));
}

async fn source_commands_survive_mirror_outage() {
    let surreal = crate::test_support::mem_surreal().await;
    let new = NewContractReview {
        notation_id: Uuid::new_v4(),
        playbook_id: Uuid::new_v4(),
        asset_id: None,
    };
    let id = create(&surreal, &new).await.unwrap();
    let original = by_id(&surreal, id).await.unwrap().unwrap();
    record_analysis(&surreal, id, "Synthetic analysis", &[])
        .await
        .unwrap();
    update_findings(&surreal, id, &[]).await.unwrap();
    update_risk_summary(&surreal, id, "Synthetic edited summary")
        .await
        .unwrap();
    set_status(&surreal, id, STATUS_REJECTED).await.unwrap();
    let final_row = by_id(&surreal, id).await.unwrap().unwrap();
    assert_eq!(final_row.id, id);
    assert_eq!(final_row.notation_id, new.notation_id);
    assert_eq!(final_row.playbook_id, new.playbook_id);
    assert_eq!(final_row.status, STATUS_REJECTED);
    assert_eq!(
        final_row.risk_summary.as_deref(),
        Some("Synthetic edited summary")
    );
    assert_eq!(final_row.inserted_at, original.inserted_at);
    assert!(findings_of(&final_row).unwrap().is_empty());
}
