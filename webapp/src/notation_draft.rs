//! `/notations/drafts/{id}` — a pushed notation **draft** (LAW-29).
//!
//! The other half of `navigator notation preview`: a template pushed to
//! the Project it belongs to, rendered by the real portal rather than a
//! second local imitation kept in step with it by hand. Reuses
//! [`crate::notation_preview::NotationPreviewContent`] — the same parsed
//! shape a published notation and a local lint both already render from —
//! but in a different order: **questionnaire first, document body below
//! it**. The respondent answers before there is a document to read; the
//! author previewing the intake is the only other reader, and they are
//! previewing the intake. Leading with the body, as the published page
//! does, is backwards for both.
//!
//! The demo controls are the client-side-only stage
//! [`crate::notation_demo::QuestionnaireDemo`] renders for a local
//! preview — they never save anything. Since LAW-203 a pushed draft may
//! carry the real notation run its push opened ([`InjectedNotationDraft::run_path`]);
//! the page then leads with the hand-off to that run, where answers count.
//! A draft whose push predates the link — or whose run could not open —
//! keeps the "Draft — not run" badge and the demo alone.
//!
//! No auth layer, same as [`crate::notation_preview::NotationPreviewEntry`]
//! — the draft's id is the access control, exactly like a preview link. An
//! expired or unknown id renders not-found.
//!
//! The `{id}` resolves to a live draft in the router's own awaited
//! middleware (`portal::dioxus_app::inject_notation_draft`), mirroring how
//! [`crate::notation_preview`] resolves `{slug}` from a compiled-in `Vec` —
//! not through `ServeConfig::context_providers` and a `consume_context`
//! read inside the render task, the pattern the authenticated,
//! `admin.rs`-routed pages use. A public page giving itself a second,
//! independent `context_providers` closure destabilized the Dioxus
//! render-task pool shared by every public page on this router — a
//! resource conflict, not a logic bug, that surfaced several renders later
//! as a `cucumber` runtime-drop panic with no connection to this file on
//! its face. [`project_draft_source`] is the one piece portal's middleware
//! needs from here; it is `pub` for exactly that call.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{Accordion, CodeBlock, PublicShell};
use crate::harvard_outline::{HARVARD_OUTLINE_SCRIPT_HREF, HARVARD_OUTLINE_STYLESHEET_HREF};
use crate::notation_demo::{questionnaire_error_panel, render_live_template, QuestionnaireDemo};
use crate::notation_preview::NotationPreviewContent;
use crate::notation_workflow::WorkflowDiagram;
use crate::public_chrome::PublicChrome;

/// What `portal::dioxus_app::inject_notation_draft` injects for the render
/// task to read back — a distinct type (rather than a bare
/// `Option<NotationPreviewContent>`) so its *presence* on the request can
/// mean "this was the draft route", the same role
/// [`crate::notation_preview::InjectedNotationPreview`] plays for the
/// preview route. Both routes render through the same shared
/// [`crate::App`], and `content: None` here (an unknown or expired id)
/// has to stay distinguishable from this extension never having been
/// inserted at all (some other route entirely).
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct InjectedNotationDraft {
    /// The resolved draft, or `None` for an unknown or expired id.
    pub content: Option<NotationPreviewContent>,
    /// The lawyer-side path of the real run this draft opened, when it
    /// opened one (LAW-203) — the hand-off the page renders.
    pub run_path: Option<String>,
}

/// The resolved draft, or `None` for an unknown or expired id. `matched`
/// is `true` exactly when the current request was the draft route at all;
/// see [`InjectedNotationDraft`]. `run_path` carries the real run the
/// draft opened, when it opened one — the hand-off target.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct NotationDraftView {
    pub chrome: PublicChrome,
    pub content: Option<NotationPreviewContent>,
    pub run_path: Option<String>,
    pub matched: bool,
}

