//! Provider-derived chart rendering and calculation provenance.

use super::{AstrologyChart, BodyPosition};
use crate::ephemeris_labels::{body_display_name, body_symbol, line_body_named};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum ClockPrecision {
    Exact,
    Approximate,
    Unknown,
}

/// Provider-derived rendering values and complete calculation provenance.
/// Personal calculation values deliberately have no debug formatting.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct CalculatedChart {
    positions: Vec<BodyPosition>,
    house_cusps: Option<[f64; 12]>,
    precision: ClockPrecision,
    metadata: serde_json::Value,
}

impl CalculatedChart {
    /// Adapt the provider's actual output without recomputing astronomy or retaining the request.
    pub fn from_response(
        response: &ephemeris::AstrologyChartResponse,
    ) -> Result<Self, ephemeris::ChartError> {
        #[derive(Serialize)]
        struct Metadata<'a> {
            provenance: &'a ephemeris::ChartProvenance,
            settings: &'a ephemeris::ChartSettings,
            body_states: &'a [ephemeris::BodyPosition],
        }
        response.request.resolve()?;
        let precision = match response.time.original() {
            ephemeris::BirthTime::Exact(_) => ClockPrecision::Exact,
            ephemeris::BirthTime::Approximate { .. } => ClockPrecision::Approximate,
            ephemeris::BirthTime::Unknown { .. } => ClockPrecision::Unknown,
        };
        let positions = if precision == ClockPrecision::Unknown {
            Vec::new()
        } else {
            response
                .positions
                .as_ref()
                .ok_or(ephemeris::ChartError::CalculationFailed)?
                .iter()
                .map(display_position)
                .collect::<Result<Vec<_>, _>>()?
        };
        let house_cusps = if precision == ClockPrecision::Exact {
            response
                .angles
                .as_ref()
                .and_then(|angles| angles.house_cusps_deg)
        } else {
            None
        };
        if house_cusps.is_some_and(|cusps| cusps.iter().any(|cusp| !cusp.is_finite())) {
            return Err(ephemeris::ChartError::InvalidGeometry);
        }
        let metadata = serde_json::to_value(Metadata {
            provenance: &response.provenance,
            settings: &response.request.settings,
            body_states: if precision == ClockPrecision::Unknown {
                &[]
            } else {
                response.positions.as_deref().unwrap_or(&[])
            },
        })
        .map_err(|_| ephemeris::ChartError::SerializationFailed)?;
        Ok(Self {
            positions,
            house_cusps,
            precision,
            metadata,
        })
    }

    /// Calculation identity/settings for application-side provenance; never a basemap payload.
    #[must_use]
    pub fn metadata(&self) -> &serde_json::Value {
        &self.metadata
    }

    fn detail(&self, pointer: &str) -> String {
        self.metadata
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Unavailable")
            .to_owned()
    }
}

fn display_position(
    position: &ephemeris::BodyPosition,
) -> Result<BodyPosition, ephemeris::ChartError> {
    let finite_angles_and_rates = [
        position.longitude_deg,
        position.latitude_deg,
        position.right_ascension_deg,
        position.declination_deg,
        position.longitude_speed_deg_per_day,
        position.latitude_speed_deg_per_day,
    ]
    .into_iter()
    .all(f64::is_finite);
    if !finite_angles_and_rates
        || !position.distance_km.is_none_or(f64::is_finite)
        || !position
            .distance_speed_km_per_day
            .is_none_or(f64::is_finite)
    {
        return Err(ephemeris::ChartError::InvalidGeometry);
    }
    let body = line_body_named(position.body).ok_or(ephemeris::ChartError::InvalidBodySelection)?;
    if body.spk_body().map(ephemeris::Body::naif_id) != position.source_id {
        return Err(ephemeris::ChartError::InvalidBodySelection);
    }
    let name = if body == ephemeris::lines::LineBody::MeanNode {
        "Mean North Node"
    } else {
        body_display_name(body)
    };
    Ok(BodyPosition {
        name: name.to_owned(),
        glyph: body_symbol(body).to_owned(),
        longitude: position.longitude_deg,
        longitude_speed_deg_per_day: Some(position.longitude_speed_deg_per_day),
    })
}

