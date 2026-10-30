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

/// A KPI tile: a label, a headline value, an optional delta versus the prior
/// period, and an optional [`Sparkline`] of the recent trend. The delta's
/// direction is a prop — a card cannot know whether down is good.
#[component]
pub fn StatCard(
    /// What the number measures, e.g. `Weekly visits`.
    label: String,
    /// The headline value, preformatted by the caller (`1,204`, `$4,200`).
    value: String,
    /// The change versus the prior period, preformatted (`+12%`).
    #[props(default)]
    delta: Option<String>,
    /// The delta's direction. Independent of the value and of the text.
    #[props(default)]
    delta_tone: DeltaTone,
    /// Recent points behind the headline, drawn as a sparkline.
    #[props(default)]
    sparkline: Option<Vec<SeriesPoint>>,
) -> Element {
    rsx! {
        div { class: "nav-chart__stat",
            p { class: "nav-chart__stat-label", "{label}" }
            p { class: "nav-chart__stat-value",
                "{value}"
                if let Some(delta) = &delta {
                    span { class: "nav-chart__delta {delta_tone.class()}", "{delta}" }
                }
            }
            match sparkline {
                Some(points) => rsx! {
                    Sparkline {
                        title: format!("{label} trend"),
                        points,
                    }
                },
                None => rsx! {},
            }
        }
    }
}

/// A multi-series time line. Optional area fill under every series (`area`).
/// The y domain scales from the data; a series with a single point renders
/// a dot, not a degenerate path.
#[component]
pub fn LineChart(
    /// Heading used for the SVG name, the `<title>`, and the table caption.
    title: String,
    /// One sentence on what the lines show, for the `<desc>`.
    description: String,
    series: Vec<Series>,
    /// Fill under every series (rendered beneath the stroke).
    #[props(default)]
    area: bool,
    /// Height in viewBox units; width is fluid via the viewBox.
    #[props(default = 280.0)]
    height: f64,
) -> Element {
    const W: f64 = 600.0;
    const LEFT: f64 = 44.0;
    const RIGHT: f64 = W - 16.0;
    const TOP: f64 = 16.0;
    const X_LABEL_MAX: usize = 12;

    let bottom = height - 28.0;
    let all_values: Vec<f64> = series
        .iter()
        .flat_map(|s| s.points.iter().map(|p| p.value))
        .filter(|value| value.is_finite())
        .collect();
    let domain = value_domain(&all_values, false);
    let (lo, hi) = domain;
    let span = hi - lo;

    rsx! {
        ChartFrame {
            title,
            description,
            series: series.clone(),
            view_box: format!("0 0 {W} {height}"),
            variant: "nav-chart--line".to_string(),
            legend: true,
            g { class: "nav-chart__grid",
                for fraction in GRID_FRACTIONS {
                    {let gy = TOP + fraction * (bottom - TOP);
                     let tick = hi - fraction * span;
                     rsx! {
                        line {
                            x1: "{LEFT}", y1: "{gy}", x2: "{RIGHT}", y2: "{gy}",
                            stroke: "var(--nav-chart-grid)", "stroke-width": "1",
                        }
                        text {
                            x: "{LEFT - 8.0}", y: "{gy}",
                            "text-anchor": "end", "dominant-baseline": "central",
                            class: "nav-chart__tick", fill: "currentColor",
                            "{format_value(tick)}"
                        }
                     }}
                }
            }
            for s in &series {
                {let points = drawable(&s.points);
                 let n = points.len();
                 let coords: Vec<(f64, f64)> = points
                     .iter()
                     .enumerate()
                     .map(|(index, (_, value))| {
                         (
                             x_at(index, n, LEFT, RIGHT),
                             scaled_value(*value, domain, bottom, TOP),
                         )
                     })
                     .collect();
                 let stroke = tone_var(s.tone);
                 rsx! {
                    if area && coords.len() > 1 {
                        path {
                            d: "{area_path(&coords, bottom)}",
                            fill: "{stroke}", "fill-opacity": "0.15", stroke: "none",
                        }
                    }
                    if coords.len() == 1 {
                        circle {
                            cx: "{coords[0].0}", cy: "{coords[0].1}", r: "3.5",
                            fill: "{stroke}", class: "nav-chart__dot",
                        }
                    } else if coords.len() > 1 {
                        path {
                            d: "{line_path(&coords)}",
                            fill: "none", stroke: "{stroke}",
                            "stroke-width": "2", "stroke-linecap": "round",
                            "stroke-linejoin": "round", class: "nav-chart__line",
                        }
                    }
                 }}
            }
            if let Some(first) = series.first() {
                {let points = drawable(&first.points);
                 let n = points.len();
                 // At most six x labels: step so crowded periods thin out.
                 let stride = n.div_ceil(6);
                 rsx! {
                    for (index, (label, _)) in points.iter().enumerate() {
                        {if index % stride == 0 || index + 1 == n {
                            let x = x_at(index, n, LEFT, RIGHT);
                            rsx! {
                                text {
                                    x: "{x}", y: "{bottom + 18.0}",
                                    "text-anchor": "middle",
                                    class: "nav-chart__tick", fill: "currentColor",
                                    title { "{label}" }
                                    "{truncate_label(label, X_LABEL_MAX)}"
                                }
                            }
                        } else { rsx! {} }}
                    }
                 }}
            }
        }
    }
}

