//! The Twilio webhook resolution spine.
//!
//! Every Twilio event — an inbound text, its delivery callback, and one day
//! a voice call's status — takes the same spine: look the key number up in
//! the Firm's PostgreSQL registry, verify `X-Twilio-Signature` with that
//! number's own KMS-backed credential over the canonical URL rebuilt from
//! the deployment host, then resolve the registered Brand to its Firm.
//! Everything left of a verified signature fails closed: an unknown number
//! is refused before any Brand is considered, an unresolvable credential
//! never falls back to a shared token, and the signed URL never comes from
//! the request line.
//!
//! Per-number credentials are Firm integration secrets: the registry row's
//! `auth_token_secret_ref` names one sealed `twilio_auth_token` version,
//! and the spine refuses a registration whose secret belongs to a different
//! Firm than the Brand's — torn configuration, not a fallback opportunity.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use thiserror::Error;

/// Which webhook lane a request arrived on. The lane chooses the path
/// Twilio signs and the registry lanes the number must be registered for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// `POST /webhook/twilio/sms` — an inbound text.
    SmsIngest,
    /// `POST /webhook/twilio/status` — an outbound text's delivery event.
    SmsStatus,
}

impl Lane {
    /// The path Twilio was told to call, which is the path it signs.
    #[must_use]
    pub const fn signed_path(self) -> &'static str {
        match self {
            Self::SmsIngest => crate::twilio_sms::SMS_WEBHOOK_PATH,
            Self::SmsStatus => crate::twilio_sms::SMS_STATUS_PATH,
        }
    }

    /// The registry lane the registered number must carry.
    #[must_use]
    pub const fn kinds(self) -> store::brand_phone_numbers::ChannelKinds {
        match self {
            Self::SmsIngest | Self::SmsStatus => store::brand_phone_numbers::ChannelKinds::Sms,
        }
    }
}

/// Why the spine refused a request. Anything before a verified signature is
/// fail-closed; [`ResolutionError::status`] maps each to the answer Twilio sees.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ResolutionError {
    /// The registry is configured but the lookup failed — a store outage,
    /// not an unknown number.
    #[error("the receiving-number registry lookup failed")]
    RegistryLookup,
    /// No PostgreSQL registry is configured, so the deployment has no
    /// Twilio receiving numbers at all.
    #[error("no receiving-number registry is configured")]
    RegistryUnavailable,
    #[error("{0} is not a registered receiving number")]
    UnknownNumber(String),
    #[error("the registered number does not carry this lane")]
    LaneNotServed,
    /// The referenced secret is missing, revoked, or not a Twilio auth
    /// token. Redeliveries give the operator time to repair the
    /// registration; nothing is ingested unauthenticated in the meantime.
    #[error("the registered number's credential is missing, revoked, or the wrong kind")]
    CredentialUnavailable,
    #[error("the registered number's credential belongs to another Firm")]
    TornRegistration,
    #[error("the registered Brand has no owning Firm")]
    NoOwningFirm,
    #[error("the deployment names no public host to rebuild the signed URL from")]
    MissingPublicHost,
    #[error("the signature is missing or does not verify")]
    BadSignature,
}

impl ResolutionError {
    /// The answer Twilio sees. Configuration faults are 503 so Twilio
    /// redelivers while the registration is repaired; an unknown number or
    /// a lane the number does not carry is 404, the same answer an
    /// unconfigured deployment gives; a bad signature stays 401.
    #[must_use]
    pub const fn status(&self) -> StatusCode {
        match self {
            Self::UnknownNumber(_) | Self::LaneNotServed => StatusCode::NOT_FOUND,
            Self::BadSignature => StatusCode::UNAUTHORIZED,
            Self::RegistryLookup
            | Self::RegistryUnavailable
            | Self::CredentialUnavailable
            | Self::TornRegistration
            | Self::NoOwningFirm
            | Self::MissingPublicHost => StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}

impl IntoResponse for ResolutionError {
    fn into_response(self) -> Response {
        self.status().into_response()
    }
}

/// A verified Twilio event: the registration it arrived for and the Firm
/// that Brand belongs to. Downstream steps ingest from here — never from
/// the request's host or a deployment-wide credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTwilioWebhook {
    pub registration: store::brand_phone_numbers::BrandPhoneNumber,
    pub firm_id: uuid::Uuid,
}

/// The stores and KMS one spine resolves against. Handlers hold these in
/// [`crate::AppState`]; tests hand out the same seams.
pub struct Spine<'a> {
    pub registry: &'a store::postgres::PostgresDb,
    pub surreal: &'a store::surreal::SurrealDb,
    pub kms: &'a dyn cloud::RuntimeKms,
}

