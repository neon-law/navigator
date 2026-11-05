//! Cross-module integration flow: the Notion service seam feeding the
//! project-reconcile logic. The portal builds `NotionPageSnapshot`s from
//! live `NotionService` reads and hands them to `reconcile_notion_project`;
//! this suite drives that same handoff through `FakeNotion` and asserts
//! every repair decision on a page the service itself created.

use cloud::{
    ensure_private_page, reconcile_notion_project, FakeNotion, NotionError, NotionPageSnapshot,
    NotionProjectInput, NotionRepairDecision, NotionService,
};

const PROJECT_CODE: &str = "sample-project";
const CANONICAL_URL: &str = "https://notion.example/recorded-permalink";

fn input() -> NotionProjectInput {
    NotionProjectInput {
        project_code: PROJECT_CODE.into(),
        canonical_url: CANONICAL_URL.into(),
    }
}

/// Snapshot of the page exactly as the portal records it: fetched back
/// from the service by id, with the accessibility flag from the same
/// read.
async fn snapshot_of(notion: &FakeNotion, page_id: &str) -> NotionPageSnapshot {
    let page = NotionService::get_page(notion, page_id)
        .await
        .expect("get_page")
        .expect("page recorded under its id");
    NotionPageSnapshot {
        id: page.id,
        url: page.url,
        project_code: page.project_code,
        accessible: !page.archived,
    }
}

#[tokio::test]
async fn service_created_page_flows_through_every_reconcile_decision() {
    let notion = FakeNotion::new();

    // Before anything exists, the reconcile answer is Missing.
    assert_eq!(
        reconcile_notion_project(&input(), &[]),
        NotionRepairDecision::Missing
    );

    // Find-or-create through the helper: first call creates, second
    // finds — the page is never duplicated by re-running ensure.
    let (page, created) = ensure_private_page(&notion, PROJECT_CODE)
        .await
        .expect("first ensure creates the page");
    assert!(created);
    let (again, created_again) = ensure_private_page(&notion, PROJECT_CODE)
        .await
        .expect("second ensure finds the page");
    assert!(!created_again);
    assert_eq!(again.id, page.id);
    assert_eq!(notion.create_calls(), 1);

    // The created page, read back by id, reconciles as Unchanged when the
    // recorded permalink matches — but a fake's permalink is not the
    // recorded canonical URL, so the honest first answer is Repair.
    let snap = snapshot_of(&notion, &page.id).await;
    let decision = reconcile_notion_project(&input(), std::slice::from_ref(&snap));
    match &decision {
        NotionRepairDecision::Repair { page_id, .. } => {
            assert_eq!(page_id, &page.id, "a moved/renamed page repairs");
        }
        NotionRepairDecision::Unchanged { .. } => {}
        other => panic!("unexpected decision for the created page: {other:?}"),
    }

    // When the URL does agree, the page is Unchanged.
    let agreeing = NotionProjectInput {
        canonical_url: snap.url.clone(),
        ..input()
    };
    assert_eq!(
        reconcile_notion_project(&agreeing, std::slice::from_ref(&snap)),
        NotionRepairDecision::Unchanged {
            page_id: page.id.clone(),
        }
    );

    // A page whose id no longer resolves (archived away, deleted) is
    // Unavailable, not Missing — the recorded pointer is stale, the
    // project code is not.
    let unreachable = NotionPageSnapshot {
        accessible: false,
        ..snap.clone()
    };
    assert_eq!(
        reconcile_notion_project(&input(), &[unreachable]),
        NotionRepairDecision::Unavailable {
            page_id: page.id.clone(),
        }
    );

    // Renaming the page under a different project code is a Conflict:
    // the found page belongs to another project.
    let foreign = NotionPageSnapshot {
        project_code: "other-project".into(),
        ..snap.clone()
    };
    assert_eq!(
        reconcile_notion_project(&input(), &[foreign]),
        NotionRepairDecision::Conflict {
            page_id: page.id.clone(),
            observed_code: "other-project".into(),
        }
    );

    // A second page for the same code is a Duplicate — reconcile names
    // every page id so a human collapses them.
    notion.add_duplicate(PROJECT_CODE, "duplicate-page-id");
    let listed = NotionService::list_private_pages(&notion, PROJECT_CODE)
        .await
        .expect("list");
    assert_eq!(listed.len(), 2);
    let snapshots: Vec<NotionPageSnapshot> = listed
        .into_iter()
        .map(|page| NotionPageSnapshot {
            id: page.id,
            url: page.url,
            project_code: page.project_code,
            accessible: !page.archived,
        })
        .collect();
    assert_eq!(
        reconcile_notion_project(&input(), &snapshots),
        NotionRepairDecision::Duplicate {
            page_ids: snapshots.iter().map(|page| page.id.clone()).collect(),
        }
    );
}

#[tokio::test]
async fn an_unavailable_notion_fails_the_lookup_instead_of_repairing() {
    let notion = FakeNotion::new();
    notion.set_unavailable(true);

    // An outage must surface as an error to the caller — never as an
    // empty result that reconcile would read as "Missing, create one".
    let ensured = ensure_private_page(&notion, PROJECT_CODE).await;
    assert!(
        matches!(ensured, Err(NotionError::Transport)),
        "{ensured:?}"
    );
}
