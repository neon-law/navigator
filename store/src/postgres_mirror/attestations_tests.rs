use super::*;
use crate::postgres_mirror::attestations;
use crate::{
    attestations::{
        self as source, NewAttestation, STATUS_FAILED, STATUS_PENDING, STATUS_RECORDED,
    },
    postgres_mirror::tests::{connect, fresh_schema},
    test_support::mem_surreal,
};
use chrono::{DateTime, TimeZone, Utc};
use std::process::Command;

fn new_attestation<'a>(
    notation_id: Uuid,
    status: &'a str,
    recorded_at: Option<&'a str>,
) -> NewAttestation<'a> {
    NewAttestation {
        notation_id,
        chain: "solana",
        sha256: "synthetic-sha256",
        status,
        pda: Some("synthetic-pda"),
        tx_signature: Some("synthetic-signature"),
        firm_wallet: Some("synthetic-firm-wallet"),
        client_wallet: Some("synthetic-client-wallet"),
        recorded_at,
    }
}

async fn required_pg() -> Option<(crate::postgres::PostgresDb, crate::surreal::SurrealDb)> {
    let (options, database) = fresh_schema().await?;
    Some((connect(&options, &database).await, mem_surreal().await))
}

async fn notation_fixture(
    pg: &crate::postgres::PostgresDb,
    surreal: &crate::surreal::SurrealDb,
) -> Uuid {
    let notation_id = crate::test_support::seed_notation(surreal).await;
    let notation = crate::notations::find_by_id(surreal, notation_id)
        .await
        .unwrap()
        .unwrap();
    let project = crate::projects::find_by_id(surreal, notation.project_id)
        .await
        .unwrap()
        .unwrap();
    crate::postgres_mirror::tests::mirror_identity(pg, &[project.entity_id], &[notation.person_id])
        .await;
    notation_id
}

#[test]
fn attestation_key_keeps_the_source_id_and_table_name() {
    let id = Uuid::new_v4();
    let key = MirrorKey::Attestation(id);
    assert_eq!(key.table(), "attestation");
    assert_eq!(key.to_string(), format!("attestation:{id}"));
}

#[tokio::test]
async fn status_updates_replay_with_source_id_and_insertion_time() {
    let Some((pg, surreal)) = required_pg().await else {
        return;
    };
    let notation_id = notation_fixture(&pg, &surreal).await;
    crate::postgres_mirror::mirror(&surreal, &pg, &MirrorKey::Notation(notation_id))
        .await
        .unwrap();

    let pending = source::record(
        &surreal,
        &new_attestation(notation_id, STATUS_PENDING, None),
    )
    .await
    .unwrap();
    assert_eq!(pending.id.get_version_num(), 4);
    let key = MirrorKey::Attestation(pending.id);
    assert_eq!(
        crate::postgres_mirror::mirror(&surreal, &pg, &key)
            .await
            .unwrap(),
        Mirrored::Written
    );
    assert_eq!(
        crate::postgres_mirror::mirror(&surreal, &pg, &key)
            .await
            .unwrap(),
        Mirrored::Unchanged
    );
    let inserted_at = pending.inserted_at;
    let target_inserted_at: DateTime<Utc> =
        sqlx::query_scalar("SELECT inserted_at FROM attestation WHERE id=$1")
            .bind(pending.id)
            .fetch_one(&pg.0)
            .await
            .unwrap();

    let recorded_at = "2026-10-21T02:30:00.123456789+00:00";
    let recorded = source::record(
        &surreal,
        &new_attestation(notation_id, STATUS_RECORDED, Some(recorded_at)),
    )
    .await
    .unwrap();
    assert_eq!(recorded.id, pending.id);
    assert_eq!(recorded.inserted_at, inserted_at);
    crate::postgres_mirror::mirror(&surreal, &pg, &key)
        .await
        .unwrap();
    let replayed_inserted_at: DateTime<Utc> =
        sqlx::query_scalar("SELECT inserted_at FROM attestation WHERE id=$1")
            .bind(pending.id)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(replayed_inserted_at, target_inserted_at);
    let stored_recorded_at: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT recorded_at FROM attestation WHERE id=$1")
            .bind(key_id(&key))
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(
        stored_recorded_at,
        Some(
            Utc.with_ymd_and_hms(2026, 10, 21, 2, 30, 0).unwrap()
                + chrono::Duration::microseconds(123_456)
        )
    );
    assert!(attestations::parity(&surreal, &pg)
        .await
        .unwrap()
        .is_clean());

    let failed = source::record(&surreal, &new_attestation(notation_id, STATUS_FAILED, None))
        .await
        .unwrap();
    assert_eq!(failed.id, pending.id);
    assert_eq!(failed.inserted_at, inserted_at);
    crate::postgres_mirror::mirror(&surreal, &pg, &key)
        .await
        .unwrap();
    assert!(attestations::parity(&surreal, &pg)
        .await
        .unwrap()
        .is_clean());
    pg.0.close().await;
}