impl Spine<'_> {
    /// Run the spine for one request.
    ///
    /// `key_number` is the number the lane keys on — the callee `To` for an
    /// inbound text, the Firm's own sending number for its status callbacks.
    /// `params` is the parsed form body in arrival order (the check sorts
    /// its own copy). `host_lookup` is the deployment-host seam —
    /// `std::env::var` in production — so tests rebuild canonical URLs
    /// without touching the process environment.
    pub async fn resolve(
        &self,
        lane: Lane,
        host_lookup: impl Fn(&str) -> Option<String>,
        key_number: &str,
        params: &[(String, String)],
        provided_signature: Option<&str>,
    ) -> Result<ResolvedTwilioWebhook, ResolutionError> {
        // 1. Registry lookup. Fail closed on anything unregistered: brand
        //    inference from the request host is exactly what the spine
        //    exists to make impossible.
        let registration = store::brand_phone_numbers::find_by_number(self.registry, key_number)
            .await
            .map_err(|_| ResolutionError::RegistryLookup)?
            .ok_or_else(|| ResolutionError::UnknownNumber(key_number.to_string()))?;
        if !registration.channel_kinds.serves(lane.kinds()) {
            return Err(ResolutionError::LaneNotServed);
        }
        // 2. The number's own credential. A missing, revoked, or wrong-kind
        //    secret stops here — never a shared token.
        let (auth_token, secret_firm) = store::firm_secrets::resolve_twilio_auth_token_by_ref(
            self.surreal,
            registration.auth_token_secret_ref,
            self.kms,
        )
        .await
        .map_err(|_| ResolutionError::CredentialUnavailable)?;
        // 3. The canonical URL Twilio signed, rebuilt from the deployment host.
        let host = canonical_host(&host_lookup).ok_or(ResolutionError::MissingPublicHost)?;
        let webhook_url = format!("https://{host}{}", lane.signed_path());
        // 4. Constant-time verification with that number's token.
        let provided = provided_signature.ok_or(ResolutionError::BadSignature)?;
        if !crate::twilio_sms::verify_signature(
            auth_token.expose_for_provider_call(),
            &webhook_url,
            params,
            provided,
        ) {
            return Err(ResolutionError::BadSignature);
        }
        // 5. Brand → Firm; a registration straddling Firms is torn config.
        let firm_id = store::firms::firm_id_for_brand_key(self.surreal, &registration.brand_key)
            .await
            .map_err(|_| ResolutionError::NoOwningFirm)?
            .ok_or(ResolutionError::NoOwningFirm)?;
        if firm_id != secret_firm {
            return Err(ResolutionError::TornRegistration);
        }
        Ok(ResolvedTwilioWebhook {
            registration,
            firm_id,
        })
    }
}

