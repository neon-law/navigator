//! The monthly and annual privacy offer, authored in the brand home catalog.
use serde::{Deserialize, Serialize};

/// Copy for the one-page data removal and privacy protection product.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivacyCopy {
    pub eyebrow: String,
    pub price: String,
    pub price_term: String,
    pub annual_price: String,
    pub annual_term: String,
    pub monthly_label: String,
    pub annual_label: String,
    /// Billing details stated under the monthly price.
    pub household_note: String,
    pub annual_household_note: String,
    pub offer_note: String,
    pub gift_link: String,
    pub benefits_heading: String,
    pub record_label: String,
    pub record_heading: String,
    pub record_body: String,
    pub gift_heading: String,
    pub gift_body: String,
    pub gift_cta: String,
    pub gift_card_label: String,
    pub gift_card_term: String,
    pub closing_heading: String,
    pub benefits: Vec<[String; 2]>,
    /// Sibling practices the page points a reader to: the optional
    /// [Death Check](https://www.neonlaw.com/glossary#death-check) through
    /// Vesta Estate Planning, and cyber-crime claims through Cyber Injury Law.
    #[serde(default)]
    pub sibling_practices: Vec<super::SiblingPracticeCopy>,
}