#[tokio::test]
async fn parity_repairs_changed_rows_and_removes_target_only_rows() {
    let Some((pg, surreal)) = required_pg().await else {
        return;
    };
    let notation_id = notation_fixture(&pg, &surreal).await;
    crate::postgres_mirror::mirror(&surreal, &pg, &MirrorKey::Notation(notation_id))
        .await
        .unwrap();
    let attestation = source::record(
        &surreal,
        &new_attestation(notation_id, STATUS_RECORDED, Some("2026-10-21T02:30:00Z")),
    )
    .await
    .unwrap();
    let key = MirrorKey::Attestation(attestation.id);
    crate::postgres_mirror::mirror(&surreal, &pg, &key)
        .await
        .unwrap();

    sqlx::query("UPDATE attestation SET status='failed' WHERE id=$1")
        .bind(attestation.id)
        .execute(&pg.0)
        .await
        .unwrap();
    let stale_id = Uuid::new_v4();
    sqlx::query("INSERT INTO attestation (id,notation_id,chain,sha256,status,inserted_at,updated_at) VALUES ($1,$2,'solana','stale','failed',now(),now())")
        .bind(stale_id)
        .bind(notation_id)
        .execute(&pg.0)
        .await
        .unwrap();

    let drift = attestations::parity(&surreal, &pg).await.unwrap();
    assert_eq!(drift.tables[0].source_rows, 1);
    assert_eq!(drift.tables[0].target_rows, 2);
    let mut expected = [
        MirrorKey::Attestation(attestation.id),
        MirrorKey::Attestation(stale_id),
    ];
    expected.sort();
    assert_eq!(drift.tables[0].divergent, expected);
    let repaired = attestations::reconcile(&surreal, &pg).await.unwrap();
    assert!(repaired.is_clean(), "{repaired:?}");
    let status: String = sqlx::query_scalar("SELECT status FROM attestation WHERE id=$1")
        .bind(attestation.id)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(status, STATUS_RECORDED);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM attestation WHERE id=$1")
            .bind(stale_id)
            .fetch_one(&pg.0)
            .await
            .unwrap(),
        0
    );

    surreal
        .query("DELETE $id")
        .bind((
            "id",
            crate::surreal::record_id("attestation", attestation.id),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();
    let deleted = attestations::reconcile(&surreal, &pg).await.unwrap();
    assert!(deleted.is_clean(), "{deleted:?}");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM attestation WHERE id=$1")
            .bind(attestation.id)
            .fetch_one(&pg.0)
            .await
            .unwrap(),
        0
    );
    pg.0.close().await;
}

