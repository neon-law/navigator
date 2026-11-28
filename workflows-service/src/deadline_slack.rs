//! Deliver Deadline reminders against the current docket, using the owning Firm's Slack bot.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write,
    sync::Arc,
    time::Duration,
};

use chrono::{DateTime, Datelike, Days, FixedOffset, NaiveDate, NaiveTime, TimeZone, Utc, Weekday};
use dbos::{StepOptions, WorkflowRef, DBOS};
use serde::{Deserialize, Serialize};
use store::application_store::ApplicationStore;
use store::persons::{self, Role};
use store::project_deadlines::{self, Deadline, STATUS_OPEN};
use store::projects::{self, Project};
use uuid::Uuid;
use workflows::dbos_engine::ScheduleStarter;

use crate::firm_slack::{post_once, ProjectSlackBots};
use crate::nightly_project_update::slack::{matter_link, slack_plain_text};
use crate::project_slack::LaneError;

const INTERVAL_SECONDS: i64 = 15 * 60;

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
enum ReminderError {
    #[error("Slack Deadline reminder could not be delivered")]
    Delivery,
    #[error("Slack Deadline reminder roster could not be read")]
    Roster,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Stage {
    Lead,
    BusinessDayBefore,
    DueToday,
    Overdue,
}

impl Stage {
    fn label(self) -> &'static str {
        match self {
            Self::Lead => "Deadline reminder",
            Self::BusinessDayBefore => "Upcoming deadline",
            Self::DueToday => "Due today",
            Self::Overdue => "Overdue",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Reminder {
    project_id: Uuid,
    deadline_id: Uuid,
    revision: Option<i64>,
    stage: Stage,
}

impl Reminder {
    fn key(&self) -> String {
        format!(
            "DeadlineSlackReminder:{}:{}:{:?}",
            self.deadline_id,
            self.revision.unwrap_or_default(),
            self.stage
        )
    }
}

#[derive(Clone)]
struct Delivery {
    store: ApplicationStore,
    bots: Arc<dyn ProjectSlackBots>,
    portal_origin: Option<String>,
}

pub(crate) fn register(
    dbos: &DBOS,
    store: ApplicationStore,
    bots: Arc<dyn ProjectSlackBots>,
    portal_origin: Option<String>,
) -> dbos::Result<ScheduleStarter> {
    let delivery = Delivery {
        store: store.clone(),
        bots,
        portal_origin,
    };
    let reminder = dbos.register_workflow("DeadlineSlackReminder", move |request: Reminder| {
        let delivery = delivery.clone();
        async move {
            dbos::step_with(
                "deliver-deadline-reminder",
                StepOptions {
                    max_attempts: 3,
                    ..StepOptions::default()
                },
                move || {
                    let delivery = delivery.clone();
                    let request = request.clone();
                    async move {
                        delivery
                            .send(&request)
                            .await
                            .map_err(dbos::Error::Application)
                    }
                },
            )
            .await
        }
    })?;
    let sweep = dbos.register_workflow("DeadlineSlackSweep", move |slot: DateTime<Utc>| {
        let store = store.clone();
        let reminder = reminder.clone();
        async move {
            let candidates = dbos::step("select-deadline-reminders", move || {
                let store = store.clone();
                async move {
                    candidates(&store, slot)
                        .await
                        .map_err(dbos::Error::Application)
                }
            })
            .await?;
            for candidate in candidates {
                let id = candidate.key();
                if reminder
                    .run_with(
                        candidate,
                        dbos::RunOptions {
                            workflow_id: Some(&id),
                            ..Default::default()
                        },
                    )
                    .await
                    .is_err()
                {
                    tracing::warn!(workflow_id = %id, "Slack Deadline reminder failed");
                }
            }
            dbos::Result::<_, ReminderError>::Ok(())
        }
    })?;
    Ok(Box::new(move |stop| tokio::spawn(schedule(sweep, stop))))
}

async fn schedule(
    sweep: WorkflowRef<DateTime<Utc>, (), ReminderError>,
    stop: dbos::CancellationToken,
) {
    let mut ticks = tokio::time::interval(Duration::from_mins(15));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
            _ = ticks.tick() => {}
        }
        let now = Utc::now();
        let timestamp = now.timestamp().div_euclid(INTERVAL_SECONDS) * INTERVAL_SECONDS;
        let Some(slot) = DateTime::from_timestamp(timestamp, 0) else {
            continue;
        };
        let id = format!("DeadlineSlackSweep:{timestamp}");
        if let Err(error) = sweep
            .start_with(
                slot,
                dbos::StartOptions {
                    workflow_id: Some(&id),
                    ..Default::default()
                },
            )
            .await
        {
            tracing::warn!(workflow_id = %id, %error, "Slack Deadline sweep could not start");
        }
    }
}

fn internal_channel(project: &Project) -> Option<&str> {
    (project.status == "open" && project.firm_id.is_some())
        .then_some(project.internal_slack_channel_id.as_deref())
        .flatten()
        .filter(|id| !id.is_empty())
}

async fn candidates(
    store: &ApplicationStore,
    slot: DateTime<Utc>,
) -> Result<Vec<Reminder>, ReminderError> {
    let projects: BTreeMap<_, _> = projects::all(store)
        .await
        .map_err(|_| ReminderError::Roster)?
        .into_iter()
        .filter(|project| internal_channel(project).is_some())
        .map(|project| (project.id, project))
        .collect();
    let ids: Vec<_> = projects.keys().copied().collect();
    let deadlines = project_deadlines::open_for_projects(&ids)
        .await
        .map_err(|_| ReminderError::Roster)?;
    let mut zones = BTreeMap::new();
    let mut reminders = Vec::new();
    for deadline in deadlines {
        let Some(project) = projects.get(&deadline.project_id) else {
            continue;
        };
        let timezone = match &deadline.due_on {
            Some(on) => Some(on.timezone),
            None => match zones.entry(project.id) {
                std::collections::btree_map::Entry::Occupied(entry) => *entry.get(),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    let zone = projects::calendar_timezone(store, project).await;
                    if zone.is_err() {
                        tracing::warn!(project_id = %project.id, "Could not read the Project time zone for Slack Deadline reminders");
                    }
                    *entry.insert(zone.ok())
                }
            },
        };
        if let Some(stage) = timezone.and_then(|zone| stage(&deadline, slot, &zone)) {
            reminders.push(Reminder {
                project_id: deadline.project_id,
                deadline_id: deadline.id,
                revision: deadline.calendar_sequence,
                stage,
            });
        }
    }
    Ok(reminders)
}

