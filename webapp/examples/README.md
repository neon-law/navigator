# Calculated chart reference

`chart_specimen.rs` regenerates the public design gallery's fixed Astrology Chart. It accepts no personal inputs. The
reference uses 2000-01-01 at 12:00 UTC, latitude zero, longitude zero, tropical true-of-date coordinates, equal houses,
and the mean north node. Its explicit inventory is the Sun, Moon, Mercury through Pluto, and the mean node. Minor bodies
are excluded.

Set `NAVIGATOR_EPHEMERIS_DIR` to a directory containing the exact DE440 file pinned in `ephemeris/manifest.json`. The
generator verifies the pin and fails if the source is unavailable; it does not download or replace source files.

```bash
cargo run -p webapp --features server --example chart_specimen > /tmp/chart-specimen.json
```

Review the generated metadata and numeric changes before replacing `webapp/src/assets/chart-specimen.json`. The JSON
retains full body states, source identifiers, manifest pins, clock conversions, calculation settings, and the SHA-256 of
the generator source. Format the generator before regenerating so that its hash identifies the final file.

```bash
cargo test -p webapp --features server --lib astrology_chart::
```

With the pinned kernel directory set, the reference test recomputes the chart and checks its stored positions and house
cusps against the provider. Rendering uses the checked-in JSON without loading a kernel at request time. Personal
calculations must remain on a separately authorized private surface.
