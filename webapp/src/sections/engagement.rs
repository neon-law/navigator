//! The `engagement_letter` and `document_body` sections of a Matter Page.
//!
//! `engagement_letter` shows the matter's executed engagement letter: each
//! document the [Onboarding](../../../docs/glossary/onboarding.md) Notation
//! run filed, or any document a lawyer filed under a kind that opens a matter
//! (`rules::kind::Kind::opens_a_matter`). A client reads only the documents a
//! lawyer marked client-visible. Before anything is filed, the section shows
//! the page's `draft:`, labelled as a draft. Once a letter is filed, the draft
//! never stands in for it, even on a lens that may not open the filed copy.
//!
//! `document_body` renders the page's `draft:` as block-structured Markdown,
//! through the same [`NotationDocument`] renderer `notation_document` uses.
//!
//! Every value here is display-ready and wasm-safe. The server half builds
//! them, already scoped to the lens.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{Callout, CalloutTone, NotationDocument, NotationDocumentData};

/// The DOM id of the engagement letter's draft, distinct from the
/// `notation_document` section's so both can share one page.
pub const ENGAGEMENT_DRAFT_ID: &str = "engagement-letter-draft";

/// The DOM id of the `document_body` section's rendered document.
pub const DOCUMENT_BODY_ID: &str = "document-body";

/// The page's `draft:`, read once for every section that shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageDraft {
    /// The page names no draft, or the draft names no stored Template.
    #[default]
    Missing,
    /// The draft's body, parsed into its outline.
    Body(NotationDocumentData),
    /// The draft's stored body could not be read, so no text is shown.
    Unreadable,
}

/// One filed engagement letter, as `engagement_letter` lists it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LetterDocument {
    /// The Onboarding Draft's title, or the filename when no Notation run
    /// filed it.
    pub title: String,
    /// The filename, when the title is not already the filename.
    pub detail: String,
    /// How the letter stands: its signing record, or the Notation run's
    /// client phrase, or `On file`.
    pub status: String,
    /// The document's page.
    pub href: String,
}

/// The `engagement_letter` section's data.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngagementLetterView {
    /// The filed letters this lens may open, newest first.
    pub filed: Vec<LetterDocument>,
    /// The page's draft, shown only when nothing is filed at all.
    pub draft: PageDraft,
}

impl EngagementLetterView {
    /// The view for a lens: the letters it may open, else the draft — but
    /// only when no letter is filed at all. A letter filed for the lawyer
    /// lens alone leaves the client with the empty state, never a draft that
    /// reads as if nothing were signed.
    #[must_use]
    pub fn new(filed_any: bool, filed: Vec<LetterDocument>, draft: &PageDraft) -> Self {
        Self {
            draft: if filed_any {
                PageDraft::Missing
            } else {
                draft.clone()
            },
            filed,
        }
    }

    fn state(&self) -> &'static str {
        if !self.filed.is_empty() {
            return "executed";
        }
        match self.draft {
            PageDraft::Missing => "empty",
            PageDraft::Body(_) => "draft",
            PageDraft::Unreadable => "unreadable",
        }
    }
}

/// The alert shown in place of a draft whose stored body could not be read.
#[component]
fn UnreadableDraft() -> Element {
    rsx! {
        div { class: "matter-section__error", role: "alert",
            Callout { tone: CalloutTone::Danger, title: "This draft could not be read".to_string(),
                p { "Its stored text could not be loaded, so it is not shown. The error has been logged." }
            }
        }
    }
}

