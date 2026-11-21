//! Explicit references to operator-owned immutable config and persistent receipts.
use super::{kubectl_json, ShipConfig};
use anyhow::{ensure, Context, Result};
use serde_json::Value;

pub(super) const ENV: &str = "NAVIGATOR_ARCHIVE_WRITE_ONLY_CONFIG";
pub(super) const PATH: &str = "/etc/navigator/archive-write-only/config.json";
pub(super) const LEGACY_BUCKET_ENV_LINE: &str =
    "            - name: NAVIGATOR_ARCHIVES_BUCKET\n              value: YOUR_ARCHIVES_BUCKET\n";
const LOCAL_ROOT: &str = "/var/lib/navigator/iceberg";
const CONFIG: &str = "navigator-archive-write-only-config";
const CLAIM: &str = "navigator-archive-write-only";

pub(super) fn enabled(get: &impl Fn(&str) -> Option<String>) -> Result<bool> {
    let Some(path) = get(ENV) else {
        return Ok(false);
    };
    ensure!(
        path == PATH,
        "{ENV} must name the deployment's mounted configuration path"
    );
    ensure!(
        get("NAVIGATOR_ARCHIVE_SOURCE").is_none_or(|value| value == "postgres"),
        "{ENV} accepts only PostgreSQL archive sources"
    );
    Ok(true)
}

pub(super) fn verify(
    cfg: &ShipConfig,
    get: &impl Fn(&str) -> Option<String>,
    dry_run: bool,
) -> Result<()> {
    if !enabled(get)? {
        return Ok(());
    }
    if dry_run {
        eprintln!("DRY-RUN: would verify immutable write-only archive config and bound shared filesystem claim");
        return Ok(());
    }
    let config = kubectl_json(cfg, "configmap", CONFIG).map_err(|_| {
        anyhow::anyhow!("write-only archive configuration is unavailable; no rollout attempted")
    })?;
    let claim = kubectl_json(cfg, "persistentvolumeclaim", CLAIM).map_err(|_| {
        anyhow::anyhow!("write-only archive receipt claim is unavailable; no rollout attempted")
    })?;
    validate_objects(&config, &claim)
}

fn validate_objects(config: &Value, claim: &Value) -> Result<()> {
    ensure!(
        config["metadata"]["name"] == CONFIG && config["immutable"] == true,
        "write-only archive configuration must be immutable under the expected name"
    );
    let raw = config["data"]["config.json"]
        .as_str()
        .context("write-only archive config.json is missing")?;
    ensure!(
        raw.len() <= 8 * 1024 * 1024,
        "write-only archive configuration exceeds its limit"
    );
    let parsed: archives::write_only::operations::Config = serde_json::from_str(raw)
        .map_err(|_| anyhow::anyhow!("invalid write-only archive configuration"))?;
    parsed
        .validate()
        .map_err(|_| anyhow::anyhow!("invalid write-only archive configuration"))?;
    ensure!(
        parsed.local_root == std::path::Path::new(LOCAL_ROOT),
        "write-only archive local_root must name the persistent receipt mount"
    );
    ensure!(
        claim["metadata"]["name"] == CLAIM
            && claim["status"]["phase"] == "Bound"
            && claim["metadata"].get("deletionTimestamp").is_none(),
        "write-only archive receipt claim must be bound and not deleting"
    );
    ensure!(
        claim["spec"]["volumeMode"].as_str().unwrap_or("Filesystem") == "Filesystem"
            && claim["spec"]["accessModes"]
                .as_array()
                .is_some_and(|modes| modes.iter().any(|mode| mode == "ReadWriteMany")),
        "write-only archive receipts require a shared persistent filesystem across web replicas"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{validate_objects, CLAIM, CONFIG, LOCAL_ROOT};
    use serde_json::json;

    fn inputs() -> (serde_json::Value, serde_json::Value) {
        let config = archives::write_only::operations::Config {
            format_version: 1,
            local_root: LOCAL_ROOT.into(),
            source_project_number: None,
            source_identity_sha256: "2".repeat(64),
            capture: archives::write_only::operations::CaptureScope::RegisteredNavigator,
            destinations: Vec::new(),
        };
        (
            json!({"metadata":{"name":CONFIG},"immutable":true,
            "data":{"config.json":serde_json::to_string(&config).unwrap()}}),
            json!({"metadata":{"name":CLAIM},"status":{"phase":"Bound"},
            "spec":{"accessModes":["ReadWriteMany"],"volumeMode":"Filesystem"}}),
        )
    }

    #[test]
    fn refuses_mutable_invalid_or_unmounted_configuration_without_exposing_its_values() {
        let (config, claim) = inputs();
        validate_objects(&config, &claim).unwrap();
        let mut changed = config.clone();
        changed["immutable"] = json!(false);
        assert!(validate_objects(&changed, &claim).is_err());
        changed = config.clone();
        changed["data"]["config.json"] = json!("not-json");
        assert!(validate_objects(&changed, &claim).is_err());
        let mut body: serde_json::Value =
            serde_json::from_str(config["data"]["config.json"].as_str().unwrap()).unwrap();
        body["local_root"] = json!("/tmp/ephemeral");
        changed = config;
        changed["data"]["config.json"] = json!(serde_json::to_string(&body).unwrap());
        let error = validate_objects(&changed, &claim).unwrap_err().to_string();
        assert!(!error.contains("/tmp/ephemeral"));
    }

    #[test]
    fn refuses_pending_single_node_block_or_deleting_receipt_claims() {
        let (config, claim) = inputs();
        for (field, value) in [
            ("phase", "Pending"),
            ("access", "ReadWriteOnce"),
            ("mode", "Block"),
            ("delete", "synthetic"),
        ] {
            let mut changed = claim.clone();
            match field {
                "phase" => changed["status"]["phase"] = json!(value),
                "access" => changed["spec"]["accessModes"] = json!([value]),
                "mode" => changed["spec"]["volumeMode"] = json!(value),
                _ => changed["metadata"]["deletionTimestamp"] = json!(value),
            }
            assert!(validate_objects(&config, &changed).is_err());
        }
    }
}
