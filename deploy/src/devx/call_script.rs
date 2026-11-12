//! `ops call-script` — the answer menu on a Twilio receiving number.
//!
//! A Call Script is what a caller hears when a Brand Phone Number picks up:
//! a greeting, one extension per person who takes calls, and the message
//! everyone else hears. The number's Twilio Studio Flow runs it; this
//! command renders the Flow definition from the script and publishes it, so
//! the menu is reconciled from flags rather than drawn in a console.
//!
//! Read-only by default: the command reports whether the published Flow
//! already matches and changes nothing until `--apply`. A forwarding
//! destination is a person's own phone, so it arrives as a flag and is
//! never compiled in or echoed back.
//!
//! Auth is the Twilio account's own `TWILIO_ACCOUNT_SID` and
//! `TWILIO_AUTH_TOKEN`. With `--project-id` both are read from that
//! deployment's Secret Manager for the one run and held only in memory;
//! otherwise they come from the environment. Tests stand up a `wiremock`
//! server and point [`TwilioStudio::with_base_urls`] at it.

use std::str::FromStr;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

const TWILIO_API_BASE_URL: &str = "https://api.twilio.com";
const TWILIO_STUDIO_BASE_URL: &str = "https://studio.twilio.com";

/// One person a caller can reach: the digits they key, the name the
/// greeting speaks, and the phone the call forwards to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extension {
    pub digits: String,
    pub name: String,
    pub forward_to: String,
}

impl FromStr for Extension {
    type Err = String;

    /// `<DIGITS>=<NAME>=<E.164>`, the shape `--extension` takes.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = value.split('=').map(str::trim).collect();
        let [digits, name, forward_to] = parts[..] else {
            return Err("expected <DIGITS>=<NAME>=<E.164 NUMBER>".into());
        };
        if !(1..=10).contains(&digits.len()) || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err("the extension is one to ten digits".into());
        }
        if name.is_empty() {
            return Err("the extension names who it reaches".into());
        }
        if !store::brand_phone_numbers::is_e164(forward_to) {
            return Err("the forwarding number is E.164, such as +15555550142".into());
        }
        Ok(Self {
            digits: digits.into(),
            name: name.into(),
            forward_to: forward_to.into(),
        })
    }
}

/// What a caller hears on one receiving number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallScript {
    /// The opening sentence, before the extensions are offered.
    pub greeting: String,
    pub extensions: Vec<Extension>,
    /// Said, before hanging up, to a caller who keys nothing or an unknown extension.
    pub fallback: String,
}

impl CallScript {
    fn validate(&self) -> Result<()> {
        if self.greeting.trim().is_empty() || self.fallback.trim().is_empty() {
            bail!("a call script needs a greeting and a fallback message");
        }
        if self.extensions.is_empty() {
            bail!("a call script needs at least one --extension");
        }
        let mut digits: Vec<&str> = self.extensions.iter().map(|e| e.digits.as_str()).collect();
        digits.sort_unstable();
        digits.dedup();
        if digits.len() != self.extensions.len() {
            bail!("two extensions share the same digits");
        }
        Ok(())
    }

    /// The full greeting: the opening sentence, then each extension spoken
    /// digit by digit so "702" is not read as a quantity.
    fn spoken_greeting(&self) -> String {
        let mut sentences = vec![self.greeting.trim().to_string()];
        for extension in &self.extensions {
            let keys: Vec<String> = extension.digits.chars().map(String::from).collect();
            sentences.push(format!(
                "To reach {}, press {}, followed by the pound key.",
                extension.name,
                keys.join(" ")
            ));
        }
        sentences.join(" ")
    }

