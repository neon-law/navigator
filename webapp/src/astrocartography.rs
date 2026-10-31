//! Offline SVG astrocartography map: calculated lines, no JavaScript.
//!
//! [`AstrocartographyMap`] renders the Natural Earth 1:110m land silhouette
//! with any number of preprojected [`MapOverlay`]s. [`overlays`] adapts the
//! ephemeris crate's line-engine output — one [`AngularLine`] per body and
//! angle, carrying WGS84 longitude/latitude polylines — into those overlays.
//! The map stays purely presentational: every astronomy calculation happens
//! in the ephemeris engine, and this crate imports only the engine's geometry
//! types.
//!
//! The design specimen ([`AstrocartographyDesign`]) draws a real chart: the
//! line engine's output for a fixed specimen instant, embedded as a
//! provenance-stamped JSON fixture ([`specimen_lines`]) so the page renders
//! offline with no JavaScript and no kernel dependency at request time. The
//! fixture's provenance and regeneration path live in its metadata fields;
//! the kernel-gated test in `webapp/tests/specimen_fixture.rs` re-casts the
//! same instant through the engine and compares.

use std::sync::LazyLock;

use dioxus::prelude::*;
use ephemeris::lines::{Angle, AngularLine, LineBody, Point, LINE_BODIES};
use serde::Deserialize;

const WIDTH: f64 = 960.0;
const HEIGHT: f64 = 480.0;
// Natural Earth v5.1.2, 1:110m land. Public-domain coordinates projected to 960×480.
// Source: https://github.com/nvkelso/natural-earth-vector/blob/v5.1.2/geojson/ne_110m_land.geojson
// License: https://www.naturalearthdata.com/about/terms-of-use/
const LAND_PATH: &str = include_str!("assets/natural-earth-110m-land.path");

/// The specimen chart's civil instant, ISO-8601 UTC: noon on 2000-01-01, the
/// probe instant the ephemeris fixtures already anchor.
pub const SPECIMEN_INSTANT: &str = "2000-01-01T12:00:00Z";

/// The embedded specimen chart: the line engine's real output at
/// [`SPECIMEN_INSTANT`], snapshotted with provenance metadata.
const SPECIMEN_JSON: &str = include_str!("assets/world-specimen-lines.json");

/// Project a validated longitude/latitude pair onto an equirectangular map.
/// Both antimeridian edges remain distinct so paths can be split at the seam.
pub(crate) fn project(longitude: f64, latitude: f64) -> Option<(f64, f64)> {
    if !longitude.is_finite()
        || !latitude.is_finite()
        || !(-180.0..=180.0).contains(&longitude)
        || !(-90.0..=90.0).contains(&latitude)
    {
        return None;
    }
    Some((
        (longitude + 180.0) / 360.0 * WIDTH,
        (90.0 - latitude) / 180.0 * HEIGHT,
    ))
}

/// Wrap a longitude into [−180, 180). The engine's polylines run continuously
/// through the seam (e.g. a horizon curve may carry 190° rather than −170°);
/// mirroring the engine's `GeoJSON` per-point wrapping keeps every vertex on the
/// map while the engine's split segments prevent tears.
fn wrap_longitude(longitude: f64) -> f64 {
    let wrapped = longitude.rem_euclid(360.0);
    if wrapped >= 180.0 {
        wrapped - 360.0
    } else {
        wrapped
    }
}

/// Start a new SVG subpath at invalid points or a crossing of the map seam.
pub(crate) fn curve_path(points: &[(f64, f64)]) -> String {
    use std::fmt::Write;
    let mut path = String::new();
    let mut previous: Option<f64> = None;
    for &(longitude, latitude) in points {
        let longitude = wrap_longitude(longitude);
        let Some((x, y)) = project(longitude, latitude) else {
            previous = None;
            continue;
        };
        let connected = previous.is_some_and(|last| (longitude - last).abs() <= 180.0);
        let command = if connected { 'L' } else { 'M' };
        let _ = write!(path, "{command}{x:.2},{y:.2} ");
        previous = Some(longitude);
    }
    path
}

