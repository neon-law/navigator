//! The e2e matrix. Each scenario sends real signed webhook requests through the
//! host `web` listener to the real broker and branch worker, then asserts on
//! durable records and the exact provider and Slack call counts the fixtures
//! saw. Counts are deltas against a baseline taken at the start of the
//! scenario, so scenarios do not depend on each other's traffic.

use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{ensure, Result};
use tokio::sync::Notify;

use super::fixtures::{SlackStep, VertexStep};
use super::harness::{Fault, Harness, Post, RawEmail, CHANNEL, DEPLOYMENT, MAILBOX};

const SETTLE: Duration = Duration::from_secs(90);
/// Time for a duplicate or replayed request to have done any damage.
const QUIET: Duration = Duration::from_secs(4);

/// Per-scenario assertion tally, rendered as the run's evidence.
#[derive(Default)]
pub struct Evidence {
    done: Vec<(String, usize)>,
    current: usize,
}

impl Evidence {
    fn check(&mut self, ok: bool, what: &str) -> Result<()> {
        ensure!(ok, "assertion failed: {what}");
        self.current += 1;
        Ok(())
    }

    #[allow(clippy::needless_pass_by_value)] // call sites pass temporaries
    fn eq<T: PartialEq + Debug>(&mut self, actual: T, expected: T, what: &str) -> Result<()> {
        ensure!(
            actual == expected,
            "assertion failed: {what}: expected {expected:?}, got {actual:?}"
        );
        self.current += 1;
        Ok(())
    }

    fn finish(&mut self, name: &str) {
        println!("PASS {name} ({} assertions)", self.current);
        self.done.push((name.to_string(), self.current));
        self.current = 0;
    }

    #[must_use]
    pub fn render(&self) -> String {
        let total: usize = self.done.iter().map(|(_, count)| count).sum();
        let mut lines = vec!["email-summary-e2e: all scenarios passed".to_string()];
        lines.extend(
            self.done
                .iter()
                .map(|(name, count)| format!("  {name}: {count} assertions")),
        );
        lines.push(format!(
            "  {} scenarios, {total} assertions",
            self.done.len()
        ));
        lines.join("\n")
    }
}

/// Fixture and store counters at one instant.
struct Baseline {
    gemini: usize,
    posts: usize,
    replies: usize,
    receipts: i64,
    letters: i64,
}

impl Baseline {
    async fn take(h: &Harness) -> Result<Self> {
        Ok(Self {
            gemini: h.fx.vertex_calls().len(),
            posts: h.fx.slack_posts().len(),
            replies: h.fx.slack_calls().len() - h.fx.slack_posts().len(),
            receipts: h.receipt_count().await?,
            letters: h.letter_count().await?,
        })
    }

    fn gemini(&self, h: &Harness) -> usize {
        h.fx.vertex_calls().len() - self.gemini
    }

    fn posts(&self, h: &Harness) -> usize {
        h.fx.slack_posts().len() - self.posts
    }

    fn replies(&self, h: &Harness) -> usize {
        (h.fx.slack_calls().len() - h.fx.slack_posts().len()) - self.replies
    }

    async fn receipts(&self, h: &Harness) -> Result<i64> {
        Ok(h.receipt_count().await? - self.receipts)
    }

    async fn letters(&self, h: &Harness) -> Result<i64> {
        Ok(h.letter_count().await? - self.letters)
    }
}

pub async fn run_all(h: &mut Harness) -> Result<Evidence> {
    let mut ev = Evidence::default();
    let confirmed = happy_path(h, &mut ev).await?;
    concurrent_redelivery(h, &mut ev).await?;
    rejected_requests(h, &mut ev).await?;
    restate_unavailable(h, &mut ev).await?;
    provider_retries_and_failures(h, &mut ev).await?;
    slack_drops_the_response(h, &mut ev).await?;
    worker_killed_mid_delivery(h, &mut ev).await?;
    worker_restart_after_confirmed_delivery(h, &mut ev, &confirmed).await?;
    non_support_mail(h, &mut ev).await?;
    Ok(ev)
}

/// A message that has been delivered once, for the restart scenario to replay.
struct Confirmed {
    email: RawEmail,
    receipt_id: uuid::Uuid,
}

