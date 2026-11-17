//! Letterhead fields and resolved rendering data.
use serde::{Deserialize, Serialize};

/// Which mark heads the letterhead: the Brand's wordmark in letterspaced
/// capitals, or its uploaded logo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LetterheadMark {
    #[default]
    Wordmark,
    Logo,
}

impl LetterheadMark {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Wordmark => "wordmark",
            Self::Logo => "logo",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "wordmark" => Some(Self::Wordmark),
            "logo" => Some(Self::Logo),
            _ => None,
        }
    }
}

/// What an admin set on a Brand's letterhead. `None` means "fall back to the
/// Brand" (see [`resolve`]).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LetterheadFields {
    pub wordmark: Option<String>,
    #[serde(default)]
    pub mark: LetterheadMark,
    pub phone: Option<String>,
    pub email: Option<String>,
    /// The canonical public HTTPS website.
    pub site_url: Option<String>,
    /// The postal address, one line per row.
    pub postal_address: Option<String>,
    /// One of the Brand's uploaded font families.
    pub font_family: Option<String>,
}

/// The letterhead a renderer frames a letter with: every line resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrandLetterhead {
    pub brand_key: String,
    /// The wordmark across the top, in natural case.
    pub wordmark: String,
    pub mark: LetterheadMark,
    /// The Brand's uploaded logo object, when it has one. A renderer asked
    /// for [`LetterheadMark::Logo`] without one prints the wordmark.
    pub logo_object_key: Option<String>,
    /// The legal name of the Firm that owns the Brand.
    pub firm_legal_name: Option<String>,
    pub phone: Option<String>,
    pub email: Option<String>,
    /// The site as a reader types it, e.g. `www.example.com`.
    pub web: Option<String>,
    pub postal_address: Option<String>,
    /// The letterhead font family and its uploaded `.woff2` object. `None`
    /// is the renderer's own default face.
    pub font_family: Option<String>,
    pub font_object_key: Option<String>,
    /// The stored row's last edit as RFC 3339 — the revision a render's
    /// provenance names. `None` when no row was read (an in-memory
    /// preview resolve, or no admin has saved a row yet).
    pub updated_at: Option<String>,
}
