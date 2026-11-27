//! What each scheduled `CronJob` binary submits, as data for
//! [`workflows::trigger::run_cron_trigger`]. The clock is an argument so a
//! key's stability across a pod retry is a tested decision.

use chrono::{DateTime, TimeDelta, Timelike, Utc};
use serde_json::json;
use workflows::trigger::{restate_run, CronTrigger};

/// A workflow the DBOS engine fires from its own schedule, keyed on the UTC
/// date: Restate admits one invocation per key, so a same-day re-fire is a
/// no-op.
fn daily(flag: &'static str, service: &'static str, run_id: String) -> CronTrigger {
    CronTrigger {
        flag,
        service,
        dbos: None,
        restate: restate_run(run_id, json!({})),
    }
}

/// The nightly `DriDigest` run.
#[must_use]
pub fn dri_digest(now: DateTime<Utc>) -> CronTrigger {
    daily(
        "NAVIGATOR_DBOS_DRI_DIGEST_ENABLED",
        "DriDigest",
        now.format("%Y-%m-%d").to_string(),
    )
}

/// The daily `GeneralNag` run.
#[must_use]
pub fn general_nag(now: DateTime<Utc>) -> CronTrigger {
    daily(
        "NAVIGATOR_DBOS_GENERAL_NAG_ENABLED",
        "GeneralNag",
        now.format("%Y-%m-%d").to_string(),
    )
}

/// The nightly `NotationReset` run.
#[must_use]
pub fn notation_reset(now: DateTime<Utc>) -> CronTrigger {
    daily(
        "NAVIGATOR_DBOS_NOTATION_RESET_ENABLED",
        "NotationReset",
        crate::notation_reset::run_key(now),
    )
}

/// Nightly Project Update reuses its workflow ID throughout each UTC day.
#[must_use]
pub fn nightly_project_update(now: DateTime<Utc>) -> serde_json::Value {
    let run_id = crate::nightly_project_update::run_key(now);
    json!({ "workflow_id": crate::dbos_nightly_project_update::workflow_id(&run_id) })
}

/// The social publication reconciler. The DBOS identity is slot-floored: the
/// worker's internal schedule already runs every five-minute slot, so the
/// submit joins that run instead of starting a second reconciler beside it.
#[must_use]
pub fn social_publish_reconciler(now: DateTime<Utc>) -> CronTrigger {
    let slot = crate::dbos_social_publisher::reconcile_slot(now);
    let tick = now.format("%Y-%m-%d-%H-%M").to_string();
    CronTrigger {
        flag: workflows::trigger::SOCIAL_PUBLISH_RECONCILER_ENABLED_ENV,
        service: "SocialPublishReconciler",
        dbos: Some((
            "/internal/dbos/social-publish-reconciler",
            json!({
                "workflow_id": workflows::trigger::social_publish_reconciler_workflow_id(slot),
                "request": { "tick": slot.to_rfc3339() },
            }),
        )),
        restate: restate_run(tick.clone(), json!({ "tick": tick })),
    }
}

/// The scheduled video-comments poll. The `CronJob` runs every 15 minutes; a pod
/// restart in its retry window must join the same DBOS run even if the UTC
/// minute has advanced, so the DBOS tick is floored to the quarter hour.
#[must_use]
pub fn youtube_comments_poll(now: DateTime<Utc>) -> serde_json::Value {
    let slot = now - TimeDelta::minutes(i64::from(now.minute() % 15));
    json!({ "tick": slot.format("%Y-%m-%d-%H-%M").to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn restate(trigger: CronTrigger) -> (String, serde_json::Value) {
        (trigger.restate)(&|_| None).unwrap()
    }

    #[test]
    fn engine_scheduled_workflows_key_restate_on_the_utc_date_and_name_no_dbos_route() {
        let late = Utc.with_ymd_and_hms(2026, 11, 8, 23, 59, 59).unwrap();
        for (trigger, service) in [
            (dri_digest(late), "DriDigest"),
            (general_nag(late), "GeneralNag"),
            (notation_reset(late), "NotationReset"),
        ] {
            assert_eq!(trigger.service, service);
            assert!(trigger.dbos.is_none(), "{service}");
            assert_eq!(restate(trigger), ("2026-11-08".to_owned(), json!({})));
        }
    }

    #[test]
    fn the_nightly_project_update_names_one_run_per_utc_date() {
        let first = Utc.with_ymd_and_hms(2026, 11, 8, 3, 33, 0).unwrap();
        let retry = first + TimeDelta::minutes(7);
        let body = nightly_project_update(first);
        assert_eq!(body, nightly_project_update(retry));
        assert!(body["workflow_id"]
            .as_str()
            .unwrap()
            .ends_with("2026-11-08"));
    }

    #[test]
    fn the_reconciler_joins_the_worker_slot_on_dbos_and_keys_restate_by_minute() {
        let at = Utc.with_ymd_and_hms(2026, 11, 1, 21, 23, 40).unwrap();
        let (route, body) = social_publish_reconciler(at).dbos.unwrap();
        assert_eq!(route, "/internal/dbos/social-publish-reconciler");
        assert_eq!(body["request"]["tick"], "2026-11-01T21:20:00+00:00");
        assert_eq!(
            body,
            social_publish_reconciler(at + TimeDelta::seconds(60))
                .dbos
                .unwrap()
                .1
        );
        let (key, request) = restate(social_publish_reconciler(at));
        assert_eq!(key, "2026-11-01-21-23");
        let request: crate::social_publisher::ReconcileRequest =
            serde_json::from_value(request).unwrap();
        assert_eq!(request.tick, key);
    }

    #[test]
    fn the_youtube_dbos_tick_is_stable_across_cronjob_retries() {
        let first = Utc.with_ymd_and_hms(2026, 11, 8, 23, 45, 20).unwrap();
        let retry = Utc.with_ymd_and_hms(2026, 11, 8, 23, 48, 20).unwrap();
        let next = Utc.with_ymd_and_hms(2026, 11, 9, 0, 0, 20).unwrap();
        assert_eq!(
            youtube_comments_poll(first),
            json!({"tick": "2026-11-08-23-45"})
        );
        assert_eq!(youtube_comments_poll(retry), youtube_comments_poll(first));
        assert_eq!(
            youtube_comments_poll(next),
            json!({"tick": "2026-11-09-00-00"})
        );
    }
}
