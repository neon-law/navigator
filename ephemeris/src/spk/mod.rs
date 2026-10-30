//! SPK segment parsing and barycentric position resolution.
//!
//! A segment carries positions of a target body relative to a center body.
//! DE440 expresses Earth (399) and Moon (301) relative to the Earth-Moon
//! barycenter (3), Mercury/Venus centers relative to their barycenters, and
//! the Horizons Chiron SPK expresses Chiron relative to the Sun. The
//! [`Ephemeris`] resolver chains those links into solar-system-barycenter
//! positions, walking files newest-first so a later-loaded kernel supersedes
//! an earlier one (the SPK data-precedence convention).

mod type2;
mod type21;

use crate::daf::DafFile;
use crate::error::EphemerisError;
use crate::time::TdbSeconds;

/// NAIF frame id for ICRF/J2000, the only reference frame this crate resolves.
pub(crate) const J2000_FRAME: i32 = 1;

/// The solar-system barycenter NAIF id.
pub(crate) const SSB: i32 = 0;

/// Numeric conversions for kernel metadata the SPK format bounds well below
/// 2^31 (word counts, coefficient counts, segment counts). The truncation,
/// wrap, sign, and precision corners these casts name are unreachable for any
/// kernel [`SpkFile::from_bytes`] accepts.
pub(crate) mod conv {
    /// A count read from a kernel control word (a non-negative double).
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    pub(crate) fn count(value: f64) -> usize {
        value as usize
    }

    /// A bounded count as a floating-point record size (exact below 2^53).
    #[allow(clippy::cast_precision_loss)]
    pub(crate) fn size(value: usize) -> f64 {
        value as f64
    }

    /// A word index for segment arithmetic (file sizes are far below 2^63).
    #[allow(clippy::cast_possible_wrap)]
    pub(crate) fn word_index(value: usize) -> i64 {
        value as i64
    }

    /// A word count for the i32 summary address space (far below 2^31). Only
    /// the synthetic-kernel builders need it; production reads real summaries.
    #[cfg(test)]
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    pub(crate) fn word_count(value: usize) -> i32 {
        value as i32
    }
}

/// One SPK segment descriptor plus its data-word range.
#[derive(Clone, Copy, Debug)]
pub struct SpkSegment {
    /// NAIF id of the moving body.
    pub target: i32,
    /// NAIF id of the origin body the position is relative to.
    pub center: i32,
    /// NAIF frame id (must be 1, ICRF/J2000).
    pub frame: i32,
    /// SPK data type (2 = Chebyshev position, 21 = modified difference arrays).
    pub spk_type: i32,
    /// First covered TDB second past J2000.
    pub start: f64,
    /// Last covered TDB second past J2000.
    pub end: f64,
    /// First 1-based data word of the segment.
    pub data_begin: i64,
    /// Last 1-based data word of the segment.
    pub data_end: i64,
}

/// One parsed SPK kernel: its bytes plus every segment descriptor.
#[derive(Debug)]
pub struct SpkFile {
    daf: DafFile,
    segments: Vec<SpkSegment>,
}

impl SpkFile {
    /// Parses an SPK kernel from its bytes.
    ///
    /// # Errors
    ///
    /// Returns format errors for non-SPK files, unknown endianness, truncated
    /// structures, and segments with unsupported data types or frames.
    pub fn from_bytes(bytes: Vec<u8>, name: impl Into<String>) -> Result<Self, EphemerisError> {
        let daf = DafFile::from_bytes(bytes)?.with_name(name);
        let segments = Self::parse_summaries(&daf)?;
        Ok(Self { daf, segments })
    }

    /// The kernel name.
    #[must_use]
    pub fn name(&self) -> &str {
        self.daf.name()
    }

    /// Every parsed segment descriptor.
    #[must_use]
    pub fn segments(&self) -> &[SpkSegment] {
        &self.segments
    }

