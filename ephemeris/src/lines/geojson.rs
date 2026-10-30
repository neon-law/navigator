//! `GeoJSON` serialization of the line set, with computation provenance.
//!
//! RFC 7946 `FeatureCollection`: one `Feature` per line, each a
//! `MultiLineString` of the [`AngularLine::segments`] pieces with
//! `[longitude, latitude]` coordinates and longitude wrapped into
//! [−180, 180) — the domain every map renderer expects. The segments are
//! already split at the antimeridian, so per-point wrapping cannot tear a
//! segment across the seam.
//!
//! The computation's provenance rides as a foreign member on the
//! collection (RFC 7946 §6.1 permits extra members): the instant, the
//! models, and the SHA-256 digests of the kernel manifest that verified
//! the load — so any exported map can state exactly what computed it.

use serde::Serialize;

use super::{wrap_longitude, AngularLine, Point};
use crate::error::EphemerisError;
use crate::loader::EphemerisManifest;
use hifitime::Epoch;

/// The ΔT model behind UT1, verbatim provenance (see [`super::deltat`]).
const DELTA_T_MODEL: &str = "Espenak & Meeus (2006) polynomials to 1955, \
Astronomical Almanac table with fourth-order Bessel interpolation \
1955-2028, Stephenson-Morrison-Hohenkerk (2016) future blend";

/// The sidereal model behind the MC/IC longitudes, verbatim provenance
/// (see [`super::sidereal`]).
const SIDEREAL_MODEL: &str = "IERS Earth Rotation Angle, IAU 2006 GMST \
polynomial, IAU 2000B equation of the equinoxes with true obliquity";

/// The lunar-node convention, when the node is among the lines.
const NODE_CONVENTION: &str = "Moon mean ascending node, Swiss Ephemeris \
SE_MEAN_NODE convention";

/// One kernel file's verifying identity, copied from the manifest.
#[derive(Debug, Serialize)]
pub struct KernelProvenance {
    /// Bare file name inside the data root, for example `de440.bsp`.
    pub name: String,
    /// SHA-256 of the exact bytes that verified at load time.
    pub sha256: String,
    /// Human-readable coverage start (TDB).
    pub coverage_start: String,
    /// Human-readable coverage end (TDB).
    pub coverage_end: String,
}

/// The models and kernel bytes behind a line set.
#[derive(Debug, Serialize)]
pub struct LineProvenance {
    /// The civil instant the lines describe, ISO-8601 UTC to the second.
    pub instant_utc: String,
    /// The time-scale model string from the verifying manifest.
    pub time_scale_model: String,
    /// The reference-frame string from the verifying manifest.
    pub reference_frame: String,
    /// The ΔT model behind UT1 and the horizon geometry.
    pub delta_t_model: &'static str,
    /// The sidereal model behind the MC/IC longitudes.
    pub sidereal_model: &'static str,
    /// The lunar-node convention, when the node is rendered.
    pub node_convention: &'static str,
    /// The verifying manifest's kernel digests, in manifest order.
    pub kernels: Vec<KernelProvenance>,
}

impl LineProvenance {
    /// Provenance for the lines computed at `utc`, carrying the manifest
    /// that verified the ephemeris load.
    #[must_use]
    pub fn new(manifest: &EphemerisManifest, utc: Epoch) -> Self {
        let kernels = manifest
            .files
            .iter()
            .map(|file| KernelProvenance {
                name: file.name.clone(),
                sha256: file.sha256.clone(),
                coverage_start: file.coverage_start.clone(),
                coverage_end: file.coverage_end.clone(),
            })
            .collect();
        Self {
            instant_utc: instant_iso_utc(utc),
            time_scale_model: manifest.time_scale_model.clone(),
            reference_frame: manifest.reference_frame.clone(),
            delta_t_model: DELTA_T_MODEL,
            sidereal_model: SIDEREAL_MODEL,
            node_convention: NODE_CONVENTION,
            kernels,
        }
    }
}

