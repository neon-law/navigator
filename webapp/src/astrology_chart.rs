//! Interactive, synthetic chart specimen for the portal design gallery.

use dioxus::prelude::*;

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

#[derive(Clone, Copy)]
struct FixturePosition {
    name: &'static str,
    glyph: &'static str,
    longitude: f64,
}

// Deliberately invented positions: no date, birth location, or ephemeris claim.
const BODIES: [FixturePosition; 11] = [
    FixturePosition {
        name: "Sun",
        glyph: "☉",
        longitude: 15.0,
    },
    FixturePosition {
        name: "Moon",
        glyph: "☽",
        longitude: 135.0,
    },
    FixturePosition {
        name: "Mercury",
        glyph: "☿",
        longitude: 42.0,
    },
    FixturePosition {
        name: "Venus",
        glyph: "♀",
        longitude: 75.0,
    },
    FixturePosition {
        name: "Mars",
        glyph: "♂",
        longitude: 195.0,
    },
    FixturePosition {
        name: "Jupiter",
        glyph: "♃",
        longitude: 255.0,
    },
    FixturePosition {
        name: "Saturn",
        glyph: "♄",
        longitude: 310.0,
    },
    FixturePosition {
        name: "Uranus",
        glyph: "♅",
        longitude: 330.0,
    },
    FixturePosition {
        name: "Neptune",
        glyph: "♆",
        longitude: 285.0,
    },
    FixturePosition {
        name: "Pluto",
        glyph: "♇",
        longitude: 165.0,
    },
    FixturePosition {
        name: "North Node",
        glyph: "☊",
        longitude: 110.0,
    },
];

/// A named body and its ecliptic longitude in degrees. Non-finite values are omitted.
#[derive(Clone, Debug, PartialEq)]
pub struct BodyPosition {
    pub name: String,
    pub glyph: String,
    pub longitude: f64,
}

fn fixture_positions() -> Vec<BodyPosition> {
    BODIES
        .iter()
        .map(|body| BodyPosition {
            name: body.name.to_owned(),
            glyph: body.glyph.to_owned(),
            longitude: body.longitude,
        })
        .collect()
}

fn aspect(first: Longitude, second: Longitude) -> Option<&'static str> {
    let separation = first.separation(second);
    [
        (0.0, "Conjunction"),
        (60.0, "Sextile"),
        (90.0, "Square"),
        (120.0, "Trine"),
        (180.0, "Opposition"),
    ]
    .into_iter()
    .find_map(|(angle, name)| ((separation - angle).abs() <= 3.0).then_some(name))
}

/// Gallery specimen with explicitly synthetic data and provenance labels.
#[component]
pub fn AstrologyChartDesign() -> Element {
    rsx! { AstrologyChart { positions: fixture_positions(), illustrative: true } }
}

/// Render supplied ecliptic positions with keyboard-operable body selection.
/// This component draws geometry; it performs no astronomical calculation.
#[component]
pub fn AstrologyChart(
    positions: Vec<BodyPosition>,
    #[props(default)] illustrative: bool,
) -> Element {
    let positions: Vec<_> = positions
        .into_iter()
        .filter_map(|mut body| {
            let longitude = Longitude::new(body.longitude)?;
            body.longitude = longitude.0;
            Some(body)
        })
        .collect();
    let mut selected = use_signal(|| 0_usize);
    let mut show_aspects = use_signal(|| true);
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
            div { class: "astrology-chart__layout",
                svg { xmlns: "http://www.w3.org/2000/svg", "viewBox": "0 0 600 600",
                    role: "img", "aria-labelledby": "chart-wheel-title chart-wheel-description",
                    class: "astrology-chart__wheel",
                    title { id: "chart-wheel-title", if illustrative { "Illustrative zodiac wheel" } else { "Zodiac wheel" } }
                    desc { id: "chart-wheel-description", "Supplied planetary positions on twelve equal zodiac sectors. Lines show major aspects within three degrees. The adjacent table provides all positions in text. Houses and ascendant are not calculated." }
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
                    if show_aspects() {
                        for (first_index, first) in positions.iter().enumerate() {
                            for second in positions.iter().skip(first_index + 1) {
                                {let a = Longitude(first.longitude);
                                 let b = Longitude(second.longitude);
                                 let (x1,y1) = a.point(174.0);
                                 let (x2,y2) = b.point(174.0);
                                 rsx! {
                                    if let Some(name) = aspect(a,b) {
                                        line { x1: "{x1}", y1: "{y1}", x2: "{x2}", y2: "{y2}", stroke: "var(--nav-color-primary, currentColor)", "stroke-opacity": ".35", "stroke-width": "1.5",
                                            title { "{first.name}–{second.name}: {name}" }
                                        }
                                    }
                                 }}
                            }
                        }
                    }
                    for (index, body) in positions.iter().enumerate() {
                        {let (x,y) = Longitude(body.longitude).point(198.0);
                         rsx! {
                            circle { cx: "{x}", cy: "{y}", r: "21", fill: "var(--nav-color-surface)", stroke: "currentColor", "stroke-width": if selected() == index { "2" } else { "0" } }
                            text { x: "{x}", y: "{y}", "text-anchor": "middle", "dominant-baseline": "central", "font-size": "25", fill: "currentColor", "{body.glyph}" }
                         }}
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
                        thead { tr { th { scope: "col", "Body" } th { scope: "col", "Position" } } }
                        tbody {
                            for body in positions.iter() {
                                tr { th { scope: "row", "{body.glyph} {body.name}" } td { "{position_label(body.longitude)}" } }
                            }
                        }
                    }
                    if show_aspects() {
                        details {
                            summary { "Aspect pairs · within 3° of the exact angle" }
                            ul {
                                for (first_index, first) in positions.iter().enumerate() {
                                    for second in positions.iter().skip(first_index + 1) {
                                        if let Some(name) = aspect(Longitude(first.longitude), Longitude(second.longitude)) {
                                            li { "{first.name}–{second.name}: {name}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    p { class: "nav-text-muted", "Equal zodiac sectors are not astronomical constellation boundaries. Houses, birth time, and geographic coordinates are not modeled in this specimen." }
                }
            }
        }
    }
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
            aspect(Longitude(359.0), Longitude(1.0)),
            Some("Conjunction")
        );
        assert_eq!(aspect(Longitude(350.0), Longitude(80.0)), Some("Square"));
        assert_eq!(aspect(Longitude(0.0), Longitude(124.0)), None);
        assert_eq!(aspect(Longitude(0.0), Longitude(180.0)), Some("Opposition"));
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
            },
        );
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    #[test]
    fn ssr_exposes_synthetic_labels_positions_and_default_selection() {
        let mut dom = VirtualDom::new(AstrologyChartDesign);
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
        }]);
        assert!(html.contains("No finite positions to display."));
        assert!(!html.contains("Invalid fixture"));
        assert!(!html.contains("NaN"));
    }
}
