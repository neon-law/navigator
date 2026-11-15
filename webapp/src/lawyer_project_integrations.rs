//! The lawyer workbench's link to the latest Xero invoice.

use dioxus::prelude::*;

use crate::lawyer_project_detail::LawyerDetailView;

pub(crate) fn integrations_section(view: &LawyerDetailView) -> Element {
    rsx! {
        if let Some(url) = view.xero_invoice_url.as_ref() {
            section { class: "lawyer-detail__section project-integrations",
                h2 { "Integrations" }
                p { class: "lawyer-detail__integration-links",
                    a {
                        class: "nav-btn nav-btn--secondary",
                        href: "{url}",
                        target: "_blank",
                        rel: "noopener noreferrer",
                        "Xero"
                    }
                }
            }
        }
    }
}
