//! The Nautilus Legal Sandbox page (`/app/nautilus`) — the presentational
//! slice of the subscriber workspace ([Legal Sandbox](docs/glossary/legal-sandbox.md)).
//!
//! The page renders from an injected [`SandboxView`] only — the portal router
//! (wiring, later) resolves the JWT gate, the active-subscription check, and
//! the usage-ledger row, then hands this module wasm-safe props, the same seam
//! [`crate::client_intake::InjectedIntake`] uses. Nothing here calls a store or
//! a model backend.
//!
//! Every session carries the credit meter, the model-backend selector, and the
//! self-help disclosure. The credit state ([`credit_state`]) is a pure
//! function of the injected row: the composer stands in Active and Low
//! balance (a quiet notice at ≤ 20% remaining), the à la carte offer replaces
//! the composer once the credit is spent, and an authenticated visitor
//! without an active subscription sees the read-only preview — never a
//! composer.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

/// The page title in the browser tab.
const PAGE_TITLE: &str = "Legal Sandbox";

/// The billing-month credit row for the current subscriber, in integer cents —
/// the same unit the `billing` contract prices in. `None` renders the preview
/// state: an authenticated visitor without an active Nautilus subscription.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct SandboxCredit {
    /// The monthly grant: 500 cents ($5.00) per the glossary contract.
    pub granted_cents: u64,
    /// What the ledger still holds for this billing month, in cents.
    pub remaining_cents: u64,
    /// The backend this session's completions are metered against, e.g.
    /// `"Claude Sonnet (Vertex AI)"`.
    pub model_backend: String,
    /// Every backend the gateway exposes, the session default first.
    pub backends: Vec<String>,
}

/// The wasm-safe page contract the portal injects. Every field renders.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct SandboxView {
    /// The resolved brand's tokens stylesheet href, so the page wears its own
    /// palette rather than the firm's on a non-default host.
    #[serde(default)]
    pub tokens_href: String,
    /// The billing-month credit row; `None` means no active subscription.
    pub credit: Option<SandboxCredit>,
    /// The CSRF token the composer's native POST carries.
    pub csrf_token: String,
}

/// The four credit states a sandbox session can render, per the spec's
/// session-flow table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreditState {
    /// More than 20% of the grant remains: full composer.
    Active,
    /// At or below 20% of the grant remains: composer plus a quiet notice.
    LowBalance,
    /// The credit is spent: the à la carte offer replaces the composer. The
    /// crossing completion already completed and was billed.
    Exhausted,
    /// Authenticated, no active subscription: read-only description.
    Preview,
}

/// The low-balance threshold as a fraction of the grant (denominator), so the
/// comparison stays in integer arithmetic: `remaining × 5 ≤ granted` is
/// `remaining ≤ grant / 5` (20%) with no division to round.
const LOW_BALANCE_NUMERATOR: u64 = 5;

/// Classify the session from the injected credit row. A pure function so the
/// render tests can pin each state and the wiring layer cannot drift from
/// them.
#[must_use]
pub fn credit_state(credit: Option<&SandboxCredit>) -> CreditState {
    let Some(row) = credit else {
        return CreditState::Preview;
    };
    match row.remaining_cents {
        0 => CreditState::Exhausted,
        remaining if remaining * LOW_BALANCE_NUMERATOR <= row.granted_cents => {
            CreditState::LowBalance
        }
        _ => CreditState::Active,
    }
}

/// Cents to a plain-dollar figure — the meter shows subscriber-English money,
/// never bare cent counts.
fn format_cents(cents: u64) -> String {
    format!("${}.{:02}", cents / 100, cents % 100)
}

/// Percent of the grant remaining, rounded down — meter copy only; the state
/// comparison in [`credit_state`] never relies on it.
fn remaining_percent(credit: &SandboxCredit) -> u64 {
    if credit.granted_cents == 0 {
        return 0;
    }
    credit.remaining_cents * 100 / credit.granted_cents
}

/// The self-help disclosure, visible in every state: working in the sandbox
/// never creates an attorney-client relationship or privilege, and a lawyer
/// review is a separate prepaid engagement.
fn disclosure() -> Element {
    rsx! {
        p { class: "nav-muted legal-sandbox-disclosure",
            "Working in the sandbox is self-help: it does not create an "
            "attorney-client relationship or privilege. A lawyer review of "
            "your work is a separate prepaid engagement."
        }
    }
}

