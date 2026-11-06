//! Canonical geographic geometry derived from one validated chart response.
//!
//! These request-local values contain calculation provenance, not the original
//! birth input. Coordinate-linear horizon edges carry a continuous angular bound.

mod interpolation;

use serde::Serialize;

use crate::lines::{horizon, sidereal, vertical, Angle, AngularLine};
use crate::{AstrologyChartResponse, ChartError, ChartProvenance, ChartSettings};

const VERTEX_TOLERANCE_DEG: f64 = 1e-9;

/// Request-local geographic lines and their calculation identity.
/// Personal geometry and epoch values deliberately have no debug formatting.
#[derive(Clone, Serialize)]
pub struct AstrocartographyGeometry {
    provenance: GeometryProvenance,
    lines: Vec<GeographicLine>,
}

/// A body/angle line with ordered, antimeridian-split longitude/latitude segments.
#[derive(Clone, Serialize)]
pub struct GeographicLine {
    body: &'static str,
    source_id: Option<i32>,
    angle: &'static str,
    segments: Vec<Vec<[f64; 2]>>,
}

/// Source pins, precise clock readings, settings, and geometric conventions.
#[derive(Clone, Serialize)]
pub struct GeometryProvenance {
    pub calculation: ChartProvenance,
    pub settings: ChartSettings,
    pub convention: GeometryConvention,
}

/// Geometry tolerances describe the solution, separately from source accuracy.
#[derive(Clone, Serialize)]
pub struct GeometryConvention {
    pub solver: &'static str,
    pub coordinates: &'static str,
    pub celestial_frame: &'static str,
    pub longitude: &'static str,
    pub horizon_altitude_deg: f64,
    pub refraction: &'static str,
    pub parallax: &'static str,
    pub orientation: &'static str,
    pub vertex_altitude_tolerance_deg: f64,
    pub meridian_hour_angle_tolerance_deg: f64,
    /// Maximum horizon angular-condition residual along straight WGS84 lon/lat edges.
    /// This is neither geographic displacement nor a projected/geodesic path bound.
    pub polyline_interpolation_error_bound_deg: Option<f64>,
    pub polyline_interpolation_metric: &'static str,
    pub polyline_interpolation_sine_roundoff_allowance: f64,
}

/// One numerical-table row from a canonical segment, in body/angle/segment/vertex order.
/// Segment and vertex indices are zero-based within their body/angle line.
#[derive(Clone, Serialize)]
pub struct GeometryPointRow {
    pub body: &'static str,
    pub angle: &'static str,
    pub segment_index: usize,
    pub vertex_index: usize,
    pub longitude_deg: f64,
    pub latitude_deg: f64,
}

