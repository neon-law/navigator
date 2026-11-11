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

