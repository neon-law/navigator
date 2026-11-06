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
kernel bytes plus the conversion settings recorded here. Source coverage dates use proleptic Gregorian TDB.

- **Kernels** — `de440.bsp` (type-2 Chebyshev segments), covered 1549-12-31 through 2650-01-25 TDB, plus a JPL
  Horizons-generated SPK for 2060 Chiron (type-21 modified-difference-array segments), covered 1899-12-30 through
  2050-01-02 TDB. Kernel files stay outside the repository; `ephemeris/manifest.json` pins each file's SHA-256 digest,
  byte size, source URL, retrieval UTC, upstream version, and coverage window.
- **Loading** — the boot-time loader verifies every file's byte size and SHA-256 before parsing and refuses any
  mismatch. Storage resolves through the `EphemerisStore` trait; `FilesystemStore` rooted at `$NAVIGATOR_EPHEMERIS_DIR`
  serves development and tests, and production storage (for example an object store) remains an open decision the trait
  keeps reversible.
- **Time standard** — normalized civil birth labels before 1972 are UT1. Adding the pinned delta-T model gives physical
  TT. From 1972, hifitime converts UTC→TAI→TT using pinned IERS leap seconds and TT = TAI + 32.184 s; TAI−UTC is 37 s
  from 2017. Both paths convert TT→TDB with the periodic relativistic term, bounded by about 1.7 ms. SPK segments are
  functions of physical TDB. The historical civil convention approximates UTC without reconstructing fractional
  1961–1971 adjustments.
- **Reference frame** — ICRF/J2000 (SPK frame id 1). Segments declared in any other frame are refused rather than
  rotated.
- **Verification** — JPL geometric comparison gates are 0.01 km for tested major-body states, 0.1 km for the Moon,
  and 50 km for Chiron. They are software/model cross-check margins, not absolute astronomical uncertainties.

The mean ascending lunar node uses the IERS Conventions (2003) longitude from ERFA 2.0.1, with IAU 2000B nutation and
IAU 2006 true obliquity. The calculation provenance names these models. Swiss Ephemeris 2.10.03 supplies independent
validation fixtures; the mean-node grid retains a one-arcsecond comparison limit. ERFA-derived routines retain the
upstream notice in the repository `NOTICE`.

## Local raw-source acquisition

After `navigator site login --host <deployment>`, run:

```bash
navigator ephemeris download --host <deployment> --dir ./jpl-reference
```

The deployment authorizes the signed session as Owner or Admin through `GET /app/api/ephemeris/sources` before any
source transfer or local bundle check. Cached credential roles do not grant access, and scoped CI sessions cannot use
this door. The bounded version-2 catalogue contains only the checked-in DE440 pin. The CLI rejects different versions,
source URLs, hashes, sizes, additional fields, or files rather than adopting new pins.

The CLI obtains `de440.bsp` directly from its pinned NAIF URL using a separate client without Navigator credentials;
neither client follows redirects. [NAIF's kernel rules](https://naif.jpl.nasa.gov/naif/rules.html) permit downloading
and using kernels on its server and redistributing unmodified NAIF kernels. The [official planetary kernel
directory](https://naif.jpl.nasa.gov/pub/naif/generic_kernels/spk/planets/) distributes DE440. Generated Horizons
kernels are outside this acquisition catalogue.

`--dir` names an immutable completed bundle containing the ordinary raw `de440.bsp` file and `source-manifest.json`. The
CLI verifies the exact byte count and SHA-256 in a sibling staging directory before atomic publication without
replacement. A repeated command accepts only the same fully verified bundle. Existing empty directories, different or
corrupt contents, links, and concurrent destination creation are refused without replacing user data. Failed transfers
remove only their own staging directory. This command acquires raw reference sources; it creates no personal chart
export or derived numerical table.

## Private calculation boundary

`AstrologyChartProvider` loads a version-2 manifest and calculates locally. It makes no network calls and creates no
persistent records. Its owner chooses the authorized sharing scope, bounded FIFO cache capacity, and lifetime.
`clear_cache()` releases retained results; existing callers may still hold their returned references. Cache identity
includes the original civil input and precision, resolved instant, coordinates, body order, every setting, IANA rules
version, algorithm version, and the complete source manifest. Concurrent callers receive their own original input.

A [Birth Time](birth-time.md) resolves through pinned IANA rules. The initial calculation window is 1900-01-01 inclusive
to 2050-01-01 exclusive in the normalized universal civil calendar. The whole uncertainty interval must fit that window.
Unknown times retain only the date and zone and return no positions or angles. Approximate times return nominal
positions with their stated uncertainty, without houses or angles. Civil leap-second input is rejected.

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

Provenance records the civil calendar and its UT1/UTC scale, physical TT/TDB, and explicit UT1. Historical inputs have
no invented UTC reading. Modern Earth rotation uses TT minus [Delta T](delta-t.md), with pinned USNO observations and
forecasts and explicit extrapolation beyond the published forecast. Historical dates before 1955 use the published
Espenak-Meeus polynomials. The quoted source error is separate from a guaranteed model error; missing errors and
extrapolation are not labeled zero uncertainty. Precession, nutation, and node series use TDB as a TT approximation
within about 1.7 ms. Future UTC follows the pinned clock model. Calculation errors omit birth input and derived
positions; personal request and response types have no debug formatting.

The calculation provenance and cache identity include every numerical model and the original Earth-rotation source URLs,
retrieval UTC, upstream snapshot identity, SHA-256, and byte count. These public source data are verified and normalized
at compile time; requests perform no source-network calls. The source contract is in
[`ephemeris/data/usno/README.md`](../../ephemeris/data/usno/README.md).

Run the synthetic full-inventory benchmark with exact manifest files available locally:

```bash
NAVIGATOR_EPHEMERIS_DIR=/path/to/pinned-kernels cargo run -p ephemeris --release --example chart_benchmark
```

It reports verified loading, first private calculation, and 100 observations per operation: request-local
`calculate_uncached`, bounded-cache miss, independently prewarmed cache hit, canonical geometry construction, ordered
numerical-row materialization, and GeoJSON encoding. Cache clearing and hit prewarming occur outside timed calls;
returned-value destruction also occurs outside each timed operation. Geometry and encoding use the same fixed synthetic
full-inventory chart. Output contains durations and counts, without birth inputs, positions, epochs or provenance.

Summaries use nearest-rank p50 and p95: sort observations and select the one-based rank `ceil(p * count / 100)`. For an
even count, nearest-rank p50 selects the lower middle observation rather than an averaged median. These are local
measurements, without a CI latency threshold or an inference about source accuracy. The benchmark measures native
SPK-backed calculations; it supplies no sampled-table interpolation or replacement astronomy engine.

Swiss house fixtures validate the angle construction with explicit sidereal angle and obliquity inputs, independently of
the chosen Earth-rotation prediction model.

Resolved chart results carry a `ChartInstant`, pairing the physical epoch with explicit UT1. Use `gast_degrees_for`,
`lines_for_instant`, and `LineProvenance::for_instant` for these results. Raw `Epoch` line APIs interpret their input
through hifitime's proleptic UTC model and do not resolve historical birth clocks. The algorithm identity is chart-v3.