    /// The Studio Flow definition that runs this script: gather digits up
    /// to `#`, forward a known extension with the caller's own number as
    /// caller ID, and say the fallback to everyone else.
    #[must_use]
    pub fn definition(&self) -> Value {
        const DIGITS: &str = "{{widgets.gather_input.Digits}}";
        let mut matches = vec![json!({"event": "noMatch", "next": "say_fallback"})];
        let mut forwards = Vec::new();
        for (index, extension) in self.extensions.iter().enumerate() {
            let widget = format!("forward_{}", extension.digits);
            matches.push(json!({
                "event": "match",
                "next": widget,
                "conditions": [{
                    "friendly_name": extension.digits,
                    "arguments": [DIGITS],
                    "type": "equal_to",
                    "value": extension.digits,
                }],
            }));
            forwards.push(json!({
                "name": widget,
                "type": "connect-call-to",
                "properties": {
                    "noun": "number",
                    "to": extension.forward_to,
                    "caller_id": "{{contact.channel.address}}",
                    "timeout": 30,
                    "record": false,
                    "offset": {"x": 20 + 260 * index, "y": 760},
                },
                "transitions": [{"event": "callCompleted"}, {"event": "hangup"}],
            }));
        }
        let mut states = vec![
            json!({
                "name": "Trigger",
                "type": "trigger",
                "properties": {"offset": {"x": 250, "y": 50}},
                "transitions": [
                    {"event": "incomingMessage"},
                    {"event": "incomingCall", "next": "gather_input"},
                    {"event": "incomingConversationMessage"},
                    {"event": "incomingRequest"},
                    {"event": "incomingParent"},
                ],
            }),
            json!({
                "name": "gather_input",
                "type": "gather-input-on-call",
                "properties": {
                    "say": self.spoken_greeting(),
                    "voice": "alice",
                    "language": "en",
                    "loop": 1,
                    "timeout": 5,
                    "finish_on_key": "#",
                    "stop_gather": true,
                    "offset": {"x": 290, "y": 250},
                },
                "transitions": [
                    {"event": "keypress", "next": "split_extension"},
                    {"event": "speech", "next": "say_fallback"},
                    {"event": "timeout", "next": "say_fallback"},
                ],
            }),
            json!({
                "name": "split_extension",
                "type": "split-based-on",
                "properties": {"input": DIGITS, "offset": {"x": 120, "y": 500}},
                "transitions": matches,
            }),
        ];
        states.extend(forwards);
        states.push(json!({
            "name": "say_fallback",
            "type": "say-play",
            "properties": {
                "say": self.fallback.trim(),
                "voice": "alice",
                "language": "en",
                "loop": 1,
                "offset": {"x": 20 + 260 * self.extensions.len(), "y": 760},
            },
            "transitions": [{"event": "audioComplete"}],
        }));
        json!({
            "description": "Call Script",
            "flags": {"allow_concurrent_calls": true},
            "initial_state": "Trigger",
            "states": states,
        })
    }
}

/// The Twilio account that owns the receiving number and its Studio Flow.
pub struct TwilioStudio {
    http: reqwest::Client,
    /// The Twilio Account SID: the account's public identifier, which
    /// Twilio's own resource paths carry. The auth token is the credential.
    sid: String,
    auth_token: String,
    api_base_url: String,
    studio_base_url: String,
}

/// A published Studio Flow, as Twilio reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct Flow {
    pub sid: String,
    pub revision: i64,
    pub definition: Value,
}

