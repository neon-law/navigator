//! Attorney document-review screen for an inbound contract review's Notation
//! document source (ENG-579) — `/app/lawyer/contract-reviews/{id}/document`.
//!
//! Extends the existing contract-review workbench ([`crate::contract_review`])
//! with what its per-finding cards cannot show: the imported document's
//! canonical outline, one supported-block editor, a word-level diff from
//! the version an edit started from, and anchored comments with granular
//! per-comment accept/reject/edit decisions. There is **no bulk-decide**:
//! every comment is its own native `POST` form.
//!
//! # Authorization
//!
//! Same gate as [`crate::contract_review`]: `/app/lawyer/*` embedded Rego
//! policy plus the per-matter row scope
//! (`store::access::can_see_project_as_lawyer`) — a client role, or a
//! lawyer not disclosed to the matter, gets the established `404`. Nothing
//! here changes the client asset lens: this page reads only internal
//! [`store::notation_documents`] rows, which a client route never resolves.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::people::ViewerRole;

/// One outline row for a block in the current version.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct BlockRow {
    pub anchor: String,
    /// The cumulative outline path (`II.B.1`), empty for a block with no
    /// outline identity.
    pub path: String,
    pub marker: String,
    pub editable: bool,
}

/// One comment anchored to a block, as the attorney sees it.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct CommentRow {
    pub id: String,
    pub anchor: String,
    pub body: String,
    /// `""` until decided; otherwise `accepted` / `rejected` / `edited`.
    pub decision: String,
}

impl CommentRow {
    #[must_use]
    pub fn is_decided(&self) -> bool {
        !self.decision.is_empty()
    }
}

/// One span of the word-level diff between the version an edit started from
/// and the current version.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct DiffSpanRow {
    /// `equal` / `removed` / `added`.
    pub kind: String,
    pub text: String,
}

/// One protected-token kind's count in the current accepted text — an
/// informational badge, not a persisted lock.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct ProtectedCountRow {
    pub kind: String,
    pub count: usize,
}

/// Everything the page renders. `found` is `false` for a missing review or a
/// caller outside the matter's lawyer lens, and for a review whose Notation
/// has no document imported yet.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct NotationDocumentReviewView {
    #[serde(default)]
    pub tokens_href: String,
    #[serde(default)]
    pub firm_name: String,
    pub review_id: String,
    pub found: bool,
    /// The review is open for edits: `analyzed` status and the matter
    /// parked at `lawyer_review` — the same rule
    /// [`crate::contract_review::ContractReviewView::editable`] applies.
    pub editable: bool,
    pub version_id: String,
    pub parent_version_id: String,
    pub blocks: Vec<BlockRow>,
    pub diff: Vec<DiffSpanRow>,
    pub comments: Vec<CommentRow>,
    pub protected_counts: Vec<ProtectedCountRow>,
    pub error: Option<String>,
    pub csrf_token: String,
    pub role: ViewerRole,
}

#[cfg(feature = "server")]
struct ReviewContext {
    role: ViewerRole,
    csrf_token: String,
    error: Option<String>,
    person_id: Option<uuid::Uuid>,
}

/// The review screen's `?error=` flash (set by a mutation handler's
/// redirect-on-refusal).
#[cfg(feature = "server")]
#[derive(Deserialize, Default)]
struct ErrorFlashQuery {
    #[serde(default)]
    error: Option<String>,
}

#[cfg(feature = "server")]
async fn review_context() -> ReviewContext {
    let role = dioxus_fullstack_core::FullstackContext::extract::<axum::Extension<ViewerRole>, _>()
        .await
        .map(|axum::Extension(role)| role)
        .unwrap_or_default();
    let csrf_token = dioxus_fullstack_core::FullstackContext::extract::<
        axum::Extension<crate::csrf::CsrfToken>,
        _,
    >()
    .await
    .map(|axum::Extension(token)| token.0)
    .unwrap_or_default();
    let error = dioxus_fullstack_core::FullstackContext::extract::<
        axum::extract::Query<ErrorFlashQuery>,
        _,
    >()
    .await
    .ok()
    .and_then(|axum::extract::Query(q)| q.error);
    let person_id = dioxus_fullstack_core::FullstackContext::extract::<
        axum::Extension<crate::portal_project_list::PersonId>,
        _,
    >()
    .await
    .ok()
    .and_then(|axum::Extension(pid)| pid.0)
    .and_then(|raw| raw.parse::<uuid::Uuid>().ok());
    ReviewContext {
        role,
        csrf_token,
        error,
        person_id,
    }
}

