//! The Nautilus Legal Sandbox.
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
//! preview. The decision itself is pure: [`access_for`] over the caller's
//! Purchases.
//!
//! A subscriber exchange walks the spec's session flow: the prompt lands in
//! the subscriber's [Assistant Chat](../../docs/glossary/assistant-chat.md)
//! transcript, the metered session runs the completion against the credit
//! ledger — nothing is charged for a failed completion — and a search-tool
//! call the model makes is executed against the read-only catalog tools and
//! answered back to the model, bounded, until a final reply closes the
//! exchange. Every step appends its turn to the transcript, so the page
//! renders the conversation the ledger paid for.

use std::sync::Arc;

use axum::extract::{Form, RawQuery, Request, State};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use chrono::Datelike;
use gateway::{
    MeteredError, MeteredOutcome, MeteredSession, ModelProvider, StoreLedger, UsageLedger,
};
use mcp::SearchBackends;
use serde::Deserialize;
use store::assistant_turns::{self, NewTurn, TurnKind};
use store::persons::{self, Person};
use store::postgres::PostgresDb;
use store::purchases::{self, Purchase, PurchaseState};
use store::sandbox_usage::billing_period;
use store::surreal::SurrealDb;
use webapp::sandbox::SandboxView;

use crate::{gated, AppState, AuthClaims, SessionData};

/// The Legal Sandbox page.
pub const NAUTILUS_PATH: &str = "/app/nautilus";

/// The message route the sandbox composer's native POST lands on. The page's
/// form posts here; the handler redirects back to [`NAUTILUS_PATH`] (PRG), so
/// a reload never re-submits an exchange.
pub const MESSAGES_PATH: &str = "/app/nautilus/messages";

/// The offerings whose active Purchase is a Nautilus subscription. Neon sells
/// the one subscription under two names — DeleteYourData.com and Nautilus —
/// and the services catalog records both as offerings of the same price pair,
/// so a gate keyed on either name would drift when a third name is published.
pub const NAUTILUS_OFFERINGS: [&str; 2] = ["delete-your-data-monthly", "delete-your-data-annual"];

/// The bounded number of tool passes one exchange may spend before the loop
/// stops and says so. A search that keeps asking for more searches is a loop,
/// not a session; the bound keeps one prompt from spinning on the subscriber's
/// credit.
const MAX_TOOL_PASSES: usize = 3;

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

/// The person the gate resolved, inserted beside the [`SandboxAccess`].
/// `None` when the identity names nobody the store knows — a preview caller.
#[derive(Debug, Clone, Copy)]
pub struct NautilusPerson(pub Option<uuid::Uuid>);

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

/// State the subscription layer reads, and the handlers complete against:
/// the transcript, the ledger, and the backends behind a session. Cheap to
/// clone.
#[derive(Clone)]
pub struct NautilusGateState {
    surreal: SurrealDb,
    /// A Postgres handle for the Purchase ledger. When absent, the layer
    /// resolves one from the environment per request — the same lazy posture
    /// the purchase page takes. When the environment is unconfigured too,
    /// every caller sees the preview: the gate fails closed rather than
    /// opening the composer because a database is missing.
    pg: Option<PostgresDb>,
    /// The model backend this deployment's sessions complete against. `None`
    /// in production until the first exchange lazily mints the Vertex
    /// provider from the environment — Application Default Credentials need
    /// an async first token, and boot must not block on GCP. A test injects
    /// the deterministic stub.
    model: Option<Arc<dyn ModelProvider>>,
    /// The sandbox search tools' provider clients. An unconfigured provider
    /// degrades its tool's searches to "search unavailable" inside the
    /// session rather than failing it.
    search: SearchBackends,
}

/// The model and search backends behind a sandbox session: production builds
/// them from the environment, a test injects them.
pub struct SandboxBackends {
    /// The model provider. `None` selects the production posture — the lazy
    /// Vertex provider minted from the environment on first use.
    pub model: Option<Arc<dyn ModelProvider>>,
    /// The search tools' provider clients.
    pub search: SearchBackends,
}