/// Project a draft's raw Markdown source onto the same content shape a
/// published notation and a local lint both render from. Pure — the same
/// projection `portal::notation_preview_doc::from_markdown` runs, minus
/// the `source_href`/GitHub-link concern a draft has no use for. It lives
/// here, `pub`, rather than in `portal::notation_preview_doc`, because
/// `portal` depends on `webapp` and not the other way around, and
/// `portal`'s draft-resolving middleware needs to call it directly.
#[cfg(feature = "server")]
#[must_use]
pub fn project_draft_source(source: &str) -> NotationPreviewContent {
    let doc = views::harvard_outline::parse(source);
    let frontmatter = doc.frontmatter.clone().unwrap_or_default();
    let origin_url = views::harvard_outline::frontmatter_field(&frontmatter, "origin_url");
    let (demo_questions, questionnaire_error) =
        match views::questionnaire_preview::parse(&frontmatter) {
            Ok(questions) => (
                questions
                    .into_iter()
                    .map(|q| {
                        let interactive = q.is_interactive();
                        crate::notation_demo::DemoQuestion {
                            code: q.code,
                            answer_type: q.answer_type,
                            prompt: q.prompt,
                            choices: q.choices,
                            interactive,
                            role_label: q.role_label,
                            group: q.group,
                            help: q.help,
                            columns: q
                                .columns
                                .into_iter()
                                .map(|column| crate::notation_demo::DemoColumn {
                                    name: column.name,
                                    label: column.label,
                                    value_type: column.value_type,
                                })
                                .collect(),
                        }
                    })
                    .collect(),
                None,
            ),
            // Loud, not silent: a draft whose declared questionnaire cannot be
            // read renders the failure panel, never an empty demo section.
            Err(failure) => (Vec::new(), Some(failure.to_string())),
        };
    let demo_workflow = views::workflow_preview::parse(&frontmatter)
        .into_iter()
        .map(|s| crate::notation_workflow::WorkflowStateView {
            name: s.name,
            transitions: s.transitions.into_iter().map(|t| (t.event, t.to)).collect(),
        })
        .collect();
    NotationPreviewContent {
        title: doc.title.clone(),
        source_href: String::new(),
        demo_questions,
        demo_workflow,
        frontmatter,
        stage_html: views::harvard_outline::stage_html(&doc),
        origin_url,
        questionnaire_error,
    }
}

/// Read back the draft `portal::dioxus_app::inject_notation_draft` already
/// resolved for `{id}` — mirroring
/// [`crate::notation_preview::notation_preview_view`]'s synchronous
/// `Extension` read exactly, since that middleware's awaited store lookup
/// is what makes this one synchronous. `matched` is `false` (and `content`
/// `None`) on every request that was not the draft route at all — this
/// server fn runs unconditionally on every render through the shared
/// [`crate::App`], including a preview request, since Dioxus hooks run
/// every pass regardless of which branch's result [`crate::App`] uses.
#[server]
pub async fn notation_draft_view() -> Result<NotationDraftView, ServerFnError> {
    let injected = dioxus_fullstack_core::FullstackContext::extract::<
        axum::Extension<InjectedNotationDraft>,
        _,
    >()
    .await
    .ok();
    Ok(NotationDraftView {
        chrome: crate::public_chrome::firm_public_chrome_from_context().await,
        content: injected
            .as_ref()
            .and_then(|axum::Extension(d)| d.content.clone()),
        run_path: injected
            .as_ref()
            .and_then(|axum::Extension(d)| d.run_path.clone()),
        matched: injected.is_some(),
    })
}

