//! Import encrypted dashboard changes into production SOPS without plaintext files.
use super::deployments::{Deployment, CONFIG_FILE, SECRETS_FILE, TREE};
use anyhow::{bail, Context, Result};
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
    sync::Arc,
};
use store::deployment_secret_changes::{EncryptedChange, PRESETS};

pub fn import(root: &Path, deployment: &str, request: &Path) -> Result<()> {
    let target = Deployment::load(root, deployment)?;
    if target
        .coordinates
        .get("NAVIGATOR_ENVIRONMENT")
        .map(String::as_str)
        != Some("production")
        || target
            .coordinates
            .get("NAVIGATOR_CREDENTIAL_ENVIRONMENT")
            .map(String::as_str)
            != Some("production")
        || target
            .coordinates
            .get("NAVIGATOR_SIMULATED_MATTERS")
            .map(String::as_str)
            != Some("false")
    {
        bail!(
            "Social app changes may only target a production deployment without simulated matters"
        );
    }
    let bytes = std::fs::read(request).context("Could not read encrypted request")?;
    if bytes.len() > 1024 * 1024 {
        bail!("Encrypted request is too large");
    }
    let rows: Vec<EncryptedChange> =
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("Invalid encrypted request"))?;
    validate_requests(&rows, chrono::Utc::now())?;
    let runtime = tokio::runtime::Runtime::new().context("Could not start encryption runtime")?;
    let values = runtime.block_on(async {
        let token = cloud::AdcTokenSource::new().await?;
        let config = cloud::GoogleKmsConfig::from_env()?;
        if !config
            .key_name
            .starts_with(&format!("projects/{}/", target.project_id()))
        {
            bail!("Runtime encryption key must belong to the selected production deployment");
        }
        let kms = cloud::GoogleKms::new(config, Arc::new(token));
        let mut values = Vec::new();
        for row in &rows {
            values.push((
                &row.key,
                store::deployment_secret_changes::open(&kms, row).await?,
            ));
        }
        Ok::<_, anyhow::Error>(values)
    })?;
    stage_values(&root.join(TREE).join(deployment), &values, set_secret)?;
    eprintln!("Encrypted changes imported. Review the diff and run inventory validation and secrets apply --dry-run. Nothing was deployed.");
    Ok(())
}

fn stage_values<F: FnMut(&Path, &str, &str) -> Result<()>>(
    dir: &Path,
    values: &[(&String, String)],
    mut writer: F,
) -> Result<()> {
    let config_path = dir.join(CONFIG_FILE);
    let secret_path = dir.join(SECRETS_FILE);
    let scratch_path = dir.join(format!(
        ".social-key-change-{}.enc.yaml",
        uuid::Uuid::now_v7()
    ));
    let scratch = EncryptedScratch(scratch_path);
    let original_ciphertext = std::fs::read(&secret_path)?;
    std::fs::copy(&secret_path, &scratch.0)
        .context("Could not prepare encrypted SOPS working copy")?;
    let mut document = std::fs::read_to_string(&config_path)?.parse::<toml_edit::DocumentMut>()?;
    for (key, value) in values {
        let preset = PRESETS
            .iter()
            .find(|preset| preset.key == key.as_str())
            .context("Unsupported key")?;
        if preset.secret {
            writer(&scratch.0, key, value)?;
        } else {
            document["env"][key.as_str()] = toml_edit::value(value.as_str());
        }
    }
    let config_scratch = EncryptedScratch(dir.join(format!(
        ".social-key-change-{}.config.toml",
        uuid::Uuid::now_v7()
    )));
    std::fs::write(&config_scratch.0, document.to_string())?;
    std::fs::rename(&scratch.0, &secret_path)
        .context("Could not finalize encrypted SOPS changes")?;
    if std::fs::rename(&config_scratch.0, config_path).is_err() {
        std::fs::write(&secret_path, original_ciphertext)
            .context("Could not restore encrypted SOPS working copy")?;
        bail!("Could not finalize public coordinates; encrypted SOPS changes were rolled back");
    }
    Ok(())
}

fn set_secret(path: &Path, key: &str, value: &str) -> Result<()> {
    let mut process = Command::new("sops")
        .args(["set", "--value-stdin"])
        .arg(path)
        .arg(serde_json::to_string(&vec![key])?)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("Could not start SOPS")?;
    process
        .stdin
        .take()
        .context("SOPS stdin unavailable")?
        .write_all(&serde_json::to_vec(value)?)?;
    if !process.wait()?.success() {
        bail!("SOPS update failed; deployment was not applied");
    }
    Ok(())
}