/// The theme style for a line: one of the specimen's four styles per angle,
/// so culmination meridians read solid and horizon curves read dashed on the
/// fallback map. Body identity belongs to the surrounding page's controls.
fn style_index(angle: Angle) -> usize {
    match angle {
        Angle::Mc => 0,
        Angle::Ic => 1,
        Angle::Asc => 2,
        Angle::Dsc => 3,
    }
}

/// Convert line-engine output into map overlays: one overlay per line, each
/// path joining the line's antimeridian-split polylines (which the engine's
/// [`AngularLine::segments`] already cut at ±180°).
#[must_use]
pub fn overlays(lines: &[AngularLine]) -> Vec<MapOverlay> {
    lines.iter().map(overlay).collect()
}

fn overlay(line: &AngularLine) -> MapOverlay {
    let mut path = String::new();
    for segment in line.segments() {
        let points: Vec<(f64, f64)> = segment.iter().map(|point| (point.0, point.1)).collect();
        path.push_str(&curve_path(&points));
    }
    MapOverlay {
        path,
        style_index: style_index(line.angle),
        selected: false,
    }
}

/// The fixture's provenance metadata: who generated the snapshot, for which
/// instant, and against which binding verification gate.
#[derive(Deserialize)]
struct SpecimenChart {
    generator: String,
    instant: String,
    swisseph_gate: String,
    rows: Vec<SpecimenRow>,
}

/// One line of the snapshot: a body, an angle, and the WGS84 polyline.
#[derive(Deserialize)]
struct SpecimenRow {
    body: String,
    angle: String,
    points: Vec<[f64; 2]>,
}

fn line_body_named(name: &str) -> Option<LineBody> {
    LINE_BODIES.iter().copied().find(|body| body.name() == name)
}

fn angle_named(name: &str) -> Option<Angle> {
    match name {
        "mc" => Some(Angle::Mc),
        "ic" => Some(Angle::Ic),
        "asc" => Some(Angle::Asc),
        "dsc" => Some(Angle::Dsc),
        _ => None,
    }
}

impl SpecimenRow {
    fn to_line(&self) -> Result<AngularLine, String> {
        let body = line_body_named(&self.body)
            .ok_or_else(|| format!("unknown line body {:?}", self.body))?;
        let angle =
            angle_named(&self.angle).ok_or_else(|| format!("unknown angle {:?}", self.angle))?;
        Ok(AngularLine {
            body,
            angle,
            points: self.points.iter().map(|p| Point(p[0], p[1])).collect(),
        })
    }
}

impl SpecimenChart {
    fn lines(&self) -> Result<Vec<AngularLine>, String> {
        self.rows.iter().map(SpecimenRow::to_line).collect()
    }
}

fn parse_specimen(json: &str) -> Result<SpecimenChart, String> {
    let chart: SpecimenChart = serde_json::from_str(json).map_err(|error| error.to_string())?;
    if chart.instant != SPECIMEN_INSTANT {
        return Err(format!(
            "fixture was generated for {} but this build expects {}",
            chart.instant, SPECIMEN_INSTANT
        ));
    }
    Ok(chart)
}

/// The fixture's provenance metadata: who generated the snapshot and against
/// which binding verification gate. Surfaced in the page's transparency copy.
static SPECIMEN_PROVENANCE: LazyLock<Result<(String, String), String>> = LazyLock::new(|| {
    parse_specimen(SPECIMEN_JSON)
        .map(|chart| (chart.generator, chart.swisseph_gate))
        .map_err(|error| format!("embedded specimen fixture is malformed: {error}"))
});

/// The embedded specimen chart as line-engine geometry: 48 calculated lines
/// (twelve bodies × four angles) at [`SPECIMEN_INSTANT`]. The fixture carries
/// the generator, instant, and Swiss Ephemeris gate in its metadata; the
/// kernel-gated test in `webapp/tests/specimen_fixture.rs` re-casts the same
/// instant through the engine and compares row by row.
///
/// # Errors
///
/// A malformed embedded fixture surfaces as a message the specimen renders
/// instead of a chart; the fixture ships with the crate, so this only fires
/// if the asset and its parser drift apart.
pub fn specimen_lines() -> Result<Vec<AngularLine>, String> {
    parse_specimen(SPECIMEN_JSON).and_then(|chart| chart.lines())
}

