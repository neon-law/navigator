//! The navigator-signature seal: the signature-state element a matter page
//! shows on a Notation with an e-signature envelope.
//!
//! The completion webhook (`portal/src/esignature_webhook.rs`) classifies
//! the provider's callback, stamps `signed_at` on the Notation's
//! `signature` row, and archives the `CompletedDocuments` — the signed PDF
//! and the Certificate of Completion, the ESIGN record — into object
//! storage. The executed state renders exactly those facts and nothing
//! else: it is never built from storage presence or workflow state alone,
//! so it cannot read as executed before the provider confirmed it. The
//! other two states say so in words: an outstanding envelope shows an
//! empty labelled signature line, and a declined, voided, or expired
//! envelope is marked in so many words — never a seal.
//!
//! Like every matter-surface element it needs no JavaScript: static markup,
//! plain links, and a class the theme styles. Every colour resolves through
//! a `--nav-*` token, so the element follows the brand and the light/dark
//! schemes without a media query of its own.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

/// Where the Notation's envelope stands. The store's `SignatureState` maps
/// onto this at the wiring seam: `requested` is [`SealState::Awaiting`],
/// `completed` is [`SealState::Signed`], and `declined`/`voided`/`expired`
/// keep their names — the three states that must never render a seal.
/// `Default` is the pre-execution state.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SealState {
    /// The envelope is out for signature; the element shows empty labelled
    /// signature lines, one per declared signer.
    #[default]
    Awaiting,
    /// The provider confirmed every signer executed the envelope.
    Signed,
    /// A signer declined the envelope.
    Declined,
    /// The envelope was voided before completion.
    Voided,
    /// The envelope expired before completion.
    Expired,
}

impl SealState {
    /// The `data-state` attribute the theme keys off.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Awaiting => "awaiting",
            Self::Signed => "executed",
            Self::Declined => "declined",
            Self::Voided => "voided",
            Self::Expired => "expired",
        }
    }

    /// Whether this state renders the seal mark. Only the provider-confirmed
    /// execution does.
    #[must_use]
    pub fn is_sealed(self) -> bool {
        matches!(self, Self::Signed)
    }
}

/// One signer's role on the envelope. The template's `signers:` order
/// declares them; anything outside the three party roles reads as a
/// third party.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SealRole {
    /// The client of the engagement.
    #[default]
    Client,
    /// The Firm — a seal line for the Firm role carries the signing
    /// attorney's bar number.
    Firm,
    /// Any other signer: a witness, a notary, the other side.
    ThirdParty,
}

impl SealRole {
    /// The role as a reader says it.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Firm => "firm",
            Self::ThirdParty => "third party",
        }
    }
}

/// One signer's line on the seal: who, in what role, and — when the
/// provider reported it — the date-time they signed, with its time zone.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default, Debug)]
pub struct SealSigner {
    /// The signer's name as the envelope declared it.
    pub name: String,
    /// The signer's role on the envelope.
    pub role: SealRole,
    /// The signing attorney's bar number — the Firm role's line only.
    pub bar_number: Option<String>,
    /// When this signer signed, as the provider stamped it (an RFC 3339
    /// string; its offset is the time zone a reader sees).
    pub signed_at: Option<String>,
}

