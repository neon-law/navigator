//! The authenticated visitor's service start door.
//!
//! A service card names the only services `/start/{service_id}` can open, and
//! a published notation supplies its template. The Nevada formation notation starts in the signed-in
//! founder's personal Project. Firm service cards use the existing lawyer intake and its disclosures.

use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::StatusCode;
use axum::middleware::{from_fn, from_fn_with_state, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Extension, Form, RequestExt, Router};
use dioxus_server::{render_handler, FullstackState, ServeConfig};
use serde::Deserialize;
use uuid::Uuid;

use crate::csrf::CsrfMode;
use crate::retainer_walk::{open_intake_matter, IntakeClient, OpenIntakeError};
use crate::{gated, AppState, SessionData};

/// The brand-mounted start route.
pub const START_PATH: &str = "/start/{service_id}";
/// The notation-mounted start route a published notation's Workflow links to.
pub const NOTATION_START_PATH: &str = "/notations/{slug}/start";
const MAX_BODY_BYTES: usize = 8 * 1024;
const ON_CALL_ENV: &str = "NAVIGATOR_ON_CALL_LAWYER_EMAIL";

/// The client intake route reuses start copy after the POST/redirect handoff.
/// It is parsed from the same locale catalog as the host route.
pub(crate) fn bundled_start_copy() -> Option<views::locales::services::StartDoorCopy> {
    static COPY: std::sync::OnceLock<Option<views::locales::services::StartDoorCopy>> =
        std::sync::OnceLock::new();
    COPY.get_or_init(|| {
        let yaml = include_str!("../../neon/locales/en/neon/services-catalog.yaml");
        views::locales::services::ServicesCatalog::parse(yaml)
            .ok()
            .map(|catalog| catalog.start)
    })
    .clone()
}

/// A published notation a signed-in client may start intake for.
#[derive(Clone, Debug)]
pub struct NotationOffer {
    /// The `/notations/{slug}` path segment.
    pub slug: String,
    pub title: String,
    pub template_code: String,
}

#[derive(Clone)]
struct DoorState {
    app: AppState,
    catalog: Option<views::locales::services::ServicesCatalog>,
    /// `Some` for the notation door, whose ids are notation slugs.
    notations: Option<std::sync::Arc<Vec<NotationOffer>>>,
}

/// What one door opens, from either a service card or a notation.
struct Offer {
    name: String,
    blurb: String,
    template: String,
    /// The door's own path: the form's action and the conversion's page.
    path: String,
    /// The funnel's `service_id`: the service id, or `notation:{slug}`.
    funnel_id: String,
}

impl Offer {
    fn personal_formation(&self, brand: views::brand::BrandKey) -> bool {
        brand == views::brand::BrandKey::Neon
            && self.funnel_id == "notation:nevada-llc-formation"
            && self.template == store::personal_projects::FORMATION
    }
}

impl DoorState {
    fn offer(&self, id: &str) -> Option<Offer> {
        if let Some(notations) = &self.notations {
            return notations.iter().find(|n| n.slug == id).map(|n| Offer {
                name: n.title.clone(),
                blurb: n.title.clone(),
                template: n.template_code.clone(),
                path: NOTATION_START_PATH.replace("{slug}", &n.slug),
                funnel_id: format!("notation:{}", n.slug),
            });
        }
        let service = self
            .catalog
            .as_ref()?
            .services
            .iter()
            .find(|s| s.id == id)?;
        Some(Offer {
            name: service.name.clone(),
            blurb: service.blurb.clone(),
            template: service.template.clone()?,
            path: START_PATH.replace("{service_id}", &service.id),
            funnel_id: service.id.clone(),
        })
    }
}

#[derive(Debug, Deserialize, Default)]
struct StartForm {
    #[serde(default)]
    seen_notation: Option<Uuid>,
    #[serde(default)]
    move_formation: Option<Uuid>,
    #[serde(default)]
    _csrf: String,
    /// Accepted but ignored. The route maps the service id to its template
    /// from the server-owned catalog.
    #[serde(default, rename = "template")]
    _template: Option<String>,
}

