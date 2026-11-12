//! Validation an entity write runs before it reaches the store.

/// Refuse entity metadata whose `ein` is not formatted `NN-NNNNNNN`.
///
/// # Errors
/// The refusal, as the sentence shown to the operator.
pub fn validate_metadata(
    metadata: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Result<(), &'static str> {
    if let Some(ein) = metadata.and_then(|m| m.get("ein")) {
        let valid = ein.as_str().is_some_and(|value| {
            value.len() == 10
                && value.bytes().enumerate().all(|(i, byte)| {
                    if i == 2 {
                        byte == b'-'
                    } else {
                        byte.is_ascii_digit()
                    }
                })
        });
        if !valid {
            return Err("EIN must be a string formatted as NN-NNNNNNN.");
        }
    }
    Ok(())
}
