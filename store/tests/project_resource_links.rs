//! A Project's collaboration resources are columns on the matter row.
//!
//! Six resources travel with every matter — the private Slack channel, the
//! private Notion page, the private Drive folder, and the optional shared
//! Slack channel, shared Notion page, and Portal. Four of them are
//! stored URLs a lawyer configures; this suite covers those columns and the
//! link validation every stored URL passes before it can be rendered as an
//! `href`.

#[path = "support/native_project.rs"]
mod native_project;

use store::projects::{
    find_by_id, is_valid_resource_url, update_project, ProjectCommandError, UpdateProjectCommand,
};

fn command(name: &str) -> UpdateProjectCommand {
    UpdateProjectCommand {
        name: Some(name.to_string()),
        ..Default::default()
    }
}

/// Both Notion pages round-trip through the descriptive update, the same way
/// the two Slack channels do.
#[tokio::test]
async fn both_notion_pages_are_stored_on_the_matter_row() {
    store::test_support::with_postgres(async {
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let matter = native_project::create("notion-round-trip").await;

        update_project(
            &application_store,
            matter.id,
            &UpdateProjectCommand {
                private_notion_page_url: Some(
                    "https://www.notion.so/neonlaw/Private-abc123".to_string(),
                ),
                shared_notion_page_url: Some(
                    "https://www.notion.so/neonlaw/Shared-def456".to_string(),
                ),
                ..command("Notion round trip")
            },
        )
        .await
        .expect("update stores both Notion pages");

        let saved = find_by_id(&application_store, matter.id)
            .await
            .expect("read back")
            .expect("matter exists");
        assert_eq!(
            saved.private_notion_page_url.as_deref(),
            Some("https://www.notion.so/neonlaw/Private-abc123")
        );
        assert_eq!(
            saved.shared_notion_page_url.as_deref(),
            Some("https://www.notion.so/neonlaw/Shared-def456")
        );
    })
    .await;
}

/// A blank submission clears the column, matching every other descriptive
/// field: a lawyer removes a resource by emptying its input.
#[tokio::test]
async fn a_blank_notion_field_clears_the_column() {
    store::test_support::with_postgres(async {
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let matter = native_project::create("notion-clears").await;

        update_project(
            &application_store,
            matter.id,
            &UpdateProjectCommand {
                private_notion_page_url: Some(
                    "https://www.notion.so/neonlaw/Gone-abc123".to_string(),
                ),
                ..command("Notion clears")
            },
        )
        .await
        .expect("store a page");
        update_project(
            &application_store,
            matter.id,
            &UpdateProjectCommand {
                private_notion_page_url: Some(String::new()),
                ..command("Notion clears")
            },
        )
        .await
        .expect("clear the page");

        let saved = find_by_id(&application_store, matter.id)
            .await
            .expect("read back")
            .expect("matter exists");
        assert_eq!(saved.private_notion_page_url, None, "blank clears");
    })
    .await;
}

/// Every resource URL is rendered as an `href` on the matter page, so a value
/// that would execute rather than navigate is refused at the command boundary
/// instead of stored. This is what separates a resource link from a plain
/// descriptive string.
#[tokio::test]
async fn a_resource_url_that_would_not_navigate_is_refused() {
    store::test_support::with_postgres(async {
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let matter = native_project::create("notion-refuses").await;

        for hostile in [
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "file:///etc/passwd",
            "https://user:token@www.notion.so/page",
        ] {
            let outcome = update_project(
                &application_store,
                matter.id,
                &UpdateProjectCommand {
                    private_notion_page_url: Some(hostile.to_string()),
                    ..command("Notion refuses")
                },
            )
            .await;
            assert!(
                matches!(outcome, Err(ProjectCommandError::Invalid(_))),
                "`{hostile}` must be refused, got {outcome:?}"
            );
        }

        let saved = find_by_id(&application_store, matter.id)
            .await
            .expect("read back")
            .expect("matter exists");
        assert_eq!(saved.private_notion_page_url, None, "nothing was stored");
    })
    .await;
}

/// The same gate covers the two Slack columns, which are rendered as links on
/// the identical panel.
#[tokio::test]
async fn the_slack_columns_pass_the_same_gate() {
    store::test_support::with_postgres(async {
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let matter = native_project::create("slack-validated").await;

        for field in ["internal", "external"] {
            let mut input = command("Slack validated");
            if field == "internal" {
                input.internal_slack_channel_url = Some("javascript:alert(1)".to_string());
            } else {
                input.external_slack_channel_url = Some("javascript:alert(1)".to_string());
            }
            assert!(
                matches!(
                    update_project(&application_store, matter.id, &input).await,
                    Err(ProjectCommandError::Invalid(_))
                ),
                "the {field} Slack channel must reject a script URL"
            );
        }
    })
    .await;
}

/// The shape rule itself, exercised directly so the accept/refuse boundary is
/// documented without a database round-trip per case.
#[test]
fn resource_url_shape_is_http_with_a_host_and_a_path() {
    for accepted in [
        "https://www.notion.so/neonlaw/Matter-abc123",
        "https://neonlaw.slack.com/archives/C0123456789",
        "https://drive.google.com/drive/folders/1QaBcD",
        "http://localhost:3001/app/projects/sample-litigation/portal/",
    ] {
        assert!(is_valid_resource_url(accepted), "{accepted} should pass");
    }

    for refused in [
        "",
        "   ",
        // A forge or host names a service, not a resource on it.
        "https://www.notion.so",
        "https://www.notion.so/",
        // Non-navigating or host-reading schemes.
        "javascript:alert(1)",
        "data:text/html,x",
        "file:///etc/passwd",
        "ssh://git@example.com/repo",
        // An embedded credential would put a secret in a rendered page.
        "https://user:token@www.notion.so/page",
        // Whitespace splits an href.
        "https://www.notion.so/a page",
    ] {
        assert!(!is_valid_resource_url(refused), "{refused:?} should fail");
    }
}
