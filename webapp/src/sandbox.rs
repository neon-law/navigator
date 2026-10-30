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
