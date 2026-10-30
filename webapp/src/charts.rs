//! The dashboard charting layer — first-party Dioxus components that render
//! inline SVG (issue: dashboard graphing).
//!
//! Every chart is a presentational component: data arrives through props
//! shaped like `analytics::PeriodTotal`/`DimensionTotal`, so the analytics
//! server function can feed them later without a reshaping layer, and the
//! component computes nothing about where the data came from. There is no
//! JavaScript charting dependency — the SVG is complete in the
//! server-rendered HTML, following the construction `astrology_chart` proves
//! out (SSR-safe elements, no hydration-required drawing, no interop).
//!
//! # The shell
//!
//! Every chart renders inside [`ChartFrame`], which owns the accessibility
//! contract once instead of once per chart: a `<svg role="img">` carrying an
//! accessible name and `<title>`/`<desc>`, a visually hidden companion table
//! holding every label/value pair (the screen-reader path — the drawing is
//! for everyone else), and an honest empty state when there are no points.
//!
//! # Colour
//!
//! Series colors read only `--nav-chart-1`…`--nav-chart-6` (the categorical
//! ramp `tokens.css` declares for both color schemes) and `currentColor` for
//! text; no literal color appears in this module. Geometry is fluid — the
//! `viewBox` plus `preserveAspectRatio` scale the drawing, and the `.nav-chart`
//! stylesheet family (landed with the theme work) sizes the rest.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

/// How many categorical series colors `tokens.css` declares (`--nav-chart-1`
/// through `--nav-chart-6`). A series with a larger tone wraps onto the front
/// of the ramp rather than reading an undeclared variable.
const RAMP_LEN: usize = 6;

/// The gridline fractions a value axis draws, top to bottom.
const GRID_FRACTIONS: [f64; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];

/// One point in a time series — mirrors `analytics::PeriodTotal`, widened to
/// carry an ISO date label for the companion table.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct SeriesPoint {
    /// The bucket's label — `2026-07-09` for a daily bucket, `2026-07` for a
    /// monthly one, or a category name.
    pub label: String,
    pub value: f64,
}

impl SeriesPoint {
    #[must_use]
    pub fn new(label: impl Into<String>, value: f64) -> Self {
        Self {
            label: label.into(),
            value,
        }
    }
}

/// A named series — one line, one bar set, or one donut's slices. Mirrors
/// `analytics::DimensionTotal`'s shape so the same feed can build it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Series {
    /// The axis legend and companion-table column header.
    pub name: String,
    pub points: Vec<SeriesPoint>,
    /// Index into the theme's categorical ramp; resolved to
    /// `var(--nav-chart-N)` at render, wrapping modulo the ramp length.
    pub tone: usize,
}

impl Series {
    #[must_use]
    pub fn new(name: impl Into<String>, tone: usize, points: Vec<SeriesPoint>) -> Self {
        Self {
            name: name.into(),
            points,
            tone,
        }
    }
}

/// Which way a stat card's delta moved. Carried as a prop, never computed
/// from the value: whether a drop is good news is the caller's judgment, not
/// the chart's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DeltaTone {
    #[default]
    Flat,
    Up,
    Down,
}

impl DeltaTone {
    /// The BEM modifier the theme stylesheet colors.
    fn class(self) -> &'static str {
        match self {
            DeltaTone::Up => "nav-chart__delta--up",
            DeltaTone::Down => "nav-chart__delta--down",
            DeltaTone::Flat => "nav-chart__delta--flat",
        }
    }
}

/// The CSS variable a tone reads its color from — the categorical ramp,
/// wrapped so an out-of-range tone reuses the front of the palette instead
/// of reading an undeclared variable.
fn tone_var(tone: usize) -> String {
    format!("var(--nav-chart-{})", tone % RAMP_LEN + 1)
}

/// The swatch class a legend item uses for its tone.
fn swatch_class(tone: usize) -> String {
    format!(
        "nav-chart__swatch nav-chart__swatch--{}",
        tone % RAMP_LEN + 1
    )
}

