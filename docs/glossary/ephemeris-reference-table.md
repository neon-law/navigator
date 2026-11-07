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

No latest-release pointer, public hosting, cloud resources or snapshot-expiry service is created. The command accepts
universal reference dates and a pinned raw bundle only; it has no personal-chart import or export selector.

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

## Portable DuckDB file

`navigator ephemeris tables --duckdb-bin /path/to/duckdb` adds a materialized `reference.duckdb`, a typed
`duckdb-manifest.json` receipt and tested `query.sql` Moon/date-range query. It combines with `--iceberg`; the ordinary
four-file bundle and its stable reference-content snapshot ID retain the same format. The native receipt separately
binds the actual database/query bytes, reference snapshot and creator executable. Native layout bytes need not match
between independent creations. Reruns verify and preserve the existing files, generation time and timestamps.

The operator supplies an executable expanded from the exact platform asset in the [official DuckDB 2.0.0
release](https://github.com/duckdb/duckdb/releases/tag/v2.0.0), using the [checked-in
pins](../../cli/src/ephemeris_duckdb_pins.json) for macOS arm64 or Linux amd64 GNU/musl. Expanded size and SHA-256 must
match; the version string alone is insufficient. Match the GNU or musl engine to the available Linux runtime. Navigator
does not install or download it. Linux engine runtime requirements remain separate from Navigator's static musl
executable. Authorization through the protected source catalog precedes opening the tool, raw source or output.
Navigator copies the executable through a regular-file descriptor into a private directory, verifies its exact size and
SHA-256, and executes that copy with an empty environment and owned home. User startup files and agents are disabled.

The file explicitly uses storage version `v0.10.0`. Readers tested with DuckDB 2.0.0 on macOS arm64 and Linux amd64
GNU/musl, and 1.4.5 on macOS arm64, retain the full reference schema and values. These are specific tested consumers,
not a promise about every engine version. The 1.4.5 reader is not an approved creator. DuckDB describes its storage
compatibility in the [versioned storage documentation](https://duckdb.org/docs/2.0/internals/storage).

The database contains only the 20-column materialized `ephemeris_reference_table` and three-column
`reference_provenance` table. Provenance retains the reference snapshot ID and exact derived/source manifest JSON; there
are no external views or relations. Integer nanoseconds stay native BIGINT, nullable source IDs stay INTEGER, angular
and clock quantities stay DOUBLE, and calendar/model labels stay VARCHAR. Fresh processes compare all columns, integer
values, floating-point bits and exact null masks; documented unavailable quantities remain null. The database can be
copied without the Parquet source and read independently. An accompanying Iceberg layout retains its own fixed-location
constraint.

Each child starts with clean memory. Autoloading and extension installation are disabled before attaching any database;
external access is disabled and locked after setting an exact private-file allowlist. Creation permits only its owned
Parquet, database and two observed checkpoint/WAL paths. Read-only verification permits only a verified private database
copy and its absent WAL path. Operator WAL files are never copied. No directory-wide access or spill directory is
allowed. The process uses one thread and a 128 MB working-memory limit; SQL input, output and runtime are bounded. SQL
travels through stdin, without shell execution or data in process arguments. Errors omit child output and SQL.

Checkpointing and process termination precede validation. Sidecars must be absent, database/query checksums must match,
and strict schema, object inventory, provenance and row checks must pass before the bundle's existing atomic NOREPLACE
publication. Failure removes only owned stages; completed or unrelated operator artifacts are preserved. The native file
shares the 4 MiB artifact cap. This is one immutable reference file, without append, expiry or analytical-server
lifecycle services.

## Existing-bundle observations

`navigator ephemeris tables --measure` observes an existing matching DuckDB bundle. Supply the same source directory,
coverage and output directory as the completed bundle, an explicit `--duckdb-bin`, and `--iceberg` when that layout is
present. The command never creates a missing bundle or changes its files, permissions, modification times, reference
identity or generation time. A missing, mismatched or corrupt bundle is refused. Server-authoritative Owner/Admin
admission precedes local tool, source and output access; a cached role grants no access.

Source, coverage, schema, exact artifact bytes and native table semantics are verified before observations begin.
Verification recalculates the bounded expected reference rows with the current validated source/model/release. Queries
run against a checksum-verified private read-only database copy using the existing bounded child runner. No arbitrary
SQL, import selector, original WAL file, persistent report file or analytical service is added.

The stdout JSON report binds the reference snapshot ID, coverage-day/row/body counts, Navigator release, OS,
architecture, debug-assertion mode and available parallelism when known. Storage fields contain ordinary file lengths:
Parquet, CSV and DuckDB bytes, auxiliary-file bytes, total file count and the actual Parquet row-group count. These are
logical file sizes, not allocated disk blocks. The current Arrow writer and closed file inventories remain unchanged.

Four fixed queries read every ordered column: all rows, Moon rows, the first 31 calendar days or the whole shorter
coverage, and Moon rows in that window. Their complete results are checked against expected values/nulls outside the
timing phase. The engine uses one thread, a 128 MB working-memory limit and disabled spilling. The report includes the
verified engine version and these limits.

Each query has five independent fresh-process observations. A separate process performs one excluded prewarm followed by
five warm observations in the same process. Engine seconds come from the pinned native query profile. Fresh driver
seconds start before spawning and end after child exit and captured I/O; they include bootstrap, read-only attachment,
query execution and output transfer, but exclude source verification, private copies and JSON decoding. Warm engine
seconds exclude the prewarm and process setup. Validation and cleanup are outside the measured query operations.

A fresh process is not a cold OS page cache or cold storage device. The command neither flushes system caches nor claims
that control. Engine-reported read-byte counters describe engine I/O requests, not physical device reads; they can
exceed file length and need not fall on repeated queries. Raw profiles, SQL text, file paths and ambiguous rows-scanned
counters are omitted from the report.

Summaries use the one-based nearest rank `ceil(p * count / 100)` for p50 and p95. With five samples, p95 is the maximum
observation, not a stable tail-latency estimate or performance guarantee. No timing threshold is a test requirement. For
reproducible engineering evidence, record CPU model, RAM, exact OS version, Cargo build profile/optimization,
concurrency and cache conditions alongside the report and its snapshot identity. OS/architecture and debug-assertion
fields alone do not describe all hardware or build settings.

These observations measure the current bounded inventory. The flat layout is provisional; observations do not promise an
optimal partition policy for a larger inventory or authorize public publication. The source/model/coverage and resource
bounds remain those of the reference table. Query profiling is described in the [DuckDB 2.0
documentation](https://duckdb.org/docs/2.0/sql/statements/profiling).
