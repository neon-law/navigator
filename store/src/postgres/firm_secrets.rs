//! Serialized encrypted Firm integration-secret rotation.

use crate::firm_secrets::{IntegrationProvider, Mode, SecretPutRequest, SecretStoreError};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use cloud::{KmsContext, RuntimeKms};
use uuid::Uuid;

pub(crate) async fn write_secret(
    pg: &crate::postgres::PostgresDb,
    request: SecretPutRequest<'_>,
    actor: Uuid,
    stripe_mode: Option<Mode>,
    kms: &dyn RuntimeKms,
) -> Result<Uuid, SecretStoreError> {
    let SecretPutRequest {
        firm_id: firm,
        provider,
        kind,
        value,
        ..
    } = request;
    let mut tx = pg
        .pool()
        .begin()
        .await
        .map_err(|_| SecretStoreError::Postgres)?;
    sqlx::query("SELECT pg_advisory_xact_lock(1312902741,hashtext($1::text))")
        .bind(serde_json::json!([firm, provider.as_str(), kind.as_str()]).to_string())
        .execute(&mut *tx)
        .await
        .map_err(|_| SecretStoreError::Postgres)?;
    let last: Option<i64> = sqlx::query_scalar("SELECT max(version) FROM firm_integration_secret WHERE firm_id=$1 AND provider=$2 AND kind=$3")
        .bind(firm).bind(provider.as_str()).bind(kind.as_str()).fetch_one(&mut *tx).await.map_err(|_| SecretStoreError::Postgres)?;
    let version = last
        .unwrap_or(0)
        .checked_add(1)
        .ok_or(SecretStoreError::WriteReturnedNothing)?;
    let context = KmsContext::new(firm, provider.as_str(), kind.as_str());
    let data_key: [u8; 32] = rand::random();
    let ciphertext =
        cloud::seal(value.as_bytes(), &data_key, &context).map_err(SecretStoreError::Kms)?;
    let wrapped = kms
        .wrap_data_key(&data_key, &context)
        .await
        .map_err(SecretStoreError::Kms)?;
    if provider != IntegrationProvider::Twilio {
        sqlx::query("UPDATE firm_integration_secret SET status='revoked',updated_at=now() WHERE firm_id=$1 AND provider=$2 AND kind=$3 AND status='active'")
            .bind(firm).bind(provider.as_str()).bind(kind.as_str()).execute(&mut *tx).await.map_err(|_| SecretStoreError::Postgres)?;
    }
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO firm_integration_secret (id,firm_id,provider,kind,version,ciphertext,wrapped_dek,kms_key_version,kms_context,status,actor_id,created_at,updated_at,stripe_mode) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,'active',$10,now(),now(),$11)")
        .bind(id).bind(firm).bind(provider.as_str()).bind(kind.as_str()).bind(version)
        .bind(BASE64.encode(ciphertext)).bind(BASE64.encode(wrapped.ciphertext)).bind(wrapped.key_version)
        .bind(context.aad()).bind(actor).bind(stripe_mode.map(|m| m.to_string())).execute(&mut *tx).await.map_err(|_| SecretStoreError::Postgres)?;
    tx.commit().await.map_err(|_| SecretStoreError::Postgres)?;
    Ok(id)
}
