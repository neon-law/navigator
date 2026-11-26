//! Intended provider destinations, independent of OAuth credentials.
use crate::{
    persons::Role,
    postgres::social as authority,
    social_accounts::{SocialAccountError, SocialProvider},
    surreal::SurrealDb,
};
use serde::Deserialize;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, SurrealValue, Deserialize)]
pub struct SocialAccountSelection {
    pub brand_key: String,
    pub provider: String,
    pub provider_account_id: String,
}

/// Save one destination without altering an authorized account or scheduled Post.
pub async fn save(
    db: &SurrealDb,
    role: Role,
    actor: Uuid,
    brand: &str,
    provider: SocialProvider,
    identity: &str,
) -> Result<(), SocialAccountError> {
    if !crate::posts::manageable_brand_keys(db, role, actor)
        .await
        .map_err(|e| SocialAccountError::Db(e.to_string()))?
        .iter()
        .any(|key| key == brand)
    {
        return Err(SocialAccountError::Denied);
    }
    let firm = crate::brands::find_by_key(db, brand)
        .await
        .map_err(|e| SocialAccountError::Db(e.to_string()))?
        .and_then(|brand| brand.firm_id)
        .ok_or(SocialAccountError::BrandNotFound)?;
    let identity = identity.trim();
    if !crate::social_handles::is_valid_handle(identity) {
        return Err(SocialAccountError::MissingAccountIdentity);
    }
    let pg = authority::authority()
        .await?
        .ok_or(authority::SocialAuthorityError::Unavailable)?;
    // One destination per Brand and provider: a later save replaces it.
    let query = sqlx::query(
        "INSERT INTO social_account_selection (id, firm_id, brand_key, provider, \
         provider_account_id, inserted_at, updated_at) VALUES ($1, $2, $3, $4, $5, now(), now()) \
         ON CONFLICT (brand_key, provider) DO UPDATE SET firm_id = EXCLUDED.firm_id, \
         provider_account_id = EXCLUDED.provider_account_id, updated_at = now()",
    )
    .bind(format!("{brand}:{}", provider.as_str()))
    .bind(firm)
    .bind(brand.to_owned())
    .bind(provider.as_str())
    .bind(identity.to_owned());
    authority::execute(&pg, query).await?;
    Ok(())
}

/// Read only the selected Brand's non-secret destinations.
pub async fn for_brand(
    db: &SurrealDb,
    brand: &str,
) -> Result<Vec<SocialAccountSelection>, SocialAccountError> {
    let Some(firm) = crate::brands::find_by_key(db, brand)
        .await
        .map_err(|e| SocialAccountError::Db(e.to_string()))?
        .and_then(|brand| brand.firm_id)
    else {
        return Ok(Vec::new());
    };
    if let Some(pg) = authority::authority().await? {
        let query = sqlx::query(
            "SELECT to_jsonb(s) AS row FROM social_account_selection s \
             WHERE s.brand_key = $1 AND s.firm_id = $2 ORDER BY s.provider",
        )
        .bind(brand.to_owned())
        .bind(firm);
        return Ok(authority::rows(&pg, query).await?);
    }
    let mut response = db.query("SELECT brand_key, provider, provider_account_id FROM social_account_selection WHERE brand_key = $brand AND firm_id = $firm ORDER BY provider").bind(("brand", brand.to_string())).bind(("firm", crate::surreal::record_id("firm", firm))).await?.check()?;
    Ok(response.take(0)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{brands::NewBrand, firms::NewFirm, persons::NewPerson, test_support};
    async fn practice(db: &SurrealDb, name: &str) -> (Uuid, Uuid, String) {
        let admin = crate::persons::create(
            db,
            &NewPerson::with_role(
                format!("{name} Admin"),
                format!("{name}-{}@example.com", Uuid::now_v7()),
                Role::Admin,
            ),
        )
        .await
        .unwrap();
        let firm = crate::firms::create(
            db,
            &NewFirm {
                name: format!("{name} Firm"),
                status: "active".into(),
                entity_id: test_support::seed_entity(db).await,
                admin_dri_person_id: admin.id,
            },
        )
        .await
        .unwrap();
        let brand_key = format!("{name}-brand");
        crate::brands::create(
            db,
            Role::Admin,
            Some(admin.id),
            &NewBrand {
                name: format!("{name} Brand"),
                key: brand_key.clone(),
                firm_id: Some(firm.id),
                primary_color: None,
                accent_color: None,
                typeface: None,
            },
        )
        .await
        .unwrap();
        (firm.id, admin.id, brand_key)
    }

    #[tokio::test]
    async fn saved_destinations_reload_without_authorization_and_remain_brand_scoped() {
        test_support::with_required_postgres(async {
            let db = SurrealDb::uninitialized();
            let (_, actor, brand) = practice(&db, "selected").await;
            let (_, other_actor, other_brand) = practice(&db, "other").await;
            save(
                &db,
                Role::Admin,
                actor,
                &brand,
                SocialProvider::Facebook,
                "page-99",
            )
            .await
            .unwrap();
            assert_eq!(
                for_brand(&db, &brand).await.unwrap()[0].provider_account_id,
                "page-99"
            );
            assert!(for_brand(&db, &other_brand).await.unwrap().is_empty());
            assert!(save(
                &db,
                Role::Admin,
                other_actor,
                &brand,
                SocialProvider::Facebook,
                "stolen"
            )
            .await
            .is_err());
            assert!(save(
                &db,
                Role::Admin,
                actor,
                &brand,
                SocialProvider::Facebook,
                "not an identity"
            )
            .await
            .is_err());
            assert!(crate::social_accounts::for_actor(&db, Role::Admin, actor)
                .await
                .unwrap()
                .is_empty());
            save(
                &db,
                Role::Admin,
                actor,
                &brand,
                SocialProvider::Facebook,
                "page-100",
            )
            .await
            .unwrap();
            assert_eq!(
                for_brand(&db, &brand).await.unwrap()[0].provider_account_id,
                "page-100"
            );
        })
        .await;
    }
}