/// Render calculated positions with their precision policy and source identity.
#[component]
pub fn CalculatedAstrologyChart(chart: CalculatedChart) -> Element {
    let details = [
        ("Algorithm", chart.detail("/provenance/algorithm")),
        ("Version", chart.detail("/provenance/version")),
        (
            "Kernel frame",
            chart.detail("/provenance/manifest/reference_frame"),
        ),
        (
            "Output frame",
            match chart.detail("/settings/frame").as_str() {
                "true_of_date" => "True equator and ecliptic of date".to_owned(),
                "icrf_j2000" => "ICRF / J2000".to_owned(),
                _ => "Unavailable".to_owned(),
            },
        ),
        ("House convention", chart.detail("/settings/houses")),
        ("Lunar node", chart.detail("/settings/north_node")),
        (
            "Universal calendar",
            chart.detail("/provenance/time_scales/reference/calendar"),
        ),
        (
            "Time scale",
            chart.detail("/provenance/time_scales/reference/scale"),
        ),
    ];
    rsx! {
        section { class: "calculated-astrology-chart", "aria-label": "Calculated astrology chart",
            match chart.precision {
                ClockPrecision::Unknown => rsx! { p { role: "status", "Birth time is unknown. No positions, angles or houses are calculated." } },
                ClockPrecision::Approximate => rsx! { p { role: "status", "Approximate birth time: positions are nominal; angles and houses are unavailable." } },
                ClockPrecision::Exact => rsx! { p { "Calculated from the supplied clock and the recorded ephemeris settings." } },
            }
            if chart.precision != ClockPrecision::Unknown {
                AstrologyChart { positions: chart.positions, house_cusps: chart.house_cusps }
            }
            details {
                summary { "Calculation details" }
                dl {
                    for (label, value) in details {
                        dt { "{label}" }
                        dd { "{value}" }
                    }
                }
            }
        }
    }
}

