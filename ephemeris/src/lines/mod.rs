//! Astrocartography line generation: the pure geometry layer.
//!
//! [`lines_for`] turns one civil instant into the full set of astrocartography
//! lines — for every rendered body, the MC and IC meridians (where the body
//! culminates and anti-culminates) and the ASC and DSC horizon curves (where
//! the body rises and sets). Points are WGS84 geodetic longitude/latitude in
//! degrees, the native vocabulary of both `GeoJSON` (`MapLibre`) and the SVG
//! fallback's projection helpers, so the output stays renderer-agnostic.
//!
//! The astronomy underneath:
//!
//! * **MC/IC** — the body culminates where local sidereal time equals its
//!   right ascension of date, so the MC meridian is λ = RA − GAST and the IC
//!   meridian sits 180° away. Straight meridians in longitude.
//! * **ASC/DSC** — the set of points where the body's altitude is exactly
//!   zero, the "horizon locus": a great circle through every longitude once,
//!   solved per longitude λ from the hour angle H = GAST + λ − RA and the
//!   declination δ: sin φ · sin δ + cos φ · cos δ · cos H = 0, so
//!   tan φ = −cos H / tan δ. The culmination meridian cuts it into the
//!   eastern ASC and western DSC semicircles, each spanning 180° of
//!   longitude between the two meridians. Sampled adaptively (finer where
//!   the curve is steep) and split at the antimeridian; an exactly
//!   equatorial body degenerates to two meridians.
//!
//! Nothing here touches the webapp, I/O, or persistence: the module consumes
//! an [`LoadedEphemeris`] and a civil instant and returns values.

pub mod deltat;
pub mod horizon;
pub mod sidereal;
pub mod vertical;

use crate::apparent::{apparent_geocentric, mean_node};
use crate::loader::LoadedEphemeris;
use crate::spk::Body;
use crate::time::utc_to_tdb;
use hifitime::Epoch;

/// A geodetic point: WGS84 longitude and latitude in degrees.
///
/// Longitude is east-positive; latitude north-positive. Tuple fields keep
/// the `GeoJSON` serialization direct (`[lon, lat]`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point(pub f64, pub f64);

/// The celestial bodies the /world page renders lines for, in stable order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineBody {
    /// The Sun.
    Sun,
    /// The Moon.
    Moon,
    /// Mercury.
    Mercury,
    /// Venus.
    Venus,
    /// Mars.
    Mars,
    /// Jupiter.
    Jupiter,
    /// Saturn.
    Saturn,
    /// Uranus.
    Uranus,
    /// Neptune.
    Neptune,
    /// Pluto.
    Pluto,
    /// 2060 Chiron.
    Chiron,
    /// The Moon's mean ascending node (North Node), Swiss Ephemeris
    /// `SE_MEAN_NODE` convention.
    MeanNode,
}

impl LineBody {
    /// The SPK body to look up, or `None` for the mean node, which is lunar
    /// orbital geometry rather than an ephemeris body.
    #[must_use]
    pub fn spk_body(self) -> Option<Body> {
        match self {
            LineBody::Sun => Some(Body::Sun),
            LineBody::Moon => Some(Body::Moon),
            LineBody::Mercury => Some(Body::Mercury),
            LineBody::Venus => Some(Body::Venus),
            LineBody::Mars => Some(Body::Mars),
            LineBody::Jupiter => Some(Body::Jupiter),
            LineBody::Saturn => Some(Body::Saturn),
            LineBody::Uranus => Some(Body::Uranus),
            LineBody::Neptune => Some(Body::Neptune),
            LineBody::Pluto => Some(Body::Pluto),
            LineBody::Chiron => Some(Body::Chiron),
            LineBody::MeanNode => None,
        }
    }

    /// The stable lowercase name used in `GeoJSON` properties and fixtures.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            LineBody::Sun => "sun",
            LineBody::Moon => "moon",
            LineBody::Mercury => "mercury",
            LineBody::Venus => "venus",
            LineBody::Mars => "mars",
            LineBody::Jupiter => "jupiter",
            LineBody::Saturn => "saturn",
            LineBody::Uranus => "uranus",
            LineBody::Neptune => "neptune",
            LineBody::Pluto => "pluto",
            LineBody::Chiron => "chiron",
            LineBody::MeanNode => "mean_node",
        }
    }
}

