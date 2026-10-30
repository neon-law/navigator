#!/usr/bin/env python3
"""Rebuild the bundled GeoNames city/country index for the `geotime` crate.

Downloads the GeoNames `cities15000` and `countryInfo` dumps, filters them to
the columns the crate needs, dedupes cities by (ASCII name, country) keeping
the most populous, and writes:

    geotime/data/cities.tsv      one row per city, sorted for the crate's
                                 binary-searched index
    geotime/data/countries.tsv   ISO 3166-1 codes and country names
    geotime/data/SOURCE.md       provenance: source URLs, dump checksums,
                                 download date, and the CC-BY 4.0 attribution

GeoNames data is licensed CC BY 4.0 (https://creativecommons.org/licenses/by/4.0);
attribution must survive any redistribution — see NOTICE and
THIRD-PARTY-NOTICES.txt.

Usage:
    python3 scripts/geonames-fetch.py [--cities-zip PATH] [--countries PATH]

`--cities-zip` and `--countries` accept previously downloaded copies; without
them the script fetches the current dumps from download.geonames.org. The
script needs only the Python standard library.
"""

import argparse
import csv
import hashlib
import io
import sys
import tempfile
import urllib.request
import zipfile
from datetime import datetime, timezone
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DATA_DIR = REPO_ROOT / "geotime" / "data"

CITIES_URL = "https://download.geonames.org/export/dump/cities15000.zip"
COUNTRIES_URL = "https://download.geonames.org/export/dump/countryInfo.txt"
LICENSE_URL = "https://creativecommons.org/licenses/by/4.0"

# cities15000.txt columns (see GeoNames readme): geonameid, name, ascii name,
# alternate names, latitude, longitude, feature class, feature code, country
# code, cc2, admin1, admin2, admin3, admin4, population, elevation, dem,
# timezone, modification date.
COL_NAME = 1
COL_ASCII = 2
COL_LAT = 4
COL_LON = 5
COL_COUNTRY = 8
COL_POPULATION = 14
COL_TIMEZONE = 17

# countryInfo.txt columns: ISO, ISO3, ISO-numeric, fips, Country, ...
COL_ISO2 = 0
COL_ISO3 = 1
COL_COUNTRY_NAME = 4


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def fetch(url: str, cache: Path | None) -> Path:
    if cache is not None:
        return cache
    tmp = Path(tempfile.mkstemp(suffix=Path(url).name)[1])
    with urllib.request.urlopen(url) as response, tmp.open("wb") as out:
        out.write(response.read())
    return tmp


def normalize(name: str) -> str:
    """Lowercase and collapse whitespace — the same normalization the crate
    applies to queries, so the sorted keys line up with binary search."""
    return " ".join(name.casefold().split())


def load_cities(zip_path: Path) -> list[tuple[str, str, str, str, str, str, str]]:
    with zipfile.ZipFile(zip_path) as archive:
        member = next(
            name for name in archive.namelist() if name.endswith(".txt")
        )
        raw = archive.read(member).decode("utf-8")

    best: dict[tuple[str, str], tuple[str, str, str, str, str, str, str, int]] = {}
    for line in raw.splitlines():
        if not line.strip():
            continue
        fields = line.split("\t")
        name, ascii_name = fields[COL_NAME], fields[COL_ASCII]
        country = fields[COL_COUNTRY]
        timezone_name = fields[COL_TIMEZONE]
        if not name or not ascii_name or not country or not timezone_name:
            continue
        lat, lon = fields[COL_LAT], fields[COL_LON]
        try:
            population = int(fields[COL_POPULATION] or "0")
        except ValueError:
            population = 0
        row = (name, ascii_name, country, lat, lon, timezone_name, str(population))
        key = (normalize(ascii_name), country)
        prior = best.get(key)
        if prior is None or population > int(prior[6]):
            best[key] = row

    return sorted(best.values(), key=lambda row: (normalize(row[1]), row[2]))


def load_countries(path: Path) -> list[tuple[str, str, str]]:
    rows = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        fields = line.split("\t")
        iso2, iso3, name = fields[COL_ISO2], fields[COL_ISO3], fields[COL_COUNTRY_NAME]
        if len(iso2) == 2 and len(iso3) == 3 and name:
            rows.append((iso2, iso3, name))
    return sorted(rows, key=lambda row: normalize(row[2]))


def write_tsv(path: Path, rows: list[tuple[str, ...]]) -> None:
    buffer = io.StringIO()
    writer = csv.writer(buffer, delimiter="\t", lineterminator="\n",
                        quoting=csv.QUOTE_NONE, escapechar=None)
    writer.writerows(rows)
    path.write_text(buffer.getvalue(), encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cities-zip", type=Path, help="previously downloaded cities15000.zip")
    parser.add_argument("--countries", type=Path, help="previously downloaded countryInfo.txt")
    args = parser.parse_args()

    cities_zip = fetch(CITIES_URL, args.cities_zip)
    countries_txt = fetch(COUNTRIES_URL, args.countries)

    downloaded = datetime.now(timezone.utc).strftime("%Y-%m-%d")
    cities = load_cities(cities_zip)
    countries = load_countries(countries_txt)

    DATA_DIR.mkdir(parents=True, exist_ok=True)
    write_tsv(DATA_DIR / "cities.tsv", cities)
    write_tsv(DATA_DIR / "countries.tsv", countries)

    with zipfile.ZipFile(cities_zip) as archive:
        member = next(n for n in archive.namelist() if n.endswith(".txt"))
        member_bytes = archive.read(member)
    cities_sha = hashlib.sha256(member_bytes).hexdigest()
    countries_sha = sha256(countries_txt)

    source = f"""\
# Bundled geodata provenance

The `geotime` crate embeds `cities.tsv` and `countries.tsv` from this directory.
Both are filtered extracts of the GeoNames free database, regenerated by
`scripts/geonames-fetch.py` — never edited by hand.

- Sources: <{CITIES_URL}> and
  <{COUNTRIES_URL}>
- Downloaded: {downloaded}
- Cities dump SHA-256 (`cities15000.txt`): `{cities_sha}`
- Countries dump SHA-256 (`countryInfo.txt`): `{countries_sha}`
- City rows: {len(cities)} · Country rows: {len(countries)}

GeoNames is licensed under CC BY 4.0 <{LICENSE_URL}>. Attribution — "GeoNames,
<https://www.geonames.org/>, licensed under CC BY 4.0" — must accompany any
redistribution of the bundled index, including compiled binaries that embed
it. See NOTICE and THIRD-PARTY-NOTICES.txt.

To regenerate with a later dump, rerun the script, commit the new data files,
and update the checksums and date above.
"""
    (DATA_DIR / "SOURCE.md").write_text(source, encoding="utf-8")

    print(f"cities.tsv: {len(cities)} rows; countries.tsv: {len(countries)} rows")
    print(f"downloaded {downloaded}; cities sha256 {cities_sha}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