async fn happy_path(h: &Harness, ev: &mut Evidence) -> Result<Confirmed> {
    let before = Baseline::take(h).await?;
    let email = h.new_email(MAILBOX);
    ev.eq(
        h.post(&email, &Post::default()).await?,
        202,
        "the webhook admits the message",
    )?;
    let receipt = h.wait_receipt(&email).await?;
    let delivery = h.wait_delivery(receipt.id, "confirmed", SETTLE).await?;
    h.wait_invocation_completed(receipt.id, SETTLE).await?;

    ev.eq(before.gemini(h), 1, "Gemini receives exactly one request")?;
    ev.check(
        h.fx.token_calls() >= 1,
        "the worker authenticated through the metadata fixture",
    )?;
    let call = h.fx.vertex_calls().pop().expect("a Gemini call");
    ev.check(
        call.prompt.contains(&email.body),
        "the model input is the bounded message body",
    )?;
    ev.check(
        call.has_response_schema,
        "the request carries the response schema",
    )?;
    ev.eq(
        call.max_output_tokens,
        Some(u64::from(workflows::DEFAULT_MAX_OUTPUT_TOKENS)),
        "the output budget is the default",
    )?;
    ev.eq(before.posts(h), 1, "exactly one summary reaches Slack")?;
    let post = h.fx.slack_posts().pop().expect("a Slack post");
    ev.eq(
        post.channel.as_str(),
        CHANNEL,
        "the post targets the configured channel",
    )?;
    ev.check(
        post.text
            .contains(&format!("Support summary receipt: {}", receipt.id))
            && post.text.contains("*Gemini*")
            && post.text.contains("Summary:"),
        "the post is the labeled Gemini summary",
    )?;
    ev.eq(before.replies(h), 1, "the full body is threaded once")?;
    ev.check(
        delivery
            .slack_timestamp
            .as_deref()
            .is_some_and(|ts| ts.starts_with("1700000000.")),
        "the Slack channel and timestamp are persisted",
    )?;
    ev.eq(
        delivery.channel_id.as_str(),
        CHANNEL,
        "the persisted channel",
    )?;
    ev.eq(before.receipts(h).await?, 1, "one receipt")?;
    ev.eq(before.letters(h).await?, 1, "one letter")?;
    let archive = h.storage.get(&receipt.archive_key).await?;
    ev.check(
        archive.bytes == email.bytes,
        "the archive is byte-identical to the message",
    )?;
    ev.eq(
        h.invocations(receipt.id).await?,
        vec!["completed".to_string()],
        "one completed invocation",
    )?;
    ev.finish("happy path");
    Ok(Confirmed {
        email,
        receipt_id: receipt.id,
    })
}

async fn concurrent_redelivery(h: &Harness, ev: &mut Evidence) -> Result<()> {
    let before = Baseline::take(h).await?;
    let email = h.new_email(MAILBOX);
    let boundaries = ["----e2e-a", "----e2e-b", "----e2e-c", "----e2e-d"];
    let posts = boundaries.iter().map(|boundary| async {
        h.post(
            &email,
            &Post {
                boundary: Some(boundary),
                ..Post::default()
            },
        )
        .await
    });
    let statuses = futures::future::join_all(posts).await;
    for status in statuses {
        ev.eq(status?, 202, "every redelivery is admitted")?;
    }
    let receipt = h.wait_receipt(&email).await?;
    h.wait_delivery(receipt.id, "confirmed", SETTLE).await?;
    h.wait_invocation_completed(receipt.id, SETTLE).await?;
    tokio::time::sleep(QUIET).await;

    ev.eq(
        before.receipts(h).await?,
        1,
        "one receipt for four deliveries",
    )?;
    ev.eq(before.letters(h).await?, 1, "one letter")?;
    ev.eq(h.invocations(receipt.id).await?.len(), 1, "one invocation")?;
    ev.eq(before.gemini(h), 1, "one model request")?;
    ev.eq(before.posts(h), 1, "one confirmed Slack message")?;
    ev.finish("concurrent redelivery");
    Ok(())
}