/// A single-series bar chart, vertical by default or horizontal. The value
/// domain always includes zero, so bar lengths are honest proportions; a
/// zero bar renders at its true (zero) height. Categorical labels truncate
/// with a `<title>`; the companion table carries the full label.
#[component]
pub fn BarChart(
    /// Heading used for the SVG name, the `<title>`, and the table caption.
    title: String,
    /// One sentence on what the bars show, for the `<desc>`.
    description: String,
    series: Series,
    /// Lay the bars out horizontally (labels at the left, rows top to
    /// bottom); the height then derives from the row count.
    #[props(default)]
    horizontal: bool,
    /// Height in viewBox units for the vertical orientation.
    #[props(default = 280.0)]
    height: f64,
) -> Element {
    const W: f64 = 600.0;
    const VERTICAL_LABEL_MAX: usize = 12;
    const HORIZONTAL_LABEL_MAX: usize = 18;
    const LABEL_W: f64 = 140.0;
    const ROW_H: f64 = 36.0;
    const BAR_H: f64 = 14.0;

    let points = drawable(&series.points);
    let n = points.len();
    let values: Vec<f64> = points.iter().map(|(_, value)| *value).collect();
    let domain = value_domain(&values, true);
    let stroke = tone_var(series.tone);

    let view_box = if horizontal {
        format!("0 0 {W} {}", count_units(n) * ROW_H + 16.0)
    } else {
        format!("0 0 {W} {height}")
    };
    let variant = if horizontal {
        "nav-chart--bar-horizontal"
    } else {
        "nav-chart--bar"
    };

    rsx! {
        ChartFrame {
            title,
            description,
            series: vec![series.clone()],
            view_box,
            variant: variant.to_string(),
            for (index, (label, value)) in points.iter().enumerate() {
                {if horizontal {
                    // Horizontal rows: label column, then the bar's track.
                    let row_top = 8.0 + count_units(index) * ROW_H;
                    let track_left = LABEL_W + 12.0;
                    let track_right = W - 8.0;
                    let baseline = scaled_value(0.0, domain, track_left, track_right);
                    let end = scaled_value(*value, domain, track_left, track_right);
                    let (bar_x, bar_w) = if end >= baseline {
                        (baseline, end - baseline)
                    } else {
                        (end, baseline - end)
                    };
                    rsx! {
                        text {
                            x: "{LABEL_W}", y: "{row_top + ROW_H / 2.0}",
                            "text-anchor": "end", "dominant-baseline": "central",
                            class: "nav-chart__tick", fill: "currentColor",
                            title { "{label}" }
                            "{truncate_label(label, HORIZONTAL_LABEL_MAX)}"
                        }
                        rect {
                            x: "{bar_x}", y: "{row_top + (ROW_H - BAR_H) / 2.0}",
                            width: "{bar_w}", height: "{BAR_H}",
                            fill: "{stroke}", class: "nav-chart__bar",
                            title { "{label}: {format_value(*value)}" }
                        }
                    }
                } else {
                    // Vertical columns rising from the value axis' baseline.
                    let left = 44.0;
                    let right = W - 16.0;
                    let bottom = height - 28.0;
                    let top = 16.0;
                    let slot = if n == 0 {
                        0.0
                    } else {
                        (right - left) / count_units(n)
                    };
                    let bar_w = slot * 0.7;
                    let bar_x = left + count_units(index) * slot + slot * 0.15;
                    let baseline = scaled_value(0.0, domain, bottom, top);
                    let value_y = scaled_value(*value, domain, bottom, top);
                    let (bar_y, bar_h) = if value_y <= baseline {
                        (value_y, baseline - value_y)
                    } else {
                        (baseline, value_y - baseline)
                    };
                    rsx! {
                        rect {
                            x: "{bar_x}", y: "{bar_y}",
                            width: "{bar_w}", height: "{bar_h}",
                            fill: "{stroke}", class: "nav-chart__bar",
                            title { "{label}: {format_value(*value)}" }
                        }
                        text {
                            x: "{bar_x + bar_w / 2.0}", y: "{bottom + 18.0}",
                            "text-anchor": "middle",
                            class: "nav-chart__tick", fill: "currentColor",
                            title { "{label}" }
                            "{truncate_label(label, VERTICAL_LABEL_MAX)}"
                        }
                    }
                }}
            }
        }
    }
}

