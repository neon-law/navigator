//! `/app/projects/{project_code}/{notation_id}/outline` — Harvard outline of
//! one notation.
//!
//! The lawyer recording surface bound to a matter's notation, so a client
//! (or a lawyer on the matter) can read the letter they were given — an
//! onboarding letter, a closing letter — without opening a PDF. The
//! Harvard-stage header (title, counter, hint) stays on the article; the
//! `/app` navbar and footer wrap it the way every other matter page does.
//!
//! Clerk is refused the same way they are refused documents: they may know
//! the matter exists, and they never receive the legal work.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::harvard_outline::{HARVARD_OUTLINE_SCRIPT_HREF, HARVARD_OUTLINE_STYLESHEET_HREF};
use crate::people::ViewerRole;

/// Browser path for one notation's outline on a matter.
#[must_use]
pub fn notation_outline_href(project_code: &str, notation_id: &str) -> String {
    format!("/app/projects/{project_code}/{notation_id}/outline")
}

/// Everything the page renders. `stage_html` is `None` for every refusal —
/// unknown matter, unknown notation, a notation on another matter, and a
/// Clerk — all of which render the same not-found body.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct NotationOutlineView {
    pub title: String,
    pub stage_html: Option<String>,
    pub back_href: String,
    /// A read failure. Rendered as an error page under a committed `500`.
    pub failed: bool,
    pub role: ViewerRole,
    #[serde(default)]
    pub logo: Option<crate::components::AppLogo>,
    /// The resolved brand's tokens stylesheet href, so the page wears
    /// its own palette rather than the firm's on a non-default host.
    #[serde(default)]
    pub tokens_href: String,
    #[serde(default)]
    pub firm_name: String,
}

/// Map a parsed outline onto the data a
/// [`crate::components::NotationDocument`] renders: units nested by depth,
/// blocks under the unit they sit in, and each run's Markdown rendered to
/// inline HTML on the server so the component needs no Markdown parser.
#[cfg(feature = "server")]
#[must_use]
pub fn notation_document(
    doc: &views::harvard_outline::OutlineDocument,
) -> crate::components::NotationDocumentData {
    use crate::components::{
        NotationBlock, NotationBlockKind, NotationDocumentData, NotationRun, NotationSection,
    };
    use views::harvard_outline::{self as outline, OutlineNode, Run, Unit, UnitKind};

    fn runs(markdown: &str) -> Vec<NotationRun> {
        outline::runs(markdown)
            .into_iter()
            .map(|run| match run {
                Run::Text(text) => NotationRun::Text {
                    html: outline::inline_html(&text),
                },
                Run::OpenTerm { label, question } => NotationRun::OpenTerm { label, question },
            })
            .collect()
    }
    fn block(unit: &Unit) -> NotationBlock {
        let (kind, markdown) = match unit.kind {
            UnitKind::Hold => (
                NotationBlockKind::Hold,
                outline::hold_body(&unit.markdown).unwrap_or(&unit.markdown),
            ),
            UnitKind::Quotation => (NotationBlockKind::Quotation, unit.markdown.as_str()),
            _ => (NotationBlockKind::Paragraph, unit.markdown.as_str()),
        };
        NotationBlock {
            fragment: unit.fragment.clone(),
            kind,
            runs: runs(markdown),
        }
    }
    fn section(units: &[Unit], node: &OutlineNode) -> NotationSection {
        let unit = &units[node.unit];
        NotationSection {
            fragment: unit.fragment.clone(),
            marker: unit.marker.clone(),
            path: unit.path.clone(),
            quoted: unit.kind == UnitKind::Subsection,
            heading: runs(&unit.markdown),
            blocks: node.blocks.iter().map(|&i| block(&units[i])).collect(),
            sections: node
                .children
                .iter()
                .map(|child| section(units, child))
                .collect(),
        }
    }

    let tree = outline::outline_tree(doc);
    NotationDocumentData {
        title: doc.title.clone(),
        numbering: doc.numbering.map(|numbering| numbering.label().to_string()),
        preamble: tree
            .preamble
            .iter()
            .map(|&i| block(&doc.units[i]))
            .collect(),
        sections: tree
            .sections
            .iter()
            .map(|node| section(&doc.units, node))
            .collect(),
    }
}

