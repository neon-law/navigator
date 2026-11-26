//! Copy for the Daybridge divorce-law home page.

use serde::{Deserialize, Serialize};

/// Copy for the Daybridge homepage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaybridgeCopy {
    pub testimonials_heading: String,
    pub signup_label: String,
    pub signin_label: String,
    /// Sibling practices this home points a reader to, such as Vesta Estate
    /// Planning.
    #[serde(default)]
    pub sibling_practices: Vec<super::SiblingPracticeCopy>,
}
