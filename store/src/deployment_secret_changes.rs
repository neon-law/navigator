//! Owner-authored encrypted changes awaiting operator entry into SOPS.
//! Values never leave the store as plaintext; export carries only envelopes.
use crate::{application_store::ApplicationStore, persons::Role, DeploymentEnvironment};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use cloud::{KmsContext, RuntimeKms, WrappedDataKey};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

const TABLE: &str = "deployment_secret_change";

/// A preset field in the production social application configuration.
#[derive(Clone, Copy, Debug)]
pub struct KeyPreset {
    pub provider: &'static str,
    pub key: &'static str,
    pub secret: bool,
}
macro_rules! preset {
    ($provider:literal, $key:literal, $secret:literal) => {
        KeyPreset {
            provider: $provider,
            key: $key,
            secret: $secret,
        }
    };
}
/// Closed keys accepted by the dashboard and operator import.
pub const PRESETS: &[KeyPreset] = &[
    preset!("Meta", "NAVIGATOR_META_APP_ID", false),
    preset!("Meta", "NAVIGATOR_META_APP_SECRET", true),
    preset!("Meta", "NAVIGATOR_META_WEBHOOK_VERIFY_TOKEN", true),
    preset!("Meta", "NAVIGATOR_META_WEBHOOK_APP_SECRET", true),
    preset!("Threads", "NAVIGATOR_THREADS_APP_ID", false),
    preset!("Threads", "NAVIGATOR_THREADS_APP_SECRET", true),
    preset!("YouTube", "NAVIGATOR_YOUTUBE_CLIENT_ID", false),
    preset!("YouTube", "NAVIGATOR_YOUTUBE_CLIENT_SECRET", true),
    preset!("LinkedIn", "NAVIGATOR_LINKEDIN_CLIENT_ID", false),
    preset!("LinkedIn", "NAVIGATOR_LINKEDIN_CLIENT_SECRET", true),
    preset!("X", "NAVIGATOR_X_CLIENT_ID", false),
    preset!("X", "NAVIGATOR_X_CLIENT_SECRET", true),
    preset!(
        "TikTok for Business",
        "NAVIGATOR_TIKTOK_BUSINESS_AUTHORIZATION_URL",
        false
    ),
    preset!(
        "TikTok for Business",
        "NAVIGATOR_TIKTOK_BUSINESS_APP_ID",
        false
    ),
    preset!(
        "TikTok for Business",
        "NAVIGATOR_TIKTOK_BUSINESS_APP_SECRET",
        true
    ),
];

/// Safe, bounded errors omit supplied keys, values, and database diagnostics.
#[derive(Debug, thiserror::Error)]
pub enum ChangeError {
    #[error("Owner access in production is required")]
    Forbidden,
    #[error("Unsupported credential field or invalid value")]
    Invalid,
    #[error("Runtime encryption is unavailable; ask the deployment operator to configure it")]
    Encryption,
    #[error("Could not store the encrypted change")]
    Store,
}

/// Portable encrypted export. No plaintext value is serialized.
#[derive(Clone, Serialize, Deserialize)]
pub struct EncryptedChange {
    pub request_id: String,
    pub actor_id: String,
    pub key: String,
    pub ciphertext: String,
    pub wrapped_key: String,
    pub kms_key_version: String,
    pub inserted_at: String,
}

/// Production secrets never enter development or simulated-matters deployments.
#[must_use]
pub fn accepts_changes_from<F: Fn(&str) -> Option<String>>(get: F) -> bool {
    crate::config::social_egress_from(&get).is_ok()
        && get(crate::deployment::CREDENTIAL_ENVIRONMENT).as_deref() == Some("production")
}

fn authorize(role: Role, environment: DeploymentEnvironment) -> Result<(), ChangeError> {
    if role == Role::Owner && environment == DeploymentEnvironment::Production {
        Ok(())
    } else {
        Err(ChangeError::Forbidden)
    }
}