/// The `engagement_letter` section.
#[component]
pub fn EngagementLetterSection(view: EngagementLetterView) -> Element {
    let state = view.state();
    rsx! {
        section {
            class: "matter-section",
            "data-section": "engagement_letter",
            "data-state": state,
            aria_labelledby: "section-engagement_letter",
            crate::components::Card {
                header: rsx! { h2 { id: "section-engagement_letter", "Engagement letter" } },
                if !view.filed.is_empty() {
                    ul { class: "matter-section__rows",
                        for letter in view.filed.iter() {
                            li { class: "matter-section__row",
                                a { class: "nav-link", href: "{letter.href}", strong { "{letter.title}" } }
                                if !letter.detail.is_empty() {
                                    " · "
                                    span { "{letter.detail}" }
                                }
                                " "
                                span { class: "status-chip", "{letter.status}" }
                            }
                        }
                    }
                } else {
                    match view.draft {
                        PageDraft::Missing => rsx! {
                            p { class: "matter-section__empty", "No engagement letter is on file yet." }
                        },
                        PageDraft::Unreadable => rsx! { UnreadableDraft {} },
                        PageDraft::Body(document) => rsx! {
                            Callout { tone: CalloutTone::Info, title: "Draft".to_string(),
                                p {
                                    "No engagement letter has been signed and filed yet. This is the draft \
                                     your legal team prepared; it is not in effect."
                                }
                            }
                            NotationDocument { document, id: ENGAGEMENT_DRAFT_ID.to_string(), open: true }
                        },
                    }
                }
            }
        }
    }
}

/// The `document_body` section.
#[component]
pub fn DocumentBodySection(draft: PageDraft) -> Element {
    let state = match draft {
        PageDraft::Missing => "empty",
        PageDraft::Body(_) => "body",
        PageDraft::Unreadable => "unreadable",
    };
    rsx! {
        section {
            class: "matter-section",
            "data-section": "document_body",
            "data-state": state,
            aria_labelledby: "section-document_body",
            crate::components::Card {
                header: rsx! { h2 { id: "section-document_body", "Document" } },
                match draft {
                    PageDraft::Missing => rsx! {
                        p { class: "matter-section__empty", "No draft is attached to this page yet." }
                    },
                    PageDraft::Unreadable => rsx! { UnreadableDraft {} },
                    PageDraft::Body(document) => rsx! {
                        NotationDocument { document, id: DOCUMENT_BODY_ID.to_string(), open: true }
                    },
                }
            }
        }
    }
}

/// The outline of a draft body, through the parser and renderer
/// `notation_document` uses.
#[cfg(feature = "server")]
#[must_use]
pub fn page_draft(body: &str) -> PageDraft {
    PageDraft::Body(crate::notation_outline::notation_document(
        &views::harvard_outline::parse(body),
    ))
}

/// One Onboarding Notation run on the matter: the Notation and its Draft's
/// title.
#[cfg(feature = "server")]
#[derive(Clone, Debug)]
pub struct OnboardingRun {
    pub notation_id: uuid::Uuid,
    pub title: String,
    pub state: String,
}

/// The Onboarding run whose document key holds `asset`'s bytes, if any.
#[cfg(feature = "server")]
fn filing_run<'a>(
    asset: &store::assets::Asset,
    runs: &'a [OnboardingRun],
) -> Option<&'a OnboardingRun> {
    let keys = || {
        std::iter::once(asset.storage_key.as_str()).chain(asset.secondary_storage_key.as_deref())
    };
    runs.iter().find(|run| {
        let generated = store::notations::document_pdf_storage_key(run.notation_id);
        let signed = store::notations::signed_document_storage_key(run.notation_id);
        keys().any(|key| key == generated || key == signed)
    })
}

/// Whether `asset` is a filed engagement letter: an Onboarding run filed it,
/// or a lawyer filed it under a kind that opens a matter.
#[cfg(feature = "server")]
fn is_letter(asset: &store::assets::Asset, run: Option<&OnboardingRun>) -> bool {
    run.is_some() || store::projects::template_opens_a_matter(asset.kind.as_deref())
}

/// How a filed letter stands, from facts on file only: its signing record,
/// else the run's client phrase, else that it is on file. Never "signed"
/// without a signing record.
#[cfg(feature = "server")]
fn letter_status(asset: &store::assets::Asset, run: Option<&OnboardingRun>) -> String {
    if let Some(signing) = asset.signing {
        let how = match signing.method {
            store::documents::SigningMethod::WetInk => "in wet ink",
            store::documents::SigningMethod::Docusign => "through DocuSign",
        };
        return format!("Signed {how} on {}", signing.signed_on.format("%Y-%m-%d"));
    }
    run.map_or_else(
        || "On file".to_string(),
        |run| {
            workflows::client_phrase_for(&run.state)
                .where_this_is
                .to_string()
        },
    )
}