#[cfg(feature = "server")]
struct Scoped {
    review: store::contract_reviews::ContractReview,
    notation: store::notations::Notation,
}

#[cfg(feature = "server")]
async fn load_scoped(
    surreal: &store::surreal::SurrealDb,
    review_id: uuid::Uuid,
    person_id: Option<uuid::Uuid>,
    store_role: store::persons::Role,
) -> Result<Option<Scoped>, String> {
    let Some(review) = store::contract_reviews::by_id(surreal, review_id)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let Some(notation) = store::notations::find_by_id(surreal, review.notation_id)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    if !store::access::can_see_project_as_lawyer(
        surreal,
        person_id,
        store_role,
        notation.project_id,
    )
    .await?
    {
        return Ok(None);
    }
    Ok(Some(Scoped { review, notation }))
}

/// Load the document-review screen for the `{id}` (contract review id) in
/// the request path, enforcing the same per-matter row scope the handler
/// does.
#[server]
pub async fn get_notation_document_review() -> Result<NotationDocumentReviewView, ServerFnError> {
    let ReviewContext {
        role,
        csrf_token,
        error,
        person_id,
    } = review_context().await;
    let firm_name = crate::app_chrome::firm_name_from_context().await;

    let not_found = |review_id: String| NotationDocumentReviewView {
        firm_name: firm_name.clone(),
        review_id,
        found: false,
        role,
        ..NotationDocumentReviewView::default()
    };

    if !role.is_lawyer_tier() {
        dioxus_fullstack_core::FullstackContext::commit_http_status(
            axum::http::StatusCode::NOT_FOUND,
            None,
        );
        return Ok(not_found(String::new()));
    }
    let Ok(axum::extract::Path(review_id)) =
        dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Path<uuid::Uuid>, _>()
            .await
    else {
        return Ok(not_found(String::new()));
    };
    let store_role = match role {
        ViewerRole::Owner => store::persons::Role::Owner,
        ViewerRole::Admin => store::persons::Role::Admin,
        ViewerRole::Lawyer => store::persons::Role::Lawyer,
        ViewerRole::Clerk => store::persons::Role::Clerk,
        ViewerRole::Client => store::persons::Role::Client,
    };
    let id = review_id.to_string();
    let surreal = consume_context::<store::surreal::SurrealDb>();
    let storage = consume_context::<std::sync::Arc<dyn cloud::StorageService>>();
    let map_err = |e: String| ServerFnError::new(e);

    let Some(Scoped { review, notation }) = load_scoped(&surreal, review_id, person_id, store_role)
        .await
        .map_err(map_err)?
    else {
        dioxus_fullstack_core::FullstackContext::commit_http_status(
            axum::http::StatusCode::NOT_FOUND,
            None,
        );
        return Ok(not_found(id));
    };

    let Some(current) =
        store::notation_documents::current_version(&surreal, notation.project_id, notation.id)
            .await
            .map_err(|e| ServerFnError::new(e.to_string()))?
    else {
        // No document imported yet — a genuine, expected state until the
        // Word-intake step (ENG-582) lands; not a 404.
        return Ok(NotationDocumentReviewView {
            tokens_href: crate::app_chrome::app_tokens_href_from_context().await,
            firm_name,
            review_id: id,
            found: true,
            role,
            csrf_token,
            error,
            ..NotationDocumentReviewView::default()
        });
    };

    document_view(DocumentViewArgs {
        surreal: &surreal,
        storage: &storage,
        notation: &notation,
        review: &review,
        current,
        review_id: id,
        firm_name,
        error,
        csrf_token,
        role,
    })
    .await
}

/// What [`document_view`] needs, carried as one struct so the function stays
/// under clippy's argument budget.
#[cfg(feature = "server")]
struct DocumentViewArgs<'a> {
    surreal: &'a store::surreal::SurrealDb,
    storage: &'a std::sync::Arc<dyn cloud::StorageService>,
    notation: &'a store::notations::Notation,
    review: &'a store::contract_reviews::ContractReview,
    current: store::notation_documents::NotationDocumentVersion,
    review_id: String,
    firm_name: String,
    error: Option<String>,
    csrf_token: String,
    role: ViewerRole,
}

