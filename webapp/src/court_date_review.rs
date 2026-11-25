//! The Project lawyer's proposed court dates and their exact source pages.
use crate::components::citation_highlight::citation_highlight_assets;
use crate::components::{StatusCell, StatusStrip};
use crate::keyboard::{court_date_shortcuts, PageShortcuts};
use dioxus::prelude::*;

#[component]
pub fn CourtDateReview(
    project_id: String,
    project_code: String,
    csrf: String,
    #[props(default)] supervised: bool,
) -> Element {
    rsx! {
        {citation_highlight_assets()}
        document::Stylesheet { href: "/public/css/court-date-review.css" }
        document::Script { src: "/public/js/court-date-review.js", defer: true }
        PageShortcuts { shortcuts: court_date_shortcuts(supervised) }
        court-date-review {
            "data-endpoint": "/app/api/projects/{project_id}/court-date-proposals",
            "data-csrf": "{csrf}",
            "data-project-code": "{project_code}",
            "data-supervised": "{supervised}",
            header { class: "court-review__heading",
                h2 { "Court date review" }
                StatusStrip { cells: vec![StatusCell::new("Pending", "—")] }
            }
            div { "data-queue-views": "true", class: "court-review__actions" }
            div { class: "court-review__grid",
                nav { "aria-label": "Proposed dates", "data-queue": "true" }
                section { "aria-label": "Selected proposal", "data-detail": "true",
                    p { "Loading proposals…" }
                }
            }
            p { "data-message": "true", role: "status", aria_live: "polite" }
            p { class: "court-review__keys nav-muted",
                span { kbd { "J" } " / " kbd { "K" } " Move" }
                span { kbd { "R" } " Record" }
                if !supervised { span { kbd { "A" } " Authority" } }
                span { kbd { "?" } " Shortcuts" }
            }
            dialog { "aria-label": "Court date review", "data-review-dialog": "true" }
            noscript { "Enable JavaScript to review evidence and confirm a court date." }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn court_review_renders_the_project_endpoint_scoped_shortcuts_and_inert_dialog() {
        let mut dom = VirtualDom::new_with_props(
            CourtDateReview,
            CourtDateReviewProps {
                project_id: "synthetic-project".into(),
                project_code: "sample-litigation".into(),
                csrf: "test-token".into(),
                supervised: false,
            },
        );
        dom.rebuild_in_place();
        let html = dioxus_ssr::render(&dom);
        for expected in [
            "/app/api/projects/synthetic-project/court-date-proposals",
            "test-token",
            "Proposed dates",
            "Selected proposal",
            "nav-status-strip",
            "Mod+Enter",
            "data-review-dialog",
        ] {
            assert!(html.contains(expected), "{html}");
        }
    }
}