/// The fixed public reference generated by `webapp/examples/chart_specimen.rs`.
/// No request-time ephemeris load or personal input is involved.
pub fn specimen_chart() -> Result<CalculatedChart, serde_json::Error> {
    serde_json::from_str(include_str!("../assets/chart-specimen.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dioxus::prelude::*;
    use ephemeris::{AstrologyChartRequest, AstrologyChartResponse, BirthTime, LocalBirthTime};

    fn response(approximate: bool, unknown: bool) -> AstrologyChartResponse {
        let local = LocalBirthTime {
            local: "2000-01-01T12:00:00"
                .parse()
                .expect("synthetic civil clock"),
            zone: "UTC".parse().expect("reference zone"),
            offset_seconds: None,
        };
        let birth_time = if unknown {
            BirthTime::Unknown {
                date: local.local.date(),
                zone: local.zone,
            }
        } else if approximate {
            BirthTime::Approximate {
                local,
                uncertainty_seconds: std::num::NonZeroU32::new(900).expect("positive uncertainty"),
            }
        } else {
            BirthTime::Exact(local)
        };
        let mut request = AstrologyChartRequest::new(birth_time, 0.0, 0.0);
        request.bodies = vec![ephemeris::lines::LineBody::Mercury];
        request.settings.houses = ephemeris::HouseSystem::Equal;
        let time = request.resolve().expect("valid synthetic request");
        let instant = time.instant().expect("reference clock converts");
        let provenance = ephemeris::ChartProvenance {
            algorithm: "synthetic-provider-adapter-test",
            version: "test",
            manifest: ephemeris::EphemerisManifest {
                version: ephemeris::MANIFEST_VERSION,
                time_scale_model: "synthetic clock".to_owned(),
                reference_frame: "synthetic frame".to_owned(),
                files: Vec::new(),
            },
            tzdb_version: time.tzdb_version(),
            delta_t_model: "synthetic",
            delta_t_sources: &[],
            sidereal_model: "synthetic",
            node_model: "mean",
            time_scales: instant.map(ephemeris::ChartInstant::time_scales),
        };
        AstrologyChartResponse {
            request,
            time,
            instant,
            positions: (!unknown).then(|| {
                vec![ephemeris::BodyPosition {
                    body: "mercury",
                    source_id: Some(199),
                    longitude_deg: 73.125,
                    latitude_deg: 0.5,
                    right_ascension_deg: 72.0,
                    declination_deg: 20.0,
                    distance_km: Some(100_000_000.0),
                    longitude_speed_deg_per_day: -0.25,
                    latitude_speed_deg_per_day: 0.0,
                    distance_speed_km_per_day: Some(1.0),
                }]
            }),
            // Even a malformed caller must not enable precise houses for uncertain clocks.
            angles: Some(ephemeris::ChartAngles {
                ascendant_deg: 7.25,
                midheaven_deg: 97.25,
                descendant_deg: 187.25,
                imum_coeli_deg: 277.25,
                house_cusps_deg: Some(std::array::from_fn(|index| {
                    7.25 + f64::from(u32::try_from(index).expect("twelve houses")) * 30.0
                })),
            }),
            provenance,
        }
    }

    fn render(response: &AstrologyChartResponse) -> String {
        let chart = CalculatedChart::from_response(response).expect("provider result adapts");
        let mut dom = VirtualDom::new_with_props(
            CalculatedAstrologyChart,
            CalculatedAstrologyChartProps { chart },
        );
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    #[test]
    fn calculated_wheel_uses_provider_positions_motion_houses_and_provenance() {
        let html = render(&response(false, false));
        assert!(
            html.contains("73.1° longitude"),
            "the rendered wheel must consume the supplied provider result"
        );
        assert!(html.contains("Retrograde") && html.contains("House cusp longitudes"));
        assert!(
            html.contains("synthetic-provider-adapter-test") && html.contains("synthetic frame")
        );
        assert!(
            !html.contains("Illustrative design"),
            "a computed result must carry its own calculation state"
        );
    }

    #[test]
    fn adapter_metadata_keeps_stable_body_ids_and_full_states() {
        let chart = CalculatedChart::from_response(&response(false, false))
            .expect("synthetic adapter input");
        let state = &chart.metadata()["body_states"][0];
        assert_eq!(state["body"], "mercury");
        assert_eq!(state["source_id"], 199);
        assert_eq!(state["right_ascension_deg"], 72.0);
    }

    #[test]
    fn adapter_refuses_non_finite_equatorial_metadata() {
        let mut input = response(false, false);
        input.positions.as_mut().expect("known positions")[0].declination_deg = f64::NAN;
        assert!(
            matches!(
                CalculatedChart::from_response(&input),
                Err(ephemeris::ChartError::InvalidGeometry)
            ),
            "non-finite provider data must not become null metadata in a calculated result"
        );
    }

    #[test]
    fn adapter_refuses_a_mislabeled_source_identity() {
        let mut input = response(false, false);
        input.positions.as_mut().expect("known positions")[0].source_id = Some(499);
        assert!(
            matches!(
                CalculatedChart::from_response(&input),
                Err(ephemeris::ChartError::InvalidBodySelection)
            ),
            "a display label must not conceal a different source body"
        );
    }

    #[test]
    fn fixed_reference_preserves_the_public_core_inventory_and_source_pins() {
        let chart = specimen_chart().expect("retained reference parses");
        let states = chart.metadata()["body_states"]
            .as_array()
            .expect("full reference states");
        assert_eq!(states.len(), 11);
        assert!(
            states.iter().all(|state| state["body"] != "chiron"),
            "the fixed public reference excludes minor-body output"
        );
        let files = chart.metadata()["provenance"]["manifest"]["files"]
            .as_array()
            .expect("verified sources");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0]["name"], "de440.bsp");
        assert_eq!(
            chart.metadata()["reference"]["instant_utc"],
            crate::astrocartography::SPECIMEN_INSTANT
        );
    }

    #[test]
    fn fixed_reference_recomputes_from_the_exact_provider_sources() {
        if std::env::var_os(ephemeris::DATA_DIR_ENV).is_none() {
            eprintln!("skipping: pinned reference kernels are unavailable");
            return;
        }
        let mut manifest: ephemeris::EphemerisManifest =
            serde_json::from_str(include_str!("../../../ephemeris/manifest.json"))
                .expect("source manifest");
        manifest.files.retain(|file| file.name == "de440.bsp");
        let provider = ephemeris::AstrologyChartProvider::load(
            &ephemeris::FilesystemStore::from_env().expect("reference sources"),
            &manifest,
            std::num::NonZeroUsize::new(1).expect("positive cache capacity"),
        )
        .expect("exact pins verify");
        let mut input = response(false, false).request;
        input.bodies = ephemeris::lines::LINE_BODIES
            .into_iter()
            .filter(|body| *body != ephemeris::lines::LineBody::Chiron)
            .collect();
        let computed = provider
            .calculate(&input)
            .expect("fixed reference calculates");
        let computed =
            CalculatedChart::from_response(&computed).expect("validated rendering model");
        let embedded = specimen_chart().expect("retained reference");
        assert_eq!(computed.positions.len(), embedded.positions.len());
        assert_eq!(
            computed.metadata()["provenance"]["manifest"],
            embedded.metadata()["provenance"]["manifest"]
        );
        assert_eq!(
            computed.metadata()["provenance"]["algorithm"],
            embedded.metadata()["provenance"]["algorithm"]
        );
        for (actual, reference) in computed.positions.iter().zip(&embedded.positions) {
            assert_eq!(actual.name, reference.name);
            assert!(
                (actual.longitude - reference.longitude).abs() < 1e-8,
                "fixed reference positions must track the exact provider"
            );
            assert!(
                actual
                    .longitude_speed_deg_per_day
                    .zip(reference.longitude_speed_deg_per_day)
                    .is_some_and(|(first, second)| (first - second).abs() < 1e-7),
                "fixed reference signed rates must track the exact provider"
            );
        }
        assert!(
            computed
                .house_cusps
                .zip(embedded.house_cusps)
                .is_some_and(|(first, second)| first
                    .into_iter()
                    .zip(second)
                    .all(|(first, second)| (first - second).abs() < 1e-8)),
            "the supplied Equal-house cusps must track the exact provider"
        );
    }

    #[test]
    fn display_distinguishes_apparent_output_from_the_raw_kernel_frame() {
        let html = render(&response(false, false));
        assert!(html.contains("Kernel frame") && html.contains("synthetic frame"));
        assert!(
            html.contains("Output frame") && html.contains("True equator and ecliptic of date"),
            "the raw SPK frame must not be presented as the apparent chart's frame"
        );
    }

    #[test]
    fn adapter_refuses_settings_that_would_mislabel_the_display() {
        let mut input = response(false, false);
        input.request.settings.zodiac = ephemeris::Zodiac::Sidereal {
            ayanamsa: ephemeris::Ayanamsa::Lahiri,
        };
        assert!(
            matches!(
                CalculatedChart::from_response(&input),
                Err(ephemeris::ChartError::UnsupportedSetting(_))
            ),
            "unsupported settings must not acquire a tropical chart label"
        );
    }

    #[test]
    fn unknown_clock_does_not_fabricate_positions_or_geometry() {
        let html = render(&response(false, true));
        assert!(html.contains("Birth time is unknown"));
        assert!(!html.contains("<svg") && !html.contains("House cusp longitudes"));
    }

    #[test]
    fn approximate_clock_keeps_nominal_positions_and_suppresses_houses() {
        let html = render(&response(true, false));
        assert!(html.contains("Approximate birth time") && html.contains("73.1° longitude"));
        assert!(
            !html.contains("House cusp longitudes"),
            "uncertain clocks cannot acquire precise houses through the display adapter"
        );
    }
}
