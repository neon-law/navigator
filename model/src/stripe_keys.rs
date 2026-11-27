//! Metadata shared by the Firm secret command and its server.
use serde::{Deserialize, Serialize};
use strum::{Display, EnumString};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum Account {
    Nevada,
    Operating,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum KeyKind {
    Publishable,
    Secret,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum Mode {
    Sandbox,
    Live,
}

impl Mode {
    #[must_use]
    pub fn accepts(self, kind: KeyKind, value: &str) -> bool {
        let prefixes: &[_] = match (self, kind) {
            (Self::Sandbox, KeyKind::Publishable) => &["pk_test_"],
            (Self::Sandbox, KeyKind::Secret) => &["sk_test_", "rk_test_"],
            (Self::Live, KeyKind::Publishable) => &["pk_live_"],
            (Self::Live, KeyKind::Secret) => &["sk_live_", "rk_live_"],
        };
        prefixes.iter().any(|&prefix| {
            value.strip_prefix(prefix).is_some_and(|suffix| {
                !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_alphanumeric())
            })
        })
    }
}

#[derive(Serialize, Deserialize)]
pub struct KeyMetadata {
    pub account: Account,
    pub kind: KeyKind,
    /// Credentials stored before mode tracking have no recorded mode.
    pub mode: Option<Mode>,
    pub updated_at: String,
}

#[derive(Serialize, Deserialize)]
pub struct Directory {
    pub mode: Mode,
    pub keys: Vec<KeyMetadata>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_the_selected_kind_and_mode_accept_each_key() {
        for (value, expected_kind, expected_mode) in [
            ("pk_test_example", KeyKind::Publishable, Mode::Sandbox),
            ("sk_test_example", KeyKind::Secret, Mode::Sandbox),
            ("rk_test_example", KeyKind::Secret, Mode::Sandbox),
            ("pk_live_example", KeyKind::Publishable, Mode::Live),
            ("sk_live_example", KeyKind::Secret, Mode::Live),
            ("rk_live_example", KeyKind::Secret, Mode::Live),
        ] {
            for kind in [KeyKind::Publishable, KeyKind::Secret] {
                for mode in [Mode::Sandbox, Mode::Live] {
                    assert_eq!(
                        mode.accepts(kind, value),
                        (kind, mode) == (expected_kind, expected_mode)
                    );
                }
            }
        }
        for invalid in ["", "sk_test_", "rk_test_", "sk_test_a\nsk_test_b"] {
            assert!(!Mode::Sandbox.accepts(KeyKind::Secret, invalid));
        }
    }
}
