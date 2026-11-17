//! Read `WhatsApp` text exports in source order and preserve their timestamp labels.

pub(crate) struct Entry<'a> {
    pub timestamp: &'a str,
    pub sender: &'a str,
    pub message: String,
}

pub(crate) fn entries(text: &str) -> Vec<Entry<'_>> {
    let mut entries: Vec<Entry<'_>> = Vec::new();
    for line in text.trim_start_matches('\u{feff}').lines() {
        if let Some((timestamp, rest)) = split_entry(line) {
            let (sender, message) = rest.split_once(": ").unwrap_or(("WhatsApp", rest));
            entries.push(Entry {
                timestamp,
                sender,
                message: message.to_owned(),
            });
        } else if let Some(previous) = entries.last_mut() {
            previous.message.push('\n');
            previous.message.push_str(line);
        } else if !line.is_empty() {
            entries.push(Entry {
                timestamp: "",
                sender: "",
                message: line.to_owned(),
            });
        }
    }
    entries
}

fn split_entry(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_start_matches(['\u{200e}', '\u{200f}']);
    let (timestamp, rest) = if let Some(line) = line.strip_prefix('[') {
        let (timestamp, rest) = line.split_once(']')?;
        (timestamp, rest.strip_prefix(' ')?)
    } else {
        line.split_once(" - ")?
    };
    let (date, time) = timestamp.split_once(", ")?;
    let date_fields: Vec<_> = date.split(['/', '.', '-']).collect();
    if date_fields.len() != 3
        || !date_fields.iter().all(|field| {
            (1..=4).contains(&field.len()) && field.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        return None;
    }
    let time = time
        .strip_suffix("AM")
        .or_else(|| time.strip_suffix("PM"))
        .unwrap_or(time)
        .trim_end();
    let time_fields: Vec<_> = time.split(':').collect();
    if !(2..=3).contains(&time_fields.len())
        || !time_fields.iter().all(|field| {
            (1..=2).contains(&field.len()) && field.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        return None;
    }
    Some((timestamp, rest))
}

/// Whether the source contains a supported timestamped `WhatsApp` entry.
#[must_use]
pub fn supported(text: &str) -> bool {
    text.lines()
        .any(|line| split_entry(line.trim_start_matches('\u{feff}')).is_some())
}

/// Render a page of source entries. Every source value and URL is escaped.
#[must_use]
pub fn timeline_html(text: &str, requested_page: usize, base: &str, download: &str) -> String {
    use crate::html_escape::escape_attr as escape;
    use std::fmt::Write as _;
    const PAGE_SIZE: usize = 100;
    let entries = entries(text);
    let page_count = entries.len().div_ceil(PAGE_SIZE).max(1);
    let index = requested_page.saturating_sub(1).min(page_count - 1);
    let base = escape(base);
    let download = escape(download);
    let mut body = format!(
        r#"<section><p>{} entries · Page {} of {page_count}</p><p class="muted">Timestamps and order come from the original export.</p>
<p><a href="{download}">Download original text export</a></p></section><nav aria-label="Timeline pages">"#,
        entries.len(),
        index + 1
    );
    if index > 0 {
        let _ = write!(body, r#"<a href="{base}?page={index}">Previous page</a>"#);
    }
    if index + 1 < page_count {
        let _ = write!(body, r#"<a href="{base}?page={}">Next page</a>"#, index + 2);
    }
    if page_count > 1 {
        let _ = write!(
            body,
            r#"<form method="get"><label for="page">Jump to page</label>
<input id="page" type="number" name="page" min="1" max="{page_count}" value="{}" required><button>Go</button></form>"#,
            index + 1,
        );
    }
    body.push_str("</nav>");
    for entry in entries.iter().skip(index * PAGE_SIZE).take(PAGE_SIZE) {
        let _ = write!(
            body,
            r#"<article class="whatsapp-entry" style="border:1px solid #cfdae6;border-radius:.6rem;padding:1rem;margin:.8rem 0"><strong>{}</strong> <span class="timestamp">{}</span><p style="white-space:pre-wrap;overflow-wrap:anywhere">{}</p></article>"#,
            escape(entry.sender),
            escape(entry.timestamp),
            escape(&entry.message)
        );
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whatsapp_export_keeps_multiline_and_equal_time_messages_in_source_order() {
        let rows = entries(
            "\u{200e}[11/3/26, 9:00:00\u{202f}AM] Pat: Hello\nsecond line\n\
             [11/3/26, 9:00:00\u{202f}AM] Sam: <Media omitted>\n\
             11/3/26, 09:15 - Encryption notice",
        );
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].message, "Hello\nsecond line");
        assert_eq!(rows[1].sender, "Sam");
        assert_eq!(rows[1].message, "<Media omitted>");
        assert_eq!(rows[2].message, "Encryption notice");
    }
}
