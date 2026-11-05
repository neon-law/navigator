//! Provider-backed and illustrative SVG chart rendering with accessible controls.

pub mod calculated;

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

const SIGNS: [&str; 12] = [
    "Aries",
    "Taurus",
    "Gemini",
    "Cancer",
    "Leo",
    "Virgo",
    "Libra",
    "Scorpio",
    "Sagittarius",
    "Capricorn",
    "Aquarius",
    "Pisces",
];
const GLYPHS: [&str; 12] = [
    "♈", "♉", "♊", "♋", "♌", "♍", "♎", "♏", "♐", "♑", "♒", "♓",
];

/// A finite ecliptic longitude, normalized to the half-open interval [0, 360).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Longitude(f64);

impl Longitude {
    fn new(degrees: f64) -> Option<Self> {
        degrees.is_finite().then(|| {
            let normalized = degrees.rem_euclid(360.0);
            // Floating-point remainder can round a tiny negative input up to 360.
            Self(if normalized >= 360.0 { 0.0 } else { normalized })
        })
    }

    fn zodiac(self) -> (&'static str, f64) {
        let sign = SIGNS
            .into_iter()
            .zip(0_u32..12)
            .find_map(|(sign, index)| (self.0 < f64::from(index + 1) * 30.0).then_some(sign))
            .unwrap_or("Pisces");
        (sign, self.0 % 30.0)
    }

    fn separation(self, other: Self) -> f64 {
        let difference = (self.0 - other.0).abs();
        difference.min(360.0 - difference)
    }

    fn point(self, radius: f64) -> (f64, f64) {
        let radians = (180.0 - self.0).to_radians();
        (
            300.0 + radius * radians.cos(),
            300.0 - radius * radians.sin(),
        )
    }
}

/// A named body and its ecliptic longitude in degrees. Non-finite values are omitted.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct BodyPosition {
    pub name: String,
    pub glyph: String,
    pub longitude: f64,
    /// Signed longitude rate; absent for an illustrative position.
    pub longitude_speed_deg_per_day: Option<f64>,
}

const ASPECTS: [(f64, &str); 5] = [
    (0.0, "Conjunction"),
    (60.0, "Sextile"),
    (90.0, "Square"),
    (120.0, "Trine"),
    (180.0, "Opposition"),
];

fn aspect(
    first: Longitude,
    second: Longitude,
    orb: f64,
    enabled: [bool; 5],
) -> Option<&'static str> {
    if !orb.is_finite() || !(0.0..=10.0).contains(&orb) {
        return None;
    }
    let separation = first.separation(second);
    ASPECTS
        .into_iter()
        .zip(enabled)
        .find_map(|((angle, name), selected)| {
            (selected && (separation - angle).abs() <= orb).then_some(name)
        })
}

// The provider inventory has at most twelve bodies. At radius 198 a fourteen-degree
// separation leaves more than 44 pixels between label centers. Move symbols only;
// dots, leader lines and the table preserve the supplied longitudes.
fn label_longitudes(positions: &[BodyPosition]) -> Vec<Option<Longitude>> {
    let mut occupied = Vec::<Longitude>::new();
    positions
        .iter()
        .map(|body| {
            let actual = Longitude::new(body.longitude)?;
            let placement = (0..=180)
                .flat_map(|offset| {
                    let offset = f64::from(offset);
                    [actual.0 + offset, actual.0 - offset]
                })
                .filter_map(Longitude::new)
                .find(|candidate| {
                    occupied
                        .iter()
                        .all(|other| candidate.separation(*other) >= 14.0)
                });
            if let Some(longitude) = placement {
                occupied.push(longitude);
            }
            placement
        })
        .collect()
}

/// Gallery reference at a fixed synthetic instant, with verified source provenance.
#[component]
pub fn AstrologyChartDesign() -> Element {
    match calculated::specimen_chart() {
        Ok(chart) => rsx! {
            p { role: "note", strong { "Calculated reference · fixed synthetic instant" }
                " — 2000-01-01 at 12:00 UTC, 0° north, 0° east. This reference is not a person's reading. No birth information is collected here."
            }
            calculated::CalculatedAstrologyChart { chart }
        },
        Err(_) => rsx! { p { role: "alert", "The calculated chart reference is unavailable." } },
    }
}