#[tokio::test]
async fn malformed_recorded_timestamp_is_reported_without_inventing_a_value() {
    let Some((pg, surreal)) = required_pg().await else {
        return;
    };
    let notation_id = notation_fixture(&pg, &surreal).await;
    crate::postgres_mirror::mirror(&surreal, &pg, &MirrorKey::Notation(notation_id))
        .await
        .unwrap();
    let attestation = source::record(
        &surreal,
        &new_attestation(notation_id, STATUS_PENDING, None),
    )
    .await
    .unwrap();
    let key = MirrorKey::Attestation(attestation.id);
    crate::postgres_mirror::mirror(&surreal, &pg, &key)
        .await
        .unwrap();
    surreal
        .query("UPDATE $id SET recorded_at='not-an-rfc3339-timestamp'")
        .bind((
            "id",
            crate::surreal::record_id("attestation", attestation.id),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();

    assert!(crate::postgres_mirror::mirror(&surreal, &pg, &key)
        .await
        .is_err());
    assert!(attestations::parity(&surreal, &pg).await.is_err());
    let stored: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT recorded_at FROM attestation WHERE id=$1")
            .bind(attestation.id)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(stored, None);
    pg.0.close().await;
}

#[tokio::test]
async fn notation_deletion_is_restricted_and_orphan_attestation_stays_visible() {
    let Some((pg, surreal)) = required_pg().await else {
        return;
    };
    let notation_id = notation_fixture(&pg, &surreal).await;
    crate::postgres_mirror::mirror(&surreal, &pg, &MirrorKey::Notation(notation_id))
        .await
        .unwrap();
    let attestation = source::record(
        &surreal,
        &new_attestation(notation_id, STATUS_RECORDED, Some("2026-10-21T02:30:00Z")),
    )
    .await
    .unwrap();
    let key = MirrorKey::Attestation(attestation.id);
    crate::postgres_mirror::mirror(&surreal, &pg, &key)
        .await
        .unwrap();

    let deletion = sqlx::query("DELETE FROM notation WHERE id=$1")
        .bind(notation_id)
        .execute(&pg.0)
        .await;
    assert!(matches!(
        &deletion,
        Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("23503")
    ));

    assert_eq!(
        crate::notation_reset::delete_draft(&surreal, notation_id)
            .await
            .unwrap(),
        crate::notation_reset::ResetOutcome::Deleted
    );
    let drift = crate::postgres_mirror::parity_all(&surreal, &pg)
        .await
        .unwrap();
    let notation_drift = drift
        .iter()
        .flat_map(|mirror| &mirror.report.tables)
        .find(|table| table.table == "notation")
        .unwrap();
    assert_eq!(notation_drift.divergent, [MirrorKey::Notation(notation_id)]);
    let attestation_drift = drift
        .iter()
        .flat_map(|mirror| &mirror.report.tables)
        .find(|table| table.table == "attestation")
        .unwrap();
    assert_eq!(attestation_drift.divergent, std::slice::from_ref(&key));
    let after = crate::postgres_mirror::reconcile_all(&surreal, &pg)
        .await
        .unwrap();
    let after_notation = after
        .iter()
        .flat_map(|mirror| &mirror.report.tables)
        .find(|table| table.table == "notation")
        .unwrap();
    let after_attestation = after
        .iter()
        .flat_map(|mirror| &mirror.report.tables)
        .find(|table| table.table == "attestation")
        .unwrap();
    assert_eq!(after_notation.divergent, [MirrorKey::Notation(notation_id)]);
    assert_eq!(after_attestation.divergent, std::slice::from_ref(&key));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM attestation WHERE id=$1")
            .bind(attestation.id)
            .fetch_one(&pg.0)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM notation WHERE id=$1")
            .bind(notation_id)
            .fetch_one(&pg.0)
            .await
            .unwrap(),
        1
    );
    pg.0.close().await;
}

/// Run the opt-in process hook in a child so its environment and pool do not
/// leak into the other parallel store tests.
#[test]
fn source_reset_hook_and_global_reconcile_are_isolated() {
    let Some((options, database)) = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(fresh_schema())
    else {
        return;
    };
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "postgres_mirror::attestations::tests::source_reset_hook_probe",
            "--nocapture",
        ])
        .env("NAV_ATTESTATION_RESET_HOOK_PROBE", "1")
        .env(crate::postgres_mirror::MIRROR_ENV, "1")
        .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
        .env("NAVIGATOR_POSTGRES_PORT", options.get_port().to_string())
        .env("NAVIGATOR_POSTGRES_USER", "postgres")
        .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
        .env("NAVIGATOR_POSTGRES_DATABASE", database)
        .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
        .env_remove("NAVIGATOR_POSTGRES_ROOT_CERT")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "source reset hook probe: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("test postgres_mirror::attestations::tests::source_reset_hook_probe ... ok"),
        "source reset hook probe did not run: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn source_reset_hook_probe() {
    if std::env::var_os("NAV_ATTESTATION_RESET_HOOK_PROBE").is_none() {
        return;
    }
    let pg = crate::postgres::PostgresDb::from_env().await.unwrap();
    let surreal = mem_surreal().await;
    let notation_id = notation_fixture(&pg, &surreal).await;
    crate::postgres_mirror::mirror(&surreal, &pg, &MirrorKey::Notation(notation_id))
        .await
        .unwrap();
    let attestation = source::record(
        &surreal,
        &new_attestation(notation_id, STATUS_RECORDED, Some("2026-10-21T02:30:00Z")),
    )
    .await
    .unwrap();
    let key = MirrorKey::Attestation(attestation.id);
    crate::postgres_mirror::mirror(&surreal, &pg, &key)
        .await
        .unwrap();

    assert_eq!(
        crate::notation_reset::delete_draft(&surreal, notation_id)
            .await
            .unwrap(),
        crate::notation_reset::ResetOutcome::Deleted
    );
    let report = crate::postgres_mirror::reconcile_all(&surreal, &pg)
        .await
        .unwrap();
    let notation_drift = report
        .iter()
        .flat_map(|mirror| &mirror.report.tables)
        .find(|table| table.table == "notation")
        .unwrap();
    let attestation_drift = report
        .iter()
        .flat_map(|mirror| &mirror.report.tables)
        .find(|table| table.table == "attestation")
        .unwrap();
    assert_eq!(notation_drift.divergent, [MirrorKey::Notation(notation_id)]);
    assert_eq!(attestation_drift.divergent, std::slice::from_ref(&key));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM attestation WHERE id=$1")
            .bind(attestation.id)
            .fetch_one(&pg.0)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM notation WHERE id=$1")
            .bind(notation_id)
            .fetch_one(&pg.0)
            .await
            .unwrap(),
        1
    );
    pg.0.close().await;
}

fn key_id(key: &MirrorKey) -> Uuid {
    match key {
        MirrorKey::Attestation(id) => *id,
        _ => unreachable!("attestation test key"),
    }
}
