//! The `Heartbeat` workflow's data: the report its `beat` step checkpoints and
//! the one-line ops notice its `notify` step posts. The workflow itself is
//! `dbos_engine::heartbeat`, behind the `dbos` feature.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The checkpointed result of the `beat` step, carried into the Slack notice.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct HeartbeatReport {
    /// The DBOS workflow id of this run, `Heartbeat-<UTC date>T<hour>`: the
    /// operator's handle into the `dbos.workflow_status` row.
    pub invocation_id: String,
    /// Wall-clock instant the `beat` step executed, captured inside the
    /// checkpointed step so a replay re-uses it rather than re-reading the
    /// clock.
    pub beat_at: DateTime<Utc>,
}

/// The one-line Slack notice for a completed heartbeat: a heart glyph, the
/// liveness assertion, and the checkpointed beat instant. The assertion is
/// engine liveness only ("Durable execution OK") — it never claims any backup
/// or data write succeeded.
#[must_use]
pub fn heartbeat_message(report: &HeartbeatReport) -> String {
    let beat = report.beat_at.format("%Y-%m-%d %H:%M UTC");
    format!("💓 Durable execution OK — heartbeat {beat}")
}

#[cfg(test)]
mod tests {
    use super::{heartbeat_message, HeartbeatReport};
    use chrono::{TimeZone, Utc};

    fn sample_report() -> HeartbeatReport {
        HeartbeatReport {
            invocation_id: "inv_abc123".into(),
            beat_at: Utc.with_ymd_and_hms(2026, 6, 12, 18, 0, 0).unwrap(),
        }
    }

    #[test]
    fn heartbeat_message_is_one_line_with_heart_and_liveness() {
        let msg = heartbeat_message(&sample_report());
        assert_eq!(
            msg,
            "💓 Durable execution OK — heartbeat 2026-06-12 18:00 UTC"
        );
        // One line: no embedded newline a chat client would split on.
        assert!(
            !msg.contains('\n'),
            "heartbeat notice must be a single line: {msg}"
        );
        // It carries the heart glyph and asserts only engine liveness.
        assert!(msg.starts_with('💓'));
        assert!(msg.contains("Durable execution OK"));
    }
}