static SPECIMEN: LazyLock<Result<Vec<AngularLine>, String>> = LazyLock::new(specimen_lines);

/// One preprojected overlay for the reusable map. Coordinates use a 960×480 view box.
/// Callers supply paths computed from their own data; the map performs no astrology calculation.
#[derive(Clone, PartialEq)]
pub struct MapOverlay {
    /// SVG path in equirectangular map coordinates; split crossings at ±180°.
    pub path: String,
    /// Theme line style index (the specimen supplies styles zero through three).
    pub style_index: usize,
    /// Emphasize the overlay selected by the surrounding controls.
    pub selected: bool,
}

/// Offline world map renderer, independent of the specimen and its controls.
#[component]
pub fn AstrocartographyMap(overlays: Vec<MapOverlay>) -> Element {
    rsx! {
                svg { view_box: "-48 -28 1056 540", role: "img", "aria-label": "World map with planetary line overlays.",
                    title { "Astrocartography world map" }
                    desc { "Longitude runs from 180 degrees west to 180 degrees east; latitude runs from 90 degrees north to 90 degrees south. Public-domain Natural Earth land silhouettes provide geographic context." }
                    rect { x: "0", y: "0", width: "960", height: "480", class: "astro-map-background" }
                    path { d: LAND_PATH, class: "astro-map-land", fill_rule: "evenodd" }
                    for longitude in (-180..=180).step_by(30) {
                        line { x1: "{(f64::from(longitude) + 180.0) / 360.0 * WIDTH}", x2: "{(f64::from(longitude) + 180.0) / 360.0 * WIDTH}", y1: "0", y2: "480", class: "astro-map-grid" }
                        text { x: "{(f64::from(longitude) + 180.0) / 360.0 * WIDTH}", y: "504", text_anchor: "middle", class: "astro-map-coordinate", "{longitude}°" }
                    }
                    for latitude in (-90..=90).step_by(30) {
                        line { x1: "0", x2: "960", y1: "{(90.0 - f64::from(latitude)) / 180.0 * HEIGHT}", y2: "{(90.0 - f64::from(latitude)) / 180.0 * HEIGHT}", class: "astro-map-grid" }
                        text { x: "-8", y: "{(90.0 - f64::from(latitude)) / 180.0 * HEIGHT + 5.0}", text_anchor: "end", class: "astro-map-coordinate", "{latitude}°" }
                    }
                    for overlay in overlays {
                        path { d: overlay.path, fill: "none", class: "astro-map-line astro-map-line-{overlay.style_index}", stroke_width: if overlay.selected { "4" } else { "2" } }
                    }
                    text { x: "480", y: "-10", text_anchor: "middle", class: "astro-map-coordinate", "NORTH · +90°" }
                }
    }
}

fn body_index(body: LineBody) -> usize {
    LINE_BODIES
        .iter()
        .position(|candidate| *candidate == body)
        .unwrap_or(0)
}

fn body_symbol(body: LineBody) -> &'static str {
    match body {
        LineBody::Sun => "☉",
        LineBody::Moon => "☽",
        LineBody::Mercury => "☿",
        LineBody::Venus => "♀",
        LineBody::Mars => "♂",
        LineBody::Jupiter => "♃",
        LineBody::Saturn => "♄",
        LineBody::Uranus => "♅",
        LineBody::Neptune => "♆",
        LineBody::Pluto => "♇",
        LineBody::Chiron => "⚷",
        LineBody::MeanNode => "☊",
    }
}

fn body_display_name(body: LineBody) -> &'static str {
    match body {
        LineBody::Sun => "Sun",
        LineBody::Moon => "Moon",
        LineBody::Mercury => "Mercury",
        LineBody::Venus => "Venus",
        LineBody::Mars => "Mars",
        LineBody::Jupiter => "Jupiter",
        LineBody::Saturn => "Saturn",
        LineBody::Uranus => "Uranus",
        LineBody::Neptune => "Neptune",
        LineBody::Pluto => "Pluto",
        LineBody::Chiron => "Chiron",
        LineBody::MeanNode => "North Node",
    }
}

