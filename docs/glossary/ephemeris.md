---
title: Ephemeris
description: An Ephemeris provides celestial positions for specified times under explicit calculation settings.
---

## Ephemeris

An Ephemeris provides celestial positions for specified times under explicit calculation settings. A calculated
[Astrology Chart](astrology-chart.md) or [Astrocartography](astrocartography.md) map records its provider, version, time
standard, coordinate frame, and relevant calculation settings so the result can be reproduced.

Navigator's design specimens contain invented positions and overlays. They do not include a live ephemeris provider or
persist personal birth inputs. Choosing and licensing a provider precedes activating calculated readings.

## Navigator's ephemeris implementation

Navigator computes positions itself instead of calling an external provider. The `ephemeris` workspace crate reads
NASA/JPL DAF/SPK kernels and interpolates barycentric positions in kilometers, so a chart is reproducible from pinned
kernel bytes plus the conversion settings recorded here.

- **Kernels** — `de440.bsp` (type-2 Chebyshev segments), covered 1549-12-21 through 2650-01-25 TDB, plus a
  JPL Horizons-generated SPK for 2060 Chiron (type-21 modified-difference-array segments), covered 1899-12-31 through
  2050-01-01 TDB. Kernel files stay outside the repository; `ephemeris/manifest.json` pins each file's SHA-256 digest,
  byte size, source URL, and coverage window.
- **Loading** — the boot-time loader verifies every file's SHA-256 before parsing and refuses any mismatch. Storage
  resolves through the `EphemerisStore` trait; `FilesystemStore` rooted at `$NAVIGATOR_EPHEMERIS_DIR` serves
  development and tests, and production storage (for example an object store) remains an open decision the trait keeps
  reversible.
- **Time standard** — civil UTC converts to TDB through hifitime: UTC→TAI applies hifitime's built-in leap-second table
  (27 s as of 2017, the table's last entry, with pre-1972 UTC drift modeled), TAI→TT adds the fixed 32.184 s offset,
  and TT→TDB adds the periodic relativistic term (amplitude ≈ 1.6 ms). Classical ΔT (TT−UT1) is not consumed by the
  lookup; SPK segments are functions of TDB only, and ΔT belongs to Earth-rotation quantities such as the ascendant and
  midheaven, computed by later layers.
- **Reference frame** — ICRF/J2000 (SPK frame id 1). Segments declared in any other frame are refused rather than
  rotated.
- **Verification** — the crate's tests interpolate a checked-in table of JPL Horizons reference positions (every DE440
  body plus Chiron, 1900–2050) within tolerances derived from the documented DE441–DE440 agreement: about one meter for
  planets across DE440's coverage, about ten meters for the Moon a century from present.
