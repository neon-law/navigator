//! Route mail addressed to the lawyer's own mailbox into the Firm inbox.
//!
//! The lawyer's mailbox shares the SendGrid Inbound Parse endpoint with the
//! public support mailboxes, but it is not a Brand surface: the mail is Firm
//! correspondence without a matter, and it joins the shared inbox under the
//! `email` channel exactly like support intake. Every configured address —
//! aliases included — records under one provider account, so a correspondent
//! who writes to either address lands on one Conversation; the receiving
//! alias is kept in `source_page`. Once attributed to a Project, the
//! Conversation projects onto the matter's Communications like any other
//! Inbox thread.
//!
//! A message whose RFC From belongs to a lawyer-tier or clerk Person is that
//! person's own outgoing mail, not correspondence the mailbox received, so it
//! never ingests as inbound — the same guard the support lane applies.
//!
//! Configuration: `NAVIGATOR_LAWYER_MAILBOXES` (comma-separated addresses;
//! the first entry is the canonical provider account) and
//! `NAVIGATOR_INBOX_FIRM_ID` (the owning Firm, shared with intake).
//! Absent or incomplete configuration disables the channel entirely; the
//! webhook then only archives the raw message as before.

use uuid::Uuid;

use crate::inbound_email::InboundEmail;
use crate::inbox::extract_addr;

/// The lawyer-mailbox routing configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Config {
    /// Lowercase addresses and aliases; the first is the canonical
    /// provider account every alias records under.
    pub addresses: Vec<String>,
    /// The Firm the correspondence belongs to.
    pub firm_id: Uuid,
}

impl Config {
    /// Read the routing configuration from the deployment environment.
    pub(crate) fn from_env() -> Option<Self> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Testable seam for [`Config::from_env`].
    pub(crate) fn from_lookup<F: Fn(&str) -> Option<String>>(get: F) -> Option<Self> {
        let firm_id =
            get("NAVIGATOR_INBOX_FIRM_ID").and_then(|value| Uuid::parse_str(value.trim()).ok())?;
        let mut addresses = Vec::new();
        for part in get("NAVIGATOR_LAWYER_MAILBOXES")
            .unwrap_or_default()
            .split(',')
        {
            let address = part.trim().to_ascii_lowercase();
            if !address.is_empty() && !addresses.contains(&address) {
                addresses.push(address);
            }
        }
        if addresses.is_empty() {
            return None;
        }
        Some(Self { addresses, firm_id })
    }

    /// The receiving alias this message arrived at. The SMTP envelope is
    /// routing authority; the display To header only covers forwarding rules
    /// that rewrite the envelope recipient.
    pub(crate) fn receiving_alias(&self, email: &InboundEmail) -> Option<String> {
        if let Some(envelope) = email.envelope.as_ref() {
            for to in &envelope.to {
                let address = extract_addr(to).to_ascii_lowercase();
                if self
                    .addresses
                    .iter()
                    .any(|configured| configured.as_str() == address.as_str())
                {
                    return Some(address);
                }
            }
        }
        email
            .to
            .split(',')
            .map(extract_addr)
            .map(|address| address.to_ascii_lowercase())
            .find(|address| {
                self.addresses
                    .iter()
                    .any(|configured| configured.as_str() == address.as_str())
            })
    }

    /// The provider account every alias records under: the first configured
    /// address, so aliases share one channel identity and one thread set.
    pub(crate) fn provider_account(&self) -> &str {
        self.addresses
            .first()
            .map(String::as_str)
            .unwrap_or_default()
    }
}

/// Ingest one lawyer-mailbox delivery into the Firm inbox.
///
/// Returns `None` when the channel is unconfigured, the recipient is not a
/// configured lawyer address, or the sender is the mailbox owner's own
/// lawyer-tier or clerk mail. The raw `.eml` stays archived either way.
///
/// # Errors
///
/// Propagates database failures as a string for the webhook's best-effort
/// log path.
pub(crate) async fn ingest(
    db: &store::surreal::SurrealDb,
    email: &InboundEmail,
) -> Result<Option<store::inbox::IngestedInbound>, String> {
    ingest_for_config(db, email, Config::from_env()).await
}

