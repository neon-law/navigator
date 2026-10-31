//! The Nautilus Legal Sandbox gate.
//!
//! `/app/nautilus` is the subscriber's private workspace promised by the
//! [Legal Sandbox](https://www.neonlaw.com/glossary/legal-sandbox) glossary
//! entry: a session with the AI assistant backed by the subscription's $5 a
//! month of model usage. The page rides the same session boundary every other
//! `/app` page takes — [`crate::gated`] — plus one more layer of its own: an
//! active-subscription check that decides between the composer and the
//! preview state.
//!
//! The boundary decides *that* there is a caller; this layer decides *what*
//! the caller may use. A caller is identified by the [`SessionData`] the
//! boundary resolved (a browser cookie or a CLI bearer blob), or — for a
//! deployment with an enforced OIDC verifier — by the [`AuthClaims`] a raw
//! bearer JWT carries. Either way the identity resolves to a `persons` row,
//! and the caller's Xero-backed Purchases answer the question: an active
//! Purchase of a Nautilus offering opens the composer, anything else sees the
//! preview.
//!
//! The decision itself is pure: [`access_for`] over the caller's Purchases.
//! The middleware only resolves identity and ledger, so the money question is
//! unit-testable without a database.
//!
//! The four credit states the spec names (Active, Low balance, Exhausted,
//! Preview) are the usage ledger's concern — a sibling change. This gate
//! fixes only who reaches the composer at all.

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Extension, Router};
use store::persons::{self, Person};
use store::postgres::PostgresDb;
use store::purchases::{self, Purchase, PurchaseState};
use store::surreal::SurrealDb;

use crate::{gated, AppState, AuthClaims, SessionData};

/// The Legal Sandbox page.
pub const NAUTILUS_PATH: &str = "/app/nautilus";

/// The offerings whose active Purchase is a Nautilus subscription. Neon sells
/// the one subscription under two names — DeleteYourData.com and Nautilus —
/// and the services catalog records both as offerings of the same price pair,
/// so a gate keyed on either name would drift when a third name is published.
pub const NAUTILUS_OFFERINGS: [&str; 2] = ["delete-your-data-monthly", "delete-your-data-annual"];

/// What the gate decided about this caller. The sandbox page reads it from
/// the request extension this middleware inserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxAccess {
    /// An active Nautilus subscription backs this caller: the composer renders.
    Subscribed,
    /// Authenticated without an active subscription: the preview renders —
    /// a read-only description of the sandbox, no composer, no credit.
    Preview,
}

/// Whether an offering id is one of the Nautilus subscription's own.
#[must_use]
pub fn is_nautilus_offering(offering: &str) -> bool {
    NAUTILUS_OFFERINGS.contains(&offering)
}

/// The gate's decision over a caller's Purchases: the active Purchase of a
/// Nautilus offering is the whole test. An `invoiced`-but-unpaid subscription
/// is not yet in force — the Purchase ledger turns it `active` only when Xero
/// reports the invoice paid — so it previews, like no subscription at all.
#[must_use]
pub fn access_for(purchases: &[Purchase]) -> SandboxAccess {
    if purchases
        .iter()
        .any(|p| p.state == PurchaseState::Active && is_nautilus_offering(&p.offering))
    {
        SandboxAccess::Subscribed
    } else {
        SandboxAccess::Preview
    }
}

/// State the subscription layer reads. Cheap to clone.
#[derive(Clone)]
pub struct NautilusGateState {
    surreal: SurrealDb,
    /// A Postgres handle for the Purchase ledger. When absent, the layer
    /// resolves one from the environment per request — the same lazy posture
    /// the purchase page takes. When the environment is unconfigured too,
    /// every caller sees the preview: the gate fails closed rather than
    /// opening the composer because a database is missing.
    pg: Option<PostgresDb>,
}

/// Build the gated sandbox route family: the caller's session boundary, the
/// policy gate, and the subscription layer, innermost.
pub fn routes(state: &AppState) -> Router {
    routes_with(state, None)
}

