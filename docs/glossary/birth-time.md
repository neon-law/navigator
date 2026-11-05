---
title: Birth Time
description: A Birth Time records a civil date, time zone, and the known precision of the clock time.
---

## Birth Time

A Birth Time records a civil date, IANA time zone, and an exact, approximate, or unknown clock time for an [Astrology
Chart](astrology-chart.md). An approximate clock retains its stated uncertainty. An unknown clock retains its date and
zone without an assumed instant.

The [Ephemeris](ephemeris.md) time resolver preserves the original civil input, the resolved UTC instant and offset, and
the IANA rules version. A repeated wall clock requires an explicit offset choice. A skipped wall clock is rejected.
Civil leap-second input is unsupported. Precision-dependent angles and houses require an exact clock.

Birth Time is personal input. Calculation tests use synthetic reference dates, and resolution errors contain no input
values.
