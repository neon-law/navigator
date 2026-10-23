//! A card on one practice's home pointing a reader to a sibling practice.
use serde::{Deserialize, Serialize};

/// One sibling-practice card, authored in a brand home catalog.
///
/// The card names its destination by [`crate::brand::BrandKey`] slug, never
/// by URL: a catalog file cannot know whether production or staging renders
/// it, so the brand crate resolves the address per deployment and drops a
/// card whose brand is not live.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiblingPracticeCopy {
    /// A [`crate::brand::BrandKey`] slug such as `vesta` or
    /// `delete-your-data`.
    pub brand: String,
    pub eyebrow: String,
    pub heading: String,
    pub body: String,
    pub cta_label: String,
    /// An optional paragraph under the body explaining why the offer
    /// matters, grounded in the `sources` that follow it.
    #[serde(default)]
    pub note: String,
    /// The authorities `note` relies on, each `[citation, url]`. Every
    /// citation carries its link; an empty list publishes no source line.
    #[serde(default)]
    pub sources: Vec<[String; 2]>,
}