/// The civil instant as ISO-8601 UTC to the second, suffixed `Z`.
#[must_use]
fn instant_iso_utc(utc: Epoch) -> String {
    let (year, month, day, hour, minute, second, _) = utc.to_gregorian_utc();
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Serializes the line set as an RFC 7946 `FeatureCollection` with a
/// `provenance` foreign member.
///
/// # Errors
///
/// [`EphemerisError::Serialization`] when any coordinate is not finite —
/// a NaN or infinite latitude would otherwise serialize as `null` and
/// corrupt the geometry silently.
pub fn feature_collection_json(
    lines: &[AngularLine],
    provenance: &LineProvenance,
) -> Result<String, EphemerisError> {
    let features = lines
        .iter()
        .map(feature)
        .collect::<Result<Vec<Feature>, EphemerisError>>()?;
    let collection = FeatureCollection {
        kind: "FeatureCollection",
        provenance,
        features,
    };
    serde_json::to_string(&collection).map_err(|error| EphemerisError::Serialization {
        problem: error.to_string(),
    })
}

#[derive(Serialize)]
struct FeatureCollection<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    provenance: &'a LineProvenance,
    features: Vec<Feature>,
}

#[derive(Serialize)]
struct Feature {
    #[serde(rename = "type")]
    kind: &'static str,
    properties: Properties,
    geometry: Geometry,
}

#[derive(Serialize)]
struct Properties {
    body: &'static str,
    angle: &'static str,
}

#[derive(Serialize)]
struct Geometry {
    #[serde(rename = "type")]
    kind: &'static str,
    coordinates: Vec<Vec<[f64; 2]>>,
}

/// One line as a `GeoJSON` `Feature` with a `MultiLineString` geometry.
///
/// # Errors
///
/// [`EphemerisError::Serialization`] when any coordinate is not finite.
fn feature(line: &AngularLine) -> Result<Feature, EphemerisError> {
    let mut coordinates = Vec::with_capacity(line.points.len());
    for segment in line.segments() {
        coordinates.push(
            segment
                .iter()
                .map(point_coordinates)
                .collect::<Result<Vec<[f64; 2]>, EphemerisError>>()?,
        );
    }
    Ok(Feature {
        kind: "Feature",
        properties: Properties {
            body: line.body.name(),
            angle: line.angle.name(),
        },
        geometry: Geometry {
            kind: "MultiLineString",
            coordinates,
        },
    })
}