/// The credit meter — remaining cents and percent of the grant — plus the
/// model-backend selector. Rendered in every metered state.
fn meter(credit: &SandboxCredit) -> Element {
    rsx! {
        section { class: "legal-sandbox-meter portal-detail__section",
            h2 { "Credit" }
            p {
                class: "legal-sandbox-meter__balance",
                "{format_cents(credit.remaining_cents)}",
                " of {format_cents(credit.granted_cents)}",
                " ({remaining_percent(credit)}% remaining)"
            }
            label { class: "legal-sandbox-meter__backend-label", r#for: "legal-sandbox-backend",
                "Model backend"
            }
            select {
                class: "nav-input",
                id: "legal-sandbox-backend",
                name: "backend",
                for option in credit.backends.iter() {
                    option {
                        value: "{option}",
                        selected: *option == credit.model_backend,
                        "{option}"
                    }
                }
            }
        }
    }
}

/// The composer — a plain `<textarea>` and a native POST (no HTMX, no
/// JavaScript) to the sandbox messages handler, which redirects back (PRG).
/// The handler itself lands with the wiring; the form already points there.
fn composer(view: &SandboxView) -> Element {
    rsx! {
        section { class: "portal-detail__section",
            h2 { "Ask the sandbox" }
            form { class: "legal-sandbox-composer", method: "post",
                action: "/app/nautilus/messages",
                input { r#type: "hidden", name: "_csrf", value: "{view.csrf_token}" }
                textarea {
                    class: "nav-input",
                    id: "legal-sandbox-prompt",
                    name: "prompt",
                    rows: "4",
                    placeholder: "Describe your legal question or the draft you want to start…",
                    required: true,
                }
                button { class: "nav-btn nav-btn--primary", r#type: "submit", "Send" }
            }
        }
    }
}

/// The loaded page. Split from the component so the tests render a fixed view
/// without standing up the server function — and so the wiring layer can
/// mount exactly this body once the route lands.
pub fn sandbox_body(view: &SandboxView) -> Element {
    rsx! {
        document::Title { "{PAGE_TITLE}" }
        document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
        document::Stylesheet { href: "{view.tokens_href}" }

        main { id: "legal-sandbox", class: "nav-theme portal-conversation",
            h1 { "Legal Sandbox" }
            p { class: "nav-muted",
                "Your private workspace for your own legal questions and "
                "drafts. Your subscription includes $5.00 of model usage each "
                "month; anything beyond that is priced à la carte."
            }
            {disclosure()}

            match view.credit.as_ref() {
                Some(credit) => {
                    rsx! {
                        {meter(credit)}
                        match credit_state(view.credit.as_ref()) {
                            CreditState::Active => rsx! { {composer(view)} },
                            CreditState::LowBalance => rsx! {
                                p { class: "status-chip status-chip--due legal-sandbox-low-balance",
                                    "Your credit is running low — {format_cents(credit.remaining_cents)} left this month."
                                }
                                {composer(view)}
                            },
                            CreditState::Exhausted => rsx! {
                                section { class: "legal-sandbox-exhausted portal-detail__section",
                                    h2 { "This month's credit is spent" }
                                    p {
                                        "Your included $5.00 of model usage is used up. "
                                        "Anything further is priced à la carte — reply to "
                                        "your Nautilus receipt to be quoted for the overage."
                                    }
                                }
                            },
                            CreditState::Preview => rsx! {},
                        }
                    }
                }
                None => rsx! {
                    section { class: "legal-sandbox-preview portal-detail__section",
                        h2 { "The Legal Sandbox comes with Nautilus" }
                        p {
                            "An active Nautilus subscription unlocks this "
                            "workspace: your own private sessions with an AI "
                            "assistant on your legal questions and drafts, with "
                            "$5.00 of model usage included each month."
                        }
                        p {
                            a { class: "nav-btn nav-btn--primary", href: "/nautilus",
                                "See Nautilus" }
                        }
                    }
                },
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// The default fixture: an active session on a full 500¢ grant.
    fn credit(remaining_cents: u64) -> SandboxCredit {
        SandboxCredit {
            granted_cents: 500,
            remaining_cents,
            model_backend: "Claude Sonnet (Vertex AI)".to_string(),
            backends: vec![
                "Claude Sonnet (Vertex AI)".to_string(),
                "Local open-weights".to_string(),
            ],
        }
    }

    fn view(credit: Option<SandboxCredit>) -> SandboxView {
        SandboxView {
            tokens_href: String::new(),
            credit,
            csrf_token: "TOK".to_string(),
        }
    }

    fn render(view: &SandboxView) -> String {
        dioxus_ssr::render_element(sandbox_body(view))
    }

    // -- state classification ------------------------------------------------

    /// > 20% remaining is Active.
    #[test]
    fn more_than_twenty_percent_remaining_is_active() {
        assert_eq!(credit_state(Some(&credit(400))), CreditState::Active);
    }

    /// Exactly 20% — the boundary — is Low balance (≤ 20%).
    #[test]
    fn exactly_twenty_percent_remaining_is_low_balance() {
        assert_eq!(credit_state(Some(&credit(100))), CreditState::LowBalance);
        assert_eq!(credit_state(Some(&credit(99))), CreditState::LowBalance);
    }

    /// Zero remaining is Exhausted, whatever the grant was.
    #[test]
    fn zero_remaining_is_exhausted() {
        assert_eq!(credit_state(Some(&credit(0))), CreditState::Exhausted);
    }

    /// No credit row at all is the Preview state.
    #[test]
    fn no_credit_row_is_preview() {
        assert_eq!(credit_state(None), CreditState::Preview);
    }

    // -- rendering per state --------------------------------------------------

    /// Active: meter, backend selector, and a live composer.
    #[test]
    fn the_active_state_renders_the_meter_and_a_live_composer() {
        let html = render(&view(Some(credit(500))));
        assert!(html.contains("$5.00"), "{html}");
        assert!(html.contains("100% remaining"), "{html}");
        assert!(html.contains("legal-sandbox-composer"), "{html}");
        assert!(html.contains("legal-sandbox-backend"), "{html}");
        assert!(!html.contains("legal-sandbox-low-balance"), "{html}");
    }

    /// Low balance: the quiet notice stands next to the composer — no
    /// interruption, no à la carte offer.
    #[test]
    fn the_low_balance_state_shows_the_notice_beside_the_composer() {
        let html = render(&view(Some(credit(100))));
        assert!(html.contains("legal-sandbox-low-balance"), "{html}");
        assert!(html.contains("$1.00 left this month"), "{html}");
        assert!(html.contains("legal-sandbox-composer"), "{html}");
        assert!(!html.contains("legal-sandbox-exhausted"), "{html}");
    }

    /// Exhausted: the à la carte offer replaces the composer — the crossing
    /// completion already completed and was billed; this one waits for consent.
    #[test]
    fn the_exhausted_state_replaces_the_composer_with_the_a_la_carte_offer() {
        let html = render(&view(Some(credit(0))));
        assert!(html.contains("legal-sandbox-exhausted"), "{html}");
        assert!(html.contains("à la carte"), "{html}");
        assert!(!html.contains("legal-sandbox-composer"), "{html}");
    }

    /// Preview: the read-only description — no composer, no meter, no credit.
    #[test]
    fn the_preview_state_renders_the_description_without_a_composer() {
        let html = render(&view(None));
        assert!(html.contains("legal-sandbox-preview"), "{html}");
        assert!(!html.contains("legal-sandbox-composer"), "{html}");
        assert!(!html.contains("legal-sandbox-meter"), "{html}");
        assert!(!html.contains("legal-sandbox-exhausted"), "{html}");
    }

    // -- invariants -----------------------------------------------------------

    /// The non-engagement disclosure renders in all four states.
    #[test]
    fn the_disclosure_renders_in_every_state() {
        for state in [
            view(Some(credit(500))),
            view(Some(credit(100))),
            view(Some(credit(0))),
            view(None),
        ] {
            let html = render(&state);
            assert!(
                html.contains("legal-sandbox-disclosure"),
                "disclosure missing from: {html}"
            );
            assert!(
                html.contains("does not create an attorney-client relationship"),
                "disclosure text missing from: {html}"
            );
        }
    }

    /// The composer is a native POST carrying the CSRF token — no JavaScript.
    #[test]
    fn the_composer_posts_natively_with_the_csrf_token() {
        let html = render(&view(Some(credit(500))));
        assert!(html.contains(r#"method="post""#), "{html}");
        assert!(
            html.contains(r#"action="/app/nautilus/messages""#),
            "{html}"
        );
        assert!(html.contains(r#"name="_csrf""#), "{html}");
        assert!(html.contains("TOK"), "{html}");
        assert!(!html.contains("<script"), "{html}");
    }

    /// The backend selector marks the session's backend and offers the other
    /// one.
    #[test]
    fn the_backend_selector_marks_the_session_backend() {
        let html = render(&view(Some(credit(500))));
        assert!(
            html.contains(r#"value="Claude Sonnet (Vertex AI)" selected=true"#),
            "session backend not selected: {html}"
        );
        assert!(html.contains("Local open-weights"), "{html}");
    }

    /// Money is subscriber-English dollars, never bare cent counts.
    #[test]
    fn the_meter_shows_dollars_not_cents() {
        let html = render(&view(Some(credit(375))));
        assert!(html.contains("$3.75"), "{html}");
        assert!(html.contains("75% remaining"), "{html}");
        assert!(!html.contains("375¢"), "{html}");
    }
}