/// Validate the whole submission before encryption or persistence.
pub fn replacements(fields: &BTreeMap<String, String>) -> Result<Vec<(&str, &str)>, ChangeError> {
    let mut result = Vec::new();
    for (key, value) in fields {
        if key == "_csrf" {
            continue;
        }
        if !PRESETS.iter().any(|preset| preset.key == key)
            || value.len() > 8192
            || value.contains('\0')
        {
            return Err(ChangeError::Invalid);
        }
        if !value.trim().is_empty() {
            result.push((key.as_str(), value.as_str()));
        }
    }
    Ok(result)
}

/// Whether a saved request can still be exported and imported by the operator.
#[must_use]
pub fn exportable_at(request: &EncryptedChange, now: chrono::DateTime<chrono::Utc>) -> bool {
    chrono::DateTime::parse_from_rfc3339(&request.inserted_at).is_ok_and(|timestamp| {
        let age = now.signed_duration_since(timestamp);
        age <= chrono::Duration::hours(24) && age >= -chrono::Duration::minutes(5)
    })
}

fn context(request: &EncryptedChange) -> Result<KmsContext, ChangeError> {
    let id = Uuid::parse_str(&request.request_id).map_err(|_| ChangeError::Invalid)?;
    if !PRESETS.iter().any(|preset| preset.key == request.key) {
        return Err(ChangeError::Invalid);
    }
    Ok(KmsContext::new(
        id,
        "production-sops-change",
        format!(
            "{}\n{}\n{}",
            request.key, request.inserted_at, request.actor_id
        ),
    ))
}

/// Save each nonblank replacement as an immutable encrypted request.
/// Blank inputs produce no write and preserve existing pending and deployed values.
pub async fn save(
    _db: &ApplicationStore,
    kms: &dyn RuntimeKms,
    role: Role,
    environment: DeploymentEnvironment,
    actor_id: Uuid,
    fields: &BTreeMap<String, String>,
) -> Result<usize, ChangeError> {
    authorize(role, environment)?;
    let pg = crate::postgres::matter::authority_for(TABLE)
        .await
        .map_err(|_| ChangeError::Store)?;
    let selected = replacements(fields)?;
    let mut encrypted = Vec::with_capacity(selected.len());
    let mut ids = Vec::with_capacity(selected.len());
    for (key, value) in selected {
        let request_id = Uuid::now_v7();
        ids.push(request_id);
        let mut change = EncryptedChange {
            request_id: request_id.to_string(),
            actor_id: actor_id.to_string(),
            key: key.into(),
            ciphertext: String::new(),
            wrapped_key: String::new(),
            kms_key_version: String::new(),
            inserted_at: chrono::Utc::now().to_rfc3339(),
        };
        let context = context(&change)?;
        let data_key: [u8; 32] = rand::random();
        let wrapped = kms
            .wrap_data_key(&data_key, &context)
            .await
            .map_err(|_| ChangeError::Encryption)?;
        change.ciphertext = BASE64.encode(
            cloud::seal(value.as_bytes(), &data_key, &context)
                .map_err(|_| ChangeError::Encryption)?,
        );
        change.wrapped_key = BASE64.encode(wrapped.ciphertext);
        change.kms_key_version = wrapped.key_version;
        encrypted.push(change);
    }
    let count = encrypted.len();
    if count == 0 {
        return Ok(0);
    }
    let pg = pg;
    let mut tx = pg.pool().begin().await.map_err(|_| ChangeError::Store)?;
    for row in encrypted {
        let id = Uuid::parse_str(&row.request_id).map_err(|_| ChangeError::Store)?;
        let timestamp = chrono::DateTime::parse_from_rfc3339(&row.inserted_at)
            .map_err(|_| ChangeError::Store)?;
        sqlx::query("INSERT INTO deployment_secret_change (id,request_id,actor_id,key,ciphertext,wrapped_key,kms_key_version,inserted_at,inserted_at_text,updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$8)")
                .bind(id).bind(row.request_id).bind(row.actor_id).bind(row.key).bind(row.ciphertext)
                .bind(row.wrapped_key).bind(row.kms_key_version).bind(timestamp).bind(row.inserted_at)
                .execute(&mut *tx).await.map_err(|_| ChangeError::Store)?;
    }
    tx.commit().await.map_err(|_| ChangeError::Store)?;
    Ok(count)
}

