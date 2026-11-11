//! Task-owned synthetic HTTP providers for the email-summary e2e: the Vertex
//! metadata token endpoint, Gemini `generateContent`, and the Slack Web API.
//!
//! One axum server answers all three on a host port the in-cluster worker
//! reaches by the host's address. Behaviour is scripted per call (the front of
//! a queue is consumed; an empty queue answers normally) and every call is
//! recorded so a scenario can assert exact request counts. Only synthetic
//! content passes through here.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use bytes::Bytes;
use tokio::sync::Notify;

/// How the next Gemini call is answered.
pub enum VertexStep {
    /// A valid four-field summary that finishes with `STOP`.
    Summary,
    /// This exact text as the candidate, with this `finishReason`.
    Candidate {
        text: String,
        finish_reason: &'static str,
    },
    /// A bare HTTP status with no body.
    Status(u16),
    /// Wait, then answer with a valid summary.
    Delay(Duration),
}

/// How the next Slack `chat.postMessage` is answered.
pub enum SlackStep {
    /// `{"ok":true,"channel":...,"ts":...}`.
    Ok,
    /// Record the call, signal `arrived`, wait for `release`, then answer ok.
    Hold {
        arrived: Arc<Notify>,
        release: Arc<Notify>,
    },
    /// Record the call, then cut the response off mid-body: Slack accepted the
    /// post but the caller never learns it.
    AcceptThenDrop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VertexCall {
    pub path: String,
    pub prompt: String,
    pub has_response_schema: bool,
    pub max_output_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlackCall {
    pub channel: String,
    pub text: String,
    pub thread_ts: Option<String>,
}

#[derive(Default)]
struct Inner {
    vertex_script: VecDeque<VertexStep>,
    vertex_calls: Vec<VertexCall>,
    slack_script: VecDeque<SlackStep>,
    slack_calls: Vec<SlackCall>,
    token_calls: usize,
}

/// The running fixture server. Dropping it stops the server.
pub struct Fixtures {
    inner: Arc<Mutex<Inner>>,
    addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Fixtures {
    /// Bind `0.0.0.0` on a free port so the cluster's pods can reach it.
    pub async fn start() -> Result<Self> {
        let inner = Arc::new(Mutex::new(Inner::default()));
        let app = Router::new()
            .fallback(handle)
            .with_state(Arc::clone(&inner));
        let listener = tokio::net::TcpListener::bind("0.0.0.0:0")
            .await
            .context("bind the fixture server")?;
        let addr = listener.local_addr()?;
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self { inner, addr, task })
    }

    #[must_use]
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub fn script_vertex(&self, steps: impl IntoIterator<Item = VertexStep>) {
        self.lock().vertex_script.extend(steps);
    }

    pub fn script_slack(&self, steps: impl IntoIterator<Item = SlackStep>) {
        self.lock().slack_script.extend(steps);
    }

    #[must_use]
    pub fn vertex_calls(&self) -> Vec<VertexCall> {
        self.lock().vertex_calls.clone()
    }

    #[must_use]
    pub fn slack_calls(&self) -> Vec<SlackCall> {
        self.lock().slack_calls.clone()
    }

    /// Slack calls that are summary posts, not thread replies.
    #[must_use]
    pub fn slack_posts(&self) -> Vec<SlackCall> {
        self.slack_calls()
            .into_iter()
            .filter(|call| call.thread_ts.is_none())
            .collect()
    }

    #[must_use]
    pub fn token_calls(&self) -> usize {
        self.lock().token_calls
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("fixture state")
    }
}

impl Drop for Fixtures {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The valid four-field summary text every default Gemini answer carries.
#[must_use]
pub fn valid_summary_text() -> String {
    serde_json::json!({
        "summary": "The sender asks for an update on the synthetic matter.",
        "requested_actions": ["Send an update"],
        "sender_stated_dates": [],
        "missing_information": []
    })
    .to_string()
}

fn gemini_body(text: &str, finish_reason: &str) -> serde_json::Value {
    serde_json::json!({
        "candidates": [{
            "content": { "role": "model", "parts": [{ "text": text }] },
            "finishReason": finish_reason
        }],
        "usageMetadata": { "promptTokenCount": 40, "candidatesTokenCount": 30 }
    })
}

async fn handle(
    State(inner): State<Arc<Mutex<Inner>>>,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> Response {
    let path = uri.path().to_string();
    if method == Method::GET && path == "/token" {
        inner.lock().expect("fixture state").token_calls += 1;
        return axum::Json(serde_json::json!({
            "access_token": "synthetic-token",
            "expires_in": 3600,
            "token_type": "Bearer"
        }))
        .into_response();
    }
    if method == Method::POST
        && path.starts_with("/v1/projects/")
        && path.ends_with(":generateContent")
    {
        return vertex(&inner, path, &body).await;
    }
    if method == Method::POST && path == "/slack/chat.postMessage" {
        return slack(&inner, &body).await;
    }
    StatusCode::NOT_FOUND.into_response()
}

async fn vertex(inner: &Arc<Mutex<Inner>>, path: String, body: &[u8]) -> Response {
    let json: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    let step = {
        let mut guard = inner.lock().expect("fixture state");
        guard.vertex_calls.push(VertexCall {
            path,
            prompt: json["contents"][0]["parts"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            has_response_schema: json["generationConfig"]["responseSchema"].is_object(),
            max_output_tokens: json["generationConfig"]["maxOutputTokens"].as_u64(),
        });
        guard
            .vertex_script
            .pop_front()
            .unwrap_or(VertexStep::Summary)
    };
    match step {
        VertexStep::Summary => {
            axum::Json(gemini_body(&valid_summary_text(), "STOP")).into_response()
        }
        VertexStep::Candidate {
            text,
            finish_reason,
        } => axum::Json(gemini_body(&text, finish_reason)).into_response(),
        VertexStep::Status(code) => StatusCode::from_u16(code)
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
            .into_response(),
        VertexStep::Delay(delay) => {
            tokio::time::sleep(delay).await;
            axum::Json(gemini_body(&valid_summary_text(), "STOP")).into_response()
        }
    }
}

async fn slack(inner: &Arc<Mutex<Inner>>, body: &[u8]) -> Response {
    let json: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    let (step, ts) = {
        let mut guard = inner.lock().expect("fixture state");
        guard.slack_calls.push(SlackCall {
            channel: json["channel"].as_str().unwrap_or_default().to_string(),
            text: json["text"].as_str().unwrap_or_default().to_string(),
            thread_ts: json["thread_ts"].as_str().map(ToOwned::to_owned),
        });
        let ts = format!("1700000000.{:06}", guard.slack_calls.len());
        (guard.slack_script.pop_front().unwrap_or(SlackStep::Ok), ts)
    };
    let channel = json["channel"].as_str().unwrap_or_default().to_string();
    let ok = || {
        axum::Json(serde_json::json!({ "ok": true, "channel": channel, "ts": ts })).into_response()
    };
    match step {
        SlackStep::Ok => ok(),
        SlackStep::Hold { arrived, release } => {
            arrived.notify_one();
            release.notified().await;
            ok()
        }
        SlackStep::AcceptThenDrop => {
            let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
                Ok(Bytes::from_static(b"{\"ok\":tr")),
                Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "synthetic dropped response",
                )),
            ];
            Response::new(Body::from_stream(futures::stream::iter(chunks)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(fixtures: &Fixtures, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", fixtures.port())
    }

    async fn gemini(fixtures: &Fixtures) -> reqwest::Response {
        reqwest::Client::new()
            .post(url(
                fixtures,
                "/v1/projects/p/locations/global/publishers/google/models/m:generateContent",
            ))
            .json(&serde_json::json!({
                "contents": [{ "role": "user", "parts": [{ "text": "synthetic prompt" }] }],
                "generationConfig": { "responseSchema": {}, "maxOutputTokens": 4096 }
            }))
            .send()
            .await
            .expect("gemini request")
    }

    async fn slack_post(
        fixtures: &Fixtures,
        thread: Option<&str>,
    ) -> reqwest::Result<reqwest::Response> {
        reqwest::Client::new()
            .post(url(fixtures, "/slack/chat.postMessage"))
            .json(&serde_json::json!({ "channel": "C1", "text": "hello", "thread_ts": thread }))
            .send()
            .await
    }

    #[tokio::test]
    async fn serves_a_token_and_a_valid_summary_and_records_the_call() {
        let fixtures = Fixtures::start().await.expect("start");
        let token: serde_json::Value = reqwest::get(url(&fixtures, "/token"))
            .await
            .expect("token")
            .json()
            .await
            .expect("json");
        assert_eq!(token["access_token"], "synthetic-token");

        let body: serde_json::Value = gemini(&fixtures).await.json().await.expect("json");
        assert_eq!(body["candidates"][0]["finishReason"], "STOP");
        let text = body["candidates"][0]["content"]["parts"][0]["text"]
            .as_str()
            .expect("text");
        assert!(workflows::parse_summary(text, 1_000).is_ok());

        assert_eq!(fixtures.token_calls(), 1);
        let calls = fixtures.vertex_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].prompt, "synthetic prompt");
        assert!(calls[0].has_response_schema);
        assert_eq!(calls[0].max_output_tokens, Some(4096));
    }

    #[tokio::test]
    async fn scripted_vertex_steps_are_consumed_in_order_then_default() {
        let fixtures = Fixtures::start().await.expect("start");
        fixtures.script_vertex([
            VertexStep::Status(429),
            VertexStep::Candidate {
                text: "{\"summary\": \"cut".to_string(),
                finish_reason: "MAX_TOKENS",
            },
        ]);
        assert_eq!(gemini(&fixtures).await.status(), 429);
        let truncated: serde_json::Value = gemini(&fixtures).await.json().await.expect("json");
        assert_eq!(truncated["candidates"][0]["finishReason"], "MAX_TOKENS");
        assert_eq!(gemini(&fixtures).await.status(), 200);
        assert_eq!(fixtures.vertex_calls().len(), 3);
    }

    #[tokio::test]
    async fn slack_posts_get_distinct_timestamps_and_threads_are_separated() {
        let fixtures = Fixtures::start().await.expect("start");
        let first: serde_json::Value = slack_post(&fixtures, None)
            .await
            .expect("post")
            .json()
            .await
            .expect("json");
        let second: serde_json::Value = slack_post(&fixtures, Some("1.2"))
            .await
            .expect("post")
            .json()
            .await
            .expect("json");
        assert_eq!(first["ok"], true);
        assert_ne!(first["ts"], second["ts"]);
        assert_eq!(fixtures.slack_calls().len(), 2);
        assert_eq!(fixtures.slack_posts().len(), 1);
    }

    #[tokio::test]
    async fn an_accepted_post_whose_response_is_dropped_is_still_recorded() {
        let fixtures = Fixtures::start().await.expect("start");
        fixtures.script_slack([SlackStep::AcceptThenDrop]);
        let result = match slack_post(&fixtures, None).await {
            Ok(response) => response.bytes().await.map(|_| ()),
            Err(error) => Err(error),
        };
        assert!(
            result.is_err(),
            "the caller must not receive a complete response"
        );
        assert_eq!(fixtures.slack_posts().len(), 1);
    }

    #[tokio::test]
    async fn a_held_post_is_released_on_signal() {
        let fixtures = Fixtures::start().await.expect("start");
        let arrived = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        fixtures.script_slack([SlackStep::Hold {
            arrived: Arc::clone(&arrived),
            release: Arc::clone(&release),
        }]);
        let port = fixtures.port();
        let pending = tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("http://127.0.0.1:{port}/slack/chat.postMessage"))
                .json(&serde_json::json!({ "channel": "C1", "text": "hello" }))
                .send()
                .await
        });
        arrived.notified().await;
        assert!(!pending.is_finished());
        release.notify_one();
        let response = pending.await.expect("join").expect("response");
        assert_eq!(response.status(), 200);
    }
}