/// The filed engagement letters on a matter: whether any is filed at all,
/// and the ones `lens` may open, newest first. A client opens only what a
/// lawyer marked client-visible, the same rule `download_list` applies.
#[cfg(feature = "server")]
#[must_use]
pub fn filed_letters(
    code: &str,
    assets: &[store::assets::Asset],
    runs: &[OnboardingRun],
    lens: super::PageLens,
) -> (bool, Vec<LetterDocument>) {
    let mut filed_any = false;
    let mut letters: Vec<(&store::assets::Asset, LetterDocument)> = assets
        .iter()
        .map(|asset| (asset, filing_run(asset, runs)))
        .filter(|(asset, run)| is_letter(asset, *run))
        .inspect(|_| filed_any = true)
        .filter(|(asset, _)| {
            lens != super::PageLens::Client
                || asset.visibility == store::documents::visibility::CLIENT
        })
        .map(|(asset, run)| {
            let filename = asset
                .filename
                .clone()
                .or_else(|| asset.slug.clone())
                .unwrap_or_else(|| "Engagement letter".to_string());
            let (title, detail) = match run {
                Some(run) => (run.title.clone(), filename),
                None => (filename, String::new()),
            };
            (
                asset,
                LetterDocument {
                    title,
                    detail,
                    status: letter_status(asset, run),
                    href: format!("/app/projects/{code}/documents/{}", asset.id),
                },
            )
        })
        .collect();
    letters.sort_by_key(|(asset, _)| std::cmp::Reverse(asset.inserted_at));
    (
        filed_any,
        letters.into_iter().map(|(_, letter)| letter).collect(),
    )
}

/// Every Onboarding Notation run on the matter.
///
/// # Errors
///
/// A store read failed.
#[cfg(feature = "server")]
pub async fn onboarding_runs(
    surreal: &store::surreal::SurrealDb,
    project_id: uuid::Uuid,
) -> Result<Vec<OnboardingRun>, String> {
    let notations = store::notations::list_by_project(surreal, project_id)
        .await
        .map_err(|e| e.to_string())?;
    let template_ids: Vec<uuid::Uuid> = notations.iter().map(|n| n.template_id).collect();
    let templates = store::templates::find_by_ids(surreal, &template_ids)
        .await
        .map_err(|e| e.to_string())?;
    Ok(notations
        .iter()
        .filter_map(|notation| {
            let template = templates.iter().find(|t| t.id == notation.template_id)?;
            store::projects::template_opens_a_matter(template.kind.as_deref()).then(|| {
                OnboardingRun {
                    notation_id: notation.id,
                    title: template.title.clone(),
                    state: notation.state.clone(),
                }
            })
        })
        .collect())
}

