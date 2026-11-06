//! Exact pinned JPL geometry reaches a provenance-free display boundary.

use ephemeris::lines::LINE_BODIES;
use ephemeris::{
    AstrocartographyGeometry, AstrologyChartProvider, AstrologyChartRequest, BirthTime,
    EphemerisManifest, FilesystemStore, LocalBirthTime, DATA_DIR_ENV,
};
use std::{f64::consts::PI, num::NonZeroUsize, sync::Arc};
use webapp::astrocartography::mercator::{latitude_limit_deg, project_lines};

fn on_edge(point: [f64; 2], a: [f64; 2], b: [f64; 2]) -> bool {
    let axis = usize::from((b[1] - a[1]).abs() > (b[0] - a[0]).abs());
    let span = b[axis] - a[axis];
    if span.abs() < 1e-14 {
        return (point[0] - a[0]).abs().max((point[1] - a[1]).abs()) < 1e-8;
    }
    let t = (point[axis] - a[axis]) / span;
    (-1e-8..=1.0 + 1e-8).contains(&t)
        && (0..2).all(|i| (point[i] - (a[i] + t * (b[i] - a[i]))).abs() < 1e-8)
}

#[test]
fn pinned_jpl_geometry_clips_without_mutating_sources_or_retaining_private_response() {
    if std::env::var_os(DATA_DIR_ENV).is_none() {
        assert!(
            std::env::var_os("NAV_REQUIRE_MERCATOR_SOURCES").is_none(),
            "pinned sources required"
        );
        eprintln!("skipping pinned JPL lane: source directory is not configured");
        return;
    }
    let manifest: EphemerisManifest =
        serde_json::from_str(include_str!("../../ephemeris/manifest.json")).unwrap();
    let provider = AstrologyChartProvider::load(
        &FilesystemStore::from_env().unwrap(),
        &manifest,
        NonZeroUsize::MIN,
    )
    .unwrap();
    for civil in [
        "1900-01-01T00:00:00.123456789",
        "2000-01-01T12:00:00.123456789",
        "2049-12-31T23:59:59.123456789",
    ] {
        let mut request = AstrologyChartRequest::new(
            BirthTime::Exact(LocalBirthTime {
                local: civil.parse().unwrap(),
                zone: "UTC".parse().unwrap(),
                offset_seconds: None,
            }),
            12.5,
            34.5,
        );
        request.bodies = LINE_BODIES.to_vec();
        let response = provider.calculate_uncached(&request).unwrap();
        let weak = Arc::downgrade(&response);
        let geometry = AstrocartographyGeometry::from_response(&response)
            .unwrap()
            .unwrap();
        let before = geometry.geojson().unwrap();
        let output = project_lines(geometry.lines(), &[-1, 0, 1]).unwrap();
        assert_eq!(output.len(), 12 * 4 * 3);
        for (index, copies) in output.as_chunks::<3>().0.iter().enumerate() {
            let source = &geometry.lines()[index];
            for (display, world) in copies.iter().zip([-1, 0, 1]) {
                assert_eq!(display.line_index, index);
                assert_eq!(display.world_copy, world);
                assert_eq!(display.body, source.body());
                assert_eq!(display.angle, source.angle());
                assert_eq!(display.source_id, source.source_id());
                for segment in &display.segments {
                    assert!(segment.points.len() >= 2);
                    let original = &source.segments()[segment.source_segment_index];
                    for point in &segment.points {
                        let x = point[0] - f64::from(world);
                        assert!((0.0..=1.0).contains(&x));
                        assert!((0.0..=1.0).contains(&point[1]));
                        let inverse = [
                            x * 360.0 - 180.0,
                            ((0.5 - point[1]) * 2.0 * PI).sinh().atan().to_degrees(),
                        ];
                        assert!(inverse[1].abs() <= latitude_limit_deg() + 1e-12);
                        assert!(
                            original
                                .windows(2)
                                .any(|edge| on_edge(inverse, edge[0], edge[1])),
                            "display endpoint must lie on its original geographic edge"
                        );
                    }
                }
            }
            for (left, right) in copies[0].segments.iter().zip(&copies[2].segments) {
                for (a, b) in left.points.iter().zip(&right.points) {
                    assert!((b[0] - a[0] - 2.0).abs() < 1e-14);
                    assert!((b[1] - a[1]).abs() < 1e-14);
                }
            }
        }
        assert!(
            geometry.geojson().unwrap() == before,
            "canonical geometry and provenance remain unchanged"
        );
        assert!(geometry
            .lines()
            .iter()
            .flat_map(ephemeris::GeographicLine::segments)
            .flatten()
            .any(|p| p[1].abs() > latitude_limit_deg()));
        drop(response);
        drop(geometry);
        assert!(
            weak.upgrade().is_none(),
            "display geometry cannot retain the private provider response"
        );
        assert!(!output.is_empty());
    }
}
