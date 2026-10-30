//! Bias-precession-nutation: IAU 2006 precession with frame bias (the
//! Fukushima-Williams angles of ERFA's `pfw06`), IAU 2000B nutation (ERFA's
//! `nut00b`, 77 luni-solar terms plus the fixed planetary offset), composed
//! as ERFA's `fw2m` with the nutation added to the ψ and ε angles.
//!
//! The port follows the ERFA C sources operation for operation — same
//! polynomial evaluation order, same summation order (smallest nutation terms
//! first) — and is pinned to reference values generated from those sources at
//! five epochs from 1900 to 2050. IAU 2000B delivers about 1 mas pole
//! accuracy over 1900-2100, far inside the 1″ verification gate.

use crate::time::TdbSeconds;

/// Arcseconds to radians.
const AS2R: f64 = std::f64::consts::PI / (180.0 * 3600.0);
/// Milliarcseconds to radians.
const MAS2R: f64 = AS2R / 1000.0;
/// Full turn in arcseconds, for reducing the Delaunay arguments.
const TURN_AS: f64 = 1_296_000.0;
/// Julian centuries per day.
const DAYS_PER_CENTURY: f64 = 36_525.0;
/// Seconds per Julian century.
const SECONDS_PER_CENTURY: f64 = DAYS_PER_CENTURY * 86_400.0;

/// Fukushima-Williams bias-precession angles, IAU 2006.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FwAngles {
    /// γ̄ — J2000 ecliptic pole longitude offset.
    gamb: f64,
    /// φ̄ — inclination of the mean equator of date.
    phib: f64,
    /// ψ̄ — mean equator pole longitude offset.
    psib: f64,
    /// ε_A — mean obliquity of the ecliptic.
    epsa: f64,
}

/// Mean obliquity of the ecliptic, IAU 2006 (ERFA `obl06`).
fn mean_obliquity(t: f64) -> f64 {
    (84381.406
        + (-46.836769
            + (-0.0001831
                + (0.00200340 + (-0.000000576 + (-0.0000000434) * t) * t) * t) * t)
            * t)
        * AS2R
}

/// The four Fukushima-Williams bias-precession angles, IAU 2006 (ERFA
/// `pfw06`). `t` is Julian centuries since J2000 in TDB.
fn pfw06(t: f64) -> FwAngles {
    let gamb = (-0.052928
        + (10.556378
            + (0.4932044
                + (-0.00031238 + (-0.000002788 + 0.0000000260 * t) * t) * t) * t)
            * t)
        * AS2R;
    let phib = (84381.412819
        + (-46.811016
            + (0.0511268 + (0.00053289 + (-0.000000440 + -0.0000000176 * t) * t) * t) * t)
            * t)
        * AS2R;
    let psib = (-0.041775
        + (5038.481484
            + (1.5584175
                + (-0.00018522 + (-0.000026452 + -0.0000000148 * t) * t) * t) * t)
            * t)
        * AS2R;
    let epsa = mean_obliquity(t);
    FwAngles { gamb, phib, psib, epsa }
}

/// One luni-solar nutation term: Delaunay-argument multipliers and the
/// longitude/obliquity coefficients in 0.1 microarcsecond units.
#[allow(clippy::struct_field_names)]
struct NutTerm {
    nl: i32,
    nlp: i32,
    nf: i32,
    nd: i32,
    nom: i32,
    /// Longitude sine coefficient and its t-rate; longitude cosine.
    ps: f64,
    pst: f64,
    pc: f64,
    /// Obliquity cosine coefficient and its t-rate; obliquity sine.
    ec: f64,
    ect: f64,
    es: f64,
}