fn stage(deadline: &Deadline, now: DateTime<Utc>, timezone: &impl TimeZone) -> Option<Stage> {
    if deadline.status != STATUS_OPEN || deadline.calendar_review_pending {
        return None;
    }
    let due = due_instant(deadline)?.with_timezone(timezone);
    if now >= due {
        // Escalate shortly after the cutoff, without flooding channels with old docket entries.
        return (now.signed_duration_since(due) < chrono::Duration::days(1))
            .then_some(Stage::Overdue);
    }
    let local = now.with_timezone(timezone);
    let (today, local_time) = (local.date_naive(), local.time());
    let due_date = deadline
        .due_on
        .as_ref()
        .map_or_else(|| due.date_naive(), |on| on.date);
    let morning = NaiveTime::from_hms_opt(8, 0, 0)?;
    if today == due_date {
        // Move the due-day reminder earlier for early-morning cutoffs.
        let before_cutoff = if deadline
            .due_on
            .as_ref()
            .is_some_and(|on| on.cutoff.is_none())
        {
            morning
        } else {
            let one_hour_earlier = due.checked_sub_signed(chrono::Duration::hours(1))?;
            if one_hour_earlier.date_naive() < due_date {
                NaiveTime::MIN
            } else {
                one_hour_earlier.time().min(morning)
            }
        };
        return (local_time >= before_cutoff).then_some(Stage::DueToday);
    }
    if local_time < morning {
        return None;
    }
    if previous_weekday(due_date) == Some(today) {
        return Some(Stage::BusinessDayBefore);
    }
    let lead = u64::try_from(deadline.internal_lead_days?).ok()?;
    (due_date.checked_sub_days(Days::new(lead)) == Some(today)).then_some(Stage::Lead)
}

fn previous_weekday(date: NaiveDate) -> Option<NaiveDate> {
    date.checked_sub_days(Days::new(match date.weekday() {
        Weekday::Mon => 3,
        Weekday::Sun => 2,
        _ => 1,
    }))
}

