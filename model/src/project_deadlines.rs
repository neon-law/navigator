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
        || matches!(category, Some("hearing" | "court"))
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
}

impl Hearing {
    #[must_use]
    pub fn minutes(&self) -> u32 {
        self.duration_minutes.map_or(60, std::num::NonZeroU32::get)
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
    let category = if role == "hearing" {
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