async fn rejected_requests(h: &Harness, ev: &mut Evidence) -> Result<()> {
    let before = Baseline::take(h).await?;
    let email = h.new_email(MAILBOX);
    ev.eq(
        h.post(
            &email,
            &Post {
                fault: Fault::Tampered,
                ..Post::default()
            },
        )
        .await?,
        401,
        "a tampered body is refused",
    )?;
    ev.eq(
        h.post(
            &email,
            &Post {
                fault: Fault::NoSignature,
                ..Post::default()
            },
        )
        .await?,
        401,
        "a missing signature is refused",
    )?;
    ev.eq(
        h.post(
            &email,
            &Post {
                fault: Fault::WrongPathSecret,
                ..Post::default()
            },
        )
        .await?,
        401,
        "a wrong path secret is refused",
    )?;
    ev.eq(
        h.post(
            &email,
            &Post {
                fault: Fault::InvalidEnvelope,
                ..Post::default()
            },
        )
        .await?,
        400,
        "an invalid envelope is refused",
    )?;

    h.clam_proxy.set_enabled(false);
    let outage = h
        .post(
            &email,
            &Post {
                attachment: true,
                ..Post::default()
            },
        )
        .await;
    h.clam_proxy.set_enabled(true);
    ev.eq(
        outage?,
        503,
        "a scanner outage is an explicit retriable error",
    )?;

    tokio::time::sleep(QUIET).await;
    ev.check(
        h.receipt(&email).await?.is_none(),
        "no receipt was admitted",
    )?;
    ev.eq(before.receipts(h).await?, 0, "no receipt rows")?;
    ev.eq(before.gemini(h), 0, "no model work")?;
    ev.eq(before.posts(h) + before.replies(h), 0, "no Slack work")?;
    ev.finish("rejected requests and scanner outage");
    Ok(())
}

async fn restate_unavailable(h: &Harness, ev: &mut Evidence) -> Result<()> {
    let before = Baseline::take(h).await?;
    let email = h.new_email(MAILBOX);
    h.restate_proxy.set_enabled(false);
    let down = h.post(&email, &Post::default()).await;
    h.restate_proxy.set_enabled(true);
    ev.eq(down?, 503, "an unavailable broker is a retriable response")?;
    let receipt = h.wait_receipt(&email).await?;
    ev.eq(
        receipt.processing_state.as_str(),
        "archived",
        "the receipt is retained, archived",
    )?;
    ev.check(
        h.delivery(receipt.id).await?.is_none(),
        "nothing was delivered",
    )?;
    ev.eq(before.gemini(h), 0, "no model work before admission")?;
    let archive_before = h.storage.get(&receipt.archive_key).await?;

    ev.eq(
        h.post(
            &email,
            &Post {
                boundary: Some("----e2e-replay"),
                ..Post::default()
            },
        )
        .await?,
        202,
        "the replay is admitted",
    )?;
    h.wait_delivery(receipt.id, "confirmed", SETTLE).await?;
    h.wait_invocation_completed(receipt.id, SETTLE).await?;
    ev.eq(
        before.receipts(h).await?,
        1,
        "the replay reuses the receipt",
    )?;
    ev.eq(before.letters(h).await?, 1, "the replay reuses the letter")?;
    let archive_after = h.storage.get(&receipt.archive_key).await?;
    ev.check(
        archive_before.bytes == archive_after.bytes && archive_after.bytes == email.bytes,
        "the archive is unchanged and byte-identical",
    )?;
    ev.eq(before.gemini(h), 1, "one model request after recovery")?;
    ev.eq(before.posts(h), 1, "one Slack message after recovery")?;
    ev.finish("restate unavailable before admission, then replay");
    Ok(())
}

/// One delivered message whose model answer is scripted; returns the Slack
/// post text and the model request count.
async fn scripted_run(h: &Harness, steps: Vec<VertexStep>) -> Result<(String, usize)> {
    let before = Baseline::take(h).await?;
    h.fx.script_vertex(steps);
    let email = h.new_email(MAILBOX);
    ensure!(
        h.post(&email, &Post::default()).await? == 202,
        "the webhook admits the message"
    );
    let receipt = h.wait_receipt(&email).await?;
    h.wait_delivery(receipt.id, "confirmed", Duration::from_secs(240))
        .await?;
    h.wait_invocation_completed(receipt.id, SETTLE).await?;
    let text =
        h.fx.slack_posts()
            .pop()
            .map(|post| post.text)
            .unwrap_or_default();
    Ok((text, before.gemini(h)))
}