/// Assemble the view from an already-scoped review, notation, and current
/// document version — the part of [`get_notation_document_review`] that
/// reads the version's Markdown, manifest, parent diff, comments, and
/// protected-token counts.
#[cfg(feature = "server")]
async fn document_view(
    args: DocumentViewArgs<'_>,
) -> Result<NotationDocumentReviewView, ServerFnError> {
    let DocumentViewArgs {
        surreal,
        storage,
        notation,
        review,
        current,
        review_id: id,
        firm_name,
        error,
        csrf_token,
        role,
    } = args;
    let map_err = |e: String| ServerFnError::new(e);

    let markdown = store::notation_documents::markdown_of(surreal, storage, &current)
        .await
        .map_err(|e| map_err(e.to_string()))?;
    let manifest_bytes = store::notation_documents::anchor_manifest_of(surreal, storage, &current)
        .await
        .map_err(|e| map_err(e.to_string()))?;
    let manifest: Vec<word::BlockManifestEntry> =
        serde_json::from_slice(&manifest_bytes).unwrap_or_default();
    let blocks = manifest
        .into_iter()
        .map(|row| BlockRow {
            anchor: row.anchor,
            path: row.path.unwrap_or_default(),
            marker: row.marker.unwrap_or_default(),
            editable: row.editable,
        })
        .collect();

    let diff = document_diff(surreal, storage, notation, &current, &markdown).await?;

    let comments = store::notation_documents::comments_for_version(
        surreal,
        notation.project_id,
        notation.id,
        current.id,
    )
    .await
    .map_err(|e| map_err(e.to_string()))?
    .into_iter()
    .map(|c| CommentRow {
        id: c.id.to_string(),
        anchor: c.anchor,
        body: c.body,
        decision: c.decision.unwrap_or_default(),
    })
    .collect();

    let protected_counts = protected_token_counts(&markdown);
    let editable = review.status == store::contract_reviews::STATUS_ANALYZED
        && notation.state == "lawyer_review";

    Ok(NotationDocumentReviewView {
        tokens_href: crate::app_chrome::app_tokens_href_from_context().await,
        firm_name,
        review_id: id,
        found: true,
        editable,
        version_id: current.id.to_string(),
        parent_version_id: current
            .parent_version_id
            .map(|id| id.to_string())
            .unwrap_or_default(),
        blocks,
        diff,
        comments,
        protected_counts,
        error,
        csrf_token,
        role,
    })
}

/// The word-level diff between `current`'s parent version (if any) and
/// `current_markdown`.
#[cfg(feature = "server")]
async fn document_diff(
    surreal: &store::surreal::SurrealDb,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
    notation: &store::notations::Notation,
    current: &store::notation_documents::NotationDocumentVersion,
    current_markdown: &str,
) -> Result<Vec<DiffSpanRow>, ServerFnError> {
    let Some(parent_id) = current.parent_version_id else {
        return Ok(Vec::new());
    };
    let parent = store::notation_documents::find_version(
        surreal,
        notation.project_id,
        notation.id,
        parent_id,
    )
    .await
    .map_err(|e| ServerFnError::new(e.to_string()))?;
    let Some(parent) = parent else {
        return Ok(Vec::new());
    };
    let parent_markdown = store::notation_documents::markdown_of(surreal, storage, &parent)
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    Ok(
        store::notation_documents::word_diff(&parent_markdown, current_markdown)
            .unwrap_or_default()
            .into_iter()
            .map(|span| DiffSpanRow {
                kind: match span.kind {
                    store::notation_documents::DiffKind::Equal => "equal",
                    store::notation_documents::DiffKind::Removed => "removed",
                    store::notation_documents::DiffKind::Added => "added",
                }
                .to_string(),
                text: span.text,
            })
            .collect(),
    )
}

/// Per-kind protected-token counts in `markdown`, in the order the editor's
/// lock checkboxes display them.
#[cfg(feature = "server")]
fn protected_token_counts(markdown: &str) -> Vec<ProtectedCountRow> {
    let tokens = store::notation_documents::protected_tokens(markdown);
    [
        (
            "number",
            store::notation_documents::ProtectedTokenKind::Number,
        ),
        (
            "percentage",
            store::notation_documents::ProtectedTokenKind::Percentage,
        ),
        (
            "currency",
            store::notation_documents::ProtectedTokenKind::Currency,
        ),
        ("date", store::notation_documents::ProtectedTokenKind::Date),
    ]
    .into_iter()
    .map(|(label, kind)| ProtectedCountRow {
        kind: label.to_string(),
        count: tokens.iter().filter(|t| t.kind == kind).count(),
    })
    .collect()
}

