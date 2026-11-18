use crate::remote;
use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
pub use cli::inbox_import::*;
use serde_json::{json, Value};
use std::{path::PathBuf, time::Duration};

/// A mapped metadata-only Fi import; apply is deliberately opt-in.
pub struct NewFiImport {
    pub file: PathBuf,
    pub mapping: PathBuf,
    pub account: String,
    pub brand: String,
    pub project: Option<String>,
    pub apply: bool,
    pub host: Option<String>,
}

fn fi_request(new: &NewFiImport, mapping: &store::fi_import::Mapping, bytes: &[u8]) -> Value {
    json!({"account":new.account,"brand":new.brand,"project":new.project,
        "mapping":mapping,"payload_base64":base64::engine::general_purpose::STANDARD.encode(bytes),
        "dry_run":!new.apply})
}

/// Preview or apply a complete mapped Fi CSV. Neither branch prints raw bytes.
pub(crate) async fn import_fi(new: NewFiImport) -> Result<()> {
    if std::fs::metadata(&new.mapping)?.len() > 65_536 {
        return Err(anyhow!("Fi mapping exceeds 64 KiB"));
    }
    if std::fs::metadata(&new.file)?.len() > u64::try_from(store::fi_import::MAX_CSV_BYTES)? {
        return Err(anyhow!("Fi CSV exceeds its byte limit"));
    }
    let mapping: store::fi_import::Mapping = serde_json::from_slice(&std::fs::read(&new.mapping)?)
        .context("parse explicit Fi mapping")?;
    let bytes = std::fs::read(&new.file).context("read Fi metadata CSV")?;
    // A malformed or ambiguous file never reaches even the authenticated preview.
    store::fi_import::parse(&mapping, &new.account, &bytes).context("validate Fi metadata CSV")?;
    let (base, token) = remote::resolve(new.host.as_deref())?;
    let response=import_http_client(&base)?.post(format!("{base}/app/api/inbox/import/fi"))
        .timeout(Duration::from_secs(120)).bearer_auth(token).json(&fi_request(&new,&mapping,&bytes))
        .send().await.context("Fi request interrupted; rerun the unchanged batch to reconcile stable source identities")?;
    let status = response.status();
    let text = response.text().await.context("read Fi import result")?;
    let report: Value = serde_json::from_str(&text).context("parse Fi import result")?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !status.is_success() {
        return Err(anyhow!(
            "Fi import refused: {status}; inspect the row diagnostics and committed IDs above"
        ));
    }
    Ok(())
}