/// The pure draft page: questionnaire first, rendered body below it, and —
/// when the push opened a real run (LAW-203) — the hand-off to it, so the
/// preview's answerable surface is one click from the shareable draft.
/// Prop-driven, so it server-renders and unit-tests without a server
/// future.
#[component]
pub fn NotationDraftPage(
    chrome: PublicChrome,
    content: Option<NotationPreviewContent>,
    run_path: Option<String>,
) -> Element {
    let Some(content) = content else {
        return rsx! {
            PublicShell {
                header: rsx! {},
                footer: rsx! {},
                document::Title { "{chrome.brand_name} | Draft not found" }
                article { class: "notation-draft notation-draft--missing",
                    h1 { "Draft not found" }
                    p { "This draft does not exist, or it has expired." }
                }
            }
        };
    };
    #[cfg(not(target_arch = "wasm32"))]
    let title = {
        let head_title = format!("{} | Draft | {}", chrome.brand_name, content.title);
        rsx! { document::Title { "{head_title}" } }
    };
    #[cfg(target_arch = "wasm32")]
    let title = rsx! {};
    rsx! {
        PublicShell { header: rsx! {}, footer: rsx! {},
            {title}
            document::Stylesheet { href: HARVARD_OUTLINE_STYLESHEET_HREF }
            document::Script { src: HARVARD_OUTLINE_SCRIPT_HREF, defer: true }
            article { class: "notation-draft",
                header { class: "notation-draft__header",
                    p { class: "notation-draft__badge",
                        if run_path.is_some() { "Draft — run open" } else { "Draft — not run" }
                    }
                    h1 { "{content.title}" }
                }
                if let Some(run_path) = run_path.as_ref() {
                    section { class: "notation-draft__handoff",
                        p {
                            "This preview opened a real run. The questionnaire is answered "
                            "there — what you try on this page stays here."
                        }
                        a { href: "{run_path}", "Open the real questionnaire" }
                    }
                }
                if let Some(error) = &content.questionnaire_error {
                    {questionnaire_error_panel(error)}
                }
                if !content.demo_questions.is_empty() {
                    section { class: "notation-draft__questionnaire",
                        h2 { "Questionnaire" }
                        QuestionnaireDemo {
                            questions: content.demo_questions.clone(),
                            template_html: content.stage_html.clone(),
                            embed_document: false,
                        }
                    }
                }
                section { class: "notation-draft__body",
                    h2 { "Document" }
                    div {
                        dangerous_inner_html: "{render_live_template(&content.stage_html, &content.demo_questions, &std::collections::BTreeMap::new())}"
                    }
                }
                if !content.demo_workflow.is_empty() {
                    section { class: "notation-draft__workflow",
                        h2 { "Workflow" }
                        WorkflowDiagram { states: content.demo_workflow.clone() }
                    }
                }
                if !content.frontmatter.is_empty() {
                    Accordion { title: "Frontmatter".to_string(),
                        CodeBlock { code: content.frontmatter.clone(), lang: "yaml".to_string() }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notation_demo::DemoQuestion;
    use crate::notation_workflow::WorkflowStateView;

    fn ssr(app: fn() -> Element) -> String {
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    fn draft_content() -> NotationPreviewContent {
        NotationPreviewContent {
            title: "Engagement Letter".to_string(),
            source_href: String::new(),
            frontmatter: "title: Engagement Letter\ncode: engagement__letter".to_string(),
            stage_html: "<article class=\"harvard-stage\" data-harvard-outline>\
                    <section class=\"harvard-unit harvard-unit--depth-1\" data-harvard-path=\"I\">\
                    <h2>Scope of the engagement</h2></section></article>"
                .to_string(),
            origin_url: None,
            demo_questions: vec![DemoQuestion {
                code: "custom_text__client_name".to_string(),
                answer_type: "custom_text".to_string(),
                prompt: "What is your name?".to_string(),
                choices: Vec::new(),
                interactive: true,
                ..DemoQuestion::default()
            }],
            demo_workflow: vec![WorkflowStateView {
                name: "BEGIN".to_string(),
                transitions: vec![("intake_submitted".to_string(), "lawyer_review".to_string())],
            }],
            questionnaire_error: None,
        }
    }

    /// LAW-29's ordering fix: the questionnaire renders before the body,
    /// not after — the respondent answers before there is a document to
    /// read, and the author previewing intake is the other reader.
    #[test]
    fn the_questionnaire_renders_before_the_document_body() {
        fn app() -> Element {
            let chrome = PublicChrome {
                brand_name: "Neon Law".to_string(),
                ..PublicChrome::default()
            };
            rsx! { NotationDraftPage { chrome, content: Some(draft_content()), run_path: None } }
        }
        let out = ssr(app);
        let questionnaire_at = out
            .find("Try answering this")
            .expect("questionnaire renders");
        let body_at = out
            .find("Scope of the engagement")
            .expect("document body renders");
        assert!(
            questionnaire_at < body_at,
            "questionnaire must render before the body: {out}"
        );
    }

    /// LAW-150: the page shows the document as the recipient will read it.
    /// The "This is a preview" sentence never renders, whatever the notation
    /// kind; the CLI's own output says the draft was not run.
    #[test]
    fn the_page_carries_no_preview_banner_for_any_notation_kind() {
        fn app() -> Element {
            let chrome = PublicChrome {
                brand_name: "Neon Law".to_string(),
                ..PublicChrome::default()
            };
            rsx! { NotationDraftPage { chrome, content: Some(draft_content()), run_path: None } }
        }
        let out = ssr(app);
        assert!(out.contains("Scope of the engagement"), "{out}");
        for banner in [
            "This is a preview",
            "No notation was created",
            "no workflow started",
            "nothing was filed",
            "notation-draft__notice",
        ] {
            assert!(!out.contains(banner), "{banner:?} must not render: {out}");
        }
    }

    #[test]
    fn an_unknown_or_expired_draft_renders_not_found() {
        fn app() -> Element {
            let chrome = PublicChrome {
                brand_name: "Neon Law".to_string(),
                ..PublicChrome::default()
            };
            rsx! { NotationDraftPage { chrome, content: None, run_path: None } }
        }
        let out = ssr(app);
        assert!(out.contains("Draft not found"), "{out}");
    }

    /// A draft whose declared questionnaire cannot be parsed renders the
    /// visible failure panel above the document — the author sees the
    /// break, never a silently empty intake.
    #[test]
    fn a_draft_parse_failure_renders_the_visible_error_panel() {
        fn app() -> Element {
            let chrome = PublicChrome {
                brand_name: "Neon Law".to_string(),
                ..PublicChrome::default()
            };
            let content = NotationPreviewContent {
                demo_questions: Vec::new(),
                questionnaire_error: Some(
                    "questionnaire failed to parse: a questionnaire is declared but no steps resolve from `BEGIN`"
                        .to_string(),
                ),
                ..draft_content()
            };
            rsx! { NotationDraftPage { chrome, content: Some(content), run_path: None } }
        }
        let out = ssr(app);
        assert!(
            out.contains("Questionnaire failed to parse"),
            "panel headline: {out}"
        );
        assert!(out.contains(r#"role="alert""#), "announced: {out}");
    }

    /// `project_draft_source` maps an unreadable questionnaire onto the
    /// panel field — the parse failure surfaces, it is not swallowed.
    #[test]
    fn a_draft_with_a_broken_questionnaire_carries_the_parse_failure() {
        let source =
            "---\ntitle: Broken\nquestionnaire:\n  BEGIN:\n    _: END\n  END: {}\n---\n\nBody.\n";
        let content = project_draft_source(source);
        let error = content
            .questionnaire_error
            .expect("a declared-but-unreadable questionnaire fails loud");
        assert!(error.contains("no steps resolve"), "{error}");
        assert!(content.demo_questions.is_empty());
    }

    /// A draft with no questionnaire block at all stays failure-free —
    /// a letter drafts with no intake, not with an error.
    #[test]
    fn a_draft_with_no_questionnaire_block_carries_no_error() {
        let content = project_draft_source("---\ntitle: Letter\n---\n\nBody.\n");
        assert!(content.questionnaire_error.is_none());
        assert!(content.demo_questions.is_empty());
    }

    /// LAW-203's hand-off: a draft that opened a real run leads with the
    /// link to it — the answerable surface — and says honestly that the
    /// demo on this page is not where answers count.
    #[test]
    fn a_run_backed_draft_hands_off_to_the_real_questionnaire() {
        fn app() -> Element {
            let chrome = PublicChrome {
                brand_name: "Neon Law".to_string(),
                ..PublicChrome::default()
            };
            rsx! {
                NotationDraftPage {
                    chrome,
                    content: Some(draft_content()),
                    run_path: Some("/app/lawyer/notations/0123456789/step".to_string()),
                }
            }
        }
        let out = ssr(app);
        assert!(
            out.contains("href=\"/app/lawyer/notations/0123456789/step\""),
            "the hand-off links the run's step page: {out}"
        );
        assert!(out.contains("Open the real questionnaire"), "{out}");
        assert!(out.contains("Draft — run open"), "{out}");
        // The hand-off leads: it renders before the demo questionnaire.
        let handoff_at = out
            .find("notation-draft__handoff")
            .expect("hand-off renders");
        let demo_at = out.find("Try answering this").expect("demo renders");
        assert!(handoff_at < demo_at, "the hand-off leads the page: {out}");
    }

    /// A run-less draft keeps the "not run" badge and renders no hand-off.
    #[test]
    fn a_run_less_draft_keeps_the_not_run_badge_and_no_handoff() {
        fn app() -> Element {
            let chrome = PublicChrome {
                brand_name: "Neon Law".to_string(),
                ..PublicChrome::default()
            };
            rsx! { NotationDraftPage { chrome, content: Some(draft_content()), run_path: None } }
        }
        let out = ssr(app);
        assert!(out.contains("Draft — not run"), "{out}");
        assert!(!out.contains("notation-draft__handoff"), "{out}");
    }
}
