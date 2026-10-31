//! The admin-only client-testimonial entry card for the lawyer workbench: the
//! attribution boundary (the words belong to the client DRI) and the
//! publish/unpublish controls.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{Field, FormCard, Heading};

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct AdminTestimonialView {
    pub quote: String,
    pub attribution: String,
    pub public_requested: bool,
    /// Approved for the public site: the row has both consent and a publish
    /// time, the same condition the public reads require.
    pub published: bool,
}

/// The admin-only testimonial entry card. Its copy makes the attribution
/// boundary explicit: the words belong to the client DRI, while the admin is
/// recording them for a client who will not sign in to the portal.
pub(crate) fn admin_testimonial_card(
    code: &str,
    csrf: &str,
    testimonial: Option<&AdminTestimonialView>,
) -> Element {
    let testimonial = testimonial.cloned().unwrap_or_default();
    let publication = if testimonial.public_requested {
        "public"
    } else {
        "private"
    };
    let has_testimonial = !testimonial.quote.is_empty();
    let status = if testimonial.published {
        "Published"
    } else if testimonial.public_requested {
        "Awaiting approval"
    } else {
        "Private"
    };
    let publication_action = if testimonial.published {
        Some(("unpublish", "Unpublish"))
    } else if testimonial.public_requested {
        Some(("publish", "Publish"))
    } else {
        None
    };
    rsx! {
        section { class: "lawyer-detail__section project-testimonial",
            h2 { "Client testimonial" }
            if has_testimonial {
                p { class: "nav-muted", "Status: {status}" }
                if let Some((verb, label)) = publication_action {
                    form {
                        method: "post",
                        action: format!("/app/admin/projects/{code}/testimonial/{verb}"),
                        input { r#type: "hidden", name: "_csrf", value: "{csrf}" }
                        button { r#type: "submit", class: "nav-btn nav-btn--secondary", "{label}" }
                    }
                }
            }
            FormCard {
                title: "Enter client testimonial".to_string(),
                action: format!("/app/admin/projects/{code}/testimonial"),
                submit_label: "Save testimonial".to_string(),
                heading: Heading::Hidden,
                csrf_token: Some(csrf.to_string()),
                intro: rsx! {
                    p { class: "nav-muted",
                        "As an admin, you attest that the client supplied this text and agreed to its use."
                    }
                },
                fields: vec![
                    Field::textarea("Testimonial", "quote", testimonial.quote, 5)
                        .required()
                        .help("Enter the client's words exactly as supplied."),
                    Field::text("Attribution", "attribution", testimonial.attribution)
                        .help("Optional. Use only an attribution the client supplied."),
                    Field::radio(
                        "Publication",
                        "publication",
                        vec![
                            crate::components::Choice::new("private", "Keep private"),
                            crate::components::Choice::new("public", "Request public use"),
                        ],
                        Some(publication.to_string()),
                    )
                    .required()
                    .help("Public use is a request and still needs the normal publication approval."),
                ],
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_testimonial_form_preserves_client_words_and_attribution_boundary() {
        let html = dioxus_ssr::render_element(admin_testimonial_card(
            "project",
            "TOK",
            Some(&AdminTestimonialView {
                quote: "Exact words from the client.".to_string(),
                attribution: "Client supplied attribution".to_string(),
                public_requested: true,
                published: false,
            }),
        ));
        assert!(
            html.contains(
                "As an admin, you attest that the client supplied this text and agreed to its use."
            ),
            "{html}"
        );
        assert!(html.contains("Exact words from the client."), "{html}");
        assert!(html.contains("Client supplied attribution"), "{html}");
        assert!(
            html.contains(r#"action="/app/admin/projects/project/testimonial""#),
            "{html}"
        );
        assert!(html.contains(r#"value="public" checked"#), "{html}");
    }

    fn testimonial_card_html(public_requested: bool, published: bool) -> String {
        dioxus_ssr::render_element(admin_testimonial_card(
            "project",
            "TOK",
            Some(&AdminTestimonialView {
                quote: "Exact words from the client.".to_string(),
                attribution: String::new(),
                public_requested,
                published,
            }),
        ))
    }

    #[test]
    fn a_testimonial_without_consent_is_private_and_offers_no_publication_button() {
        let html = testimonial_card_html(false, false);
        assert!(html.contains("Status: Private"), "{html}");
        assert!(!html.contains("/testimonial/publish"), "{html}");
        assert!(!html.contains("/testimonial/unpublish"), "{html}");
    }

    #[test]
    fn a_consented_unpublished_testimonial_awaits_approval_and_offers_publish_only() {
        let html = testimonial_card_html(true, false);
        assert!(html.contains("Status: Awaiting approval"), "{html}");
        assert!(
            html.contains(r#"action="/app/admin/projects/project/testimonial/publish""#),
            "{html}"
        );
        assert!(html.contains(">Publish<"), "{html}");
        assert!(!html.contains("/testimonial/unpublish"), "{html}");
        assert!(html.contains(r#"name="_csrf" value="TOK""#), "{html}");
    }

    #[test]
    fn a_published_testimonial_offers_unpublish_only() {
        let html = testimonial_card_html(true, true);
        assert!(html.contains("Status: Published"), "{html}");
        assert!(
            html.contains(r#"action="/app/admin/projects/project/testimonial/unpublish""#),
            "{html}"
        );
        assert!(html.contains(">Unpublish<"), "{html}");
        assert!(!html.contains("/testimonial/publish"), "{html}");
        assert!(html.contains(r#"name="_csrf" value="TOK""#), "{html}");
    }

    #[test]
    fn no_testimonial_yet_shows_no_status_or_publication_buttons() {
        let html = dioxus_ssr::render_element(admin_testimonial_card("project", "TOK", None));
        assert!(!html.contains("Status:"), "{html}");
        assert!(!html.contains("/testimonial/publish"), "{html}");
    }

    #[test]
    fn a_new_admin_testimonial_defaults_to_private_with_empty_fields() {
        let html = dioxus_ssr::render_element(admin_testimonial_card("project", "TOK", None));
        assert!(html.contains(r#"value="private" checked"#), "{html}");
        assert!(html.contains(r#"name="quote""#), "{html}");
        assert!(html.contains(r#"name="attribution""#), "{html}");
        assert!(!html.contains(r#"value="public" checked"#), "{html}");
    }
}
