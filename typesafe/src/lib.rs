//! `TypeSafe` System One client — the Jev-assisted inbox assignment method
//! (LAW-347). One Choice question per unassigned thread: which open Project
//! does this matter belong to, or none.
//!
//! The wire contract is the live `TypeSafe` documentation
//! (<https://docs.typesafe.ai/api/system-one>): `POST /v1/systemone` with a
//! Bearer key, a top-level `state` every question refers to, and a
//! `questions` map whose Choice entries carry `instructions` and a
//! `criteria` map of option name → generated description (255-option
//! ceiling). The answer names its choice, the full probability
//! distribution, and `TypeSafe`'s own confidence for the choice.
//!
//! The crate stays pure of Navigator: no store types, no Firm logic. The
//! portal owns option generation from Navigator rows and the confidence
//! bands that turn an answer into an assignment decision.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// The Jev model the LAW-347 issue pins. `TypeSafe`'s `GET /v1/models`
/// discovers the live names; this is the deployed pin, overridable only in
/// code, so a staging run proves the exact model the decision log records.
pub const MODEL_ID: &str = "jev-1.14.0";

/// The System One endpoint path, appended to the injected base URL.
pub const SYSTEMONE_PATH: &str = "/v1/systemone";

/// The option id that means "not matter mail" — never a Project.
pub const NONE_OPTION: &str = "none";

/// The question's name in the `questions` map. The model never sees the
/// name; the answer comes back under it.
pub const QUESTION_ID: &str = "assignment";

/// The operator flag that opens the LAW-346 zero-data-retention gate. Only
/// written evidence of `TypeSafe`'s ZDR terms (LAW-346) justifies flipping
/// it; a present key alone never does.
pub const ZDR_CONFIRMED_ENV: &str = "NAVIGATOR_TYPESAFE_ZDR_CONFIRMED";

/// The Secret Manager-projected `TypeSafe` key. Read as an environment
/// variable; never a repo file.
pub const API_KEY_ENV: &str = "TYPESAFE_API_KEY";

/// The base URL override — tests point the client at a mock server.
pub const BASE_URL_ENV: &str = "TYPESAFE_API_BASE_URL";

/// The production `TypeSafe` base.
pub const DEFAULT_BASE_URL: &str = "https://api.typesafe.ai";

/// How long one System One call may run before the thread falls back to
/// the inbox. The assignment door runs in bounded batches; a hung model
/// must never eat the operator's run.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Confidence at or above which a clear choice assigns on its own.
pub const AUTO_ASSIGN_CONFIDENCE: f64 = 0.9;

/// Confidence at or above which a clear choice proposes for lawyer
/// confirmation instead of assigning.
pub const PROPOSE_CONFIDENCE: f64 = 0.6;

/// How close a runner-up option's probability may sit to the choice's
/// before the choice counts as tied — the two-Project shape the LAW-347
/// issue calls out. A tie is the lawyer's call, never the model's.
const TIE_EPSILON: f64 = 0.05;

/// The thread headers — and, only when headers alone cannot decide, the
/// short body excerpt — a Jev question refers to. Attachment names ride
/// along; attachment bytes never do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThreadState {
    pub from: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub to: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub cc: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub attachments: Vec<String>,
    /// First excerpt of the message body, quoted history stripped —
    /// populated only on the below-threshold retry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

/// One Choice question: what decides, and every option with its generated
/// description. Option ids are the Project ids (plus [`NONE_OPTION`]);
/// descriptions come from Navigator rows, never literals.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChoiceQuestion {
    /// Always `"choice"` — a Choice selects one of the criteria options.
    pub r#type: &'static str,
    pub instructions: String,
    /// Option name → description, the live docs' `criteria` shape
    /// (≤ 255 entries).
    pub criteria: BTreeMap<String, String>,
}

impl ChoiceQuestion {
    /// A Choice question with the wire's fixed type tag.
    #[must_use]
    pub fn new(instructions: String, criteria: BTreeMap<String, String>) -> Self {
        Self {
            r#type: "choice",
            instructions,
            criteria,
        }
    }
}