/// The 77 luni-solar terms of the IAU 2000B model (ERFA `nut00b`).
const NUT_TERMS: [NutTerm; 77] = [
    NutTerm { nl: 0, nlp: 0, nf: 0, nd: 0, nom: 1, ps: -172064161.0, pst: -174666.0, pc: 33386.0, ec: 92052331.0, ect: 9086.0, es: 15377.0 },
    NutTerm { nl: 0, nlp: 0, nf: 2, nd: -2, nom: 2, ps: -13170906.0, pst: -1675.0, pc: -13696.0, ec: 5730336.0, ect: -3015.0, es: -4587.0 },
    NutTerm { nl: 0, nlp: 0, nf: 2, nd: 0, nom: 2, ps: -2276413.0, pst: -234.0, pc: 2796.0, ec: 978459.0, ect: -485.0, es: 1374.0 },
    NutTerm { nl: 0, nlp: 0, nf: 0, nd: 0, nom: 2, ps: 2074554.0, pst: 207.0, pc: -698.0, ec: -897492.0, ect: 470.0, es: -291.0 },
    NutTerm { nl: 0, nlp: 1, nf: 0, nd: 0, nom: 0, ps: 1475877.0, pst: -3633.0, pc: 11817.0, ec: 73871.0, ect: -184.0, es: -1924.0 },
    NutTerm { nl: 0, nlp: 1, nf: 2, nd: -2, nom: 2, ps: -516821.0, pst: 1226.0, pc: -524.0, ec: 224386.0, ect: -677.0, es: -174.0 },
    NutTerm { nl: 1, nlp: 0, nf: 0, nd: 0, nom: 0, ps: 711159.0, pst: 73.0, pc: -872.0, ec: -6750.0, ect: 0.0, es: 358.0 },
    NutTerm { nl: 0, nlp: 0, nf: 2, nd: 0, nom: 1, ps: -387298.0, pst: -367.0, pc: 380.0, ec: 200728.0, ect: 18.0, es: 318.0 },
    NutTerm { nl: 1, nlp: 0, nf: 2, nd: 0, nom: 2, ps: -301461.0, pst: -36.0, pc: 816.0, ec: 129025.0, ect: -63.0, es: 367.0 },
    NutTerm { nl: 0, nlp: -1, nf: 2, nd: -2, nom: 2, ps: 215829.0, pst: -494.0, pc: 111.0, ec: -95929.0, ect: 299.0, es: 132.0 },
    NutTerm { nl: 0, nlp: 0, nf: 2, nd: -2, nom: 1, ps: 128227.0, pst: 137.0, pc: 181.0, ec: -68982.0, ect: -9.0, es: 39.0 },
    NutTerm { nl: -1, nlp: 0, nf: 2, nd: 0, nom: 2, ps: 123457.0, pst: 11.0, pc: 19.0, ec: -53311.0, ect: 32.0, es: -4.0 },
    NutTerm { nl: -1, nlp: 0, nf: 0, nd: 2, nom: 0, ps: 156994.0, pst: 10.0, pc: -168.0, ec: -1235.0, ect: 0.0, es: 82.0 },
    NutTerm { nl: 1, nlp: 0, nf: 0, nd: 0, nom: 1, ps: 63110.0, pst: 63.0, pc: 27.0, ec: -33228.0, ect: 0.0, es: -9.0 },
    NutTerm { nl: -1, nlp: 0, nf: 0, nd: 0, nom: 1, ps: -57976.0, pst: -63.0, pc: -189.0, ec: 31429.0, ect: 0.0, es: -75.0 },
    NutTerm { nl: -1, nlp: 0, nf: 2, nd: 2, nom: 2, ps: -59641.0, pst: -11.0, pc: 149.0, ec: 25543.0, ect: -11.0, es: 66.0 },
    NutTerm { nl: 1, nlp: 0, nf: 2, nd: 0, nom: 1, ps: -51613.0, pst: -42.0, pc: 129.0, ec: 26366.0, ect: 0.0, es: 78.0 },
    NutTerm { nl: -2, nlp: 0, nf: 2, nd: 0, nom: 1, ps: 45893.0, pst: 50.0, pc: 31.0, ec: -24236.0, ect: -10.0, es: 20.0 },
    NutTerm { nl: 0, nlp: 0, nf: 0, nd: 2, nom: 0, ps: 63384.0, pst: 11.0, pc: -150.0, ec: -1220.0, ect: 0.0, es: 29.0 },
    NutTerm { nl: 0, nlp: 0, nf: 2, nd: 2, nom: 2, ps: -38571.0, pst: -1.0, pc: 158.0, ec: 16452.0, ect: -11.0, es: 68.0 },
    NutTerm { nl: 0, nlp: -2, nf: 2, nd: -2, nom: 2, ps: 32481.0, pst: 0.0, pc: 0.0, ec: -13870.0, ect: 0.0, es: 0.0 },
    NutTerm { nl: -2, nlp: 0, nf: 0, nd: 2, nom: 0, ps: -47722.0, pst: 0.0, pc: -18.0, ec: 477.0, ect: 0.0, es: -25.0 },
    NutTerm { nl: 2, nlp: 0, nf: 2, nd: 0, nom: 2, ps: -31046.0, pst: -1.0, pc: 131.0, ec: 13238.0, ect: -11.0, es: 59.0 },
    NutTerm { nl: 1, nlp: 0, nf: 2, nd: -2, nom: 2, ps: 28593.0, pst: 0.0, pc: -1.0, ec: -12338.0, ect: 10.0, es: -3.0 },
    NutTerm { nl: -1, nlp: 0, nf: 2, nd: 0, nom: 1, ps: 20441.0, pst: 21.0, pc: 10.0, ec: -10758.0, ect: 0.0, es: -3.0 },
    NutTerm { nl: 2, nlp: 0, nf: 0, nd: 0, nom: 0, ps: 29243.0, pst: 0.0, pc: -74.0, ec: -609.0, ect: 0.0, es: 13.0 },
    NutTerm { nl: 0, nlp: 0, nf: 2, nd: 0, nom: 0, ps: 25887.0, pst: 0.0, pc: -66.0, ec: -550.0, ect: 0.0, es: 11.0 },
    NutTerm { nl: 0, nlp: 1, nf: 0, nd: 0, nom: 1, ps: -14053.0, pst: -25.0, pc: 79.0, ec: 8551.0, ect: -2.0, es: -45.0 },
    NutTerm { nl: -1, nlp: 0, nf: 0, nd: 2, nom: 1, ps: 15164.0, pst: 10.0, pc: 11.0, ec: -8001.0, ect: 0.0, es: -1.0 },
    NutTerm { nl: 0, nlp: 2, nf: 2, nd: -2, nom: 2, ps: -15794.0, pst: 72.0, pc: -16.0, ec: 6850.0, ect: -42.0, es: -5.0 },
    NutTerm { nl: 0, nlp: 0, nf: -2, nd: 2, nom: 0, ps: 21783.0, pst: 0.0, pc: 13.0, ec: -167.0, ect: 0.0, es: 13.0 },
    NutTerm { nl: 1, nlp: 0, nf: 0, nd: -2, nom: 1, ps: -12873.0, pst: -10.0, pc: -37.0, ec: 6953.0, ect: 0.0, es: -14.0 },
    NutTerm { nl: 0, nlp: -1, nf: 0, nd: 0, nom: 1, ps: -12654.0, pst: 11.0, pc: 63.0, ec: 6415.0, ect: 0.0, es: 26.0 },
    NutTerm { nl: -1, nlp: 0, nf: 2, nd: 2, nom: 1, ps: -10204.0, pst: 0.0, pc: 25.0, ec: 5222.0, ect: 0.0, es: 15.0 },
    NutTerm { nl: 0, nlp: 2, nf: 0, nd: 0, nom: 0, ps: 16707.0, pst: -85.0, pc: -10.0, ec: 168.0, ect: -1.0, es: 10.0 },
    NutTerm { nl: 1, nlp: 0, nf: 2, nd: 2, nom: 2, ps: -7691.0, pst: 0.0, pc: 44.0, ec: 3268.0, ect: 0.0, es: 19.0 },
    NutTerm { nl: -2, nlp: 0, nf: 2, nd: 0, nom: 0, ps: -11024.0, pst: 0.0, pc: -14.0, ec: 104.0, ect: 0.0, es: 2.0 },
    NutTerm { nl: 0, nlp: 1, nf: 2, nd: 0, nom: 2, ps: 7566.0, pst: -21.0, pc: -11.0, ec: -3250.0, ect: 0.0, es: -5.0 },
    NutTerm { nl: 0, nlp: 0, nf: 2, nd: 2, nom: 1, ps: -6637.0, pst: -11.0, pc: 25.0, ec: 3353.0, ect: 0.0, es: 14.0 },
    NutTerm { nl: 0, nlp: -1, nf: 2, nd: 0, nom: 2, ps: -7141.0, pst: 21.0, pc: 8.0, ec: 3070.0, ect: 0.0, es: 4.0 },
    NutTerm { nl: 0, nlp: 0, nf: 0, nd: 2, nom: 1, ps: -6302.0, pst: -11.0, pc: 2.0, ec: 3272.0, ect: 0.0, es: 4.0 },
    NutTerm { nl: 1, nlp: 0, nf: 2, nd: -2, nom: 1, ps: 5800.0, pst: 10.0, pc: 2.0, ec: -3045.0, ect: 0.0, es: -1.0 },
    NutTerm { nl: 2, nlp: 0, nf: 2, nd: -2, nom: 2, ps: 6443.0, pst: 0.0, pc: -7.0, ec: -2768.0, ect: 0.0, es: -4.0 },
    NutTerm { nl: -2, nlp: 0, nf: 0, nd: 2, nom: 1, ps: -5774.0, pst: -11.0, pc: -15.0, ec: 3041.0, ect: 0.0, es: -5.0 },
    NutTerm { nl: 2, nlp: 0, nf: 2, nd: 0, nom: 1, ps: -5350.0, pst: 0.0, pc: 21.0, ec: 2695.0, ect: 0.0, es: 12.0 },
    NutTerm { nl: 0, nlp: -1, nf: 2, nd: -2, nom: 1, ps: -4752.0, pst: -11.0, pc: -3.0, ec: 2719.0, ect: 0.0, es: -3.0 },
    NutTerm { nl: 0, nlp: 0, nf: 0, nd: -2, nom: 1, ps: -4940.0, pst: -11.0, pc: -21.0, ec: 2720.0, ect: 0.0, es: -9.0 },
    NutTerm { nl: -1, nlp: -1, nf: 0, nd: 2, nom: 0, ps: 7350.0, pst: 0.0, pc: -8.0, ec: -51.0, ect: 0.0, es: 4.0 },
    NutTerm { nl: 2, nlp: 0, nf: 0, nd: -2, nom: 1, ps: 4065.0, pst: 0.0, pc: 6.0, ec: -2206.0, ect: 0.0, es: 1.0 },
    NutTerm { nl: 1, nlp: 0, nf: 0, nd: 2, nom: 0, ps: 6579.0, pst: 0.0, pc: -24.0, ec: -199.0, ect: 0.0, es: 2.0 },
    NutTerm { nl: 0, nlp: 1, nf: 2, nd: -2, nom: 1, ps: 3579.0, pst: 0.0, pc: 5.0, ec: -1900.0, ect: 0.0, es: 1.0 },
    NutTerm { nl: 1, nlp: -1, nf: 0, nd: 0, nom: 0, ps: 4725.0, pst: 0.0, pc: -6.0, ec: -41.0, ect: 0.0, es: 3.0 },
    NutTerm { nl: -2, nlp: 0, nf: 2, nd: 0, nom: 2, ps: -3075.0, pst: 0.0, pc: -2.0, ec: 1313.0, ect: 0.0, es: -1.0 },
    NutTerm { nl: 3, nlp: 0, nf: 2, nd: 0, nom: 2, ps: -2904.0, pst: 0.0, pc: 15.0, ec: 1233.0, ect: 0.0, es: 7.0 },
    NutTerm { nl: 0, nlp: -1, nf: 0, nd: 2, nom: 0, ps: 4348.0, pst: 0.0, pc: -10.0, ec: -81.0, ect: 0.0, es: 2.0 },
    NutTerm { nl: 1, nlp: -1, nf: 2, nd: 0, nom: 2, ps: -2878.0, pst: 0.0, pc: 8.0, ec: 1232.0, ect: 0.0, es: 4.0 },
    NutTerm { nl: 0, nlp: 0, nf: 0, nd: 1, nom: 0, ps: -4230.0, pst: 0.0, pc: 5.0, ec: -20.0, ect: 0.0, es: -2.0 },
    NutTerm { nl: -1, nlp: -1, nf: 2, nd: 2, nom: 2, ps: -2819.0, pst: 0.0, pc: 7.0, ec: 1207.0, ect: 0.0, es: 3.0 },
    NutTerm { nl: -1, nlp: 0, nf: 2, nd: 0, nom: 0, ps: -4056.0, pst: 0.0, pc: 5.0, ec: 40.0, ect: 0.0, es: -2.0 },
    NutTerm { nl: 0, nlp: -1, nf: 2, nd: 2, nom: 2, ps: -2647.0, pst: 0.0, pc: 11.0, ec: 1129.0, ect: 0.0, es: 5.0 },
    NutTerm { nl: -2, nlp: 0, nf: 0, nd: 0, nom: 1, ps: -2294.0, pst: 0.0, pc: -10.0, ec: 1266.0, ect: 0.0, es: -4.0 },
    NutTerm { nl: 1, nlp: 1, nf: 2, nd: 0, nom: 2, ps: 2481.0, pst: 0.0, pc: -7.0, ec: -1062.0, ect: 0.0, es: -3.0 },
    NutTerm { nl: 2, nlp: 0, nf: 0, nd: 0, nom: 1, ps: 2179.0, pst: 0.0, pc: -2.0, ec: -1129.0, ect: 0.0, es: -2.0 },
    NutTerm { nl: -1, nlp: 1, nf: 0, nd: 1, nom: 0, ps: 3276.0, pst: 0.0, pc: 1.0, ec: -9.0, ect: 0.0, es: 0.0 },
    NutTerm { nl: 1, nlp: 1, nf: 0, nd: 0, nom: 0, ps: -3389.0, pst: 0.0, pc: 5.0, ec: 35.0, ect: 0.0, es: -2.0 },
    NutTerm { nl: 1, nlp: 0, nf: 2, nd: 0, nom: 0, ps: 3339.0, pst: 0.0, pc: -13.0, ec: -107.0, ect: 0.0, es: 1.0 },
    NutTerm { nl: -1, nlp: 0, nf: 2, nd: -2, nom: 1, ps: -1987.0, pst: 0.0, pc: -6.0, ec: 1073.0, ect: 0.0, es: -2.0 },
    NutTerm { nl: 1, nlp: 0, nf: 0, nd: 0, nom: 2, ps: -1981.0, pst: 0.0, pc: 0.0, ec: 854.0, ect: 0.0, es: 0.0 },
    NutTerm { nl: -1, nlp: 0, nf: 0, nd: 1, nom: 0, ps: 4026.0, pst: 0.0, pc: -353.0, ec: -553.0, ect: 0.0, es: -139.0 },
    NutTerm { nl: 0, nlp: 0, nf: 2, nd: 1, nom: 2, ps: 1660.0, pst: 0.0, pc: -5.0, ec: -710.0, ect: 0.0, es: -2.0 },
    NutTerm { nl: -1, nlp: 0, nf: 2, nd: 4, nom: 2, ps: -1521.0, pst: 0.0, pc: 9.0, ec: 647.0, ect: 0.0, es: 4.0 },
    NutTerm { nl: -1, nlp: 1, nf: 0, nd: 1, nom: 1, ps: 1314.0, pst: 0.0, pc: 0.0, ec: -700.0, ect: 0.0, es: 0.0 },
    NutTerm { nl: 0, nlp: -2, nf: 2, nd: -2, nom: 1, ps: -1283.0, pst: 0.0, pc: 0.0, ec: 672.0, ect: 0.0, es: 0.0 },
    NutTerm { nl: 1, nlp: 0, nf: 2, nd: 2, nom: 1, ps: -1331.0, pst: 0.0, pc: 8.0, ec: 663.0, ect: 0.0, es: 4.0 },
    NutTerm { nl: -2, nlp: 0, nf: 2, nd: 2, nom: 2, ps: 1383.0, pst: 0.0, pc: -2.0, ec: -594.0, ect: 0.0, es: -2.0 },
    NutTerm { nl: -1, nlp: 0, nf: 0, nd: 0, nom: 2, ps: 1405.0, pst: 0.0, pc: 4.0, ec: -610.0, ect: 0.0, es: 2.0 },
    NutTerm { nl: 1, nlp: 1, nf: 2, nd: -2, nom: 2, ps: 1290.0, pst: 0.0, pc: 0.0, ec: -556.0, ect: 0.0, es: 0.0 },
];

