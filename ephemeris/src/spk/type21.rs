//! SPK data type 21 — extensible modified difference arrays (the Horizons
//! small-body layout).
//!
//! Each difference line summarizes one interval by a reference position and
//! velocity plus divided-difference tables; a chained recurrence reconstructs
//! the interpolating polynomial without ever materializing it. The segment
//! closes with N line end-epochs, a directory of every 100th epoch (for
//! segments over 100 lines), and two control words: MAXDIM and N.
//!
//! Line layout (0-based words within a `4·MAXDIM + 11` word line): `[0]` TL,
//! `[1..=MAXDIM]` G divisions, interleaved `[MAXDIM+1]` x / `[MAXDIM+2]` vx /
//! `[MAXDIM+3]` y / `[MAXDIM+4]` vy / `[MAXDIM+5]` z / `[MAXDIM+6]` vz
//! reference state, `[MAXDIM+7 .. MAXDIM+7+3·MAXDIM]` per-axis DT tables, then
//! KQMAX1 and the three per-axis KQ orders.

use crate::daf::DafFile;
use crate::error::EphemerisError;
use crate::spk::conv;
use crate::spk::SpkSegment;

/// SPICE bounds MAXDIM at 25 on the writer side (15 minimum); the reader
/// accepts anything that fits the recurrence's fixed stack arrays.
const MAX_DIMENSION: usize = 25;

/// Recurrence workspace: W indices reach 2·KQMAX1 − 5 ≤ 45.
const WORKSPACE: usize = 2 * MAX_DIMENSION;

/// Evaluates a type-21 segment at TDB second `t`, returning kilometers in ICRF.
pub(crate) fn segment_position(
    daf: &DafFile,
    segment: &SpkSegment,
    t: f64,
) -> Result<[f64; 3], EphemerisError> {
    // Trailing control words: MAXDIM, then N.
    let maxdim = daf.f64_word(segment.data_end - 1)?;
    let count = daf.f64_word(segment.data_end)?;
    let maxdim_valid = maxdim.is_finite()
        && (1.0..=conv::size(MAX_DIMENSION)).contains(&maxdim)
        && maxdim.fract() == 0.0;
    let count_valid = count.is_finite() && count >= 1.0 && count.fract() == 0.0;
    if !maxdim_valid || !count_valid {
        return Err(EphemerisError::MalformedSegment {
            path: daf.name().to_string(),
            target: segment.target,
            center: segment.center,
            problem: format!("type-21 control words maxdim={maxdim} n={count}"),
        });
    }
    // The guards above pin these to small whole numbers.
    #[allow(clippy::cast_possible_truncation)]
    let (maxdim, count) = (conv::count(maxdim), conv::count(count));

    let words_per_line = 4 * maxdim + 11;
    // The directory block holds every 100th end epoch; segments of 100 lines
    // or fewer carry none.
    let expected_words = words_per_line * count + count + count / 100 + 2;
    if i64::try_from(expected_words)
        .map_or(true, |w| w != segment.data_end - segment.data_begin + 1)
    {
        return Err(EphemerisError::MalformedSegment {
            path: daf.name().to_string(),
            target: segment.target,
            center: segment.center,
            problem: format!(
                "type-21 line count {count} × {words_per_line} + epochs + directory does not fill words {}..={}",
                segment.data_begin, segment.data_end
            ),
        });
    }

    // N line end-epochs immediately follow the lines.
    let epochs_base = segment.data_begin + conv::word_index(words_per_line * count);
    let epochs: Vec<f64> = (0..count)
        .map(|index| daf.f64_word(epochs_base + conv::word_index(index)))
        .collect::<Result<_, _>>()?;
    let line_index = select_line(&epochs, t);
    let line_base = segment.data_begin + conv::word_index(words_per_line * line_index);
    evaluate_line(daf, segment, line_base, maxdim, t)
}

/// Selects the first difference line whose end epoch is at or after `t`.
///
/// Mirrors SPICE's `lstltd`-based selection: the line covering `t` is the
/// first one ending at or after it. Segments over 100 lines carry a directory
/// of every 100th end epoch to skip the scan.
fn select_line(epochs: &[f64], t: f64) -> usize {
    let count = epochs.len();
    if count <= 100 {
        return first_index_ge(epochs, t);
    }
    let groups = count / 100;
    let group = (0..groups)
        .find(|&group| epochs[group * 100 + 99] >= t)
        .unwrap_or(groups);
    let within = first_index_ge(&epochs[group * 100..], t);
    (group * 100 + within).min(count - 1)
}

