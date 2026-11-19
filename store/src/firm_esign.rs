//! The e-signature provider a Firm signs through.
//!
//! A Brand reached by its URL names its Firm (`brand.firm_id`), and the Firm names one provider.
//! No stored choice means DocuSign, the provider every Firm used before the choice existed.

use uuid::Uuid;

use crate::{
    firms::FirmError,
    persons::Role,
    postgres::{tenancy, PostgresDb},
    signatures::SignatureProvider,
};

async fn pg() -> Result<PostgresDb, FirmError> {
    Ok(tenancy::db().await?)
}

fn resolve(stored: Option<&str>) -> SignatureProvider {
    stored
        .and_then(SignatureProvider::from_stored)
        .unwrap_or(SignatureProvider::DocuSign)
}

/// The provider a Firm signs through.
///
/// # Errors
///
/// Returns [`FirmError`] when PostgreSQL is unavailable or the query fails.
pub async fn for_firm(firm_id: Uuid) -> Result<SignatureProvider, FirmError> {
    let pg = pg().await?;
    let stored: Option<Option<String>> =
        sqlx::query_scalar("SELECT esign_provider FROM firm WHERE id=$1")
            .bind(firm_id)
            .fetch_optional(pg.pool())
            .await?;
    Ok(resolve(stored.flatten().as_deref()))
}

/// The provider for the Firm that owns a Brand, as a request to its URL resolves it. A Brand with
/// no Firm signs through DocuSign.
///
/// # Errors
///
/// Returns [`FirmError`] when PostgreSQL is unavailable or the query fails.
pub async fn for_brand(brand_key: &str) -> Result<SignatureProvider, FirmError> {
    let pg = pg().await?;
    let stored: Option<Option<String>> = sqlx::query_scalar(
        "SELECT f.esign_provider FROM brand b JOIN firm f ON f.id=b.firm_id WHERE b.brand_key=$1",
    )
    .bind(brand_key)
    .fetch_optional(pg.pool())
    .await?;
    Ok(resolve(stored.flatten().as_deref()))
}

/// Choose a Firm's provider. An Owner, or that Firm's own Admin, may change it.
///
/// # Errors
///
/// Returns [`FirmError::InvalidEsignProvider`] for a value outside the registry,
/// [`FirmError::NotAuthorized`] for a caller outside the Firm, and
/// [`FirmError::NoSuchFirm`] when the Firm does not exist.
pub async fn set_for_firm(
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    firm_id: Uuid,
    value: &str,
) -> Result<SignatureProvider, FirmError> {
    let provider =
        SignatureProvider::from_stored(value.trim()).ok_or(FirmError::InvalidEsignProvider)?;
    let pg = pg().await?;
    tenancy::authorize_firm_management(pg.pool(), actor_role, actor_person_id, firm_id).await?;
    let updated = sqlx::query("UPDATE firm SET esign_provider=$2, updated_at=now() WHERE id=$1")
        .bind(firm_id)
        .bind(provider.as_str())
        .execute(pg.pool())
        .await?
        .rows_affected();
    if updated == 0 {
        return Err(FirmError::NoSuchFirm(firm_id));
    }
    Ok(provider)
}

#[cfg(test)]
mod tests {
    use super::resolve;
    use crate::signatures::SignatureProvider;

    #[test]
    fn no_stored_choice_or_an_unknown_one_means_docusign() {
        assert_eq!(resolve(None), SignatureProvider::DocuSign);
        assert_eq!(resolve(Some("carrier_pigeon")), SignatureProvider::DocuSign);
        assert_eq!(
            resolve(Some("dropbox_sign")),
            SignatureProvider::DropboxSign
        );
    }
}
