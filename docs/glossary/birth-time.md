---
title: Birth Time
description: A Birth Time records a civil date, time zone, and the known precision of the clock time.
---

## Birth Time

A Birth Time records a civil date, IANA time zone, and an exact, approximate, or unknown clock time for an [Astrology
Chart](astrology-chart.md). An approximate clock retains its stated uncertainty. An unknown clock retains its date and
zone without an assumed instant.

The [Ephemeris](ephemeris.md) time resolver preserves the original civil input, the normalized universal calendar label,
its time scale, the resolved offset, and the IANA rules version. Before 1972 the universal civil label is interpreted as
UT1, a mean solar clock. From 1972 it is UTC with pinned leap seconds. The historical convention approximates civil UTC
without reconstructing the fractional adjustments used in 1961–1971. A repeated wall clock requires an explicit offset
choice. A skipped wall clock is rejected. Civil leap-second input is unsupported. Precision-dependent angles and houses
require an exact clock.

Birth Time is personal input. Calculation tests use synthetic reference dates, and resolution errors contain no input
values.
