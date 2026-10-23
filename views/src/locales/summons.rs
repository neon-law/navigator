//! Summons Defense offer and government-source guidance.

use serde::{Deserialize, Serialize};

/// Authored copy for the OATH hearing practice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SummonsCopy {
    pub eyebrow: String,
    pub nav: Vec<[String; 2]>,
    pub hero_note: String,
    pub fees_heading: String,
    pub fees: Vec<[String; 4]>,
    pub fees_note: String,
    pub process_heading: String,
    pub steps: Vec<[String; 2]>,
    pub guides_heading: String,
    pub guides_intro: String,
    pub guides: Vec<[String; 5]>,
    pub cases_heading: String,
    pub cases_note: String,
    pub cases: Vec<[String; 4]>,
    pub resources_heading: String,
    pub resources: Vec<[String; 2]>,
    pub intake_heading: String,
    pub intake_body: String,
    pub intake_checklist: Vec<String>,
    pub intake_note: String,
    pub faq_heading: String,
    pub faqs: Vec<[String; 2]>,
    pub footer_note: String,
    pub reviewed: String,
}