/// Build the service door beside the firm's public Dioxus pages.
pub fn routes(
    state: &AppState,
    catalog: Option<views::locales::services::ServicesCatalog>,
    notations: Vec<NotationOffer>,
) -> Router {
    let notation_state = DoorState {
        app: state.clone(),
        catalog: catalog.clone(),
        notations: Some(std::sync::Arc::new(notations)),
    };
    door_routes(
        state,
        START_PATH,
        DoorState {
            app: state.clone(),
            catalog,
            notations: None,
        },
    )
    .merge(door_routes(state, NOTATION_START_PATH, notation_state))
}

fn door_routes(state: &AppState, path: &'static str, door_state: DoorState) -> Router {
    let render = Router::<FullstackState>::new()
        .route(
            path,
            get(render_handler)
                .layer(from_fn(inject_public_context))
                .layer(from_fn_with_state(door_state.clone(), inject_start_content))
                .layer(from_fn(inject_start_csrf))
                .layer(from_fn(crate::dioxus_app::dioxus_document_head)),
        )
        .with_state(FullstackState::new(
            ServeConfig::new(),
            webapp::start_door::StartDoorEntry,
        ));
    let command = Router::new()
        .route(path, post(post_start))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .route_layer(from_fn_with_state(
            (state.sessions.clone(), CsrfMode::Strict),
            crate::csrf::require_csrf,
        ))
        .route_layer(from_fn_with_state(
            state.rate_limit.clone(),
            crate::rate_limit::enforce,
        ))
        .with_state(door_state);
    gated(state, render).merge(gated(state, command))
}

async fn inject_public_context(mut req: Request, next: Next) -> Response {
    let utility = crate::dioxus_app::public_utility_links(req.extensions().get::<SessionData>());
    req.extensions_mut()
        .insert(webapp::public_chrome::firm_public_chrome(utility));
    next.run(req).await
}

async fn inject_start_csrf(mut req: Request, next: Next) -> Response {
    let token = req
        .extensions()
        .get::<SessionData>()
        .map_or_else(String::new, |session| session.csrf_token.clone());
    req.extensions_mut()
        .insert(webapp::start_door::StartDoorCsrf(token));
    next.run(req).await
}

