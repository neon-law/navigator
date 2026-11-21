//! Copy for the Daybridge divorce-law home page.

use serde::{Deserialize, Serialize};

/// The two prices, what sits beside them, and the plain-spoken story.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaybridgeCopy {
    pub eyebrow: String,
    pub fee_label: String,
    pub agreed_label: String,
    pub agreed_price: String,
    pub agreed_unit: String,
    pub agreed_body: String,
    pub contested_label: String,
    pub contested_price: String,
    pub contested_unit: String,
    pub contested_body: String,
    pub testimonials_heading: String,
    pub signup_label: String,
    pub signin_label: String,
    pub closing_heading: String,
    pub closing_body: String,
    /// Sibling practices this home points a reader to, such as Vesta Estate
    /// Planning.
    #[serde(default)]
    pub sibling_practices: Vec<super::SiblingPracticeCopy>,
}
