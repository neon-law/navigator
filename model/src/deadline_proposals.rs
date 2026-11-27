//! Versioned court dates and the source regions a lawyer reviews before publication.
use crate::project_deadlines::{AuthorityKind, CalendarRole, Category, Due, DueOn, Hearing};
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveTime};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    SignedOrder,
    Calculated,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    SignedOrder,
    Trigger,
    Authority,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub role: Role,
    pub id: Uuid,
    pub sha256_hex: String,
    pub page: u32,
    #[serde(default)]
    pub rects: Vec<Rect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forgejo: Option<ForgejoPin>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForgejoRepository {
    Project,
    FirmLibrary,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgejoPin {
    pub repository: ForgejoRepository,
    pub commit: String,
    pub path: String,
}

impl ForgejoPin {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.commit.len() != 40
            || self.commit.bytes().all(|b| b == b'0')
            || !self
                .commit
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("a Forgejo citation requires a full lowercase commit SHA");
        }
        if self.path.len() > 4096
            || self.path.chars().any(char::is_control)
            || self.path.contains(['\\', '%', '#', '?', ':'])
            || self.path.split('/').any(|part| {
                part.is_empty()
                    || part.len() > 255
                    || part.trim() != part
                    || matches!(part, "." | "..")
                    || part.eq_ignore_ascii_case(".git")
            })
        {
            return Err("a Forgejo citation requires a safe repository-relative file path");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CountingBasis {
    CalendarDays,
    CourtDays,
    Holidays,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Calculation {
    pub trigger: String,
    pub trigger_on: NaiveDate,
    pub offset_days: i32,
    pub counting_basis: CountingBasis,
    pub authority_kind: AuthorityKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Draft<Zone = Tz> {
    pub kind: Kind,
    pub title: String,
    pub timezone: Zone,
    pub due_on: Option<NaiveDate>,
    pub due_at: Option<DateTime<FixedOffset>>,
    pub cutoff: Option<NaiveTime>,
    pub category: Option<Category>,
    pub calendar_role: Option<CalendarRole>,
    pub hearing: Option<Hearing>,
    pub calculation: Option<Calculation>,
    #[serde(default)]
    pub evidence: Vec<Evidence>,
}

/// Submitted dates may inherit the Project or saved revision’s timezone.
pub type DraftInput = Draft<Option<Tz>>;

impl DraftInput {
    #[must_use]
    pub fn with_timezone(self, timezone: Tz) -> Draft {
        Draft {
            kind: self.kind,
            title: self.title,
            timezone,
            due_on: self.due_on,
            due_at: self.due_at,
            cutoff: self.cutoff,
            category: self.category,
            calendar_role: self.calendar_role,
            hearing: self.hearing,
            calculation: self.calculation,
            evidence: self.evidence,
        }
    }
}

impl Draft {
    /// Each saved region is normalized to the original page in top-left coordinates.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.title.chars().count() > 200 || self.evidence.len() > 32 {
            return Err("proposal title or evidence is too long");
        }
        for source in &self.evidence {
            source.validate()?;
        }
        Ok(())
    }

    /// Required only at confirmation. A human supplies dates counted with court days or holidays.
    pub fn reviewed_due(&self) -> Result<Due, &'static str> {
        self.validate()?;
        if self.title.trim().is_empty() {
            return Err("a reviewed title is required");
        }
        if self.evidence.is_empty() || self.evidence.iter().any(|e| e.rects.is_empty()) {
            return Err("highlight every cited page before confirmation");
        }
        let has = |role| self.evidence.iter().any(|e| e.role == role);
        let date = self.due_on.or_else(|| {
            self.due_at
                .map(|at| at.with_timezone(&self.timezone).date_naive())
        });
        match (&self.kind, &self.calculation) {
            (Kind::SignedOrder, None) if has(Role::SignedOrder) => (),
            (Kind::Calculated, Some(rule))
                if has(Role::Trigger)
                    && has(Role::Authority)
                    && !rule.trigger.trim().is_empty()
                    && matches!(
                        rule.authority_kind,
                        AuthorityKind::CourtRule | AuthorityKind::Statute | AuthorityKind::CaseLaw
                    ) =>
            {
                if rule.counting_basis == CountingBasis::CalendarDays
                    && rule
                        .trigger_on
                        .checked_add_signed(chrono::Duration::days(i64::from(rule.offset_days)))
                        != date
                {
                    return Err("reviewed date disagrees with the calendar-day calculation");
                }
            }
            _ => {
                return Err(
                    "cite a signed order or a triggering document and filed governing authority",
                )
            }
        }
        if self.due_on.is_none() && self.cutoff.is_some() {
            return Err("only a date-only deadline accepts a cutoff");
        }
        if self.category == Some(Category::TrialDay) && self.kind != Kind::SignedOrder {
            return Err("a trial day requires the filed order or stipulation");
        }
        let appearance = crate::project_deadlines::is_appearance(
            if self.calendar_role == Some(CalendarRole::Hearing) {
                "hearing"
            } else {
                ""
            },
            self.category,
        );
        if appearance && self.due_at.is_none() || self.hearing.is_some() && !appearance {
            return Err("an appearance requires a timed start and appearance category");
        }
        if self.calendar_role == Some(CalendarRole::Hearing)
            && self
                .category
                .is_some_and(|category| !category.is_appearance())
        {
            return Err("a hearing role requires an appearance category");
        }
        if let (Some(details), Some(start)) = (&self.hearing, self.due_at) {
            details.validate_appearance(self.category, start)?;
        }
        Due::from_fields(
            self.due_at.map(|v| v.to_rfc3339()).as_deref(),
            self.due_on.map(|date| DueOn {
                date,
                timezone: self.timezone,
                cutoff: self.cutoff,
            }),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Approved,
    Published,
    Returned,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub id: Uuid,
    pub project_id: Uuid,
    pub revision: i64,
    pub status: Status,
    #[serde(flatten)]
    pub draft: Draft,
    pub deadline_id: Option<Uuid>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Decision {
    Confirm,
    Return { reason: String },
}

impl Evidence {
    pub fn validate(&self) -> Result<(), &'static str> {
        if let Some(pin) = &self.forgejo {
            pin.validate()?;
        }
        if self.id.is_nil()
            || self.page == 0
            || self.sha256_hex.len() != 64
            || !self
                .sha256_hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("evidence requires a source digest and one-based page");
        }
        if self.rects.len() > 64
            || self.rects.iter().any(|r| {
                !(r.x >= 0.
                    && r.y >= 0.
                    && r.width > 0.
                    && r.height > 0.
                    && r.x + r.width <= 1.
                    && r.y + r.height <= 1.)
            })
        {
            return Err("evidence rectangle is outside the page");
        }

        Ok(())
    }
}

impl Calculation {
    #[must_use]
    pub fn authority_class(&self) -> Option<&'static str> {
        match self.authority_kind {
            AuthorityKind::CaseLaw => Some("case_law"),
            AuthorityKind::CourtRule => Some("regulation"),
            AuthorityKind::Statute => Some("statute"),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn submitted_timezone_can_inherit_but_malformed_draft_fields_are_rejected() {
        let mut input =
            json!({"kind":"signed_order","title":"File response","due_on":"2030-07-16"});
        for supplied in [
            None,
            Some(serde_json::Value::Null),
            Some(json!("America/Chicago")),
        ] {
            if let Some(value) = supplied {
                input["timezone"] = value;
            }
            let draft: DraftInput = serde_json::from_value(input.clone()).unwrap();
            let zone = draft.timezone.unwrap_or(chrono_tz::America::New_York);
            let resolved = draft.with_timezone(zone);
            assert_eq!(
                resolved.timezone.name(),
                input["timezone"].as_str().unwrap_or("America/New_York")
            );
            assert_eq!(resolved.due_on.unwrap().to_string(), "2030-07-16");
        }
        input["timezone"] = json!("unrecognized zone");
        assert!(serde_json::from_value::<DraftInput>(input.clone()).is_err());
        input["timezone"] = serde_json::Value::Null;
        input["unrecognized"] = json!(true);
        assert!(serde_json::from_value::<DraftInput>(input).is_err());
    }

    #[test]
    fn forgejo_citations_pin_a_file_and_refuse_unscoped_paths_or_mutable_refs() {
        let mut evidence: Evidence = serde_json::from_value(json!({
            "role":"authority","id":Uuid::from_u128(1),"sha256_hex":"a".repeat(64),"page":1
        }))
        .unwrap();
        assert!(serde_json::to_value(&evidence)
            .unwrap()
            .get("forgejo")
            .is_none());
        let mut pin = ForgejoPin {
            repository: ForgejoRepository::FirmLibrary,
            commit: "c".repeat(40),
            path: "rules/Court rule.pdf".into(),
        };
        assert!(pin.validate().is_ok());
        evidence.forgejo = Some(pin.clone());
        evidence.role = Role::SignedOrder;
        assert!(evidence.validate().is_ok());
        for path in [
            "",
            "/x",
            "rules/../x",
            "x//y",
            "x/%2e%2e/y",
            "x?branch=main",
            "x#y",
            "x\\y",
            ".Git/config",
            " ./a",
        ] {
            pin.path = path.into();
            assert!(pin.validate().is_err(), "{path}");
        }
        pin.path = "rules/a.pdf".into();
        for commit in ["main".to_owned(), "A".repeat(40), "0".repeat(40)] {
            pin.commit = commit;
            assert!(pin.validate().is_err());
        }
    }
    #[test]
    fn calculated_dates_require_a_trigger_a_governing_source_and_consistent_local_date() {
        let evidence = |role| json!({"role":role,"id":Uuid::from_u128(1),"sha256_hex":"a".repeat(64),"page":2,"rects":[{"x":0.1,"y":0.2,"width":0.3,"height":0.2}]});
        let mut draft:Draft=serde_json::from_value(json!({"kind":"calculated","title":"Synthetic response","timezone":"America/Chicago","due_on":"2028-04-01","evidence":[evidence("trigger"),evidence("authority")],"calculation":{"trigger":"Filed synthetic notice","trigger_on":"2028-03-28","offset_days":4,"counting_basis":"calendar_days","authority_kind":"case_law"}})).unwrap();
        assert!(draft.reviewed_due().is_ok());
        draft.due_on = Some("2028-04-02".parse().unwrap());
        assert!(draft.reviewed_due().is_err());
        draft.calculation.as_mut().unwrap().counting_basis = CountingBasis::CourtDays;
        assert!(draft.reviewed_due().is_ok());
        draft.evidence.remove(1);
        assert!(draft.reviewed_due().is_err());
        draft.evidence[0].rects[0].width = 1.0;
        assert!(draft.validate().is_err());
    }
}
