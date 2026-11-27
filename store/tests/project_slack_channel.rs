//! The bot's private-channel address is persisted on the authoritative Project row.

#[path = "support/native_project.rs"]
mod native_project;

use store::projects::{
    set_internal_slack_channel, set_internal_slack_channel_id, ProjectCommandError,
};

#[tokio::test]
async fn a_slack_channel_id_is_written_for_the_project() {
    store::test_support::with_postgres(async {
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let project = native_project::create("slack-channel").await;

        let updated = set_internal_slack_channel_id(&application_store, project.id, "C123456")
            .await
            .expect("set Slack channel")
            .expect("Project exists");
        assert_eq!(
            updated.internal_slack_channel_id.as_deref(),
            Some("C123456")
        );
    })
    .await;
}

#[tokio::test]
async fn an_invalid_slack_channel_id_is_refused() {
    store::test_support::with_postgres(async {
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let project = native_project::create("slack-invalid").await;

        assert!(matches!(
            set_internal_slack_channel_id(&application_store, project.id, "C-123").await,
            Err(ProjectCommandError::Invalid(_))
        ));
    })
    .await;
}

#[tokio::test]
async fn the_channel_id_and_url_are_persisted_as_one_coordinate() {
    store::test_support::with_postgres(async {
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let project = native_project::create("slack-atomic").await;

        let updated = set_internal_slack_channel(
            &application_store,
            project.id,
            "C123456",
            "https://app.slack.com/archives/C123456",
        )
        .await
        .expect("set Slack channel")
        .expect("Project exists");
        assert_eq!(
            updated.internal_slack_channel_id.as_deref(),
            Some("C123456")
        );
        assert_eq!(
            updated.internal_slack_channel_url.as_deref(),
            Some("https://app.slack.com/archives/C123456")
        );
    })
    .await;
}