/// [`routes`] with a caller-supplied Purchase ledger — a deployment that
/// already holds a pool passes it here; a test injects its isolated fixture.
pub fn routes_with(state: &AppState, pg: Option<PostgresDb>) -> Router {
    gated(
        state,
        Router::new()
            .route(NAUTILUS_PATH, get(sandbox_page))
            .route_layer(axum::middleware::from_fn_with_state(
                NautilusGateState {
                    surreal: state.surreal.clone(),
                    pg,
                },
                require_nautilus_subscription,
            )),
    )
}

/// The subscription layer: resolve the caller to a `persons` row, ask the
/// Purchase ledger what that row holds, and insert the [`SandboxAccess`]
/// the page renders from.
///
/// It runs innermost — after [`crate::auth::require_session`] has resolved
/// the session — so an anonymous request never reaches it. Should one
/// misroute here anyway, it answers with the same login door the boundary
/// uses rather than rendering a state for a caller it cannot name.
pub async fn require_nautilus_subscription(
    State(state): State<NautilusGateState>,
    mut req: Request,
    next: Next,
) -> Response {
    let identity = caller_identity(&req);
    let Some(identity) = identity else {
        return login_door(&req);
    };
    let access = access_for_caller(&state, &identity).await;
    req.extensions_mut().insert(access);
    next.run(req).await
}

/// Who the already-resolved boundary says the caller is: the session's
/// person where one rides the request, otherwise the OIDC subject of a raw
/// bearer JWT.
fn caller_identity(req: &Request) -> Option<Caller> {
    if let Some(session) = req.extensions().get::<SessionData>() {
        return Some(Caller::Session(Box::new(session.clone())));
    }
    req.extensions()
        .get::<AuthClaims>()
        .map(|claims| Caller::Oidc(claims.sub.clone()))
}

enum Caller {
    Session(Box<SessionData>),
    Oidc(String),
}

impl Caller {
    /// The `persons` row this caller resolves to, or `None` when the
    /// identity names nobody the store knows. An unresolvable caller is a
    /// preview caller — never an error the subscriber sees.
    ///
    /// A raw bearer JWT rides `require_session`'s JWT leg into the policy
    /// gate, which synthesizes a session for its Rego input — `person_id`
    /// unset, the IdP *subject* parked in the `email` slot — and inserts
    /// it. So a session without a person id resolves by OIDC subject
    /// first, the identity a bearer-JWT caller actually carries, and only
    /// then by email.
    async fn person(&self, surreal: &SurrealDb) -> Option<Person> {
        match self {
            Caller::Session(session) => {
                if let Some(person_id) = session.person_id {
                    return persons::find_by_id(surreal, person_id).await.ok().flatten();
                }
                if let Some(person) = persons::find_by_oidc_subject(surreal, &session.sub)
                    .await
                    .ok()
                    .flatten()
                {
                    return Some(person);
                }
                let email = session.email.as_deref()?;
                persons::find_by_any_email(surreal, email)
                    .await
                    .ok()
                    .flatten()
            }
            Caller::Oidc(subject) => persons::find_by_oidc_subject(surreal, subject)
                .await
                .ok()
                .flatten(),
        }
    }
}

async fn access_for_caller(state: &NautilusGateState, caller: &Caller) -> SandboxAccess {
    let Some(person) = caller.person(&state.surreal).await else {
        return SandboxAccess::Preview;
    };
    let Some(pg) = ledger(state).await else {
        tracing::warn!(person = %person.id, "nautilus gate: no Purchase ledger configured; rendering the preview");
        return SandboxAccess::Preview;
    };
    match purchases::for_person(&pg, person.id).await {
        Ok(purchases) => access_for(&purchases),
        Err(e) => {
            tracing::warn!(person = %person.id, error = %e, "nautilus gate: Purchase ledger unreadable; rendering the preview");
            SandboxAccess::Preview
        }
    }
}

/// The Purchase ledger for this request: the injected handle when there is
/// one, else the environment's.
async fn ledger(state: &NautilusGateState) -> Option<PostgresDb> {
    if let Some(pg) = &state.pg {
        return Some(pg.clone());
    }
    PostgresDb::from_env().await.ok()
}

