use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, NaiveTime, TimeZone, Timelike};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, VariantNames};

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, VariantNames,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum AuthorityKind {
    Statute,
    CourtRule,
    CaseLaw,
    CourtOrder,
    Contract,
    Internal,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, VariantNames,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum CalendarRole {
    Firm,
    Opponent,
    Hearing,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, VariantNames,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum Category {
    Court,
    Hearing,
    Discovery,
    Internal,
    Tickler,
    TrialDay,
    Deposition,
    Conference,
    Consult,
    Call,
}

pub const CATEGORIES: &[&str] = Category::VARIANTS;
pub const AUTHORITY_KINDS: &[&str] = AuthorityKind::VARIANTS;
pub const CALENDAR_ROLES: &[&str] = CalendarRole::VARIANTS;

/// Whether a new or rescheduled event requires reviewed court evidence.
#[must_use]
pub fn requires_court_review(
    calendar_role: &str,
    authority_kind: &str,
    category: Option<&str>,
) -> bool {
    matches!(
        authority_kind,
        "statute" | "court_rule" | "court_order" | "case_law"
    ) || calendar_role == "hearing"
        || matches!(category, Some("hearing" | "court" | "trial_day"))
}

/// Historical inputs may leave the category blank for the role-based default.
pub fn optional_category<'de, D>(deserializer: D) -> Result<Option<Category>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)?
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::parse)
        .transpose()
        .map_err(serde::de::Error::custom)
}

