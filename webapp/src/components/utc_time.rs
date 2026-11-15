//! An instant as a `<time>` element, labelled in UTC ("Nov 6, 2026, 22:29
//! UTC") with the machine-readable RFC 3339 value in `datetime`.

use dioxus::prelude::*;

/// An RFC 3339 instant as the label every firm surface shows, in UTC. A value
/// that does not parse is shown as stored rather than dropped.
#[must_use]
pub fn utc_label(at: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(at).map_or_else(
        |_| at.to_string(),
        |at| {
            at.with_timezone(&chrono::Utc)
                .format("%b %-d, %Y, %H:%M UTC")
                .to_string()
        },
    )
}

/// `at` (RFC 3339) as `<time datetime="{at}">{utc_label(at)}</time>`.
#[component]
pub fn UtcTime(at: String) -> Element {
    let label = utc_label(&at);
    rsx! {
        time { datetime: "{at}", "{label}" }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_an_offset_instant_in_utc() {
        assert_eq!(
            utc_label("2026-11-06T14:29:11-08:00"),
            "Nov 6, 2026, 22:29 UTC"
        );
        assert_eq!(utc_label("not a time"), "not a time");
    }
}
