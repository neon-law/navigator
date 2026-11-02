//! The Firm's Twilio receiving numbers, mapped to Brands.
//!
//! One row per E.164 number a Firm points at this deployment. The webhook
//! resolution spine looks the callee up here and nowhere else: an unknown
//! number is refused, never inferred from the request host. The row's
//! `auth_token_secret_ref` names a KMS-backed `firm_integration_secret`
//! holding that number's Twilio auth token. The table is born in PostgreSQL
//! with no Surreal source, so it carries no parity obligation.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::postgres::PostgresDb;

#[derive(Debug, thiserror::Error)]
pub enum BrandPhoneNumberError {
    #[error("invalid Brand phone number: {0}")]
    Invalid(&'static str),
    #[error("the number is already registered with different content")]
    Conflict,
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
    #[error("PostgreSQL is unavailable")]
    Postgres(#[from] crate::postgres::PostgresError),
}

/// Which lanes a registered number serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelKinds {
    Sms,
    Voice,
    Both,
}

impl ChannelKinds {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sms => "sms",
            Self::Voice => "voice",
            Self::Both => "both",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "sms" => Some(Self::Sms),
            "voice" => Some(Self::Voice),
            "both" => Some(Self::Both),
            _ => None,
        }
    }

    /// Whether the registered number carries a lane. `Both` carries either.
    #[must_use]
    pub fn serves(self, lane: Self) -> bool {
        match (self, lane) {
            (Self::Both, _) => true,
            (registered, lane) => registered == lane,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrandPhoneNumber {
    /// E.164 callee, for example `+15551234567` — the unique key.
    pub number_e164: String,
    /// Deployment-unique [Brand](crate::brands) key the number speaks for.
    pub brand_key: String,
    /// Per-number credential set: the Twilio account the number lives in.
    pub twilio_account_sid: String,
    /// KMS-backed `firm_integration_secret` holding the number's auth token.
    pub auth_token_secret_ref: Uuid,
    pub channel_kinds: ChannelKinds,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl BrandPhoneNumber {
    /// Content identity for the idempotent `put`: timestamps never differ.
    fn same_registration(&self, input: &NewBrandPhoneNumber) -> bool {
        self.brand_key == input.brand_key
            && self.twilio_account_sid == input.twilio_account_sid
            && self.auth_token_secret_ref == input.auth_token_secret_ref
            && self.channel_kinds == input.channel_kinds
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewBrandPhoneNumber {
    pub number_e164: String,
    pub brand_key: String,
    pub twilio_account_sid: String,
    pub auth_token_secret_ref: Uuid,
    pub channel_kinds: ChannelKinds,
}

/// Whether a string is an E.164 number: `+` then 8 to 15 ASCII digits.
/// The same rule the PostgreSQL CHECK constraint and the webhook params
/// enforce, in one place.
#[must_use]
pub fn is_e164(value: &str) -> bool {
    let digits = value.strip_prefix('+').unwrap_or_default();
    (8..=15).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
}

fn validate(input: &NewBrandPhoneNumber) -> Result<(), BrandPhoneNumberError> {
    if !is_e164(&input.number_e164) {
        return Err(BrandPhoneNumberError::Invalid("number_e164"));
    }
    if input.brand_key.trim().is_empty() || input.brand_key.len() > 100 {
        return Err(BrandPhoneNumberError::Invalid("brand_key"));
    }
    if input.twilio_account_sid.trim().is_empty() || input.twilio_account_sid.len() > 64 {
        return Err(BrandPhoneNumberError::Invalid("twilio_account_sid"));
    }
    Ok(())
}

fn from_row(row: &sqlx::postgres::PgRow) -> Result<BrandPhoneNumber, sqlx::Error> {
    let channel_kinds: String = row.try_get("channel_kinds")?;
    Ok(BrandPhoneNumber {
        number_e164: row.try_get("number_e164")?,
        brand_key: row.try_get("brand_key")?,
        twilio_account_sid: row.try_get("twilio_account_sid")?,
        auth_token_secret_ref: row.try_get("auth_token_secret_ref")?,
        channel_kinds: ChannelKinds::parse(&channel_kinds)
            .expect("invariant: the channel_kinds CHECK constrains stored values"),
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// Register a receiving number. A repeated identical request returns the
/// existing row; the same number with different content is a loud conflict,
/// never a silent overwrite.
pub async fn put(
    pg: &PostgresDb,
    input: &NewBrandPhoneNumber,
) -> Result<BrandPhoneNumber, BrandPhoneNumberError> {
    validate(input)?;
    let mut tx = pg.0.begin().await?;
    let existing =
        sqlx::query("SELECT * FROM brand_phone_number WHERE number_e164 = $1 FOR UPDATE")
            .bind(&input.number_e164)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some(row) = existing {
        let registered = from_row(&row)?;
        if registered.same_registration(input) {
            tx.commit().await?;
            return Ok(registered);
        }
        return Err(BrandPhoneNumberError::Conflict);
    }
    let row = sqlx::query("INSERT INTO brand_phone_number (number_e164, brand_key, twilio_account_sid, auth_token_secret_ref, channel_kinds) VALUES ($1, $2, $3, $4, $5) RETURNING *")
        .bind(&input.number_e164)
        .bind(input.brand_key.trim())
        .bind(input.twilio_account_sid.trim())
        .bind(input.auth_token_secret_ref)
        .bind(input.channel_kinds.as_str())
        .fetch_one(&mut *tx)
        .await?;
    let registered = from_row(&row)?;
    tx.commit().await?;
    Ok(registered)
}

/// The registry row for a callee, or `None` when the number is unknown.
pub async fn find_by_number(
    pg: &PostgresDb,
    number_e164: &str,
) -> Result<Option<BrandPhoneNumber>, BrandPhoneNumberError> {
    let row = sqlx::query("SELECT * FROM brand_phone_number WHERE number_e164 = $1")
        .bind(number_e164)
        .fetch_optional(&pg.0)
        .await?;
    row.map(|row| from_row(&row))
        .transpose()
        .map_err(BrandPhoneNumberError::Database)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registration(number: &str, brand: &str) -> NewBrandPhoneNumber {
        NewBrandPhoneNumber {
            number_e164: number.into(),
            brand_key: brand.into(),
            twilio_account_sid: "AC7f5ab4c8de0123456789abcdef01234".into(),
            auth_token_secret_ref: Uuid::from_u128(1),
            channel_kinds: ChannelKinds::Both,
        }
    }

    #[test]
    fn e164_shape_is_enforced_in_one_place() {
        assert!(is_e164("+15551234567"));
        assert!(is_e164("+3312345678"));
        assert!(!is_e164("5551234567"), "no country code");
        assert!(!is_e164("+1"), "too few digits");
        assert!(!is_e164("+1234567890123456789"), "too many digits");
        assert!(!is_e164("+1555123456a"), "letters");
        assert!(!is_e164("+1 555 123 4567"), "spaces");
        assert!(!is_e164(""), "empty");
    }

    #[test]
    fn channel_kinds_round_trip_and_lane_membership() {
        for kinds in [ChannelKinds::Sms, ChannelKinds::Voice, ChannelKinds::Both] {
            assert_eq!(ChannelKinds::parse(kinds.as_str()), Some(kinds));
        }
        assert_eq!(ChannelKinds::parse("fax"), None);
        assert!(ChannelKinds::Sms.serves(ChannelKinds::Sms));
        assert!(!ChannelKinds::Sms.serves(ChannelKinds::Voice));
        assert!(ChannelKinds::Voice.serves(ChannelKinds::Voice));
        assert!(ChannelKinds::Both.serves(ChannelKinds::Sms));
        assert!(ChannelKinds::Both.serves(ChannelKinds::Voice));
    }

    #[test]
    fn validation_rejects_an_ambiguous_registration() {
        assert!(validate(&registration("+15551234567", "neon")).is_ok());
        assert_eq!(
            validate(&registration("5551234567", "neon"))
                .unwrap_err()
                .to_string(),
            "invalid Brand phone number: number_e164"
        );
        assert_eq!(
            validate(&registration("+15551234567", "  "))
                .unwrap_err()
                .to_string(),
            "invalid Brand phone number: brand_key"
        );
        assert_eq!(
            validate(&registration("+15551234567", &"b".repeat(101)))
                .unwrap_err()
                .to_string(),
            "invalid Brand phone number: brand_key"
        );
        let no_account_sid = NewBrandPhoneNumber {
            twilio_account_sid: "   ".into(),
            ..registration("+15551234567", "neon")
        };
        assert_eq!(
            validate(&no_account_sid).unwrap_err().to_string(),
            "invalid Brand phone number: twilio_account_sid"
        );
    }
}