/// Verified details of a timed appearance. The calendar reserves one hour
/// when the hearing notice gives no duration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Hearing {
    pub duration_minutes: Option<std::num::NonZeroU32>,
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance: Option<Appearance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prep: Option<Vec<PreparationDeadline>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Appearance {
    TrialDay {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        additional_starts: Vec<DateTime<FixedOffset>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        day: Option<TrialDay>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        cancel: bool,
    },
    Deposition {
        deponent_role: DeponentRole,
        taken_by: TakenBy,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct TrialDay {
    pub series_id: uuid::Uuid,
    pub number: u16,
    pub total: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, EnumString)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum DeponentRole {
    Party,
    NonParty,
    Expert,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, EnumString)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum TakenBy {
    Firm,
    Opponent,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparationDeadline {
    pub title: String,
    pub due_at: DateTime<FixedOffset>,
    pub effort_minutes: i32,
}

impl Hearing {
    #[must_use]
    pub fn minutes(&self) -> u32 {
        self.duration_minutes.map_or(60, std::num::NonZeroU32::get)
    }
}

impl Hearing {
    /// Internal planning suggestions use ordinary calendar days. Court-rule dates need evidence review.
    #[must_use]
    pub fn preparation(&self, start: DateTime<FixedOffset>) -> Vec<PreparationDeadline> {
        let tasks: &[(&str, i64, i32)] = match self.appearance {
            Some(Appearance::TrialDay { .. }) => &[
                ("Review pretrial requirements", 28, 120),
                ("Prepare exhibit and witness lists", 21, 360),
                ("Review motions in limine and disclosures", 14, 360),
            ],
            Some(Appearance::Deposition { .. }) => &[
                ("Review deposition notice and subpoena service", 14, 60),
                ("Prepare deposition outline", 3, 240),
            ],
            None => &[],
        };
        tasks
            .iter()
            .filter_map(|(title, days, minutes)| {
                Some(PreparationDeadline {
                    title: (*title).into(),
                    due_at: start.checked_sub_signed(chrono::TimeDelta::days(*days))?,
                    effort_minutes: if *title == "Prepare deposition outline" {
                        i32::try_from(i64::from(self.minutes()) * 4).unwrap_or(i32::MAX)
                    } else {
                        *minutes
                    },
                })
            })
            .collect()
    }

    pub fn validate_appearance(
        &self,
        category: Option<Category>,
        start: DateTime<FixedOffset>,
    ) -> Result<(), &'static str> {
        match (&self.appearance, category) {
            (
                Some(Appearance::TrialDay {
                    additional_starts, ..
                }),
                Some(Category::TrialDay),
            ) => {
                if additional_starts.len() >= 120
                    || !std::iter::once(&start)
                        .chain(additional_starts)
                        .zip(additional_starts)
                        .all(|(earlier, later)| earlier < later)
                {
                    return Err("trial starts must be in date order, with at most 120 days");
                }
            }
            (Some(Appearance::Deposition { .. }), Some(Category::Deposition)) => {}
            (None, Some(Category::TrialDay | Category::Deposition)) | (Some(_), _) => {
                return Err("trial and deposition details must match the deadline category");
            }
            _ => {}
        }
        if self.appearance.is_some()
            && (self.duration_minutes.is_none()
                || self
                    .location
                    .as_deref()
                    .is_none_or(|location| location.trim().is_empty()))
        {
            return Err(
                "a trial day or deposition requires a duration and location or remote link",
            );
        }
        if self.prep.as_ref().is_some_and(|prep| {
            prep.len() > 32
                || prep.iter().any(|row| {
                    row.title.trim().is_empty()
                        || row.title.chars().count() > 200
                        || row.effort_minutes < 0
                        || row.due_at >= start
                })
        }) {
            return Err(
                "preparation needs a title, nonnegative effort, and a date before the appearance",
            );
        }
        Ok(())
    }
}

/// The firm's internal calendar targets preserve the deadline's recorded legal date.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(try_from = "(i64, NaiveTime)", into = "(i64, NaiveTime)")]
pub struct SchedulingPolicy {
    buffer_minutes: i64,
    end_of_business: NaiveTime,
}

impl Default for SchedulingPolicy {
    fn default() -> Self {
        Self {
            buffer_minutes: 15,
            end_of_business: NaiveTime::from_hms_opt(17, 0, 0).unwrap_or(NaiveTime::MIN),
        }
    }
}

impl TryFrom<(i64, NaiveTime)> for SchedulingPolicy {
    type Error = &'static str;

    fn try_from((buffer, end): (i64, NaiveTime)) -> Result<Self, Self::Error> {
        Self::new(buffer, end)
    }
}

impl From<SchedulingPolicy> for (i64, NaiveTime) {
    fn from(policy: SchedulingPolicy) -> Self {
        (policy.buffer_minutes, policy.end_of_business)
    }
}

impl SchedulingPolicy {
    pub fn new(buffer_minutes: i64, end_of_business: NaiveTime) -> Result<Self, &'static str> {
        if !(10..=1440).contains(&buffer_minutes) {
            return Err("deadline buffer must be between 10 and 1440 minutes");
        }
        Ok(Self {
            buffer_minutes,
            end_of_business,
        })
    }

    #[must_use]
    pub const fn end_of_business(self) -> NaiveTime {
        self.end_of_business
    }

    #[must_use]
    pub const fn buffer_minutes(self) -> i64 {
        self.buffer_minutes
    }

    #[must_use]
    pub fn appearance(
        self,
        start: DateTime<FixedOffset>,
        duration_minutes: u32,
    ) -> Option<(DateTime<FixedOffset>, DateTime<FixedOffset>)> {
        let buffer = chrono::TimeDelta::minutes(self.buffer_minutes);
        let end = chrono::TimeDelta::minutes(i64::from(duration_minutes)) + buffer;
        Some((
            start.checked_sub_signed(buffer)?,
            start.checked_add_signed(end)?,
        ))
    }

    #[must_use]
    pub fn submit_by(
        self,
        due: DateTime<FixedOffset>,
        civil: Option<&DueOn>,
    ) -> Option<DateTime<FixedOffset>> {
        let cutoff = match civil {
            Some(day) if day.cutoff.is_none() => day
                .timezone
                .from_local_datetime(&day.date.and_time(self.end_of_business))
                .earliest()?
                .fixed_offset(),
            Some(day) => due.with_timezone(&day.timezone).fixed_offset(),
            None => due,
        };
        cutoff.checked_sub_signed(chrono::TimeDelta::minutes(self.buffer_minutes))
    }
}

#[must_use]
pub fn is_appearance(role: &str, category: Option<Category>) -> bool {
    role == "hearing" || category.is_some_and(Category::is_appearance)
}

impl Category {
    #[must_use]
    pub const fn is_appearance(self) -> bool {
        matches!(
            self,
            Self::Hearing
                | Self::TrialDay
                | Self::Deposition
                | Self::Conference
                | Self::Consult
                | Self::Call
        )
    }

    #[must_use]
    pub const fn default_effort_minutes(self) -> i32 {
        match self {
            Self::Court => 360,
            Self::Discovery => 240,
            Self::Internal => 30,
            Self::Tickler => 15,
            _ => 0,
        }
    }
}

/// Input may inherit the current Project zone. Saved dates always carry the resolved zone.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DueOnInput {
    pub date: NaiveDate,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<Tz>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cutoff: Option<NaiveTime>,
}