/// Every rendered body in stable generation order: ephemeris bodies first,
/// then the geometry-only mean node.
pub const LINE_BODIES: [LineBody; 12] = [
    LineBody::Sun,
    LineBody::Moon,
    LineBody::Mercury,
    LineBody::Venus,
    LineBody::Mars,
    LineBody::Jupiter,
    LineBody::Saturn,
    LineBody::Uranus,
    LineBody::Neptune,
    LineBody::Pluto,
    LineBody::Chiron,
    LineBody::MeanNode,
];

/// The four astrocartography angles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Angle {
    /// Midheaven: where the body culminates (hour angle 0°).
    Mc,
    /// Imum coeli: where the body anti-culminates (hour angle 180°).
    Ic,
    /// Ascendant: where the body rises (altitude zero, crossing upward).
    Asc,
    /// Descendant: where the body sets (altitude zero, crossing downward).
    Dsc,
}

impl Angle {
    /// The stable lowercase name used in `GeoJSON` properties and fixtures.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Angle::Mc => "mc",
            Angle::Ic => "ic",
            Angle::Asc => "asc",
            Angle::Dsc => "dsc",
        }
    }
}

/// One rendered line: which body and angle, and the polyline that draws it.
///
/// `points` is one or more polylines' worth of vertices after pole clipping
/// and antimeridian splitting (see [`AngularLine::segments`]).
#[derive(Clone, Debug, PartialEq)]
pub struct AngularLine {
    /// The celestial body the line belongs to.
    pub body: LineBody,
    /// The angle the line expresses.
    pub angle: Angle,
    /// The polyline vertices, WGS84 longitude/latitude in degrees. For
    /// lines that cross the antimeridian this holds the segments joined in
    /// order; [`AngularLine::segments`] recovers the drawing pieces.
    pub points: Vec<Point>,
}

impl AngularLine {
    /// The line split into drawable polylines: curves that cross the
    /// antimeridian become one segment per side, everything else is a
    /// single segment.
    #[must_use]
    pub fn segments(&self) -> Vec<Vec<Point>> {
        split_antimeridian(&self.points)
    }
}

/// Computes every astrocartography line for `utc` from the loaded ephemeris.
///
/// # Errors
///
/// Returns SPK lookup failures from the apparent-position layer (kernel
/// coverage, light-time non-convergence); the node and all geometry are
/// infallible.
pub fn lines_for(
    loaded: &LoadedEphemeris,
    utc: Epoch,
) -> Result<Vec<AngularLine>, crate::EphemerisError> {
    let t = utc_to_tdb(utc);
    let gast = sidereal::gast_degrees(utc);

    // Four lines per body: two culmination meridians and two horizon-curve
    // semicircles.
    let mut lines = Vec::with_capacity(LINE_BODIES.len() * 4);
    for body in LINE_BODIES {
        let position = match body.spk_body() {
            Some(spk_body) => apparent_geocentric(&loaded.ephemeris, spk_body, t)?,
            None => mean_node(t),
        };

        let (mc, ic) = vertical::mc_ic_longitudes(position.ra_deg, gast);
        lines.push(vertical::vertical_line(body, Angle::Mc, mc));
        lines.push(vertical::vertical_line(body, Angle::Ic, ic));

        let (asc, dsc) = horizon::asc_dsc_curves(body, position.dec_deg, mc);
        lines.push(asc);
        lines.push(dsc);
    }
    Ok(lines)
}

/// Splits a polyline at the antimeridian so no segment jumps between ±180°.
///
/// Returns one or more polylines, each drawable without a wrap seam. A
/// crossing is detected where consecutive vertices' wrapped longitudes jump
/// by more than 180° — the signature of stepping over the ±180° seam.
#[must_use]
pub fn split_antimeridian(points: &[Point]) -> Vec<Vec<Point>> {
    if points.len() < 2 {
        return vec![points.to_vec()];
    }

    let mut segments = Vec::new();
    let mut current = vec![points[0]];
    let mut prev = wrap_longitude(points[0].0);
    for &point in &points[1..] {
        let lon = wrap_longitude(point.0);
        if (lon - prev).abs() > 180.0 {
            segments.push(std::mem::take(&mut current));
        }
        current.push(point);
        prev = lon;
    }
    segments.push(current);
    segments
}

/// Wraps a longitude into \[−180, 180).
#[must_use]
pub fn wrap_longitude(lon: f64) -> f64 {
    let mut lon = lon.rem_euclid(360.0);
    if lon >= 180.0 {
        lon -= 360.0;
    }
    lon
}
