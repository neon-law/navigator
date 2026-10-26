//! The Notation sections of a Matter Page: `notation_document`,
//! `notation_list`, `notation_checklist`, and the client lens's
//! `decision_queue`.
//!
//! `notation_document` renders one Notation's body as a
//! [`NotationDocument`](crate::components::NotationDocument): the Notation
//! opened from the page's `draft:` (or, with no `draft:`, the matter's newest
//! Notation). Its body comes through `store::notations::pinned_body`, which
//! refuses bytes that do not hash to the digest the Notation pinned. A refused
//! body renders a visible error and no text. On the lawyer lens, a page whose
//! Draft has no Notation yet previews the Draft's current version instead.
//!
//! `notation_list` shows every Notation on the matter with the phrase the
//! client phrase table (`workflows::client_phrase_for`) gives its state.
//! `notation_checklist` derives one step per hold and blank of the same body
//! `notation_document` shows ([`super::checklist`]), and shows nothing from a
//! body that failed its digest. On the client lens `decision_queue` names the
//! one next step that is the client's to take.
//!
//! Every value here is display-ready and wasm-safe; the server half
//! ([`load_notations`]) builds them, already scoped to the lens.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use super::checklist::ChecklistStep;
use crate::components::{
    Badge, BadgeTone, Callout, CalloutTone, Empty, NotationDocument, NotationDocumentData, Panel,
};

/// What `notation_document` has to show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DocumentState {
    /// No Notation, and no Draft this reader may preview.
    #[default]
    Empty,
    /// The Notation's body, verified against its pinned digest.
    Notation,
    /// The page's Draft, previewed on the lawyer lens before any Notation is
    /// opened from it.
    DraftPreview,
    /// The stored body did not verify, so no text is shown.
    Unverified,
}

/// The `notation_document` section's data.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotationDocumentView {
    pub state: DocumentState,
    /// The outline to render; empty unless `state` is `Notation` or
    /// `DraftPreview`.
    pub document: NotationDocumentData,
}

impl NotationDocumentView {
    /// A view that shows `document` in `state`.
    #[must_use]
    pub fn showing(state: DocumentState, document: NotationDocumentData) -> Self {
        Self { state, document }
    }

    /// The fail-closed view: a body that did not verify shows no text.
    #[must_use]
    pub fn unverified() -> Self {
        Self {
            state: DocumentState::Unverified,
            document: NotationDocumentData::default(),
        }
    }
}

/// The DOM id of the rendered disclosure, so a link can land on the whole
/// document.
pub const NOTATION_DOCUMENT_ID: &str = "notation-document";

/// The `notation_document` section.
#[component]
pub fn NotationDocumentSection(view: NotationDocumentView) -> Element {
    rsx! {
        section {
            class: "matter-section",
            "data-section": "notation_document",
            "data-state": match view.state {
                DocumentState::Empty => "empty",
                DocumentState::Notation => "notation",
                DocumentState::DraftPreview => "draft-preview",
                DocumentState::Unverified => "unverified",
            },
            aria_labelledby: "section-notation_document-title",
            Panel { id: "section-notation_document".to_string(), title: "Document".to_string(),
                match view.state {
                    DocumentState::Empty => rsx! {
                        div { class: "matter-section__empty",
                            Empty { title: "No document is ready to read here yet.".to_string() }
                        }
                    },
                    DocumentState::Unverified => rsx! {
                        div { class: "matter-section__error", role: "alert",
                            Callout { tone: CalloutTone::Danger, title: "This document could not be verified".to_string(),
                                p {
                                    "Its stored text does not match the version this Notation was opened from, so \
                                     it is not shown. The error has been logged."
                                }
                            }
                        }
                    },
                    DocumentState::DraftPreview => rsx! {
                        Callout { tone: CalloutTone::Info, title: "Draft preview".to_string(),
                            p { "No Notation has been opened from this Draft yet. A client does not see this preview." }
                        }
                        NotationDocument {
                            document: view.document.clone(),
                            id: NOTATION_DOCUMENT_ID.to_string(),
                            open: true,
                        }
                    },
                    DocumentState::Notation => rsx! {
                        NotationDocument {
                            document: view.document.clone(),
                            id: NOTATION_DOCUMENT_ID.to_string(),
                            open: true,
                        }
                    },
                }
            }
        }
    }
}

/// One Notation on the matter, as `notation_list` shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotationListRow {
    /// The Draft's title.
    pub title: String,
    /// Where it stands, from the client phrase table.
    pub phrase: String,
    /// What happens next, from the same table.
    pub next: String,
    /// The raw workflow state, on the lawyer lens only.
    pub state: String,
    /// The Notation's outline page.
    pub href: String,
}

/// The `notation_checklist` section's data.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChecklistView {
    /// Whose body the steps came from: the same state `notation_document`
    /// shows. `Unverified` carries no steps.
    pub state: DocumentState,
    pub steps: Vec<ChecklistStep>,
    /// The page also renders the document, so each step links to its
    /// paragraph.
    pub linked: bool,
}