async fn inject_start_content(
    State(state): State<DoorState>,
    mut req: Request,
    next: Next,
) -> Response {
    let Ok(Path(service_id)) = req.extract_parts::<Path<String>>().await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(catalog) = state.catalog.as_ref() else {
        let path = START_PATH.replace("{service_id}", &service_id);
        insert_refusal(&mut req, service_id, path, None);
        return next.run(req).await;
    };
    let Some(offer) = state.offer(&service_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let brand = views::brand::brand_key();
    let mut existing = Vec::new();
    if offer.personal_formation(brand) {
        if let Some(person) = req
            .extensions()
            .get::<SessionData>()
            .filter(|session| session.role == store::persons::Role::Client)
            .and_then(|session| session.person_id)
        {
            let Ok(db) = store::postgres::PostgresDb::application().await else {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            };
            let Ok(rows) = store::personal_projects::formations(&db, person, brand.as_str()).await
            else {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            };
            existing = rows
                .into_iter()
                .map(|row| webapp::start_door::ExistingIntake {
                    can_move: row.project_code == store::personal_projects::personal_code(person),
                    href: row.intake_path(),
                    notation_id: row.id.to_string(),
                    project_name: row.label,
                })
                .collect();
        }
    }
    let personal = offer.personal_formation(brand);
    let can_start = (personal
        || configured_lawyer(&state.app, brand, &service_id)
            .await
            .is_some())
        && store::firms::firm_id_for_brand_key(&state.app.application_store, brand.as_str())
            .await
            .ok()
            .flatten()
            .is_some();
    req.extensions_mut()
        .insert(webapp::start_door::InjectedStartDoor(
            webapp::start_door::StartDoorContent {
                service_id,
                service_name: offer.name,
                action: offer.path,
                disclosure: if personal {
                    catalog.start.formation_disclosure.clone()
                } else {
                    catalog.start.disclosure.clone()
                },
                refusal: catalog.start.refusal.clone(),
                start_label: catalog.start.label.clone(),
                can_start,
                existing,
                resume_label: catalog.start.resume_formation.clone(),
                separate_label: catalog.start.separate_formation.clone(),
            },
        ));
    next.run(req).await
}

fn insert_refusal(
    req: &mut Request,
    service_id: String,
    action: String,
    catalog: Option<&views::locales::services::ServicesCatalog>,
) {
    let (disclosure, refusal, start_label) = catalog.map_or_else(
        || {
            (
                String::new(),
                bundled_start_copy()
                    .map(|copy| copy.refusal)
                    .unwrap_or_default(),
                String::new(),
            )
        },
        |catalog| {
            (
                catalog.start.disclosure.clone(),
                catalog.start.refusal.clone(),
                catalog.start.label.clone(),
            )
        },
    );
    req.extensions_mut()
        .insert(webapp::start_door::InjectedStartDoor(
            webapp::start_door::StartDoorContent {
                service_id,
                service_name: String::new(),
                action,
                disclosure,
                refusal,
                start_label,
                can_start: false,
                ..Default::default()
            },
        ));
}

/// The offer's terms as proposed questionnaire answers: the service's
/// name proposes the matter name (`project__engagement`) and its blurb
/// proposes the engagement scope (`custom_text__engagement_scope`) — the
/// two dotted placeholders the canonical retainer letter renders. The
/// writer refuses a term whose question the opened notation's
/// questionnaire does not declare, so a template without a state simply
/// gets no proposal for it.
fn offer_terms(offer: &Offer) -> Vec<(&'static str, String)> {
    vec![
        ("project__engagement", offer.name.clone()),
        ("custom_text__engagement_scope", offer.blurb.clone()),
    ]
}

async fn configured_lawyer(
    state: &AppState,
    brand: views::brand::BrandKey,
    service_id: &str,
) -> Option<Uuid> {
    let configured = state
        .on_call_lawyer_email
        .as_deref()
        .map(|email| (email, "configured"));
    let bootstrap_owner = state
        .bootstrap_owner_email
        .as_deref()
        .map(|email| (email, "bootstrap_owner"));
    let Some((email, on_call_source)) = configured.or(bootstrap_owner) else {
        tracing::error!(
            target: "audit",
            audit = true,
            service_id,
            brand = brand.as_str(),
            outcome = "configuration_missing",
            "{ON_CALL_ENV} and NAVIGATOR_BOOTSTRAP_OWNER_EMAIL are not configured"
        );
        return None;
    };
    tracing::info!(
        target: "audit",
        audit = true,
        service_id,
        brand = brand.as_str(),
        on_call_source,
        outcome = "configuration_selected",
        "start door lawyer source selected"
    );
    let person = match store::persons::find_by_email_ci(&state.application_store, email).await {
        Ok(person) => person,
        Err(error) => {
            tracing::error!(
                target: "audit",
                audit = true,
                service_id,
                brand = brand.as_str(),
                on_call_source,
                outcome = "configuration_lookup_failed",
                error = %error,
                "start door lawyer source could not resolve its Person"
            );
            return None;
        }
    };
    let Some(person) = person else {
        tracing::error!(
            target: "audit",
            audit = true,
            service_id,
            brand = brand.as_str(),
            on_call_source,
            outcome = "configuration_person_missing",
            "start door lawyer source does not name a Person"
        );
        return None;
    };
    if !person.role.is_lawyer_tier()
        || !store::persons::is_admitted(&state.application_store, person.id)
            .await
            .ok()
            .unwrap_or(false)
    {
        tracing::error!(
            target: "audit",
            audit = true,
            service_id,
            brand = brand.as_str(),
            on_call_source,
            outcome = "configuration_person_not_admitted_lawyer",
            "start door lawyer source does not name an admitted lawyer-tier Person"
        );
        return None;
    }
    Some(person.id)
}

async fn start_personal_formation(
    app: &AppState,
    owner: Uuid,
    brand: &str,
    form: &StartForm,
    path: &str,
) -> anyhow::Result<Response> {
    let pg = store::postgres::PostgresDb::application().await?;
    if let Some(id) = form.move_formation {
        return Ok(
            match store::personal_projects::separate_formation(&pg, owner, brand, id).await? {
                Some(url) => Redirect::to(&url).into_response(),
                None => StatusCode::NOT_FOUND.into_response(),
            },
        );
    }
    let template = store::templates::resolve(
        &app.application_store,
        None,
        store::personal_projects::FORMATION,
    )
    .await?
    .ok_or_else(|| anyhow::anyhow!("shared formation template missing"))?;
    let snapshot = workflows::notation_session::questionnaire_snapshot_for_template(
        &app.application_store,
        Some(&app.storage),
        &template,
    )
    .await?;
    let result = store::personal_projects::start_formation(
        &pg,
        owner,
        brand,
        template.id,
        snapshot,
        form.seen_notation,
    )
    .await?;
    Ok(match result {
        store::personal_projects::Start::Intake(url) => Redirect::to(&url).into_response(),
        store::personal_projects::Start::Choose => Redirect::to(path).into_response(),
    })
}

#[allow(clippy::too_many_lines)]
async fn post_start(
    State(state): State<DoorState>,
    Extension(session): Extension<SessionData>,
    Path(path_id): Path<String>,
    headers: axum::http::HeaderMap,
    Form(body): Form<StartForm>,
) -> Response {
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(offer) = state.offer(&path_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let service_id = offer.funnel_id.clone();
    let brand = views::brand::brand_key();
    if session.role.is_lawyer_tier() {
        return Redirect::to("/app/lawyer/retainers/new").into_response();
    }
    if session.role != store::persons::Role::Client {
        audit(&session, None, None, &service_id, brand, "rejected_role");
        return (StatusCode::FORBIDDEN, catalog.start.refusal.clone()).into_response();
    }
    let Some(person_id) = session.person_id else {
        audit(&session, None, None, &service_id, brand, "rejected_person");
        return (StatusCode::FORBIDDEN, catalog.start.refusal.clone()).into_response();
    };
    if offer.personal_formation(brand) {
        return start_personal_formation(&state.app, person_id, brand.as_str(), &body, &offer.path)
            .await
            .unwrap_or_else(|error| {
                tracing::error!(%error, "self-serve formation unavailable");
                StatusCode::SERVICE_UNAVAILABLE.into_response()
            });
    }
    let Some(lawyer_dri_id) = configured_lawyer(&state.app, brand, &service_id).await else {
        audit(
            &session,
            None,
            None,
            &service_id,
            brand,
            "rejected_configuration",
        );
        return (StatusCode::OK, catalog.start.refusal.clone()).into_response();
    };
    let repo = repos::RepoStore::from_env().ok();
    let Some(template) = start_template(&state.app, brand, &offer.template, repo.as_ref()).await
    else {
        audit(
            &session,
            None,
            None,
            &service_id,
            brand,
            "rejected_template",
        );
        return (StatusCode::OK, catalog.start.refusal.clone()).into_response();
    };
    let snapshot = match workflows::notation_session::questionnaire_snapshot_for_template(
        &state.app.application_store,
        Some(&state.app.storage),
        &template,
    )
    .await
    {
        Ok(snapshot) => snapshot,
        Err(error) => {
            tracing::error!(error = %error, "start door: questionnaire snapshot failed");
            audit(
                &session,
                None,
                None,
                &service_id,
                brand,
                "rejected_snapshot",
            );
            return (StatusCode::OK, catalog.start.refusal.clone()).into_response();
        }
    };
    match open_intake_matter(
        &state.app.application_store,
        &template,
        snapshot,
        IntakeClient::Existing(person_id),
        lawyer_dri_id,
        brand,
        "pitch",
        false,
    )
    .await
    {
        Ok(opened) => {
            audit(
                &session,
                Some(opened.project.id),
                Some(opened.rows.notation_id),
                &service_id,
                brand,
                "accepted",
            );
            // The offer's terms land as extracted proposals the client
            // confirms in intake — never silently accepted. A term whose
            // question the opened notation's questionnaire does not declare
            // is simply not proposed (the writer refuses it), and a failed
            // proposal never blocks the acceptance itself: the lawyer is
            // asked the question instead.
            let mut proposed = 0usize;
            for (state_name, term) in offer_terms(&offer) {
                match workflows::notation_session::record_extracted_answer(
                    &state.app.application_store,
                    Some(&state.app.storage),
                    opened.rows.notation_id,
                    state_name,
                    &term,
                )
                .await
                {
                    Ok(true) => proposed += 1,
                    Ok(false) => {}
                    Err(error) => {
                        tracing::error!(
                            error = %error,
                            state_name,
                            "start door: offer term not proposed"
                        );
                    }
                }
            }
            tracing::info!(
                target: "audit",
                audit = true,
                service_id,
                proposed_terms = proposed,
                "start door: offer terms proposed as extracted answers"
            );
            let person_id = person_id.to_string();
            let project_id = opened.project.id.to_string();
            let notation_id = opened.rows.notation_id.to_string();
            telemetry::record_funnel_event(telemetry::FunnelEvent::OfferAccepted {
                person_id: &person_id,
                project_id: &project_id,
                notation_id: &notation_id,
                service_id: &service_id,
                brand: brand.as_str(),
            });
            state
                .app
                .plausible_conversions
                .record(
                    brand,
                    crate::plausible::ConversionGoal::IntakeStarted,
                    &offer.path,
                    crate::rate_limit::client_ip_from_headers(&headers),
                    headers
                        .get(axum::http::header::USER_AGENT)
                        .and_then(|value| value.to_str().ok()),
                )
                .await;
            Redirect::to(&format!(
                "/app/projects/{}/intake/{}?started=1",
                opened.project.code, opened.rows.notation_id
            ))
            .into_response()
        }
        Err(OpenIntakeError::Conflict) => {
            audit(
                &session,
                None,
                None,
                &service_id,
                brand,
                "rejected_conflict",
            );
            (StatusCode::OK, catalog.start.refusal.clone()).into_response()
        }
        Err(OpenIntakeError::BrandNotWorn) => {
            audit(&session, None, None, &service_id, brand, "rejected_brand");
            (StatusCode::OK, catalog.start.refusal.clone()).into_response()
        }
        Err(OpenIntakeError::Internal) => {
            audit(
                &session,
                None,
                None,
                &service_id,
                brand,
                "rejected_internal",
            );
            (StatusCode::OK, catalog.start.refusal.clone()).into_response()
        }
    }
}

/// The Template a client start opens on: the Firm Library `retainer` of the
/// Firm wearing this brand when its library names one, otherwise the
/// service's mapped template — the Firm's own Library version of that code
/// before the shared one. `None` when neither resolves.
async fn start_template(
    app: &AppState,
    brand: views::brand::BrandKey,
    service_template: &str,
    repo: Option<&repos::RepoStore>,
) -> Option<store::templates::Template> {
    let firm_id = store::firms::firm_id_for_brand_key(&app.application_store, brand.as_str())
        .await
        .ok()
        .flatten();
    let Some(firm_id) = firm_id else {
        return store::templates::resolve(&app.application_store, None, service_template)
            .await
            .ok()
            .flatten();
    };
    let library = match repo {
        Some(repo) => {
            store::firm_library::sync_or_default(
                &app.application_store,
                &app.storage,
                repo,
                firm_id,
            )
            .await
        }
        None => None,
    };
    if let Some(retainer) = library.and_then(|synced| synced.retainer) {
        return Some(retainer);
    }
    store::templates::resolve_for_firm(&app.application_store, firm_id, service_template)
        .await
        .ok()
        .flatten()
}

fn audit(
    session: &SessionData,
    project_id: Option<Uuid>,
    notation_id: Option<Uuid>,
    service_id: &str,
    brand: views::brand::BrandKey,
    outcome: &str,
) {
    match (project_id, notation_id) {
        (Some(project_id), Some(notation_id)) => tracing::info!(
            target: "audit", audit = true, person_id = ?session.person_id,
            %project_id, %notation_id, service_id, brand = brand.as_str(), outcome,
            "start door"
        ),
        (Some(project_id), None) => tracing::info!(
            target: "audit", audit = true, person_id = ?session.person_id,
            %project_id, service_id, brand = brand.as_str(), outcome, "start door"
        ),
        (None, Some(notation_id)) => tracing::info!(
            target: "audit", audit = true, person_id = ?session.person_id,
            %notation_id, service_id, brand = brand.as_str(), outcome, "start door"
        ),
        (None, None) => tracing::info!(
            target: "audit", audit = true, person_id = ?session.person_id,
            service_id, brand = brand.as_str(), outcome, "start door"
        ),
    }
}

#[cfg(test)]
#[path = "start_door_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "start_door_native_tests.rs"]
mod native_tests;
