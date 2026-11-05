# Earth-rotation source data

These original USNO files form the pinned 2026-11-05 retrieval snapshot. USNO publishes these products without a release
tag; SHA-256 and retrieval UTC identify the exact version in `manifest.json`. The USNO [Delta T product
page](https://maia.usno.navy.mil/products/deltaT) approves public release with unlimited distribution.

`build.rs` verifies every file's digest and byte count, then generates immutable Rust samples and provenance. Missing,
changed, duplicate, nonfinite, or malformed source data fail the build. Runtime calculations require no data downloads.

| File | Samples used |
| --- | --- |
| `historic_deltat.data` | 1955-01-01 through 1973-01-01, before the monthly series |
| `deltat.data` | Monthly observations from 1973-02-01 through 2026-10-01 |
| `deltat.preds` | Predictions strictly after the last observation through MJD 63871, 2033-10-01 |

Historic fractional years use the elapsed fraction of the actual proleptic Gregorian year. Monthly dates use their
listed Gregorian midnight; predictions use their listed MJD. Observations take precedence over overlapping forecasts.
Linear interpolation joins successive samples, including the observation-to-forecast transition. Before 1955,
[Espenak-Meeus historical polynomials](https://eclipse.gsfc.nasa.gov/SEcat5/deltatpoly.html) supply the model.

After the final prediction, the final segment's slope continues linearly. This is explicit extrapolation, with no
reported error bound. It is a versioned numerical convention, not a new USNO prediction. The chart boundary admits UTC
instants in [1900-01-01, 2050-01-01); full source coverage does not imply uniformly known astronomical accuracy.

The estimate records historical model, observation, forecast, or extrapolation. At a sample, the source's quoted error
is retained. Between samples, both endpoints must quote an error and the larger is retained. These values are not
guaranteed interpolation or prediction bounds. Missing errors remain absent; extrapolation never inherits a bounded
forecast error. UTC-to-TT leap-second assumptions are recorded separately from this TT-minus-UT1 model.

To refresh the snapshot, obtain the three original products, record their exact retrieval UTC and identities, and update
the source manifest together. Cargo verifies and normalizes the files deterministically. Model/source changes require a
distinct numerical algorithm identity and comparison against the independent position, house, and line references. No
source coefficients or interpolation implementation come from Swiss Ephemeris.