fn due_instant(deadline: &Deadline) -> Option<DateTime<FixedOffset>> {
    match &deadline.due_on {
        Some(on) => on.instant().ok(),
        None => DateTime::parse_from_rfc3339(&deadline.due_at).ok(),
    }
}

impl Delivery {
    async fn send(&self, request: &Reminder) -> Result<(), ReminderError> {
        let Some(project) = projects::find_by_id(&self.store, request.project_id)
            .await
            .map_err(|_| ReminderError::Delivery)?
        else {
            return Ok(());
        };
        let Some(channel) = internal_channel(&project) else {
            return Ok(());
        };
        let Some(deadline) = project_deadlines::find(&self.store, project.id, request.deadline_id)
            .await
            .map_err(|_| ReminderError::Delivery)?
        else {
            return Ok(());
        };
        if deadline.calendar_sequence != request.revision {
            return Ok(());
        }
        let timezone = match &deadline.due_on {
            Some(on) => on.timezone,
            None => projects::calendar_timezone(&self.store, &project)
                .await
                .map_err(|_| ReminderError::Delivery)?,
        };
        if stage(&deadline, Utc::now(), &timezone) != Some(request.stage) {
            return Ok(());
        }
        let slack = match self.bots.for_project(&self.store, project.id).await {
            Ok(slack) => slack,
            Err(LaneError::Refused(_) | LaneError::ProjectGone(_)) => return Ok(()),
            Err(_) => return Err(ReminderError::Delivery),
        };
        let mentions = self.mentions(slack.as_ref(), &deadline).await?;
        let link = matter_link(self.portal_origin.as_deref(), &project.code);
        let Some(due) = due_instant(&deadline) else {
            return Ok(());
        };
        let post = message(
            request.stage,
            &deadline,
            due.with_timezone(&timezone).fixed_offset(),
            &mentions,
            &link,
        );
        post_once(slack.as_ref(), channel, &request.key(), &post)
            .await
            .map_err(|_| ReminderError::Delivery)
    }

    async fn mentions(
        &self,
        slack: &dyn workflows::SlackBot,
        deadline: &Deadline,
    ) -> Result<String, ReminderError> {
        let rows = projects::participations_for_project(&self.store, deadline.project_id)
            .await
            .map_err(|_| ReminderError::Delivery)?;
        let mut ids = BTreeSet::new();
        for row in rows.into_iter().filter(|row| {
            !row.is_client_dri
                && !projects::PARTICIPATION_CLIENT_SIDE.contains(&row.participation.as_str())
                && (row.is_lawyer_dri || Some(row.person_id) == deadline.owner_person_id)
        }) {
            let Some(person) = persons::find_by_id(&self.store, row.person_id)
                .await
                .map_err(|_| ReminderError::Delivery)?
            else {
                continue;
            };
            if person.role == Role::Client
                || (row.is_lawyer_dri
                    && !person.role.is_lawyer_tier()
                    && Some(person.id) != deadline.owner_person_id)
            {
                continue;
            }
            let identity = store::external_identities::find_by_person_and_system(
                &self.store,
                person.id,
                store::external_identities::ExternalSystem::Slack,
            )
            .await
            .map_err(|_| ReminderError::Delivery)?;
            let user_id = match identity {
                Some(identity) => Some(identity.external_id),
                None => slack
                    .user_id_by_email(&person.email)
                    .await
                    .map_err(|_| ReminderError::Delivery)?,
            };
            if let Some(id) = user_id.filter(|id| {
                id.starts_with(['U', 'W']) && id.bytes().all(|c| c.is_ascii_alphanumeric())
            }) {
                ids.insert(id);
            }
        }
        let mut mentions = String::new();
        for id in ids {
            let _ = write!(mentions, "<@{id}> ");
        }
        Ok(mentions)
    }
}