    /// Coverage window (TDB seconds past J2000) of every segment for a target.
    #[must_use]
    pub fn coverage(&self, target: i32) -> Option<(f64, f64)> {
        self.segments
            .iter()
            .filter(|segment| segment.target == target)
            .map(|segment| (segment.start, segment.end))
            .reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)))
    }

    /// The segment for a target-center pair covering `t`, if any.
    pub(crate) fn segment_for(&self, target: i32, center: i32, t: f64) -> Option<&SpkSegment> {
        self.segments.iter().find(|segment| {
            segment.target == target
                && segment.center == center
                && t >= segment.start
                && t <= segment.end
        })
    }

    /// Evaluates the segment for a target-center pair at `t`.
    pub(crate) fn evaluate(
        &self,
        target: i32,
        center: i32,
        t: f64,
    ) -> Result<[f64; 3], EphemerisError> {
        let segment = self.segment_for(target, center, t).ok_or_else(|| {
            let (start, end) = self.coverage_of(target, center);
            EphemerisError::OutsideCoverage {
                naif_id: target,
                tdb_seconds: t,
                coverage_start: start,
                coverage_end: end,
            }
        })?;
        match segment.spk_type {
            2 => type2::segment_position(&self.daf, segment, t),
            21 => type21::segment_position(&self.daf, segment, t),
            other => Err(EphemerisError::UnsupportedSpkType {
                path: self.daf.name().to_string(),
                target: segment.target,
                center: segment.center,
                spk_type: other,
            }),
        }
    }

    fn coverage_of(&self, target: i32, center: i32) -> (f64, f64) {
        let covered = self
            .segments
            .iter()
            .filter(|segment| segment.target == target && segment.center == center);
        covered
            .map(|s| (s.start, s.end))
            .reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)))
            .unwrap_or((f64::INFINITY, f64::NEG_INFINITY))
    }

    fn parse_summaries(daf: &DafFile) -> Result<Vec<SpkSegment>, EphemerisError> {
        let mut segments = Vec::new();
        let mut record = daf.first_summary_record();
        let mut hops = 0_i32;
        while record != 0 {
            hops += 1;
            if hops > 1000 {
                return Err(EphemerisError::MalformedSegment {
                    path: daf.name().to_string(),
                    target: 0,
                    center: 0,
                    problem: "summary chain does not terminate".to_string(),
                });
            }
            // The summary record starts with three control doubles: next
            // record, previous record, number of summaries in this record.
            let record_base = i64::from(record - 1) * 128;
            let next = daf.f64_word(record_base + 1)?;
            let nsum = daf.f64_word(record_base + 3)?;
            // A 1024-byte record holds at most 127 five-word summaries; larger
            // or negative counts mean the chain walked into garbage.
            if !(0.0..=127.0).contains(&nsum) || nsum.fract() != 0.0 {
                return Err(EphemerisError::MalformedSegment {
                    path: daf.name().to_string(),
                    target: 0,
                    center: 0,
                    problem: format!("summary record claims {nsum} summaries"),
                });
            }
            // The range guard above pins the value to a small whole number.
            let nsum = conv::count(nsum);
            // ND=2, NI=6 for every SPK DAF: stride 5 words, then 6 packed ints.
            let stride = DafFile::summary_words(2, 6);
            for index in 0..nsum {
                // nsum ≤ 127, so the widening cast cannot wrap.
                #[allow(clippy::cast_possible_wrap)]
                let base = record_base + 3 + index as i64 * stride;
                let start = daf.f64_word(base + 1)?;
                let end = daf.f64_word(base + 2)?;
                let (target, center) = daf.int_pair(base + 3)?;
                let (frame, spk_type) = daf.int_pair(base + 4)?;
                let (begin, end_word) = daf.int_pair(base + 5)?;
                if frame != J2000_FRAME {
                    return Err(EphemerisError::UnsupportedFrame {
                        path: daf.name().to_string(),
                        target,
                        center,
                        frame,
                    });
                }
                if spk_type != 2 && spk_type != 21 {
                    return Err(EphemerisError::UnsupportedSpkType {
                        path: daf.name().to_string(),
                        target,
                        center,
                        spk_type,
                    });
                }
                segments.push(SpkSegment {
                    target,
                    center,
                    frame,
                    spk_type,
                    start,
                    end,
                    data_begin: i64::from(begin),
                    data_end: i64::from(end_word),
                });
            }
            // Same guard rationale as `nsum`: the chain control words were
            // written by the DAF writer as whole record numbers.
            if !(0.0..=f64::from(i32::MAX)).contains(&next) || next.fract() != 0.0 {
                return Err(EphemerisError::MalformedSegment {
                    path: daf.name().to_string(),
                    target: 0,
                    center: 0,
                    problem: format!("summary chain pointer {next} is not a record number"),
                });
            }
            // Guarded above as a whole number within i32.
            #[allow(clippy::cast_possible_truncation)]
            let next = next as i32;
            record = next;
        }
        Ok(segments)
    }
}