/// Latest immutable request for each preset; owner-only, production-only.
pub async fn latest(
    _db: &ApplicationStore,
    role: Role,
    environment: DeploymentEnvironment,
) -> Result<Vec<EncryptedChange>, ChangeError> {
    authorize(role, environment)?;
    let pg = crate::postgres::matter::authority_for(TABLE)
        .await
        .map_err(|_| ChangeError::Store)?;
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT jsonb_build_object('request_id',request_id,'actor_id',actor_id,'key',key,
             'ciphertext',ciphertext,'wrapped_key',wrapped_key,'kms_key_version',kms_key_version,
             'inserted_at',inserted_at_text) FROM deployment_secret_change ORDER BY inserted_at,id",
    )
    .fetch_all(pg.pool())
    .await
    .map_err(|_| ChangeError::Store)?;
    let mut latest = BTreeMap::new();
    for row in rows {
        let row: EncryptedChange = serde_json::from_value(row).map_err(|_| ChangeError::Store)?;
        latest.insert(row.key.clone(), row);
    }
    Ok(latest.into_values().collect())
}

/// Operator-only decryption seam. Callers must feed values into SOPS stdin,
/// never print them or serialize a plaintext export.
pub async fn open(kms: &dyn RuntimeKms, change: &EncryptedChange) -> Result<String, ChangeError> {
    let context = context(change)?;
    let wrapped = WrappedDataKey {
        ciphertext: BASE64
            .decode(&change.wrapped_key)
            .map_err(|_| ChangeError::Invalid)?,
        key_version: change.kms_key_version.clone(),
    };
    let data_key = kms
        .unwrap_data_key(&wrapped, &context)
        .await
        .map_err(|_| ChangeError::Encryption)?;
    let ciphertext = BASE64
        .decode(&change.ciphertext)
        .map_err(|_| ChangeError::Invalid)?;
    let plaintext =
        cloud::open(&ciphertext, &data_key, &context).map_err(|_| ChangeError::Encryption)?;
    String::from_utf8(plaintext).map_err(|_| ChangeError::Invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn production_scope_requires_real_matters_and_production_credentials() {
        let mut values = BTreeMap::from([
            ("NAVIGATOR_ENVIRONMENT", "production"),
            ("NAVIGATOR_CREDENTIAL_ENVIRONMENT", "production"),
            ("NAVIGATOR_SIMULATED_MATTERS", "false"),
        ]);
        assert!(accepts_changes_from(|key| values
            .get(key)
            .map(|value| (*value).into())));
        values.insert("NAVIGATOR_SIMULATED_MATTERS", "true");
        assert!(!accepts_changes_from(|key| values
            .get(key)
            .map(|value| (*value).into())));
        values.insert("NAVIGATOR_SIMULATED_MATTERS", "false");
        values.insert("NAVIGATOR_CREDENTIAL_ENVIRONMENT", "staging");
        assert!(!accepts_changes_from(|key| values
            .get(key)
            .map(|value| (*value).into())));
    }
    #[test]
    fn allowlist_and_blank_preservation() {
        let fields = BTreeMap::from([("NAVIGATOR_META_APP_SECRET".into(), String::new())]);
        assert!(replacements(&fields).unwrap().is_empty());
        assert!(replacements(&BTreeMap::from([(
            "SESSION_SECRET".into(),
            "private-value".into()
        )]))
        .is_err());
        for role in [Role::Admin, Role::Lawyer, Role::Clerk, Role::Client] {
            assert!(authorize(role, DeploymentEnvironment::Production).is_err());
        }
        assert!(authorize(Role::Owner, DeploymentEnvironment::Dev).is_err());
    }
}