struct EncryptedScratch(std::path::PathBuf);
impl Drop for EncryptedScratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn validate_requests(rows: &[EncryptedChange], now: chrono::DateTime<chrono::Utc>) -> Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    for row in rows {
        if !PRESETS.iter().any(|preset| preset.key == row.key) || !seen.insert(row.key.clone()) {
            bail!("Encrypted request contains unsupported or duplicate keys");
        }
        let timestamp = chrono::DateTime::parse_from_rfc3339(&row.inserted_at)
            .map_err(|_| anyhow::anyhow!("Encrypted request has an invalid timestamp"))?;
        let age = now.signed_duration_since(timestamp);
        if age > chrono::Duration::hours(24) || age < -chrono::Duration::minutes(5) {
            bail!("Encrypted request expired or has a future timestamp; save fresh replacements in the owner dashboard");
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn row() -> EncryptedChange {
        EncryptedChange {
            request_id: uuid::Uuid::now_v7().to_string(),
            actor_id: uuid::Uuid::nil().to_string(),
            key: "NAVIGATOR_META_APP_SECRET".into(),
            ciphertext: "ciphertext-only".into(),
            wrapped_key: "wrapped".into(),
            kms_key_version: "synthetic-key".into(),
            inserted_at: chrono::Utc::now().to_rfc3339(),
        }
    }
    #[test]
    fn staged_updates_preserve_unselected_values_and_comments() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(CONFIG_FILE),
            "# operator note\n[env]\nUNSELECTED = \"coordinate\"\n",
        )
        .unwrap();
        let original = "UNSELECTED_SECRET: ENC[ORIGINAL]\n";
        std::fs::write(dir.path().join(SECRETS_FILE), original).unwrap();
        let secret_key = "NAVIGATOR_META_APP_SECRET".to_string();
        let id_key = "NAVIGATOR_META_APP_ID".to_string();
        stage_values(
            dir.path(),
            &[
                (&secret_key, "synthetic-secret".into()),
                (&id_key, "synthetic-id".into()),
            ],
            |path, key, value| {
                assert_eq!(key, secret_key);
                assert_eq!(value, "synthetic-secret");
                let mut content = std::fs::read_to_string(path)?;
                content.push_str("NAVIGATOR_META_APP_SECRET: ENC[SYNTHETIC]\n");
                std::fs::write(path, content)?;
                Ok(())
            },
        )
        .unwrap();
        let config = std::fs::read_to_string(dir.path().join(CONFIG_FILE)).unwrap();
        assert!(config.contains("# operator note"));
        assert!(config.contains("UNSELECTED = \"coordinate\""));
        assert!(config.contains("synthetic-id"));
        assert!(!config.contains("synthetic-secret"));
        let encrypted = std::fs::read_to_string(dir.path().join(SECRETS_FILE)).unwrap();
        assert!(encrypted.contains(original));
        assert!(!encrypted.contains("synthetic-secret"));
    }
    #[test]
    fn sops_failure_leaves_both_source_files_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let original_config = "[env]\nEXISTING = \"coordinate\"\n";
        let original_secret = "EXISTING_SECRET: ENC[ORIGINAL]\n";
        std::fs::write(dir.path().join(CONFIG_FILE), original_config).unwrap();
        std::fs::write(dir.path().join(SECRETS_FILE), original_secret).unwrap();
        let key = "NAVIGATOR_META_APP_SECRET".to_string();
        assert!(stage_values(
            dir.path(),
            &[(&key, "synthetic-secret".into())],
            |path, _, _| {
                std::fs::write(path, "CHANGED: ENC[STAGED]\n")?;
                bail!("Synthetic provider failure")
            }
        )
        .is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join(CONFIG_FILE)).unwrap(),
            original_config
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join(SECRETS_FILE)).unwrap(),
            original_secret
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
    #[test]
    fn rejects_stale_unknown_and_duplicate_requests() {
        let now = chrono::Utc::now();
        let mut request = row();
        assert!(validate_requests(&[request.clone()], now).is_ok());
        assert!(validate_requests(&[request.clone(), request.clone()], now).is_err());
        request.inserted_at = (now - chrono::Duration::hours(25)).to_rfc3339();
        assert!(validate_requests(&[request.clone()], now).is_err());
        request.key = "SESSION_SECRET".into();
        assert!(validate_requests(&[request], now).is_err());
    }
}