/// The seal's data: what the webhook recorded and archived, or what the
/// envelope's live state is.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default, Debug)]
pub struct SignatureSealData {
    /// Where the envelope stands. Only [`SealState::Signed`] renders the
    /// seal mark.
    pub state: SealState,
    /// The e-signature provider that owns the envelope, display-ready
    /// (`DocuSign`).
    pub provider: String,
    /// When the provider reported the envelope executed, as the webhook
    /// stamped it on the signature row (an RFC 3339 string). Present only
    /// for an executed envelope.
    pub signed_at: Option<String>,
    /// The declared signers, in the template's `signers:` order. Empty
    /// until the wiring supplies them; the element then renders what it
    /// has rather than inventing names.
    pub signers: Vec<SealSigner>,
    /// The provider's envelope id — the `docusign_envelope_id` pointer a
    /// reader can match against the certificate.
    pub envelope_id: Option<String>,
    /// The signed PDF's SHA-256, so a reader can match the link below
    /// against the `<name>_signed.pdf` they hold.
    pub signed_sha256: Option<String>,
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

/// One signer's line inside the seal mark: name and role, the Firm role
/// carrying the attorney's bar number, and the signed-at stamp when the
/// provider reported one for this signer.
fn signer_line(signer: &SealSigner) -> Element {
    let role = match (&signer.role, &signer.bar_number) {
        (SealRole::Firm, Some(bar)) => format!("{}, Bar No. {bar}", signer.role.label()),
        (role, _) => role.label().to_string(),
    };
    rsx! {
        span { class: "navigator-signature__signer",
            "{signer.name} — {role}"
            if let Some(stamp) = signer.signed_at.as_ref() {
                span { class: "navigator-signature__signed-at", " {stamp}" }
            }
        }
    }
}

/// The navigator-signature element: the state a Notation's envelope is in,
/// rendered from the provider's recorded evidence alone.
#[component]
pub fn SignatureSeal(seal: SignatureSealData) -> Element {
    let provider = provider_label(&seal.provider);
    let refused = matches!(
        seal.state,
        SealState::Declined | SealState::Voided | SealState::Expired
    );
    let class = if refused {
        "navigator-signature navigator-signature--refused"
    } else {
        "navigator-signature"
    };
    rsx! {
        figure {
            class: "{class}",
            "data-state": "{seal.state.as_str()}",
            "data-provider": "{seal.provider}",
            match seal.state {
                SealState::Signed => rsx! {
                    figcaption { class: "navigator-signature__label", "Executed" }
                    p { class: "navigator-signature__seal",
                        strong { class: "navigator-signature__seal-title",
                            "Signed electronically via {provider}"
                        }
                        for signer in &seal.signers {
                            { signer_line(signer) }
                        }
                        if seal.signers.is_empty() {
                            if let Some(stamp) = seal.signed_at.as_ref() {
                                span { class: "navigator-signature__signed-at", "{stamp}" }
                            }
                        }
                    }
                    if let Some(stamp) = seal.signed_at.as_ref() {
                        p { class: "navigator-signature__meta",
                            "Signed through {provider} on {signed_on(stamp)}."
                        }
                    }
                },
                SealState::Awaiting => rsx! {
                    figcaption { class: "navigator-signature__label", "Awaiting signature" }
                    if seal.signers.is_empty() {
                        p { class: "navigator-signature__awaiting", "Awaiting signature." }
                    }
                    for signer in &seal.signers {
                        p { class: "navigator-signature__awaiting",
                            // The ruled line is decoration; the text beside
                            // it is what a screen reader reads.
                            span { class: "navigator-signature__line", "aria-hidden": "true" }
                            span { class: "navigator-signature__awaiting-label",
                                "Awaiting signature from {signer.name} ({signer.role.label()})."
                            }
                        }
                    }
                },
                SealState::Declined => rsx! {
                    figcaption { class: "navigator-signature__label", "Not executed" }
                    p { class: "navigator-signature__meta",
                        "A signer declined the envelope. This document was not executed."
                    }
                },
                SealState::Voided => rsx! {
                    figcaption { class: "navigator-signature__label", "Not executed" }
                    p { class: "navigator-signature__meta",
                        "The envelope was voided before completion. This document was not executed."
                    }
                },
                SealState::Expired => rsx! {
                    figcaption { class: "navigator-signature__label", "Not executed" }
                    p { class: "navigator-signature__meta",
                        "The envelope expired before completion. This document was not executed."
                    }
                },
            }
            if seal.state.is_sealed() {
                if seal.envelope_id.is_some() || seal.signed_sha256.is_some() {
                    p { class: "navigator-signature__facts",
                        if let Some(id) = seal.envelope_id.as_ref() {
                            "Envelope {id}. "
                        }
                        if let Some(hash) = seal.signed_sha256.as_ref() {
                            "SHA-256 {hash}. "
                        }
                    }
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
    /// `CompletedDocuments`. This is the payload shape
    /// `portal/src/esignature_webhook.rs` parses on
    /// `/webhook/esignature/:secret`.
    const FIXTURE_COMPLETED_WEBHOOK: &str = r#"{
        "event": "envelope-completed",
        "data": {
            "envelopeId": "env-seal-1",
            "envelopeSummary": { "status": "completed" }
        }
    }"#;

    /// The seal facts a completed webhook payload leaves behind, with the
    /// declared signers the envelope named.
    fn fixture_seal() -> SignatureSealData {
        let payload: serde_json::Value = serde_json::from_str(FIXTURE_COMPLETED_WEBHOOK).unwrap();
        assert_eq!(payload["event"], "envelope-completed");
        SignatureSealData {
            state: SealState::Signed,
            provider: "docusign".into(),
            signed_at: Some("2026-06-30T00:00:00Z".into()),
            signers: vec![
                SealSigner {
                    name: "Avery Client".into(),
                    role: SealRole::Client,
                    bar_number: None,
                    signed_at: Some("2026-06-30T17:05:00-07:00".into()),
                },
                SealSigner {
                    name: "Nicholas Shook".into(),
                    role: SealRole::Firm,
                    bar_number: Some("12345".into()),
                    signed_at: Some("2026-06-30T17:06:00-07:00".into()),
                },
            ],
            envelope_id: Some("env-seal-1".into()),
            signed_sha256: Some(
                "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08".into(),
            ),
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

    #[test]
    fn the_seal_mark_names_the_provider_its_signers_and_their_stamps() {
        let html = render_seal(fixture_seal());
        assert!(
            html.contains("Signed electronically via DocuSign"),
            "{html}"
        );
        assert!(html.contains("Avery Client — client"), "{html}");
        assert!(html.contains("2026-06-30T17:05:00-07:00"), "{html}");
        assert!(
            html.contains("Nicholas Shook — firm, Bar No. 12345"),
            "{html}"
        );
        assert!(
            html.contains(r#"class="navigator-signature__seal""#),
            "{html}"
        );
    }

    #[test]
    fn the_seal_carries_the_envelope_pointer_and_the_signed_hash() {
        let html = render_seal(fixture_seal());
        assert!(html.contains("Envelope env-seal-1."), "{html}");
        assert!(
            html.contains(
                "SHA-256 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08."
            ),
            "{html}"
        );
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

    /// The awaiting state shows empty labelled signature lines — one per
    /// declared signer — and never the seal mark. The ruled line is
    /// decoration; the real text carries the meaning for a screen reader.
    #[test]
    fn the_awaiting_state_shows_labelled_empty_lines_not_a_seal() {
        let mut seal = fixture_seal();
        seal.state = SealState::Awaiting;
        seal.signed_at = None;
        seal.envelope_id = None;
        seal.signed_sha256 = None;
        seal.signed_href = None;
        seal.certificate_href = None;
        let html = render_seal(seal);
        assert!(html.contains(r#"data-state="awaiting""#), "{html}");
        assert!(!html.contains("navigator-signature__seal"), "{html}");
        assert!(html.contains("Awaiting signature"), "{html}");
        assert!(
            html.contains("Awaiting signature from Avery Client (client)."),
            "{html}"
        );
        assert!(
            html.contains("Awaiting signature from Nicholas Shook (firm)."),
            "{html}"
        );
        // The ruled line is decoration only — hidden from the accessibility
        // tree, with no label of its own to contradict its hiding.
        assert!(
            html.contains(r#"class="navigator-signature__line" aria-hidden="true""#),
            "{html}"
        );
        assert!(!html.contains("aria-label="), "{html}");
        assert!(
            !html.contains("href="),
            "an unexecuted envelope links to nothing: {html}"
        );
    }

    /// A declined, voided, or expired envelope is marked in so many words —
    /// role-labelled so the reason reads as text — and never renders the
    /// seal mark or any executed fact.
    #[test]
    fn the_refused_states_are_marked_in_words_and_never_a_seal() {
        for (state, wording) in [
            (SealState::Declined, "A signer declined the envelope."),
            (
                SealState::Voided,
                "The envelope was voided before completion.",
            ),
            (
                SealState::Expired,
                "The envelope expired before completion.",
            ),
        ] {
            let mut seal = fixture_seal();
            seal.state = state;
            let html = render_seal(seal);
            assert!(
                html.contains(r#"class="navigator-signature navigator-signature--refused""#),
                "{state:?}: {html}"
            );
            assert!(!html.contains("navigator-signature__seal"), "{html}");
            assert!(html.contains("Not executed"), "{html}");
            assert!(html.contains(wording), "{html}");
            assert!(
                !html.contains("href="),
                "a refused envelope links to nothing: {html}"
            );
        }
    }

    /// An executed envelope whose per-signer facts have not landed yet —
    /// the wiring seam still to come — renders the mark from the
    /// envelope-level stamp rather than inventing names.
    #[test]
    fn an_executed_seal_without_signers_uses_the_envelope_stamp() {
        let mut seal = fixture_seal();
        seal.signers = Vec::new();
        let html = render_seal(seal);
        assert!(html.contains(r#"data-state="executed""#), "{html}");
        assert!(html.contains("2026-06-30T00:00:00Z"), "{html}");
        assert!(!html.contains("Avery Client"), "{html}");
    }

    #[test]
    fn stamps_and_providers_read_as_their_display_forms() {
        assert_eq!(signed_on("2026-06-30T00:00:00Z"), "2026-06-30");
        assert_eq!(signed_on("bare"), "bare");
        assert_eq!(provider_label("docusign"), "DocuSign");
        assert_eq!(provider_label("wet-ink"), "wet-ink");
        assert_eq!(SealState::Signed.as_str(), "executed");
        assert!(SealState::Signed.is_sealed());
        assert!(!SealState::Awaiting.is_sealed());
        assert!(!SealState::Declined.is_sealed());
        assert_eq!(SealRole::ThirdParty.label(), "third party");
    }
}