/// A vertex as an RFC 7946 `[longitude, latitude]` pair, longitude wrapped
/// into [−180, 180).
///
/// # Errors
///
/// [`EphemerisError::Serialization`] when either coordinate is not finite.
fn point_coordinates(point: &Point) -> Result<[f64; 2], EphemerisError> {
    let (lon, lat) = (point.0, point.1);
    if !lon.is_finite() || !lat.is_finite() {
        return Err(EphemerisError::Serialization {
            problem: format!("non-finite coordinate ({lon}, {lat})"),
        });
    }
    Ok([wrap_longitude(lon), lat])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lines::{Angle, LineBody};
    use serde_json::Value;

    /// A minimal manifest for provenance construction.
    fn test_manifest() -> EphemerisManifest {
        EphemerisManifest {
            version: 1,
            time_scale_model: "Espenak-Meeus/Almanac dT".to_owned(),
            reference_frame: "ICRF/J2000, IAU 2006/2000B of-date".to_owned(),
            files: vec![crate::loader::EphemerisFile {
                name: "de440.bsp".to_owned(),
                sha256: "a1b2c3".to_owned(),
                bytes: 11,
                source: "https://example.test/de440.bsp".to_owned(),
                coverage_start: "1549-12-21".to_owned(),
                coverage_end: "2650-01-25".to_owned(),
            }],
        }
    }

    /// The probe instant used across the line tests.
    fn test_epoch() -> Epoch {
        Epoch::from_gregorian_utc(2026, 10, 30, 12, 0, 0, 0)
    }

    #[test]
    fn collection_carries_features_in_order() {
        let mut lines = Vec::new();
        lines.push(super::super::vertical::vertical_line(
            LineBody::Sun,
            Angle::Mc,
            0.821_313_136,
        ));
        let (asc, _) = super::super::horizon::asc_dsc_curves(LineBody::Moon, 20.0, 0.821_313_136);
        lines.push(asc);

        let provenance = LineProvenance::new(&test_manifest(), test_epoch());
        let json = feature_collection_json(&lines, &provenance).expect("serialization");
        let value: Value = serde_json::from_str(&json).expect("valid JSON");

        assert_eq!(value["type"], "FeatureCollection");
        let features = value["features"].as_array().expect("features array");
        assert_eq!(features.len(), 2);
        assert_eq!(features[0]["properties"]["body"], "sun");
        assert_eq!(features[0]["properties"]["angle"], "mc");
        assert_eq!(features[1]["properties"]["body"], "moon");
        assert_eq!(features[1]["properties"]["angle"], "asc");
        assert_eq!(features[0]["geometry"]["type"], "MultiLineString");
    }

    #[test]
    fn coordinates_wrap_into_half_open_range() {
        let mut lines = Vec::new();
        lines.push(super::super::vertical::vertical_line(
            LineBody::Sun,
            Angle::Ic,
            185.0,
        ));
        // ASC arc for a northern declination crossing the antimeridian.
        let (asc, _) = super::super::horizon::asc_dsc_curves(LineBody::Moon, 20.0, 170.0);
        lines.push(asc);

        let provenance = LineProvenance::new(&test_manifest(), test_epoch());
        let json = feature_collection_json(&lines, &provenance).expect("serialization");
        let value: Value = serde_json::from_str(&json).expect("valid JSON");

        let meridian = &value["features"][0]["geometry"]["coordinates"];
        assert_eq!(meridian[0][0][0], -175.0);

        let asc_coords = &value["features"][1]["geometry"]["coordinates"];
        let members = asc_coords.as_array().expect("line members");
        assert_eq!(
            members.len(),
            2,
            "an arc crossing the antimeridian splits into two members"
        );
        for member in members {
            for pair in member.as_array().expect("coordinates") {
                let lon = pair[0].as_f64().expect("longitude");
                assert!(
                    (-180.0..180.0).contains(&lon),
                    "longitude {lon} out of range"
                );
            }
        }
    }

    #[test]
    fn provenance_carries_manifest_and_models() {
        let provenance = LineProvenance::new(&test_manifest(), test_epoch());
        let lines = Vec::new();
        let json = feature_collection_json(&lines, &provenance).expect("serialization");
        let value: Value = serde_json::from_str(&json).expect("valid JSON");

        let provenance_json = &value["provenance"];
        assert_eq!(provenance_json["instant_utc"], "2026-10-30T12:00:00Z");
        assert_eq!(
            provenance_json["time_scale_model"],
            "Espenak-Meeus/Almanac dT"
        );
        assert_eq!(
            provenance_json["reference_frame"],
            "ICRF/J2000, IAU 2006/2000B of-date"
        );
        let kernels = provenance_json["kernels"].as_array().expect("kernels");
        assert_eq!(kernels[0]["name"], "de440.bsp");
        assert_eq!(kernels[0]["sha256"], "a1b2c3");
        assert_eq!(kernels[0]["coverage_start"], "1549-12-21");
        assert_eq!(provenance_json["delta_t_model"], DELTA_T_MODEL);
        assert_eq!(provenance_json["sidereal_model"], SIDEREAL_MODEL);
        assert_eq!(provenance_json["node_convention"], NODE_CONVENTION);
    }

    #[test]
    fn nonfinite_coordinates_are_rejected() {
        let mut line = super::super::vertical::vertical_line(LineBody::Sun, Angle::Mc, 0.0);
        line.points[0] = Point(f64::NAN, 0.0);
        let provenance = LineProvenance::new(&test_manifest(), test_epoch());

        let error = feature_collection_json(&[line], &provenance)
            .expect_err("NaN coordinate must be rejected");
        assert!(error.to_string().contains("non-finite"));
    }
}
