//! Civil-clock conversion and Earth rotation against a pinned external validator.

use ephemeris::{BirthTime, BirthTimeScale, LocalBirthTime};
use serde::Deserialize;

#[derive(Deserialize)]
struct Reference {
    rows: Vec<Row>,
}

#[derive(Deserialize)]
struct Row {
    year: i32,
    delta_t_seconds: f64,
    jd_tt: f64,
    jd_ut1: f64,
    gast_degrees: f64,
    sun_flags: i32,
    sun_ra_degrees: f64,
    ascendant_degrees: f64,
    midheaven_degrees: f64,
}

fn reference() -> Reference {
    serde_json::from_str(include_str!("../fixtures/swisseph_time_reference.json"))
        .expect("retained standalone reference parses")
}

fn birth_time(row: &Row) -> BirthTime {
    BirthTime::Exact(LocalBirthTime {
        local: chrono::NaiveDate::from_ymd_opt(row.year, 1, 1)
            .and_then(|date| date.and_hms_opt(0, 0, 0))
            .expect("synthetic reference calendar"),
        zone: chrono_tz::UTC,
        offset_seconds: None,
    })
}

fn angular_error(first: f64, second: f64) -> f64 {
    ((first - second + 180.0).rem_euclid(360.0) - 180.0).abs()
}

#[test]
fn civil_clock_tt_ut1_and_sidereal_readings_match_the_external_converter() {
    for row in reference().rows {
        let instant = birth_time(&row)
            .resolve()
            .expect("reference clock resolves")
            .instant()
            .expect("reference converts")
            .expect("known clock");
        let scales = instant.time_scales();
        assert!(
            (scales.julian_day_tt - row.jd_tt).abs() * 86_400.0 < 0.0001
                && (scales.model_julian_day_ut1 - row.jd_ut1).abs() * 86_400.0 < 0.0001,
            "physical TT and explicit UT1 must match the independently converted civil label"
        );
        assert!(
            (scales.delta_t_seconds - row.delta_t_seconds).abs() < 1e-7,
            "the comparison must use matched Earth-rotation data"
        );
        let manifest =
            serde_json::from_str(include_str!("../manifest.json")).expect("reference manifest");
        let provenance = ephemeris::lines::geojson::LineProvenance::for_instant(&manifest, instant);
        assert_eq!(provenance.instant_utc.is_none(), row.year < 1972);
        assert!(
            provenance
                .delta_t_reading
                .is_some_and(|estimate| (estimate.seconds - row.delta_t_seconds).abs() < 1e-7),
            "line provenance must retain the civil-reference delta-T"
        );
        let historical = row.year < 1972;
        assert_eq!(scales.julian_day_utc.is_none(), historical);
        assert_eq!(
            scales.reference.scale,
            if historical {
                BirthTimeScale::Ut1
            } else {
                BirthTimeScale::Utc
            }
        );
        assert!(
            angular_error(
                ephemeris::lines::sidereal::gast_degrees_for(instant),
                row.gast_degrees
            ) < 1.0 / 3600.0,
            "sidereal rotation must preserve the explicit historical UT1 reading"
        );
    }
}

#[test]
fn historical_civil_charts_and_lines_use_the_matched_physical_epoch() {
    use ephemeris::{
        AstrologyChartProvider, AstrologyChartRequest, EphemerisManifest, FilesystemStore,
    };
    if std::env::var_os(ephemeris::DATA_DIR_ENV).is_none() {
        eprintln!("skipping: pinned reference kernels are unavailable");
        return;
    }
    let manifest: EphemerisManifest =
        serde_json::from_str(include_str!("../manifest.json")).expect("reference manifest");
    let store = FilesystemStore::from_env().expect("reference sources");
    let loaded = ephemeris::load(&store, &manifest).expect("exact reference kernels");
    let provider = AstrologyChartProvider::load(
        &store,
        &manifest,
        std::num::NonZeroUsize::new(1).expect("positive capacity"),
    )
    .expect("exact reference provider");
    for row in reference().rows {
        assert_eq!(row.sun_flags, 2052);
        let mut request = AstrologyChartRequest::new(birth_time(&row), 0.0, 0.0);
        request.settings.houses = ephemeris::HouseSystem::Equal;
        let chart = provider.calculate(&request).expect("reference chart");
        let angles = chart.angles.as_ref().expect("exact reference angles");
        assert!(
            angular_error(angles.ascendant_deg, row.ascendant_degrees) < 1.0 / 3600.0
                && angular_error(angles.midheaven_deg, row.midheaven_degrees) < 1.0 / 3600.0,
            "returned chart angles must use the independently validated clock"
        );
        let positions = chart.positions.as_ref().expect("known-time positions");
        let lines = ephemeris::lines::lines_for_instant(
            &loaded,
            chart.instant.expect("physical chart epoch"),
        )
        .expect("reference lines");
        let name = "sun";
        let reference_ra = row.sun_ra_degrees;
        let position = positions
            .iter()
            .find(|position| position.body == name)
            .expect("requested body");
        assert!(
            angular_error(position.right_ascension_deg, reference_ra) < 1.0 / 3600.0,
            "body directions must use physical TT/TDB from the civil clock"
        );
        let line = lines
            .iter()
            .find(|line| line.body.name() == name && line.angle == ephemeris::lines::Angle::Mc)
            .expect("body culmination line");
        let longitude = line.points[0].0;
        assert!(
            angular_error(longitude, reference_ra - row.gast_degrees) < 0.01,
            "line longitude must pair the physical body state with explicit UT1"
        );
    }
}
