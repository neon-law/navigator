use super::{for_project, replace_for_project, tests::matter};
use word::matter::Matter;

#[tokio::test]
async fn source_caption_round_trips_nested_fields_and_removes_an_empty_block() {
    let surreal = crate::surreal::test_support::mem().await;
    let project_id = crate::postgres_mirror::from_source(
        crate::test_support::seed_project_surreal(&surreal, "source-caption"),
    )
    .await;
    let declared = matter("00CV000006");

    let stored = crate::postgres_mirror::from_source(replace_for_project(
        &surreal,
        project_id,
        Some(&declared),
    ))
    .await
    .unwrap()
    .unwrap();
    assert_eq!(stored.matter, declared);
    assert_eq!(
        crate::postgres_mirror::from_source(for_project(&surreal, project_id))
            .await
            .unwrap(),
        Some(stored)
    );

    assert!(crate::postgres_mirror::from_source(replace_for_project(
        &surreal,
        project_id,
        Some(&Matter::default()),
    ))
    .await
    .unwrap()
    .is_none());
    assert!(
        crate::postgres_mirror::from_source(for_project(&surreal, project_id))
            .await
            .unwrap()
            .is_none()
    );
}