/// usize → f64 without a precision-losing cast: counts here are series
/// lengths, and a chart with four billion points is not a chart this ramp
/// renders.
fn count_units(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

/// The value axis' domain: data-driven, zero included when the chart family
/// floors at zero (bars, donut fills), and padded to a nonzero width when
/// every value is equal so the scale never divides by zero. Empty input
/// yields a unit domain; non-finite values never reach here (they are
/// filtered first, see [`drawable`]).
fn value_domain(values: &[f64], include_zero: bool) -> (f64, f64) {
    if values.is_empty() {
        return (0.0, 1.0);
    }
    let mut lo = values.iter().copied().fold(f64::INFINITY, f64::min);
    let mut hi = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if include_zero {
        lo = lo.min(0.0);
        hi = hi.max(0.0);
    }
    if hi <= lo {
        // All values equal: pad around the shared value so the line sits
        // mid-chart. A zero-valued floored chart pads upward from zero so
        // the baseline stays at the domain floor.
        let pad = if lo == 0.0 { 1.0 } else { lo.abs() / 2.0 };
        if include_zero {
            hi = lo + pad;
        } else {
            lo -= pad;
            hi += pad;
        }
    }
    (lo, hi)
}

/// Map `value` from `domain` onto the pixel range `[start, end]`. A y axis
/// calls it with the pixel arguments reversed (start = bottom, end = top)
/// because SVG's y grows downward. A degenerate domain maps to the range's
/// midpoint rather than dividing by zero.
fn scaled_value(value: f64, domain: (f64, f64), start: f64, end: f64) -> f64 {
    let (lo, hi) = domain;
    let span = hi - lo;
    if span <= 0.0 || !span.is_finite() {
        return start.midpoint(end);
    }
    start + (value - lo) / span * (end - start)
}

/// The x coordinate of the `index`-th of `count` evenly spaced points across
/// `[left, right]`: endpoints inclusive, and a lone point centered rather
/// than pinned to an edge.
fn x_at(index: usize, count: usize, left: f64, right: f64) -> f64 {
    if count <= 1 {
        return left.midpoint(right);
    }
    left + (right - left) * (count_units(index) / count_units(count - 1))
}

/// The fraction of the whole one donut slice holds; a zero total yields zero
/// (an invisible slice) rather than a division by zero.
fn donut_fraction(value: f64, total: f64) -> f64 {
    if total > 0.0 {
        value / total
    } else {
        0.0
    }
}

/// Whole-value formatting for labels and the companion table: integral
/// values render without a decimal tail, everything else to two places.
fn format_value(value: f64) -> String {
    if !value.is_finite() {
        return String::new();
    }
    if value.fract().abs() < 1.0e-9 {
        return format!("{value:.0}");
    }
    format!("{value:.2}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

/// A category label shortened to `max_chars` for tight axis real estate,
/// with an ellipsis marking the cut. The companion table carries the full
/// label; the `<title>` on the rendered text repeats it as a tooltip.
fn truncate_label(label: &str, max_chars: usize) -> String {
    if label.chars().count() <= max_chars {
        return label.to_owned();
    }
    let head: String = label.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{head}…")
}

/// The drawable points of a series: the finite ones, in order. Non-finite
/// values are skipped — the astrology chart's own convention — so one bad
/// row cannot turn into a NaN coordinate.
fn drawable(points: &[SeriesPoint]) -> Vec<(&str, f64)> {
    points
        .iter()
        .filter(|point| point.value.is_finite())
        .map(|point| (point.label.as_str(), point.value))
        .collect()
}

/// The SVG path data for a polyline through the points.
fn line_path(points: &[(f64, f64)]) -> String {
    use std::fmt::Write as _;
    let mut d = String::new();
    for (index, (x, y)) in points.iter().enumerate() {
        let command = if index == 0 { 'M' } else { 'L' };
        let _ = write!(d, "{command} {x:.2} {y:.2} ");
    }
    d.trim_end().to_owned()
}

/// The SVG path data for the fill under a line: out along the points, down
/// to the baseline, and closed.
fn area_path(points: &[(f64, f64)], base_y: f64) -> String {
    use std::fmt::Write as _;
    let Some((first_x, _)) = points.first() else {
        return String::new();
    };
    let Some((last_x, _)) = points.last() else {
        return String::new();
    };
    let mut d = line_path(points);
    let _ = write!(d, " L {last_x:.2} {base_y:.2} L {first_x:.2} {base_y:.2} Z");
    d
}

/// The shared accessible shell every chart renders inside: the `<svg
/// role="img">` with its name, title, and description; the visually hidden
/// companion table carrying every label/value pair; and the honest empty
/// state when no series has a drawable point. The chart-specific drawing
/// arrives as children — SVG element namespaces come from the element type,
/// so the marks pass through the component boundary intact.
///
/// Not a chart itself: charts hand it their `viewBox` and marks.
#[component]
pub fn ChartFrame(
    /// The accessible name of the SVG, the `<title>`, and the table caption.
    title: String,
    /// The `<desc>` — one sentence on what the drawing shows.
    description: String,
    /// Every series the drawing encodes; the table renders their points.
    series: Vec<Series>,
    /// The drawing's coordinate system, e.g. `0 0 600 280`. Fluid width
    /// comes from scaling the viewBox to the container.
    view_box: String,
    /// The BEM modifier for this chart family, e.g. `nav-chart--line`.
    #[props(default)]
    variant: String,
    /// The companion table's value column header, e.g. `Visits`.
    #[props(default = "Value".to_string())]
    value_label: String,
    /// Whether to render a visible legend of the series names.
    #[props(default)]
    legend: bool,
    /// Shown in place of the drawing when no series has a drawable point.
    #[props(default = "No data for this period".to_string())]
    empty_message: String,
    /// The chart's marks — elements positioned in the `view_box`'s space.
    children: Element,
) -> Element {
    let has_points = series
        .iter()
        .any(|s| s.points.iter().any(|point| point.value.is_finite()));
    let mut chart_class = String::from("nav-chart");
    if !variant.is_empty() {
        chart_class.push(' ');
        chart_class.push_str(&variant);
    }

    rsx! {
        div { class: "{chart_class}",
            if has_points {
                svg {
                    xmlns: "http://www.w3.org/2000/svg",
                    "viewBox": "{view_box}",
                    "preserveAspectRatio": "xMidYMid meet",
                    role: "img",
                    "aria-label": "{title}",
                    class: "nav-chart__svg",
                    title { "{title}" }
                    desc { "{description}" }
                    {children}
                }
                if legend {
                    ChartLegend { series: series.clone() }
                }
                table { class: "nav-chart__table nav-visually-hidden",
                    caption { "{title}" }
                    thead {
                        tr {
                            th { scope: "col", "Series" }
                            th { scope: "col", "Label" }
                            th { scope: "col", "{value_label}" }
                        }
                    }
                    tbody {
                        for s in &series {
                            for point in &s.points {
                                tr {
                                    th { scope: "row", "{s.name}" }
                                    td { "{point.label}" }
                                    td { "{format_value(point.value)}" }
                                }
                            }
                        }
                    }
                }
            } else {
                p { class: "nav-chart__empty", role: "status", "{empty_message}" }
            }
        }
    }
}

/// The visible series legend: one swatch (colored by the same CSS variable
/// the drawing reads) and one name per series.
#[component]
fn ChartLegend(series: Vec<Series>) -> Element {
    rsx! {
        ul { class: "nav-chart__legend",
            for s in &series {
                li { class: "nav-chart__legend-item",
                    span { class: "{swatch_class(s.tone)}", "aria-hidden": "true" }
                    "{s.name}"
                }
            }
        }
    }
}