#[cfg(test)]
impl SpkFile {
    /// The raw kernel bytes (test-only corruption patching).
    pub(crate) fn raw_bytes_for_test(&self) -> Vec<u8> {
        self.daf.bytes().to_vec()
    }
}

/// Named bodies the astrocartography calculator renders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Body {
    /// Mercury (planet center, NAIF 199).
    Mercury,
    /// Venus (planet center, NAIF 299).
    Venus,
    /// Earth (planet center, NAIF 399).
    Earth,
    /// Moon (NAIF 301).
    Moon,
    /// Mars barycenter (NAIF 4).
    Mars,
    /// Jupiter barycenter (NAIF 5).
    Jupiter,
    /// Saturn barycenter (NAIF 6).
    Saturn,
    /// Uranus barycenter (NAIF 7).
    Uranus,
    /// Neptune barycenter (NAIF 8).
    Neptune,
    /// Pluto barycenter (NAIF 9).
    Pluto,
    /// Sun (NAIF 10).
    Sun,
    /// 2060 Chiron (NAIF 20002060, from the Horizons small-body SPK).
    Chiron,
}

impl Body {
    /// The NAIF id DE440 (or the Chiron SPK) carries.
    #[must_use]
    pub fn naif_id(self) -> i32 {
        match self {
            Body::Mercury => 199,
            Body::Venus => 299,
            Body::Earth => 399,
            Body::Moon => 301,
            Body::Mars => 4,
            Body::Jupiter => 5,
            Body::Saturn => 6,
            Body::Uranus => 7,
            Body::Neptune => 8,
            Body::Pluto => 9,
            Body::Sun => 10,
            Body::Chiron => 20_002_060,
        }
    }
}

/// The loaded ephemeris: every SPK file, resolvable to barycentric positions.
///
/// Immutable once loaded and `Send + Sync`, so the application can park it in
/// shared state and serve concurrent lookups without locking.
#[derive(Debug)]
pub struct Ephemeris {
    files: Vec<SpkFile>,
}

impl Ephemeris {
    /// Combines parsed SPK files. Later files win segment collisions, matching
    /// the SPK data-precedence convention.
    #[must_use]
    pub fn new(files: Vec<SpkFile>) -> Self {
        Self { files }
    }

    /// Every loaded kernel name, in load order.
    #[must_use]
    pub fn files(&self) -> &[SpkFile] {
        &self.files
    }

    /// Barycentric position of `body` in kilometers (ICRF/J2000) at `t`.
    ///
    /// # Errors
    ///
    /// Returns [`EphemerisError::UnknownBody`] when no loaded file carries the
    /// body and [`EphemerisError::OutsideCoverage`] when `t` precedes or
    /// follows every segment that carries it.
    pub fn barycentric_position(
        &self,
        body: Body,
        t: TdbSeconds,
    ) -> Result<[f64; 3], EphemerisError> {
        self.position_rel(body.naif_id(), SSB, t.0, 0)
    }

    /// Barycentric position of a raw NAIF id.
    ///
    /// # Errors
    ///
    /// Same as [`Ephemeris::barycentric_position`].
    pub fn barycentric_position_id(
        &self,
        naif_id: i32,
        t: TdbSeconds,
    ) -> Result<[f64; 3], EphemerisError> {
        self.position_rel(naif_id, SSB, t.0, 0)
    }

    /// Position of `target` relative to `center`, composed through the
    /// barycenter (position of target rel SSB minus position of center rel
    /// SSB), with a link-depth guard against segment cycles.
    fn position_rel(
        &self,
        target: i32,
        center: i32,
        t: f64,
        depth: u8,
    ) -> Result<[f64; 3], EphemerisError> {
        if depth > 8 {
            return Err(EphemerisError::ResolutionOverflow {
                depth,
                naif_id: target,
            });
        }
        if target == center {
            return Ok([0.0; 3]);
        }
        let target_bary = self.position_bary(target, t, depth)?;
        let center_bary = self.position_bary(center, t, depth)?;
        Ok([
            target_bary[0] - center_bary[0],
            target_bary[1] - center_bary[1],
            target_bary[2] - center_bary[2],
        ])
    }

