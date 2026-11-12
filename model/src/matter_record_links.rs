//! The pure half of `store::matter_record_links`.

/// `value` is one of `allowed`, or the refusal naming the field.
pub fn one_of(name: &str, value: &str, allowed: &[&str]) -> Result<String, LinkRefusal> {
    let value = value.trim();
    if allowed.contains(&value) {
        Ok(value.to_string())
    } else {
        Err(format!("{name} must be one of {}", allowed.join(", ")))
    }
}

/// Why a link check refused a write. Each module folds this into its own
/// `Invalid` error so the caller reads one plain sentence.
pub type LinkRefusal = String;