async fn ingest_for_config(
    db: &store::surreal::SurrealDb,
    email: &InboundEmail,
    config: Option<Config>,
) -> Result<Option<store::inbox::IngestedInbound>, String> {
    let Some(config) = config else {
        return Ok(None);
    };
    let Some(receiving) = config.receiving_alias(email) else {
        return Ok(None);
    };
    let from = extract_addr(&email.from);
    if let Some(person) = store::persons::find_by_email_ci(db, &from)
        .await
        .map_err(|error| error.to_string())?
    {
        if person.role.is_lawyer_tier() || person.role.is_clerk() {
            return Ok(None);
        }
    }
    let provider_message_id = crate::inbox::provider_message_id(email);
    store::inbox::ingest_inbound(
        db,
        &store::inbox::IngestInbound {
            firm_id: config.firm_id,
            brand_key: None,
            interaction_type: Some("message"),
            provider_post_id: None,
            parent_provider_message_id: None,
            visibility: None,
            channel: store::inbox::channel::EMAIL,
            provider_account: config.provider_account(),
            provider_message_id: &provider_message_id,
            provider_thread_ref: None,
            external_address: &from,
            external_name: None,
            subject: Some(email.subject.as_str()),
            body: email.text.as_str(),
            source_page: Some(&receiving),
            source_channel: Some(store::inbox::channel::EMAIL),
            matter_consent: false,
            promotional_opt_in: false,
            email_conversation_id: None,
            attachment_asset_id: None,
            occurred_at: &chrono::Utc::now().to_rfc3339(),
        },
    )
    .await
    .map(Some)
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{ingest_for_config, Config};
    use crate::inbound_email::{InboundAttachment, InboundEmail, SmtpEnvelope};
    use store::persons::{NewPerson, Role};
    use store::test_support::{ensure_person, mem_surreal, seed_firm_with_brand};
    use uuid::Uuid;

    fn fixture_firm() -> Uuid {
        Uuid::now_v7()
    }

    fn config(addresses: &[&str], firm_id: Uuid) -> Option<Config> {
        Some(Config {
            addresses: addresses.iter().map(|a| (*a).to_string()).collect(),
            firm_id,
        })
    }

    fn email(from: &str, to: &str) -> InboundEmail {
        InboundEmail {
            from: from.to_string(),
            to: to.to_string(),
            subject: "Quarterly report".to_string(),
            text: "The signed report is attached.".to_string(),
            ..InboundEmail::default()
        }
    }

    #[test]
    fn config_requires_both_addresses_and_firm() {
        let firm_id = fixture_firm();
        let env = |key: &str| match key {
            "NAVIGATOR_INBOX_FIRM_ID" => Some(firm_id.to_string()),
            "NAVIGATOR_LAWYER_MAILBOXES" => Some("nick@neonlaw.com".to_string()),
            _ => None,
        };
        assert!(Config::from_lookup(env).is_some());
        assert!(Config::from_lookup(|_| None).is_none());
        assert!(Config::from_lookup(|key| {
            if key == "NAVIGATOR_LAWYER_MAILBOXES" {
                Some("nick@neonlaw.com".to_string())
            } else {
                None
            }
        })
        .is_none());
        assert!(Config::from_lookup(|key| {
            if key == "NAVIGATOR_INBOX_FIRM_ID" {
                Some(firm_id.to_string())
            } else {
                None
            }
        })
        .is_none());
    }

    #[test]
    fn config_parses_every_address_normalized_and_deduplicated() {
        let firm_id = fixture_firm();
        let config = Config::from_lookup(|key| match key {
            "NAVIGATOR_INBOX_FIRM_ID" => Some(format!("  {firm_id} ")),
            "NAVIGATOR_LAWYER_MAILBOXES" => {
                Some(" Nick@NeonLaw.com , nick+intake@neonlaw.com,, nick@neonlaw.com ".into())
            }
            _ => None,
        })
        .expect("complete configuration");
        assert_eq!(
            config.addresses,
            vec![
                "nick@neonlaw.com".to_string(),
                "nick+intake@neonlaw.com".to_string()
            ]
        );
        assert_eq!(config.provider_account(), "nick@neonlaw.com");
    }

    #[test]
    fn receiving_alias_prefers_the_envelope_over_the_display_header() {
        let config = config(&["nick@neonlaw.com"], fixture_firm()).unwrap();
        let mut email = email("Client <client@example.com>", "Nick <other@example.com>");
        email.envelope = Some(SmtpEnvelope {
            to: vec!["nick@neonlaw.com".into()],
            from: None,
        });
        assert_eq!(
            config.receiving_alias(&email).as_deref(),
            Some("nick@neonlaw.com")
        );
    }

    #[test]
    fn receiving_alias_falls_back_to_the_display_header_case_insensitively() {
        let config = config(&["nick@neonlaw.com"], fixture_firm()).unwrap();
        let addressed = email("Client <client@example.com>", "Nick <NICK@NEONLAW.COM>");
        assert_eq!(
            config.receiving_alias(&addressed).as_deref(),
            Some("nick@neonlaw.com")
        );
        let unaddressed = email("Client <client@example.com>", "Nick <other@example.com>");
        assert_eq!(config.receiving_alias(&unaddressed), None);
    }

    #[tokio::test]
    async fn unaddressed_mail_is_not_routed() {
        let db = mem_surreal().await;
        let firm_id = seed_firm_with_brand(&db, "neon").await;
        let config = config(&["nick@neonlaw.com"], firm_id);
        let email = email("Client <client@example.com>", "Nick <other@example.com>");
        let ingested = ingest_for_config(&db, &email, config).await.unwrap();
        assert!(ingested.is_none());
    }

    #[tokio::test]
    async fn unconfigured_channel_is_not_routed() {
        let db = mem_surreal().await;
        let email = email("Client <client@example.com>", "Nick <nick@neonlaw.com>");
        let ingested = ingest_for_config(&db, &email, None).await.unwrap();
        assert!(ingested.is_none());
    }

    #[tokio::test]
    async fn lawyer_tier_sender_is_never_inbound_correspondence() {
        let db = mem_surreal().await;
        let firm_id = seed_firm_with_brand(&db, "neon").await;
        ensure_person(
            &db,
            &NewPerson::with_role("Nick", "nick@neonlaw.com", Role::Lawyer),
        )
        .await;
        let config = config(&["nick@neonlaw.com"], firm_id);
        let own_mail = email("Nick <nick@neonlaw.com>", "Nick <nick@neonlaw.com>");
        let ingested = ingest_for_config(&db, &own_mail, config.clone())
            .await
            .unwrap();
        assert!(ingested.is_none());
        let clerk_mail = email("Assistant <clerk@neonlaw.com>", "Nick <nick@neonlaw.com>");
        ensure_person(
            &db,
            &NewPerson::with_role("Assistant", "clerk@neonlaw.com", Role::Clerk),
        )
        .await;
        let ingested = ingest_for_config(&db, &clerk_mail, config).await.unwrap();
        assert!(ingested.is_none());
    }

    #[tokio::test]
    async fn client_mail_ingests_under_one_provider_account_across_aliases() {
        let db = mem_surreal().await;
        let firm_id = seed_firm_with_brand(&db, "neon").await;
        let config = config(&["nick@neonlaw.com", "nick+intake@neonlaw.com"], firm_id);
        let mut first = email("Client <client@example.com>", "Nick <nick@neonlaw.com>");
        first.message_id = Some("first@example.com".into());
        let mut second = email(
            "Client <client@example.com>",
            "Nick <nick+intake@neonlaw.com>",
        );
        second.message_id = Some("second@example.com".into());
        let first_ingested = ingest_for_config(&db, &first, config.clone())
            .await
            .unwrap()
            .expect("first delivery ingests");
        let second_ingested = ingest_for_config(&db, &second, config)
            .await
            .unwrap()
            .expect("second delivery ingests");
        assert!(!first_ingested.deduped);
        assert_eq!(
            first_ingested.conversation_id,
            second_ingested.conversation_id
        );
        let conversation = store::inbox::conversation_by_id(&db, first_ingested.conversation_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(conversation.channel, store::inbox::channel::EMAIL);
        assert_eq!(conversation.provider_account, "nick@neonlaw.com");
        assert_eq!(conversation.firm_id, firm_id);
        assert_eq!(conversation.external_address, "client@example.com");
    }

    #[tokio::test]
    async fn redelivered_message_dedupes_on_the_provider_id() {
        let db = mem_surreal().await;
        let firm_id = seed_firm_with_brand(&db, "neon").await;
        let config = config(&["nick@neonlaw.com"], firm_id);
        let mut email = email("Client <client@example.com>", "Nick <nick@neonlaw.com>");
        email.message_id = Some("repeat@example.com".into());
        let first = ingest_for_config(&db, &email, config.clone())
            .await
            .unwrap()
            .unwrap();
        let second = ingest_for_config(&db, &email, config)
            .await
            .unwrap()
            .unwrap();
        assert!(!first.deduped);
        assert!(second.deduped);
        assert_eq!(first.message_id, second.message_id);
        let conversation = store::inbox::conversation_by_id(&db, first.conversation_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(conversation.unread, true);
    }

    #[tokio::test]
    async fn attachment_bytes_change_the_dedup_key_when_the_provider_id_is_missing() {
        let db = mem_surreal().await;
        let firm_id = seed_firm_with_brand(&db, "neon").await;
        let config = config(&["nick@neonlaw.com"], firm_id);
        let mut email = email("Client <client@example.com>", "Nick <nick@neonlaw.com>");
        email.attachments.push(InboundAttachment {
            filename: "report.pdf".into(),
            content_type: "application/pdf".into(),
            bytes: b"%PDF-1.7 attachment bytes".to_vec(),
        });
        let first = ingest_for_config(&db, &email, config.clone())
            .await
            .unwrap()
            .unwrap();
        let second = ingest_for_config(&db, &email, config)
            .await
            .unwrap()
            .unwrap();
        assert!(!first.deduped);
        assert!(second.deduped);
    }
}