/// Index of the first end-epoch ≥ `t`; `t` past the last line is clamped to
/// the last line because the caller's coverage check already rejected it.
fn first_index_ge(epochs: &[f64], t: f64) -> usize {
    epochs
        .iter()
        .position(|&epoch| epoch >= t)
        .unwrap_or(epochs.len().saturating_sub(1))
}

/// Evaluates one difference line through the MDA recurrence.
fn evaluate_line(
    daf: &DafFile,
    segment: &SpkSegment,
    line_base: i64,
    maxdim: usize,
    t: f64,
) -> Result<[f64; 3], EphemerisError> {
    let tl = daf.f64_word(line_base)?;
    let word = |offset: i64| daf.f64_word(line_base + offset);

    // Per-axis interpolation order and difference-table size.
    let kqmax1 = word(4 * conv::word_index(maxdim) + 7)?;
    let kq = [
        word(4 * conv::word_index(maxdim) + 8)?,
        word(4 * conv::word_index(maxdim) + 9)?,
        word(4 * conv::word_index(maxdim) + 10)?,
    ];
    let valid_order = |value: f64| {
        value.is_finite() && (2.0..=conv::size(maxdim)).contains(&value) && value.fract() == 0.0
    };
    let kq_all_valid = kq
        .iter()
        .all(|value| value.is_finite() && (0.0..kqmax1).contains(value) && value.fract() == 0.0);
    if !valid_order(kqmax1) || !kq_all_valid {
        return Err(EphemerisError::MalformedDifferenceLine {
            path: daf.name().to_string(),
            target: segment.target,
            center: segment.center,
            problem: format!("kqmax1={kqmax1} kq={kq:?} maxdim={maxdim}"),
        });
    }
    // Guarded above as small whole numbers.
    #[allow(clippy::cast_possible_truncation)]
    let (kqmax1, kq) = (
        conv::count(kqmax1),
        [conv::count(kq[0]), conv::count(kq[1]), conv::count(kq[2])],
    );

    let delta = t - tl;
    let mut tp = delta;

    // FC/WC hold the chained divided-difference factors.
    let mut fc = [0.0_f64; WORKSPACE];
    let mut wc = [0.0_f64; WORKSPACE];
    fc[0] = 1.0;
    let max_j = kqmax1.saturating_sub(2);
    for j in 1..=max_j {
        let division = word(conv::word_index(j))?; // G[j-1]
        fc[j] = tp / division;
        wc[j - 1] = delta / division;
        tp = delta + division;
    }

    let mut w = [0.0_f64; WORKSPACE + 2];
    for (index, value) in w.iter_mut().enumerate().take(kqmax1) {
        *value = 1.0 / (conv::size(index) + 1.0);
    }

    let mut jx = 0_usize;
    let mut ks = kqmax1 - 1;
    let mut ks1 = ks - 1;
    while ks >= 2 {
        jx += 1;
        for j in 1..=jx {
            w[j + ks - 1] = fc[j] * w[j + ks1 - 1] - wc[j - 1] * w[j + ks - 1];
        }
        ks = ks1;
        ks1 = ks1.saturating_sub(1);
    }

    // Sum each axis difference table over its order, then add the reference
    // state: pos = refpos + Δ·(refvel + Δ·Σ DT·W).
    let mut position = [0.0_f64; 3];
    for axis in 0..3 {
        let table_base =
            conv::word_index(maxdim) + 7 + conv::word_index(axis) * conv::word_index(maxdim);
        let mut sum = 0.0;
        for j in (1..=kq[axis]).rev() {
            let coefficient = word(table_base + conv::word_index(j) - 1)?;
            sum += coefficient * w[j + ks - 1];
        }
        let refpos = word(conv::word_index(maxdim) + 1 + 2 * conv::word_index(axis))?;
        let refvel = word(conv::word_index(maxdim) + 2 + 2 * conv::word_index(axis))?;
        position[axis] = refpos + delta * (refvel + delta * sum);
    }
    Ok(position)
}

// Synthetic fixtures are built so evaluations reproduce the expected values
// bit-exactly; a tolerance would weaken these assertions.
#[allow(clippy::float_cmp)]
#[cfg(test)]
mod tests {
    use super::select_line;
    use crate::spk::conv;
    use crate::spk::test_support::*;

    const DAY: f64 = 86_400.0;