/// Load one notation's Harvard stage through the caller's matter lens.
#[cfg(feature = "server")]
#[allow(clippy::too_many_lines)]
async fn load() -> Result<NotationOutlineView, ServerFnError> {
    let role = dioxus_fullstack_core::FullstackContext::extract::<axum::Extension<ViewerRole>, _>()
        .await
        .map(|axum::Extension(role)| role)
        .unwrap_or_default();
    let logo = crate::app_chrome::app_logo_from_context().await;
    let tokens_href = crate::app_chrome::app_tokens_href_from_context().await;
    let firm_name = crate::app_chrome::firm_name_from_context().await;
    let missing = NotationOutlineView {
        firm_name: firm_name.clone(),
        title: String::new(),
        stage_html: None,
        back_href: String::new(),
        failed: false,
        role,
        logo: logo.clone(),
        tokens_href: tokens_href.clone(),
    };
    let failed = || {
        dioxus_fullstack_core::FullstackContext::commit_http_status(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            None,
        );
        NotationOutlineView {
            firm_name: firm_name.clone(),
            title: String::new(),
            stage_html: None,
            back_href: String::new(),
            failed: true,
            role,
            logo: logo.clone(),
            tokens_href: tokens_href.clone(),
        }
    };

    let Ok(axum::extract::Path((project_code, notation_id))) =
        dioxus_fullstack_core::FullstackContext::extract::<
            axum::extract::Path<(String, uuid::Uuid)>,
            _,
        >()
        .await
    else {
        return Ok(missing);
    };
    let person_id = dioxus_fullstack_core::FullstackContext::extract::<
        axum::Extension<crate::portal_project_list::PersonId>,
        _,
    >()
    .await
    .ok()
    .and_then(|axum::Extension(pid)| pid.0)
    .and_then(|raw| raw.parse::<uuid::Uuid>().ok());

    let store_role = match role {
        ViewerRole::Owner => store::persons::Role::Owner,
        ViewerRole::Admin => store::persons::Role::Admin,
        ViewerRole::Lawyer => store::persons::Role::Lawyer,
        ViewerRole::Clerk => store::persons::Role::Clerk,
        ViewerRole::Client => store::persons::Role::Client,
    };

    let surreal = consume_context::<store::surreal::SurrealDb>();
    let project_id = match store::projects::find_by_code(&surreal, &project_code).await {
        Ok(Some(project)) => project.id,
        Ok(None) => return Ok(missing),
        Err(_) => return Ok(failed()),
    };
    let viewer = match store::access::matter_viewer(&surreal, person_id, store_role, project_id)
        .await
    {
        Ok(viewer) => viewer,
        Err(e) => {
            tracing::error!(error = %e, %project_id, %notation_id, "notation outline access check failed");
            return Ok(failed());
        }
    };
    // A Clerk may know the matter exists. They never receive the letter.
    match viewer {
        Some(store::access::MatterViewer::Clerk) | None => return Ok(missing),
        Some(_) => {}
    }

    let notation = match store::notations::find_by_id(&surreal, notation_id).await {
        Ok(row) => row,
        Err(e) => {
            tracing::error!(error = %e, %project_id, %notation_id, "db error loading notation outline");
            return Ok(failed());
        }
    };
    let Some(notation) = notation.filter(|n| n.project_id == project_id) else {
        return Ok(missing);
    };

    let template = match store::templates::find_by_id(&surreal, notation.template_id).await {
        Ok(Some(template)) => template,
        Ok(None) => return Ok(missing),
        Err(error) => {
            tracing::error!(%error, %project_id, %notation_id, "notation outline visibility check failed");
            return Ok(failed());
        }
    };
    if !notation_kind_visible_to_viewer(template.kind.as_deref(), viewer) {
        return Ok(missing);
    }

    // The body is the Draft the Notation was opened from, verified against
    // the digest it pinned; a mismatch fails closed rather than drawing
    // different words.
    let storage = consume_context::<std::sync::Arc<dyn cloud::StorageService>>();
    let body = match store::notations::pinned_body(&surreal, &storage, &notation).await {
        Ok(body) => body,
        Err(store::notations::PinnedBodyError::MissingTemplate { .. }) => return Ok(missing),
        Err(e) => {
            tracing::error!(error = %e, %notation_id, "notation outline template body failed");
            return Ok(failed());
        }
    };
    let doc = views::harvard_outline::parse(&body);
    Ok(NotationOutlineView {
        firm_name,
        title: doc.title.clone(),
        stage_html: Some(views::harvard_outline::stage_html(&doc)),
        back_href: format!("/app/projects/{project_code}"),
        failed: false,
        role,
        logo,
        tokens_href,
    })
}

