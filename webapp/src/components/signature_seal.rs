//! The navigator-signature seal: the post-execution mark a matter page
//! shows on a Notation whose e-signature envelope has completed.
//!
//! The completion webhook (`portal/src/esignature_webhook.rs`) classifies
//! the provider's callback, stamps `signed_at` on the Notation's
//! `signature` row, and archives the CompletedDocuments — the signed PDF
//! and the Certificate of Completion, the ESIGN record — into object
//! storage. This component renders exactly those facts and nothing else:
//! it is never built from storage presence or workflow state alone, so it
//! cannot read as executed before the provider confirmed it.
//!
//! Like every matter-surface element it needs no JavaScript: static markup,
//! plain links, and a class the theme styles.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

/// The seal's data: what the completion webhook recorded and archived.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default, Debug)]
pub struct SignatureSealData {
    /// When the provider reported the envelope executed, as the webhook
    /// stamped it on the signature row (an RFC 3339 string).
    pub signed_at: String,
    /// The e-signature provider that executed the envelope, display-ready
    /// (`DocuSign`).
    pub provider: String,
    /// The executed document's download href, when the signed PDF the
    /// webhook archived is on file.
    pub signed_href: Option<String>,
    /// The Certificate of Completion's download href, when the certificate
    /// the webhook archived is on file.
    pub certificate_href: Option<String>,
}

/// The date a reader sees: the `YYYY-MM-DD` prefix of the RFC 3339 stamp.
/// The stamp the webhook writes is always at least that long; a shorter
/// string is shown as-is rather than truncated.
#[must_use]
pub fn signed_on(signed_at: &str) -> &str {
    signed_at.get(..10).unwrap_or(signed_at)
}

/// The provider as a reader says it. The store keeps the provider's
/// lowercased key (`docusign`); the display name is the brand the firm
/// knows.
#[must_use]
pub fn provider_label(provider: &str) -> &str {
    match provider {
        "docusign" => "DocuSign",
        other => other,
    }
}

/// The navigator-signature element, rendered after the envelope completes.
#[component]
pub fn SignatureSeal(seal: SignatureSealData) -> Element {
    rsx! {
        figure {
            class: "navigator-signature",
            "data-state": "executed",
            "data-provider": "{seal.provider}",
            figcaption { class: "navigator-signature__label", "Executed" }
            p { class: "navigator-signature__meta",
                "Signed through {provider_label(&seal.provider)} on {signed_on(&seal.signed_at)}."
            }
            if seal.signed_href.is_some() || seal.certificate_href.is_some() {
                p { class: "navigator-signature__links",
                    if let Some(href) = seal.signed_href.as_ref() {
                        a { class: "nav-link", href: "{href}", "Signed copy" }
                        if seal.certificate_href.is_some() {
                            " · "
                        }
                    }
                    if let Some(href) = seal.certificate_href.as_ref() {
                        a { class: "nav-link", href: "{href}", "Certificate of Completion" }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_seal(seal: SignatureSealData) -> String {
        fn app(seal: SignatureSealData) -> Element {
            rsx! { SignatureSeal { seal } }
        }
        let mut dom = VirtualDom::new_with_props(app, seal);
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    /// The fixture webhook payload the completion webhook classifies, and
    /// the seal facts its classification leaves behind: the completed
    /// `signature` row (stamped `signed_at`) and the two archived
    /// CompletedDocuments. This is the payload shape
    /// `portal/src/esignature_webhook.rs` parses on
    /// `/webhook/esignature/:secret`.
    const FIXTURE_COMPLETED_WEBHOOK: &str = r#"{
        "event": "envelope-completed",
        "data": {
            "envelopeId": "env-seal-1",
            "envelopeSummary": { "status": "completed" }
        }
    }"#;

    /// The seal facts a completed webhook payload leaves behind.
    fn fixture_seal() -> SignatureSealData {
        let payload: serde_json::Value = serde_json::from_str(FIXTURE_COMPLETED_WEBHOOK).unwrap();
        assert_eq!(payload["event"], "envelope-completed");
        SignatureSealData {
            signed_at: "2026-06-30T00:00:00Z".into(),
            provider: "docusign".into(),
            signed_href: Some("/app/notations/n1/documents/signed".into()),
            certificate_href: Some("/app/notations/n1/documents/certificate".into()),
        }
    }

    #[test]
    fn the_seal_renders_the_executed_element_with_its_documents() {
        let html = render_seal(fixture_seal());
        assert!(html.contains(r#"class="navigator-signature""#), "{html}");
        assert!(html.contains(r#"data-state="executed""#), "{html}");
        assert!(html.contains("Executed"), "{html}");
        assert!(
            html.contains("Signed through DocuSign on 2026-06-30."),
            "{html}"
        );
        assert!(
            html.contains(r#"href="/app/notations/n1/documents/signed""#),
            "{html}"
        );
        assert!(
            html.contains(r#"href="/app/notations/n1/documents/certificate""#),
            "{html}"
        );
        assert!(!html.contains("style="), "{html}");
    }

    /// A seal whose archive is still landing shows the executed mark and
    /// the signing date — the webhook already confirmed execution — with
    /// only the links that exist yet, never a dead one.
    #[test]
    fn a_seal_with_a_missing_archive_omits_only_that_link() {
        let mut seal = fixture_seal();
        seal.certificate_href = None;
        let html = render_seal(seal);
        assert!(html.contains(r#"data-state="executed""#), "{html}");
        assert!(
            html.contains(r#"href="/app/notations/n1/documents/signed""#),
            "{html}"
        );
        assert!(!html.contains("documents/certificate"), "{html}");
        assert!(!html.contains(" · "), "{html}");
    }

    #[test]
    fn stamps_and_providers_read_as_their_display_forms() {
        assert_eq!(signed_on("2026-06-30T00:00:00Z"), "2026-06-30");
        assert_eq!(signed_on("bare"), "bare");
        assert_eq!(provider_label("docusign"), "DocuSign");
        assert_eq!(provider_label("wet-ink"), "wet-ink");
    }
}
