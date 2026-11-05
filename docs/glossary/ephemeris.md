---
title: Ephemeris
description: An Ephemeris provides celestial positions for specified times under explicit calculation settings.
---

## Ephemeris

An Ephemeris provides celestial positions for specified times under explicit calculation settings. A calculated
[Astrology Chart](astrology-chart.md) or [Astrocartography](astrocartography.md) map records its provider, version, time
standard, coordinate frame, and relevant calculation settings so the result can be reproduced.

Navigator's calculated specimen map uses fixed inputs and records its calculation provenance. Personal birth inputs
remain request-local; the public birth form resolves a place and time without persisting them.

## Navigator's ephemeris implementation

Navigator computes positions itself instead of calling an external provider. The `ephemeris` workspace crate reads
NASA/JPL DAF/SPK kernels and interpolates barycentric positions in kilometers, so a chart is reproducible from pinned
kernel bytes plus the conversion settings recorded here.

- **Kernels** — `de440.bsp` (type-2 Chebyshev segments), covered 1549-12-21 through 2650-01-25 TDB, plus a JPL
  Horizons-generated SPK for 2060 Chiron (type-21 modified-difference-array segments), covered 1899-12-31 through
  2050-01-02 TDB. Kernel files stay outside the repository; `ephemeris/manifest.json` pins each file's SHA-256 digest,
  byte size, source URL, retrieval UTC, upstream version, and coverage window.
- **Loading** — the boot-time loader verifies every file's byte size and SHA-256 before parsing and refuses any
  mismatch. Storage resolves through the `EphemerisStore` trait; `FilesystemStore` rooted at `$NAVIGATOR_EPHEMERIS_DIR`
  serves development and tests, and production storage (for example an object store) remains an open decision the trait
  keeps reversible.
- **Time standard** — civil UTC converts to TDB through hifitime: UTC→TAI applies hifitime's built-in leap-second table
  (TAI−UTC is 37 s from 2017: 27 inserted leap seconds plus the initial 10 s; pre-1972 UTC drift is modeled), TAI→TT
  adds the fixed 32.184 s offset, and TT→TDB adds the periodic relativistic term (amplitude ≈ 1.6 ms). Classical delta-T
  (TT−UT1) is not consumed by the lookup; SPK segments are functions of TDB only, and delta-T belongs to Earth-rotation
  quantities such as the ascendant and midheaven, computed by later layers.
- **Reference frame** — ICRF/J2000 (SPK frame id 1). Segments declared in any other frame are refused rather than
  rotated.
- **Verification** — the crate's tests interpolate a checked-in table of JPL Horizons reference positions (every DE440
  body plus Chiron, 1900–2050) within tolerances derived from the documented DE441–DE440 agreement: about one meter for
  planets across DE440's coverage, about ten meters for the Moon a century from present.

The mean ascending lunar node uses the IERS Conventions (2003) longitude from ERFA 2.0.1, with IAU 2000B nutation and
IAU 2006 true obliquity. The calculation provenance names these models. Swiss Ephemeris 2.10.03 supplies independent
validation fixtures; the mean-node grid retains a one-arcsecond comparison limit. ERFA-derived routines retain the
upstream notice in the repository `NOTICE`.

## Private calculation boundary

`AstrologyChartProvider` loads a version-2 manifest and calculates locally. It makes no network calls and creates no
persistent records. Its owner chooses the authorized sharing scope, bounded FIFO cache capacity, and lifetime.
`clear_cache()` releases retained results; existing callers may still hold their returned references. Cache identity
includes the original civil input and precision, resolved instant, coordinates, body order, every setting, IANA rules
version, algorithm version, and the complete source manifest. Concurrent callers receive their own original input.

A [Birth Time](birth-time.md) resolves through pinned IANA rules. The initial calculation window is 1900-01-01 inclusive
to 2050-01-01 exclusive UTC. The whole uncertainty interval must fit that window. Unknown times retain only the date and
zone and return no positions or angles. Approximate times return nominal positions with their stated uncertainty,
without houses or angles. Civil leap-second input is rejected.

| Convention | Supported selection |
| --- | --- |
| Zodiac | Tropical |
| Observer | Geocentric |
| Frame | True equator and ecliptic of date, IAU 2006/2000B |
| North Node | Mean ascending node, IERS 2003 |
| Refraction | None |
| Houses | None, Equal, Whole Sign |

Sidereal/Lahiri, topocentric, true-node, refracted, raw ICRF chart output, and Placidus requests return an explicit
error. There is no substituted house system. At a pole or degenerate horizon/ecliptic intersection, angles and houses
are absent. Equal houses start at the ascendant; Whole Sign starts at the beginning of its tropical sign.

The selectable inventory is Sun, Moon, Mercury, Venus, Mars, Jupiter, Saturn, Uranus, Neptune, Pluto, Chiron, and the
mean ascending lunar node. It includes no fixed-star catalog or additional minor bodies. Each physical body records its
SPK target ID. The node has neither a target ID nor a physical distance. Empty selections request angles only; repeated
bodies are rejected.

Positions include ecliptic longitude/latitude and right ascension/declination in degrees, plus light-time-corrected
physical distance in kilometers. Signed speeds use a centered 60-second difference, in degrees/day and kilometers/day;
longitude differences wrap across zero. The apparent solver applies light time, annual aberration, precession, and
nutation. It does not apply gravitational light deflection or atmospheric refraction. Position lookup evaluates native
SPK polynomials rather than interpolating a daily table.

Provenance distinguishes UTC, TT, TDB, and modeled UT1. The existing sidereal layer approximates TT by TDB within about
1.7 ms when subtracting delta-T. Its delta-T convention is recorded separately from the orbital source. Future UTC and
Earth rotation follow pinned models and do not promise future observed precision. Calculation errors omit birth input
and derived position values; personal request and response types have no debug formatting.

Run the synthetic full-inventory benchmark with exact manifest files available locally:

```bash
NAVIGATOR_EPHEMERIS_DIR=/path/to/pinned-kernels cargo run -p ephemeris --release --example chart_benchmark
```

It reports verified loading, first calculation, and 100-run uncached/cache-hit medians. It prints durations only and is
an observation tool rather than a timing assertion in CI. Swiss house fixtures validate the angle construction with
explicit sidereal angle and obliquity inputs, independently of the chosen Earth-rotation prediction model.
