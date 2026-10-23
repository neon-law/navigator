//! The public handle a Brand holds on each social provider.
//!
//! A handle is not a credential: `NeonLawUSA` on X belongs to `neon` whether or
//! not anyone has connected a [`crate::social_accounts`] connection for it. It
//! is recorded first, and a Post's social link may only name a handle recorded
//! here, so a link can never point at an account the Brand does not hold. A
//! provider permalink that is derivable from the handle and the provider's post
//! id is derived here rather than typed in.

use surrealdb::types::SurrealValue;

use crate::social_accounts::SocialProvider;
use crate::surreal::SurrealDb;

const TABLE: &str = "social_handle";

/// A Brand's handle on one provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocialHandle {
    pub brand_key: String,
    pub provider: String,
    pub handle: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SocialHandleError {
    #[error("database: {0}")]
    Db(String),
    #[error("unknown social provider `{0}`")]
    UnknownProvider(String),
    #[error("`{0}` is not a valid handle")]
    InvalidHandle(String),
}

impl From<surrealdb::Error> for SocialHandleError {
    fn from(value: surrealdb::Error) -> Self {
        Self::Db(value.to_string())
    }
}

#[derive(SurrealValue)]
struct HandleRow {
    brand_key: String,
    provider: String,
    handle: String,
}

/// Whether `provider` names a provider Navigator publishes to.
#[must_use]
pub fn is_provider(provider: &str) -> bool {
    [
        SocialProvider::Facebook,
        SocialProvider::Instagram,
        SocialProvider::Threads,
        SocialProvider::Youtube,
        SocialProvider::Linkedin,
        SocialProvider::X,
        SocialProvider::Tiktok,
    ]
    .iter()
    .any(|known| known.as_str() == provider)
}

/// A handle is the provider's own public name: letters, digits, `.`, `_`, `-`,
/// without the leading `@`.
#[must_use]
pub fn is_valid_handle(handle: &str) -> bool {
    !handle.is_empty()
        && handle.len() <= 64
        && handle
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Record or replace the Brand's handle on a provider.
///
/// # Errors
///
/// An unknown provider, an invalid handle, or a database failure.
pub async fn set(
    db: &SurrealDb,
    brand_key: &str,
    provider: &str,
    handle: &str,
) -> Result<(), SocialHandleError> {
    if !is_provider(provider) {
        return Err(SocialHandleError::UnknownProvider(provider.to_string()));
    }
    if !is_valid_handle(handle) {
        return Err(SocialHandleError::InvalidHandle(handle.to_string()));
    }
    db.query(format!(
        "DELETE {TABLE} WHERE brand_key = $brand AND provider = $provider; \
         CREATE {TABLE} SET brand_key = $brand, provider = $provider, handle = $handle"
    ))
    .bind(("brand", brand_key.to_string()))
    .bind(("provider", provider.to_string()))
    .bind(("handle", handle.to_string()))
    .await?
    .check()?;
    crate::postgres_mirror::after_write(
        db,
        crate::postgres_mirror::MirrorKey::SocialHandle {
            brand_key: brand_key.to_string(),
            provider: provider.to_string(),
        },
    )
    .await;
    Ok(())
}

/// Every recorded handle, for the PostgreSQL parity check.
pub(crate) async fn all(db: &SurrealDb) -> Result<Vec<SocialHandle>, SocialHandleError> {
    let mut response = db
        .query(format!("SELECT brand_key, provider, handle FROM {TABLE}"))
        .await?
        .check()?;
    let rows: Vec<HandleRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .map(|row| SocialHandle {
            brand_key: row.brand_key,
            provider: row.provider,
            handle: row.handle,
        })
        .collect())
}

/// The Brand's recorded handle on a provider, if any.
///
/// # Errors
///
/// A database failure.
pub async fn find(
    db: &SurrealDb,
    brand_key: &str,
    provider: &str,
) -> Result<Option<SocialHandle>, SocialHandleError> {
    let mut response = db
        .query(format!(
            "SELECT brand_key, provider, handle FROM {TABLE} \
             WHERE brand_key = $brand AND provider = $provider LIMIT 1"
        ))
        .bind(("brand", brand_key.to_string()))
        .bind(("provider", provider.to_string()))
        .await?
        .check()?;
    let row: Option<HandleRow> = response.take(0)?;
    Ok(row.map(|row| SocialHandle {
        brand_key: row.brand_key,
        provider: row.provider,
        handle: row.handle,
    }))
}

/// The provider's own permalink for a post, when the handle and the post id
/// alone determine it. `None` for a provider whose permalink needs the
/// provider's read endpoint.
#[must_use]
pub fn derived_permalink(provider: &str, handle: &str, post_id: &str) -> Option<String> {
    (provider == "x").then(|| format!("https://x.com/{handle}/status/{post_id}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;

    #[test]
    fn x_permalink_is_derived_from_handle_and_post_id() {
        assert_eq!(
            derived_permalink("x", "NeonLawUSA", "1850000000000000000").as_deref(),
            Some("https://x.com/NeonLawUSA/status/1850000000000000000")
        );
        assert_eq!(derived_permalink("threads", "neonlaw", "abc"), None);
    }

    #[test]
    fn handles_are_the_public_name_without_an_at_sign() {
        assert!(is_valid_handle("NeonLawUSA"));
        assert!(!is_valid_handle("@NeonLawUSA"));
        assert!(!is_valid_handle(""));
        assert!(!is_valid_handle("has space"));
    }

    #[tokio::test]
    async fn a_brand_holds_one_handle_per_provider() {
        let db = test_support::mem_surreal().await;
        assert!(find(&db, "neon", "x").await.unwrap().is_none());
        set(&db, "neon", "x", "NeonLawUSA").await.unwrap();
        set(&db, "neon", "x", "NeonLaw").await.unwrap();
        let handle = find(&db, "neon", "x").await.unwrap().unwrap();
        assert_eq!(handle.handle, "NeonLaw");
        assert!(find(&db, "vesta", "x").await.unwrap().is_none());
        assert!(matches!(
            set(&db, "neon", "myspace", "x").await,
            Err(SocialHandleError::UnknownProvider(_))
        ));
    }
}
