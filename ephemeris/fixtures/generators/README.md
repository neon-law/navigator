# Independent numerical references

`state_reference.c` is a standalone validation probe for Swiss Ephemeris 2.10.03, upstream revision
`175e1fcb3108bcd5c0d146c803f51dcf23508012`. It requests Moshier explicitly and records every actual returned flag. No
Swiss data files or library are linked into Navigator's runtime. The fixture records the upstream source manifest and
the probe's SHA-256.

Build against the pinned external source tree after building its static library:

```bash
cc -O2 -I /path/to/swisseph ephemeris/fixtures/generators/state_reference.c \
  /path/to/swisseph/libswe.a -lm -o /tmp/state-reference
/tmp/state-reference
```

The probe evaluates TT JD 2451545.0, corresponding to synthetic UTC 2000-01-01T11:58:55.816 under the pinned clock
model. Center, before, after, and equatorial calls retain their actual flags. The generated numeric rows and source
identities are retained in `../swisseph_state_reference.json`.

Signed angular and range rates use the same centered 60-second definition as Navigator. The toolkit's reported velocity
fields are separate observations and are not substituted for derivatives of its reported coordinates or distance.
Longitude differences wrap across zero.

The independent Moshier comparison gates are one arcsecond for apparent directions, one arcsecond/day for angular rates,
ten parts per million for range, and five kilometers/day for range derivatives. These are comparison margins, not
absolute astronomical uncertainties. The native SPK reader has separate, tighter JPL geometric reference vectors. System
barycenters and planet centers retain their distinct source identifiers.

## Civil-clock reference

`time_reference.c` links the same pinned external toolkit and calls `swe_utc_to_jd`. Inputs are synthetic Gregorian
midnights from 1900 through 2000. The toolkit interprets pre-1972 inputs as UT1 and later inputs as UTC; see the
[primary conversion documentation](https://www.astro.com/swisseph/swephprg.2.10.pdf). The probe injects matched
NASA/USNO delta-T anchors so time conversion is compared independently of different Earth-rotation models. It retains
physical TT, UT1, apparent sidereal time, Sun right ascension from the Moshier model, and Equal-house angles at zero
longitude/latitude.

```bash
cc -Wall -Wextra -Werror -O2 -I /path/to/swisseph ephemeris/fixtures/generators/time_reference.c \
  /path/to/swisseph/libswe.a -lm -o /tmp/time-reference
/tmp/time-reference
```

`../swisseph_time_reference.json` retains original numeric output, actual flags, upstream source hashes, and probe hash.
Conversion comparisons allow 0.1 millisecond; sidereal, Sun and chart angles retain a one-arcsecond comparison gate, and
geographic culmination lines retain 0.01 degree. Historical 1961–1971 civil UTC is approximated as UT1 rather than
reconstructed from fractional UTC adjustments. No Swiss runtime source, library or ephemeris data files are distributed.
The existing tighter JPL and Swiss-data lunar/body grids remain separate from this clock/Sun reference.