/// Proportions of a whole as donut slices, the center showing the total.
/// Slices read the categorical ramp starting at the series' own tone; a
/// zero total renders the track and the center total honestly (no slices).
#[component]
pub fn DonutChart(
    /// Heading used for the SVG name, the `<title>`, and the table caption.
    title: String,
    /// One sentence on what the proportions show, for the `<desc>`.
    description: String,
    /// The slices: one point per slice, its value the share of the total.
    series: Series,
    /// The donut's (square) side in viewBox units.
    #[props(default = 200.0)]
    size: f64,
    /// The ring's thickness in viewBox units.
    #[props(default = 24.0)]
    thickness: f64,
) -> Element {
    let center = size / 2.0;
    let radius = (size - thickness) / 2.0;
    let circumference = std::f64::consts::TAU * radius;
    let points = drawable(&series.points);
    let total: f64 = points.iter().map(|(_, value)| value).sum();

    rsx! {
        ChartFrame {
            title,
            description,
            series: vec![series.clone()],
            view_box: format!("0 0 {size} {size}"),
            variant: "nav-chart--donut".to_string(),
            legend: true,
            circle {
                cx: "{center}", cy: "{center}", r: "{radius}",
                fill: "none", stroke: "var(--nav-chart-grid)",
                "stroke-width": "{thickness}",
            }
            {let mut start = 0.0;
             rsx! {
                for (index, (label, value)) in points.iter().enumerate() {
                    {let fraction = donut_fraction(*value, total);
                     if fraction <= 0.0 { rsx! {} } else {
                        let dash = fraction * circumference;
                        let angle = start * 360.0 - 90.0;
                        start += fraction;
                        rsx! {
                            circle {
                                cx: "{center}", cy: "{center}", r: "{radius}",
                                fill: "none",
                                stroke: "{tone_var(series.tone + index)}",
                                "stroke-width": "{thickness}",
                                "stroke-dasharray": "{dash} {circumference}",
                                transform: "rotate({angle} {center} {center})",
                                class: "nav-chart__slice",
                                title { "{label}: {format_value(*value)}" }
                            }
                        }
                     }}
                }
             }}
            text {
                x: "{center}", y: "{center}",
                "text-anchor": "middle", "dominant-baseline": "central",
                "font-size": "24", fill: "currentColor",
                class: "nav-chart__total",
                "{format_value(total)}"
            }
        }
    }
}

