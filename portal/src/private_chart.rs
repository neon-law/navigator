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
    let viewer = store::access::matter_viewer(
        &state.application_store,
        session.person_id,
        session.role,
        project_id,
    )
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