/// Render supplied ecliptic positions with keyboard-operable body selection.
/// This component draws geometry; it performs no astronomical calculation.
#[component]
pub fn AstrologyChart(
    positions: Vec<BodyPosition>,
    #[props(default)] illustrative: bool,
    #[props(default)] house_cusps: Option<[f64; 12]>,
) -> Element {
    let positions: Vec<_> = positions
        .into_iter()
        .filter_map(|mut body| {
            let longitude = Longitude::new(body.longitude)?;
            body.longitude = longitude.0;
            Some(body)
        })
        .collect();
    let house_cusps = house_cusps.filter(|cusps| cusps.iter().all(|cusp| cusp.is_finite()));
    let label_positions = label_longitudes(&positions);
    let mut selected = use_signal(|| 0_usize);
    let mut show_aspects = use_signal(|| true);
    let mut aspect_orb = use_signal(|| "3".to_owned());
    let orb = aspect_orb()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && (0.0..=10.0).contains(value));
    let mut enabled_aspects = use_signal(|| [true; 5]);
    let current = positions.get(selected()).or_else(|| positions.first());
    rsx! {
        section { id: "astrology-chart", class: "astrology-chart", "aria-labelledby": "astrology-chart-heading",
            h2 { id: "astrology-chart-heading", "Chart wheel" }
            p { class: "nav-text-muted", "A circular view of the sky for a conversation about life and legacy." }
            if illustrative {
                p { role: "note", strong { "Illustrative design · synthetic positions" }
                    " — This is not a birth chart or an ephemeris reading. No birth information is collected."
                }
            }
            div { class: "astrology-chart__controls",
                for (index, body) in positions.iter().enumerate() {
                    button { class: "nav-btn", r#type: "button", "aria-pressed": "{selected() == index}",
                        onclick: move |_| selected.set(index),
                        "{body.glyph} {body.name}"
                    }
                }
                button { class: "nav-btn", r#type: "button", "aria-pressed": "{show_aspects()}",
                    onclick: move |_| show_aspects.set(!show_aspects()),
                    if show_aspects() { "Hide aspects" } else { "Show aspects" }
                }
            }
            fieldset { class: "astrology-chart__aspect-controls", disabled: !show_aspects(),
                legend { "Selected aspects" }
                label { r#for: "chart-aspect-orb", "Aspect orb (degrees)" }
                input { id: "chart-aspect-orb", r#type: "number", min: "0", max: "10", step: "any", value: "{aspect_orb()}",
                    "aria-describedby": "chart-aspect-help",
                    "aria-invalid": "{orb.is_none()}",
                    oninput: move |event| aspect_orb.set(event.value())
                }
                if orb.is_none() {
                    p { role: "status", "Enter an aspect orb from 0° to 10°. Aspect results are unavailable until the value is valid." }
                }
                div { class: "astrology-chart__aspect-types",
                    for (index, (_, name)) in ASPECTS.into_iter().enumerate() {
                        label {
                            input { r#type: "checkbox", checked: enabled_aspects()[index],
                                oninput: move |event| {
                                    let mut enabled = enabled_aspects();
                                    enabled[index] = event.checked();
                                    enabled_aspects.set(enabled);
                                }
                            }
                            " {name}"
                        }
                    }
                }
                p { id: "chart-aspect-help", class: "nav-text-muted", "The orb is the allowed difference from the selected aspect's exact angle, from 0° to 10°." }
            }
            div { class: "astrology-chart__layout",
                svg { xmlns: "http://www.w3.org/2000/svg", "viewBox": "0 0 600 600",
                    role: "img", "aria-labelledby": "chart-wheel-title chart-wheel-description",
                    class: "astrology-chart__wheel",
                    title { id: "chart-wheel-title", if illustrative { "Illustrative zodiac wheel" } else { "Zodiac wheel" } }
                    desc { id: "chart-wheel-description",
                        "Supplied planetary positions on twelve equal zodiac sectors. Symbols move for readability; dots and leader lines mark their exact positions. The adjacent tables provide positions, motion and supplied house cusps in text."
                        if house_cusps.is_none() { " Houses and ascendant are not calculated." }
                    }
                    circle { cx: "300", cy: "300", r: "275", fill: "var(--nav-color-surface)", stroke: "currentColor", "stroke-opacity": ".35" }
                    circle { cx: "300", cy: "300", r: "224", fill: "none", stroke: "currentColor", "stroke-opacity": ".35" }
                    circle { cx: "300", cy: "300", r: "174", fill: "none", stroke: "currentColor", "stroke-opacity": ".15" }
                    for (index, glyph) in (0_u32..12).zip(GLYPHS) {
                        {let angle = Longitude(f64::from(index) * 30.0);
                         let (x1,y1) = angle.point(224.0);
                         let (x2,y2) = angle.point(275.0);
                         let (tx,ty) = Longitude(angle.0 + 15.0).point(250.0);
                         rsx! {
                            line { x1: "{x1}", y1: "{y1}", x2: "{x2}", y2: "{y2}", stroke: "currentColor", "stroke-opacity": ".35" }
                            text { x: "{tx}", y: "{ty}", "text-anchor": "middle", "dominant-baseline": "central", "font-size": "24", fill: "currentColor", "{glyph}" }
                         }}
                    }
                    if let Some(cusps) = house_cusps {
                        for (index, cusp) in cusps.iter().enumerate() {
                            {let house = index + 1;
                             let longitude = Longitude(cusp.rem_euclid(360.0));
                             let (x1,y1) = longitude.point(145.0);
                             let (x2,y2) = longitude.point(224.0);
                             let (tx,ty) = longitude.point(158.0);
                             rsx! {
                                g { "data-house": "{house}",
                                    line { x1: "{x1}", y1: "{y1}", x2: "{x2}", y2: "{y2}", stroke: "currentColor", "stroke-opacity": ".4", "stroke-dasharray": "3 3" }
                                    text { x: "{tx}", y: "{ty}", "text-anchor": "middle", "dominant-baseline": "central", "font-size": "16", fill: "currentColor", "{house}" }
                                }
                             }}
                        }
                    }
                    if show_aspects() {
                        for (first_index, first) in positions.iter().enumerate() {
                            for second in positions.iter().skip(first_index + 1) {
                                {let a = Longitude(first.longitude);
                                 let b = Longitude(second.longitude);
                                 let (x1,y1) = a.point(174.0);
                                 let (x2,y2) = b.point(174.0);
                                 rsx! {
                                    if let Some(name) = orb.and_then(|orb| aspect(a,b,orb,enabled_aspects())) {
                                        line { x1: "{x1}", y1: "{y1}", x2: "{x2}", y2: "{y2}", stroke: "var(--nav-color-primary, currentColor)", "stroke-opacity": ".35", "stroke-width": "1.5", "data-aspect": "{name}",
                                            title { "{first.name}–{second.name}: {name}" }
                                        }
                                    }
                                 }}
                            }
                        }
                    }
                    for (index, body) in positions.iter().enumerate() {
                        if let Some(label) = label_positions.get(index).copied().flatten() {
                            {let (x,y) = label.point(198.0);
                             let (actual_x, actual_y) = Longitude(body.longitude).point(224.0);
                             rsx! {
                                line { x1: "{actual_x}", y1: "{actual_y}", x2: "{x}", y2: "{y}", stroke: "currentColor", "stroke-opacity": ".45" }
                                circle { cx: "{actual_x}", cy: "{actual_y}", r: "3", fill: "currentColor" }
                                circle { cx: "{x}", cy: "{y}", r: "21", fill: "var(--nav-color-surface)", stroke: "currentColor", "stroke-width": if selected() == index { "2" } else { "0" } }
                                text { x: "{x}", y: "{y}", "text-anchor": "middle", "dominant-baseline": "central", "font-size": "25", fill: "currentColor", "{body.glyph}" }
                                if body.longitude_speed_deg_per_day.is_some_and(|rate| rate.is_finite() && rate < 0.0) {
                                    text { x: "{x + 12.0}", y: "{y + 14.0}", "font-size": "14", fill: "currentColor", "aria-hidden": "true", "℞" }
                                }
                             }}
                        }
                    }
                    text { x: "300", y: "291", "text-anchor": "middle", fill: "currentColor", "font-size": "20", "LIFE & LEGACY" }
                    text { x: "300", y: "320", "text-anchor": "middle", fill: "currentColor", "font-size": "12", if illustrative { "ILLUSTRATIVE SKY" } else { "ECLIPTIC POSITIONS" } }
                }
                div {
                    p { "aria-live": "polite",
                        if let Some(current) = current {
                            strong { "{current.glyph} {current.name}" }
                            " · {position_label(current.longitude)}"
                        } else { "No finite positions to display." }
                    }
                    p { "Select a body to locate it on the wheel. Aspect lines connect positions at selected angular relationships; they do not make predictions." }
                    table { class: "astrology-chart__table",
                        caption { if illustrative { "Synthetic ecliptic longitudes · tropical zodiac" } else { "Ecliptic longitudes · tropical zodiac" } }
                        thead { tr { th { scope: "col", "Body" } th { scope: "col", "Position" } th { scope: "col", "Motion" } th { scope: "col", "Rate (°/day)" } } }
                        tbody {
                            for body in positions.iter() {
                                tr {
                                    th { scope: "row", "{body.glyph} {body.name}" }
                                    td { "{position_label(body.longitude)}" }
                                    td { "{motion_label(body.longitude_speed_deg_per_day)}" }
                                    td { "{rate_label(body.longitude_speed_deg_per_day)}" }
                                }
                            }
                        }
                    }
                    if let Some(cusps) = house_cusps {
                        table { class: "astrology-chart__table",
                            caption { "House cusp longitudes" }
                            thead { tr { th { scope: "col", "House" } th { scope: "col", "Position" } } }
                            tbody {
                                for (index, cusp) in cusps.iter().enumerate() {
                                    tr { th { scope: "row", "{index + 1}" } td { "{position_label(*cusp)}" } }
                                }
                            }
                        }
                    }
                    if show_aspects() && orb.is_some() {
                        details {
                            summary { "Aspect pairs · within {aspect_orb()}° of the exact angle" }
                            ul {
                                for (first_index, first) in positions.iter().enumerate() {
                                    for second in positions.iter().skip(first_index + 1) {
                                        if let Some(name) = orb.and_then(|orb| aspect(Longitude(first.longitude), Longitude(second.longitude), orb, enabled_aspects())) {
                                            li { "{first.name}–{second.name}: {name}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    p { class: "nav-text-muted",
                        "Equal zodiac sectors are not astronomical constellation boundaries."
                        if illustrative { " Houses, birth time, and geographic coordinates are not modeled in this specimen." }
                        else if house_cusps.is_none() { " No house cusps were supplied for this view." }
                    }
                }
            }
        }
    }
}

fn motion_label(rate: Option<f64>) -> &'static str {
    match rate.filter(|rate| rate.is_finite()) {
        Some(rate) if rate < 0.0 => "Retrograde",
        Some(rate) if rate > 0.0 => "Direct",
        Some(_) => "Stationary",
        None => "Unavailable",
    }
}

fn rate_label(rate: Option<f64>) -> String {
    rate.filter(|rate| rate.is_finite())
        .map_or_else(|| "Unavailable".to_owned(), |rate| format!("{rate:.4}"))
}

fn position_label(degrees: f64) -> String {
    Longitude::new(degrees).map_or_else(
        || "Unavailable".to_owned(),
        |longitude| {
            let (sign, degree) = longitude.zodiac();
            format!("{degree:.1}° {sign} ({:.1}° longitude)", longitude.0)
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_positions() -> Vec<BodyPosition> {
        let longitudes = [
            15.0, 135.0, 42.0, 75.0, 195.0, 255.0, 310.0, 330.0, 285.0, 165.0, 110.0,
        ];
        ephemeris::lines::LINE_BODIES
            .into_iter()
            .filter(|body| *body != ephemeris::lines::LineBody::Chiron)
            .zip(longitudes)
            .map(|(body, longitude)| BodyPosition {
                name: crate::ephemeris_labels::body_display_name(body).to_owned(),
                glyph: crate::ephemeris_labels::body_symbol(body).to_owned(),
                longitude,
                longitude_speed_deg_per_day: None,
            })
            .collect()
    }

    #[test]
    fn longitude_normalizes_and_rejects_non_finite_inputs() {
        assert_eq!(Longitude::new(-15.0), Some(Longitude(345.0)));
        assert_eq!(Longitude::new(720.0), Some(Longitude(0.0)));
        for degrees in [-f64::EPSILON, -f64::MIN_POSITIVE, -f64::from_bits(1)] {
            let longitude = Longitude::new(degrees).expect("finite input");
            assert!((0.0..360.0).contains(&longitude.0));
            assert_eq!(longitude.zodiac(), ("Aries", 0.0));
        }
        assert!(Longitude::new(f64::NAN).is_none());
        assert!(Longitude::new(f64::INFINITY).is_none());
    }

    #[test]
    fn zodiac_sector_boundaries_are_half_open() {
        assert_eq!(Longitude(29.9).zodiac(), ("Aries", 29.9));
        assert_eq!(Longitude(30.0).zodiac(), ("Taurus", 0.0));
        assert_eq!(Longitude(359.0).zodiac(), ("Pisces", 29.0));
    }

    #[test]
    fn aspects_use_shortest_angle_across_zero_and_bounded_orb() {
        assert!((Longitude(359.0).separation(Longitude(1.0)) - 2.0).abs() < 1e-9);
        assert_eq!(
            aspect(Longitude(359.0), Longitude(1.0), 3.0, [true; 5]),
            Some("Conjunction")
        );
        assert_eq!(
            aspect(Longitude(350.0), Longitude(80.0), 3.0, [true; 5]),
            Some("Square")
        );
        assert_eq!(
            aspect(Longitude(0.0), Longitude(124.0), 3.0, [true; 5]),
            None
        );
        assert_eq!(
            aspect(Longitude(0.0), Longitude(180.0), 3.0, [true; 5]),
            Some("Opposition")
        );
    }

    #[test]
    fn aspect_orbs_and_selected_relationships_control_the_actual_pairs() {
        assert_eq!(
            aspect(Longitude(359.0), Longitude(1.0), 1.0, [true; 5]),
            None
        );
        assert_eq!(
            aspect(Longitude(359.0), Longitude(1.0), 2.0, [true; 5]),
            Some("Conjunction")
        );
        assert_eq!(
            aspect(
                Longitude(0.0),
                Longitude(90.0),
                3.0,
                [true, true, false, true, true]
            ),
            None
        );
        assert_eq!(
            aspect(Longitude(0.0), Longitude(0.0), f64::NAN, [true; 5]),
            None
        );
    }

    #[test]
    fn aspect_controls_explain_the_orb_and_expose_keyboard_inputs() {
        let html = render(fixture_positions());
        assert!(
            html.contains("Aspect orb") && html.contains("Selected aspects"),
            "the aspect relation and allowed separation need explicit controls"
        );
        assert!(html.contains("type=\"checkbox\"") && html.contains("max=\"10\""));
    }

    #[test]
    fn wheel_places_zero_left_and_ninety_above() {
        let (x, y) = Longitude(0.0).point(100.0);
        assert!((x - 200.0).abs() < 1e-9 && (y - 300.0).abs() < 1e-9);
        let (x, y) = Longitude(90.0).point(100.0);
        assert!((x - 300.0).abs() < 1e-9 && (y - 200.0).abs() < 1e-9);
    }

    fn render(positions: Vec<BodyPosition>) -> String {
        let mut dom = VirtualDom::new_with_props(
            AstrologyChart,
            AstrologyChartProps {
                positions,
                illustrative: false,
                house_cusps: None,
            },
        );
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    #[test]
    fn ssr_exposes_synthetic_labels_positions_and_default_selection() {
        let mut dom = VirtualDom::new_with_props(
            AstrologyChart,
            AstrologyChartProps {
                positions: fixture_positions(),
                illustrative: true,
                house_cusps: None,
            },
        );
        dom.rebuild_in_place();
        let html = dioxus_ssr::render(&dom);
        assert!(html.contains("Illustrative design · synthetic positions"));
        assert!(html.contains("aria-labelledby=\"chart-wheel-title chart-wheel-description\""));
        assert!(html.contains("Synthetic ecliptic longitudes · tropical zodiac"));
        assert!(html.contains("15.0° Aries (15.0° longitude)"));
        assert!(html.contains("aria-pressed=\"true\""));
        assert!(html.contains("<table"));
        assert!(html.contains("Sun–Moon: Trine"));
    }

    #[test]
    fn crowded_wheel_symbols_do_not_overlap_across_the_zodiac_seam() {
        let positions = (0..12)
            .map(|index| BodyPosition {
                name: format!("Synthetic body {index}"),
                glyph: "☉".to_owned(),
                longitude: if index % 2 == 0 { 359.9 } else { 0.1 },
                longitude_speed_deg_per_day: None,
            })
            .collect();
        let html = render(positions);
        let centers: Vec<_> = html
            .split("<circle")
            .skip(1)
            .filter_map(|element| {
                let tag = element.split('>').next()?;
                if !tag.contains("r=\"21\"") {
                    return None;
                }
                let coordinate = |name: &str| -> Option<f64> {
                    tag.split_once(&format!("{name}=\""))?
                        .1
                        .split('"')
                        .next()?
                        .parse()
                        .ok()
                };
                Some((coordinate("cx")?, coordinate("cy")?))
            })
            .collect();
        assert_eq!(centers.len(), 12);
        for (index, first) in centers.iter().enumerate() {
            for second in centers.iter().skip(index + 1) {
                let distance = (first.0 - second.0).hypot(first.1 - second.1);
                assert!(
                    distance >= 44.0,
                    "rendered body symbols need room for their circles and borders"
                );
            }
        }
        assert!(
            html.contains("359.9° longitude") && html.contains("0.1° longitude"),
            "display label placement must preserve the actual longitudes in the text alternative"
        );
    }

    #[test]
    fn supplied_retrograde_motion_is_visible_in_the_wheel_and_text() {
        let html = render(vec![BodyPosition {
            name: "Mercury".to_owned(),
            glyph: "☿".to_owned(),
            longitude: 42.0,
            longitude_speed_deg_per_day: Some(-0.25),
        }]);
        assert!(
            html.contains("Retrograde") && html.contains("℞"),
            "signed decreasing longitude needs both a visible marker and a text equivalent"
        );
        assert!(
            html.contains("-0.2500"),
            "the signed rate must remain available in text"
        );
    }

    #[test]
    fn supplied_house_cusps_have_an_equivalent_text_table() {
        let mut dom = VirtualDom::new_with_props(
            AstrologyChart,
            AstrologyChartProps {
                positions: fixture_positions(),
                illustrative: false,
                house_cusps: Some(std::array::from_fn(|index| {
                    f64::from(u32::try_from(index).expect("twelve houses")) * 30.0
                })),
            },
        );
        dom.rebuild_in_place();
        let html = dioxus_ssr::render(&dom);
        assert!(
            html.contains("House cusp longitudes"),
            "calculated cusps need a labeled text alternative"
        );
        assert!(
            html.contains("data-house=\"12\""),
            "every supplied cusp needs wheel geometry"
        );
        assert!(
            !html.contains("Houses and ascendant are not calculated"),
            "calculated geometry must carry truthful labels"
        );
    }

    #[test]
    fn gallery_uses_a_fixed_reference_with_recorded_calculation_provenance() {
        let mut dom = VirtualDom::new(AstrologyChartDesign);
        dom.rebuild_in_place();
        let html = dioxus_ssr::render(&dom);
        assert!(
            html.contains("Calculated reference · fixed synthetic instant"),
            "the gallery needs an explicit calculated-reference state"
        );
        assert!(
            html.contains("chart-v3") && html.contains("House cusp longitudes"),
            "the reference must exercise the provider-backed component"
        );
    }

    #[test]
    fn supplied_positions_are_not_labeled_synthetic() {
        let html = render(fixture_positions());
        assert!(!html.contains("synthetic"));
        assert!(!html.contains("Synthetic"));
        assert!(!html.contains("ILLUSTRATIVE SKY"));
        assert!(html.contains("Ecliptic longitudes · tropical zodiac"));
    }

    #[test]
    fn ssr_handles_empty_and_non_finite_inputs() {
        assert!(render(Vec::new()).contains("No finite positions to display."));
        let html = render(vec![BodyPosition {
            name: "Invalid fixture".to_owned(),
            glyph: "?".to_owned(),
            longitude: f64::NAN,
            longitude_speed_deg_per_day: None,
        }]);
        assert!(html.contains("No finite positions to display."));
        assert!(!html.contains("Invalid fixture"));
        assert!(!html.contains("NaN"));
    }
}