/// A tiny inline trend: no axes, no labels, no grid. Rendered inside
/// [`StatCard`] but able to stand alone; a single point renders a dot.
#[component]
pub fn Sparkline(
    /// The accessible name — what the trend measures.
    title: String,
    points: Vec<SeriesPoint>,
    /// Which ramp tone to stroke.
    #[props(default)]
    tone: usize,
    /// Width in viewBox units.
    #[props(default = 96.0)]
    width: f64,
    /// Height in viewBox units.
    #[props(default = 28.0)]
    height: f64,
) -> Element {
    const PAD: f64 = 3.0;

    let points = drawable(&points);
    let n = points.len();
    let values: Vec<f64> = points.iter().map(|(_, value)| *value).collect();
    let domain = value_domain(&values, false);
    let stroke = tone_var(tone);
    let coords: Vec<(f64, f64)> = points
        .iter()
        .enumerate()
        .map(|(index, (_, value))| {
            (
                x_at(index, n, PAD, width - PAD),
                scaled_value(*value, domain, height - PAD, PAD),
            )
        })
        .collect();

    rsx! {
        ChartFrame {
            title: title.clone(),
            description: "The recent trend, drawn without axes. The companion table carries the values.".to_string(),
            series: vec![Series::new(title.clone(), tone, points.iter().map(|(label, value)| SeriesPoint::new(*label, *value)).collect())],
            view_box: format!("0 0 {width} {height}"),
            variant: "nav-chart--sparkline".to_string(),
            value_label: "Trend".to_string(),
            if coords.len() == 1 {
                circle {
                    cx: "{coords[0].0}", cy: "{coords[0].1}", r: "2.5",
                    fill: "{stroke}", class: "nav-chart__dot",
                }
            } else if coords.len() > 1 {
                path {
                    d: "{line_path(&coords)}",
                    fill: "none", stroke: "{stroke}",
                    "stroke-width": "1.5", "stroke-linecap": "round",
                    "stroke-linejoin": "round", class: "nav-chart__line",
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(component: Element) -> String {
        dioxus_ssr::render_element(component)
    }

    fn close(actual: f64, expected: f64) -> bool {
        (actual - expected).abs() < 1.0e-9
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            close(actual, expected),
            "expected {actual} to be within 1e-9 of {expected}"
        );
    }

    // -- fixtures ---------------------------------------------------------

    fn line_series() -> Vec<Series> {
        vec![
            Series::new(
                "Signed retainers",
                0,
                vec![
                    SeriesPoint::new("2026-07-07", 4.0),
                    SeriesPoint::new("2026-07-08", 8.0),
                    SeriesPoint::new("2026-07-09", 6.0),
                ],
            ),
            Series::new(
                "Open matters",
                1,
                vec![
                    SeriesPoint::new("2026-07-07", 2.0),
                    SeriesPoint::new("2026-07-08", 5.0),
                    SeriesPoint::new("2026-07-09", 9.0),
                ],
            ),
        ]
    }

    fn category_series() -> Series {
        Series::new(
            "Matters by practice",
            0,
            vec![
                SeriesPoint::new("Estate planning", 12.0),
                SeriesPoint::new("Tenant defense", 7.0),
                SeriesPoint::new("Immigration", 3.0),
            ],
        )
    }

    // -- scale helpers ----------------------------------------------------

    #[test]
    fn value_domain_includes_zero_only_when_floored() {
        let values = [5.0, 8.0];
        assert_eq!(value_domain(&values, true), (0.0, 8.0));
        assert_eq!(value_domain(&values, false), (5.0, 8.0));
    }

    #[test]
    fn value_domain_handles_empty_input() {
        assert_eq!(value_domain(&[], true), (0.0, 1.0));
    }

    #[test]
    fn value_domain_pads_a_single_point() {
        let (lo, hi) = value_domain(&[4.0], false);
        assert_close(lo, 2.0);
        assert_close(hi, 6.0);
        // A floored chart keeps the baseline at zero and pads upward.
        assert_eq!(value_domain(&[4.0], true), (0.0, 4.0));
    }

    #[test]
    fn value_domain_pads_all_equal_values() {
        let (lo, hi) = value_domain(&[3.0, 3.0, 3.0], false);
        assert!(hi > lo, "domain must have width to avoid dividing by zero");
        assert_close(lo, 1.5);
        assert_close(hi, 4.5);
        let (lo, hi) = value_domain(&[0.0, 0.0], true);
        assert_eq!((lo, hi), (0.0, 1.0));
    }

    #[test]
    fn scaled_value_maps_domain_onto_the_range() {
        let domain = (0.0, 8.0);
        assert_close(scaled_value(0.0, domain, 0.0, 100.0), 0.0);
        assert_close(scaled_value(8.0, domain, 0.0, 100.0), 100.0);
        assert_close(scaled_value(4.0, domain, 0.0, 100.0), 50.0);
        // A y axis reverses the pixel arguments (SVG y grows downward): the
        // domain maximum lands at the top.
        assert_close(scaled_value(8.0, domain, 280.0, 40.0), 40.0);
    }

    #[test]
    fn scaled_value_maps_a_degenerate_domain_to_the_midpoint() {
        assert_close(scaled_value(5.0, (5.0, 5.0), 0.0, 100.0), 50.0);
    }

    #[test]
    fn x_at_centers_a_lone_point_and_spreads_the_rest() {
        assert_close(x_at(0, 1, 10.0, 30.0), 20.0);
        assert_close(x_at(0, 4, 10.0, 30.0), 10.0);
        assert_close(x_at(3, 4, 10.0, 30.0), 30.0);
        assert_close(x_at(1, 4, 10.0, 30.0), 10.0 + 20.0 / 3.0);
    }

    #[test]
    fn donut_fraction_handles_a_zero_total() {
        assert_close(donut_fraction(4.0, 8.0), 0.5);
        assert_close(donut_fraction(4.0, 0.0), 0.0);
        assert_close(donut_fraction(0.0, 0.0), 0.0);
    }

    #[test]
    fn format_value_omits_trailing_zeros() {
        assert_eq!(format_value(4.0), "4");
        assert_eq!(format_value(4.5), "4.5");
        assert_eq!(format_value(4.25), "4.25");
        assert_eq!(format_value(1234.0), "1234");
        assert_eq!(format_value(-2.5), "-2.5");
        assert_eq!(format_value(0.1 + 0.2), "0.3");
    }

    #[test]
    fn truncate_label_marks_the_cut() {
        assert_eq!(truncate_label("short", 12), "short");
        assert_eq!(truncate_label("exactly-12-c", 12), "exactly-12-c");
        assert_eq!(truncate_label("exactly-12-ch", 12), "exactly-12-…");
        let cut = truncate_label("Probate administration", 12);
        assert_eq!(cut.chars().count(), 12);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn tone_var_wraps_onto_the_ramp() {
        assert_eq!(tone_var(0), "var(--nav-chart-1)");
        assert_eq!(tone_var(5), "var(--nav-chart-6)");
        assert_eq!(tone_var(6), "var(--nav-chart-1)");
        assert_eq!(tone_var(7), "var(--nav-chart-2)");
    }

    #[test]
    fn drawable_skips_non_finite_values() {
        let points = vec![
            SeriesPoint::new("a", 4.0),
            SeriesPoint::new("b", f64::NAN),
            SeriesPoint::new("c", 6.0),
        ];
        let drawn = drawable(&points);
        assert_eq!(drawn.len(), 2);
        assert_eq!(drawn[0].0, "a");
        assert_eq!(drawn[1].0, "c");
    }

    // -- rendered output ---------------------------------------------------

    #[test]
    fn line_chart_ssrs_with_accessible_name_and_companion_table() {
        let html = render(rsx! {
            LineChart {
                title: "Visits by day".to_string(),
                description: "Daily visits for the trailing week.".to_string(),
                series: line_series(),
                area: false,
                height: 280.0,
            }
        });
        assert!(html.contains("role=\"img\""), "{html}");
        assert!(html.contains("aria-label=\"Visits by day\""), "{html}");
        assert!(html.contains("<title"), "{html}");
        assert!(html.contains("<desc"), "{html}");
        assert!(html.contains("nav-visually-hidden"), "{html}");
        assert!(html.contains("<table"), "{html}");
        assert!(html.contains("Signed retainers"), "{html}");
        assert!(html.contains("Open matters"), "{html}");
        assert!(html.contains("2026-07-09"), "{html}");
        assert!(html.contains("<path"), "{html}");
        assert!(html.contains("nav-chart__legend"), "{html}");
        // Series colors are theme variables, never literals.
        assert!(!html.contains("stroke=\"#"), "{html}");
        assert!(!html.contains("NaN"), "{html}");
    }

    #[test]
    fn line_chart_without_points_shows_the_empty_state() {
        let html = render(rsx! {
            LineChart {
                title: "Visits by day".to_string(),
                description: "Daily visits for the trailing week.".to_string(),
                series: vec![Series::new("Empty", 0, vec![])],
                area: false,
                height: 280.0,
            }
        });
        assert!(!html.contains("<svg"), "{html}");
        assert!(html.contains("role=\"status\""), "{html}");
        assert!(html.contains("No data for this period"), "{html}");
    }

    #[test]
    fn line_chart_single_point_renders_a_dot_not_a_path() {
        let html = render(rsx! {
            LineChart {
                title: "Visits by day".to_string(),
                description: "Daily visits for the trailing week.".to_string(),
                series: vec![Series::new(
                    "Signed retainers",
                    0,
                    vec![SeriesPoint::new("2026-07-09", 4.0)],
                )],
                area: false,
                height: 280.0,
            }
        });
        assert!(html.contains("<circle"), "{html}");
        assert!(!html.contains("<path"), "{html}");
        assert!(!html.contains("NaN"), "{html}");
    }

    #[test]
    fn line_chart_all_equal_values_render_finite_geometry() {
        let html = render(rsx! {
            LineChart {
                title: "Flat line".to_string(),
                description: "Same value every day.".to_string(),
                series: vec![Series::new(
                    "Flat",
                    0,
                    vec![
                        SeriesPoint::new("2026-07-07", 3.0),
                        SeriesPoint::new("2026-07-08", 3.0),
                        SeriesPoint::new("2026-07-09", 3.0),
                    ],
                )],
                area: false,
                height: 280.0,
            }
        });
        assert!(html.contains("<path"), "{html}");
        assert!(!html.contains("NaN"), "{html}");
        assert!(!html.contains("Infinity"), "{html}");
    }

    #[test]
    fn line_chart_area_flag_fills_under_the_series() {
        let html = render(rsx! {
            LineChart {
                title: "Visits by day".to_string(),
                description: "Daily visits for the trailing week.".to_string(),
                series: line_series(),
                area: true,
                height: 280.0,
            }
        });
        assert!(html.contains("fill-opacity=\"0.15\""), "{html}");
    }

    #[test]
    fn bar_chart_ssrs_bars_with_full_labels_in_the_table() {
        let html = render(rsx! {
            BarChart {
                title: "Matters by practice".to_string(),
                description: "Open matters grouped by practice area.".to_string(),
                series: category_series(),
                horizontal: false,
                height: 280.0,
            }
        });
        assert!(html.contains("role=\"img\""), "{html}");
        assert!(html.contains("<rect"), "{html}");
        assert!(html.contains("nav-chart__bar"), "{html}");
        assert!(html.contains("<table"), "{html}");
        assert!(html.contains("Estate planning"), "{html}");
        assert!(html.contains("Matters by practice"), "{html}");
    }

    #[test]
    fn bar_chart_truncates_long_axis_labels() {
        let html = render(rsx! {
            BarChart {
                title: "Matters".to_string(),
                description: "Open matters by practice.".to_string(),
                series: Series::new(
                    "Matters by practice",
                    0,
                    vec![SeriesPoint::new("Probate administration", 4.0)],
                ),
                horizontal: false,
                height: 280.0,
            }
        });
        // The axis shows the cut; the companion table carries the full label.
        assert!(html.contains("…"), "{html}");
        assert!(html.contains("Probate administration"), "{html}");
    }

    #[test]
    fn bar_chart_horizontal_sizes_the_view_box_from_the_row_count() {
        let html = render(rsx! {
            BarChart {
                title: "Matters by practice".to_string(),
                description: "Open matters grouped by practice area.".to_string(),
                series: category_series(),
                horizontal: true,
                height: 280.0,
            }
        });
        // Three rows at 36 units plus the 16-unit margin.
        assert!(html.contains("viewBox=\"0 0 600 124\""), "{html}");
        assert!(html.contains("<rect"), "{html}");
    }

    #[test]
    fn bar_chart_zero_values_render_zero_height_bars() {
        let html = render(rsx! {
            BarChart {
                title: "Matters".to_string(),
                description: "Open matters by practice.".to_string(),
                series: Series::new(
                    "Matters by practice",
                    0,
                    vec![
                        SeriesPoint::new("Estate planning", 0.0),
                        SeriesPoint::new("Tenant defense", 0.0),
                    ],
                ),
                horizontal: false,
                height: 280.0,
            }
        });
        assert!(html.contains("<rect"), "{html}");
        assert!(!html.contains("NaN"), "{html}");
        assert!(!html.contains("Infinity"), "{html}");
    }

    #[test]
    fn donut_chart_ssrs_slices_center_total_and_legend() {
        let html = render(rsx! {
            DonutChart {
                title: "Matters by practice".to_string(),
                description: "Share of open matters by practice area.".to_string(),
                series: Series::new(
                    "Matters by practice",
                    0,
                    vec![
                        SeriesPoint::new("Estate planning", 3.0),
                        SeriesPoint::new("Tenant defense", 1.0),
                        SeriesPoint::new("Immigration", 2.0),
                    ],
                ),
                size: 200.0,
                thickness: 24.0,
            }
        });
        assert!(html.contains("role=\"img\""), "{html}");
        // One dashed circle per nonzero slice, over the track.
        assert_eq!(html.matches("stroke-dasharray").count(), 3, "{html}");
        assert!(html.contains("nav-chart__slice"), "{html}");
        // The center shows the total.
        assert!(html.contains("nav-chart__total"), "{html}");
        assert!(html.contains("6</text>"), "{html}");
        assert!(html.contains("nav-chart__legend"), "{html}");
        assert!(!html.contains("NaN"), "{html}");
    }

    #[test]
    fn donut_chart_zero_total_renders_the_track_honestly() {
        let html = render(rsx! {
            DonutChart {
                title: "Matters by practice".to_string(),
                description: "Share of open matters by practice area.".to_string(),
                series: Series::new(
                    "Matters by practice",
                    0,
                    vec![SeriesPoint::new("Estate planning", 0.0)],
                ),
                size: 200.0,
                thickness: 24.0,
            }
        });
        assert!(html.contains("nav-chart__total"), "{html}");
        assert!(html.contains("0</text>"), "{html}");
        assert_eq!(html.matches("stroke-dasharray").count(), 0, "{html}");
        assert!(!html.contains("NaN"), "{html}");
    }

    #[test]
    fn donut_chart_single_slice_spans_the_whole_ring() {
        let html = render(rsx! {
            DonutChart {
                title: "Matters by practice".to_string(),
                description: "Share of open matters by practice area.".to_string(),
                series: Series::new(
                    "Matters by practice",
                    0,
                    vec![SeriesPoint::new("Estate planning", 5.0)],
                ),
                size: 200.0,
                thickness: 24.0,
            }
        });
        assert!(html.contains("stroke-dasharray"), "{html}");
        assert!(!html.contains("NaN"), "{html}");
    }

    #[test]
    fn sparkline_ssrs_named_svg_with_companion_table() {
        let html = render(rsx! {
            Sparkline {
                title: "Weekly visits".to_string(),
                points: vec![
                    SeriesPoint::new("2026-07-05", 1.0),
                    SeriesPoint::new("2026-07-06", 3.0),
                    SeriesPoint::new("2026-07-07", 2.0),
                ],
                tone: 0,
                width: 96.0,
                height: 28.0,
            }
        });
        assert!(html.contains("role=\"img\""), "{html}");
        assert!(html.contains("aria-label=\"Weekly visits\""), "{html}");
        assert!(html.contains("<title"), "{html}");
        assert!(html.contains("viewBox=\"0 0 96 28\""), "{html}");
        assert!(html.contains("<path"), "{html}");
        assert!(html.contains("<table"), "{html}");
    }

    #[test]
    fn stat_card_renders_label_value_and_delta_tone() {
        let html = render(rsx! {
            StatCard {
                label: "Weekly visits".to_string(),
                value: "1,204".to_string(),
                delta: Some("+12%".to_string()),
                delta_tone: DeltaTone::Up,
                sparkline: None,
            }
        });
        assert!(html.contains("Weekly visits"), "{html}");
        assert!(html.contains("1,204"), "{html}");
        assert!(html.contains("+12%"), "{html}");
        assert!(html.contains("nav-chart__delta--up"), "{html}");
        // No sparkline, no SVG: a stat card is not a chart shell by itself.
        assert!(!html.contains("<svg"), "{html}");
    }

    #[test]
    fn stat_card_embeds_a_sparkline_when_given_points() {
        let html = render(rsx! {
            StatCard {
                label: "Weekly visits".to_string(),
                value: "1,204".to_string(),
                delta: None,
                delta_tone: DeltaTone::Flat,
                sparkline: Some(vec![
                    SeriesPoint::new("2026-07-05", 1.0),
                    SeriesPoint::new("2026-07-06", 3.0),
                ]),
            }
        });
        assert!(html.contains("<svg"), "{html}");
        assert!(html.contains("role=\"img\""), "{html}");
        assert!(html.contains("Weekly visits trend"), "{html}");
    }
}
