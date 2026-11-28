//! The verified cap and rates for the Legal Plan benefit linked to a Project.

use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanCapShape {
    HourlyCap,
    DollarCap,
    ContestedUncontested,
    ConsultOnly,
    FlatFee,
}

impl PlanCapShape {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HourlyCap => "hourly_cap",
            Self::DollarCap => "dollar_cap",
            Self::ContestedUncontested => "contested_uncontested",
            Self::ConsultOnly => "consult_only",
            Self::FlatFee => "flat_fee",
        }
    }
}

impl FromStr for PlanCapShape {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "hourly-cap" | "hourly_cap" => Ok(Self::HourlyCap),
            "dollar-cap" | "dollar_cap" => Ok(Self::DollarCap),
            "contested-uncontested" | "contested_uncontested" => Ok(Self::ContestedUncontested),
            "consult-only" | "consult_only" => Ok(Self::ConsultOnly),
            "flat-fee" | "flat_fee" => Ok(Self::FlatFee),
            _ => {
                Err("use hourly-cap, dollar-cap, contested-uncontested, consult-only, or flat-fee")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanBenefitTerms {
    pub cap_shape: PlanCapShape,
    /// The total cap in cents, or the uncontested cap when both are stated.
    pub cap_cents: Option<i32>,
    pub cap_minutes: Option<i32>,
    pub contested_cap_cents: Option<i32>,
    pub plan_rate_cents_per_hour: Option<i32>,
    pub paralegal_rate_cents_per_hour: Option<i32>,
    /// Verified hourly rate paid by the member for separately agreed uncovered work.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member_rate_cents_per_hour: Option<i32>,
}

impl PlanBenefitTerms {
    /// Check the combination of cap units and the nonnegative rates.
    ///
    /// # Errors
    /// Returns a diagnostic when the selected cap shape needs other units or a rate is negative.
    pub fn validate(&self) -> Result<(), &'static str> {
        let no_hours = self.cap_minutes.is_none();
        let no_contested_cap = self.contested_cap_cents.is_none();
        let dollars = self.cap_cents.is_some_and(|amount| amount > 0);
        let valid_cap = match self.cap_shape {
            PlanCapShape::HourlyCap => {
                self.cap_minutes.is_some_and(|minutes| minutes > 0)
                    && self.cap_cents.is_none()
                    && no_contested_cap
            }
            PlanCapShape::DollarCap | PlanCapShape::FlatFee => {
                no_hours && no_contested_cap && dollars
            }
            PlanCapShape::ContestedUncontested => {
                no_hours && dollars && self.contested_cap_cents.is_some_and(|amount| amount > 0)
            }
            PlanCapShape::ConsultOnly => no_hours && self.cap_cents.is_none() && no_contested_cap,
        };
        if !valid_cap {
            return Err("the cap values do not match the Legal Plan cap shape");
        }
        if [
            self.plan_rate_cents_per_hour,
            self.paralegal_rate_cents_per_hour,
            self.member_rate_cents_per_hour,
        ]
        .into_iter()
        .flatten()
        .any(|cents| cents < 0)
        {
            return Err("Legal Plan rates must be nonnegative");
        }
        Ok(())
    }
}