async fn provider_retries_and_failures(h: &Harness, ev: &mut Evidence) -> Result<()> {
    let (text, calls) =
        scripted_run(h, vec![VertexStep::Status(429), VertexStep::Status(503)]).await?;
    ev.eq(calls, 3, "429 and 5xx are retried within the bound")?;
    ev.check(
        text.contains("Summary:"),
        "the third attempt recovers and the summary is posted",
    )?;

    let (text, calls) = scripted_run(
        h,
        vec![
            VertexStep::Status(500),
            VertexStep::Status(500),
            VertexStep::Status(500),
        ],
    )
    .await?;
    ev.eq(calls, 3, "retries stop at the bound")?;
    ev.check(
        text.contains("Failure: provider_error:"),
        "exhausted retries post a clear failure notice",
    )?;

    let (text, calls) = scripted_run(h, vec![VertexStep::Status(400)]).await?;
    ev.eq(calls, 1, "a permanent failure is not retried")?;
    ev.check(
        text.contains("Failure: provider_error:"),
        "the permanent failure is reported",
    )?;

    let (text, calls) = scripted_run(
        h,
        vec![VertexStep::Candidate {
            text: "{}".into(),
            finish_reason: "STOP",
        }],
    )
    .await?;
    ev.eq(calls, 1, "invalid output is not retried")?;
    ev.check(
        text.contains("Failure: invalid_output:") && text.contains("finish_reason=STOP"),
        "invalid output names the finish reason",
    )?;

    let (text, calls) = scripted_run(
        h,
        vec![VertexStep::Candidate {
            text: "{\"summary\": \"cut o".into(),
            finish_reason: "MAX_TOKENS",
        }],
    )
    .await?;
    ev.eq(calls, 1, "truncated output is not retried")?;
    ev.check(
        text.contains("Failure: output_truncated:finish_reason=MAX_TOKENS"),
        "truncation is named explicitly",
    )?;

    let timeout = Duration::from_secs(35);
    let (text, calls) = scripted_run(
        h,
        vec![
            VertexStep::Delay(timeout),
            VertexStep::Delay(timeout),
            VertexStep::Delay(timeout),
        ],
    )
    .await?;
    ev.eq(calls, 3, "a provider timeout is retried within the bound")?;
    ev.check(
        text.contains("Failure: provider_error:"),
        "repeated timeouts post a failure notice",
    )?;
    ev.finish("provider 429/5xx, permanent, invalid, truncated, and timeout outcomes");
    Ok(())
}

async fn slack_drops_the_response(h: &Harness, ev: &mut Evidence) -> Result<()> {
    let before = Baseline::take(h).await?;
    h.fx.script_slack([SlackStep::AcceptThenDrop]);
    let email = h.new_email(MAILBOX);
    ev.eq(
        h.post(&email, &Post::default()).await?,
        202,
        "the webhook admits the message",
    )?;
    let receipt = h.wait_receipt(&email).await?;
    let delivery = h.wait_delivery(receipt.id, "unknown", SETTLE).await?;
    h.wait_invocation_completed(receipt.id, SETTLE).await?;
    tokio::time::sleep(QUIET).await;
    ev.eq(
        before.posts(h),
        1,
        "Slack saw one post and automation did not resend",
    )?;
    ev.eq(before.gemini(h), 1, "one model request")?;
    ev.check(
        delivery.slack_timestamp.is_none(),
        "no timestamp is claimed for an unconfirmed post",
    )?;

    let ts = format!("1700000000.{:06}", h.fx.slack_calls().len());
    store::email_deliveries::reconcile_confirmed(&h.db, receipt.id, "e2e-operator", CHANNEL, &ts)
        .await?;
    let reconciled = h.delivery(receipt.id).await?.expect("delivery");
    ev.eq(
        reconciled.state.as_str(),
        "confirmed",
        "explicit reconciliation completes the delivery",
    )?;
    ev.eq(
        reconciled.reconciled_by.as_deref(),
        Some("e2e-operator"),
        "the operator is recorded",
    )?;
    ev.eq(before.posts(h), 1, "reconciliation posts nothing")?;
    ev.finish("slack accepts then drops the response");
    Ok(())
}

