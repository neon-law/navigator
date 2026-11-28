//! Firm-owned filing rules for covered Legal Plan benefits.

use std::collections::BTreeSet;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// The event that opens a window to submit a covered claim.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FilingTrigger {
    ServiceCompleted,
    HoursExhausted,
    MatterClosed,
    HoursExhaustedOrMatterClosed,
}

impl FilingTrigger {
    /// The stable spelling used in configuration files and storage.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ServiceCompleted => "service_completed",
            Self::HoursExhausted => "hours_exhausted",
            Self::MatterClosed => "matter_closed",
            Self::HoursExhaustedOrMatterClosed => "hours_exhausted_or_matter_closed",
        }
    }
}

/// A dated rule for one exact benefit code on a Legal Plan.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FilingRule {
    pub benefit_code: String,
    pub effective_on: NaiveDate,
    pub trigger: FilingTrigger,
    pub filing_window_days: Option<u32>,
    pub filing_destination: String,
    pub filer: String,
}

/// Plan metadata and explicitly verified benefit rules to apply at the Firm.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PlanBillingConfiguration {
    pub name: String,
    pub contact: Option<String>,
    pub portal: Option<String>,
    pub acceptance_sla_hours: Option<i32>,
    pub turnaround_sla_days: Option<i32>,
    pub reporting_requirements: Option<String>,
    #[serde(default)]
    pub filing_rules: Vec<FilingRule>,
}

impl PlanBillingConfiguration {
    /// Validate the full document before it is sent or written.
    ///
    /// # Errors
    /// Returns a static reason when metadata or a filing rule is inconsistent.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !label(self.name.trim(), 200) {
            return Err("name");
        }
        if self.acceptance_sla_hours.is_some_and(|hours| hours <= 0) {
            return Err("acceptance_sla_hours");
        }
        if self.turnaround_sla_days.is_some_and(|days| days <= 0) {
            return Err("turnaround_sla_days");
        }
        for (field, value) in [
            ("contact", &self.contact),
            ("portal", &self.portal),
            ("reporting_requirements", &self.reporting_requirements),
        ] {
            if value.as_deref().is_some_and(|text| text.contains('\0')) {
                return Err(field);
            }
        }

        let mut dates = BTreeSet::new();
        for rule in &self.filing_rules {
            if !label(&rule.benefit_code, 128) {
                return Err("benefit_code");
            }
            if !dates.insert((&rule.benefit_code, rule.effective_on)) {
                return Err("duplicate benefit_code and effective_on");
            }
            if rule.filing_window_days == Some(0) {
                return Err("filing_window_days");
            }
            if !label(&rule.filing_destination, 2048) {
                return Err("filing_destination");
            }
            if !label(&rule.filer, 200) {
                return Err("filer");
            }
        }
        Ok(())
    }
}

fn label(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value == value.trim()
        && value.len() <= max_bytes
        && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_refuses_unknown_fields_and_duplicate_rule_keys() {
        let yaml = r"
name: Sample Plan
filing_rules:
  - benefit_code: EXAMPLE
    effective_on: 2026-01-01
    trigger: service_completed
    filing_window_days: 30
    filing_destination: https://filing.example/
    filer: responsible lawyer
";
        let mut config: PlanBillingConfiguration = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.validate(), Ok(()));
        let unknown = format!("{yaml}member_id: not-a-plan-field\n");
        assert!(serde_yaml::from_str::<PlanBillingConfiguration>(&unknown).is_err());
        let unknown_rule = yaml.replace("    filer:", "    evidence: unknown\n    filer:");
        assert!(serde_yaml::from_str::<PlanBillingConfiguration>(&unknown_rule).is_err());

        config.filing_rules.push(config.filing_rules[0].clone());
        assert_eq!(
            config.validate(),
            Err("duplicate benefit_code and effective_on")
        );
        config.filing_rules.pop();
        config.filing_rules[0].filing_window_days = Some(0);
        assert_eq!(config.validate(), Err("filing_window_days"));
    }
}