impl TwilioStudio {
    #[must_use]
    pub fn new(sid: impl Into<String>, auth_token: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            sid: sid.into(),
            auth_token: auth_token.into(),
            api_base_url: TWILIO_API_BASE_URL.into(),
            studio_base_url: TWILIO_STUDIO_BASE_URL.into(),
        }
    }

    /// Build from `TWILIO_ACCOUNT_SID` and `TWILIO_AUTH_TOKEN`; `lookup` is
    /// `std::env::var` in production.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let read = |name: &str| {
            lookup(name)
                .filter(|value| !value.trim().is_empty())
                .with_context(|| format!("{name} is not set"))
        };
        Ok(Self::new(
            read("TWILIO_ACCOUNT_SID")?,
            read("TWILIO_AUTH_TOKEN")?,
        ))
    }

    /// Build from a deployment's own Secret Manager: the same two objects
    /// its pods receive, read at `versions/latest` and never written down.
    pub async fn for_deployment(
        client: &super::gcp::client::GcpClient,
        project_id: &str,
    ) -> Result<Self> {
        let read = |key: &'static str| async move {
            super::deployments::record_value(client, project_id, key)
                .await
                .map(|value| value.trim().to_string())
        };
        Ok(Self::new(
            read("TWILIO_ACCOUNT_SID").await?,
            read("TWILIO_AUTH_TOKEN").await?,
        ))
    }

    #[cfg(test)]
    fn with_base_urls(mut self, api: impl Into<String>, studio: impl Into<String>) -> Self {
        self.api_base_url = api.into();
        self.studio_base_url = studio.into();
        self
    }

    async fn json(&self, request: reqwest::RequestBuilder, what: &str) -> Result<Value> {
        let response = request
            .basic_auth(&self.sid, Some(&self.auth_token))
            .send()
            .await
            .with_context(|| format!("{what}: Twilio was unreachable"))?;
        let status = response.status();
        if !status.is_success() {
            bail!("{what}: Twilio answered {status}");
        }
        response
            .json()
            .await
            .with_context(|| format!("{what}: Twilio's answer was not JSON"))
    }

    /// The SID of the Studio Flow that answers `number`. A number Twilio
    /// does not hold, or one answered by anything other than a Studio Flow,
    /// is refused: the command edits an existing Flow and never repoints a
    /// number.
    async fn flow_sid_for_number(&self, number: &str) -> Result<String> {
        let url = format!(
            "{}/2010-04-01/Accounts/{}/IncomingPhoneNumbers.json",
            self.api_base_url, self.sid
        );
        let body = self
            .json(
                self.http.get(url).query(&[("PhoneNumber", number)]),
                "look up the receiving number",
            )
            .await?;
        let voice_url = body["incoming_phone_numbers"]
            .get(0)
            .context("the Twilio account does not hold that number")?["voice_url"]
            .as_str()
            .unwrap_or_default();
        voice_url
            .split_once("/Flows/")
            .map(|(_, sid)| sid.trim_end_matches('/'))
            .filter(|sid| sid.starts_with("FW") && sid.bytes().all(|b| b.is_ascii_alphanumeric()))
            .map(str::to_string)
            .context("the number is not answered by a Twilio Studio Flow")
    }

    async fn flow(&self, sid: &str) -> Result<Flow> {
        let url = format!("{}/v2/Flows/{sid}", self.studio_base_url);
        let body = self
            .json(self.http.get(url), "read the Studio Flow")
            .await?;
        flow_from(&body)
    }

    async fn publish(&self, sid: &str, definition: &Value) -> Result<Flow> {
        let url = format!("{}/v2/Flows/{sid}", self.studio_base_url);
        let form = [
            ("Status", "published".to_string()),
            ("CommitMessage", "hudson ops call-script".to_string()),
            ("Definition", definition.to_string()),
        ];
        let body = self
            .json(self.http.post(url).form(&form), "publish the Studio Flow")
            .await?;
        flow_from(&body)
    }
}

fn flow_from(body: &Value) -> Result<Flow> {
    Ok(Flow {
        sid: body["sid"]
            .as_str()
            .context("the Studio Flow has no sid")?
            .to_string(),
        revision: body["revision"].as_i64().unwrap_or_default(),
        definition: body["definition"].clone(),
    })
}

/// What one run found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The published Flow already runs this script.
    Unchanged { flow_sid: String, revision: i64 },
    /// The published Flow differs; nothing was changed.
    Planned { flow_sid: String, revision: i64 },
    /// The script was published as a new revision.
    Published { flow_sid: String, revision: i64 },
}

impl Outcome {
    /// The one line the operator reads. It names only what Twilio reported
    /// — never the script's own text or a forwarding number.
    #[must_use]
    pub fn line(&self) -> String {
        match self {
            Self::Unchanged { flow_sid, revision } => {
                format!("UNCHANGED  Studio Flow {flow_sid} revision {revision} runs this call script")
            }
            Self::Planned { flow_sid, revision } => format!(
                "UPDATE     Studio Flow {flow_sid} revision {revision} differs; rerun with --apply to publish"
            ),
            Self::Published { flow_sid, revision } => {
                format!("PUBLISHED  Studio Flow {flow_sid} revision {revision}")
            }
        }
    }
}

