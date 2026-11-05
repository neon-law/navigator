---
title: Delta T
description: Delta T is TT minus UT1, the time correction connecting uniform clocks to Earth's rotation.
---

## Delta T

Delta T is the difference in seconds between Terrestrial Time (TT) and Earth-rotation time (UT1). An
[Ephemeris](ephemeris.md) uses it to orient an [Astrology Chart](astrology-chart.md) or
[Astrocartography](astrocartography.md) map relative to Earth's horizon and meridian. Orbital SPK interpolation uses
Barycentric Dynamical Time (TDB), independently of this Earth-rotation correction.

Navigator records whether a correction comes from a historical model, an observation, a published forecast, or
extrapolation. Source URLs, retrieval time, checksums, and the interpolation model make the estimate reproducible.
Quoted source errors retain their stated meaning; an absent error is unknown rather than zero. Extrapolated Earth
rotation has no stated error bound and does not promise future observed precision.