/// The part of the System One response this crate reads: the model that
/// actually answered (it may differ from the alias sent) and the answer
/// under [`QUESTION_ID`].
#[derive(Debug, Clone, PartialEq)]
pub struct JevAnswer {
    pub model: String,
    pub choice: String,
    pub confidence: f64,
    pub probabilities: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct SystemOneResponse {
    model: String,
    answers: BTreeMap<String, ChoiceAnswer>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct ChoiceAnswer {
    #[allow(dead_code)] // the wire's discriminator; the choice field carries the value
    r#type: String,
    choice: String,
    confidence: f64,
    probabilities: BTreeMap<String, f64>,
}

/// Why a Jev call did not produce a decision.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JevError {
    /// The key was refused (401/403) — a deployment problem, not a
    /// per-thread one.
    #[error("TypeSafe refused the API key")]
    Auth,
    /// Any other non-success status.
    #[error("TypeSafe returned HTTP {0}")]
    Status(u16),
    /// The call outlived the request timeout.
    #[error("TypeSafe call timed out")]
    Timeout,
    /// A success status whose body did not parse as an answer.
    #[error("TypeSafe response was unreadable: {0}")]
    Malformed(&'static str),
    /// The request never left, or the connection died.
    #[error("TypeSafe call failed: {0}")]
    Http(String),
}

/// The seam the portal's handler tests inject a mock behind. Production
/// builds [`JevClient`] through [`gate_from_env`].
#[async_trait]
pub trait JevAssigner: Send + Sync {
    /// Ask the Choice question about one thread's state.
    ///
    /// # Errors
    /// Any transport, auth, status, or parse failure surfaces here —
    /// the caller degrades to leaving the thread in the inbox.
    async fn choose(
        &self,
        question: &ChoiceQuestion,
        state: &ThreadState,
    ) -> Result<JevAnswer, JevError>;
}

/// The reqwest-backed System One client.
///
/// `Clone` because it rides in `ApiState`, which the axum router clones
/// per request; the client internals (connection pool, key) are cheap to
/// share.
#[derive(Clone)]
pub struct JevClient {
    http: reqwest::Client,
    endpoint: String,
    api_key: String,
}

impl JevClient {
    /// A client for one deployment's key, pointed at one base URL, with
    /// the standard request timeout.
    #[must_use]
    pub fn new(base_url: &str, api_key: &str) -> Self {
        Self::with_timeout(base_url, api_key, REQUEST_TIMEOUT)
    }

    /// The same client with an injected timeout — the seam the timeout
    /// test needs, mirroring the injected base URL.
    #[must_use]
    pub fn with_timeout(base_url: &str, api_key: &str, timeout: Duration) -> Self {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("invariant: the reqwest client builder without a proxy config always builds");
        Self {
            http,
            endpoint: format!("{}{SYSTEMONE_PATH}", base_url.trim_end_matches('/')),
            api_key: api_key.to_owned(),
        }
    }
}

#[async_trait]
impl JevAssigner for JevClient {
    async fn choose(
        &self,
        question: &ChoiceQuestion,
        state: &ThreadState,
    ) -> Result<JevAnswer, JevError> {
        let request = serde_json::json!({
            "model": MODEL_ID,
            "state": state,
            "questions": { QUESTION_ID: question },
        });
        let response = self
            .http
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    JevError::Timeout
                } else {
                    JevError::Http(error.to_string())
                }
            })?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| JevError::Http(error.to_string()))?;
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(JevError::Auth);
        }
        if !status.is_success() {
            return Err(JevError::Status(status.as_u16()));
        }
        let parsed: SystemOneResponse =
            serde_json::from_str(&body).map_err(|_| JevError::Malformed("answers"))?;
        let answer = parsed
            .answers
            .get(QUESTION_ID)
            .ok_or(JevError::Malformed("missing answer"))?;
        if !question.criteria.contains_key(&answer.choice) {
            return Err(JevError::Malformed("unoffered answer choice"));
        }
        Ok(JevAnswer {
            model: parsed.model,
            choice: answer.choice.clone(),
            confidence: answer.confidence,
            probabilities: answer.probabilities.clone(),
        })
    }
}

/// Why the ZDR gate refuses a Jev call. Distinct spellings so the
/// decision log records which condition failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GateRefusal {
    #[error("the ZDR operator flag is not confirmed")]
    ZdrNotConfirmed,
    #[error("no TypeSafe API key is present")]
    MissingApiKey,
}

/// The LAW-346 gate: a Jev client exists only when BOTH the operator flag
/// is explicitly confirmed AND a key is present. Read at router-build
/// time; a leaked key alone never enables model calls.
///
/// # Errors
/// The refusal names which condition is missing, for the deployment log.
pub fn gate_from_env() -> Result<JevClient, GateRefusal> {
    let confirmed = std::env::var(ZDR_CONFIRMED_ENV)
        .is_ok_and(|value| matches!(value.trim(), "true" | "1" | "yes"));
    if !confirmed {
        return Err(GateRefusal::ZdrNotConfirmed);
    }
    let key = std::env::var(API_KEY_ENV)
        .ok()
        .filter(|key| !key.trim().is_empty())
        .ok_or(GateRefusal::MissingApiKey)?;
    let base = std::env::var(BASE_URL_ENV)
        .ok()
        .filter(|base| !base.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned());
    Ok(JevClient::new(base.trim(), key.trim()))
}

