//! Private, request-local Astrology Chart calculation.
//!
//! Consent authorizes this response only. Neither original input nor results enter a
//! shared cache, a standing store, a recovery archive, or a public map adapter.

use std::num::{NonZeroU32, NonZeroUsize};
use std::sync::Arc;

use axum::extract::{rejection::JsonRejection, Path, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use chrono::{NaiveDate, NaiveDateTime};
use ephemeris::{
    AstrologyChartProvider, AstrologyChartRequest, BirthTime, ChartError, HouseSystem,
    LocalBirthTime,
};
use serde::Deserialize;
use uuid::Uuid;
use webapp::astrology_chart::calculated::CalculatedChart;

use crate::api::{ApiState, AuthedSession};

const PATH_SUFFIX: &str = "/astrology-chart";
pub(crate) const MAX_BODY_BYTES: usize = 16 * 1024;

/// Immutable verified sources may be shared; personal calculation results may not.
trait CalculateChart: Send + Sync {
    fn calculate(&self, request: &AstrologyChartRequest) -> Result<CalculatedChart, ChartError>;
}

impl CalculateChart for AstrologyChartProvider {
    fn calculate(&self, request: &AstrologyChartRequest) -> Result<CalculatedChart, ChartError> {
        let result = self.calculate_uncached(request)?;
        CalculatedChart::from_response(&result)
    }
}

#[derive(Clone)]
pub(crate) struct ChartCalculator(Option<Arc<dyn CalculateChart>>);

impl ChartCalculator {
    /// Loading is opt-in through the existing local kernel-directory setting.
    /// A missing or unverified source keeps this calculation door unavailable.
    pub(crate) fn from_env() -> Self {
        if std::env::var_os(ephemeris::DATA_DIR_ENV).is_none() {
            return Self(None);
        }
        let loaded = (|| {
            let manifest = serde_json::from_str(include_str!("../../ephemeris/manifest.json"))?;
            let store = ephemeris::FilesystemStore::from_env()?;
            let provider = AstrologyChartProvider::load(&store, &manifest, NonZeroUsize::MIN)?;
            Ok::<_, Box<dyn std::error::Error>>(provider)
        })();
        if let Ok(provider) = loaded {
            Self(Some(Arc::new(provider)))
        } else {
            tracing::warn!(
                "private chart calculation unavailable: verified local sources required"
            );
            Self(None)
        }
    }
}

/// Keep an explicitly supplied in-process calculator when composing the router.
pub(crate) async fn inject_calculator(
    State(calculator): State<ChartCalculator>,
    mut request: Request,
    next: Next,
) -> Response {
    if request.extensions().get::<ChartCalculator>().is_none() {
        request.extensions_mut().insert(calculator);
    }
    next.run(request).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChartInput {
    consent: bool,
    purpose: Purpose,
    birth_time: BirthInput,
    longitude_deg: f64,
    latitude_deg: f64,
    bodies: Vec<String>,
    #[serde(default)]
    houses: Houses,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Purpose {
    PersonalReflection,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Houses {
    #[default]
    None,
    Equal,
    WholeSign,
}

#[derive(Deserialize)]
#[serde(tag = "precision", rename_all = "snake_case", deny_unknown_fields)]
enum BirthInput {
    Exact {
        local: NaiveDateTime,
        zone: String,
        offset_seconds: Option<i32>,
    },
    Approximate {
        local: NaiveDateTime,
        zone: String,
        offset_seconds: Option<i32>,
        uncertainty_seconds: NonZeroU32,
    },
    Unknown {
        date: NaiveDate,
        zone: String,
    },
}

impl ChartInput {
    fn request(self) -> Result<AstrologyChartRequest, ()> {
        let birth_time = match self.birth_time {
            BirthInput::Exact {
                local,
                zone,
                offset_seconds,
            } => BirthTime::Exact(LocalBirthTime {
                local,
                zone: zone.parse().map_err(|_| ())?,
                offset_seconds,
            }),
            BirthInput::Approximate {
                local,
                zone,
                offset_seconds,
                uncertainty_seconds,
            } => BirthTime::Approximate {
                local: LocalBirthTime {
                    local,
                    zone: zone.parse().map_err(|_| ())?,
                    offset_seconds,
                },
                uncertainty_seconds,
            },
            BirthInput::Unknown { date, zone } => BirthTime::Unknown {
                date,
                zone: zone.parse().map_err(|_| ())?,
            },
        };
        let mut request =
            AstrologyChartRequest::new(birth_time, self.longitude_deg, self.latitude_deg);
        request.bodies = self
            .bodies
            .iter()
            .map(|name| {
                ephemeris::lines::LINE_BODIES
                    .into_iter()
                    .find(|body| body.name() == name)
                    .ok_or(())
            })
            .collect::<Result<_, _>>()?;
        request.settings.houses = match self.houses {
            Houses::None => HouseSystem::None,
            Houses::Equal => HouseSystem::Equal,
            Houses::WholeSign => HouseSystem::WholeSign,
        };
        request.resolve().map_err(|_| ())?;
        Ok(request)
    }
}

fn refusal(status: StatusCode, message: &'static str) -> Response {
    (status, message).into_response()
}

pub(crate) async fn calculate(
    State(state): State<ApiState>,
    AuthedSession(session): AuthedSession,
    Path(project_id): Path<Uuid>,
    calculator: Option<Extension<ChartCalculator>>,
    body: Result<Json<ChartInput>, JsonRejection>,
) -> Response {
    // A client view is read-only even when its original actor has a firm-side role.
    if session.viewing_as_dri.is_some() {
        return refusal(StatusCode::FORBIDDEN, "read-only view");
    }
    let viewer =
        store::access::matter_viewer(&state.surreal, session.person_id, session.role, project_id)
            .await;
    if !matches!(
        viewer,
        Ok(Some(
            store::access::MatterViewer::Lawyer
                | store::access::MatterViewer::LawyerDri
                | store::access::MatterViewer::Client
                | store::access::MatterViewer::ClientDri
        ))
    ) {
        return refusal(StatusCode::NOT_FOUND, "matter not found");
    }
    let input = match body {
        Ok(Json(input)) => input,
        Err(error) if error.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return refusal(
                StatusCode::PAYLOAD_TOO_LARGE,
                "calculation input is too large",
            );
        }
        Err(_) => return refusal(StatusCode::BAD_REQUEST, "invalid calculation input"),
    };
    // The closed purpose enum cannot repurpose consent for storage or publication.
    let Purpose::PersonalReflection = &input.purpose;
    if !input.consent {
        return refusal(
            StatusCode::UNPROCESSABLE_ENTITY,
            "explicit calculation consent is required",
        );
    }
    let Ok(request) = input.request() else {
        return refusal(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid birth time, location, or calculation selection",
        );
    };
    let Some(Extension(ChartCalculator(Some(calculator)))) = calculator else {
        return refusal(
            StatusCode::SERVICE_UNAVAILABLE,
            "verified calculation sources are unavailable",
        );
    };
    // At most twelve bodies use already loaded source bytes, with no I/O or queued work.
    // All retained personal values belong to this calculation and response.
    match calculator.calculate(&request) {
        Ok(chart) => Json(chart).into_response(),
        Err(_) => refusal(
            StatusCode::UNPROCESSABLE_ENTITY,
            "calculation is unavailable for this input",
        ),
    }
}

/// Wrap the session/policy/CSRF stack so early refusals are private too.
pub(crate) async fn no_store(request: Request, next: Next) -> Response {
    let is_chart = request.uri().path().starts_with("/app/api/projects/")
        && request.uri().path().ends_with(PATH_SUFFIX);
    let mut response = if is_chart && request.uri().query().is_some() {
        refusal(
            StatusCode::BAD_REQUEST,
            "submit calculation inputs in the request body",
        )
    } else {
        next.run(request).await
    };
    if is_chart {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-store"),
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{DocumentScope, DriView, SessionData, SessionScope};
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use store::persons::Role;
    use tower::ServiceExt;

    #[derive(Debug, PartialEq, Eq)]
    struct Precision {
        name: &'static str,
        offset_seconds: Option<i32>,
        uncertainty_seconds: Option<u32>,
        has_reference: bool,
        can_compute_angles: bool,
    }

    struct Calculator {
        calls: AtomicUsize,
        fails: AtomicBool,
        precisions: std::sync::Mutex<Vec<Precision>>,
    }
    impl CalculateChart for Calculator {
        fn calculate(
            &self,
            request: &AstrologyChartRequest,
        ) -> Result<CalculatedChart, ChartError> {
            let time = request.resolve()?;
            let (name, offset_seconds, uncertainty_seconds) = match &request.birth_time {
                BirthTime::Exact(local) => ("exact", local.offset_seconds, None),
                BirthTime::Approximate {
                    local,
                    uncertainty_seconds,
                } => (
                    "approximate",
                    local.offset_seconds,
                    Some(uncertainty_seconds.get()),
                ),
                BirthTime::Unknown { .. } => ("unknown", None, None),
            };
            self.precisions.lock().unwrap().push(Precision {
                name,
                offset_seconds,
                uncertainty_seconds,
                has_reference: time.reference().is_some(),
                can_compute_angles: time.can_compute_angles(),
            });
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fails.load(Ordering::SeqCst) {
                return Err(ChartError::CalculationFailed);
            }
            // The transport test double returns only the public synthetic reference.
            webapp::astrology_chart::calculated::specimen_chart()
                .map_err(|_| ChartError::SerializationFailed)
        }
    }

    struct Fixture {
        router: axum::Router,
        project_id: Uuid,
        session: SessionData,
        sessions: crate::session::SessionStore,
        calculator: Arc<Calculator>,
    }

    async fn fixture(role: Role, participates: bool) -> Fixture {
        let db = store::surreal::test_support::mem().await;
        let project_id = store::test_support::seed_project_surreal(&db, "synthetic-chart").await;
        let actor = store::persons::create(
            &db,
            &store::persons::NewPerson::with_role(
                "Synthetic Chart Participant",
                "chart@example.com",
                role,
            ),
        )
        .await
        .unwrap();
        if participates {
            store::projects::add_participation(
                &db,
                project_id,
                actor.id,
                store::projects::participation_for_role(role),
            )
            .await
            .unwrap();
        }
        if participates && role == Role::Clerk {
            let supervisor = store::test_support::dri_person(&db).await;
            store::persons::set_role(&db, supervisor, Role::Lawyer)
                .await
                .unwrap();
            store::projects::designate_dri_in_surreal(
                &db,
                project_id,
                supervisor,
                store::projects::DriSide::Lawyer,
            )
            .await
            .unwrap();
            assert_eq!(
                store::access::matter_viewer(&db, Some(actor.id), role, project_id)
                    .await
                    .unwrap(),
                Some(store::access::MatterViewer::Clerk)
            );
        }
        let mut session = SessionData::fresh("synthetic-chart", role);
        session.person_id = Some(actor.id);
        let mut app = crate::test_support::app_state(db).await;
        app.policy = crate::policy::PolicyClient::embedded().unwrap();
        let sessions = app.sessions.clone();
        let calculator = Arc::new(Calculator {
            calls: AtomicUsize::new(0),
            fails: AtomicBool::new(false),
            precisions: std::sync::Mutex::new(Vec::new()),
        });
        let router = crate::router(app).layer(axum::middleware::from_fn_with_state(
            ChartCalculator(Some(calculator.clone())),
            inject_calculator,
        ));
        Fixture {
            router,
            project_id,
            session,
            sessions,
            calculator,
        }
    }

    fn input() -> serde_json::Value {
        serde_json::json!({
            "consent":true, "purpose":"personal_reflection",
            "birth_time":{"precision":"exact", "local":"2007-01-02T12:34:56", "zone":"UTC"},
            "longitude_deg":12.5,"latitude_deg":34.5,"bodies":["sun"],"houses":"equal"
        })
    }

    enum CookieRequest<'a> {
        Absent,
        Present(Option<&'a str>),
    }

    impl Fixture {
        async fn post(&self, input: serde_json::Value) -> Response {
            self.send(
                "POST",
                &format!("/app/api/projects/{}/astrology-chart", self.project_id),
                serde_json::to_string(&input).unwrap(),
                Some(self.session.clone()),
                CookieRequest::Absent,
            )
            .await
        }

        async fn send(
            &self,
            method: &str,
            path: &str,
            body: String,
            session: Option<SessionData>,
            csrf: CookieRequest<'_>,
        ) -> Response {
            let mut request = Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json");
            if let Some(ref session) = session {
                request = request.extension(session.clone());
                if let CookieRequest::Present(token) = csrf {
                    request = request.header(
                        "cookie",
                        format!("navigator_session={}", self.sessions.encode(session)),
                    );
                    if let Some(token) = token {
                        request = request.header("x-csrf-token", token);
                    }
                }
            }
            let response = self
                .router
                .clone()
                .oneshot(request.body(Body::from(body)).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.headers()[header::CACHE_CONTROL],
                "private, no-store"
            );
            response
        }
    }

    #[tokio::test]
    async fn own_project_requires_explicit_consent_before_calculating() {
        let app = fixture(Role::Client, true).await;
        let mut body = input();
        body["consent"] = false.into();
        assert_eq!(
            app.post(body).await.status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        for field in ["consent", "purpose"] {
            let mut body = input();
            body.as_object_mut().unwrap().remove(field);
            assert_eq!(app.post(body).await.status(), StatusCode::BAD_REQUEST);
        }
        let mut body = input();
        body["purpose"] = "publish-private-marker".into();
        let response = app.post(body).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let text = to_bytes(response.into_body(), 4096).await.unwrap();
        assert!(!String::from_utf8_lossy(&text).contains("publish-private-marker"));
        assert_eq!(app.calculator.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn project_participation_is_required_even_for_owner_and_admin() {
        for role in [Role::Owner, Role::Admin, Role::Lawyer, Role::Client] {
            let app = fixture(role, false).await;
            assert_eq!(app.post(input()).await.status(), StatusCode::NOT_FOUND);
            let response = app
                .send(
                    "POST",
                    &format!("/app/api/projects/{}/astrology-chart", Uuid::now_v7()),
                    "private-invalid-body".into(),
                    Some(app.session.clone()),
                    CookieRequest::Absent,
                )
                .await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            assert_eq!(app.calculator.calls.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn participating_clients_and_lawyers_receive_only_the_private_chart_response() {
        for role in [Role::Owner, Role::Admin, Role::Lawyer, Role::Client] {
            let app = fixture(role, true).await;
            let response = app.post(input()).await;
            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), 65536).await.unwrap();
            let chart: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert!(chart.get("positions").is_some());
            assert!(chart.get("request").is_none());
            assert!(chart.get("birth_time").is_none());
            assert!(!String::from_utf8_lossy(&body).contains("2007-01-02"));
            assert_eq!(app.calculator.calls.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn clerk_read_only_and_scoped_sessions_cannot_calculate() {
        let app = fixture(Role::Clerk, true).await;
        assert_eq!(app.post(input()).await.status(), StatusCode::NOT_FOUND);
        assert_eq!(app.calculator.calls.load(Ordering::SeqCst), 0);
        let mut app = fixture(Role::Client, true).await;
        app.session.viewing_as_dri = Some(DriView {
            actor_sub: "synthetic-viewer".into(),
            actor_email: None,
            actor_person_id: None,
            target_name: "Synthetic Participant".into(),
            target_email: "viewer@example.com".into(),
        });
        assert_eq!(app.post(input()).await.status(), StatusCode::FORBIDDEN);
        app.session.viewing_as_dri = None;
        app.session.scope = Some(SessionScope::Document(DocumentScope {
            project_id: app.project_id,
            project_code: "synthetic-chart".into(),
        }));
        assert_eq!(app.post(input()).await.status(), StatusCode::FORBIDDEN);
        assert_eq!(app.calculator.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn cookie_calculation_requires_csrf_and_anonymous_requests_are_private() {
        let app = fixture(Role::Client, true).await;
        let path = format!("/app/api/projects/{}/astrology-chart", app.project_id);
        let body = serde_json::to_string(&input()).unwrap();
        for token in [None, Some("incorrect-token")] {
            assert_eq!(
                app.send(
                    "POST",
                    &path,
                    body.clone(),
                    Some(app.session.clone()),
                    CookieRequest::Present(token)
                )
                .await
                .status(),
                StatusCode::FORBIDDEN
            );
        }
        assert_eq!(
            app.send("POST", &path, body.clone(), None, CookieRequest::Absent)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(app.calculator.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            app.send(
                "POST",
                &path,
                body,
                Some(app.session.clone()),
                CookieRequest::Present(Some(&app.session.csrf_token))
            )
            .await
            .status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn invalid_birth_inputs_never_reach_calculation_or_echo_values() {
        let app = fixture(Role::Client, true).await;
        for invalid in [
            serde_json::json!({"precision":"exact","local":"2007-03-11T02:30:00","zone":"America/New_York"}),
            serde_json::json!({"precision":"exact","local":"2007-11-04T01:30:00","zone":"America/New_York"}),
            serde_json::json!({"precision":"exact","local":"2007-01-02T12:34:56","zone":"private-invalid-zone"}),
            serde_json::json!({"precision":"exact","local":"private-invalid-clock","zone":"UTC"}),
            serde_json::json!({"precision":"exact","local":"2050-01-02T12:34:56","zone":"UTC"}),
            serde_json::json!({"precision":"approximate","local":"2007-01-02T12:34:56","zone":"UTC","uncertainty_seconds":0}),
        ] {
            let mut body = input();
            body["birth_time"] = invalid;
            let response = app.post(body).await;
            assert!(matches!(
                response.status(),
                StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
            ));
            let text = to_bytes(response.into_body(), 4096).await.unwrap();
            let text = String::from_utf8_lossy(&text);
            assert!(
                !text.contains("2007")
                    && !text.contains("2050")
                    && !text.contains("private-invalid")
            );
        }
        let mut body = input();
        body["latitude_deg"] = 91.into();
        assert_eq!(
            app.post(body).await.status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        let mut body = input();
        body["bodies"] = serde_json::json!(["sun", "sun"]);
        assert_eq!(
            app.post(body).await.status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        let mut body = input();
        body["houses"] = "placidus".into();
        assert_eq!(app.post(body).await.status(), StatusCode::BAD_REQUEST);
        assert_eq!(app.calculator.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn url_inputs_unsupported_methods_and_oversized_bodies_are_private() {
        let app = fixture(Role::Client, true).await;
        let path = format!("/app/api/projects/{}/astrology-chart", app.project_id);
        let response = app
            .send(
                "POST",
                &format!("{path}?birth=private-marker"),
                "{}".into(),
                None,
                CookieRequest::Absent,
            )
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let text = to_bytes(response.into_body(), 4096).await.unwrap();
        assert!(!String::from_utf8_lossy(&text).contains("private-marker"));
        assert_eq!(
            app.send(
                "GET",
                &path,
                String::new(),
                Some(app.session.clone()),
                CookieRequest::Absent
            )
            .await
            .status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
        assert_eq!(
            app.send(
                "POST",
                &path,
                " ".repeat(MAX_BODY_BYTES + 1),
                Some(app.session.clone()),
                CookieRequest::Absent
            )
            .await
            .status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(app.calculator.calls.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn original_precision_offset_and_uncertainty_reach_the_calculator() {
        let app = fixture(Role::Client, true).await;
        for birth in [
            serde_json::json!({"precision":"exact","local":"2007-11-04T01:30:00","zone":"America/New_York","offset_seconds":-14400}),
            serde_json::json!({"precision":"approximate","local":"2007-01-02T12:34:56","zone":"UTC","uncertainty_seconds":600}),
            serde_json::json!({"precision":"unknown","date":"2007-01-02","zone":"America/New_York"}),
        ] {
            let mut body = input();
            body["birth_time"] = birth;
            assert_eq!(app.post(body).await.status(), StatusCode::OK);
        }
        assert_eq!(
            *app.calculator.precisions.lock().unwrap(),
            vec![
                Precision {
                    name: "exact",
                    offset_seconds: Some(-14400),
                    uncertainty_seconds: None,
                    has_reference: true,
                    can_compute_angles: true
                },
                Precision {
                    name: "approximate",
                    offset_seconds: None,
                    uncertainty_seconds: Some(600),
                    has_reference: true,
                    can_compute_angles: false
                },
                Precision {
                    name: "unknown",
                    offset_seconds: None,
                    uncertainty_seconds: None,
                    has_reference: false,
                    can_compute_angles: false
                },
            ]
        );
    }
    #[tokio::test]
    async fn unconfigured_sources_refuse_before_calling_calculator() {
        let mut app = fixture(Role::Client, true).await;
        app.router = app.router.layer(Extension(ChartCalculator(None)));
        assert_eq!(
            app.post(input()).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(app.calculator.calls.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn verified_jpl_response_suppresses_unknown_and_approximate_time_geometry() {
        if std::env::var_os(ephemeris::DATA_DIR_ENV).is_none() {
            eprintln!("skipping: pinned reference kernels are unavailable");
            return;
        }
        let calculator = ChartCalculator::from_env();
        assert!(
            calculator.0.is_some(),
            "configured kernels must verify against source pins"
        );
        let mut app = fixture(Role::Client, true).await;
        app.router = app.router.layer(Extension(calculator));
        for (birth, has_positions, has_houses) in [
            (
                serde_json::json!({"precision":"exact","local":"2007-01-02T12:34:56","zone":"UTC"}),
                true,
                true,
            ),
            (
                serde_json::json!({"precision":"approximate","local":"2007-01-02T12:34:56","zone":"UTC","uncertainty_seconds":600}),
                true,
                false,
            ),
            (
                serde_json::json!({"precision":"unknown","date":"2007-01-02","zone":"UTC"}),
                false,
                false,
            ),
        ] {
            let mut body = input();
            body["birth_time"] = birth;
            let response = app.post(body).await;
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
            let chart: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                !chart["positions"].as_array().unwrap().is_empty(),
                has_positions
            );
            assert_eq!(chart["house_cusps"].is_array(), has_houses);
            assert_eq!(
                !chart["metadata"]["provenance"]["time_scales"].is_null(),
                has_positions
            );
            assert!(chart.get("birth_time").is_none() && chart.get("request").is_none());
        }
        assert_eq!(app.calculator.calls.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn calculation_failure_has_a_private_static_response() {
        let app = fixture(Role::Client, true).await;
        app.calculator.fails.store(true, Ordering::SeqCst);
        let response = app.post(input()).await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
        assert_eq!(&bytes[..], b"calculation is unavailable for this input");
        assert_eq!(app.calculator.calls.load(Ordering::SeqCst), 1);
    }
}
