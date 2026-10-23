//! Interactive SVG astrocartography specimen with explicitly synthetic lines.

use dioxus::prelude::*;

const WIDTH: f64 = 960.0;
const HEIGHT: f64 = 480.0;
// Natural Earth v5.1.2, 1:110m land. Public-domain coordinates projected to 960×480.
// Source: https://github.com/nvkelso/natural-earth-vector/blob/v5.1.2/geojson/ne_110m_land.geojson
// License: https://www.naturalearthdata.com/about/terms-of-use/
const LAND_PATH: &str = include_str!("assets/natural-earth-110m-land.path");

/// Project a validated longitude/latitude pair onto an equirectangular map.
/// Both antimeridian edges remain distinct so paths can be split at the seam.
fn project(longitude: f64, latitude: f64) -> Option<(f64, f64)> {
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

/// Start a new SVG subpath at invalid points or a crossing of the map seam.
fn curve_path(points: &[(f64, f64)]) -> String {
    use std::fmt::Write;
    let mut path = String::new();
    let mut previous: Option<f64> = None;
    for &(longitude, latitude) in points {
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

#[derive(Clone, Copy)]
struct DemoLine {
    name: &'static str,
    angle: &'static str,
    symbol: &'static str,
    description: &'static str,
    longitude: f64,
    bend: f64,
}

const LINES: [DemoLine; 4] = [
    DemoLine {
        name: "Sun",
        angle: "MC",
        symbol: "☉",
        description:
            "Visibility and purpose. What would you like your life’s work to carry forward?",
        longitude: -110.0,
        bend: 0.0,
    },
    DemoLine {
        name: "Venus",
        angle: "ASC",
        symbol: "♀",
        description: "Connection and belonging. Which places and relationships feel like home?",
        longitude: -20.0,
        bend: 90.0,
    },
    DemoLine {
        name: "Jupiter",
        angle: "DSC",
        symbol: "♃",
        description: "Growth and generosity. Where do you imagine sharing what you have learned?",
        longitude: 100.0,
        bend: -110.0,
    },
    DemoLine {
        name: "Moon",
        angle: "IC",
        symbol: "☽",
        description:
            "Roots and care. What traditions would you like the next generation to inherit?",
        longitude: 150.0,
        bend: 0.0,
    },
];

fn demo_path(line: DemoLine) -> String {
    let points: Vec<_> = (-85..=85)
        .map(|latitude| {
            let latitude = f64::from(latitude);
            let longitude = line.longitude + line.bend * latitude.to_radians().sin();
            let wrapped = (longitude + 180.0).rem_euclid(360.0) - 180.0;
            (wrapped, latitude)
        })
        .collect();
    curve_path(&points)
}

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

/// Offline world map renderer, independent of the synthetic fixture and its controls.
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

/// A self-contained design specimen. No birth details or external data are used.
#[component]
pub fn AstrocartographyDesign() -> Element {
    let mut visible = use_signal(|| [true; 4]);
    let mut selected = use_signal(|| 0_usize);
    let line = LINES[selected()];
    rsx! {
        section { id: "astrocartography", class: "astro-map-design", "aria-label": "Astrocartography design specimen",
            header {
                p { class: "astro-eyebrow", "Places / Relationships / Legacy" }
                h2 { "A world of connection" }
                p { "Explore the places that invite reflection on belonging, purpose, and the people who carry your story forward." }
            }
            p { class: "astro-demo-notice", role: "note",
                strong { "Illustrative design specimen. " }
                "These synthetic lines are not calculated from an ephemeris or birth chart. The Natural Earth world map is a geographic reference, not a navigation map."
            }
            fieldset { class: "astro-map-layers",
                legend { "Visible planetary lines" }
                for (index, item) in LINES.iter().enumerate() {
                    label {
                        input { class: "nav-checkbox", r#type: "checkbox", checked: visible()[index],
                            onchange: move |_| { visible.with_mut(|layers| layers[index] = !layers[index]); }
                        }
                        "{item.symbol} {item.name} · {item.angle}"
                    }
                }
            }
            div { class: "astro-map-frame",
                AstrocartographyMap { overlays: LINES.iter().enumerate().filter(|(index, _)| visible()[*index]).map(|(index, item)| MapOverlay { path: demo_path(*item), style_index: index, selected: selected() == index }).collect::<Vec<_>>() }
            }
            div { class: "astro-map-selection", role: "group", "aria-label": "Read about a planetary line",
                for (index, item) in LINES.iter().enumerate() {
                    button { class: "nav-btn", r#type: "button", "aria-pressed": selected() == index,
                        onclick: move |_| selected.set(index),
                        "{item.symbol} {item.name} · {item.angle}"
                    }
                }
            }
            div { class: "astro-map-detail", "aria-live": "polite",
                h3 { "{line.symbol} {line.name} · {line.angle}" }
                p { "{line.description}" }
                p { if visible()[selected()] { "This illustrative line is visible." } else { "This illustrative line is hidden. Enable its checkbox to show it." } }
            }
            p { class: "astro-map-attribution", a { href: "https://www.naturalearthdata.com/about/terms-of-use/", "Made with Natural Earth · public domain" } " · " a { href: "https://github.com/nvkelso/natural-earth-vector/blob/v5.1.2/geojson/ne_110m_land.geojson", "110m land v5.1.2" } }
            details {
                summary { "Read the map" }
                p { "MC: Midheaven. IC: lower meridian. ASC: Ascendant. DSC: Descendant. In a calculated astrocartography map, these angles mark where a celestial body is on an angle for a particular moment. Here they are labels for synthetic design fixtures only." }
                p { "Coordinates use signed degrees: negative longitude is west, positive longitude is east; positive latitude is north, negative latitude is south. Lines stop at the map edge instead of drawing across the world." }
                p { "These reflection prompts do not determine your legal wishes or recommend relocation." }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{curve_path, project};

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
}