#[cfg(feature = "server")]
fn notation_kind_visible_to_viewer(
    kind: Option<&str>,
    viewer: Option<store::access::MatterViewer>,
) -> bool {
    viewer.is_some_and(|viewer| {
        viewer.lens() == store::access::ProjectLens::Lawyer
            || kind
                .and_then(rules::kind::Kind::parse)
                .is_none_or(rules::kind::Kind::is_client_visible)
    })
}

/// Load one notation's outline through the caller's own lens.
#[server]
pub async fn get_notation_outline() -> Result<NotationOutlineView, ServerFnError> {
    load().await
}

fn outline_body(view: &NotationOutlineView) -> Element {
    let role = view.role;
    rsx! {
        document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
        document::Stylesheet { href: "{view.tokens_href}" }
        document::Stylesheet { href: HARVARD_OUTLINE_STYLESHEET_HREF }
        document::Script { src: HARVARD_OUTLINE_SCRIPT_HREF, defer: true }
        crate::components::AppNavbar {
            destinations: crate::app_chrome::app_destinations(role),
            logo: view.logo.clone(),
        }
        main { id: "harvard-outline", class: "nav-theme",
            if let Some(stage_html) = view.stage_html.as_ref() {
                document::Title { "{view.firm_name} | Outline | {view.title}" }
                header { class: "page-header",
                    h1 { "{view.title}" }
                    p { a { href: "{view.back_href}", "← Back to project" } }
                }
                div { dangerous_inner_html: "{stage_html}" }
            } else if view.failed {
                document::Title { "{view.firm_name} | Something went wrong" }
                h1 { "Something went wrong" }
                p { "This outline could not be read. The error has been logged." }
            } else {
                document::Title { "{view.firm_name} | Not found" }
                h1 { "Not found" }
                p { "No outline is available at this address." }
            }
        }
    }
}

