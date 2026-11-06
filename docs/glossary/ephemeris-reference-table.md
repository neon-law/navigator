---
title: Ephemeris Reference Table
description: An Ephemeris Reference Table contains exact daily celestial samples without personal chart inputs.
---

## Ephemeris Reference Table

An Ephemeris Reference Table contains reusable celestial positions at universal civil midnight, with explicit time,
frame, source and numerical conventions. It contains no person, Project, matter, birth location, timezone, consent,
chart request, house calculation or result-cache record.

`ephemeris::EphemerisReferenceTable::calculate` reads the pinned DE440 bytes through `EphemerisStore` and returns an
owned in-memory table. It validates the interval before reading sources. It creates no file, snapshot identifier,
derived-release manifest or publication. The [Ephemeris](ephemeris.md) raw-source manifest remains distinct from this
row contract.

## Sampling and inventory

The interval is start-inclusive and end-exclusive, inside `[1900-01-01, 2050-01-01)`. Each batch contains 1 through 366
days and at most 4,026 rows. This is an in-memory resource bound, not a smaller dataset coverage policy. Each calendar
midnight is constructed afresh; UTC leap seconds make some adjacent physical epochs 86,401 seconds apart.

Dates before 1972 use UT1, with no invented historical UTC reading. Dates from 1972 use UTC and the pinned leap-second
table. The calendar is proleptic Gregorian. No timezone or unknown birth-clock inference occurs. Every sample uses the
native exact-instant SPK evaluator; daily-row interpolation is unsupported.

Rows are ordered by date, then by this fixed eligible inventory:

| Body key | Source ID | Source identity |
| --- | --- | --- |
| `sun` | 10 | DE440 Sun |
| `moon` | 301 | DE440 Moon center |
| `mercury` | 199 | DE440 planet center |
| `venus` | 299 | DE440 planet center |
| `mars` | 4 | DE440 system barycenter |
| `jupiter` | 5 | DE440 system barycenter |
| `saturn` | 6 | DE440 system barycenter |
| `uranus` | 7 | DE440 system barycenter |
| `neptune` | 8 | DE440 system barycenter |
| `pluto` | 9 | DE440 system barycenter |
| `mean_node` | null | IERS 2003 mean ascending lunar node |

The Earth center is the observer, not a selectable direction. Chiron, other minor bodies and fixed stars are outside
this reference inventory. Only the compiled eligible DE440 source is read; no Chiron kernel is required or mirrored.

## Row dictionary

| Field | Representation and meaning |
| --- | --- |
| `reference_calendar` | Universal Gregorian midnight without a UTC suffix. |
| `reference_scale` | `ut1` before 1972, otherwise `utc`. |
| `julian_day_utc` | UTC Julian day, null for historical UT1 references. |
| `tt_nanoseconds_since_j2000` | Signed 64-bit coordinate from 2000-01-01T12:00:00 **TT**, in nanoseconds. |
| `tdb_nanoseconds_since_j2000` | Signed 64-bit coordinate from 2000-01-01T12:00:00 **TDB**, in nanoseconds. |
| `spk_tdb_seconds_since_j2000` | Actual floating-point seconds passed to the native SPK evaluator. |
| `model_julian_day_ut1` | Explicit modeled Earth-rotation reading, not live Earth-orientation data. |
| `delta_t_seconds` | Model TT minus UT1, seconds. |
| `delta_t_kind` | `historical_model`, `observation`, `forecast` or `extrapolation`. |
| `delta_t_reported_error_seconds` | Quoted source error only; null means unavailable. |
| `body` | Stable key from the inventory above. |
| `source_id` | Actual SPK target ID, null for the analytic mean node. |
| `longitude_deg`, `latitude_deg` | Tropical ecliptic-of-date direction, degrees; longitude is in `[0, 360)`. |
| `right_ascension_deg`, `declination_deg` | True-equator-of-date direction, degrees; RA is in `[0, 360)`. |
| `distance_km` | Emission-time geocentric range in kilometers, null for the mean node. |
| `longitude_speed_deg_per_day`, `latitude_speed_deg_per_day` | Signed angular derivatives, degrees per TDB day. |
| `distance_speed_km_per_day` | Signed derivative of the corrected range, kilometers per TDB day; null for the node. |