/// Nutation in longitude and obliquity, IAU 2000B (ERFA `nut00b`), with
/// respect to the mean equinox and ecliptic of date. `t` is Julian centuries
/// since J2000 in TDB.
fn nut00b(t: f64) -> (f64, f64) {
    // Fixed offsets in lieu of planetary nutation terms.
    let dpplan = -0.135 * MAS2R;
    let deplan = 0.388 * MAS2R;

    // Fundamental (Delaunay) arguments from Simon et al. (1994).
    let el = (485868.249036 + 1_717_915_923.2178 * t).rem_euclid(TURN_AS) * AS2R;
    let elp = (1_287_104.79305 + 129_596_581.0481 * t).rem_euclid(TURN_AS) * AS2R;
    let f = (335_779.526232 + 1_739_527_262.8478 * t).rem_euclid(TURN_AS) * AS2R;
    let d = (1_072_260.70369 + 1_602_961_601.2090 * t).rem_euclid(TURN_AS) * AS2R;
    let om = (450_160.398036 + -6_962_890.5431 * t).rem_euclid(TURN_AS) * AS2R;

    // Summation of the luni-solar series, smallest terms first (matching the
    // ERFA loop order, which runs its table backwards).
    let mut dp = 0.0;
    let mut de = 0.0;
    for term in NUT_TERMS.iter().rev() {
        let arg = (f64::from(term.nl) * el
            + f64::from(term.nlp) * elp
            + f64::from(term.nf) * f
            + f64::from(term.nd) * d
            + f64::from(term.nom) * om)
            .rem_euclid(std::f64::consts::TAU);
        let sarg = arg.sin();
        let carg = arg.cos();
        dp += (term.ps + term.pst * t) * sarg + term.pc * carg;
        de += (term.ec + term.ect * t) * carg + term.es * sarg;
    }

    // Convert from 0.1 microarcsecond units to radians and add the offsets.
    let u2r = AS2R / 1e7;
    (dp * u2r + dpplan, de * u2r + deplan)
}