/// The `notation_list` section.
#[component]
pub fn NotationListSection(rows: Vec<NotationListRow>) -> Element {
    rsx! {
        section {
            class: "matter-section",
            "data-section": "notation_list",
            aria_labelledby: "section-notation_list-title",
            Panel { id: "section-notation_list".to_string(), title: "Documents".to_string(),
                if rows.is_empty() {
                    div { class: "matter-section__empty",
                        Empty { title: "No document has been started on this matter yet.".to_string() }
                    }
                } else {
                    ul { class: "matter-notations",
                        for row in rows.iter() {
                            li { class: "matter-notations__item",
                                a { class: "matter-notations__title", href: "{row.href}", "{row.title}" }
                                " "
                                Badge { tone: BadgeTone::Next, "{row.phrase}" }
                                p { class: "matter-notations__next", "{row.next}" }
                                if !row.state.is_empty() {
                                    p { class: "matter-notations__state",
                                        "Workflow state: "
                                        code { "{row.state}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The `notation_checklist` section.
#[component]
pub fn NotationChecklistSection(view: ChecklistView) -> Element {
    let open = view.steps.iter().filter(|step| !step.checked).count();
    let summary = match (open, view.steps.len()) {
        (0, _) => "Every step is done.".to_string(),
        (1, 1) => "The one step is open.".to_string(),
        (1, total) => format!("1 of {total} steps is open."),
        (n, total) => format!("{n} of {total} steps are open."),
    };
    rsx! {
        section {
            class: "matter-section",
            "data-section": "notation_checklist",
            aria_labelledby: "section-notation_checklist-title",
            Panel { id: "section-notation_checklist".to_string(), title: "Checklist".to_string(),
                match view.state {
                    DocumentState::Empty => rsx! {
                        div { class: "matter-section__empty",
                            Empty { title: "No document is ready to check yet.".to_string() }
                        }
                    },
                    DocumentState::Unverified => rsx! {
                        div { class: "matter-section__error", role: "alert",
                            Callout { tone: CalloutTone::Danger, title: "This checklist could not be built".to_string(),
                                p { "The document it comes from could not be verified. The error has been logged." }
                            }
                        }
                    },
                    DocumentState::Notation | DocumentState::DraftPreview => rsx! {
                        if view.steps.is_empty() {
                            p { class: "matter-section__empty", "Nothing is left open in this document." }
                        } else {
                            p { class: "matter-checklist__summary", "{summary}" }
                            ol { class: "matter-checklist",
                                for step in view.steps.iter() {
                                    li {
                                        class: if step.checked { "matter-checklist__step matter-checklist__step--done" } else { "matter-checklist__step" },
                                        id: "{step.id}",
                                        Badge {
                                            tone: if step.checked { BadgeTone::Ready } else { BadgeTone::Review },
                                            if step.checked { "Done" } else { "Open" }
                                        }
                                        " "
                                        if view.linked {
                                            a { href: "#{step.source_id}", "{step.title}" }
                                        } else {
                                            strong { "{step.title}" }
                                        }
                                        if let Some(detail) = step.detail.as_ref() {
                                            p { class: "matter-checklist__detail", "{detail}" }
                                        }
                                    }
                                }
                            }
                        }
                    },
                }
            }
        }
    }
}

/// What the Notation sections are built from, already scoped to the lens.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NotationsData {
    pub document: NotationDocumentView,
    pub list: Vec<NotationListRow>,
    pub checklist: ChecklistView,
    /// Ordered trial-preparation cards from the lawyer's verified Notation.
    pub stages: Vec<super::SectionRow>,
    /// Ordered argument exchanges, refused when the body or block is invalid.
    pub exchanges: super::ArgumentExchangeView,
    /// On the client lens, the one step that unblocks the matter.
    pub decisions: Vec<super::SectionRow>,
}

/// The template code a page's `draft:` names: the file stem of
/// `drafts/<code>.md`.
#[must_use]
pub fn draft_code(draft: &str) -> Option<String> {
    std::path::Path::new(draft.trim())
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .map(str::to_string)
}

/// Why a body could not be shown.
#[cfg(feature = "server")]
#[derive(Debug, Clone)]
enum BodyFailure {
    /// Nothing to show: the Notation names a template that no longer reads
    /// back.
    Missing,
    /// The body failed to read or to verify; fail closed.
    Unverified,
}

/// The view for a body read, in `state` when it succeeds.
#[cfg(feature = "server")]
fn document_view(body: &Result<String, BodyFailure>, state: DocumentState) -> NotationDocumentView {
    match body {
        Ok(body) => NotationDocumentView::showing(
            state,
            crate::notation_outline::notation_document(&views::harvard_outline::parse(body)),
        ),
        Err(BodyFailure::Missing) => NotationDocumentView::default(),
        Err(BodyFailure::Unverified) => NotationDocumentView::unverified(),
    }
}

/// The checklist for a body read. An unverified body yields no steps.
#[cfg(feature = "server")]
fn checklist_view(
    body: &Result<String, BodyFailure>,
    state: DocumentState,
    linked: bool,
) -> ChecklistView {
    match body {
        Ok(body) => ChecklistView {
            state,
            steps: super::checklist::derive(&super::checklist::blocks(
                &views::harvard_outline::parse(body),
            )),
            linked,
        },
        Err(BodyFailure::Missing) => ChecklistView::default(),
        Err(BodyFailure::Unverified) => ChecklistView {
            state: DocumentState::Unverified,
            ..ChecklistView::default()
        },
    }
}

/// One Notation's row: its Draft's title and its client phrase. The raw
/// state shows on the lawyer lens only.
#[cfg(feature = "server")]
fn list_row(
    notation: &store::notations::Notation,
    title: &str,
    code: &str,
    lens: super::PageLens,
) -> NotationListRow {
    let phrase = workflows::client_phrase_for(&notation.state);
    NotationListRow {
        title: title.to_string(),
        phrase: phrase.where_this_is.to_string(),
        next: phrase.whats_next.to_string(),
        state: if lens == super::PageLens::Lawyer {
            notation.state.clone()
        } else {
            String::new()
        },
        href: crate::notation_outline::notation_outline_href(code, &notation.id.to_string()),
    }
}

/// The one step that unblocks the matter on the client lens: the open
/// Notation whose next move is the client's, waiting longest. Nothing when
/// no Notation waits on the client.
#[cfg(feature = "server")]
fn next_client_step(
    notations: &[store::notations::Notation],
    title_of: impl Fn(&store::notations::Notation) -> String,
) -> Vec<super::SectionRow> {
    notations
        .iter()
        .filter(|n| n.state != "END")
        .filter(|n| workflows::client_phrase_for(&n.state).actor == workflows::ClientActor::You)
        .min_by_key(|n| n.updated_at)
        .map(|n| {
            let phrase = workflows::client_phrase_for(&n.state);
            super::SectionRow {
                label: phrase.whats_next.to_string(),
                detail: title_of(n),
                status: phrase.where_this_is.to_string(),
            }
        })
        .into_iter()
        .collect()
}

/// Where a page's Notation sections read from.
#[cfg(feature = "server")]
pub struct NotationsSource<'a> {
    pub project_id: uuid::Uuid,
    pub code: &'a str,
    /// The page's `draft:` path.
    pub draft: Option<&'a str>,
    pub lens: super::PageLens,
    /// The page also declares `notation_document` on this lens.
    pub document_on_page: bool,
}

/// Load the Notation sections' data for `source.lens`. A clerk never
/// receives the legal work, so the clerk lens loads nothing.
///
/// # Errors
///
/// A store read failed. A body that fails its digest is not an error here:
/// it renders the fail-closed state.
#[cfg(feature = "server")]
pub async fn load_notations(
    surreal: &store::surreal::SurrealDb,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
    source: &NotationsSource<'_>,
) -> Result<NotationsData, String> {
    let lens = source.lens;
    if lens == super::PageLens::Clerk {
        return Ok(NotationsData::default());
    }
    let notations = store::notations::list_by_project(surreal, source.project_id)
        .await
        .map_err(|e| e.to_string())?;
    let template_ids: Vec<uuid::Uuid> = notations.iter().map(|n| n.template_id).collect();
    let templates = store::templates::find_by_ids(surreal, &template_ids)
        .await
        .map_err(|e| e.to_string())?;
    let template_of =
        |n: &store::notations::Notation| templates.iter().find(|t| t.id == n.template_id);
    let title_of = |n: &store::notations::Notation| {
        template_of(n).map_or_else(|| "Document".to_string(), |t| t.title.clone())
    };
    let code = source.draft.and_then(draft_code);
    // Newest first, so the first match is the page's activated Notation.
    let activated = notations.iter().find(|notation| {
        let template = template_of(notation);
        template.is_some_and(|template| {
            notation_kind_visible_to_lens(template.kind.as_deref(), lens)
                && code.as_ref().is_none_or(|code| &template.code == code)
        })
    });

    let read = if let Some(notation) = activated {
        let body = match store::notations::pinned_body(surreal, storage, notation).await {
            Ok(body) => Ok(body),
            Err(store::notations::PinnedBodyError::MissingTemplate { .. }) => {
                Err(BodyFailure::Missing)
            }
            Err(e) => {
                tracing::error!(error = %e, notation_id = %notation.id, "notation body refused");
                Err(BodyFailure::Unverified)
            }
        };
        Some((body, DocumentState::Notation))
    } else if let (super::PageLens::Lawyer, Some(code)) = (lens, code.as_deref()) {
        match store::templates::resolve_exact(surreal, Some(source.project_id), code)
            .await
            .map_err(|e| e.to_string())?
        {
            Some(template) => {
                let body = store::templates::body(surreal, storage, &template)
                    .await
                    .map_err(|e| {
                        tracing::error!(error = %e, template_id = %template.id, "draft preview body refused");
                        BodyFailure::Unverified
                    });
                Some((body, DocumentState::DraftPreview))
            }
            None => None,
        }
    } else {
        None
    };
    let stage_kind = activated
        .and_then(template_of)
        .and_then(|template| template.kind.as_deref());
    let stages = read.as_ref().map_or_else(Vec::new, |(body, _)| {
        trial_prep_stages(stage_kind, lens, body)
    });
    let exchanges = read
        .as_ref()
        .map_or_else(super::ArgumentExchangeView::default, |(body, _)| {
            trial_prep_exchanges(stage_kind, lens, body)
        });
    let (document, checklist) = read.map_or_else(
        || (NotationDocumentView::default(), ChecklistView::default()),
        |(body, state)| {
            (
                document_view(&body, state),
                checklist_view(&body, state, source.document_on_page),
            )
        },
    );
    Ok(NotationsData {
        document,
        checklist,
        stages,
        exchanges,
        list: notations
            .iter()
            .filter(|notation| {
                template_of(notation).is_some_and(|template| {
                    notation_kind_visible_to_lens(template.kind.as_deref(), lens)
                })
            })
            .map(|n| list_row(n, &title_of(n), source.code, lens))
            .collect(),
        decisions: if lens == super::PageLens::Client {
            next_client_step(&notations, title_of)
        } else {
            Vec::new()
        },
    })
}

/// Trial-preparation Notations are internal work product, so they are not
/// selectable or listed on the client lens even when a page asks for a
/// Notation document or list. Other Notation kinds retain their existing
/// lens behavior.
#[cfg(feature = "server")]
fn notation_kind_visible_to_lens(kind: Option<&str>, lens: super::PageLens) -> bool {
    lens == super::PageLens::Lawyer
        || kind
            .and_then(rules::kind::Kind::parse)
            .is_none_or(rules::kind::Kind::is_client_visible)
}

/// Read stage cards only from a lawyer-visible `TrialPrep` Notation whose
/// body already passed `pinned_body`'s digest check.
#[cfg(feature = "server")]
fn trial_prep_stages(
    kind: Option<&str>,
    lens: super::PageLens,
    body: &Result<String, BodyFailure>,
) -> Vec<super::SectionRow> {
    if lens != super::PageLens::Lawyer || kind != Some(rules::kind::Kind::TrialPrep.as_str()) {
        return Vec::new();
    }
    let Ok(body) = body else {
        return Vec::new();
    };
    match parse_trial_prep_cards(body) {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "trial-preparation Notation has an invalid card table");
            Vec::new()
        }
    }
}

/// Parse the one ordered card table authored in a `trial_prep` body. A
/// matching header is the only point where body text becomes stage data;
/// all other prose is ignored. Once found, every contiguous row must be
/// valid or the whole card list fails closed.
#[cfg(feature = "server")]
fn parse_trial_prep_cards(body: &str) -> Result<Vec<super::SectionRow>, &'static str> {
    const HEADER: [&str; 3] = ["question", "asked by", "difficulty"];
    let lines: Vec<&str> = body.lines().collect();
    let mut header_index = None;
    let mut fence = None;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            let marker = &trimmed[..3];
            if fence == Some(marker) {
                fence = None;
            } else if fence.is_none() {
                fence = Some(marker);
            }
            continue;
        }
        if fence.is_some() {
            continue;
        }
        let cells = split_markdown_table_row(line);
        if cells.as_deref().is_some_and(|cells| {
            cells.len() == HEADER.len()
                && cells
                    .iter()
                    .zip(HEADER)
                    .all(|(cell, expected)| cell.eq_ignore_ascii_case(expected))
        }) && header_index.replace(index).is_some()
        {
            return Err("more than one stage-card header");
        }
    }
    let Some(header_index) = header_index else {
        return Ok(Vec::new());
    };
    let Some(separator) = lines
        .get(header_index + 1)
        .and_then(|line| split_markdown_table_row(line))
    else {
        return Err("stage-card header has no Markdown separator row");
    };
    if separator.len() != HEADER.len() || !separator.iter().all(|cell| valid_table_separator(cell))
    {
        return Err("stage-card header has an invalid Markdown separator row");
    }

    let mut rows = Vec::new();
    for line in lines.iter().skip(header_index + 2) {
        if line.trim().is_empty() {
            break;
        }
        let Some(cells) = split_markdown_table_row(line) else {
            return Err("stage-card table contains a non-table row");
        };
        if cells.len() != HEADER.len() || cells.iter().any(String::is_empty) {
            return Err("stage-card row must have three non-empty cells");
        }
        rows.push(super::SectionRow {
            label: cells[0].clone(),
            detail: cells[1].clone(),
            status: cells[2].clone(),
        });
    }
    Ok(rows)
}

/// Split one bounded pipe-table row. An escaped pipe is a literal `|`
/// inside its cell rather than a column boundary; leading and trailing
/// structural pipes are optional.
#[cfg(feature = "server")]
fn split_markdown_table_row(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if !trimmed.contains('|') {
        return None;
    }
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut chars = trimmed.chars().peekable();
    let mut first = true;
    let mut last_was_delimiter = false;
    while let Some(ch) = chars.next() {
        match ch {
            '\\' if chars.peek() == Some(&'|') => {
                chars.next();
                cell.push('|');
                first = false;
                last_was_delimiter = false;
            }
            '|' if first && cell.is_empty() => {
                first = false;
                last_was_delimiter = true;
            }
            '|' => {
                cells.push(cell.trim().to_string());
                cell.clear();
                first = false;
                last_was_delimiter = true;
            }
            other => {
                cell.push(other);
                first = false;
                last_was_delimiter = false;
            }
        }
    }
    if last_was_delimiter {
        if !cell.trim().is_empty() {
            return None;
        }
    } else {
        cells.push(cell.trim().to_string());
    }
    Some(cells)
}

#[cfg(feature = "server")]
fn valid_table_separator(cell: &str) -> bool {
    let cell = cell.trim();
    let without_leading = cell.strip_prefix(':').unwrap_or(cell);
    let dashes = without_leading.strip_suffix(':').unwrap_or(without_leading);
    dashes.len() >= 3 && dashes.chars().all(|ch| ch == '-')
}

/// Internal exchanges only enter the view from a verified `TrialPrep` body.
#[cfg(feature = "server")]
fn trial_prep_exchanges(
    kind: Option<&str>,
    lens: super::PageLens,
    body: &Result<String, BodyFailure>,
) -> super::ArgumentExchangeView {
    if lens != super::PageLens::Lawyer || kind != Some(rules::kind::Kind::TrialPrep.as_str()) {
        return super::ArgumentExchangeView::default();
    }
    let exchanges = body
        .as_ref()
        .ok()
        .and_then(|body| parse_argument_exchanges(body).ok());
    super::ArgumentExchangeView {
        unavailable: exchanges.is_none(),
        exchanges: exchanges.unwrap_or_default(),
    }
}

/// Exactly one explicit block supplies exchanges; examples in other fences
/// are ignored. Invalid fields refuse the entire list rather than a partial argument.
#[cfg(feature = "server")]
fn parse_argument_exchanges(body: &str) -> Result<Vec<super::ArgumentExchange>, &'static str> {
    let mut fence: Option<(char, usize)> = None;
    let mut collecting = false;
    let mut found = false;
    let mut yaml = String::new();
    for line in body.lines() {
        let trimmed = line.trim();
        let marker = trimmed.chars().next().filter(|c| *c == '`' || *c == '~');
        let count = marker.map_or(0, |marker| {
            trimmed.chars().take_while(|c| *c == marker).count()
        });
        if let Some((open, length)) = fence {
            if marker == Some(open) && count >= length && trimmed[count..].trim().is_empty() {
                fence = None;
                collecting = false;
            } else if collecting {
                yaml.push_str(line);
                yaml.push('\n');
            }
        } else if count >= 3 {
            collecting = trimmed[count..].trim() == "argument_exchange";
            if collecting && found {
                return Err("more than one argument-exchange block");
            }
            found |= collecting;
            fence = marker.map(|marker| (marker, count));
        }
    }
    if collecting {
        return Err("argument-exchange block is not closed");
    }
    if !found {
        return Ok(Vec::new());
    }
    let exchanges: Vec<super::ArgumentExchange> =
        serde_yaml::from_str(&yaml).map_err(|_| "invalid argument-exchange YAML or fields")?;
    if exchanges.iter().any(|exchange| {
        exchange.title.trim().is_empty()
            || [&exchange.point, &exchange.response, &exchange.reply]
                .iter()
                .any(|turn| turn.text.trim().is_empty() || turn.authority.trim().is_empty())
    }) {
        return Err("argument-exchange fields must not be blank");
    }
    Ok(exchanges)
}

#[cfg(test)]
mod tests {
    use super::{draft_code, DocumentState, NotationDocumentView};
    use crate::sections::tests::render;
    use crate::sections::SectionView;

    #[test]
    fn a_draft_path_names_its_template_code() {
        assert_eq!(
            draft_code("drafts/onboarding__letter.md").as_deref(),
            Some("onboarding__letter")
        );
        assert_eq!(draft_code("letter.md").as_deref(), Some("letter"));
        assert_eq!(draft_code(""), None);
    }

    #[test]
    fn an_unverified_body_renders_a_visible_error_and_no_text() {
        let html = render(SectionView::NotationDocument(
            NotationDocumentView::unverified(),
        ));
        assert!(html.contains("data-state=\"unverified\""), "{html}");
        assert!(html.contains("role=\"alert\""), "{html}");
        assert!(html.contains("could not be verified"), "{html}");
        assert!(!html.contains("notation-document__body"), "{html}");
    }

    #[cfg(feature = "server")]
    const SYNTHETIC: &str = "---\ntitle: Sample Engagement Letter\n---\n\n\
        Dear {{person__client.name}},\n\n## I. Scope\n\nThe firm will draft one sample \
        agreement.\n\n[Confirm the flat fee before sending.]\n\n## II. Fees\n\nThe fee is \
        `{{custom_money__fee}}`.\n";

    #[cfg(feature = "server")]
    #[test]
    fn a_verified_body_renders_its_outline_with_path_anchors() {
        let view = super::document_view(&Ok(SYNTHETIC.to_string()), DocumentState::Notation);
        assert_eq!(view.state, DocumentState::Notation);
        let html = render(SectionView::NotationDocument(view));
        assert!(html.contains("data-state=\"notation\""), "{html}");
        assert!(html.contains("id=\"notation-document\""), "{html}");
        assert!(html.contains("Sample Engagement Letter"), "{html}");
        assert!(html.contains("id=\"section-i\""), "{html}");
        assert!(html.contains("href=\"#section-ii\""), "{html}");
        assert!(html.contains("notation-block--hold"), "{html}");
        assert!(!html.contains("matter-section__empty"), "{html}");
    }

    #[cfg(feature = "server")]
    #[test]
    fn a_refused_body_fails_closed_and_a_missing_template_is_empty() {
        let refused = super::document_view(
            &Err(super::BodyFailure::Unverified),
            DocumentState::Notation,
        );
        assert_eq!(refused, NotationDocumentView::unverified());
        let missing =
            super::document_view(&Err(super::BodyFailure::Missing), DocumentState::Notation);
        assert_eq!(missing.state, DocumentState::Empty);
    }

    #[cfg(feature = "server")]
    #[test]
    fn a_draft_preview_says_so_and_still_renders_the_outline() {
        let view = super::document_view(&Ok(SYNTHETIC.to_string()), DocumentState::DraftPreview);
        let html = render(SectionView::NotationDocument(view));
        assert!(html.contains("Draft preview"), "{html}");
        assert!(
            html.contains("A client does not see this preview"),
            "{html}"
        );
        assert!(html.contains("notation-document__body"), "{html}");
    }

    fn row(title: &str, phrase: &str, state: &str) -> super::NotationListRow {
        super::NotationListRow {
            title: title.into(),
            phrase: phrase.into(),
            next: "Read them, then approve or tell the firm what to change.".into(),
            state: state.into(),
            href: "/app/projects/sample-transactional/0190-sample/outline".into(),
        }
    }

    #[test]
    fn the_list_shows_each_notation_with_its_phrase_and_the_state_only_when_given() {
        let html = render(SectionView::NotationList(vec![
            row(
                "Sample Engagement Letter",
                "Your turn to review the drafts",
                "",
            ),
            row(
                "Sample Supply Agreement",
                "The firm is reviewing",
                "lawyer_review",
            ),
        ]));
        assert!(html.contains("data-section=\"notation_list\""), "{html}");
        assert!(html.contains("Sample Engagement Letter"), "{html}");
        assert!(html.contains("Your turn to review the drafts"), "{html}");
        assert!(
            html.contains("href=\"/app/projects/sample-transactional/0190-sample/outline\""),
            "{html}"
        );
        assert_eq!(html.matches("Workflow state").count(), 1, "{html}");
        assert!(html.contains("<code>lawyer_review</code>"), "{html}");
        assert!(!html.contains("matter-section__empty"), "{html}");
    }

    fn step(id: &str, title: &str, checked: bool) -> super::ChecklistStep {
        super::ChecklistStep {
            id: format!("{id}-check-1"),
            source_id: id.into(),
            title: title.into(),
            detail: None,
            checked,
        }
    }

    #[test]
    fn the_checklist_counts_open_steps_and_links_them_when_the_document_is_on_the_page() {
        let view = super::ChecklistView {
            state: DocumentState::Notation,
            steps: vec![
                step("section-i-p2", "Confirm the synthetic fee", false),
                step("section-ii-p1", "Resolved.", true),
            ],
            linked: true,
        };
        let html = render(SectionView::NotationChecklist(view.clone()));
        assert!(
            html.contains("data-section=\"notation_checklist\""),
            "{html}"
        );
        assert!(html.contains("1 of 2 steps is open."), "{html}");
        assert!(html.contains("href=\"#section-i-p2\""), "{html}");
        assert!(html.contains("matter-checklist__step--done"), "{html}");
        assert!(html.contains(">Done<") && html.contains(">Open<"), "{html}");

        let unlinked = render(SectionView::NotationChecklist(super::ChecklistView {
            linked: false,
            ..view
        }));
        assert!(!unlinked.contains("href=\"#section-i-p2\""), "{unlinked}");
    }

    #[test]
    fn a_checklist_with_nothing_open_or_an_unverified_body_says_so() {
        let none = render(SectionView::NotationChecklist(super::ChecklistView {
            state: DocumentState::Notation,
            ..super::ChecklistView::default()
        }));
        assert!(none.contains("Nothing is left open"), "{none}");
        let refused = render(SectionView::NotationChecklist(super::ChecklistView {
            state: DocumentState::Unverified,
            ..super::ChecklistView::default()
        }));
        assert!(refused.contains("role=\"alert\""), "{refused}");
        assert!(!refused.contains("matter-checklist__step"), "{refused}");
    }

    #[cfg(feature = "server")]
    #[test]
    fn trial_prep_table_preserves_order_and_unescapes_literal_pipes() {
        let rows = super::parse_trial_prep_cards(
            r"Introductory prose is ignored.

| Question | Asked by | Difficulty |
| --- | :---: | ---: |
| What\|where? | Plaintiff | High |
| Who? | Defense | Medium |

Later prose is outside the table.",
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label, "What|where?");
        assert_eq!(rows[0].detail, "Plaintiff");
        assert_eq!(rows[0].status, "High");
        assert_eq!(rows[1].label, "Who?");
        let trailing_escaped_pipe = super::parse_trial_prep_cards(
            "| Question | Asked by | Difficulty |\n| --- | --- | --- |\n| Final pipe? | Party | High\\|",
        )
        .expect("escaped pipe at end of an unterminated row remains cell text");
        assert_eq!(trailing_escaped_pipe[0].status, "High|");
        let html = render(SectionView::StageList(rows));
        assert!(html.contains("What|where?"), "{html}");
        assert!(html.contains("Plaintiff"), "{html}");
        assert!(html.contains("High"), "{html}");
        assert!(html.find("What|where?").unwrap() < html.find("Who?").unwrap());
    }

    #[cfg(feature = "server")]
    #[test]
    fn trial_prep_table_rejects_bad_header_separator_or_any_bad_row() {
        for body in [
            "| Question | Asked by | Difficulty |\n| --- | --- | --- |\n| incomplete | row |",
            "| Question | Asked by | Difficulty |\n| -- | --- | --- |\n| Q | P | E |",
            "| Question | Asked by | Difficulty |\n| --:-- | --- | --- |\n| Q | P | E |",
            "| Question | Asked by | Difficulty |\n| --- | --- | --- |\n| Q | P | E |\nnot a table row",
            "| Question | Asked by | Difficulty |\n| --- | --- | --- |\n| Q | P | E |\n| | P | E |",
        ] {
            assert!(super::parse_trial_prep_cards(body).is_err(), "{body}");
        }
        assert!(
            super::parse_trial_prep_cards("ordinary prose, no card table")
                .unwrap()
                .is_empty()
        );
        assert!(super::parse_trial_prep_cards(
            "```markdown\n| Question | Asked by | Difficulty |\n| --- | --- | --- |\n| Hidden? | Person | High |\n```"
        )
        .unwrap()
        .is_empty());
    }

    #[cfg(feature = "server")]
    #[test]
    fn trial_prep_cards_require_lawyer_lens_kind_and_verified_body() {
        let body = Ok(
            "| Question | Asked by | Difficulty |\n| --- | --- | --- |\n| Q? | Party | High |"
                .to_string(),
        );
        assert_eq!(
            super::trial_prep_stages(Some("trial_prep"), crate::sections::PageLens::Lawyer, &body)
                .len(),
            1
        );
        assert!(super::trial_prep_stages(
            Some("trial_prep"),
            crate::sections::PageLens::Client,
            &body
        )
        .is_empty());
        assert!(super::trial_prep_stages(
            Some("trial_prep"),
            crate::sections::PageLens::Clerk,
            &body
        )
        .is_empty());
        assert!(super::trial_prep_stages(
            Some("agreement"),
            crate::sections::PageLens::Lawyer,
            &body
        )
        .is_empty());
        let unverified = Err(super::BodyFailure::Unverified);
        assert!(super::trial_prep_stages(
            Some("trial_prep"),
            crate::sections::PageLens::Lawyer,
            &unverified
        )
        .is_empty());
        assert!(super::notation_kind_visible_to_lens(
            Some("trial_prep"),
            crate::sections::PageLens::Lawyer
        ));
        assert!(!super::notation_kind_visible_to_lens(
            Some("trial_prep"),
            crate::sections::PageLens::Client
        ));
        assert!(!super::notation_kind_visible_to_lens(
            Some("trial_prep"),
            crate::sections::PageLens::Clerk
        ));
        assert!(super::notation_kind_visible_to_lens(
            Some("agreement"),
            crate::sections::PageLens::Client
        ));
    }

    #[cfg(feature = "server")]
    fn notation(state: &str, minutes_ago: i64) -> store::notations::Notation {
        let at = chrono::Utc::now() - chrono::Duration::minutes(minutes_ago);
        store::notations::Notation {
            id: uuid::Uuid::now_v7(),
            template_id: uuid::Uuid::now_v7(),
            package_id: None,
            person_id: uuid::Uuid::now_v7(),
            entity_id: None,
            project_id: uuid::Uuid::now_v7(),
            state: state.into(),
            delivery: store::notations::DELIVERY_EMBEDDED.into(),
            questionnaire_snapshot: None,
            git_commit_sha: None,
            template_source_commit_sha: None,
            template_body_sha256: None,
            inserted_at: at,
            updated_at: at,
        }
    }

    #[cfg(feature = "server")]
    #[test]
    fn the_decision_queue_names_the_clients_longest_waiting_step_only() {
        let notations = vec![
            notation("lawyer_review", 1),
            notation("client_review", 5),
            notation("sent_for_signature__pending", 60),
            notation("END", 600),
        ];
        let rows = super::next_client_step(&notations, |n| format!("Sample {}", n.state));
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].detail, "Sample sent_for_signature__pending");
        assert_eq!(rows[0].status, "Awaiting your signature");
        assert_eq!(rows[0].label, "Review and sign when you're ready.");

        let firm_only = vec![notation("lawyer_review", 1), notation("END", 2)];
        assert!(super::next_client_step(&firm_only, |_| String::new()).is_empty());
    }

    #[cfg(feature = "server")]
    #[test]
    fn a_verified_body_yields_its_checklist_and_a_refused_one_none() {
        let view = super::checklist_view(&Ok(SYNTHETIC.to_string()), DocumentState::Notation, true);
        assert_eq!(view.steps.len(), 1, "{view:?}");
        assert_eq!(view.steps[0].title, "Confirm the flat fee before sending.");
        let refused = super::checklist_view(
            &Err(super::BodyFailure::Unverified),
            DocumentState::Notation,
            true,
        );
        assert_eq!(refused.state, DocumentState::Unverified);
        assert!(refused.steps.is_empty());
    }

    #[test]
    fn the_client_decision_renders_at_phone_width_as_one_plain_row() {
        // The decision queue is a single-column list with no fixed widths, so
        // it reflows at phone width; the stylesheet block that styles it sets
        // none either.
        let html = render(SectionView::DecisionQueue(vec![
            crate::sections::SectionRow {
                label: "Review and sign when you're ready.".into(),
                detail: "Sample Engagement Letter".into(),
                status: "Awaiting your signature".into(),
            },
        ]));
        assert!(html.contains("data-section=\"decision_queue\""), "{html}");
        assert!(html.contains("Review and sign when you"), "{html}");
        assert!(!html.contains("style="), "{html}");
        let css = include_str!("../../../server/public/css/theme.css");
        let block = &css[css.find("Notation sections").expect("notation css block")..];
        assert!(!block.contains("min-width"), "no fixed minimum width");
    }
}

#[cfg(all(test, feature = "server"))]
mod argument_exchange_tests {
    #[test]
    fn argument_exchange_preserves_multiline_turns_and_order() {
        let body = "```argument_exchange\n- title: First\n  point: {text: 'Point <script>', authority: 'Rule A'}\n  response: {text: Response, authority: 'Rule B'}\n  reply:\n    text: |\n      Reply one\n      Reply two\n    authority: 'Rule C'\n- title: Second\n  point: {text: P, authority: A}\n  response: {text: R, authority: B}\n  reply: {text: R, authority: C}\n```";
        let rows = super::parse_argument_exchanges(body).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].title, "First");
        assert_eq!(rows[1].title, "Second");
        assert_eq!(rows[0].reply.text, "Reply one\nReply two\n");
        let html = crate::sections::tests::render(crate::sections::SectionView::ArgumentExchange(
            crate::sections::ArgumentExchangeView {
                exchanges: rows,
                unavailable: false,
            },
        ));
        assert!(html.contains("Point &#60;script&#62;"), "{html}");
        assert!(!html.contains("<script>"), "{html}");
        assert!(html.contains("Rule A"), "{html}");
    }
    #[tokio::test]
    async fn argument_exchange_loader_uses_activated_pinned_body_and_hides_other_lenses() {
        use crate::sections::PageLens;
        let db = store::surreal::test_support::mem().await;
        let project_id = store::test_support::seed_project_surreal(&db, "exchange-synthetic").await;
        let person = store::persons::create(
            &db,
            &store::persons::NewPerson::new("Synthetic Lawyer", "exchange@example.com"),
        )
        .await
        .unwrap();
        let path =
            std::env::temp_dir().join(format!("navigator-exchanges-{}", uuid::Uuid::now_v7()));
        let storage: std::sync::Arc<dyn cloud::StorageService> =
            std::sync::Arc::new(cloud::FsStorage::new(path.clone()).await.unwrap());
        let body = "```argument_exchange\n- title: Verified exchange\n  point: {text: Point, authority: A}\n  response: {text: Response, authority: B}\n  reply: {text: Reply, authority: C}\n```";
        let asset_id =
            store::assets::ingest_content(&db, &storage, body.as_bytes(), "text/markdown")
                .await
                .unwrap();
        let template = store::templates::save_version(
            &db,
            Some(project_id),
            "exchange",
            store::templates::Version {
                title: "Synthetic preparation".into(),
                respondent_type: "person".into(),
                asset_id: Some(asset_id),
                form_code: None,
                kind: Some("trial_prep".into()),
                source_commit_sha: Some(store::test_support::SYNTHETIC_COMMIT_SHA.into()),
            },
        )
        .await
        .unwrap()
        .into_model();
        let mut source = super::NotationsSource {
            project_id,
            code: "exchange-synthetic",
            draft: Some("drafts/exchange.md"),
            lens: PageLens::Lawyer,
            document_on_page: false,
        };
        let preview = super::load_notations(&db, &storage, &source).await.unwrap();
        assert!(
            preview.exchanges.exchanges.is_empty(),
            "preview is not an activated Notation"
        );
        store::notations::create(
            &db,
            &store::notations::NewNotation::new(template.id, person.id, project_id, "BEGIN"),
        )
        .await
        .unwrap();
        let loaded = super::load_notations(&db, &storage, &source).await.unwrap();
        assert_eq!(loaded.exchanges.exchanges[0].title, "Verified exchange");
        for lens in [PageLens::Client, PageLens::Clerk] {
            source.lens = lens;
            let loaded = super::load_notations(&db, &storage, &source).await.unwrap();
            assert!(loaded.exchanges.exchanges.is_empty());
            assert!(loaded.list.is_empty());
        }
        source.lens = PageLens::Lawyer;
        let key = format!("blobs/{}", store::assets::sha256_hex(body.as_bytes()));
        storage
            .put(&key, b"tampered", "text/markdown")
            .await
            .unwrap();
        let refused = super::load_notations(&db, &storage, &source).await.unwrap();
        assert!(refused.exchanges.unavailable);
        assert!(refused.exchanges.exchanges.is_empty());
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn argument_exchange_requires_verified_trial_prep_and_lawyer_lens() {
        use crate::sections::{ArgumentExchangeView, PageLens, SectionView};
        let invalid = Ok("```argument_exchange\ninvalid\n```".to_string());
        let refused = super::trial_prep_exchanges(Some("trial_prep"), PageLens::Lawyer, &invalid);
        assert!(refused.unavailable);
        let html = crate::sections::tests::render(SectionView::ArgumentExchange(refused));
        assert!(html.contains("role=\"alert\""), "{html}");
        for lens in [PageLens::Client, PageLens::Clerk] {
            assert_eq!(
                super::trial_prep_exchanges(Some("trial_prep"), lens, &invalid),
                ArgumentExchangeView::default()
            );
        }
        for kind in [None, Some("memo")] {
            assert_eq!(
                super::trial_prep_exchanges(kind, PageLens::Lawyer, &invalid),
                ArgumentExchangeView::default()
            );
        }
        assert!(
            super::trial_prep_exchanges(
                Some("trial_prep"),
                PageLens::Lawyer,
                &Err(super::BodyFailure::Unverified)
            )
            .unavailable
        );
        assert!(
            !super::trial_prep_exchanges(
                Some("trial_prep"),
                PageLens::Lawyer,
                &Ok("ordinary prose".into())
            )
            .unavailable
        );
    }

    #[test]
    fn argument_exchange_rejects_malformed_or_ambiguous_data() {
        for body in [
            "```argument_exchange\nnot yaml: [\n```",
            "```argument_exchange\n- title: Missing turns\n```",
            "```argument_exchange\n[]",
            "```argument_exchange\n[]\n```\n```argument_exchange\n[]\n```",
            "```argument_exchange\n- title: Blank\n  point: {text: ' ', authority: A}\n  response: {text: R, authority: B}\n  reply: {text: R, authority: C}\n```",
            "```argument_exchange\n- title: Extra\n  unexpected: field\n  point: {text: P, authority: A}\n  response: {text: R, authority: B}\n  reply: {text: R, authority: C}\n```",
        ] {
            assert!(super::parse_argument_exchanges(body).is_err(), "{body}");
        }
        assert!(super::parse_argument_exchanges("ordinary prose")
            .unwrap()
            .is_empty());
        assert!(super::parse_argument_exchanges(
            "````markdown\n```argument_exchange\nnot data\n```\n````"
        )
        .unwrap()
        .is_empty());
    }
}