The TT and TDB origins are noon in their respective scales and are different physical instants. Integer coordinates
preserve computed clock readings without claiming nanosecond astronomical accuracy. The actual floating-point SPK
argument is retained separately, without rounding positions or rates. Each derivative uses a centered window of
plus/minus 30 TDB-coordinate seconds and a day of 86,400 TDB-coordinate seconds, rather than a UTC civil day.

## Models and versioning

Metadata records the row schema version, Navigator release, shared numerical algorithm, exact raw-source manifest, USNO
delta-T source hashes/model, IERS node convention, and explicit calculation settings. The raw SPK frame is ICRF/J2000;
the output frame is the true equator and ecliptic of date, using IAU 2006 precession/frame bias, IAU 2000B nutation and
true obliquity. Physical SPK directions include light time and annual aberration. The mean node follows its separate
analytic convention. Gravitational deflection and atmospheric refraction are not applied.

The row schema version, numerical algorithm version and raw-source manifest version identify different contracts. Frame,
units and time scale are explicit; samples from different conventions must not be silently combined. Node ranges/range
rates and source IDs remain null, as do unavailable quoted delta-T errors. The raw sources provide no position
covariance or guaranteed absolute astronomical error bound through this reader. Numerical validation margins are
software/model cross-checks and do not supply that missing uncertainty. Extrapolated delta-T readings have no quoted
error bound when the underlying model supplies none.

## Authenticated local files

The administrative CLI calculates a bounded table from an immutable raw DE440 bundle acquired by `navigator ephemeris
download`:

```bash
navigator ephemeris tables --host <deployment> --source-dir <raw-bundle> \
  --from 1900-01-01 --to-exclusive 1900-01-03 --dir <new-table-bundle>
```

The protected source catalog authorizes the current Owner/Admin session before any local source or output read,
calculation or reuse. Cached role labels grant no access. This command performs no JPL transfer. It opens the verified
ordinary kernel file without following a link, retains that descriptor, and verifies its bounded bytes again in the
exact evaluator. The raw bundle must contain only `de440.bsp` and its matching `source-manifest.json`.

Without `--iceberg`, the completed directory contains four ordinary files:

| File | Contents |
| --- | --- |
| `reference.parquet` | Snappy-compressed rows with explicit Arrow physical types and stable field IDs 1 through 20. |
| `reference.csv` | The same ordered columns/rows; empty fields represent documented nulls. |
| `source-manifest.json` | The distinct compiled raw-source declaration and checksum pin. |
| `derived-manifest.json` | Coverage, row count, schema, model/settings, source and artifact byte/hash identities. |

TT/TDB coordinates remain signed 64-bit integers. All floating-point columns retain 64-bit values, including columns
whose entire batch is null. Calendar, scale, model-kind and body labels are strings; source IDs are nullable 32-bit
integers. CSV uses round-trip decimal representations for floating-point values and exact integer decimal strings.
Consumers must use the manifest's column types rather than infer historical all-null columns or coerce epochs through
floating-point numbers.

The content snapshot ID is the SHA-256 of the compact `serde_json` content object, with object keys recursively sorted
lexicographically and array order retained. It includes coverage, schema, model/settings, source identity and all
artifact byte/hash identities. Generation time is recorded separately and does not change that identity. The raw
manifest version, row schema version and file-format identity remain separate. A changed source, algorithm, schema or
encoded file produces a different content identity.