/// A civil due date. Only a supplied cutoff carries a legal time of day.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DueOn {
    pub date: NaiveDate,
    pub timezone: Tz,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cutoff: Option<NaiveTime>,
}

impl DueOn {
    #[must_use]
    pub fn display_date(&self) -> String {
        let day = self.date.format("%A, %-d %b %Y");
        match self.cutoff {
            Some(time) => format!("{day} · {time} {}", self.timezone),
            None => day.to_string(),
        }
    }

    /// Existing sorting and overdue checks need an instant. A date without a
    /// cutoff stays open through the end of that day in the governing zone.
    pub fn instant(&self) -> Result<DateTime<FixedOffset>, &'static str> {
        if !(1..=9999).contains(&self.date.year())
            || self.cutoff.is_some_and(|time| {
                time.nanosecond() >= 1_000_000_000 || !time.nanosecond().is_multiple_of(1000)
            })
        {
            return Err(
                "calendar date or cutoff is outside the supported range or microsecond precision",
            );
        }
        let end = self
            .date
            .succ_opt()
            .filter(|date| date.year() <= 9999)
            .ok_or("due date has no supported following day")?;
        let local = match self.cutoff {
            Some(time) => self.date.and_time(time),
            None => end.and_time(NaiveTime::MIN),
        };
        let instant = self
            .timezone
            .from_local_datetime(&local)
            .single()
            .ok_or("calendar time is ambiguous or does not exist in the supplied time zone")?;
        Ok(instant.fixed_offset())
    }

    #[must_use]
    pub fn cutoff_description(&self) -> Option<String> {
        self.cutoff
            .map(|time| format!("Cutoff: {time} {}", self.timezone))
    }
}

/// The two accepted forms on Deadline creation and rescheduling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Due {
    At(DateTime<FixedOffset>),
    On(DueOn),
}

impl Due {
    pub fn from_fields(at: Option<&str>, on: Option<DueOn>) -> Result<Self, &'static str> {
        match (at, on) {
            (Some(value), None) => DateTime::parse_from_rfc3339(value.trim())
                .map(Self::At)
                .map_err(|_| "due_at must be an RFC 3339 timestamp with an explicit offset"),
            (None, Some(date)) => {
                date.instant()?;
                Ok(Self::On(date))
            }
            _ => Err("supply exactly one of due_at and due_on"),
        }
    }

    pub fn into_fields(self) -> Result<(String, Option<DueOn>), &'static str> {
        match self {
            Self::At(at) => Ok((at.to_rfc3339(), None)),
            Self::On(on) => Ok((on.instant()?.to_rfc3339(), Some(on))),
        }
    }
}

/// Title shared by the Google Calendar mirror and the iCalendar export.
#[must_use]
pub fn calendar_summary(
    code: &str,
    title: &str,
    role: &str,
    authority_kind: &str,
    category: Option<Category>,
    status: &str,
) -> String {
    let summary = classified_summary(code, title, role, authority_kind, category);
    if status == "complete" {
        format!("{summary} (complete)")
    } else {
        summary
    }
}

fn classified_summary(
    code: &str,
    title: &str,
    role: &str,
    authority_kind: &str,
    category: Option<Category>,
) -> String {
    let summary = action_summary(code, title, role, authority_kind);
    let category = if role == "hearing" && !category.is_some_and(Category::is_appearance) {
        Some(Category::Hearing)
    } else {
        category
    };
    let Some(category) = category else {
        return summary;
    };
    let label = match category {
        Category::Court => "DEADLINE",
        Category::Hearing => "HEARING",
        Category::Discovery => "DISCOVERY",
        Category::Internal => "INTERNAL",
        Category::Tickler => "TICKLER",
        Category::TrialDay => "TRIAL",
        Category::Deposition => "DEPOSITION",
        Category::Conference => "CONFERENCE",
        Category::Consult => "CONSULT",
        Category::Call => "CALL",
    };
    let prefix = format!("{code}: ");
    let action = summary.strip_prefix(&prefix).unwrap_or(&summary);
    if action.starts_with(&format!("{label} – ")) {
        return summary;
    }
    format!("{prefix}{label} – {action}")
}