/// The same anonymous-browser door [`crate::auth::require_session`] uses, so
/// a request that reaches this layer without a resolved identity lands where
/// every other `/app` page sends one.
fn login_door(req: &Request) -> Response {
    let path = req.uri().path();
    let return_to = req
        .uri()
        .path_and_query()
        .map_or_else(|| path.to_string(), ToString::to_string);
    Redirect::to(&format!(
        "/auth/login?return_to={}",
        crate::policy::percent_encode_path(&return_to)
    ))
    .into_response()
}

/// The placeholder bodies. The Dioxus sandbox page replaces these when it
/// lands — the routes, the gate, and the extension it reads are already its
/// contract, so the page swap touches only these two arms.
const COMPOSER_PLACEHOLDER: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Legal Sandbox — Neon Law</title></head>
<body data-sandbox="composer">
<h1>Legal Sandbox</h1>
<p>Your Nautilus subscription includes $5 of model usage each month. The composer is on its way; your credit accrues from your billing month.</p>
<p><em>Working in the sandbox is self-help: it is not legal advice, creates no attorney-client relationship, and no privilege. A lawyer review is a separate prepaid engagement.</em></p>
</body></html>"#;

const PREVIEW_PLACEHOLDER: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Legal Sandbox — Neon Law</title></head>
<body data-sandbox="preview">
<h1>Legal Sandbox</h1>
<p>The Legal Sandbox is part of a Nautilus subscription: a private workspace where you work with an AI assistant on your own legal questions and drafts, with $5 of model usage included every month.</p>
<p><em>Working in the sandbox is self-help: it is not legal advice, creates no attorney-client relationship, and no privilege. A lawyer review is a separate prepaid engagement.</em></p>
</body></html>"#;

async fn sandbox_page(Extension(access): Extension<SandboxAccess>) -> Response {
    match access {
        SandboxAccess::Subscribed => Html(COMPOSER_PLACEHOLDER).into_response(),
        SandboxAccess::Preview => Html(PREVIEW_PLACEHOLDER).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::{access_for, is_nautilus_offering, NAUTILUS_OFFERINGS};
    use crate::nautilus::SandboxAccess;
    use store::purchases::{Cadence, Purchase, PurchaseState};

    fn purchase(offering: &str, state: PurchaseState) -> Purchase {
        Purchase {
            id: uuid::Uuid::now_v7(),
            person_id: uuid::Uuid::now_v7(),
            email: "pat@example.com".into(),
            offering: offering.into(),
            offering_name: offering.into(),
            cadence: Cadence::Month,
            amount_cents: 2_900,
            currency: "USD".into(),
            account_code: "200".into(),
            state,
            xero_contact_id: None,
            xero_invoice_id: None,
            invoice_url: None,
            xero_repeating_invoice_id: None,
            inserted_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            activated_at: None,
        }
    }

    #[test]
    fn the_published_subscription_offerings_are_the_gate() {
        assert_eq!(
            NAUTILUS_OFFERINGS,
            ["delete-your-data-monthly", "delete-your-data-annual"]
        );
        assert!(is_nautilus_offering("delete-your-data-monthly"));
        assert!(is_nautilus_offering("delete-your-data-annual"));
        // A lookalike id, and an unrelated offering, are not the subscription.
        assert!(!is_nautilus_offering("delete-your-data"));
        assert!(!is_nautilus_offering("consultation"));
    }

    #[test]
    fn no_purchases_preview() {
        assert_eq!(access_for(&[]), SandboxAccess::Preview);
    }

    #[test]
    fn an_unpaid_subscription_previews() {
        // Invoicing and invoiced both precede Xero's PAID report.
        let invoicing = purchase("delete-your-data-monthly", PurchaseState::Invoicing);
        let invoiced = purchase("delete-your-data-annual", PurchaseState::Invoiced);
        assert_eq!(access_for(&[invoicing, invoiced]), SandboxAccess::Preview);
    }

    #[test]
    fn an_active_subscription_opens_the_composer() {
        let active = purchase("delete-your-data-monthly", PurchaseState::Active);
        assert_eq!(access_for(&[active]), SandboxAccess::Subscribed);
    }

    #[test]
    fn an_unrelated_active_offering_previews() {
        let active = purchase("consultation", PurchaseState::Active);
        assert_eq!(access_for(&[active]), SandboxAccess::Preview);
    }
}