    /// With KQMAX1 = 2 the recurrence degenerates to
    /// pos = refpos + Δ·(refvel + Δ·DT(axis,1)/2); verify the closed form.
    #[test]
    fn degree_one_recurrence_has_closed_form() {
        let maxdim = 15;
        let mut line = vec![0.0_f64; 4 * maxdim + 11];
        line[0] = 0.0; // TL
        for (index, g) in line[1..=maxdim].iter_mut().enumerate() {
            *g = (conv::size(index) + 1.0) * DAY;
        }
        line[16] = 1_000_000.0; // x refpos
        line[17] = 10.0; // x refvel
        line[18] = 2_000_000.0; // y refpos
        line[19] = -5.0; // y refvel
        line[20] = 3_000_000.0; // z refpos
        line[21] = 1.0; // z refvel
        line[22] = 1.0; // DT(x,1)
        line[22 + maxdim] = 2.0; // DT(y,1)
        line[22 + 2 * maxdim] = 3.0; // DT(z,1)
        line[4 * maxdim + 7] = 2.0; // KQMAX1
        line[4 * maxdim + 8] = 1.0; // KQ x
        line[4 * maxdim + 9] = 1.0; // KQ y
        line[4 * maxdim + 10] = 1.0; // KQ z

        let file = synthetic_type21(20_002_060, 10, maxdim, &[line], &[DAY], 0.0);
        // Chiron's segment is heliocentric, so resolution through the SSB also
        // needs a Sun segment; zero coefficients park the Sun at the barycenter.
        let sun = synthetic_type2(
            10,
            0,
            0.0,
            DAY,
            &[(
                DAY / 2.0,
                DAY / 2.0,
                &[0.0; 2][..],
                &[0.0; 2][..],
                &[0.0; 2][..],
            )],
        );
        let ephemeris = crate::spk::Ephemeris::new(vec![sun, file]);

        // Δ = half a day: with KQ=1 the recurrence degenerates to
        // pos = refpos + Δ·(refvel + Δ·DT·W[1]), and W[1] = 1/2.
        let delta = DAY / 2.0;
        let expected_x = 1_000_000.0 + delta * 10.0 + delta * delta * 1.0 * 0.5;
        let expected_y = 2_000_000.0 + delta * (-5.0) + delta * delta * 2.0 * 0.5;
        let expected_z = 3_000_000.0 + delta * 1.0 + delta * delta * 3.0 * 0.5;
        let position = ephemeris
            .barycentric_position_id(20_002_060, crate::time::TdbSeconds(delta))
            .expect("covered");
        assert!((position[0] - expected_x).abs() < 1e-6, "x {}", position[0]);
        assert!((position[1] - expected_y).abs() < 1e-6, "y {}", position[1]);
        assert!((position[2] - expected_z).abs() < 1e-6, "z {}", position[2]);
    }

    /// Line selection walks 250 lines through the directory correctly.
    #[test]
    fn selects_lines_across_directory_boundaries() {
        let maxdim = 15;
        let count = 250;
        let mut lines = Vec::new();
        let mut end_epochs = Vec::new();
        for index in 0..count {
            let mut line = vec![0.0_f64; 4 * maxdim + 11];
            line[16] = conv::size(index); // refpos x marker; refvel and DT stay 0
            line[17] = 0.0; // refvel x
            line[4 * maxdim + 7] = 2.0; // KQMAX1
            line[4 * maxdim + 8] = 1.0; // KQ x
            line[4 * maxdim + 9] = 1.0; // KQ y
            line[4 * maxdim + 10] = 1.0; // KQ z
            lines.push(line);
            end_epochs.push((conv::size(index) + 1.0) * 100.0);
        }
        let file = synthetic_type21(20_002_060, 10, maxdim, &lines, &end_epochs, 0.0);
        // Chiron's segment is heliocentric; a zero-coefficient Sun segment
        // parks the Sun at the barycenter for the composition.
        let sun = synthetic_type2(
            10,
            0,
            0.0,
            25_000.0,
            &[(
                12_500.0,
                12_500.0,
                &[0.0; 2][..],
                &[0.0; 2][..],
                &[0.0; 2][..],
            )],
        );
        let ephemeris = crate::spk::Ephemeris::new(vec![sun, file]);
        // Group boundaries: line 99 ends at 10000, line 100 starts after.
        let cases = [
            (50.0, 0.0),
            (9_999.9, 99.0),
            (10_000.0, 99.0),
            (10_000.1, 100.0),
            (25_000.0, 249.0),
        ];
        for (t, expected_marker) in cases {
            let position = ephemeris
                .barycentric_position_id(20_002_060, crate::time::TdbSeconds(t))
                .expect("covered");
            assert_eq!(position[0], expected_marker, "t={t}");
        }
    }

    #[test]
    fn first_index_ge_finds_boundaries() {
        let epochs = [10.0, 20.0, 30.0];
        assert_eq!(select_line(&epochs, 5.0), 0);
        assert_eq!(select_line(&epochs, 10.0), 0);
        assert_eq!(select_line(&epochs, 10.1), 1);
        assert_eq!(select_line(&epochs, 30.0), 2);
        assert_eq!(select_line(&epochs, 31.0), 2); // clamped; caller guards
    }
}
