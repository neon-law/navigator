use super::{tests::unknown_claims, *};

#[tokio::test]
async fn a_historical_person_keeps_primary_subject_after_schema_apply() {
    let surreal = store::surreal::test_support::unmigrated().await;
    let historical_id = Uuid::now_v7();
    surreal
        .query(
            "CREATE $id SET name = 'Historical Person', \
             email = 'historical@example.com', role = 'client', is_admitted = true, \
             oidc_subject = 'primary-subject', \
             inserted_at = type::datetime('2020-01-01T00:00:00Z'), \
             updated_at = type::datetime('2020-01-01T00:00:00Z')",
        )
        .bind(("id", store::surreal::record_id("person", historical_id)))
        .await
        .unwrap()
        .check()
        .unwrap();
    store::schema::apply(&surreal)
        .await
        .expect("the schema converges over the historical Person row");
    let mut claims = unknown_claims("historical@example.com");
    claims.sub = "primary-subject".into();
    claims.email = None;

    let (resolved_id, _, _) =
        resolve_person_from_claims(&surreal, Some(ProviderId::Primary), &claims, None)
            .await
            .expect("the historical primary-provider subject still resolves");

    let stored = store::persons::find_by_id(&surreal, resolved_id)
        .await
        .unwrap()
        .expect("the historical Person row remains present");
    assert_eq!(stored.oidc_subject.as_deref(), Some("primary-subject"));
    assert_eq!(stored.apple_subject, None);
    assert_eq!(stored.microsoft_subject, None);
}
