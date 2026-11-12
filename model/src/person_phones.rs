//! Phone numbers: the closed label vocabulary and E.164 normalization.

use serde::{Deserialize, Serialize};

/// The kind of line a phone number is. Free-text labels rot into a
/// dozen spellings of the same fact, so the vocabulary is closed and
/// the schema's ASSERT holds it.
#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum PhoneLabel {
    /// A personal cell phone. The default.
    #[default]
    Mobile,
    /// A line answered during business hours.
    Work,
    /// A residential line.
    Home,
    /// A fax line.
    Fax,
    /// A line none of the other labels describe.
    Other,
}

impl PhoneLabel {
    /// The stored spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mobile => "mobile",
            Self::Work => "work",
            Self::Home => "home",
            Self::Fax => "fax",
            Self::Other => "other",
        }
    }

    /// The label named by its stored spelling, or `None` for anything
    /// else. The inverse of [`PhoneLabel::as_str`].
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "mobile" => Some(Self::Mobile),
            "work" => Some(Self::Work),
            "home" => Some(Self::Home),
            "fax" => Some(Self::Fax),
            "other" => Some(Self::Other),
            _ => None,
        }
    }
}

/// Normalize a phone number to E.164 (`+`, then 8–15 digits).
///
/// Punctuation (dashes, dots, spaces, parentheses) is stripped. A number
/// already carrying a leading `+` keeps its country code. A bare number
/// is read as North American — exactly ten digits, or eleven opening
/// with the US country code `1` — because that is the jurisdiction
/// Navigator's own practice runs in; anything else without a `+` is
/// refused rather than guessed at.
///
/// # Errors
///
/// The rejected input, unchanged, when the digits cannot be an E.164
/// number under those rules.
pub fn normalize_e164(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    let explicit_country = trimmed.starts_with('+');
    let mut digits = String::with_capacity(trimmed.len());
    for (index, ch) in trimmed.char_indices() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else if index == 0 && ch == '+' {
            // A leading country-code marker: remembered, not carried.
        } else if ch.is_whitespace() || "()-.".contains(ch) {
            // Formatting: stripped.
        } else {
            return Err(raw.to_string());
        }
    }

    match (explicit_country, digits.len()) {
        (true, 8..=15) => Ok(format!("+{digits}")),
        (false, 10) => Ok(format!("+1{digits}")),
        (false, 11) if digits.starts_with('1') => Ok(format!("+{digits}")),
        _ => Err(raw.to_string()),
    }
}