/// The diff panel: word-level spans, styled by kind.
fn diff_panel(view: &NotationDocumentReviewView) -> Element {
    if view.diff.is_empty() {
        return rsx! {};
    }
    rsx! {
        section { class: "notation-document-diff", "aria-label": "Diff from the parent version",
            h2 { "Diff from parent version" }
            p { class: "notation-document-diff__text",
                for span in view.diff.iter() {
                    span { class: "notation-document-diff__span notation-document-diff__span--{span.kind}",
                        "{span.text}"
                    }
                }
            }
        }
    }
}

/// The outline navigation list.
fn outline_list(view: &NotationDocumentReviewView) -> Element {
    rsx! {
        nav { class: "notation-document-outline", "aria-label": "Document outline",
            h2 { "Outline" }
            ul { class: "notation-document-outline__list",
                for block in view.blocks.iter() {
                    li { class: "notation-document-outline__item", key: "{block.anchor}",
                        span { class: "notation-document-outline__path", "{block.path}" }
                        span { class: "notation-document-outline__marker", "{block.marker}" }
                        if block.editable {
                            span { class: "nav-badge nav-status--neutral", "Editable" }
                        } else {
                            span { class: "nav-badge nav-status--muted", "Read-only" }
                        }
                    }
                }
            }
        }
    }
}