fn action_summary(code: &str, title: &str, role: &str, authority_kind: &str) -> String {
    let title = title.trim();
    if !title.is_empty() {
        return crate::projects::calendar_event_title(code, title);
    }
    let kind = match (role, authority_kind) {
        ("hearing", _) => "Hearing",
        (_, "statute") => "Statutory deadline",
        (_, "court_rule") => "Court rule deadline",
        (_, "case_law") => "Case law deadline",
        (_, "court_order") => "Court order deadline",
        (_, "contract") => "Contract deadline",
        _ => "Internal deadline",
    };
    if role == "opponent" {
        crate::projects::calendar_event_title(code, &format!("{kind} (opponent)"))
    } else {
        crate::projects::calendar_event_title(code, kind)
    }
}

/// Popup lead times for a Deadline, expressed in whole days.
#[must_use]
pub fn reminder_days(status: &str, category: Option<Category>, lead: Option<i64>) -> Vec<i64> {
    if status != "open" {
        return Vec::new();
    }
    let mut days = if category == Some(Category::Court) {
        vec![7, 3, 1]
    } else {
        Vec::new()
    };
    if let Some(lead) = lead {
        days.push(lead);
    }
    days.sort_unstable_by(|a, b| b.cmp(a));
    days.dedup();
    days
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn buffered_windows_preserve_the_appearance_and_filing_times() {
        let policy = SchedulingPolicy::default();
        let start = DateTime::parse_from_rfc3339("2028-07-12T10:00:00-07:00").unwrap();
        let (from, until) = policy.appearance(start, 120).unwrap();
        assert_eq!(from.to_rfc3339(), "2028-07-12T09:45:00-07:00");
        assert_eq!(until.to_rfc3339(), "2028-07-12T12:15:00-07:00");
        let cutoff = DateTime::parse_from_rfc3339("2028-07-12T16:30:00-07:00").unwrap();
        assert_eq!(
            policy.submit_by(cutoff, None).unwrap().to_rfc3339(),
            "2028-07-12T16:15:00-07:00"
        );
        assert!(SchedulingPolicy::new(5, policy.end_of_business).is_err());
        let civil = DueOn {
            date: cutoff.date_naive(),
            timezone: chrono_tz::America::Los_Angeles,
            cutoff: None,
        };
        assert_eq!(
            policy
                .submit_by(civil.instant().unwrap(), Some(&civil))
                .unwrap()
                .to_rfc3339(),
            "2028-07-12T16:45:00-07:00"
        );
    }

    #[test]
    fn deposition_preparation_scales_with_the_deposition_and_can_be_dropped() {
        let start = DateTime::parse_from_rfc3339("2028-07-12T10:00:00-07:00").unwrap();
        let mut details = Hearing {
            duration_minutes: std::num::NonZeroU32::new(180),
            location: Some("Remote".into()),
            appearance: Some(Appearance::Deposition {
                deponent_role: DeponentRole::Party,
                taken_by: TakenBy::Firm,
            }),
            ..Default::default()
        };
        let suggestions = details.preparation(start);
        assert_eq!(suggestions.len(), 2);
        assert_eq!(suggestions[1].effort_minutes, 720);
        details.prep = Some(Vec::new());
        assert!(details
            .validate_appearance(Some(Category::Deposition), start)
            .is_ok());
    }

    #[test]
    fn civil_due_date_and_optional_cutoff_survive_the_utc_day_boundary() {
        let mut date: DueOn = serde_json::from_value(serde_json::json!({
            "date": "2028-07-12", "timezone": "America/Los_Angeles", "cutoff": "23:59:00"
        }))
        .unwrap();
        let (at, saved) = Due::from_fields(None, Some(date.clone()))
            .unwrap()
            .into_fields()
            .unwrap();
        assert_eq!(at, "2028-07-12T23:59:00-07:00");
        assert_eq!(saved.unwrap().date, date.date);
        assert_eq!(
            date.instant().unwrap().with_timezone(&Utc).date_naive(),
            date.date.succ_opt().unwrap()
        );
        assert!(Due::from_fields(Some(&at), Some(date.clone())).is_err());
        date.cutoff = None;
        assert!(date.cutoff_description().is_none());
        assert_eq!(
            date.instant().unwrap().to_rfc3339(),
            "2028-07-13T00:00:00-07:00"
        );
    }

    #[test]
    fn local_cutoffs_in_a_dst_gap_or_overlap_require_an_explicit_instant() {
        for (day, time) in [("2028-03-12", "02:30:00"), ("2028-11-05", "01:30:00")] {
            let date: DueOn = serde_json::from_value(
                serde_json::json!({"date":day,"timezone":"America/Los_Angeles","cutoff":time}),
            )
            .unwrap();
            assert!(Due::from_fields(None, Some(date)).is_err());
        }
    }
}