impl AstrocartographyGeometry {
    /// Adapt selected provider states without source I/O or body-position recalculation.
    /// Unknown and approximate clocks have no exact-time geometry. No display
    /// projection or Mercator latitude clipping is applied to this physical output.
    ///
    /// # Errors
    /// Rejects invalid settings, incoherent clock/source metadata, mismatched body
    /// identities, and nonfinite or degenerate sky directions. Exact celestial-pole
    /// declinations have no defined diurnal orientation; geographic poles remain valid.
    pub fn from_response(response: &AstrologyChartResponse) -> Result<Option<Self>, ChartError> {
        let resolved = response.request.resolve()?;
        if !resolved.can_compute_angles() {
            return Ok(None);
        }
        let instant = response.instant.ok_or(ChartError::InvalidGeometry)?;
        let expected = resolved.instant()?.ok_or(ChartError::InvalidGeometry)?;
        if !response.time.can_compute_angles()
            || instant.epoch() != expected.epoch()
            || instant.ut1_julian_day().to_bits() != expected.ut1_julian_day().to_bits()
        {
            return Err(ChartError::InvalidGeometry);
        }
        let scales = response
            .provenance
            .time_scales
            .as_ref()
            .ok_or(ChartError::InvalidGeometry)?;
        if serde_json::to_value(scales).map_err(|_| ChartError::SerializationFailed)?
            != serde_json::to_value(instant.time_scales())
                .map_err(|_| ChartError::SerializationFailed)?
        {
            return Err(ChartError::InvalidGeometry);
        }
        response
            .provenance
            .manifest
            .validate()
            .map_err(|_| ChartError::InvalidGeometry)?;
        let positions = response
            .positions
            .as_ref()
            .ok_or(ChartError::InvalidGeometry)?;
        if positions.len() != response.request.bodies.len() {
            return Err(ChartError::InvalidBodySelection);
        }
        let gast = sidereal::gast_degrees_for(instant);
        let mut lines = Vec::with_capacity(positions.len() * 4);
        for (body, state) in response.request.bodies.iter().zip(positions) {
            if state.body != body.name()
                || state.source_id != body.spk_body().map(crate::Body::naif_id)
            {
                return Err(ChartError::InvalidBodySelection);
            }
            let (ra, dec) = (state.right_ascension_deg, state.declination_deg);
            if !ra.is_finite()
                || !(0.0..360.0).contains(&ra)
                || !dec.is_finite()
                || dec <= -90.0
                || dec >= 90.0
            {
                return Err(ChartError::InvalidGeometry);
            }
            let (mc, ic) = vertical::mc_ic_longitudes(ra, gast);
            let (asc, dsc) = horizon::asc_dsc_curves(*body, dec, mc);
            for line in [
                vertical::vertical_line(*body, Angle::Mc, mc),
                vertical::vertical_line(*body, Angle::Ic, ic),
                asc,
                dsc,
            ] {
                lines.push(GeographicLine::from_line(&line, state.source_id, dec, mc)?);
            }
        }
        Ok(Some(Self {
            provenance: GeometryProvenance {
                calculation: response.provenance.clone(),
                settings: response.request.settings.clone(),
                convention: GeometryConvention {
                    solver: "navigator-diurnal-true-horizon-v2",
                    coordinates: "WGS84 geodetic longitude, latitude in degrees",
                    celestial_frame: "apparent geocentric true equator/equinox of date (IAU 2006/2000B)",
                    longitude: "east-positive [-180, 180], with paired antimeridian edges",
                    horizon_altitude_deg: 0.0,
                    refraction: "none",
                    parallax: "none; geocentric directions",
                    orientation: "instantaneous diurnal rotation at fixed RA/declination; tangent endpoints bound rising/setting arcs",
                    vertex_altitude_tolerance_deg: VERTEX_TOLERANCE_DEG,
                    meridian_hour_angle_tolerance_deg: VERTEX_TOLERANCE_DEG,
                    polyline_interpolation_error_bound_deg: Some(interpolation::BUDGET_DEG),
                    polyline_interpolation_metric: "maximum true-horizon angular-condition residual along straight WGS84 longitude/latitude edges; not geographic displacement, projected or geodesic paths",
                    polyline_interpolation_sine_roundoff_allowance: interpolation::SINE_ROUNDOFF,
                },
            },
            lines,
        }))
    }

    #[must_use]
    pub fn lines(&self) -> &[GeographicLine] {
        &self.lines
    }

    #[must_use]
    pub fn provenance(&self) -> &GeometryProvenance {
        &self.provenance
    }

    /// Equivalent numerical rows use exactly the canonical segment coordinates.
    pub fn point_rows(&self) -> impl Iterator<Item = GeometryPointRow> + '_ {
        self.lines.iter().flat_map(|line| {
            line.segments
                .iter()
                .enumerate()
                .flat_map(move |(segment_index, segment)| {
                    segment
                        .iter()
                        .enumerate()
                        .map(move |(vertex_index, point)| GeometryPointRow {
                            body: line.body,
                            angle: line.angle,
                            segment_index,
                            vertex_index,
                            longitude_deg: point[0],
                            latitude_deg: point[1],
                        })
                })
        })
    }

    /// RFC 7946 geometry and private provenance, retaining canonical segment order.
    /// A basemap adapter receives only the geometry and public body labels, not
    /// this collection's precise private epoch/provenance member.
    pub fn geojson(&self) -> Result<String, ChartError> {
        let features: Vec<_> = self
            .lines
            .iter()
            .map(|line| Feature {
                kind: "Feature",
                properties: Properties {
                    body: line.body,
                    source_id: line.source_id,
                    angle: line.angle,
                },
                geometry: MultiLineString {
                    kind: "MultiLineString",
                    coordinates: &line.segments,
                },
            })
            .collect();
        serde_json::to_string(&FeatureCollection {
            kind: "FeatureCollection",
            provenance: &self.provenance,
            features,
        })
        .map_err(|_| ChartError::SerializationFailed)
    }
}