/// The supported-block editor: one anchor picker, one textarea, and the
/// protected-token locks.
fn edit_form(view: &NotationDocumentReviewView) -> Element {
    let action = format!(
        "/app/lawyer/contract-reviews/{}/document/edit",
        view.review_id
    );
    rsx! {
        section { class: "notation-document-editor",
            h2 { "Edit a block" }
            form {
                class: "nav-form admin-form",
                method: "post",
                action: "{action}",
                "aria-label": "Edit a document block",
                input { r#type: "hidden", name: "_csrf", value: "{view.csrf_token}" }
                input {
                    r#type: "hidden",
                    name: "expected_parent_version_id",
                    value: "{view.version_id}",
                }
                div { class: "nav-field",
                    label { class: "nav-label", r#for: "block-anchor", "Block" }
                    select { class: "nav-select", id: "block-anchor", name: "anchor",
                        for block in view.blocks.iter().filter(|b| b.editable) {
                            option { value: "{block.anchor}", "{block.path} {block.marker}" }
                        }
                    }
                }
                div { class: "nav-field",
                    label { class: "nav-label", r#for: "block-text", "New text" }
                    textarea { class: "nav-input", id: "block-text", name: "text", rows: "6" }
                }
                fieldset { class: "notation-document-editor__locks",
                    legend { "Protected — refuse the save if changed" }
                    for row in view.protected_counts.iter() {
                        label { class: "nav-checkbox-label",
                            input {
                                class: "nav-checkbox",
                                r#type: "checkbox",
                                name: "locked_kinds",
                                value: "{row.kind}",
                            }
                            "{row.kind} ({row.count})"
                        }
                    }
                }
                button { class: "nav-btn nav-btn--primary", r#type: "submit", "Save edit" }
            }
        }
    }
}

/// One comment: its body, decision badge, and — when undecided — the three
/// explicit decide submits. No bulk accept.
fn comment_card(view: &NotationDocumentReviewView, comment: &CommentRow) -> Element {
    let action = format!(
        "/app/lawyer/contract-reviews/{}/document/comments/{}/decide",
        view.review_id, comment.id
    );
    rsx! {
        div { class: "nav-card notation-document-comment", key: "{comment.id}",
            div { class: "nav-card__body",
                p { class: "notation-document-comment__anchor", "{comment.anchor}" }
                p { class: "notation-document-comment__body", "{comment.body}" }
                if comment.is_decided() {
                    span { class: "nav-badge nav-status--success", "{comment.decision}" }
                } else if view.editable {
                    form {
                        class: "nav-form admin-form",
                        method: "post",
                        action: "{action}",
                        "aria-label": "Decide comment",
                        input { r#type: "hidden", name: "_csrf", value: "{view.csrf_token}" }
                        button {
                            class: "nav-btn nav-btn--primary",
                            r#type: "submit",
                            name: "decision",
                            value: "accepted",
                            "Accept"
                        }
                        button {
                            class: "nav-btn nav-btn--secondary",
                            r#type: "submit",
                            name: "decision",
                            value: "rejected",
                            "Reject"
                        }
                        button {
                            class: "nav-btn nav-btn--secondary",
                            r#type: "submit",
                            name: "decision",
                            value: "edited",
                            "Mark edited"
                        }
                    }
                } else {
                    span { class: "nav-badge nav-status--warning", "Needs action" }
                }
            }
        }
    }
}

/// The add-comment form.
fn add_comment_form(view: &NotationDocumentReviewView) -> Element {
    let action = format!(
        "/app/lawyer/contract-reviews/{}/document/comments",
        view.review_id
    );
    rsx! {
        form {
            class: "nav-form admin-form",
            method: "post",
            action: "{action}",
            "aria-label": "Add a comment",
            input { r#type: "hidden", name: "_csrf", value: "{view.csrf_token}" }
            div { class: "nav-field",
                label { class: "nav-label", r#for: "comment-anchor", "Block" }
                select { class: "nav-select", id: "comment-anchor", name: "anchor",
                    for block in view.blocks.iter() {
                        option { value: "{block.anchor}", "{block.path} {block.marker}" }
                    }
                }
            }
            div { class: "nav-field",
                label { class: "nav-label", r#for: "comment-body", "Comment" }
                textarea { class: "nav-input", id: "comment-body", name: "body", rows: "3" }
            }
            button { class: "nav-btn nav-btn--primary", r#type: "submit", "Add comment" }
        }
    }
}

/// The whole document-review body for a loaded review.
fn review_body(view: &NotationDocumentReviewView) -> Element {
    rsx! {
        document::Title { "{view.firm_name} | Lawyer | Document review" }
        header { class: "page-header",
            h1 { "Document review" }
            a {
                href: "/app/lawyer/contract-reviews/{view.review_id}",
                "Back to findings",
            }
        }
        if let Some(error) = view.error.as_ref() {
            p { class: "nav-flash nav-flash--danger", role: "alert", "{error}" }
        }
        if view.version_id.is_empty() {
            p { class: "notation-document-empty",
                "No Word document has been imported for this matter yet."
            }
        } else {
            if !view.editable {
                p { class: "nav-form-notice notation-document-locked", role: "status",
                    "This review is closed — the document is no longer editable."
                }
            }
            {outline_list(view)}
            {diff_panel(view)}
            if view.editable {
                {edit_form(view)}
            }
            h2 { class: "notation-document-comments__title", "Comments" }
            if view.comments.is_empty() {
                p { class: "notation-document-empty", "No comments yet." }
            }
            for comment in view.comments.iter() {
                {comment_card(view, comment)}
            }
            if view.editable {
                {add_comment_form(view)}
            }
        }
    }
}

/// The attorney document-review screen. Server-side rendered; every write is
/// a native `POST` form carrying the session CSRF token.
#[component]
pub fn LawyerNotationDocumentReview() -> Element {
    let resource = use_server_future(get_notation_document_review)?;

    let view = match &*resource.read() {
        Some(Ok(view)) => view.clone(),
        Some(Err(_)) => {
            return rsx! {
                main { id: "notation-document-review", p { "Failed to load the document review." } }
            }
        }
        None => {
            return rsx! {
                main { id: "notation-document-review", p { "Loading…" } }
            }
        }
    };
    let role = view.role;

    rsx! {
        document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
        document::Stylesheet { href: "{view.tokens_href}" }
        nav { class: "lawyer-nav",
            a { class: "nav-link", href: "/app/projects", "Portal" }
            if role.is_lawyer_tier() {
                a { class: "nav-link", href: "/app/lawyer", "Lawyer" }
            }
            if role.is_admin_tier() {
                a { class: "nav-link", href: "/app/admin", "Admin" }
            }
            a { class: "nav-link", href: "/auth/logout", "Sign out" }
        }
        main { id: "notation-document-review", class: "nav-theme",
            if view.found {
                {review_body(&view)}
            } else {
                document::Title { "{view.firm_name} | Lawyer | Not found" }
                h1 { "Not found" }
                p { "No document review is available at this address." }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        review_body, BlockRow, CommentRow, DiffSpanRow, NotationDocumentReviewView,
        ProtectedCountRow,
    };
    use crate::people::ViewerRole;

    fn base_view() -> NotationDocumentReviewView {
        NotationDocumentReviewView {
            tokens_href: String::new(),
            firm_name: "Neon Law".to_string(),
            review_id: "00000000-0000-0000-0000-000000000007".to_string(),
            found: true,
            editable: true,
            version_id: "00000000-0000-0000-0000-000000000001".to_string(),
            parent_version_id: String::new(),
            blocks: vec![BlockRow {
                anchor: "p0".to_string(),
                path: "I".to_string(),
                marker: "I".to_string(),
                editable: true,
            }],
            diff: vec![
                DiffSpanRow {
                    kind: "equal".to_string(),
                    text: "the ".to_string(),
                },
                DiffSpanRow {
                    kind: "removed".to_string(),
                    text: "quick".to_string(),
                },
                DiffSpanRow {
                    kind: "added".to_string(),
                    text: "slow".to_string(),
                },
            ],
            comments: vec![CommentRow {
                id: "c1".to_string(),
                anchor: "p0".to_string(),
                body: "consider a cap".to_string(),
                decision: String::new(),
            }],
            protected_counts: vec![ProtectedCountRow {
                kind: "number".to_string(),
                count: 2,
            }],
            error: None,
            csrf_token: "TOK".to_string(),
            role: ViewerRole::Lawyer,
        }
    }

    fn render(view: &NotationDocumentReviewView) -> String {
        dioxus_ssr::render_element(review_body(view))
    }

    #[test]
    fn renders_the_outline_with_paths_and_editable_badges() {
        let html = render(&base_view());
        assert!(html.contains(">I<"), "{html}");
        assert!(html.contains(">Editable<"), "{html}");
    }

    #[test]
    fn renders_the_word_level_diff_spans() {
        let html = render(&base_view());
        assert!(
            html.contains("notation-document-diff__span--removed"),
            "{html}"
        );
        assert!(
            html.contains("notation-document-diff__span--added"),
            "{html}"
        );
        assert!(html.contains(">quick<"), "{html}");
        assert!(html.contains(">slow<"), "{html}");
    }

    #[test]
    fn an_editable_review_shows_the_block_editor_and_locks() {
        let html = render(&base_view());
        assert!(html.contains(r#"action="/app/lawyer/contract-reviews/00000000-0000-0000-0000-000000000007/document/edit""#), "{html}");
        assert!(
            html.contains(r#"name="expected_parent_version_id""#),
            "{html}"
        );
        assert!(html.contains("number (2)"), "{html}");
    }

    #[test]
    fn an_undecided_comment_offers_three_explicit_decisions_no_bulk_accept() {
        let html = render(&base_view());
        assert!(html.contains(r#"value="accepted""#), "{html}");
        assert!(html.contains(r#"value="rejected""#), "{html}");
        assert!(html.contains(r#"value="edited""#), "{html}");
        assert!(html.contains("consider a cap"), "{html}");
    }

    #[test]
    fn a_decided_comment_shows_its_decision_and_no_decide_form() {
        let mut view = base_view();
        view.comments[0].decision = "accepted".to_string();
        let html = render(&view);
        assert!(html.contains(">accepted<"), "{html}");
        assert!(!html.contains(r#"value="accepted""#), "{html}");
    }

    #[test]
    fn a_closed_review_hides_the_editor_and_comment_forms() {
        let mut view = base_view();
        view.editable = false;
        let html = render(&view);
        assert!(html.contains("no longer editable"), "{html}");
        assert!(!html.contains("notation-document-editor"), "{html}");
        assert!(!html.contains("Add a comment"), "{html}");
    }

    #[test]
    fn no_document_imported_yet_says_so_without_erroring() {
        let mut view = base_view();
        view.version_id = String::new();
        view.blocks.clear();
        let html = render(&view);
        assert!(
            html.contains("No Word document has been imported"),
            "{html}"
        );
    }

    #[test]
    fn the_error_flash_renders_above_the_review() {
        let mut view = base_view();
        view.error = Some("expected parent version has moved on".to_string());
        let html = render(&view);
        assert!(html.contains("nav-flash--danger"), "{html}");
        assert!(
            html.contains("expected parent version has moved on"),
            "{html}"
        );
    }

    #[cfg(feature = "server")]
    mod loader {
        use std::sync::Arc;

        use crate::inbox::server_fn_test_support::{call, request_parts, runtime, Viewer};
        use crate::notation_document_review::{
            get_notation_document_review, protected_token_counts, NotationDocumentReviewView,
        };
        use crate::people::ViewerRole;

        const ROUTE: &str = "/app/lawyer/contract-reviews/{id}/document";

        struct Fixture {
            surreal: store::surreal::SurrealDb,
            storage: Arc<dyn cloud::StorageService>,
            notation_id: uuid::Uuid,
            project_id: uuid::Uuid,
            person_id: uuid::Uuid,
            review_id: uuid::Uuid,
        }

        /// A seeded Notation with a contract review open against it, and no
        /// Word document imported yet.
        async fn fixture() -> Fixture {
            let surreal = store::test_support::mem_surreal().await;
            let storage: Arc<dyn cloud::StorageService> =
                Arc::new(store::test_support::MemoryStorage::default());
            let notation_id = store::test_support::seed_notation(&surreal).await;
            let notation = store::notations::find_by_id(&surreal, notation_id)
                .await
                .unwrap()
                .unwrap();
            let review_id = store::contract_reviews::create(
                &surreal,
                &store::contract_reviews::NewContractReview {
                    notation_id,
                    playbook_id: uuid::Uuid::now_v7(),
                    asset_id: None,
                },
            )
            .await
            .unwrap();
            Fixture {
                surreal,
                storage,
                notation_id,
                project_id: notation.project_id,
                person_id: notation.person_id,
                review_id,
            }
        }

        fn manifest() -> Vec<u8> {
            serde_json::to_vec(&vec![
                word::BlockManifestEntry {
                    anchor: "p0".into(),
                    kind: word::CanonicalBlockKind::Outline,
                    depth: Some(1),
                    marker: Some("I.".into()),
                    path: Some("I".into()),
                    editable: true,
                },
                word::BlockManifestEntry {
                    anchor: "t0".into(),
                    kind: word::CanonicalBlockKind::Table,
                    depth: None,
                    marker: None,
                    path: None,
                    editable: false,
                },
            ])
            .unwrap()
        }

        /// Import a root version, then append an edit on top of it, and
        /// leave one comment on the edit.
        async fn import_and_edit(fixture: &Fixture) -> uuid::Uuid {
            let original_asset_id = store::assets::ingest_content(
                &fixture.surreal,
                &fixture.storage,
                b"PK\x03\x04 synthetic docx",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            )
            .await
            .unwrap();
            let manifest = manifest();
            let root = store::notation_documents::import_root(
                &fixture.surreal,
                &fixture.storage,
                store::notation_documents::ImportRoot {
                    project_id: fixture.project_id,
                    notation_id: fixture.notation_id,
                    original_asset_id,
                    markdown: b"Pay 10 dollars by the quick deadline.",
                    anchor_manifest: &manifest,
                    parser_version: "word-crate-1",
                    schema_version: 1,
                    authored_by_person_id: fixture.person_id,
                    recorded_at: "2026-09-19T10:00:00+00:00",
                },
            )
            .await
            .unwrap()
            .into_model();
            let edit = store::notation_documents::append_edit(
                &fixture.surreal,
                &fixture.storage,
                store::notation_documents::AppendEdit {
                    project_id: fixture.project_id,
                    notation_id: fixture.notation_id,
                    expected_parent_version_id: root.id,
                    markdown: b"Pay 10 dollars by the slow deadline, 5% late.",
                    anchor_manifest: &manifest,
                    parser_version: "word-crate-1",
                    schema_version: 1,
                    authored_by_person_id: fixture.person_id,
                    recorded_at: "2026-09-19T11:00:00+00:00",
                },
            )
            .await
            .unwrap();
            store::notation_documents::add_comment(
                &fixture.surreal,
                fixture.project_id,
                fixture.notation_id,
                edit.id,
                "p0",
                fixture.person_id,
                "consider a cap",
            )
            .await
            .unwrap();
            edit.id
        }

        fn load(
            rt: &tokio::runtime::Runtime,
            fixture: &Fixture,
            uri: &str,
            viewer: &Viewer,
        ) -> (NotationDocumentReviewView, axum::http::StatusCode) {
            let parts = request_parts(rt, ROUTE, uri, viewer);
            let (view, status) = call(
                rt,
                parts,
                |dom| {
                    dom.provide_root_context(fixture.surreal.clone());
                    dom.provide_root_context(fixture.storage.clone());
                },
                get_notation_document_review,
            );
            (view.expect("the loader answers"), status)
        }

        fn uri(review_id: impl std::fmt::Display) -> String {
            format!("/app/lawyer/contract-reviews/{review_id}/document")
        }

        #[test]
        fn a_client_gets_the_not_found_body_and_a_404() {
            let rt = runtime();
            let fixture = rt.block_on(fixture());
            let (view, status) = load(
                &rt,
                &fixture,
                &uri(fixture.review_id),
                &Viewer::new(ViewerRole::Client, Some(fixture.person_id)),
            );
            assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
            assert!(!view.found);
            assert!(view.review_id.is_empty(), "the id is not echoed");
            assert_eq!(view.role, ViewerRole::Client);
        }

        #[test]
        fn a_malformed_id_or_an_unknown_review_is_not_found() {
            let rt = runtime();
            let fixture = rt.block_on(fixture());
            let owner = Viewer::new(ViewerRole::Owner, Some(fixture.person_id));

            let (view, status) = load(&rt, &fixture, &uri("not-a-uuid"), &owner);
            assert_eq!(status, axum::http::StatusCode::OK);
            assert!(!view.found);

            let missing = uuid::Uuid::now_v7();
            let (view, status) = load(&rt, &fixture, &uri(missing), &owner);
            assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
            assert!(!view.found);
            assert_eq!(view.review_id, missing.to_string());
        }

        #[test]
        fn a_review_with_no_imported_document_renders_the_empty_state() {
            let rt = runtime();
            let fixture = rt.block_on(fixture());
            let mut viewer = Viewer::new(ViewerRole::Owner, Some(fixture.person_id));
            viewer.csrf_token = Some("TOKEN".into());
            let (view, status) = load(
                &rt,
                &fixture,
                &format!("{}?error=Moved%20on", uri(fixture.review_id)),
                &viewer,
            );
            assert_eq!(status, axum::http::StatusCode::OK);
            assert!(view.found);
            assert!(view.version_id.is_empty());
            assert!(view.blocks.is_empty());
            assert_eq!(view.csrf_token, "TOKEN");
            assert_eq!(view.error.as_deref(), Some("Moved on"));
        }

        #[test]
        fn an_edited_document_loads_its_outline_diff_comments_and_counts() {
            let rt = runtime();
            let fixture = rt.block_on(fixture());
            let version_id = rt.block_on(import_and_edit(&fixture));
            let owner = Viewer::new(ViewerRole::Owner, Some(fixture.person_id));

            let (view, _) = load(&rt, &fixture, &uri(fixture.review_id), &owner);
            assert!(view.found);
            assert!(!view.editable, "a pending review is not open for edits");
            assert_eq!(view.version_id, version_id.to_string());
            assert!(!view.parent_version_id.is_empty());

            assert_eq!(view.blocks.len(), 2);
            assert_eq!(view.blocks[0].anchor, "p0");
            assert_eq!(view.blocks[0].path, "I");
            assert_eq!(view.blocks[0].marker, "I.");
            assert!(view.blocks[0].editable);
            assert_eq!(view.blocks[1].path, "", "no outline identity");
            assert!(!view.blocks[1].editable);

            let kinds: Vec<&str> = view.diff.iter().map(|span| span.kind.as_str()).collect();
            assert!(kinds.contains(&"equal"), "{kinds:?}");
            assert!(kinds.contains(&"removed"), "{kinds:?}");
            assert!(kinds.contains(&"added"), "{kinds:?}");
            assert!(view
                .diff
                .iter()
                .any(|span| span.kind == "removed" && span.text.contains("quick")));

            assert_eq!(view.comments.len(), 1);
            assert_eq!(view.comments[0].anchor, "p0");
            assert_eq!(view.comments[0].body, "consider a cap");
            assert!(!view.comments[0].is_decided());

            assert_eq!(
                view.protected_counts,
                protected_token_counts("Pay 10 dollars by the slow deadline, 5% late.")
            );

            // Analyzed, with the matter parked at lawyer review: open for edits.
            rt.block_on(async {
                store::contract_reviews::set_status(
                    &fixture.surreal,
                    fixture.review_id,
                    store::contract_reviews::STATUS_ANALYZED,
                )
                .await
                .unwrap();
                store::notations::update_state(
                    &fixture.surreal,
                    fixture.notation_id,
                    "lawyer_review",
                )
                .await
                .unwrap();
            });
            let (view, _) = load(&rt, &fixture, &uri(fixture.review_id), &owner);
            assert!(view.editable);
        }

        #[test]
        fn protected_counts_cover_every_kind_in_lock_order() {
            let counts = protected_token_counts("Pay $1,000 and 15% on 2026-01-02, then 3 more.");
            let kinds: Vec<&str> = counts.iter().map(|row| row.kind.as_str()).collect();
            assert_eq!(kinds, ["number", "percentage", "currency", "date"]);
            let count = |kind: &str| counts.iter().find(|row| row.kind == kind).unwrap().count;
            assert_eq!(count("currency"), 1);
            assert_eq!(count("percentage"), 1);
            assert_eq!(count("date"), 1);
            assert!(protected_token_counts("no figures here")
                .iter()
                .all(|row| row.count == 0));
        }
    }
}