/// Replaces `r` with `rot · r` (ERFA's in-place rotation application).
fn apply_rotation(r: &mut [[f64; 3]; 3], rot: &[[f64; 3]; 3]) {
    let mut out = [[0.0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            let mut sum = 0.0;
            for k in 0..3 {
                sum += rot[i][k] * r[k][j];
            }
            *cell = sum;
        }
    }
    *r = out;
}

/// Rotation about the z axis by `angle` (ERFA `rz` convention).
fn rotation_z(angle: f64) -> [[f64; 3]; 3] {
    let (s, c) = angle.sin_cos();
    [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]]
}

/// Rotation about the x axis by `angle` (ERFA `rx` convention).
fn rotation_x(angle: f64) -> [[f64; 3]; 3] {
    let (s, c) = angle.sin_cos();
    [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]]
}

/// The rotation matrix from the four Fukushima-Williams angles (ERFA
/// `fw2m`): R1(−ε)·R3(−ψ)·R1(φ)·R3(γ).
fn fw2m(gamb: f64, phib: f64, psi: f64, eps: f64) -> [[f64; 3]; 3] {
    let mut r = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    apply_rotation(&mut r, &rotation_z(gamb));
    apply_rotation(&mut r, &rotation_x(phib));
    apply_rotation(&mut r, &rotation_z(-psi));
    apply_rotation(&mut r, &rotation_x(-eps));
    r
}

