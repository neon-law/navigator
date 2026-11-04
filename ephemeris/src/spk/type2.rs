//! SPK data type 2 — Chebyshev position coefficients (the DE440 layout).
//!
//! Each record holds a midpoint, a radius, and per-axis Chebyshev coefficient
//! blocks ordered X, Y, Z. The segment closes with four control words: INIT
//! (segment start), INTLEN (record length in seconds), RSIZE (words per
//! record), and N (record count). Positions are in kilometers.

use crate::daf::DafFile;
use crate::error::EphemerisError;
use crate::spk::conv;
use crate::spk::SpkSegment;

/// A finite whole number at least `min` (the format's control-word contract).
fn is_whole_at_least(value: f64, min: f64) -> bool {
    value.is_finite() && value >= min && value.fract() == 0.0
}

/// Evaluates a type-2 segment at TDB second `epoch_tdb`, returning kilometers
/// in ICRF.
pub(crate) fn segment_position(
    daf: &DafFile,
    segment: &SpkSegment,
    epoch_tdb: f64,
) -> Result<[f64; 3], EphemerisError> {
    // The four control words close the segment data range.
    let init = daf.f64_word(segment.data_end - 3)?;
    let intlen = daf.f64_word(segment.data_end - 2)?;
    let rsize = daf.f64_word(segment.data_end - 1)?;
    let count = daf.f64_word(segment.data_end)?;

    if !intlen.is_finite()
        || intlen <= 0.0
        || !is_whole_at_least(rsize, 2.0)
        || !is_whole_at_least(count, 1.0)
    {
        return Err(EphemerisError::MalformedSegment {
            path: daf.name().to_string(),
            target: segment.target,
            center: segment.center,
            problem: format!(
                "type-2 control words init={init} intlen={intlen} rsize={rsize} n={count}"
            ),
        });
    }
    // The guards above pin these to small whole numbers.
    let (rsize, count) = (conv::count(rsize), conv::count(count));

    let expected_words = conv::word_index(rsize * count + 4);
    if expected_words != segment.data_end - segment.data_begin + 1 {
        return Err(EphemerisError::MalformedSegment {
            path: daf.name().to_string(),
            target: segment.target,
            center: segment.center,
            problem: format!(
                "type-2 record count {count} × rsize {rsize} does not fill words {}..={}",
                segment.data_begin, segment.data_end
            ),
        });
    }

    // SPICE selects the record by truncating the elapsed interval; `t ≥ init`
    // is guaranteed by the caller's coverage check, so truncation is flooring.
    let elapsed = (epoch_tdb - init) / intlen;
    if !(0.0..=conv::size(count)).contains(&elapsed) {
        return Err(EphemerisError::OutsideCoverage {
            naif_id: segment.target,
            tdb_seconds: epoch_tdb,
            coverage_start: init,
            coverage_end: init + intlen * conv::size(count),
        });
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let record_index = elapsed as usize;
    let record_index = record_index.min(count - 1);

    let record_base = segment.data_begin + i64::try_from(record_index * rsize).unwrap_or(i64::MAX);
    let mid = daf.f64_word(record_base)?;
    let radius = daf.f64_word(record_base + 1)?;
    if !(radius.is_finite() && radius > 0.0) {
        return Err(EphemerisError::MalformedSegment {
            path: daf.name().to_string(),
            target: segment.target,
            center: segment.center,
            problem: format!("type-2 record {record_index} has radius {radius}"),
        });
    }
    let scaled_time = (epoch_tdb - mid) / radius;

    // RSIZE = 2 + 3·K: midpoint and radius plus one coefficient block per
    // X, Y, Z axis. Reject sizes that do not decompose into whole blocks.
    let coefficients_per_axis = (rsize - 2) / 3;
    if (rsize < 5) || (rsize - 2) % 3 != 0 {
        return Err(EphemerisError::MalformedSegment {
            path: daf.name().to_string(),
            target: segment.target,
            center: segment.center,
            problem: format!("type-2 rsize {rsize} is not 2 + 3·K"),
        });
    }
    let x_km = chebyshev_position(daf, record_base + 2, coefficients_per_axis, scaled_time)?;
    let y_km = chebyshev_position(
        daf,
        record_base + 2 + conv::word_index(coefficients_per_axis),
        coefficients_per_axis,
        scaled_time,
    )?;
    let z_km = chebyshev_position(
        daf,
        record_base + 2 + 2 * conv::word_index(coefficients_per_axis),
        coefficients_per_axis,
        scaled_time,
    )?;
    Ok([x_km, y_km, z_km])
}

/// Sums one axis coefficient block with the Chebyshev recurrence
/// `T_(k+1)(s) = 2s·T_k(s) − T_(k−1)(s)`, matching the DE440 writer's ordering.
fn chebyshev_position(
    daf: &DafFile,
    first_word: i64,
    coefficient_count: usize,
    s: f64,
) -> Result<f64, EphemerisError> {
    let mut t_prev = 1.0; // T_0
    let mut t_curr = s; // T_1
    let mut sum = 0.0;
    for k in 0..coefficient_count {
        let coefficient = daf.f64_word(first_word + conv::word_index(k))?;
        if k == 0 {
            sum += coefficient * t_prev;
        } else if k == 1 {
            sum += coefficient * t_curr;
        } else {
            let t_next = 2.0 * s * t_curr - t_prev;
            t_prev = t_curr;
            t_curr = t_next;
            sum += coefficient * t_curr;
        }
    }
    Ok(sum)
}

// Synthetic fixtures are built so evaluations reproduce the expected values
// bit-exactly; a tolerance would weaken these assertions.
#[allow(clippy::float_cmp)]
#[cfg(test)]
mod tests {
    use crate::spk::conv;
    use crate::spk::test_support::*;

    /// An owned synthetic type-2 record before borrowing its coefficient slices.
    type OwnedType2Record = (f64, f64, Vec<f64>, Vec<f64>, Vec<f64>);

    const DAY: f64 = 86_400.0;

    /// s = ±1 and s = 0 must hit closed-form Chebyshev sums exactly:
    /// `T_k(0)` alternates 1,0,−1,0 and `T_k(±1) = (±1)^k`.
    #[test]
    fn evaluates_chebyshev_identities() {
        // Coefficients c_k = k+1 per axis: at s=0, Σ c_k·T_k = 1 − 3 + 5 − …
        // with 6 coefficients: 1·1 + 2·0 − 3·1 + 4·0 + 5·1 + 6·0 = 3.
        let file = synthetic_type2(
            3,
            0,
            0.0,
            DAY,
            &[(
                DAY / 2.0,
                DAY / 2.0,
                &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0][..],
                &[0.0; 6][..],
                &[0.0; 6][..],
            )],
        );
        let ephemeris = crate::spk::Ephemeris::new(vec![file]);
        let center = crate::time::TdbSeconds(DAY / 2.0);
        let position = ephemeris
            .barycentric_position_id(3, center)
            .expect("covered");
        assert!(
            (position[0] - 3.0).abs() < 1e-12,
            "s=0 sum was {}",
            position[0]
        );

        // At s = 1 (t = record end): Σ (k+1)·1 = 21.
        let end = crate::time::TdbSeconds(DAY);
        let position = ephemeris.barycentric_position_id(3, end).expect("covered");
        assert!(
            (position[0] - 21.0).abs() < 1e-12,
            "s=1 sum was {}",
            position[0]
        );
    }

    /// A six-record segment resolves the correct record at every position.
    #[test]
    fn selects_the_right_record_across_the_segment() {
        // Own the coefficient vectors, then borrow them for the call so the
        // slices outlive the builder invocation.
        let records: Vec<OwnedType2Record> = (0..6)
            .map(|index| {
                let mid = (conv::size(index) + 0.5) * DAY;
                (
                    mid,
                    DAY,
                    vec![conv::size(index) * 100.0, 0.0],
                    vec![0.0; 2],
                    vec![0.0; 2],
                )
            })
            .collect();
        let borrowed: Vec<SyntheticType2Record<'_>> = records
            .iter()
            .map(|(mid, intlen, cx, cy, cz)| (*mid, *intlen, &cx[..], &cy[..], &cz[..]))
            .collect();
        let file = synthetic_type2(3, 0, 0.0, DAY, &borrowed);
        let ephemeris = crate::spk::Ephemeris::new(vec![file]);
        for index in 0..6 {
            let t = crate::time::TdbSeconds((conv::size(index) + 0.5) * DAY);
            let position = ephemeris.barycentric_position_id(3, t).expect("covered");
            assert_eq!(position[0], conv::size(index) * 100.0);
        }
    }

    /// Patches N=5 into the synthetic file's control words and confirms the
    /// word-count validation fires.
    #[test]
    fn inflated_record_count_fails_validation() {
        let mut bytes = synthetic_type2(
            3,
            0,
            0.0,
            DAY,
            &[(
                DAY / 2.0,
                DAY / 2.0,
                &[1.0; 2][..],
                &[1.0; 2][..],
                &[1.0; 2][..],
            )],
        )
        .raw_bytes_for_test();
        // Control word N is the last word of the segment data: rsize 8 (two
        // coefficients per axis) plus the four control words, starting at
        // global word 385 — so N sits at word 384 + 8 + 4 = 396. The padded
        // file's last word is NOT N.
        let last_data_word = 384 + 8 + 4;
        let offset = (last_data_word - 1) * 8;
        bytes[offset..offset + 8].copy_from_slice(&5.0_f64.to_le_bytes());
        let parsed = crate::spk::SpkFile::from_bytes(bytes, "corrupt").expect("parses");
        let ephemeris = crate::spk::Ephemeris::new(vec![parsed]);
        let error = ephemeris
            .barycentric_position_id(3, crate::time::TdbSeconds(DAY / 2.0))
            .expect_err("N must fail the word-count equation");
        assert!(matches!(
            error,
            crate::error::EphemerisError::MalformedSegment { .. }
        ));
    }
}