impl SandboxBackends {
    /// The production posture: search backends from the environment, the
    /// model backend lazy (see [`NautilusGateState`]).
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            model: None,
            search: SearchBackends::from_env(),
        }
    }
}

/// Build the gated sandbox route family: the caller's session boundary, the
/// policy gate, the subscription layer, and the CSRF check the composer's
/// POST rides.
pub fn routes(state: &AppState) -> Router {
    routes_with(state, None, SandboxBackends::from_env())
}

/// [`routes`] with caller-supplied seams — a deployment that already holds a
/// pool passes it here, and a test injects its isolated fixture and stub
/// backends.
pub fn routes_with(state: &AppState, pg: Option<PostgresDb>, backends: SandboxBackends) -> Router {
    let gate = NautilusGateState {
        surreal: state.surreal.clone(),
        pg,
        model: backends.model,
        search: backends.search,
    };
    gated(
        state,
        Router::new()
            .route(NAUTILUS_PATH, get(sandbox_page))
            .route(MESSAGES_PATH, post(sandbox_message))
            // Listed first, so it runs last: by the time the token check
            // reads the cookie, the subscription layer has already resolved
            // the caller.
            .layer(axum::middleware::from_fn_with_state(
                (state.sessions.clone(), crate::csrf::CsrfMode::Form),
                crate::csrf::require_csrf,
            ))
            // Listed last, so it runs first — every request through these
            // routes is resolved to its sandbox access before anything else
            // runs, and the handlers read the result from the extensions.
            .route_layer(axum::middleware::from_fn_with_state(
                gate.clone(),
                require_nautilus_subscription,
            ))
            .with_state(gate),
    )
}