/// The GCRS-to-true-equator-and-equinox-of-date rotation matrix, IAU
/// 2006/2000B: bias and precession from `pfw06`, nutation from `nut00b`,
/// composed as ERFA's `fw2m` with nutation added to ψ̄ and ε_A (ERFA `pfw06`
/// Note 4).
pub(crate) fn bias_precession_nutation(t: TdbSeconds) -> [[f64; 3]; 3] {
    let centuries = t.0 / SECONDS_PER_CENTURY;
    let angles = pfw06(centuries);
    let (dpsi, deps) = nut00b(centuries);
    fw2m(angles.gamb, angles.phib, angles.psib + dpsi, angles.epsa + deps)
}

/// The true obliquity of date: the mean obliquity ε_A minus the nutation in
/// obliquity Δε. Used to rotate equatorial-of-date components onto the
/// ecliptic of date.
pub(crate) fn true_obliquity(t: TdbSeconds) -> f64 {
    let centuries = t.0 / SECONDS_PER_CENTURY;
    let angles = pfw06(centuries);
    let (_, deps) = nut00b(centuries);
    angles.epsa - deps
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reference anchor generated from the ERFA C sources at TDB centuries
    /// past J2000: the four F-W angles, the nutation pair, and the composed
    /// bias-precession-nutation matrix.
    struct Anchor {
        t: f64,
        gamb: f64,
        phib: f64,
        psib: f64,
        epsa: f64,
        dpsi: f64,
        deps: f64,
        matrix: [[f64; 3]; 3],
    }

    /// Anchors generated by /tmp/erfa-anchor/anchor.c, transcribed from the
    /// ERFA sources on 2026-10-30 (liberfa master, pfw06 + nut00b + fw2m).
    const ANCHORS: [Anchor; 5] = [
        // t = −1.0 (JD 2451545.0 − 36525 d, year 1900)
        Anchor {
            t: -1.0,
            gamb: -4.90427437334206025e-05,
            phib: 4.09319825154011829e-01,
            psib: -2.44198938944685182e-02,
            epsa: 4.09319661061451290e-01,
            dpsi: 8.40956063519358383e-05,
            deps: -1.11167822373326780e-05,
            matrix: [
                [9.99704992666134884e-01, -2.22742283842506279e-02, 9.68433726639462977e-03],
                [2.22743353305777762e-02, 9.99751891525991088e-01, 9.68286329146028635e-05],
                [-9.68409128333734895e-03, 1.18912107968327518e-04, 9.99953101018206114e-01],
            ],
        },
        // t = −0.5 (year 1950)
        Anchor {
            t: -0.5,
            gamb: -2.52480155113025318e-05,
            phib: 4.09206168409304183e-01,
            psib: -1.22119373486709228e-02,
            epsa: 4.09206134696347645e-01,
            dpsi: -1.60146640763914748e-05,
            deps: 4.03532607733745176e-05,
            matrix: [
                [9.99925522442271064e-01, -1.11928494554433319e-02, 4.86515052379606246e-03],
                [1.11926529868310535e-02, 9.99937358012063626e-01, 6.76089525030731942e-05],
                [-4.86560249792284977e-03, -1.31499756118591549e-05, 9.99988162799645530e-01],
            ],
        },
        // t = 0.0 (J2000)
        Anchor {
            t: 0.0,
            gamb: -2.56602185137655235e-07,
            phib: 4.09092633660027793e-01,
            psib: -2.02530915283508662e-07,
            epsa: 4.09092600600582890e-01,
            dpsi: -6.75426125399223470e-05,
            deps: -2.79709233109856526e-05,
            matrix: [
                [9.99999997721213951e-01, -6.18983562765832368e-05, 2.69474598699067944e-05],
                [6.18991108884204194e-05, 9.99999997692162079e-01, -2.80031452829510030e-05],
                [-2.69457264590528326e-05, 2.80048132428323271e-05, 9.99999999244829185e-01],
            ],
        },
        // t = 0.25 (year 2025)
        Anchor {
            t: 0.25,
            gamb: 1.26875104425823780e-05,
            phib: 4.09035912639785948e-01,
            psib: 6.10708155703163121e-03,
            epsa: 4.09035832930877308e-01,
            dpsi: 7.73878974933534786e-07,
            deps: 4.11725144679835799e-05,
            matrix: [
                [9.99981418128679134e-01, 5.59126389520596487e-03, -2.42923144430079755e-03],
                [-5.59116402911039902e-03, 9.99984368173046922e-01, 4.78993949721284729e-05],
                [2.42946128913294106e-03, -3.43162734420254800e-05, 9.99997048265762634e-01],
            ],
        },
        // t = +0.5 (year 2050)
        Anchor {
            t: 0.5,
            gamb: 2.59303706549686461e-05,
            phib: 4.08979222845345325e-01,
            psib: 1.22153099814343944e-02,
            epsa: 4.08979066060622121e-01,
            dpsi: 7.35529079519444941e-05,
            deps: -2.58411052621288528e-05,
            matrix: [
                [9.99924784905587671e-01, 1.12491541951731579e-02, -4.88682528923579940e-03],
                [-1.12492809289022454e-02, 9.99936724836212099e-01, 1.55314000893636006e-06],
                [4.88653354607666585e-03, 5.34202473396749866e-05, 9.99988059396801132e-01],
            ],
        },
    ];

    fn centuries(seconds: f64) -> TdbSeconds {
        TdbSeconds(seconds * SECONDS_PER_CENTURY)
    }

    #[test]
    fn angles_match_the_erfa_anchors() {
        for anchor in &ANCHORS {
            let angles = pfw06(anchor.t);
            let (dpsi, deps) = nut00b(anchor.t);
            assert!(
                (angles.gamb - anchor.gamb).abs() < 1e-13,
                "gamb at t={}: {} vs {}",
                anchor.t,
                angles.gamb,
                anchor.gamb
            );
            assert!(
                (angles.phib - anchor.phib).abs() < 1e-13,
                "phib at t={}: {} vs {}",
                anchor.t,
                angles.phib,
                anchor.phib
            );
            assert!(
                (angles.psib - anchor.psib).abs() < 1e-13,
                "psib at t={}: {} vs {}",
                anchor.t,
                angles.psib,
                anchor.psib
            );
            assert!(
                (angles.epsa - anchor.epsa).abs() < 1e-13,
                "epsa at t={}: {} vs {}",
                anchor.t,
                angles.epsa,
                anchor.epsa
            );
            assert!(
                (dpsi - anchor.dpsi).abs() < 1e-13,
                "dpsi at t={}: {dpsi} vs {}",
                anchor.t,
                anchor.dpsi
            );
            assert!(
                (deps - anchor.deps).abs() < 1e-13,
                "deps at t={}: {deps} vs {}",
                anchor.t,
                anchor.deps
            );
        }
    }

    #[test]
    fn matrix_matches_the_erfa_anchors() {
        for anchor in &ANCHORS {
            let matrix = bias_precession_nutation(centuries(anchor.t));
            for i in 0..3 {
                for j in 0..3 {
                    assert!(
                        (matrix[i][j] - anchor.matrix[i][j]).abs() < 1e-14,
                        "matrix[{i}][{j}] at t={}: {} vs {}",
                        anchor.t,
                        matrix[i][j],
                        anchor.matrix[i][j]
                    );
                }
            }
        }
    }

    #[test]
    fn matrix_is_orthonormal() {
        for anchor in &ANCHORS {
            let matrix = bias_precession_nutation(centuries(anchor.t));
            for i in 0..3 {
                for j in 0..3 {
                    let dot = (0..3)
                        .map(|k| matrix[i][k] * matrix[j][k])
                        .sum::<f64>();
                    let expected = if i == j { 1.0 } else { 0.0 };
                    assert!(
                        (dot - expected).abs() < 1e-12,
                        "row dot product [{i}][{j}] at t={} is {dot}",
                        anchor.t
                    );
                }
            }
        }
    }

    #[test]
    fn true_obliquity_is_mean_minus_nutation() {
        let anchor = &ANCHORS[2]; // J2000
        let eps_true = true_obliquity(centuries(anchor.t));
        let expected = anchor.epsa - anchor.deps;
        assert!(
            (eps_true - expected).abs() < 1e-15,
            "true obliquity {eps_true} vs expected {expected}"
        );
    }
}