Each artifact is limited to 4 MiB and the derived manifest to 64 KiB. The command stages sibling files, verifies bytes,
opens fresh Parquet and CSV readers to compare schema, field IDs, rows, nulls and values, then atomically publishes the
completed directory without replacing an existing path. A failed publication removes only its own staging directory. A
matching rerun reauthorizes, checks coverage/schema/source/version, recalculates the bounded expected table and verifies
exact settings and file bytes; existing files and generation time remain unchanged. Incomplete, unrelated, corrupt,
differently configured or linked output is refused and preserved. Concurrent publication cannot replace a completed
directory.

No DuckDB database, latest-release pointer, public hosting, cloud resources or snapshot-expiry service is created. The
command accepts universal reference dates and a pinned raw bundle only; it has no personal-chart import or export
selector.

## Fixed-location Iceberg layout

`navigator ephemeris tables --iceberg` adds one immutable local Iceberg v2 table over the same reference Parquet file.
It accepts the same raw DE440 bundle and bounded dates. Authorization through the protected catalog precedes output path
canonicalization and every local source/output read, calculation or reuse.

The full canonical output path must be URI-safe ASCII, with no spaces, percent signs, non-ASCII characters or other
characters requiring URI escaping. Unsupported paths are refused before source calculation or staging. The native
filesystem reader strips the `file:` prefix without decoding escaped path characters. The command keeps valid fully
qualified file URIs and refuses unsupported locations rather than substituting an invalid URI or renaming user files.

Iceberg v2 metadata embeds the final absolute file location. The completed table must remain at that location; moving it
does not create a relocatable table. A rerun at a different location is refused and preserves the files. No relative v2
metadata paths, cloud filesystem adapter or implicit relocation are provided.

The opt-in directory retains the four reference files and adds `iceberg-manifest.json` plus a `metadata` directory:

| Metadata file | Contents |
| --- | --- |
| `v1.metadata.json` | One native Iceberg v2 schema, unpartitioned spec, unsorted order and immutable snapshot. |
| `<uuid>-m0.avro` | The native data manifest, naming only the final `reference.parquet` URI. |
| `snap-<native-id>-<uuid>.avro` | The native manifest list for that snapshot. |
| `version-hint.text` | The initial metadata version, `1`. |

The native schema retains field IDs 1 through 20 and the same physical value types. Its fields are optional; the Parquet
file's nonnullable fields remain stricter. Missing or mismatched columns fail validation; documented unavailable
quantities retain their exact null masks. Integer epoch coordinates remain native long values.

The existing v1 `derived-manifest.json` content and reference snapshot ID are unchanged by the mode. The three
Parquet/CSV/source files are byte-identical for the same calculation/settings. Independently created directories each
record their own `generated_at`; a rerun preserves all four reference files byte for byte.

`iceberg-manifest.json` separately binds `reference_snapshot_id`, final table/metadata locations, native table UUID,
snapshot ID, timestamp and each metadata file's size/hash. Its `artifact_id` hashes that actual layout. It can depend on
random UUID filenames, native generation time and final location; it is not a generation-time-independent reference
identity. Native snapshot IDs need uniqueness only within the one immutable table and are not global reference IDs.

Each native metadata file and the layout receipt is limited to 64 KiB. The command reads a closed, exact file inventory
without following links and verifies its checksums. Before publication, genuine native Iceberg readers plan and scan
staged bytes loaded into an explicit memory filesystem under their final URIs. Planned paths must name exactly the
verified reference Parquet file. All 20 ordered columns, schema/field IDs, integer values, floating-point bits, nulls
and row counts must match the computed table. No ambient filesystem or network backend can supply another file. Extra
schema/history/spec/sort/ref/statistics metadata is refused; this command supports only its own new-table shape.

Only after staged reference readers, native Iceberg scans and both receipts pass does the existing atomic NOREPLACE
publication expose the completed directory. Fresh filesystem readers verify the real final table in the interoperability
tests. Reruns reauthorize and recalculate the bounded expected reference data, verify the existing native layout and
preserve its bytes and timestamps; they do not regenerate random metadata for equality. Plain and Iceberg modes refuse
each other's existing directory shapes. There is no append, expiry, deletion or multi-snapshot maintenance command.