async fn worker_killed_mid_delivery(h: &Harness, ev: &mut Evidence) -> Result<()> {
    let before = Baseline::take(h).await?;
    let arrived = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    h.fx.script_slack([SlackStep::Hold {
        arrived: Arc::clone(&arrived),
        release: Arc::clone(&release),
    }]);
    let email = h.new_email(MAILBOX);
    ev.eq(
        h.post(&email, &Post::default()).await?,
        202,
        "the webhook admits the message",
    )?;
    tokio::time::timeout(SETTLE, arrived.notified()).await?;
    ev.eq(
        before.gemini(h),
        1,
        "the model result exists before the kill",
    )?;

    h.kill_worker().await?;
    release.notify_one();
    let receipt = h.wait_receipt(&email).await?;
    h.wait_invocation_completed(receipt.id, Duration::from_secs(240))
        .await?;
    tokio::time::sleep(QUIET).await;
    ev.eq(
        before.gemini(h),
        1,
        "the journaled model step is not invoked again",
    )?;
    ev.eq(
        before.posts(h),
        1,
        "the interrupted post is not blindly resent",
    )?;
    let delivery = h.delivery(receipt.id).await?.expect("delivery");
    ev.eq(
        delivery.state.as_str(),
        "unknown",
        "the interrupted delivery is held for reconciliation",
    )?;
    ev.eq(
        h.invocations(receipt.id).await?,
        vec!["completed".to_string()],
        "the real broker resumed and completed one invocation",
    )?;

    let ts = format!("1700000000.{:06}", h.fx.slack_calls().len());
    store::email_deliveries::reconcile_confirmed(&h.db, receipt.id, "e2e-operator", CHANNEL, &ts)
        .await?;
    ev.eq(
        h.delivery(receipt.id)
            .await?
            .expect("delivery")
            .state
            .as_str(),
        "confirmed",
        "reconciliation completes it",
    )?;
    ev.finish("worker killed after the model result is journaled");
    Ok(())
}

async fn worker_restart_after_confirmed_delivery(
    h: &Harness,
    ev: &mut Evidence,
    confirmed: &Confirmed,
) -> Result<()> {
    let before = Baseline::take(h).await?;
    h.kill_worker().await?;
    ev.eq(
        h.post(
            &confirmed.email,
            &Post {
                boundary: Some("----e2e-after-restart"),
                ..Post::default()
            },
        )
        .await?,
        202,
        "the redelivery after a restart is admitted",
    )?;
    tokio::time::sleep(QUIET).await;
    ev.eq(
        before.posts(h) + before.replies(h),
        0,
        "the replayed success does not repost",
    )?;
    ev.eq(
        before.gemini(h),
        0,
        "the replayed success does not call the model",
    )?;
    ev.eq(
        before.receipts(h).await?,
        0,
        "the replay reuses the receipt",
    )?;
    ev.eq(before.letters(h).await?, 0, "the replay reuses the letter")?;
    ev.eq(
        h.invocations(confirmed.receipt_id).await?.len(),
        1,
        "still one invocation",
    )?;
    ev.eq(
        h.delivery(confirmed.receipt_id)
            .await?
            .expect("delivery")
            .state
            .as_str(),
        "confirmed",
        "the delivery stays confirmed",
    )?;
    ev.finish("worker restart after confirmed delivery");
    Ok(())
}

async fn non_support_mail(h: &Harness, ev: &mut Evidence) -> Result<()> {
    let before = Baseline::take(h).await?;
    let email = h.new_email("someone-else@e2e.example.test");
    let status = h
        .post(
            &email,
            &Post {
                envelope_to: Some("someone-else@e2e.example.test"),
                ..Post::default()
            },
        )
        .await?;
    ev.eq(
        status,
        200,
        "mail outside the support mailbox takes the existing inbound path",
    )?;
    tokio::time::sleep(QUIET).await;
    ev.eq(before.receipts(h).await?, 0, "no summary receipt")?;
    ev.eq(before.gemini(h), 0, "no model work")?;
    ev.eq(before.posts(h) + before.replies(h), 0, "no Slack work")?;
    let digest = workflows::scoped_email_digest(DEPLOYMENT, MAILBOX, &email.bytes);
    ev.check(
        store::email_receipts::find(&h.db, MAILBOX, DEPLOYMENT, &digest)
            .await?
            .is_none(),
        "nothing is keyed in the summary lane",
    )?;
    ev.finish("non-support mail keeps the existing inbound path");
    Ok(())
}
