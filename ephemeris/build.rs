//! Verify and normalize pinned public USNO snapshots without runtime source I/O.

use anyhow::{ensure, Context, Result};
use chrono::{Datelike, NaiveDate};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fmt::Write as _, fs, path::PathBuf};

#[derive(Deserialize)]
struct Manifest {
    version: u32,
    files: Vec<InputFile>,
}
#[derive(Deserialize)]
struct InputFile {
    name: String,
    source: String,
    upstream_version: String,
    retrieved_at: String,
    sha256: String,
    bytes: u64,
}
struct Sample {
    jd: f64,
    seconds: f64,
    uncertainty: Option<f64>,
    kind: &'static str,
}

fn jd(date: NaiveDate) -> f64 {
    f64::from(date.num_days_from_ce()) + 1_721_424.5
}

fn read_verified(files: &[InputFile]) -> Result<BTreeMap<String, String>> {
    let mut texts = BTreeMap::new();
    for file in files {
        ensure!(
            matches!(
                file.name.as_str(),
                "historic_deltat.data" | "deltat.data" | "deltat.preds"
            ),
            "unknown USNO source file"
        );
        println!("cargo:rerun-if-changed=data/usno/{}", file.name);
        let bytes = fs::read(PathBuf::from("data/usno").join(&file.name))?;
        ensure!(
            u64::try_from(bytes.len())? == file.bytes,
            "USNO source size mismatch"
        );
        let mut digest = String::with_capacity(64);
        for byte in Sha256::digest(&bytes) {
            write!(digest, "{byte:02x}")?;
        }
        ensure!(digest == file.sha256, "USNO source checksum mismatch");
        ensure!(
            texts
                .insert(file.name.clone(), String::from_utf8(bytes)?)
                .is_none(),
            "duplicate USNO source"
        );
    }
    Ok(texts)
}

fn monthly(text: &str) -> Result<Vec<Sample>> {
    let mut samples = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let fields: Vec<_> = line.split_whitespace().collect();
        ensure!(fields.len() == 4, "invalid monthly USNO row");
        let date =
            NaiveDate::from_ymd_opt(fields[0].parse()?, fields[1].parse()?, fields[2].parse()?)
                .context("invalid monthly USNO date")?;
        samples.push(Sample {
            jd: jd(date),
            seconds: fields[3].parse()?,
            uncertainty: None,
            kind: "Observation",
        });
    }
    Ok(samples)
}

fn historic(text: &str, first_monthly: f64) -> Result<Vec<Sample>> {
    let mut samples = Vec::new();
    for line in text.lines().skip(2).filter(|line| !line.trim().is_empty()) {
        let fields: Vec<_> = line.split_whitespace().collect();
        ensure!(fields.len() == 5, "invalid historic USNO row");
        let (year, fraction) = fields[0]
            .split_once('.')
            .context("historic USNO year fraction")?;
        let year: i32 = year.parse()?;
        ensure!(
            matches!(fraction, "000" | "500"),
            "unsupported historic USNO year fraction"
        );
        if year < 1955 {
            continue;
        }
        let start = NaiveDate::from_ymd_opt(year, 1, 1).context("invalid historic USNO year")?;
        let days = NaiveDate::from_ymd_opt(year, 12, 31)
            .context("invalid historic USNO year")?
            .ordinal();
        let time = jd(start)
            + if fraction == "500" {
                f64::from(days) / 2.0
            } else {
                0.0
            };
        if time < first_monthly {
            samples.push(Sample {
                jd: time,
                seconds: fields[1].parse()?,
                uncertainty: Some(fields[2].parse()?),
                kind: "Observation",
            });
        }
    }
    Ok(samples)
}

fn predictions(text: &str, last_monthly: f64) -> Result<Vec<Sample>> {
    let mut samples = Vec::new();
    for line in text.lines().skip(1).filter(|line| !line.trim().is_empty()) {
        let fields: Vec<_> = line.split_whitespace().collect();
        ensure!(fields.len() >= 4, "invalid forecast USNO row");
        let time = fields[0].parse::<f64>()? + 2_400_000.5;
        if time > last_monthly {
            samples.push(Sample {
                jd: time,
                seconds: fields[2].parse()?,
                uncertainty: Some(fields.last().context("forecast uncertainty")?.parse()?),
                kind: "Forecast",
            });
        }
    }
    Ok(samples)
}

fn generated_source(samples: Vec<Sample>, files: &[InputFile]) -> Result<String> {
    let mut output = String::from("pub(super) const SAMPLES: &[Sample] = &[\n");
    for sample in samples {
        writeln!(
            output,
            "Sample {{ jd: {:?}, seconds: {:?}, uncertainty: {:?}, kind: DeltaTKind::{} }},",
            sample.jd, sample.seconds, sample.uncertainty, sample.kind
        )?;
    }
    output.push_str("];\npub(super) const SOURCES: &[DeltaTDataFile] = &[\n");
    for file in files {
        writeln!(output, "DeltaTDataFile {{ name: {:?}, source: {:?}, upstream_version: {:?}, retrieved_at: {:?}, sha256: {:?}, bytes: {} }},", file.name, file.source, file.upstream_version, file.retrieved_at, file.sha256, file.bytes)?;
    }
    output.push_str("];\n");
    Ok(output)
}

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=data/usno/manifest.json");
    let manifest: Manifest = serde_json::from_str(&fs::read_to_string("data/usno/manifest.json")?)?;
    ensure!(manifest.version == 1, "unsupported USNO source manifest");
    let texts = read_verified(&manifest.files)?;
    let mut samples = monthly(texts.get("deltat.data").context("missing monthly source")?)?;
    let first = samples.first().context("empty monthly data")?.jd;
    let last = samples.last().context("empty monthly data")?.jd;
    samples.extend(historic(
        texts
            .get("historic_deltat.data")
            .context("missing historic source")?,
        first,
    )?);
    samples.extend(predictions(
        texts
            .get("deltat.preds")
            .context("missing forecast source")?,
        last,
    )?);
    samples.sort_by(|a, b| a.jd.total_cmp(&b.jd));
    ensure!(samples.len() > 2, "insufficient USNO samples");
    ensure!(
        samples.windows(2).all(|pair| pair[0].jd < pair[1].jd),
        "USNO samples must be distinct"
    );
    ensure!(
        samples.iter().all(|sample| sample.jd.is_finite()
            && sample.seconds.is_finite()
            && sample.uncertainty.is_none_or(|u| u.is_finite() && u >= 0.0)),
        "nonfinite USNO sample"
    );
    let output = PathBuf::from(std::env::var_os("OUT_DIR").context("Cargo output directory")?);
    fs::write(
        output.join("delta_t_samples.rs"),
        generated_source(samples, &manifest.files)?,
    )?;
    Ok(())
}
