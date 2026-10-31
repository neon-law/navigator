//! The workbench's firm-internal notes and logged correspondence.
//!
//! The Overview tab renders the notes as a section; the Correspondence tab
//! renders every logged email and call. Both surfaces read one firm-visible
//! thread (`store::communications::for_project`) split once, server-side,
//! into view rows — no `store` type crosses to the wasm client build.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::lawyer_project_detail::LawyerDetailView;

/// One Communication on the Project show, as plain display fields — never a
/// `store` type, which does not cross to the wasm client build.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct CommunicationRow {
    /// `note`, `email_inbound`, `email_outbound`, or `call` — the wire
    /// shape of `store::communications::channel`.
    pub channel: String,
    /// `inbound`, `outbound`, or `internal`.
    pub direction: String,
    /// The other party as recorded; `None` on a note.
    pub counterparty: Option<String>,
    /// The author's name when a persons row is on file.
    pub author_name: Option<String>,
    pub subject: Option<String>,
    /// The body, truncated to an index entry — the full text stays in the
    /// store.
    pub body: String,
    /// RFC 3339, as the store normalises it.
    pub occurred_at: String,
}

/// Project the stored thread into the notes and correspondence shapes. Pure:
/// the caller resolves author names once, so both surfaces read one store
/// query. Notes are the `note` channel only; everything else — every email
/// and call — is correspondence.
#[cfg(feature = "server")]
pub(crate) fn communication_views(
    rows: &[store::communications::Communication],
    author_names: &std::collections::HashMap<uuid::Uuid, String>,
) -> (Vec<CommunicationRow>, Vec<CommunicationRow>) {
    let mut notes = Vec::new();
    let mut correspondence = Vec::new();
    for row in rows {
        let view = CommunicationRow {
            channel: row.channel.clone(),
            direction: row.direction.clone(),
            counterparty: row.counterparty.clone(),
            author_name: row
                .author_person_id
                .and_then(|author| author_names.get(&author).cloned()),
            subject: row.subject.clone(),
            body: excerpt_body(&row.body),
            occurred_at: row.occurred_at.clone(),
        };
        if row.channel == store::communications::channel::NOTE {
            notes.push(view);
        } else {
            correspondence.push(view);
        }
    }
    (notes, correspondence)
}

/// The tab is an index, not a reader: bodies past this cut end on an
/// ellipsis and the full text stays in the store.
#[cfg(feature = "server")]
fn excerpt_body(body: &str) -> String {
    const EXCERPT_CHARS: usize = 2_000;
    if body.chars().count() <= EXCERPT_CHARS {
        return body.to_string();
    }
    let mut excerpt: String = body.chars().take(EXCERPT_CHARS).collect();
    excerpt.push('…');
    excerpt
}

/// The notes section on the Overview tab: the Project's firm-internal notes,
/// oldest first. A note is a firm-internal observation — it never reaches
/// the client view, so the empty state names the verb that writes one rather
/// than apologising.
pub fn notes_tab(view: &LawyerDetailView) -> Element {
    rsx! {
        section {
            class: "lawyer-detail__section detail-notes",
            h2 { "Notes" }
            if view.notes.is_empty() {
                p {
                    class: "nav-muted",
                    "No notes yet. `navigator project communication note` records one; a note is internal and never reaches the client view."
                }
            } else {
                for note in view.notes.iter() {
                    article { class: "detail-note",
                        header { class: "detail-note__meta",
                            span {
                                class: "detail-note__author",
                                "{note.author_name.clone().unwrap_or_else(|| \"Navigator\".to_string())}"
                            }
                            span { class: "detail-note__when", "{note.occurred_at}" }
                        }
                        if let Some(subject) = note.subject.as_ref() {
                            p { class: "detail-note__subject", "{subject}" }
                        }
                        p { class: "detail-note__body", "{note.body}" }
                    }
                }
            }
        }
    }
}

/// The Correspondence tab's body: every logged email and call on the
/// Project, oldest first — the log emails write with or without a call.
/// Direction prints as a plain word, never an icon or colour alone.
pub fn communications_tab(view: &LawyerDetailView) -> Element {
    rsx! {
        section {
            class: "lawyer-detail__section detail-communications",
            h2 { "Communications" }
            if view.correspondence.is_empty() {
                p {
                    class: "nav-muted",
                    "No communications logged yet. Email logged on this project lands here with or without a call."
                }
            } else {
                for message in view.correspondence.iter() {
                    article { class: "detail-communication",
                        header { class: "detail-communication__meta",
                            span { class: "detail-communication__channel", "{message.channel}" }
                            span { class: "detail-communication__direction", "{message.direction}" }
                            if let Some(counterparty) = message.counterparty.as_ref() {
                                span {
                                    class: "detail-communication__counterparty",
                                    "with {counterparty}"
                                }
                            }
                            span { class: "detail-communication__when", "{message.occurred_at}" }
                        }
                        if let Some(subject) = message.subject.as_ref() {
                            p { class: "detail-communication__subject", "{subject}" }
                        }
                        p { class: "detail-communication__body", "{message.body}" }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{communications_tab, notes_tab, CommunicationRow};
    use crate::lawyer_project_detail::LawyerDetailView;

    fn note_row(subject: &str) -> CommunicationRow {
        CommunicationRow {
            channel: "note".to_string(),
            direction: "internal".to_string(),
            counterparty: None,
            author_name: Some("Aries Attorney".to_string()),
            subject: Some(subject.to_string()),
            body: "The client prefers email; do not call.".to_string(),
            occurred_at: "2026-10-30T12:00:00Z".to_string(),
        }
    }

    #[test]
    fn the_notes_tab_lists_its_notes_with_their_author() {
        let view = LawyerDetailView {
            notes: vec![note_row("Client prefers email")],
            ..LawyerDetailView::default()
        };
        let html = dioxus_ssr::render_element(notes_tab(&view));
        assert!(html.contains("Client prefers email"), "{html}");
        assert!(html.contains("Aries Attorney"), "{html}");
        // A note's body renders, but its row never prints a counterparty
        // direction — it has no other party.
        assert!(!html.contains("with "), "{html}");
    }

    #[test]
    fn the_notes_tab_empty_state_names_the_verb_and_the_exclusion() {
        let view = LawyerDetailView::default();
        let html = dioxus_ssr::render_element(notes_tab(&view));
        assert!(
            html.contains("navigator project communication note"),
            "{html}"
        );
        assert!(html.contains("never reaches the client view"), "{html}");
    }

    #[test]
    fn the_communications_tab_prints_direction_words_and_counterparties() {
        let view = LawyerDetailView {
            correspondence: vec![CommunicationRow {
                channel: "email_inbound".to_string(),
                direction: "inbound".to_string(),
                counterparty: Some("clerk@courts.example".to_string()),
                author_name: None,
                subject: Some("NEF re D.I. 12".to_string()),
                body: "Served today.".to_string(),
                occurred_at: "2026-10-30T09:00:00Z".to_string(),
            }],
            ..LawyerDetailView::default()
        };
        let html = dioxus_ssr::render_element(communications_tab(&view));
        assert!(html.contains("inbound"), "{html}");
        assert!(html.contains("clerk@courts.example"), "{html}");
        assert!(html.contains("NEF re D.I. 12"), "{html}");
    }

    #[test]
    fn the_communications_tab_empty_state_says_emails_need_no_call() {
        let view = LawyerDetailView::default();
        let html = dioxus_ssr::render_element(communications_tab(&view));
        assert!(html.contains("with or without a call"), "{html}");
    }
}