/// A short reflection prompt per body, in the specimen's register. The four
/// prompts from the earlier synthetic specimen carry over unchanged; the rest
/// stay deliberately restrained.
fn body_reflection(body: LineBody) -> &'static str {
    match body {
        LineBody::Sun => {
            "Visibility and purpose. What would you like your life’s work to carry forward?"
        }
        LineBody::Moon => {
            "Roots and care. What traditions would you like the next generation to inherit?"
        }
        LineBody::Mercury => "Curiosity and conversation. Where do the questions you enjoy live?",
        LineBody::Venus => {
            "Connection and belonging. Which places and relationships feel like home?"
        }
        LineBody::Mars => "Energy and initiative. What would you like the courage to begin?",
        LineBody::Jupiter => {
            "Growth and generosity. Where do you imagine sharing what you have learned?"
        }
        LineBody::Saturn => {
            "Patience and structure. What foundations are you willing to build slowly?"
        }
        LineBody::Uranus => {
            "Independence and invention. Where does a fresh perspective welcome you?"
        }
        LineBody::Neptune => "Imagination and rest. Which places let your mind wander and recover?",
        LineBody::Pluto => "Change and depth. What transformation are you ready to honor?",
        LineBody::Chiron => {
            "Mentoring and mending. What have you learned that could guide someone else?"
        }
        LineBody::MeanNode => "Direction and connection. Which encounters pull your story onward?",
    }
}