/// The subscription layer: resolve the caller to a `persons` row, ask the
/// Purchase ledger what that row holds, and insert the [`SandboxAccess`] and
/// [`NautilusPerson`] the handlers render and complete against.
///
/// It runs after [`crate::auth::require_session`] has resolved the session,
/// so an anonymous request never reaches it. Should one misroute here anyway,
/// it answers with the same login door the boundary uses rather than
/// rendering a state for a caller it cannot name.
pub async fn require_nautilus_subscription(
    State(state): State<NautilusGateState>,
    mut req: Request,
    next: Next,
) -> Response {
    let identity = caller_identity(&req);
    let Some(identity) = identity else {
        return login_door(&req);
    };
    let (access, person) = access_for_caller(&state, &identity).await;
    req.extensions_mut().insert(access);
    req.extensions_mut().insert(NautilusPerson(person));
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

/// The gate's decision plus the person it decided for.
type Access = (SandboxAccess, Option<uuid::Uuid>);

async fn access_for_caller(state: &NautilusGateState, caller: &Caller) -> Access {
    let Some(person) = caller.person(&state.surreal).await else {
        return (SandboxAccess::Preview, None);
    };
    let person_id = person.id;
    let Some(pg) = ledger(state).await else {
        tracing::warn!(person = %person_id, "nautilus gate: no Purchase ledger configured; rendering the preview");
        return (SandboxAccess::Preview, Some(person_id));
    };
    match purchases::for_person(&pg, person_id).await {
        Ok(purchases) => (access_for(&purchases), Some(person_id)),
        Err(e) => {
            tracing::warn!(person = %person_id, error = %e, "nautilus gate: Purchase ledger unreadable; rendering the preview");
            (SandboxAccess::Preview, Some(person_id))
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

/// `GET /app/nautilus`: the page, rendered from the caller's access, credit
/// ledger row, and transcript. A preview caller sees the sandbox's
/// description and no composer; a subscriber sees their conversation and
/// their meter.
async fn sandbox_page(
    State(state): State<NautilusGateState>,
    Extension(access): Extension<SandboxAccess>,
    Extension(person): Extension<NautilusPerson>,
    session: Option<Extension<SessionData>>,
    query: RawQuery,
) -> Response {
    let turns = match person.0 {
        Some(person_id) => match assistant_turns::history(&state.surreal, person_id).await {
            Ok(turns) => turns.iter().map(sandbox_turn).collect(),
            // A transcript read that fails renders an empty conversation —
            // the page is the session's front door, not the place for a 500.
            Err(e) => {
                tracing::warn!(error = %e, "nautilus: transcript unreadable; rendering an empty chat");
                Vec::new()
            }
        },
        None => Vec::new(),
    };
    let credit = match (access, person.0) {
        (SandboxAccess::Subscribed, Some(person_id)) => credit_view(&state, person_id).await,
        _ => None,
    };
    let completion_failed = query
        .0
        .as_deref()
        .is_some_and(|q| q.contains("error=completion"));
    let view = SandboxView {
        tokens_href: crate::site_brand::current().tokens_href,
        credit,
        csrf_token: session.map(|s| s.csrf_token.clone()).unwrap_or_default(),
        turns,
        completion_failed,
    };
    Html(webapp::sandbox::page_html(&view)).into_response()
}

/// The page's turn view for a stored one: the tool turns name their tool.
fn sandbox_turn(turn: &assistant_turns::AssistantTurn) -> webapp::sandbox::SandboxTurn {
    match turn.kind {
        TurnKind::Prompt => webapp::sandbox::SandboxTurn::Prompt {
            content: turn.content.clone(),
        },
        TurnKind::Reply => webapp::sandbox::SandboxTurn::Reply {
            content: turn.content.clone(),
        },
        TurnKind::ToolCall => webapp::sandbox::SandboxTurn::ToolCall {
            tool: turn.tool_name.clone().unwrap_or_default(),
            arguments: turn.content.clone(),
        },
        TurnKind::ToolResult => webapp::sandbox::SandboxTurn::ToolResult {
            tool: turn.tool_name.clone().unwrap_or_default(),
            content: turn.content.clone(),
        },
    }
}

/// The billing-month credit row for the page's meter. The month's grant is
/// the subscription's contract, not a job's favor: a subscriber whose reset
/// job has not run yet is granted on first sight — idempotently, the same
/// seam the monthly reset uses — so the meter reads a real row. A read
/// failure renders no meter rather than failing the page.
async fn credit_view(
    state: &NautilusGateState,
    person_id: uuid::Uuid,
) -> Option<webapp::sandbox::SandboxCredit> {
    let period = current_period();
    let usage = match store::sandbox_usage::grant(&state.surreal, person_id, &period).await {
        Ok(usage) => usage,
        Err(e) => {
            tracing::warn!(person = %person_id, error = %e, "nautilus: credit row unprovisionable; rendering no meter");
            return None;
        }
    };
    Some(webapp::sandbox::SandboxCredit {
        granted_cents: u64::try_from(usage.granted_cents.max(0)).unwrap_or(0),
        remaining_cents: u64::try_from(usage.remaining_cents().max(0)).unwrap_or(0),
        model_backend: webapp::sandbox::VERTEX_BACKEND.to_string(),
        backends: vec![webapp::sandbox::VERTEX_BACKEND.to_string()],
    })
}

/// The billing month this boot serves, spelled `YYYY-MM`.
#[must_use]
fn current_period() -> String {
    let now = chrono::Utc::now().date_naive();
    billing_period(now.year(), now.month())
}

/// The composer's POST: one prompt, one exchange.
#[derive(Deserialize)]
struct PromptForm {
    prompt: String,
}

async fn sandbox_message(
    State(state): State<NautilusGateState>,
    Extension(access): Extension<SandboxAccess>,
    Extension(person): Extension<NautilusPerson>,
    Form(form): Form<PromptForm>,
) -> Response {
    // A preview caller has nothing to spend: land them back on the page
    // they already see, where the composer never renders.
    let Some(person_id) = person.0 else {
        return redirect_home();
    };
    if access != SandboxAccess::Subscribed {
        return redirect_home();
    }
    let prompt = form.prompt.trim();
    if prompt.is_empty() {
        return redirect_home();
    }

    match run_exchange(&state, person_id, prompt).await {
        Ok(()) => redirect_home(),
        Err(e) => {
            // The ask is already in the transcript; nothing was charged.
            // The redirect flags the page to say so.
            tracing::warn!(person = %person_id, error = %e, "nautilus: exchange failed; nothing was charged");
            Redirect::to(&format!("{NAUTILUS_PATH}?error=completion")).into_response()
        }
    }
}

/// The PRG redirect back to the page after a handled exchange.
fn redirect_home() -> Response {
    Redirect::to(NAUTILUS_PATH).into_response()
}

/// One exchange failed. Nothing was charged — every arm below is a path the
/// metered session either never reached the model on, or did not bill.
#[derive(Debug, thiserror::Error)]
enum SandboxExchangeError {
    /// No model backend this deployment can name: nothing ran.
    #[error("no model backend is configured for this deployment")]
    NoModelBackend,
    /// The model backend failed after the meter admitted it.
    #[error(transparent)]
    Provider(#[from] anyhow::Error),
    /// The usage ledger failed.
    #[error(transparent)]
    Ledger(#[from] store::sandbox_usage::SandboxUsageError),
    /// The transcript refused a write or a read.
    #[error(transparent)]
    Transcript(#[from] assistant_turns::AssistantTurnError),
}

async fn run_exchange(
    state: &NautilusGateState,
    person_id: uuid::Uuid,
    prompt: &str,
) -> Result<(), SandboxExchangeError> {
    let db = state.surreal.clone();
    // The transcript records the ask even when no answer comes back: the
    // page then shows the prompt and the nothing-charged notice.
    assistant_turns::append(
        &db,
        NewTurn {
            person_id,
            kind: TurnKind::Prompt,
            content: prompt.to_string(),
            tool_name: None,
            input_tokens: None,
            output_tokens: None,
        },
    )
    .await?;

    let period = current_period();
    // The subscription gate has verified this caller's active Purchase.
    // A direct composer POST needs the same idempotent grant as the page;
    // an existing period retains all spending and its overage flag.
    store::sandbox_usage::grant(&db, person_id, &period).await?;
    let provider = model_backend(state).await?;
    let ledger: Arc<dyn UsageLedger> = Arc::new(StoreLedger::new(db.clone()));

    for pass in 0..MAX_TOOL_PASSES {
        let turns = assistant_turns::history(&db, person_id).await?;
        let request = gateway::CompletionRequest {
            messages: request_messages(&turns),
            active_skills: session_skills(),
            tools: search_tool_specs(),
        };
        let session =
            MeteredSession::new(provider.clone(), ledger.clone(), person_id, period.clone());
        match session.complete(request).await {
            Ok(MeteredOutcome::Completed { result, .. } | MeteredOutcome::Unbilled { result }) => {
                if result.tool_calls.is_empty() {
                    let reply = result
                        .messages
                        .iter()
                        .filter(|m| m.role == gateway::Role::Assistant)
                        .map(|m| m.content.clone())
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    let reply = if reply.is_empty() {
                        "The model returned no text for that answer.".to_string()
                    } else {
                        reply
                    };
                    append_turn(&db, person_id, TurnKind::Reply, reply, None).await?;
                    return Ok(());
                }
                if pass + 1 == MAX_TOOL_PASSES {
                    append_turn(
                        &db,
                        person_id,
                        TurnKind::Reply,
                        "I stopped after the searches without a final answer — try a narrower question.".to_string(),
                        None,
                    )
                    .await?;
                    return Ok(());
                }
                for call in &result.tool_calls {
                    let arguments =
                        serde_json::to_string(&call.input).unwrap_or_else(|_| "{}".to_string());
                    append_turn(
                        &db,
                        person_id,
                        TurnKind::ToolCall,
                        arguments,
                        Some(call.name.clone()),
                    )
                    .await?;
                    let payload = execute_tool(&state.search, call).await;
                    append_turn(
                        &db,
                        person_id,
                        TurnKind::ToolResult,
                        payload,
                        Some(call.name.clone()),
                    )
                    .await?;
                }
            }
            // The credit was already crossed before this exchange: nothing
            // ran. The page renders the à la carte offer from the ledger row.
            Ok(MeteredOutcome::Exhausted) => return Ok(()),
            Err(MeteredError::Provider(e)) => return Err(SandboxExchangeError::Provider(e)),
            Err(MeteredError::Ledger(e)) => return Err(SandboxExchangeError::Ledger(e)),
        }
    }
    Ok(())
}

/// Append one turn, mapping the error into the exchange's own.
async fn append_turn(
    db: &SurrealDb,
    person_id: uuid::Uuid,
    kind: TurnKind,
    content: String,
    tool_name: Option<String>,
) -> Result<(), SandboxExchangeError> {
    assistant_turns::append(
        db,
        NewTurn {
            person_id,
            kind,
            content,
            tool_name,
            input_tokens: None,
            output_tokens: None,
        },
    )
    .await?;
    Ok(())
}

/// The model backend for this session: the injected one when a caller
/// supplied it, else the production Vertex provider, minted once per process
/// from the environment and memoized — Application Default Credentials need
/// an async first token, and boot must not block on GCP.
async fn model_backend(
    state: &NautilusGateState,
) -> Result<Arc<dyn ModelProvider>, SandboxExchangeError> {
    static VERTEX: tokio::sync::OnceCell<Option<Arc<dyn ModelProvider>>> =
        tokio::sync::OnceCell::const_new();
    if let Some(model) = &state.model {
        return Ok(model.clone());
    }
    let provider = VERTEX
        .get_or_try_init(|| async {
            let project = std::env::var("GOOGLE_CLOUD_PROJECT")
                .map_err(|_| SandboxExchangeError::NoModelBackend)?;
            let token = gateway::adc_token_provider()
                .await
                .map_err(|_| SandboxExchangeError::NoModelBackend)?;
            Ok::<_, SandboxExchangeError>(Some(Arc::new(gateway::VertexProvider::new(
                project, token,
            )) as Arc<dyn ModelProvider>))
        })
        .await?;
    provider.clone().ok_or(SandboxExchangeError::NoModelBackend)
}

/// The completion request's messages, built from the stored transcript:
/// prompts become user turns, replies become assistant turns, and each tool
/// result is folded into the trailing user turn — the gateway's `Message` is
/// text-only, so the results' text carries the model's information either
/// way. Tool-call turns contribute nothing to re-ask with.
#[must_use]
fn request_messages(turns: &[assistant_turns::AssistantTurn]) -> Vec<gateway::Message> {
    let mut messages: Vec<gateway::Message> = Vec::new();
    for turn in turns {
        match turn.kind {
            TurnKind::Prompt => messages.push(gateway::Message {
                role: gateway::Role::User,
                content: turn.content.clone(),
            }),
            TurnKind::Reply => messages.push(gateway::Message {
                role: gateway::Role::Assistant,
                content: turn.content.clone(),
            }),
            TurnKind::ToolCall => {}
            TurnKind::ToolResult => match messages.last_mut() {
                // Folded into the trailing user turn so the wire stays
                // strictly alternating — the APIs' conversation rule.
                Some(last) if last.role == gateway::Role::User => {
                    last.content.push_str("\n\n");
                    last.content.push_str(&tool_result_text(turn));
                }
                _ => messages.push(gateway::Message {
                    role: gateway::Role::User,
                    content: tool_result_text(turn),
                }),
            },
        }
    }
    messages
}

/// The transcript's rendering of one tool result for the wire.
fn tool_result_text(turn: &assistant_turns::AssistantTurn) -> String {
    let tool = turn.tool_name.as_deref().unwrap_or("search");
    format!("[{tool}] {content}", content = turn.content)
}

/// The vendored sandbox skills every session loads — prompt material the
/// gateway renders into the system context, never a runtime dependency.
fn session_skills() -> Vec<gateway::SkillId> {
    gateway::catalog().iter().map(|s| s.id.clone()).collect()
}

/// The two read-only search tools, as the model may call them.
fn search_tool_specs() -> Vec<gateway::ToolSpec> {
    [
        mcp::tools::search_case_law::descriptor(),
        mcp::tools::search_legal_aid::descriptor(),
    ]
    .into_iter()
    .map(|d| gateway::ToolSpec {
        name: d
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        description: d
            .get("description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        input_schema: d
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({"type": "object"})),
    })
    .collect()
}

/// Execute one read-only search tool call. The allowlist is the two catalog
/// entries the sandbox tools ship; anything else the model asks for is an
/// unavailable result, never a session failure — and an outage inside a
/// configured provider degrades the same way, per the spec's failure paths.
async fn execute_tool(search: &SearchBackends, call: &gateway::ToolCall) -> String {
    let started = std::time::Instant::now();
    let outcome = match call.name.as_str() {
        "search_case_law" => mcp::tools::search_case_law::call(Some(search), &call.input).await,
        "search_legal_aid" => mcp::tools::search_legal_aid::call(Some(search), &call.input).await,
        _ => Err(mcp::tools::ToolError::InvalidArguments(format!(
            "no sandbox tool named {}",
            call.name
        ))),
    };
    // Audit: which tool, how it ended, how long — never the query content.
    tracing::info!(
        tool = %call.name,
        outcome = if outcome.is_ok() { "ok" } else { "error" },
        duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "sandbox tool executed"
    );
    match outcome {
        Ok(payload) => serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string()),
        Err(e) => format!(
            r#"{{"error":{}}}"#,
            serde_json::to_string(&e.to_string()).unwrap_or_else(|_| "\"tool failed\"".to_string())
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{access_for, is_nautilus_offering, request_messages, NAUTILUS_OFFERINGS};
    use crate::nautilus::SandboxAccess;
    use gateway::Role;
    use store::assistant_turns::{AssistantTurn, TurnKind};
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

    fn turn(kind: TurnKind, content: &str, tool: Option<&str>) -> AssistantTurn {
        AssistantTurn {
            id: uuid::Uuid::now_v7(),
            person_id: uuid::Uuid::now_v7(),
            kind,
            content: content.to_string(),
            tool_name: tool.map(str::to_string),
            input_tokens: None,
            output_tokens: None,
            inserted_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    /// A plain exchange is one user turn: the prompt, then the reply.
    #[test]
    fn a_prompt_and_a_reply_map_to_the_wire() {
        let turns = vec![
            turn(TurnKind::Prompt, "Is my lease assignable?", None),
            turn(TurnKind::Reply, "Generally, yes — with notice.", None),
            turn(TurnKind::Prompt, "What notice?", None),
        ];
        let messages = request_messages(&turns);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[0].content, "Is my lease assignable?");
        assert_eq!(messages[1].role, Role::Assistant);
        assert_eq!(messages[2].content, "What notice?");
    }

    /// A tool round folds its result into the trailing user turn, keeping
    /// the wire strictly alternating; the call turn itself contributes
    /// nothing.
    #[test]
    fn a_tool_result_folds_into_the_trailing_user_turn() {
        let turns = vec![
            turn(TurnKind::Prompt, "Any cases on this?", None),
            turn(
                TurnKind::ToolCall,
                r#"{"query":"lease assignment"}"#,
                Some("search_case_law"),
            ),
            turn(
                TurnKind::ToolResult,
                r#"{"cases":[{"name":"Rarig v. Testing"}]}"#,
                Some("search_case_law"),
            ),
        ];
        let messages = request_messages(&turns);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::User);
        assert!(messages[0].content.starts_with("Any cases on this?"));
        assert!(messages[0]
            .content
            .contains("[search_case_law] {\"cases\":"));
    }

    /// A search-unavailable result payload folds the same way: the model
    /// hears the degradation as the tool's answer, not an exception.
    #[test]
    fn an_unavailable_search_result_still_reaches_the_model() {
        let turns = vec![
            turn(TurnKind::Prompt, "Search for this", None),
            turn(
                TurnKind::ToolResult,
                r#"{"error":"search is unavailable"}"#,
                Some("search_case_law"),
            ),
        ];
        let messages = request_messages(&turns);
        assert_eq!(messages.len(), 1);
        assert!(messages[0].content.contains("\"search is unavailable\""));
    }
}