fn message(
    stage: Stage,
    deadline: &Deadline,
    instant: DateTime<FixedOffset>,
    mentions: &str,
    link: &str,
) -> String {
    let title = slack_plain_text(deadline.title.as_deref().unwrap_or("Matter deadline"));
    let authority = slack_plain_text(&deadline.authority.chars().take(300).collect::<String>());
    let due = deadline.due_on.as_ref().map_or_else(
        || instant.format("%a, %b %-d at %-I:%M %p %:z").to_string(),
        project_deadlines::DueOn::display_date,
    );
    let local = if deadline
        .due_on
        .as_ref()
        .is_none_or(|on| on.cutoff.is_some())
    {
        format!(
            " · Your time <!date^{}^{{date_short}} at {{time}}|{due}>",
            instant.timestamp()
        )
    } else {
        String::new()
    };
    let link = if link.starts_with('/') {
        link.to_owned()
    } else {
        format!("<{link}|Open matter>")
    };
    format!(
        "{mentions}*{}* — {title}\nDue {due}{local}\n{} — {authority}\n{link} · Deadline ID `{}`",
        stage.label(),
        deadline.authority_kind,
        deadline.id
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage(deadline: &Deadline, now: DateTime<Utc>) -> Option<Stage> {
        super::stage(deadline, now, &deadline.due_on.as_ref().unwrap().timezone)
    }

    fn message(stage: Stage, deadline: &Deadline, mentions: &str, link: &str) -> String {
        super::message(
            stage,
            deadline,
            due_instant(deadline).unwrap(),
            mentions,
            link,
        )
    }

    fn deadline() -> Deadline {
        serde_json::from_value(serde_json::json!({
            "id": Uuid::nil(),
            "project_id": Uuid::nil(),
            "title": "File response",
            "due_at": "2026-12-07T17:00:00-08:00",
            "due_on": {
                "date": "2026-12-07",
                "timezone": "America/Los_Angeles",
                "cutoff": "17:00:00"
            },
            "authority_kind": "court_order",
            "authority": "Scheduling order, § 3",
            "source": "lawyer",
            "status": "open",
            "calendar_role": "firm",
            "internal_lead_days": 7,
            "inserted_at": "2026-11-01T14:00:00Z",
            "updated_at": "2026-11-01T14:00:00Z",
            "calendar_sequence": 1
        }))
        .unwrap()
    }

    fn at(utc: &str) -> DateTime<Utc> {
        utc.parse().unwrap()
    }

    #[test]
    fn deadline_slack_uses_the_court_date_and_previous_weekday() {
        let deadline = deadline();
        for (now, expected) in [
            ("2026-11-30T15:59:00Z", None),
            ("2026-11-30T16:00:00Z", Some(Stage::Lead)),
            ("2026-12-04T16:00:00Z", Some(Stage::BusinessDayBefore)),
            ("2026-12-06T16:00:00Z", None),
            ("2026-12-07T16:00:00Z", Some(Stage::DueToday)),
            ("2026-12-08T00:59:00Z", Some(Stage::DueToday)),
            ("2026-12-08T01:00:00Z", Some(Stage::Overdue)),
            ("2026-12-09T01:00:00Z", None),
        ] {
            assert_eq!(stage(&deadline, at(now)), expected, "{now}");
        }
    }

    #[test]
    fn deadline_slack_coalesces_the_lead_and_previous_weekday_and_skips_inactive_dates() {
        let mut deadline = deadline();
        let friday = at("2026-12-04T16:00:00Z");
        deadline.internal_lead_days = Some(3);
        assert_eq!(stage(&deadline, friday), Some(Stage::BusinessDayBefore));
        deadline.calendar_review_pending = true;
        assert_eq!(stage(&deadline, friday), None);
        deadline.calendar_review_pending = false;
        for status in ["complete", "cancelled"] {
            deadline.status = status.into();
            assert_eq!(stage(&deadline, friday), None);
        }
    }

    #[test]
    fn deadline_slack_moves_early_cutoffs_and_preserves_all_day_dates() {
        let mut deadline = deadline();
        deadline.due_on.as_mut().unwrap().cutoff = NaiveTime::from_hms_opt(7, 30, 0);
        assert_eq!(stage(&deadline, at("2026-12-07T14:29:00Z")), None);
        assert_eq!(
            stage(&deadline, at("2026-12-07T14:30:00Z")),
            Some(Stage::DueToday)
        );
        deadline.due_on.as_mut().unwrap().cutoff = None;
        assert_eq!(
            stage(&deadline, at("2026-12-08T07:59:00Z")),
            Some(Stage::DueToday)
        );
        assert_eq!(
            stage(&deadline, at("2026-12-08T08:00:00Z")),
            Some(Stage::Overdue)
        );
        assert!(!message(Stage::DueToday, &deadline, "", "/matter").contains("<!date"));
    }

    #[test]
    fn deadline_slack_uses_the_project_zone_for_timestamp_only_rows() {
        let mut deadline = deadline();
        let zone = deadline.due_on.take().unwrap().timezone;
        deadline.due_at = "2026-12-08T01:00:00Z".into();
        assert_eq!(
            super::stage(&deadline, at("2026-12-07T16:00:00Z"), &zone),
            Some(Stage::DueToday)
        );
    }

    #[tokio::test]
    async fn deadline_slack_delivers_once_to_the_private_channel_and_tags_the_lawyer() {
        use crate::firm_slack::{FirmSlackBots, SharedSlackBot};
        use store::external_identities::{self, ExternalSystem};
        use store::test_support::{seed_firm_with_new_brand, seed_project_on_firm, with_postgres};

        with_postgres(async {
            let db = ApplicationStore::uninitialized();
            let (firm_id, lawyer_id) = seed_firm_with_new_brand(&db, "neon").await;
            let project = seed_project_on_firm(&db, Some(firm_id), "Slack deadline fixture").await;
            projects::set_internal_slack_channel_id(&db, project.id, "CPRIVATE")
                .await
                .unwrap();
            projects::designate_dri(&db, project.id, lawyer_id, projects::DriSide::Lawyer)
                .await
                .unwrap();
            external_identities::link(&db, lawyer_id, ExternalSystem::Slack, "ULAWYER", None)
                .await
                .unwrap();
            let deadline = project_deadlines::create(
                &db,
                &project_deadlines::NewDeadline {
                    project_id: project.id,
                    title: Some("Review draft".into()),
                    description: None,
                    due_at: (Utc::now() - chrono::Duration::hours(1)).to_rfc3339(),
                    due_on: None,
                    hearing: None,
                    effort_minutes: None,
                    authority_kind: "internal".into(),
                    authority: "Firm review target".into(),
                    source: "lawyer".into(),
                    source_document_id: None,
                    owner_person_id: Some(lawyer_id),
                    calendar_role: "firm".into(),
                    category: Some("internal".into()),
                    internal_lead_days: None,
                    client_lead_days: None,
                    replay_key: None,
                },
            )
            .await
            .unwrap();
            let mut delivery = Delivery {
                store: db,
                bots: Arc::new(FirmSlackBots::new(Arc::new(cloud::FakeKms::new("test")))),
                portal_origin: Some("https://example.test".into()),
            };
            let request = Reminder {
                project_id: project.id,
                deadline_id: deadline.id,
                revision: deadline.calendar_sequence,
                stage: Stage::Overdue,
            };
            // A Firm without an active Slack credential has no delivery path.
            delivery.send(&request).await.unwrap();

            let slack = Arc::new(workflows::CapturingSlackBot::new());
            delivery.bots = Arc::new(SharedSlackBot(slack.clone()));
            delivery.send(&request).await.unwrap();
            delivery.send(&request).await.unwrap();
            let posted = slack.posted_messages();
            assert_eq!(posted.len(), 1);
            assert_eq!(posted[0].0, "CPRIVATE");
            assert!(posted[0]
                .1
                .starts_with("<@ULAWYER> *Overdue* — Review draft"));

            let cancelled = project_deadlines::cancel(&delivery.store, project.id, deadline.id)
                .await
                .unwrap();
            delivery
                .send(&Reminder {
                    revision: cancelled.calendar_sequence,
                    ..request
                })
                .await
                .unwrap();
            assert_eq!(slack.posted_messages(), posted);
        })
        .await;
    }

    #[test]
    fn deadline_slack_formats_real_mentions_and_escapes_docket_text() {
        let mut deadline = deadline();
        deadline.title = Some("File response <!channel>".into());
        deadline.authority = "Order <@UOTHER>".into();
        let message = message(
            Stage::DueToday,
            &deadline,
            "<@ULAWYER> ",
            "https://example.test/app/projects/example",
        );
        assert!(message.starts_with("<@ULAWYER> *Due today* — File response &lt;!channel&gt;"));
        assert!(message.contains("17:00:00 America/Los_Angeles · Your time <!date^"));
        assert!(message.contains("Order &lt;@UOTHER&gt;"));
        assert!(message.contains("<https://example.test/app/projects/example|Open matter>"));
        assert!(message.contains(&deadline.id.to_string()));
    }
}