/// A self-contained design specimen: one real calculated chart, embedded.
/// No birth details or external data are used.
#[component]
pub fn AstrocartographyDesign() -> Element {
    let Ok(lines) = &*SPECIMEN else {
        return rsx! {
            section { id: "astrocartography", class: "astro-map-design", role: "alert",
                p { "The calculated specimen chart could not be read. The embedded fixture and its parser have drifted apart; regenerate the fixture from the line engine." }
            }
        };
    };
    let mut visible = use_signal(|| [true; LINE_BODIES.len()]);
    let mut selected = use_signal(|| 0_usize);
    let body = LINE_BODIES[selected()];
    let chart_overlays: Vec<MapOverlay> = overlays(lines)
        .into_iter()
        .zip(lines.iter())
        .filter(|(overlay, line)| {
            let _ = overlay;
            visible()[body_index(line.body)]
        })
        .map(|(map_overlay, _)| map_overlay)
        .collect();
    rsx! {
        section { id: "astrocartography", class: "astro-map-design", "aria-label": "Astrocartography design specimen",
            header {
                p { class: "astro-eyebrow", "Places / Relationships / Legacy" }
                h2 { "A world of connection" }
                p { "Explore the places that invite reflection on belonging, purpose, and the people who carry your story forward." }
            }
            p { class: "astro-demo-notice", role: "note",
                strong { "Calculated specimen chart. " }
                "The lines are the ephemeris line engine's output for {SPECIMEN_INSTANT} — a real chart cast for that fixed instant, not your birth data. The map and its lines render entirely on the server: no script computes them, and no network request is made."
                if let Ok((generator, gate)) = &*SPECIMEN_PROVENANCE {
                    span { class: "astro-map-provenance", " Snapshot: {generator}, verified against Swiss Ephemeris to within {gate}." }
                }
            }
            fieldset { class: "astro-map-layers",
                legend { "Visible planetary lines" }
                for (index, item) in LINE_BODIES.iter().enumerate() {
                    label {
                        input { class: "nav-checkbox", r#type: "checkbox", checked: visible()[index],
                            onchange: move |_| { visible.with_mut(|layers| layers[index] = !layers[index]); }
                        }
                        "{body_symbol(*item)} {body_display_name(*item)}"
                    }
                }
            }
            div { class: "astro-map-frame",
                AstrocartographyMap { overlays: chart_overlays }
            }
            div { class: "astro-map-selection", role: "group", "aria-label": "Read about a planetary line",
                for (index, item) in LINE_BODIES.iter().enumerate() {
                    button { class: "nav-btn", r#type: "button", "aria-pressed": selected() == index,
                        onclick: move |_| selected.set(index),
                        "{body_symbol(*item)} {body_display_name(*item)}"
                    }
                }
            }
            div { class: "astro-map-detail", "aria-live": "polite",
                h3 { "{body_symbol(body)} {body_display_name(body)}" }
                p { "{body_reflection(body)}" }
                p { "Each body carries four calculated lines: MC and IC culmination meridians, and the ASC and DSC horizon curves. Line style follows the angle, so meridians read solid and horizon curves read dashed." }
                p { if visible()[selected()] { "This body’s lines are visible on the map." } else { "This body’s lines are hidden. Enable its checkbox to show them." } }
            }
            p { class: "astro-map-attribution", a { href: "https://www.naturalearthdata.com/about/terms-of-use/", "Made with Natural Earth · public domain" } " · " a { href: "https://github.com/nvkelso/natural-earth-vector/blob/v5.1.2/geojson/ne_110m_land.geojson", "110m land v5.1.2" } }
            details {
                summary { "Read the map" }
                p { "MC: Midheaven. IC: lower meridian. ASC: Ascendant. DSC: Descendant. These angles mark where a celestial body is on an angle for a particular moment — here, the calculated chart cast for {SPECIMEN_INSTANT}." }
                p { "Coordinates use signed degrees: negative longitude is west, positive longitude is east; positive latitude is north, negative latitude is south. Lines crossing the 180th meridian are split there rather than drawn across the world." }
                p { "These reflection prompts do not determine your legal wishes or recommend relocation." }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::{specimen_lines, wrap_longitude, SPECIMEN_INSTANT};
    use ephemeris::lines::{Angle, AngularLine, LineBody, Point};

    #[test]
    fn longitude_wraps_into_half_open_range() {
        fn assert_wrap(input: f64, expected: f64) {
            let wrapped = wrap_longitude(input);
            assert!(
                (wrapped - expected).abs() < 1e-9,
                "wrap({input}) = {wrapped}, expected {expected}"
            );
        }
        assert_wrap(0.0, 0.0);
        assert_wrap(190.0, -170.0);
        assert_wrap(531.6, 171.6);
        assert_wrap(-179.2, -179.2);
        assert_wrap(-180.0, -180.0);
        assert_wrap(359.9, -0.1);
    }

    #[test]
    fn horizon_curve_across_the_seam_stays_on_the_map() {
        // A curve whose engine longitudes run past +180° must draw on both
        // sides of the seam instead of collapsing to an empty path.
        let points: Vec<(f64, f64)> = (0..=6)
            .map(|i| (170.0 + f64::from(i) * 5.0, 10.0))
            .collect();
        let path = curve_path(&points);
        assert!(path.contains('M'), "path draws");
        assert_eq!(path.matches('M').count(), 2, "one subpath per seam side");
        assert!(!path.contains("L-"), "no subpath crosses the seam");
    }

    #[test]
    fn projection_preserves_poles_and_antimeridian_edges() {
        assert_eq!(project(-180.0, 90.0), Some((0.0, 0.0)));
        assert_eq!(project(180.0, -90.0), Some((960.0, 480.0)));
        assert_eq!(project(0.0, 0.0), Some((480.0, 240.0)));
    }

    #[test]
    fn projection_rejects_invalid_coordinates() {
        for point in [
            (181.0, 0.0),
            (0.0, -91.0),
            (f64::NAN, 0.0),
            (0.0, f64::INFINITY),
        ] {
            assert_eq!(project(point.0, point.1), None);
        }
    }

    #[test]
    fn curves_split_at_antimeridian_and_invalid_samples() {
        let path = curve_path(&[(170.0, 0.0), (179.0, 1.0), (-179.0, 2.0), (-170.0, 3.0)]);
        assert_eq!(path.matches('M').count(), 2);
        assert_eq!(path.matches('L').count(), 2);
        let gap = curve_path(&[(0.0, 0.0), (f64::NAN, 0.0), (1.0, 1.0)]);
        assert_eq!(gap.matches('M').count(), 2);
        assert!(!gap.contains('L'));
    }

    fn line(body: LineBody, angle: Angle, points: Vec<Point>) -> AngularLine {
        AngularLine {
            body,
            angle,
            points,
        }
    }

    #[test]
    fn overlays_keep_engine_segments_as_separate_subpaths() {
        let crossing = line(
            LineBody::Sun,
            Angle::Asc,
            vec![
                Point(170.0, 10.0),
                Point(179.0, 12.0),
                Point(-179.0, 13.0),
                Point(-170.0, 14.0),
            ],
        );
        let rendered = overlays(&[crossing]);
        assert_eq!(rendered.len(), 1);
        assert_eq!(rendered[0].path.matches('M').count(), 2);
        assert_eq!(rendered[0].path.matches('L').count(), 2);
        assert_eq!(rendered[0].style_index, 2);
        assert!(!rendered[0].selected);
    }

    #[test]
    fn overlays_style_follows_the_angle() {
        let lines = [
            line(LineBody::Sun, Angle::Mc, vec![Point(0.0, 0.0)]),
            line(LineBody::Sun, Angle::Ic, vec![Point(0.0, 0.0)]),
            line(LineBody::Sun, Angle::Asc, vec![Point(0.0, 0.0)]),
            line(LineBody::Sun, Angle::Dsc, vec![Point(0.0, 0.0)]),
        ];
        let style_indices: Vec<usize> = overlays(&lines).iter().map(|o| o.style_index).collect();
        assert_eq!(style_indices, vec![0, 1, 2, 3]);
    }

    #[test]
    fn specimen_fixture_casts_48_lines_in_stable_order() {
        let lines = specimen_lines().expect("embedded fixture parses");
        assert_eq!(lines.len(), 48);
        assert_eq!(lines[0].body, LineBody::Sun);
        assert_eq!(lines[0].angle, Angle::Mc);
        // Culmination meridians run pole to pole in one-degree steps.
        assert_eq!(lines[0].points.len(), 181);
        for pair in lines.chunks(4) {
            assert_eq!(pair[0].body, pair[1].body);
            assert_eq!(pair[1].body, pair[2].body);
            assert_eq!(pair[2].body, pair[3].body);
        }
    }

    /// The golden SVG path snapshot: the specimen chart's rendered overlay
    /// paths must stay byte-for-byte stable, so projection or path-building
    /// drift fails here rather than on the page.
    #[test]
    fn specimen_overlay_paths_match_the_golden_snapshot() {
        const GOLDEN_JSON: &str = include_str!("assets/world-specimen-paths.json");
        #[derive(serde::Deserialize)]
        struct GoldenPath {
            body: String,
            angle: String,
            path: String,
        }
        #[derive(serde::Deserialize)]
        struct GoldenTable {
            instant: String,
            paths: Vec<GoldenPath>,
        }

        let lines = specimen_lines().expect("embedded fixture parses");
        let rendered = overlays(&lines);
        let golden: GoldenTable = serde_json::from_str(GOLDEN_JSON).expect("golden fixture parses");
        assert_eq!(golden.instant, SPECIMEN_INSTANT);
        assert_eq!(golden.paths.len(), rendered.len());
        for (overlay, expected) in rendered.iter().zip(&golden.paths) {
            assert!(
                ["mc", "ic", "asc", "dsc"].contains(&expected.angle.as_str()),
                "golden row names a real angle: {}",
                expected.angle
            );
            assert!(
                !expected.path.is_empty(),
                "golden {} path draws",
                expected.body
            );
            assert_eq!(overlay.path, expected.path, "drift for {}", expected.body);
        }
    }

    /// The specimen renders fully offline: all 48 overlays in the server-side
    /// markup, the no-JS notice in place, and the map's accessibility text.
    #[test]
    fn specimen_renders_all_overlays_without_javascript() {
        fn app() -> Element {
            rsx! { crate::astrocartography::AstrocartographyDesign {} }
        }
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        let html = dioxus_ssr::render(&dom);
        assert_eq!(html.matches("astro-map-line astro-map-line-").count(), 48);
        assert!(html.contains("Calculated specimen chart"), "{html}");
        assert!(html.contains("2000-01-01T12:00:00Z"), "{html}");
        assert!(
            html.contains("World map with planetary line overlays"),
            "{html}"
        );
    }
}