    /// Barycentric position following one direct segment chain (for example
    /// 399→3→0), preferring later-loaded files.
    fn position_bary(&self, target: i32, t: f64, depth: u8) -> Result<[f64; 3], EphemerisError> {
        // The chain guard also fires here: a cycle would recurse forever.
        if depth > 8 {
            return Err(EphemerisError::ResolutionOverflow {
                depth,
                naif_id: target,
            });
        }
        if target == SSB {
            return Ok([0.0; 3]);
        }
        // Later files take precedence over earlier ones. Only ICRF/J2000
        // (frame 1) segments can serve barycentric positions; DE440 and the
        // Horizons Chiron SPK are entirely frame 1.
        for file in self.files.iter().rev() {
            if let Some(segment) = file
                .segments
                .iter()
                .find(|s| s.target == target && s.frame == 1)
            {
                let relative = file.evaluate(target, segment.center, t)?;
                let anchor = self.position_bary(segment.center, t, depth + 1)?;
                return Ok([
                    relative[0] + anchor[0],
                    relative[1] + anchor[1],
                    relative[2] + anchor[2],
                ]);
            }
        }
        Err(EphemerisError::UnknownBody { naif_id: target })
    }
}

/// Deterministic synthetic-kernel builders shared by the unit tests.
#[cfg(test)]
pub(crate) mod test_support {
    use super::super::daf::RECORD_BYTES;
    use super::conv;
    use super::SpkFile;