/// The page's `draft:`, read from the Template it names. A draft that names
/// no stored Template is [`PageDraft::Missing`]; a body that fails to read is
/// [`PageDraft::Unreadable`] and is logged.
///
/// # Errors
///
/// The Template lookup failed.
#[cfg(feature = "server")]
pub async fn load_page_draft(
    surreal: &store::surreal::SurrealDb,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
    project_id: uuid::Uuid,
    draft: Option<&str>,
) -> Result<PageDraft, String> {
    let Some(code) = draft.and_then(super::notations::draft_code) else {
        return Ok(PageDraft::Missing);
    };
    let Some(template) = store::templates::resolve_exact(surreal, Some(project_id), &code)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(PageDraft::Missing);
    };
    Ok(
        match store::templates::body(surreal, storage, &template).await {
            Ok(body) => page_draft(&body),
            Err(error) => {
                tracing::error!(%error, template_id = %template.id, "page draft body refused");
                PageDraft::Unreadable
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::{EngagementLetterView, LetterDocument, PageDraft};
    use crate::components::NotationDocumentData;
    use crate::sections::tests::render;
    use crate::sections::SectionView;

    fn outline() -> NotationDocumentData {
        NotationDocumentData {
            title: "Sample Engagement Letter".into(),
            ..NotationDocumentData::default()
        }
    }

    #[test]
    fn a_filed_letter_lists_its_document_and_never_its_draft() {
        let view = EngagementLetterView::new(
            true,
            vec![LetterDocument {
                title: "Sample Engagement Letter".into(),
                detail: "letter_signed.pdf".into(),
                status: "Signed through DocuSign on 2026-10-01".into(),
                href: "/app/projects/sample-transactional/documents/0190".into(),
            }],
            &PageDraft::Body(outline()),
        );
        let html = render(SectionView::EngagementLetter(view));
        assert!(html.contains("data-state=\"executed\""), "{html}");
        assert!(
            html.contains("href=\"/app/projects/sample-transactional/documents/0190\""),
            "{html}"
        );
        assert!(html.contains("Signed through DocuSign"), "{html}");
        assert!(!html.contains("engagement-letter-draft"), "{html}");
        assert!(!html.contains("matter-section__empty"), "{html}");
    }

    #[test]
    fn with_nothing_filed_the_draft_shows_with_a_draft_label() {
        let view = EngagementLetterView::new(false, Vec::new(), &PageDraft::Body(outline()));
        let html = render(SectionView::EngagementLetter(view));
        assert!(html.contains("data-state=\"draft\""), "{html}");
        assert!(html.contains(">Draft<"), "{html}");
        assert!(html.contains("it is not in effect"), "{html}");
        assert!(html.contains("id=\"engagement-letter-draft\""), "{html}");
        assert!(html.contains("Sample Engagement Letter"), "{html}");
    }

    #[test]
    fn a_letter_filed_out_of_this_lens_hides_the_draft_too() {
        let view = EngagementLetterView::new(true, Vec::new(), &PageDraft::Body(outline()));
        assert_eq!(view.draft, PageDraft::Missing);
        let html = render(SectionView::EngagementLetter(view));
        assert!(html.contains("data-state=\"empty\""), "{html}");
        assert!(
            html.contains("No engagement letter is on file yet."),
            "{html}"
        );
    }

    #[test]
    fn an_unreadable_draft_renders_an_alert_and_no_text() {
        for view in [
            SectionView::EngagementLetter(EngagementLetterView::new(
                false,
                Vec::new(),
                &PageDraft::Unreadable,
            )),
            SectionView::DocumentBody(PageDraft::Unreadable),
        ] {
            let html = render(view);
            assert!(html.contains("role=\"alert\""), "{html}");
            assert!(!html.contains("notation-document__body"), "{html}");
        }
    }

    #[cfg(feature = "server")]
    mod server {
        use super::super::{filed_letters, page_draft, OnboardingRun, PageDraft};
        use crate::sections::tests::render;
        use crate::sections::{NotationDocumentView, PageLens, SectionView};

        const DRAFT: &str = "---\ntitle: Sample Engagement Letter\n---\n\n\
            Dear Sample Client,\n\n## I. Scope\n\nThe firm will draft one sample agreement.\n\n\
            - one synthetic review\n- one synthetic call\n\n## II. Fees\n\nA flat sample fee.\n";

        fn asset(
            filename: &str,
            kind: &str,
            visibility: &str,
            minutes_ago: i64,
        ) -> store::assets::Asset {
            let at = chrono::Utc::now() - chrono::Duration::minutes(minutes_ago);
            store::assets::Asset {
                id: uuid::Uuid::now_v7(),
                storage_key: format!("projects/sample-transactional/documents/{filename}"),
                secondary_storage_key: None,
                content_type: "application/pdf".into(),
                byte_size: 0,
                sha256_hex: String::new(),
                project_id: None,
                filename: Some(filename.into()),
                kind: Some(kind.into()),
                source: Some("upload".into()),
                received_at: None,
                description: None,
                visibility: visibility.into(),
                slug: None,
                published_at: None,
                source_message_id: None,
                source_sender: None,
                source_received_at: None,
                source_subject: None,
                metadata: None,
                derived_from: None,
                transcript_quality: None,
                signing: None,
                notarization: Vec::new(),
                deleted_at: None,
                purge_after: None,
                deleted_by_person_id: None,
                deleted_commit_sha: None,
                legal_hold_at: None,
                purged_at: None,
                inserted_at: at,
                updated_at: at,
            }
        }

        fn run() -> OnboardingRun {
            OnboardingRun {
                notation_id: uuid::Uuid::now_v7(),
                title: "Sample Engagement Letter".into(),
                state: "END".into(),
            }
        }

        #[test]
        fn an_executed_letter_shows_on_the_client_and_lawyer_lenses() {
            let run = run();
            let mut generated = asset("document.pdf", "unclassified", "client", 5);
            generated.secondary_storage_key = Some(store::notations::signed_document_storage_key(
                run.notation_id,
            ));
            generated.signing = Some(store::documents::Signing {
                method: store::documents::SigningMethod::Docusign,
                signed_on: chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
            });
            let assets = vec![
                generated,
                asset("sample-memo.pdf", "unclassified", "client", 1),
            ];
            for lens in [PageLens::Client, PageLens::Lawyer] {
                let (filed_any, letters) = filed_letters(
                    "sample-transactional",
                    &assets,
                    std::slice::from_ref(&run),
                    lens,
                );
                assert!(filed_any);
                assert_eq!(letters.len(), 1, "{lens:?}: {letters:?}");
                assert_eq!(letters[0].title, "Sample Engagement Letter");
                assert_eq!(letters[0].detail, "document.pdf");
                assert_eq!(letters[0].status, "Signed through DocuSign on 2026-10-01");
                assert_eq!(
                    letters[0].href,
                    format!(
                        "/app/projects/sample-transactional/documents/{}",
                        assets[0].id
                    )
                );
            }
        }

        #[test]
        fn a_client_never_opens_an_internal_letter_but_learns_one_is_filed() {
            let assets = vec![
                asset("onboarding-internal.pdf", "onboarding", "internal", 1),
                asset("onboarding-client.pdf", "onboarding", "client", 9),
            ];
            let (_, lawyer) = filed_letters("m", &assets, &[], PageLens::Lawyer);
            let titles: Vec<&str> = lawyer.iter().map(|l| l.title.as_str()).collect();
            assert_eq!(titles, ["onboarding-internal.pdf", "onboarding-client.pdf"]);
            assert_eq!(lawyer[0].status, "On file");
            let (filed_any, client) = filed_letters("m", &assets[..1], &[], PageLens::Client);
            assert!(filed_any);
            assert!(client.is_empty());
            let (none, _) = filed_letters(
                "m",
                &[asset("memo.pdf", "unclassified", "client", 1)],
                &[],
                PageLens::Client,
            );
            assert!(!none);
        }

        #[test]
        fn an_unsigned_run_document_reads_with_its_client_phrase() {
            let run = OnboardingRun {
                state: "client_review".into(),
                ..run()
            };
            let mut generated = asset("document.pdf", "unclassified", "client", 1);
            generated.storage_key = store::notations::document_pdf_storage_key(run.notation_id);
            let (_, letters) = filed_letters(
                "m",
                &[generated],
                std::slice::from_ref(&run),
                PageLens::Client,
            );
            assert_eq!(
                letters[0].status,
                workflows::client_phrase_for("client_review").where_this_is
            );
        }

        /// `document_body` renders the draft's headings and lists with the
        /// renderer `notation_document` uses, so the two show the same
        /// outline.
        #[test]
        fn document_body_renders_the_draft_like_notation_document() {
            let PageDraft::Body(document) = page_draft(DRAFT) else {
                panic!("a body parses");
            };
            let body = render(SectionView::DocumentBody(PageDraft::Body(document.clone())));
            let notation = render(SectionView::NotationDocument(
                NotationDocumentView::showing(crate::sections::DocumentState::Notation, document),
            ));
            assert!(body.contains("data-section=\"document_body\""), "{body}");
            assert!(body.contains("id=\"document-body\""), "{body}");
            assert!(!body.contains("matter-section__empty"), "{body}");
            for needle in [
                "Sample Engagement Letter",
                "id=\"section-i\"",
                "id=\"section-ii\"",
                "one synthetic review",
                "one synthetic call",
            ] {
                assert!(
                    body.contains(needle),
                    "document_body lacks {needle}: {body}"
                );
                assert!(
                    notation.contains(needle),
                    "notation_document lacks {needle}"
                );
            }
            let outline = |html: &str| {
                let start = html.find("notation-document__body").unwrap();
                html[start..].to_string()
            };
            assert!(
                notation.contains(&outline(&body)[..200]),
                "the rendered outline matches: {body}\n---\n{notation}"
            );
        }
    }
}
