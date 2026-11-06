//! The configured signature visible in an unsent email draft.

use std::fmt;

#[derive(Debug, Eq, PartialEq)]
pub(super) enum SignatureError {
    MissingConfiguration,
    AmbiguousExistingBlock,
}

impl fmt::Display for SignatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::MissingConfiguration => {
                "configure NAVIGATOR_GMAIL_SIGNATURE with the attorney signature and \
                 confidentiality notice before drafting"
            }
            Self::AmbiguousExistingBlock => {
                "existing signature needs review: a matching signature block appears before other text; \
                 provide the substantive message body without an embedded current signature"
            }
        })
    }
}

impl std::error::Error for SignatureError {}

pub(super) fn with_signature(
    body: &str,
    configured: Option<&str>,
) -> Result<String, SignatureError> {
    let configured = configured
        .ok_or(SignatureError::MissingConfiguration)?
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let signature = configured.trim();
    let signature = signature
        .strip_prefix("-- \n")
        .or_else(|| signature.strip_prefix("--\n"))
        .unwrap_or(signature)
        .trim();
    if signature.is_empty() || signature == "--" {
        return Err(SignatureError::MissingConfiguration);
    }

    let normalized = body.replace("\r\n", "\n").replace('\r', "\n");
    let mut text = normalized.trim_end();
    while let Some(prefix) = text.strip_suffix(signature) {
        if !prefix.is_empty() && !prefix.ends_with('\n') {
            break;
        }
        text = prefix.trim_end();
        if text == "--" {
            text = "";
        } else if let Some(prefix) = text.strip_suffix("\n--") {
            text = prefix.trim_end();
        }
    }
    let embedded = text.match_indices(signature).any(|(start, _)| {
        let before = &text[..start];
        let after = &text[start + signature.len()..];
        (before.is_empty() || before.ends_with('\n'))
            && (after.is_empty() || after.starts_with('\n'))
    });
    if embedded {
        return Err(SignatureError::AmbiguousExistingBlock);
    }
    let separator = if text.is_empty() { "" } else { "\n\n" };
    Ok(format!("{text}{separator}-- \n{signature}"))
}

#[cfg(test)]
mod tests {
    use super::{with_signature, SignatureError};

    const SIGNATURE: &str = "Example Attorney\nConfidential: intended recipient only.";

    #[test]
    fn configured_signature_is_visible_once_after_repeated_updates() {
        let expected = format!("Please review.\n\n-- \n{SIGNATURE}");
        let first = with_signature("Please review.", Some(SIGNATURE)).unwrap();
        assert_eq!(first, expected);
        assert_eq!(with_signature(&first, Some(SIGNATURE)).unwrap(), expected);
        let duplicated = format!("{first}\n\n-- \n{SIGNATURE}");
        assert_eq!(
            with_signature(&duplicated, Some(SIGNATURE)).unwrap(),
            expected
        );
    }

    #[test]
    fn missing_or_blank_signature_configuration_is_explicit() {
        for configured in [None, Some(""), Some(" \r\n ")] {
            assert_eq!(
                with_signature("Please review.", configured),
                Err(SignatureError::MissingConfiguration)
            );
        }
    }

    #[test]
    fn signature_cleanup_preserves_prose_and_quoted_context() {
        let body = "Please contact Example Attorney\n\n> Earlier reply\n> Example Attorney\n> Confidential: intended recipient only.";
        assert_eq!(
            with_signature(body, Some(SIGNATURE)).unwrap(),
            format!("{body}\n\n-- \n{SIGNATURE}")
        );
        assert_eq!(
            with_signature("Contact Example Attorney", Some("Example Attorney")).unwrap(),
            "Contact Example Attorney\n\n-- \nExample Attorney"
        );
    }

    #[test]
    fn line_endings_and_an_existing_signature_delimiter_are_normalized() {
        let configured = "-- \r\nExample Attorney\r\nConfidential: intended recipient only.\r\n";
        let body = format!("Please review.\r\n\r\n{configured}");
        assert_eq!(
            with_signature(&body, Some(configured)).unwrap(),
            format!("Please review.\n\n-- \n{SIGNATURE}")
        );
    }
}
