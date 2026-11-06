//! Canonical chart geometry from checksum-verified JPL sources.

use ephemeris::lines::{lines_for_instant, sidereal, LineBody};
use ephemeris::{
    AstrocartographyGeometry, AstrologyChartProvider, AstrologyChartRequest, BirthTime,
    EphemerisManifest, FilesystemStore, LocalBirthTime, DATA_DIR_ENV,
};
use std::num::NonZeroUsize;

fn altitude_deg(longitude: f64, latitude: f64, ra: f64, dec: f64, gast: f64) -> f64 {
    let (phi, delta, hour) = (
        latitude.to_radians(),
        dec.to_radians(),
        (gast + longitude - ra).to_radians(),
    );
    (phi.sin() * delta.sin() + phi.cos() * delta.cos() * hour.cos())
        .asin()
        .to_degrees()
}

fn wrapped_angle(degrees: f64) -> f64 {
    (degrees + 180.0).rem_euclid(360.0) - 180.0
}

#[test]
fn selected_real_jpl_states_preserve_geometry_provenance_and_physical_conditions() {
    if std::env::var_os(DATA_DIR_ENV).is_none() {
        eprintln!("skipping: pinned reference kernels are unavailable");
        return;
    }
    let manifest: EphemerisManifest =
        serde_json::from_str(include_str!("../manifest.json")).unwrap();
    let store = FilesystemStore::from_env().expect("configured reference source directory");
    let provider = AstrologyChartProvider::load(&store, &manifest, NonZeroUsize::MIN)
        .expect("exact pinned JPL kernels verify");
    let loaded = ephemeris::load(&store, &manifest).expect("comparison source pins verify");
    for civil in [
        "1900-01-01T00:00:00.123456789",
        "1950-01-01T12:00:00.123456789",
        "2000-01-01T12:00:00.123456789",
        "2049-12-31T23:59:59.123456789",
    ] {
        let mut request = AstrologyChartRequest::new(
            BirthTime::Exact(LocalBirthTime {
                local: civil.parse().unwrap(),
                zone: chrono_tz::UTC,
                offset_seconds: None,
            }),
            12.5,
            34.5,
        );
        request.bodies = vec![
            LineBody::Chiron,
            LineBody::MeanNode,
            LineBody::Jupiter,
            LineBody::Sun,
            LineBody::Moon,
        ];
        let response = provider.calculate_uncached(&request).unwrap();
        let geometry = AstrocartographyGeometry::from_response(&response)
            .expect("validated body states adapt")
            .expect("exact-time geometry");
        assert_eq!(geometry.lines().len(), request.bodies.len() * 4);
        let instant = response.instant.unwrap();
        let gast = sidereal::gast_degrees_for(instant);
        let comparison = lines_for_instant(&loaded, instant)
            .expect("existing engine evaluates same precise epoch");
        let tolerance = geometry
            .provenance()
            .convention
            .vertex_altitude_tolerance_deg;
        for (body_index, state) in response.positions.as_ref().unwrap().iter().enumerate() {
            assert_body_geometry(
                &geometry.lines()[body_index * 4..body_index * 4 + 4],
                state,
                &comparison,
                gast,
                tolerance,
                geometry
                    .provenance()
                    .convention
                    .polyline_interpolation_error_bound_deg
                    .unwrap(),
            );
        }
        assert_clock_provenance(&geometry, &response, civil);
    }
}

fn assert_clock_provenance(
    geometry: &AstrocartographyGeometry,
    response: &ephemeris::AstrologyChartResponse,
    civil: &str,
) {
    let value = serde_json::to_value(geometry.provenance()).unwrap();
    assert_eq!(
        value["calculation"],
        serde_json::to_value(&response.provenance).unwrap()
    );
    assert_eq!(
        value["convention"]["polyline_interpolation_error_bound_deg"],
        0.001
    );
    assert!(value["calculation"]["time_scales"]["reference"]["calendar"]
        .as_str()
        .unwrap()
        .ends_with(".123456789"));
    assert_eq!(
        value["calculation"]["time_scales"]["julian_day_utc"].is_null(),
        civil.starts_with("1900") || civil.starts_with("1950")
    );
}

fn assert_continuous_condition(
    segment: &[[f64; 2]],
    angle: &str,
    state: &ephemeris::BodyPosition,
    gast: f64,
    budget: f64,
) {
    if !matches!(angle, "asc" | "dsc") {
        return;
    }
    for edge in segment.windows(2) {
        for fraction in [0.125, 0.25, 0.5, 0.75, 0.875] {
            let longitude = edge[0][0] + fraction * (edge[1][0] - edge[0][0]);
            let latitude = edge[0][1] + fraction * (edge[1][1] - edge[0][1]);
            assert!(
                altitude_deg(
                    longitude,
                    latitude,
                    state.right_ascension_deg,
                    state.declination_deg,
                    gast
                )
                .abs()
                    <= budget,
                "independent JPL segment interiors stay within the analytic budget"
            );
        }
    }
}

fn assert_body_geometry(
    lines: &[ephemeris::GeographicLine],
    state: &ephemeris::BodyPosition,
    comparison: &[ephemeris::lines::AngularLine],
    gast: f64,
    tolerance: f64,
    budget: f64,
) {
    let mut horizon_vertices = 0;
    for line in lines {
        assert_eq!(line.body(), state.body);
        assert_eq!(line.source_id(), state.source_id);
        let reference = comparison
            .iter()
            .find(|reference| {
                reference.body.name() == state.body && reference.angle.name() == line.angle()
            })
            .unwrap();
        let reference_segments = reference.segments();
        assert_eq!(line.segments().len(), reference_segments.len());
        for (segment, expected_segment) in line.segments().iter().zip(reference_segments) {
            let mut retained = segment.iter();
            for expected in expected_segment {
                assert!(
                    retained.any(|point| (point[0] - expected.0).abs() < 1e-10
                        && (point[1] - expected.1).abs() < 1e-10),
                    "refinement preserves the existing physical vertices and order"
                );
            }
            assert_continuous_condition(segment, line.angle(), state, gast, budget);
            for point in segment {
                let hour = wrapped_angle(gast + point[0] - state.right_ascension_deg);
                match line.angle() {
                    "mc" => assert!(hour.abs() < tolerance, "MC hour angle is zero"),
                    "ic" => assert!(
                        (hour.abs() - 180.0).abs() < tolerance,
                        "IC hour angle is antipodal"
                    ),
                    "asc" | "dsc" => {
                        assert!(altitude_deg(point[0], point[1], state.right_ascension_deg, state.declination_deg, gast).abs() < tolerance,
                            "all JPL horizon vertices, including fractional-phase seam insertions, satisfy the physical condition");
                        assert!(
                            point[1].abs() <= 90.0 - state.declination_deg.abs() + tolerance,
                            "circumpolar latitudes are omitted"
                        );
                        let derivative = -point[1].to_radians().cos()
                            * state.declination_deg.to_radians().cos()
                            * hour.to_radians().sin();
                        if derivative.abs() > 1e-10 {
                            assert_eq!(derivative > 0.0, line.angle() == "asc");
                            horizon_vertices += 1;
                        }
                    }
                    _ => panic!("unknown angle"),
                }
            }
        }
    }
    assert!(
        horizon_vertices > 100,
        "every selected body has tested rising and setting interiors"
    );
}
