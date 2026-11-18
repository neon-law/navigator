//! Real PostgreSQL evidence boundaries for the Asset union handoff.
use super::*;
use crate::postgres_mirror::tests::{connect, fresh_schema};

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one full Asset provenance and field-ownership truth table"
)]
async fn native_evidence_resolves_only_owned_columns_and_preserves_unknown_drift() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let (_, notation, _) = crate::postgres::fixtures::workflow_fixture(&pg).await;
    let (firm, project) = crate::postgres::fixtures::firm_for_notation(&pg, notation).await;
    let envelope = Uuid::new_v4();
    let id = Uuid::new_v4();
    let metadata = serde_json::json!({"docusign_envelope_id":envelope,"ordinary":{"value":null}});
    sqlx::query("INSERT INTO asset(id,storage_key,content_type,byte_size,sha256_hex,project_id,kind,visibility,metadata) VALUES($1,'synthetic/key','application/pdf',1,'synthetic',$2,'onboarding','internal',$3)")
        .bind(id).bind(project).bind(metadata).execute(pg.pool()).await.unwrap();
    let image = || async {
        let row = sqlx::query(ASSET_SELECT.as_str())
            .fetch_one(pg.pool())
            .await
            .unwrap();
        AssetImage::from_row(&row).unwrap()
    };
    let held = image().await;
    let mut legacy = held.clone();
    legacy.visibility = "public".into();
    legacy.updated_at -= chrono::Duration::seconds(10);
    legacy
        .metadata
        .as_mut()
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove(crate::assets::VERIFIED_ONBOARDING_ENVELOPE_KEY);
    let map = |row: AssetImage| BTreeMap::from([(row.id, row)]);
    let (privacy, proof) = compare_union(&pg, map(legacy.clone()), map(held.clone()))
        .await
        .unwrap();
    assert!(privacy.divergent.is_empty());
    assert_eq!(proof.resolved_native_overlaps, 1);
    assert!(!native_onboarding(&pg, &held).await.unwrap());
    sqlx::query("UPDATE asset SET metadata=metadata || $2 WHERE id=$1")
        .bind(id)
        .bind(serde_json::json!({"verified_onboarding_envelope_id":envelope}))
        .execute(pg.pool())
        .await
        .unwrap();
    let held = image().await;
    let (forged, proof) = compare_union(&pg, map(held.clone()), map(held.clone()))
        .await
        .unwrap();
    assert_eq!(forged.divergent, vec![MirrorKey::Asset(id)]);
    assert_eq!(proof.unresolved_overlaps, 1);
    // A reserved marker alone proves neither native creation nor field ownership.
    let (parity, proof) = compare_union(&pg, map(legacy.clone()), map(held.clone()))
        .await
        .unwrap();
    assert_eq!(parity.divergent, vec![MirrorKey::Asset(id)]);
    assert_eq!(proof.unresolved_overlaps, 1);
    sqlx::query("INSERT INTO docusign_completion(firm_id,account_id,envelope_id,status,project_id) VALUES($1,$2,$3,'filed',$4)")
        .bind(firm).bind(Uuid::new_v4()).bind(envelope).bind(project).execute(pg.pool()).await.unwrap();
    let (parity, proof) = compare_union(&pg, map(legacy.clone()), map(held.clone()))
        .await
        .unwrap();
    assert!(parity.divergent.is_empty());
    assert_eq!(proof.resolved_native_overlaps, 1);
    assert_eq!(
        upsert_asset(&pg, &legacy).await.unwrap(),
        Mirrored::Unchanged
    );
    assert_eq!(image().await, held); // comparison never changes native privacy or timestamps
    let (parity, proof) = compare_union(&pg, BTreeMap::new(), map(held.clone()))
        .await
        .unwrap();
    assert!(parity.divergent.is_empty());
    assert_eq!(proof.proven_native_target_only, 1);
    let mut ambiguous = held.clone();
    ambiguous.description = Some("unexplained".into());
    let (parity, proof) = compare_union(&pg, map(legacy.clone()), map(ambiguous))
        .await
        .unwrap();
    assert_eq!(parity.divergent.len(), 1);
    assert_eq!(proof.unresolved_overlaps, 1);
    let mut unknown = held.clone();
    unknown.metadata = None;
    let (_, proof) = compare_union(&pg, BTreeMap::new(), map(unknown))
        .await
        .unwrap();
    assert_eq!(proof.unknown_target_only, 1);
    let (_, proof) = compare_union(&pg, map(legacy.clone()), BTreeMap::new())
        .await
        .unwrap();
    assert_eq!(proof.source_only_remaining, 1);
    // A Project-only completion cannot establish ownership of another Note's key.
    let mut scoped = held.clone();
    scoped.storage_key = format!("notations/{notation}/document.pdf");
    assert!(!native_onboarding(&pg, &scoped).await.unwrap());
    sqlx::query("UPDATE template SET kind='onboarding' WHERE id=(SELECT template_id FROM notation WHERE id=$1)")
        .bind(notation).execute(pg.pool()).await.unwrap();
    sqlx::query("INSERT INTO signature(id,notation_id,provider,provider_id,state,signed_at,inserted_at,updated_at) VALUES($1,$2,'docusign',$3,'completed','2026-11-18T00:00:00Z',now(),now())")
        .bind(Uuid::new_v4()).bind(notation).bind(envelope.to_string()).execute(pg.pool()).await.unwrap();
    assert!(native_onboarding(&pg, &scoped).await.unwrap());
    let other = Uuid::new_v4();
    sqlx::query("INSERT INTO notation(id,template_id,person_id,project_id,state) SELECT $1,template_id,person_id,project_id,'BEGIN' FROM notation WHERE id=$2")
        .bind(other).bind(notation).execute(pg.pool()).await.unwrap();
    scoped.storage_key = format!("notations/{other}/document.pdf");
    assert!(!native_onboarding(&pg, &scoped).await.unwrap());
    let mut historical = legacy;
    historical.id = Uuid::new_v4();
    historical.storage_key = "historical/source-only".into();
    assert_eq!(
        upsert_asset(&pg, &historical).await.unwrap(),
        Mirrored::Written
    );
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "{} WHERE id=$1",
        ASSET_SELECT.as_str()
    )))
    .bind(historical.id)
    .fetch_one(pg.pool())
    .await
    .unwrap();
    assert_eq!(AssetImage::from_row(&row).unwrap(), historical);
    pg.pool().close().await;
}