/// `/app/projects/{project_code}/{notation_id}/outline`.
#[component]
pub fn NotationOutline() -> Element {
    let resource = use_server_future(get_notation_outline)?;
    let view = match &*resource.read() {
        Some(Ok(view)) => view.clone(),
        Some(Err(_)) => {
            return rsx! {
                main { id: "harvard-outline", p { "Failed to load the outline." } }
            }
        }
        None => {
            return rsx! {
                main { id: "harvard-outline", p { "Loading…" } }
            }
        }
    };
    outline_body(&view)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "server")]
    #[test]
    fn trial_prep_outline_is_hidden_from_client_participants_only() {
        use store::access::MatterViewer;
        assert!(!notation_kind_visible_to_viewer(
            Some("trial_prep"),
            Some(MatterViewer::Client)
        ));
        assert!(!notation_kind_visible_to_viewer(
            Some("trial_prep"),
            Some(MatterViewer::ClientDri)
        ));
        assert!(!notation_kind_visible_to_viewer(
            Some("trial_prep"),
            Some(MatterViewer::Clerk)
        ));
        assert!(notation_kind_visible_to_viewer(
            Some("trial_prep"),
            Some(MatterViewer::Lawyer)
        ));
        assert!(notation_kind_visible_to_viewer(
            Some("agreement"),
            Some(MatterViewer::Client)
        ));
    }

    fn found() -> NotationOutlineView {
        NotationOutlineView {
            tokens_href: String::new(),
            firm_name: "Example Law".to_string(),
            title: "Onboarding letter".to_string(),
            stage_html: Some(
                "<article class=\"harvard-stage\" data-harvard-outline>\
                    <header class=\"harvard-stage__chrome\">\
                    <p class=\"harvard-stage__title\">Onboarding letter</p>\
                    </header></article>"
                    .to_string(),
            ),
            back_href: "/app/projects/sample-litigation".to_string(),
            failed: false,
            role: ViewerRole::Client,
            logo: None,
        }
    }

    #[test]
    fn the_href_names_the_matter_and_the_notation() {
        assert_eq!(
            notation_outline_href("sample-litigation", "01234567-89ab-cdef-0123-456789abcdef"),
            "/app/projects/sample-litigation/01234567-89ab-cdef-0123-456789abcdef/outline"
        );
    }

    #[test]
    fn the_found_page_keeps_app_chrome_and_the_harvard_stage_header() {
        let html = dioxus_ssr::render_element(outline_body(&found()));
        assert!(html.contains("aria-label=\"Application\""), "{html}");
        assert!(html.contains("id=\"harvard-outline\""), "{html}");
        assert!(html.contains("data-harvard-outline"), "{html}");
        assert!(html.contains("harvard-stage__chrome"), "{html}");
        assert!(html.contains("harvard-stage__title"), "{html}");
        assert!(html.contains(">Onboarding letter<"), "{html}");
        assert!(
            html.contains("href=\"/app/projects/sample-litigation\""),
            "{html}"
        );
    }

    #[test]
    fn the_page_hoists_the_stage_assets() {
        let src = include_str!("notation_outline.rs");
        assert!(
            src.contains("document::Script { src: HARVARD_OUTLINE_SCRIPT_HREF"),
            "{src}"
        );
        assert!(
            src.contains("document::Stylesheet { href: HARVARD_OUTLINE_STYLESHEET_HREF"),
            "{src}"
        );
    }

    /// A synthetic pleading outline: a preamble, a quotation, a hold, a
    /// lettered subsection, and two open terms.
    #[cfg(feature = "server")]
    const SYNTHETIC: &str = "---\ntitle: Sample Opposition\n---\n\n\
        Plaintiff opposes the motion.\n\n## 1. Introduction\n\n\
        > The standard is de novo.\n\n[Confirm the hearing date.]\n\n\
        ## 2. Argument\n\n> **A. Damages.** Plaintiff seeks `{{custom_money__damages}}` from \
        {{person__defendant.name}}.\n";

    #[cfg(feature = "server")]
    #[test]
    fn notation_document_maps_the_parsed_outline_onto_the_component_data() {
        use crate::components::{NotationBlockKind, NotationRun};

        let data = notation_document(&views::harvard_outline::parse(SYNTHETIC));
        assert_eq!(data.title, "Sample Opposition");
        assert_eq!(data.numbering.as_deref(), Some("Arabic numerals"));
        assert_eq!(data.preamble.len(), 1);
        assert_eq!(data.preamble[0].fragment, "preamble-p1");
        let intro = &data.sections[0];
        assert_eq!(intro.fragment, "section-1");
        assert_eq!(
            intro.blocks.iter().map(|b| b.kind).collect::<Vec<_>>(),
            vec![NotationBlockKind::Quotation, NotationBlockKind::Hold]
        );
        assert_eq!(
            intro.blocks[1].runs,
            vec![NotationRun::Text {
                html: "Confirm the hearing date.".to_string()
            }],
            "the hold's brackets are dropped; the flag marks it"
        );
        let damages = &data.sections[1].sections[0];
        assert!(damages.quoted);
        assert_eq!(damages.fragment, "section-2-a");
        assert_eq!(data.hold_count(), 1);
        assert_eq!(data.open_term_count(), 2);
        assert!(damages.heading.iter().any(|run| matches!(
            run,
            NotationRun::OpenTerm { question, .. } if question == "person__defendant"
        )));
    }

    #[cfg(feature = "server")]
    #[test]
    fn notation_document_ids_are_stable_across_a_reparse() {
        let first = notation_document(&views::harvard_outline::parse(SYNTHETIC));
        let again = notation_document(&views::harvard_outline::parse(SYNTHETIC));
        assert_eq!(first, again);
    }

    #[test]
    fn a_missing_outline_does_not_disclose_the_stage() {
        let html = dioxus_ssr::render_element(outline_body(&NotationOutlineView {
            firm_name: "Example Law".to_string(),
            role: ViewerRole::Client,
            ..NotationOutlineView::default()
        }));
        assert!(html.contains("No outline is available"), "{html}");
        assert!(!html.contains("data-harvard-outline"), "{html}");
        assert!(html.contains("aria-label=\"Application\""), "{html}");
    }
}