/// Compare the script with the Flow that answers `number`, and publish it
/// when `apply` is set and they differ.
pub async fn reconcile(
    twilio: &TwilioStudio,
    number: &str,
    script: &CallScript,
    apply: bool,
) -> Result<Outcome> {
    if !store::brand_phone_numbers::is_e164(number) {
        bail!("--number is E.164, such as +15555550100");
    }
    script.validate()?;
    let desired = script.definition();
    let current = twilio
        .flow(&twilio.flow_sid_for_number(number).await?)
        .await?;
    if current.definition == desired {
        return Ok(Outcome::Unchanged {
            flow_sid: current.sid,
            revision: current.revision,
        });
    }
    if !apply {
        return Ok(Outcome::Planned {
            flow_sid: current.sid,
            revision: current.revision,
        });
    }
    let published = twilio.publish(&current.sid, &desired).await?;
    Ok(Outcome::Published {
        flow_sid: published.sid,
        revision: published.revision,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, header_exists, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const NUMBER: &str = "+15555550100";
    const FLOW: &str = "FW00000000000000000000000000000001";

    fn script() -> CallScript {
        CallScript {
            greeting: "Hello, you have reached Example Law.".into(),
            extensions: vec!["702=Aries Example=+15555550142".parse().unwrap()],
            fallback: "Please email support. Goodbye.".into(),
        }
    }

    async fn twilio(server: &MockServer, voice_url: &str) -> TwilioStudio {
        Mock::given(method("GET"))
            .and(path("/2010-04-01/Accounts/AC1/IncomingPhoneNumbers.json"))
            .and(query_param("PhoneNumber", NUMBER))
            .and(header_exists("authorization"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "incoming_phone_numbers": [{"voice_url": voice_url}],
            })))
            .mount(server)
            .await;
        TwilioStudio::new("AC1", "token").with_base_urls(server.uri(), server.uri())
    }

    async fn published_flow(server: &MockServer, definition: Value) {
        Mock::given(method("GET"))
            .and(path(format!("/v2/Flows/{FLOW}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "sid": FLOW, "revision": 4, "definition": definition,
            })))
            .mount(server)
            .await;
    }

    fn flow_url() -> String {
        format!("https://webhooks.twilio.com/v1/Accounts/AC1/Flows/{FLOW}")
    }

    #[test]
    fn an_extension_parses_digits_a_name_and_an_e164_destination() {
        let extension: Extension = "702 = Aries Example = +15555550142".parse().unwrap();
        assert_eq!(extension.digits, "702");
        assert_eq!(extension.name, "Aries Example");
        assert_eq!(extension.forward_to, "+15555550142");
        for refused in [
            "702=Aries Example",
            "7a2=Aries Example=+15555550142",
            "=Aries Example=+15555550142",
            "702==+15555550142",
            "702=Aries Example=5555550142",
        ] {
            assert!(refused.parse::<Extension>().is_err(), "{refused}");
        }
    }

    #[test]
    fn the_flow_offers_each_extension_and_forwards_only_a_match() {
        let definition = script().definition();
        let state = |name: &str| {
            definition["states"]
                .as_array()
                .unwrap()
                .iter()
                .find(|state| state["name"] == name)
                .unwrap_or_else(|| panic!("no {name} widget"))
                .clone()
        };
        let gather = state("gather_input");
        assert_eq!(
            gather["properties"]["say"],
            "Hello, you have reached Example Law. To reach Aries Example, press 7 0 2, \
             followed by the pound key."
        );
        assert_eq!(gather["properties"]["finish_on_key"], "#");
        let split = state("split_extension");
        assert_eq!(split["transitions"][0]["event"], "noMatch");
        assert_eq!(split["transitions"][0]["next"], "say_fallback");
        assert_eq!(split["transitions"][1]["next"], "forward_702");
        assert_eq!(split["transitions"][1]["conditions"][0]["value"], "702");
        let forward = state("forward_702");
        assert_eq!(forward["properties"]["to"], "+15555550142");
        assert_eq!(
            forward["properties"]["caller_id"],
            "{{contact.channel.address}}"
        );
        assert_eq!(
            state("say_fallback")["properties"]["say"],
            "Please email support. Goodbye."
        );
    }

    #[tokio::test]
    async fn a_matching_flow_is_left_alone_even_with_apply() {
        let server = MockServer::start().await;
        let twilio = twilio(&server, &flow_url()).await;
        published_flow(&server, script().definition()).await;

        let outcome = reconcile(&twilio, NUMBER, &script(), true).await.unwrap();

        assert_eq!(
            outcome,
            Outcome::Unchanged {
                flow_sid: FLOW.into(),
                revision: 4
            }
        );
        assert!(outcome.line().starts_with("UNCHANGED"));
    }

    #[tokio::test]
    async fn a_differing_flow_is_only_planned_without_apply() {
        let server = MockServer::start().await;
        let twilio = twilio(&server, &flow_url()).await;
        published_flow(&server, json!({"states": []})).await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let outcome = reconcile(&twilio, NUMBER, &script(), false).await.unwrap();

        assert_eq!(
            outcome,
            Outcome::Planned {
                flow_sid: FLOW.into(),
                revision: 4
            }
        );
        let line = outcome.line();
        assert!(line.contains("--apply"), "{line}");
        assert!(!line.contains("5555550142"), "no forwarding number: {line}");
    }

    #[tokio::test]
    async fn apply_publishes_the_rendered_definition() {
        let server = MockServer::start().await;
        let twilio = twilio(&server, &flow_url()).await;
        published_flow(&server, json!({"states": []})).await;
        Mock::given(method("POST"))
            .and(path(format!("/v2/Flows/{FLOW}")))
            .and(body_string_contains("Status=published"))
            .and(body_string_contains("forward_702"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "sid": FLOW, "revision": 5, "definition": script().definition(),
            })))
            .expect(1)
            .mount(&server)
            .await;

        let outcome = reconcile(&twilio, NUMBER, &script(), true).await.unwrap();

        assert_eq!(
            outcome,
            Outcome::Published {
                flow_sid: FLOW.into(),
                revision: 5
            }
        );
        assert_eq!(
            outcome.line(),
            format!("PUBLISHED  Studio Flow {FLOW} revision 5")
        );
    }

    #[tokio::test]
    async fn a_number_without_a_studio_flow_is_refused() {
        let server = MockServer::start().await;
        let twilio = twilio(&server, "https://example.test/webhook/voice").await;

        let error = reconcile(&twilio, NUMBER, &script(), true)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("Studio Flow"), "{error}");
    }

    #[tokio::test]
    async fn an_unheld_number_and_a_twilio_refusal_are_named() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(query_param("PhoneNumber", NUMBER))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"incoming_phone_numbers": []})),
            )
            .mount(&server)
            .await;
        let twilio = TwilioStudio::new("AC1", "token").with_base_urls(server.uri(), server.uri());
        let error = reconcile(&twilio, NUMBER, &script(), false)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("does not hold"), "{error}");

        let refusing = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&refusing)
            .await;
        let twilio =
            TwilioStudio::new("AC1", "token").with_base_urls(refusing.uri(), refusing.uri());
        let error = reconcile(&twilio, NUMBER, &script(), false)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("401"), "{error}");
    }

    #[tokio::test]
    async fn an_invalid_script_is_refused_before_twilio_is_called() {
        let twilio = TwilioStudio::new("AC1", "token")
            .with_base_urls("http://127.0.0.1:1", "http://127.0.0.1:1");
        let mut duplicated = script();
        duplicated.extensions.push(duplicated.extensions[0].clone());
        let mut silent = script();
        silent.fallback = " ".into();
        let mut empty = script();
        empty.extensions.clear();
        for invalid in [duplicated, silent, empty] {
            assert!(reconcile(&twilio, NUMBER, &invalid, false).await.is_err());
        }
        assert!(reconcile(&twilio, "5555550100", &script(), false)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn credentials_are_read_from_the_deployment_secret_manager() {
        use crate::devx::gcp::client::{GcpClient, GcpService, StaticToken};
        use base64::Engine as _;
        let server = MockServer::start().await;
        for (key, value) in [
            ("TWILIO_ACCOUNT_SID", "AC1\n"),
            ("TWILIO_AUTH_TOKEN", "token"),
        ] {
            Mock::given(method("GET"))
                .and(path(format!(
                    "/v1/projects/example-test/secrets/{key}/versions/latest:access"
                )))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "payload": {"data": base64::engine::general_purpose::STANDARD.encode(value)},
                })))
                .mount(&server)
                .await;
        }
        let client = GcpClient::new(std::sync::Arc::new(StaticToken("t".into())))
            .with_base_url(GcpService::SecretManager, server.uri());

        let twilio = TwilioStudio::for_deployment(&client, "example-test")
            .await
            .unwrap();
        assert_eq!(twilio.sid, "AC1", "a trailing newline is trimmed");
        assert_eq!(twilio.auth_token, "token");

        let error = TwilioStudio::for_deployment(&client, "example-empty")
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("TWILIO_ACCOUNT_SID"), "{error}");
    }

    #[test]
    fn credentials_come_from_the_twilio_environment_names() {
        let lookup = |name: &str| match name {
            "TWILIO_ACCOUNT_SID" => Some("AC1".to_string()),
            "TWILIO_AUTH_TOKEN" => Some("token".to_string()),
            _ => None,
        };
        assert_eq!(TwilioStudio::from_lookup(lookup).unwrap().sid, "AC1");
        let error = TwilioStudio::from_lookup(|_| None).err().unwrap();
        assert!(error.to_string().contains("TWILIO_ACCOUNT_SID"), "{error}");
    }
}