/// Why a Jev answer leaves the thread in the inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaveReason {
    /// The answer carried no usable probability distribution.
    NoProbabilities,
    /// The model chose [`NONE_OPTION`]: not matter mail.
    NoneChosen,
    /// The top confidence sat under the proposal band.
    BelowThreshold,
    /// A runner-up option sat within [`TIE_EPSILON`] of the choice.
    TwoProjectTie,
}

/// What a Jev answer means for one thread. The portal maps the option ids
/// onto Project rows; this enum only knows the bands.
#[derive(Debug, Clone, PartialEq)]
pub enum JevOutcome {
    /// Clear choice at or above [`AUTO_ASSIGN_CONFIDENCE`] — assign.
    AutoAssign(String),
    /// Clear choice in the [`PROPOSE_CONFIDENCE`] band — lawyer confirms.
    Propose(String),
    /// No confident choice — the thread stays in the inbox.
    Leave(LeaveReason),
}

/// Band one answer. The tie guard inspects the runner-up: two Projects
/// within [`TIE_EPSILON`] of each other are ambiguous however sharp the
/// margin over `none`, because the choice between them is the lawyer's.
#[must_use]
pub fn band(answer: &JevAnswer) -> JevOutcome {
    if answer.probabilities.is_empty() {
        return JevOutcome::Leave(LeaveReason::NoProbabilities);
    }
    if answer.choice == NONE_OPTION {
        return JevOutcome::Leave(LeaveReason::NoneChosen);
    }
    let choice_probability = answer
        .probabilities
        .get(&answer.choice)
        .copied()
        .unwrap_or(answer.confidence);
    let runner_up = answer
        .probabilities
        .iter()
        .filter(|(id, _)| id.as_str() != answer.choice.as_str())
        .map(|(_, probability)| *probability)
        .max_by(f64::total_cmp);
    if runner_up.is_some_and(|runner_up| (choice_probability - runner_up).abs() < TIE_EPSILON) {
        return JevOutcome::Leave(LeaveReason::TwoProjectTie);
    }
    if answer.confidence >= AUTO_ASSIGN_CONFIDENCE {
        JevOutcome::AutoAssign(answer.choice.clone())
    } else if answer.confidence >= PROPOSE_CONFIDENCE {
        JevOutcome::Propose(answer.choice.clone())
    } else {
        JevOutcome::Leave(LeaveReason::BelowThreshold)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        band, gate_from_env, ChoiceQuestion, JevAnswer, JevAssigner, JevClient, JevError,
        JevOutcome, LeaveReason, SystemOneResponse, API_KEY_ENV, AUTO_ASSIGN_CONFIDENCE,
        BASE_URL_ENV, MODEL_ID, NONE_OPTION, PROPOSE_CONFIDENCE, QUESTION_ID, SYSTEMONE_PATH,
        ZDR_CONFIRMED_ENV,
    };
    use std::collections::BTreeMap;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn answer(choice: &str, confidence: f64, probabilities: &[(&str, f64)]) -> JevAnswer {
        JevAnswer {
            model: MODEL_ID.to_owned(),
            choice: choice.to_owned(),
            confidence,
            probabilities: probabilities
                .iter()
                .map(|(id, p)| ((*id).to_owned(), *p))
                .collect(),
        }
    }

    fn docs_response_body() -> String {
        // The Choice answer shape from the live TypeSafe docs, with our
        // question name.
        format!(
            r#"{{"model":"{MODEL_ID}","answers":{{"{QUESTION_ID}":{{"type":"choice","choice":"returns","confidence":1.0,"probabilities":{{"shipping":0.0,"returns":1.0,"billing":0.0}}}}}},"usage":{{"input_tokens":328,"output_tokens":34}}}}"#
        )
    }

    #[test]
    fn a_clear_choice_at_nine_tenths_auto_assigns() {
        let outcome = band(&answer(
            "proj-1",
            AUTO_ASSIGN_CONFIDENCE,
            &[("proj-1", 0.95), (NONE_OPTION, 0.05)],
        ));
        assert_eq!(outcome, JevOutcome::AutoAssign("proj-1".to_owned()));
    }

    #[test]
    fn a_clear_choice_in_the_middle_band_proposes() {
        let outcome = band(&answer(
            "proj-1",
            PROPOSE_CONFIDENCE,
            &[("proj-1", 0.7), (NONE_OPTION, 0.3)],
        ));
        assert_eq!(outcome, JevOutcome::Propose("proj-1".to_owned()));
    }

    #[test]
    fn a_choice_below_the_proposal_band_leaves_the_thread() {
        let outcome = band(&answer(
            "proj-1",
            0.4,
            &[("proj-1", 0.4), (NONE_OPTION, 0.6)],
        ));
        assert_eq!(outcome, JevOutcome::Leave(LeaveReason::BelowThreshold));
    }

    #[test]
    fn the_none_option_never_assigns() {
        for confidence in [1.0_f64, 0.95, 0.7, 0.4] {
            let outcome = band(&answer(
                NONE_OPTION,
                confidence,
                &[("proj-1", 1.0 - confidence), (NONE_OPTION, confidence)],
            ));
            assert_eq!(outcome, JevOutcome::Leave(LeaveReason::NoneChosen));
        }
    }

    #[test]
    fn an_answer_without_probabilities_leaves_the_thread() {
        let outcome = band(&answer("proj-1", 1.0, &[]));
        assert_eq!(outcome, JevOutcome::Leave(LeaveReason::NoProbabilities));
    }

    #[test]
    fn a_two_project_tie_leaves_the_thread_even_at_full_confidence() {
        let outcome = band(&answer("proj-1", 1.0, &[("proj-1", 0.5), ("proj-2", 0.5)]));
        assert_eq!(outcome, JevOutcome::Leave(LeaveReason::TwoProjectTie));
    }

    #[test]
    fn a_runner_up_within_the_epsilon_blocks_the_auto_band() {
        let outcome = band(&answer(
            "proj-1",
            AUTO_ASSIGN_CONFIDENCE,
            &[("proj-1", 0.46), ("proj-2", 0.44), (NONE_OPTION, 0.10)],
        ));
        assert_eq!(outcome, JevOutcome::Leave(LeaveReason::TwoProjectTie));
    }

    #[test]
    fn a_choice_with_no_probability_row_falls_back_to_confidence() {
        // A well-formed answer names a probability for every option, but a
        // degraded one that names the choice without its row must not
        // panic — the confidence field stands in.
        let outcome = band(&answer(
            "proj-1",
            AUTO_ASSIGN_CONFIDENCE,
            &[(NONE_OPTION, 0.05)],
        ));
        assert_eq!(outcome, JevOutcome::AutoAssign("proj-1".to_owned()));
    }

    #[test]
    fn the_wire_round_trip_matches_the_live_docs_shape() {
        let mut criteria = BTreeMap::new();
        criteria.insert("proj-1".to_owned(), "Local Project".to_owned());
        criteria.insert(NONE_OPTION.to_owned(), "Not matter mail".to_owned());
        let question = ChoiceQuestion::new("Assign the thread.".to_owned(), criteria);
        let state = super::ThreadState {
            from: "client@example.test".to_owned(),
            to: vec!["efiling-notice@firm.test".to_owned()],
            cc: Vec::new(),
            subject: Some("Re: filing status".to_owned()),
            date: Some("2026-11-15T00:00:00Z".to_owned()),
            attachments: vec!["notice.pdf".to_owned()],
            body: None,
        };
        let request = serde_json::json!({
            "model": MODEL_ID,
            "state": state,
            "questions": { QUESTION_ID: question },
        });
        let serialized = serde_json::to_value(&request).unwrap();
        assert_eq!(serialized["model"], MODEL_ID);
        assert_eq!(serialized["state"]["from"], "client@example.test");
        assert_eq!(serialized["state"]["body"], serde_json::Value::Null);
        let question = &serialized["questions"][QUESTION_ID];
        assert_eq!(question["type"], "choice");
        assert_eq!(question["criteria"]["proj-1"], "Local Project");
        assert_eq!(question["criteria"][NONE_OPTION], "Not matter mail");

        // The docs' response example parses into our answer shape.
        let parsed: SystemOneResponse = serde_json::from_str(&docs_response_body()).unwrap();
        let answer = parsed.answers.get(QUESTION_ID).unwrap();
        assert_eq!(answer.choice, "returns");
        assert!((answer.confidence - 1.0).abs() < 1e-9);
        assert_eq!(answer.probabilities.get("returns"), Some(&1.0));
    }

    #[tokio::test]
    async fn a_client_call_carries_the_bearer_key_and_parses_the_answer() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(SYSTEMONE_PATH))
            .and(header("Authorization", "Bearer test-key"))
            .and(body_partial_json(serde_json::json!({ "model": MODEL_ID })))
            .respond_with(ResponseTemplate::new(200).set_body_string(docs_response_body()))
            .mount(&server)
            .await;
        let client = JevClient::new(&server.uri(), "test-key");
        let question = ChoiceQuestion::new(
            "Assign the thread.".into(),
            BTreeMap::from([("returns".into(), "Returns".into())]),
        );
        let state = super::ThreadState {
            from: "client@example.test".to_owned(),
            to: Vec::new(),
            cc: Vec::new(),
            subject: None,
            date: None,
            attachments: Vec::new(),
            body: None,
        };
        let answer = client.choose(&question, &state).await.unwrap();
        assert_eq!(answer.model, MODEL_ID);
        assert_eq!(answer.choice, "returns");
        assert!((answer.confidence - 1.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn an_auth_refusal_maps_to_the_auth_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let client = JevClient::new(&server.uri(), "test-key");
        let question = ChoiceQuestion::new("Assign the thread.".to_owned(), BTreeMap::new());
        let state = super::ThreadState {
            from: "client@example.test".to_owned(),
            to: Vec::new(),
            cc: Vec::new(),
            subject: None,
            date: None,
            attachments: Vec::new(),
            body: None,
        };
        assert!(matches!(
            client.choose(&question, &state).await,
            Err(JevError::Auth)
        ));
    }

    #[tokio::test]
    async fn a_server_error_maps_to_its_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let client = JevClient::new(&server.uri(), "test-key");
        let question = ChoiceQuestion::new("Assign the thread.".to_owned(), BTreeMap::new());
        let state = super::ThreadState {
            from: "client@example.test".to_owned(),
            to: Vec::new(),
            cc: Vec::new(),
            subject: None,
            date: None,
            attachments: Vec::new(),
            body: None,
        };
        assert!(matches!(
            client.choose(&question, &state).await,
            Err(JevError::Status(500))
        ));
    }

    #[tokio::test]
    async fn an_unparseable_success_body_maps_to_malformed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;
        let client = JevClient::new(&server.uri(), "test-key");
        let question = ChoiceQuestion::new("Assign the thread.".to_owned(), BTreeMap::new());
        let state = super::ThreadState {
            from: "client@example.test".to_owned(),
            to: Vec::new(),
            cc: Vec::new(),
            subject: None,
            date: None,
            attachments: Vec::new(),
            body: None,
        };
        assert!(matches!(
            client.choose(&question, &state).await,
            Err(JevError::Malformed(_))
        ));
    }

    #[tokio::test]
    async fn a_hung_server_maps_to_the_timeout_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(5)))
            .mount(&server)
            .await;
        let client = JevClient::with_timeout(
            &server.uri(),
            "test-key",
            std::time::Duration::from_millis(200),
        );
        let question = ChoiceQuestion::new("Assign the thread.".to_owned(), BTreeMap::new());
        let state = super::ThreadState {
            from: "client@example.test".to_owned(),
            to: Vec::new(),
            cc: Vec::new(),
            subject: None,
            date: None,
            attachments: Vec::new(),
            body: None,
        };
        assert!(matches!(
            client.choose(&question, &state).await,
            Err(JevError::Timeout)
        ));
    }

    #[test]
    fn the_gate_refuses_without_the_operator_flag() {
        std::env::remove_var(ZDR_CONFIRMED_ENV);
        std::env::remove_var(API_KEY_ENV);
        assert!(matches!(
            gate_from_env(),
            Err(super::GateRefusal::ZdrNotConfirmed)
        ));

        std::env::set_var(ZDR_CONFIRMED_ENV, "false");
        std::env::set_var(API_KEY_ENV, "test-key");
        assert!(matches!(
            gate_from_env(),
            Err(super::GateRefusal::ZdrNotConfirmed)
        ));

        std::env::set_var(ZDR_CONFIRMED_ENV, "true");
        std::env::remove_var(API_KEY_ENV);
        assert!(matches!(
            gate_from_env(),
            Err(super::GateRefusal::MissingApiKey)
        ));

        std::env::set_var(API_KEY_ENV, "");
        assert!(matches!(
            gate_from_env(),
            Err(super::GateRefusal::MissingApiKey)
        ));

        std::env::set_var(API_KEY_ENV, "test-key");
        std::env::set_var(BASE_URL_ENV, "http://127.0.0.1:1");
        assert!(gate_from_env().is_ok());
        std::env::remove_var(ZDR_CONFIRMED_ENV);
        std::env::remove_var(API_KEY_ENV);
        std::env::remove_var(BASE_URL_ENV);
    }
}