/// The deployment's public host: `NAVIGATOR_PUBLIC_HOST`, or the
/// `CANONICAL_HOST` a GKE pod receives instead — `ops ship` rewrites every
/// literal `NAVIGATOR_PUBLIC_HOST` in the manifest, env names included, so
/// no pod env can carry the first name.
fn canonical_host(host_lookup: &impl Fn(&str) -> Option<String>) -> Option<String> {
    let host = host_lookup("NAVIGATOR_PUBLIC_HOST")
        .or_else(|| host_lookup("CANONICAL_HOST"))?
        .trim()
        .to_string();
    if host.is_empty() {
        return None;
    }
    let host = host
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/');
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cloud::FakeKms;

    fn host_lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    #[test]
    fn the_canonical_host_comes_from_the_deployment_not_the_request() {
        assert_eq!(
            canonical_host(&host_lookup(&[(
                "NAVIGATOR_PUBLIC_HOST",
                "www.example.com"
            )])),
            Some("www.example.com".into())
        );
        // A GKE pod receives the row's public host as CANONICAL_HOST only.
        assert_eq!(
            canonical_host(&host_lookup(&[(
                "CANONICAL_HOST",
                "https://staging.example.com/"
            )])),
            Some("staging.example.com".into())
        );
        assert_eq!(canonical_host(&host_lookup(&[])), None);
        assert_eq!(
            canonical_host(&host_lookup(&[("NAVIGATOR_PUBLIC_HOST", "  ")])),
            None
        );
    }

    #[test]
    fn each_lane_signs_its_own_path_and_demands_its_own_lane() {
        assert_eq!(Lane::SmsIngest.signed_path(), "/webhook/twilio/sms");
        assert_eq!(Lane::SmsStatus.signed_path(), "/webhook/twilio/status");
        assert_eq!(
            Lane::SmsIngest.kinds(),
            store::brand_phone_numbers::ChannelKinds::Sms
        );
    }

    #[test]
    fn refusals_map_to_the_answers_twilio_sees() {
        assert_eq!(
            ResolutionError::UnknownNumber("+15550009999".into()).status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            ResolutionError::LaneNotServed.status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            ResolutionError::BadSignature.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            ResolutionError::CredentialUnavailable.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "a config fault must be retried, not answered as a bad request"
        );
    }

    /// Two Brands share one deployment, each number with its own token: a
    /// request signed with the other number's credential verifies against
    /// nothing. The tokens are real Firm secrets through the FakeKms, and
    /// the registry is the PostgreSQL table the spine reads.
    #[tokio::test]
    async fn a_cross_signed_request_is_rejected_and_the_own_token_verifies() {
        let Some(pg) = crate::twilio_registry_harness::postgres().await else {
            return;
        };
        let db = store::test_support::mem_surreal().await;
        // The same key version the harness sealed the tokens under.
        let kms = FakeKms::new("spine-test");
        let neon_firm = store::test_support::seed_firm_with_brand(&db, "neon").await;
        let anchor_firm = store::test_support::seed_firm_with_brand(&db, "anchor").await;
        let neon_number = "+15551230001";
        let anchor_number = "+15551230002";

        let neon_ref = crate::twilio_registry_harness::register_number(
            &db,
            &pg,
            neon_firm,
            "neon",
            neon_number,
            "token-neon",
        )
        .await;
        let anchor_ref = crate::twilio_registry_harness::register_number(
            &db,
            &pg,
            anchor_firm,
            "anchor",
            anchor_number,
            "token-anchor",
        )
        .await;
        assert_ne!(neon_ref, anchor_ref, "each number resolves its own secret");

        let spine = Spine {
            registry: &pg,
            surreal: &db,
            kms: &kms,
        };
        let signed = |token: &str, number: &str| {
            let fields = vec![
                ("From".to_string(), "+15555550100".to_string()),
                ("MessageSid".to_string(), "SM1".to_string()),
                ("To".to_string(), number.to_string()),
            ];
            crate::twilio_sms::twilio_signature(
                token,
                &format!("https://www.example.com{}", Lane::SmsIngest.signed_path()),
                &fields,
            )
        };
        let fields: Vec<(String, String)> = vec![
            ("From".into(), "+15555550100".into()),
            ("MessageSid".into(), "SM1".into()),
            ("To".into(), anchor_number.into()),
        ];

        let cross = spine
            .resolve(
                Lane::SmsIngest,
                host_lookup(&[("NAVIGATOR_PUBLIC_HOST", "www.example.com")]),
                anchor_number,
                &fields,
                Some(&signed("token-neon", anchor_number)),
            )
            .await
            .unwrap_err();
        assert_eq!(
            cross,
            ResolutionError::BadSignature,
            "neon's token must not verify anchor's traffic"
        );

        let own = spine
            .resolve(
                Lane::SmsIngest,
                host_lookup(&[("NAVIGATOR_PUBLIC_HOST", "www.example.com")]),
                anchor_number,
                &fields,
                Some(&signed("token-anchor", anchor_number)),
            )
            .await
            .expect("the number's own token verifies");
        assert_eq!(own.firm_id, anchor_firm);
        assert_eq!(own.registration.brand_key, "anchor");
        assert_eq!(own.registration.number_e164, anchor_number);

        let unknown = spine
            .resolve(
                Lane::SmsIngest,
                host_lookup(&[("NAVIGATOR_PUBLIC_HOST", "www.example.com")]),
                "+15551239999",
                &fields,
                Some(&signed("token-anchor", anchor_number)),
            )
            .await
            .unwrap_err();
        assert_eq!(
            unknown,
            ResolutionError::UnknownNumber("+15551239999".into())
        );
    }
}
