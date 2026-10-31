//! The lawyer workbench's Integrations section: the Xero and source-repository
//! links, rendered only when at least one exists — a matter with no
//! integrations prints no heading to be empty under.

use dioxus::prelude::*;

use crate::lawyer_project_detail::LawyerDetailView;

/// The Integrations section: the Xero and source-repository links, rendered
/// only when at least one exists — a matter with no integrations prints no
/// heading to be empty under.
pub(crate) fn integrations_section(view: &LawyerDetailView) -> Element {
    if view.xero_invoice_url.is_none() && view.repository_url.is_none() {
        return rsx! {};
    }
    rsx! {
        section { class: "lawyer-detail__section project-integrations",
            h2 { "Integrations" }
            p { class: "lawyer-detail__integration-links",
                if let Some(url) = view.xero_invoice_url.as_ref() {
                    a {
                        class: "nav-btn nav-btn--secondary",
                        href: "{url}",
                        target: "_blank",
                        rel: "noopener noreferrer",
                        "Xero"
                    }
                }
                if let Some(url) = view.repository_url.as_ref() {
                    a {
                        class: "nav-btn nav-btn--secondary",
                        href: "{url}",
                        target: "_blank",
                        rel: "noopener noreferrer",
                        "Source repository"
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lawyer_project_detail::LawyerDetailView;

    /// The show page's Forgejo parity: a matter whose `repository_url` is
    /// stamped renders the Integrations section with a "Source repository"
    /// link pointing at the Forgejo repo (the blueprint's show-page row).
    #[test]
    fn a_repository_url_renders_a_source_repository_link_with_its_href() {
        let view = LawyerDetailView {
            repository_url: Some("https://forgejo.example.com/firm/sample-litigation".to_string()),
            ..LawyerDetailView::default()
        };
        let html = dioxus_ssr::render_element(integrations_section(&view));
        assert!(html.contains("project-integrations"), "{html}");
        assert!(html.contains("Integrations"), "{html}");
        assert!(html.contains("Source repository"), "{html}");
        assert!(
            html.contains(r#"href="https://forgejo.example.com/firm/sample-litigation""#),
            "{html}"
        );
        // The link opens in a new tab without granting the forge page access
        // back to the workbench.
        assert!(html.contains(r#"target="_blank""#), "{html}");
        assert!(html.contains(r#"rel="noopener noreferrer""#), "{html}");
        assert!(!html.contains(">Xero<"), "{html}");
    }

    /// An un-provisioned matter (no `repository_url`) prints no Integrations
    /// section at all — no empty heading to read past.
    #[test]
    fn a_view_without_a_repository_url_prints_no_integrations_section() {
        let view = LawyerDetailView::default();
        let html = dioxus_ssr::render_element(integrations_section(&view));
        assert!(html.is_empty(), "{html}");
        assert!(!html.contains("Integrations"), "{html}");
        assert!(!html.contains("Source repository"), "{html}");
    }
}