impl GeographicLine {
    fn from_line(
        line: &AngularLine,
        source_id: Option<i32>,
        declination: f64,
        meridian: f64,
    ) -> Result<Self, ChartError> {
        let segments = line
            .segments()
            .into_iter()
            .map(|segment| {
                segment
                    .into_iter()
                    .map(|point| {
                        if !point.0.is_finite()
                            || !(-180.0..=180.0).contains(&point.0)
                            || !point.1.is_finite()
                            || !(-90.0..=90.0).contains(&point.1)
                        {
                            return Err(ChartError::InvalidGeometry);
                        }
                        Ok([point.0, point.1])
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        let segments = if matches!(line.angle, Angle::Asc | Angle::Dsc) {
            interpolation::refine(segments, declination, meridian, line.angle)?
        } else {
            segments
        };
        Ok(Self {
            body: line.body.name(),
            source_id,
            angle: line.angle.name(),
            segments,
        })
    }

    #[must_use]
    pub fn body(&self) -> &'static str {
        self.body
    }
    #[must_use]
    pub fn source_id(&self) -> Option<i32> {
        self.source_id
    }
    #[must_use]
    pub fn angle(&self) -> &'static str {
        self.angle
    }
    #[must_use]
    pub fn segments(&self) -> &[Vec<[f64; 2]>] {
        &self.segments
    }
}

#[derive(Serialize)]
struct FeatureCollection<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    provenance: &'a GeometryProvenance,
    features: Vec<Feature<'a>>,
}
#[derive(Serialize)]
struct Feature<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    properties: Properties,
    geometry: MultiLineString<'a>,
}
#[derive(Serialize)]
struct Properties {
    body: &'static str,
    source_id: Option<i32>,
    angle: &'static str,
}
#[derive(Serialize)]
struct MultiLineString<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    coordinates: &'a [Vec<[f64; 2]>],
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lines::LineBody;
    use crate::{
        AstrologyChartRequest, BirthTime, BodyPosition, ChartProvenance, EphemerisFile,
        EphemerisManifest, LocalBirthTime,
    };

    fn response() -> AstrologyChartResponse {
        let mut request = AstrologyChartRequest::new(
            BirthTime::Exact(LocalBirthTime {
                local: "2000-01-01T12:00:00.123456789".parse().unwrap(),
                zone: chrono_tz::UTC,
                offset_seconds: None,
            }),
            12.0,
            34.0,
        );
        request.bodies = vec![LineBody::Mercury, LineBody::Sun];
        let time = request.resolve().unwrap();
        let instant = time.instant().unwrap().unwrap();
        let positions = request
            .bodies
            .iter()
            .zip([(320.0, 20.0), (110.0, -10.0)])
            .map(|(body, (ra, dec))| BodyPosition {
                body: body.name(),
                source_id: body.spk_body().map(crate::Body::naif_id),
                longitude_deg: 0.0,
                latitude_deg: 0.0,
                right_ascension_deg: ra,
                declination_deg: dec,
                distance_km: Some(1.0),
                longitude_speed_deg_per_day: 0.0,
                latitude_speed_deg_per_day: 0.0,
                distance_speed_km_per_day: Some(0.0),
            })
            .collect();
        let provenance = ChartProvenance {
            algorithm: "synthetic-chart-geometry-fixture",
            version: "test",
            manifest: EphemerisManifest {
                version: crate::MANIFEST_VERSION,
                time_scale_model: "synthetic source model".into(),
                reference_frame: "ICRF/J2000".into(),
                files: vec![EphemerisFile {
                    name: "synthetic.bsp".into(),
                    sha256: "0".repeat(64),
                    bytes: 1,
                    source: "synthetic://chart-geometry".into(),
                    upstream_version: "synthetic".into(),
                    retrieved_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
                    coverage_start: "1900-01-01".into(),
                    coverage_end: "2050-01-01".into(),
                }],
            },
            tzdb_version: time.tzdb_version(),
            delta_t_model: crate::lines::geojson::DELTA_T_MODEL,
            delta_t_sources: crate::delta_t_sources(),
            sidereal_model: crate::lines::geojson::SIDEREAL_MODEL,
            node_model: crate::lines::geojson::NODE_CONVENTION,
            time_scales: Some(instant.time_scales()),
        };
        AstrologyChartResponse {
            request,
            time,
            instant: Some(instant),
            positions: Some(positions),
            angles: None,
            provenance,
        }
    }

    #[test]
    fn near_equatorial_polyline_interiors_obey_the_angular_budget() {
        let mut response = response();
        response.positions.as_mut().unwrap()[0].declination_deg = 0.01;
        let geometry = geometry(&response);
        let instant = response.instant.unwrap();
        let gast = sidereal::gast_degrees_for(instant);
        let state = &response.positions.as_ref().unwrap()[0];
        let mut maximum = 0.0_f64;
        for line in geometry
            .lines()
            .iter()
            .filter(|line| line.body() == state.body && matches!(line.angle(), "asc" | "dsc"))
        {
            for pair in line
                .segments()
                .iter()
                .flat_map(|segment| segment.windows(2))
            {
                for fraction in [0.125, 0.25, 0.5, 0.75, 0.875] {
                    let longitude = pair[0][0] + fraction * (pair[1][0] - pair[0][0]);
                    let latitude = pair[0][1] + fraction * (pair[1][1] - pair[0][1]);
                    maximum = maximum.max(
                        altitude_deg(
                            longitude,
                            latitude,
                            state.right_ascension_deg,
                            state.declination_deg,
                            gast,
                        )
                        .abs(),
                    );
                }
            }
        }
        assert!(
            maximum <= 0.001,
            "interior altitude residual {maximum} exceeds 0.001 degrees"
        );
        assert_eq!(
            geometry
                .provenance()
                .convention
                .polyline_interpolation_error_bound_deg,
            Some(0.001)
        );
    }

    #[test]
    fn selected_chart_bodies_become_canonical_lines_in_request_order() {
        let geometry = AstrocartographyGeometry::from_response(&response())
            .expect("supported selected response")
            .expect("an exact clock has geometry");
        assert_eq!(geometry.lines.len(), 8);
        let keys: Vec<_> = geometry
            .lines
            .iter()
            .map(|line| (line.body, line.angle))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("mercury", "mc"),
                ("mercury", "ic"),
                ("mercury", "asc"),
                ("mercury", "dsc"),
                ("sun", "mc"),
                ("sun", "ic"),
                ("sun", "asc"),
                ("sun", "dsc")
            ]
        );
        assert!(geometry.lines.iter().all(|line| !line.segments.is_empty()));
    }
    fn geometry(response: &AstrologyChartResponse) -> AstrocartographyGeometry {
        AstrocartographyGeometry::from_response(response)
            .unwrap()
            .expect("exact-time geometry")
    }

    fn altitude_deg(longitude: f64, latitude: f64, ra: f64, dec: f64, gast: f64) -> f64 {
        let phi = latitude.to_radians();
        let delta = dec.to_radians();
        let hour = (gast + longitude - ra).to_radians();
        (phi.sin() * delta.sin() + phi.cos() * delta.cos() * hour.cos())
            .asin()
            .to_degrees()
    }

    fn wrapped_angle(degrees: f64) -> f64 {
        (degrees + 180.0).rem_euclid(360.0) - 180.0
    }

    #[test]
    fn every_vertex_obeys_its_physical_condition_and_diurnal_orientation() {
        let response = response();
        let geometry = geometry(&response);
        let gast = sidereal::gast_degrees_for(response.instant.unwrap());
        for line in geometry.lines() {
            let state = response
                .positions
                .as_ref()
                .unwrap()
                .iter()
                .find(|state| state.body == line.body())
                .unwrap();
            assert_eq!(line.source_id(), state.source_id);
            let mut crossing_vertices = 0;
            for point in line.segments().iter().flatten() {
                let hour = wrapped_angle(gast + point[0] - state.right_ascension_deg);
                match line.angle() {
                    "mc" => assert!(
                        hour.abs() <= VERTEX_TOLERANCE_DEG,
                        "MC hour angle must vanish"
                    ),
                    "ic" => assert!(
                        (hour.abs() - 180.0).abs() <= VERTEX_TOLERANCE_DEG,
                        "IC must be antipodal to MC"
                    ),
                    "asc" | "dsc" => {
                        let altitude = altitude_deg(
                            point[0],
                            point[1],
                            state.right_ascension_deg,
                            state.declination_deg,
                            gast,
                        );
                        assert!(altitude.abs() <= VERTEX_TOLERANCE_DEG, "every horizon vertex, including seam insertions, must have zero altitude");
                        assert!(
                            point[1].abs() <= 90.0 - state.declination_deg.abs() + 1e-9,
                            "circumpolar latitudes must be omitted"
                        );
                        let derivative = -point[1].to_radians().cos()
                            * state.declination_deg.to_radians().cos()
                            * hour.to_radians().sin();
                        if derivative.abs() > 1e-10 {
                            assert_eq!(derivative > 0.0, line.angle() == "asc");
                            crossing_vertices += 1;
                        }
                    }
                    _ => panic!("unknown angle"),
                }
            }
            if matches!(line.angle(), "asc" | "dsc") {
                assert!(
                    crossing_vertices > 100,
                    "orientation must be checked away from tangent endpoints"
                );
            }
        }
    }

    #[test]
    fn seams_keep_paired_edges_and_physical_geometry_is_not_mercator_clipped() {
        let mut response = response();
        let gast = sidereal::gast_degrees_for(response.instant.unwrap());
        response.positions.as_mut().unwrap()[0].right_ascension_deg =
            (gast + 170.0).rem_euclid(360.0);
        let geometry = geometry(&response);
        let dsc = geometry
            .lines()
            .iter()
            .find(|line| line.body() == "mercury" && line.angle() == "dsc")
            .unwrap();
        assert_eq!(dsc.segments().len(), 2);
        let left = dsc.segments()[0].last().unwrap();
        let right = dsc.segments()[1].first().unwrap();
        assert_eq!(left[0].to_bits(), 180.0_f64.to_bits());
        assert_eq!(right[0].to_bits(), (-180.0_f64).to_bits());
        assert_eq!(left[1].to_bits(), right[1].to_bits());
        for line in geometry.lines() {
            for segment in line.segments() {
                assert!(
                    segment.len() >= 2,
                    "canonical line members require two vertices"
                );
                for pair in segment.windows(2) {
                    assert!(
                        (pair[1][0] - pair[0][0]).abs() <= 180.0,
                        "separate world edges cannot be joined"
                    );
                }
            }
        }
        let mc = geometry
            .lines()
            .iter()
            .find(|line| line.angle() == "mc")
            .unwrap();
        assert_eq!(
            mc.segments()[0].first().unwrap()[1].to_bits(),
            (-90.0_f64).to_bits()
        );
        assert_eq!(
            mc.segments()[0].last().unwrap()[1].to_bits(),
            90.0_f64.to_bits()
        );
    }

    #[test]
    fn numerical_rows_and_geojson_use_the_same_segment_and_vertex_order() {
        let geometry = geometry(&response());
        let value: serde_json::Value = serde_json::from_str(&geometry.geojson().unwrap()).unwrap();
        let features = value["features"].as_array().unwrap();
        assert_eq!(features.len(), geometry.lines().len());
        let mut rows = geometry.point_rows();
        for (line, feature) in geometry.lines().iter().zip(features) {
            assert_eq!(feature["properties"]["body"], line.body());
            assert_eq!(feature["properties"]["angle"], line.angle());
            assert_eq!(
                feature["properties"]["source_id"],
                serde_json::to_value(line.source_id()).unwrap()
            );
            for (segment_index, segment) in line.segments().iter().enumerate() {
                for (vertex_index, point) in segment.iter().enumerate() {
                    let row = rows.next().expect("one row per canonical vertex");
                    assert_eq!(
                        (row.body, row.angle, row.segment_index, row.vertex_index),
                        (line.body(), line.angle(), segment_index, vertex_index)
                    );
                    assert_eq!(row.longitude_deg.to_bits(), point[0].to_bits());
                    assert_eq!(row.latitude_deg.to_bits(), point[1].to_bits());
                    let json_point =
                        &feature["geometry"]["coordinates"][segment_index][vertex_index];
                    assert!((json_point[0].as_f64().unwrap() - point[0]).abs() < 1e-12);
                    assert!((json_point[1].as_f64().unwrap() - point[1]).abs() < 1e-12);
                }
            }
        }
        assert!(
            rows.next().is_none(),
            "the table contains no disconnected or duplicated vertices"
        );
    }

    #[test]
    fn normalized_historical_ut1_and_fractional_epoch_are_preserved() {
        let mut response = response();
        let BirthTime::Exact(ref mut local) = response.request.birth_time else {
            panic!("exact fixture")
        };
        local.local = "1950-01-01T12:00:00.123456789".parse().unwrap();
        response.time = response.request.resolve().unwrap();
        response.instant = response.time.instant().unwrap();
        response.provenance.time_scales = response.instant.map(crate::ChartInstant::time_scales);
        let geometry = geometry(&response);
        let actual_mc = geometry.lines()[0].segments()[0][0][0];
        let state = &response.positions.as_ref().unwrap()[0];
        let instant = response.instant.unwrap();
        let expected_mc = state.right_ascension_deg - sidereal::gast_degrees_for(instant);
        let wrong_utc_label =
            hifitime::Epoch::maybe_from_gregorian_utc(1950, 1, 1, 12, 0, 0, 123_456_789).unwrap();
        let wrong_raw_mc = state.right_ascension_deg - sidereal::gast_degrees(wrong_utc_label);
        assert!(wrapped_angle(actual_mc - expected_mc).abs() < VERTEX_TOLERANCE_DEG);
        assert!(
            wrapped_angle(actual_mc - wrong_raw_mc).abs() > 1e-5,
            "the historical fixture must distinguish explicit UT1 from proleptic UTC"
        );
        let value = serde_json::to_value(geometry.provenance()).unwrap();
        assert_eq!(
            value["calculation"],
            serde_json::to_value(&response.provenance).unwrap()
        );
        assert_eq!(
            value["calculation"]["time_scales"]["reference"]["scale"],
            "ut1"
        );
        assert!(value["calculation"]["time_scales"]["julian_day_utc"].is_null());
        assert!(value["calculation"]["time_scales"]["reference"]["calendar"]
            .as_str()
            .unwrap()
            .ends_with(".123456789"));
        assert_eq!(
            value["calculation"]["manifest"]["reference_frame"],
            "ICRF/J2000"
        );
        assert_eq!(value["settings"]["frame"], "true_of_date");
        assert_eq!(
            value["convention"]["polyline_interpolation_error_bound_deg"],
            0.001
        );
        assert_eq!(
            value["convention"]["solver"],
            "navigator-diurnal-true-horizon-v2"
        );
    }

    #[test]
    fn unknown_and_approximate_clocks_have_no_exact_geometry() {
        let mut response = response();
        let BirthTime::Exact(local) = response.request.birth_time.clone() else {
            panic!("exact fixture")
        };
        for clock in [
            BirthTime::Unknown {
                date: local.local.date(),
                zone: local.zone,
            },
            BirthTime::Approximate {
                local,
                uncertainty_seconds: std::num::NonZeroU32::new(600).unwrap(),
            },
        ] {
            response.request.birth_time = clock;
            response.time = response.request.resolve().unwrap();
            response.instant = response.time.instant().unwrap();
            response.provenance.time_scales =
                response.instant.map(crate::ChartInstant::time_scales);
            assert!(AstrocartographyGeometry::from_response(&response)
                .unwrap()
                .is_none());
        }
    }

    #[test]
    fn invalid_frame_body_and_nonfinite_direction_fail_closed_without_values() {
        let mut response = response();
        response.request.settings.frame = crate::CoordinateFrame::IcrfJ2000;
        assert!(matches!(
            AstrocartographyGeometry::from_response(&response),
            Err(ChartError::UnsupportedSetting(_))
        ));
        response.request.settings.frame = crate::CoordinateFrame::TrueOfDate;
        response.positions.as_mut().unwrap()[0].body = "unrequested-body";
        assert!(matches!(
            AstrocartographyGeometry::from_response(&response),
            Err(ChartError::InvalidBodySelection)
        ));
        response.positions.as_mut().unwrap()[0].body = "mercury";
        response.positions.as_mut().unwrap()[0].right_ascension_deg = f64::NAN;
        let result = AstrocartographyGeometry::from_response(&response);
        assert!(matches!(result, Err(ChartError::InvalidGeometry)));
        if let Err(error) = result {
            assert!(!error.to_string().contains("NaN"));
        }
        response.positions.as_mut().unwrap()[0].right_ascension_deg = 320.0;
        response.positions.as_mut().unwrap().pop();
        assert!(matches!(
            AstrocartographyGeometry::from_response(&response),
            Err(ChartError::InvalidBodySelection)
        ));
    }

    #[test]
    fn celestial_pole_directions_have_no_defined_diurnal_orientation() {
        for declination in [-90.0, 90.0] {
            let mut response = response();
            response.positions.as_mut().unwrap()[0].declination_deg = declination;
            assert!(matches!(
                AstrocartographyGeometry::from_response(&response),
                Err(ChartError::InvalidGeometry)
            ));
        }
    }

    #[test]
    fn empty_selection_mean_node_identity_and_response_release_are_explicit() {
        let mut response = response();
        response.request.bodies = vec![LineBody::MeanNode];
        let mut node = response.positions.as_ref().unwrap()[0].clone();
        node.body = "mean_node";
        node.source_id = None;
        node.declination_deg = 0.0;
        response.positions = Some(vec![node]);
        let response = std::sync::Arc::new(response);
        let released = std::sync::Arc::downgrade(&response);
        let geometry = geometry(&response);
        drop(response);
        assert!(
            released.upgrade().is_none(),
            "canonical geometry cannot retain the provider request/result"
        );
        assert!(geometry
            .lines()
            .iter()
            .all(|line| line.source_id().is_none()));
        let value: serde_json::Value = serde_json::from_str(&geometry.geojson().unwrap()).unwrap();
        assert!(value.get("request").is_none() && value.get("birth_time").is_none());
        assert!(value["provenance"].get("longitude_deg").is_none());
        assert!(value["provenance"].get("latitude_deg").is_none());
        let mut response = self::response();
        response.request.bodies.clear();
        response.positions = Some(Vec::new());
        let geometry = AstrocartographyGeometry::from_response(&response)
            .unwrap()
            .expect("an exact empty selection is explicit");
        assert!(geometry.lines().is_empty() && geometry.point_rows().next().is_none());
    }
    #[test]
    fn mismatched_source_target_is_rejected() {
        let mut response = response();
        response.positions.as_mut().unwrap()[0].source_id = Some(1);
        assert!(
            matches!(
                AstrocartographyGeometry::from_response(&response),
                Err(ChartError::InvalidBodySelection)
            ),
            "a selected planet center cannot be relabeled with another SPK target or barycenter"
        );
    }

    #[test]
    fn fractional_mc_seam_vertices_keep_the_horizon_condition() {
        let mut response = response();
        let gast = sidereal::gast_degrees_for(response.instant.unwrap());
        response.positions.as_mut().unwrap()[0].right_ascension_deg =
            (gast + 17.123_456_789).rem_euclid(360.0);
        response.positions.as_mut().unwrap()[0].declination_deg = 20.0;
        let geometry = geometry(&response);
        let dsc = geometry
            .lines()
            .iter()
            .find(|line| line.body() == "mercury" && line.angle() == "dsc")
            .unwrap();
        assert_eq!(dsc.segments().len(), 2);
        let state = &response.positions.as_ref().unwrap()[0];
        let seam_vertices: Vec<_> = dsc
            .segments()
            .iter()
            .flatten()
            .filter(|point| (point[0].abs() - 180.0).abs() < f64::EPSILON)
            .collect();
        assert_eq!(seam_vertices.len(), 2);
        for point in seam_vertices {
            assert!(altitude_deg(point[0], point[1], state.right_ascension_deg, state.declination_deg, gast).abs() < VERTEX_TOLERANCE_DEG,
                "fractional-phase seam insertion must use the horizon great-circle plane, not linear latitude interpolation");
        }
    }
    #[test]
    fn missing_or_inconsistent_clock_and_source_metadata_are_rejected() {
        let mut response = response();
        let instant = response.instant;
        response.instant = None;
        assert!(matches!(
            AstrocartographyGeometry::from_response(&response),
            Err(ChartError::InvalidGeometry)
        ));
        response.instant = instant;
        let scales = response.provenance.time_scales.take();
        assert!(matches!(
            AstrocartographyGeometry::from_response(&response),
            Err(ChartError::InvalidGeometry)
        ));
        response.provenance.time_scales = scales;
        response
            .provenance
            .time_scales
            .as_mut()
            .unwrap()
            .model_julian_day_ut1 += 1.0;
        assert!(matches!(
            AstrocartographyGeometry::from_response(&response),
            Err(ChartError::InvalidGeometry)
        ));
        response.provenance.time_scales = response.instant.map(crate::ChartInstant::time_scales);
        response.provenance.manifest.files.clear();
        assert!(matches!(
            AstrocartographyGeometry::from_response(&response),
            Err(ChartError::InvalidGeometry)
        ));
    }
}
