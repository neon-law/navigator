#[test]
fn failed_fixture_cleanup_preserves_the_original_setup_cause() {
    let error = super::fixture_setup_failure(
        anyhow::anyhow!("synthetic schema setup failure"),
        Err(anyhow::anyhow!("synthetic cleanup transport failure")),
    );
    assert_eq!(
        error.root_cause().to_string(),
        "synthetic schema setup failure"
    );
    assert!(format!("{error:#}").contains("synthetic cleanup transport failure"));
    let clean = super::fixture_setup_failure(anyhow::anyhow!("synthetic setup cause"), Ok(()));
    assert_eq!(clean.root_cause().to_string(), "synthetic setup cause");
    assert!(!format!("{clean:#}").contains("cleanup"));
}
