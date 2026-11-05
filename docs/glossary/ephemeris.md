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

- **Kernels** — `de440.bsp` (type-2 Chebyshev segments), covered 1549-12-21 through 2650-01-25 TDB, plus a
  JPL Horizons-generated SPK for 2060 Chiron (type-21 modified-difference-array segments), covered 1899-12-31 through
  2050-01-02 TDB. Kernel files stay outside the repository; `ephemeris/manifest.json` pins each file's SHA-256 digest,
  byte size, source URL, and coverage window.
- **Loading** — the boot-time loader verifies every file's SHA-256 before parsing and refuses any mismatch. Storage
  resolves through the `EphemerisStore` trait; `FilesystemStore` rooted at `$NAVIGATOR_EPHEMERIS_DIR` serves development
  and tests, and production storage (for example an object store) remains an open decision the trait keeps reversible.
- **Time standard** — civil UTC converts to TDB through hifitime: UTC→TAI applies hifitime's built-in leap-second table
  (27 s as of 2017, the table's last entry, with pre-1972 UTC drift modeled), TAI→TT adds the fixed 32.184 s offset, and
  TT→TDB adds the periodic relativistic term (amplitude ≈ 1.6 ms). Classical delta-T (TT−UT1) is not consumed by the
  lookup; SPK segments are functions of TDB only, and delta-T belongs to Earth-rotation quantities such as the ascendant
  and midheaven, computed by later layers.
- **Reference frame** — ICRF/J2000 (SPK frame id 1). Segments declared in any other frame are refused rather than
  rotated.
- **Verification** — the crate's tests interpolate a checked-in table of JPL Horizons reference positions (every DE440
  body plus Chiron, 1900–2050) within tolerances derived from the documented DE441–DE440 agreement: about one meter for
  planets across DE440's coverage, about ten meters for the Moon a century from present.

The mean ascending lunar node uses the IERS Conventions (2003) longitude from ERFA 2.0.1, with IAU 2000B nutation and
IAU 2006 true obliquity. The calculation provenance names these models. Swiss Ephemeris 2.10.03 supplies independent
validation fixtures; the mean-node grid retains a one-arcsecond comparison limit. ERFA-derived routines retain the
upstream notice in the repository `NOTICE`.