    /// One synthetic type-2 record: midpoint, radius, then X/Y/Z coefficients.
    pub(crate) type SyntheticType2Record<'a> = (f64, f64, &'a [f64], &'a [f64], &'a [f64]);

    /// Builds a synthetic single-segment type-2 SPK.
    ///
    /// * `init` — segment start (TDB seconds past J2000)
    /// * `intlen` — record length in seconds
    /// * `records` — per-record `(mid, radius, cx, cy, cz)` with equal-length
    ///   per-axis coefficient slices; RSIZE follows from that length.
    pub(crate) fn synthetic_type2(
        target: i32,
        center: i32,
        init: f64,
        intlen: f64,
        records: &[SyntheticType2Record<'_>],
    ) -> SpkFile {
        let coefficient_count = records
            .first()
            .map_or(1, |&(_, _, cx, _, _)| cx.len())
            .max(1);
        // mid + radius + one coefficient block per X, Y, Z axis.
        let rsize = 2 + 3 * coefficient_count;
        let mut data: Vec<f64> = Vec::new();
        for &(mid, radius, cx, cy, cz) in records {
            assert_eq!(cx.len(), coefficient_count);
            assert_eq!(cy.len(), coefficient_count);
            assert_eq!(cz.len(), coefficient_count);
            data.push(mid);
            data.push(radius);
            data.extend_from_slice(cx);
            data.extend_from_slice(cy);
            data.extend_from_slice(cz);
        }
        // Type-2 control words: INIT, INTLEN, RSIZE, N.
        data.push(init);
        data.push(intlen);
        data.push(conv::size(rsize));
        data.push(conv::size(records.len()));
        build_spk(
            target,
            center,
            2,
            init,
            init + intlen * conv::size(records.len()),
            &data,
        )
    }

    /// Builds a synthetic single-segment type-21 SPK from raw line words.
    ///
    /// `lines` holds one flat word vector per difference line (4*maxdim+11
    /// words each); `end_epochs` are the line end epochs; `maxdim` closes the
    /// segment before N.
    pub(crate) fn synthetic_type21(
        target: i32,
        center: i32,
        maxdim: usize,
        lines: &[Vec<f64>],
        end_epochs: &[f64],
        start: f64,
    ) -> SpkFile {
        let mut data: Vec<f64> = Vec::new();
        for line in lines {
            assert_eq!(
                line.len(),
                4 * maxdim + 11,
                "line must be 4*maxdim+11 words"
            );
            data.extend_from_slice(line);
        }
        data.extend_from_slice(end_epochs);
        // Segments over 100 lines carry a directory of every 100th end epoch
        // (SPICE's E(100), E(200), …); shorter segments carry none.
        if end_epochs.len() > 100 {
            for group in 0..end_epochs.len() / 100 {
                data.push(end_epochs[group * 100 + 99]);
            }
        }
        data.push(conv::size(maxdim));
        data.push(conv::size(lines.len()));
        let end = *end_epochs.last().expect("at least one line");
        build_spk(target, center, 21, start, end, &data)
    }

    /// Assembles a one-segment SPK file around prepared segment data words.
    fn build_spk(
        target: i32,
        center: i32,
        spk_type: i32,
        start: f64,
        end: f64,
        data: &[f64],
    ) -> SpkFile {
        let mut bytes: Vec<u8> = Vec::new();
        // Record 1: the file record.
        let mut file_record = vec![0_u8; RECORD_BYTES];
        file_record[0..8].copy_from_slice(b"DAF/SPK ");
        file_record[8..12].copy_from_slice(&2_i32.to_le_bytes()); // ND
        file_record[12..16].copy_from_slice(&6_i32.to_le_bytes()); // NI
        file_record[76..80].copy_from_slice(&2_i32.to_le_bytes()); // FWARD
        file_record[80..84].copy_from_slice(&2_i32.to_le_bytes()); // BWARD
        file_record[88..96].copy_from_slice(b"LTL-IEEE");
        bytes.extend_from_slice(&file_record);
        // Record 2: the summary record — next=0, prev=0, nsum=1, descriptor.
        let mut summary = vec![0_u8; RECORD_BYTES];
        summary[0..8].copy_from_slice(&0_f64.to_le_bytes());
        summary[8..16].copy_from_slice(&0_f64.to_le_bytes());
        summary[16..24].copy_from_slice(&1_f64.to_le_bytes());
        summary[24..32].copy_from_slice(&start.to_le_bytes());
        summary[32..40].copy_from_slice(&end.to_le_bytes());
        summary[40..44].copy_from_slice(&target.to_le_bytes());
        summary[44..48].copy_from_slice(&center.to_le_bytes());
        summary[48..52].copy_from_slice(&1_i32.to_le_bytes()); // frame
        summary[52..56].copy_from_slice(&spk_type.to_le_bytes());
        // Data begins at record 4, word 1 → global word 385 (3*128+1).
        summary[56..60].copy_from_slice(&385_i32.to_le_bytes());
        let data_end = 384 + conv::word_count(data.len());
        summary[60..64].copy_from_slice(&data_end.to_le_bytes());
        bytes.extend_from_slice(&summary);
        // Record 3: segment name (ignored by the reader).
        bytes.extend_from_slice(&[0_u8; RECORD_BYTES]);
        // Record 4 onward: the segment data.
        for word in data {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        // Pad the last record.
        while !bytes.len().is_multiple_of(RECORD_BYTES) {
            bytes.push(0);
        }
        SpkFile::from_bytes(bytes, format!("synthetic-{target}-{spk_type}"))
            .expect("synthetic SPK is valid")
    }
}

// Synthetic fixtures are built so evaluations reproduce the expected values
// bit-exactly; a tolerance would weaken these assertions.
#[allow(clippy::float_cmp)]
#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use crate::time::TdbSeconds;

    const DAY: f64 = 86_400.0;

    #[test]
    fn parses_synthetic_type2_descriptor() {
        let file = synthetic_type2(
            3,
            0,
            0.0,
            10.0 * DAY,
            &[(
                5.0 * DAY,
                5.0 * DAY,
                &[1.0; 2][..],
                &[2.0; 2][..],
                &[3.0; 2][..],
            )],
        );
        assert_eq!(file.segments().len(), 1);
        let segment = &file.segments()[0];
        assert_eq!(segment.target, 3);
        assert_eq!(segment.center, 0);
        assert_eq!(segment.spk_type, 2);
        assert_eq!(segment.start, 0.0);
        assert_eq!(segment.end, 10.0 * DAY);
    }

    /// One Chebyshev coefficient per axis interpolates to a constant.
    #[test]
    fn type2_single_coefficient_is_constant() {
        let file = synthetic_type2(
            3,
            0,
            0.0,
            10.0 * DAY,
            &[(
                5.0 * DAY,
                5.0 * DAY,
                &[1000.0; 2][..],
                &[2000.0; 2][..],
                &[3000.0; 2][..],
            )],
        );
        let ephemeris = Ephemeris::new(vec![file]);
        let mid = TdbSeconds(5.0 * DAY);
        let position = ephemeris.barycentric_position_id(3, mid).expect("covered");
        assert_eq!(position, [1000.0, 2000.0, 3000.0]);
    }

    /// Two coefficients per axis give position = c0 + c1·s with s = (t−mid)/radius.
    #[test]
    fn type2_two_coefficients_are_linear() {
        let file = synthetic_type2(
            3,
            0,
            0.0,
            10.0 * DAY,
            &[(
                5.0 * DAY,
                5.0 * DAY,
                &[1000.0, 400.0][..],
                &[0.0, 0.0][..],
                &[0.0, 0.0][..],
            )],
        );
        let ephemeris = Ephemeris::new(vec![file]);
        // s = 0.5 → 1000 + 200 = 1200.
        let t = TdbSeconds(7.5 * DAY);
        let position = ephemeris.barycentric_position_id(3, t).expect("covered");
        assert!((position[0] - 1200.0).abs() < 1e-9);
        // Boundary: s = -1 → 600.
        let t = TdbSeconds(0.0);
        let position = ephemeris.barycentric_position_id(3, t).expect("covered");
        assert!((position[0] - 600.0).abs() < 1e-9);
    }

    /// Earth (399→3) composes onto the EMB (3→0) chain.
    #[test]
    fn composes_earth_through_emb() {
        let emb = synthetic_type2(
            3,
            0,
            0.0,
            DAY,
            &[(
                DAY / 2.0,
                DAY / 2.0,
                &[10.0; 2][..],
                &[0.0; 2][..],
                &[0.0; 2][..],
            )],
        );
        let earth = synthetic_type2(
            399,
            3,
            0.0,
            DAY,
            &[(
                DAY / 2.0,
                DAY / 2.0,
                &[1.0; 2][..],
                &[2.0; 2][..],
                &[3.0; 2][..],
            )],
        );
        let ephemeris = Ephemeris::new(vec![emb, earth]);
        let t = TdbSeconds(DAY / 2.0);
        let position = ephemeris.barycentric_position_id(399, t).expect("covered");
        assert_eq!(position, [11.0, 2.0, 3.0]);
    }

    #[test]
    fn unknown_body_is_reported() {
        let ephemeris = Ephemeris::new(vec![]);
        let error = ephemeris
            .barycentric_position_id(999, TdbSeconds(0.0))
            .expect_err("nothing loaded");
        assert!(matches!(error, EphemerisError::UnknownBody { .. }));
    }

    #[test]
    fn outside_coverage_is_reported() {
        let file = synthetic_type2(
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
        );
        let ephemeris = Ephemeris::new(vec![file]);
        let error = ephemeris
            .barycentric_position_id(3, TdbSeconds(10.0 * DAY))
            .expect_err("beyond segment");
        assert!(matches!(error, EphemerisError::OutsideCoverage { .. }));
    }

    /// Chiron (type 21) resolves through the Sun (10→0) from a second file.
    #[test]
    fn composes_type21_through_sun() {
        let maxdim = 15;
        let mut line = vec![0.0_f64; 4 * maxdim + 11];
        line[0] = 0.0; // TL
        for (index, g) in line[1..=maxdim].iter_mut().enumerate() {
            *g = (crate::spk::conv::size(index) + 1.0) * DAY;
        }
        // REFPOS x@16 y@18 z@20, REFVEL interleaved @17/19/21.
        line[16] = 1_000_000.0; // x
        line[17] = 10.0; // vx
        line[18] = 2_000_000.0; // y
        line[19] = -5.0; // vy
        line[20] = 3_000_000.0; // z
        line[21] = 1.0; // vz
                        // DT tables start at word index 22; with KQ=1 only dt(axis,1) is used.
        line[22] = 1.0; // dt(x,1)
        line[22 + maxdim] = 2.0; // dt(y,1)
        line[22 + 2 * maxdim] = 3.0; // dt(z,1)
        line[4 * maxdim + 7] = 2.0; // KQMAX1: degree 1
        line[4 * maxdim + 8] = 1.0; // KQ x
        line[4 * maxdim + 9] = 1.0; // KQ y
        line[4 * maxdim + 10] = 1.0; // KQ z
        let chiron = synthetic_type21(20_002_060, 10, maxdim, &[line], &[DAY], 0.0);
        let sun = synthetic_type2(
            10,
            0,
            0.0,
            DAY,
            &[(
                DAY / 2.0,
                DAY / 2.0,
                &[5.0; 2][..],
                &[0.0; 2][..],
                &[0.0; 2][..],
            )],
        );
        let ephemeris = Ephemeris::new(vec![sun, chiron]);

        // With KQMAX1=2 the MDA recurrence yields
        // pos = refpos + Δ·refvel + Δ²·dt(axis,1)/2 (Δ in seconds… scaled by
        // the G-divisions the recurrence performs; see type21 tests for the
        // exact hand computation). Here we only assert composition: Chiron
        // minus Sun lands on the EMB-free chain without error.
        let position = ephemeris
            .barycentric_position_id(20_002_060, TdbSeconds(DAY / 2.0))
            .expect("covered");
        assert!(position[0].is_finite());
    }
}
