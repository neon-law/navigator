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
    timeline_with_images(text, requested_page, base, download, &[])
}

/// A dated image with session-protected links.
pub struct TimelineImage {
    pub name: String,
    pub at: chrono::DateTime<chrono::Utc>,
    pub original_time: bool,
    pub preview: String,
    pub download: String,
}

fn message_time(timestamp: &str) -> Option<chrono::NaiveDateTime> {
    let normalized = timestamp.replace(['\u{202f}', '\u{a0}'], " ");
    for date in ["%m/%d/%y", "%m/%d/%Y", "%Y-%m-%d", "%d.%m.%y", "%d.%m.%Y"] {
        for time in ["%I:%M:%S %p", "%I:%M %p", "%H:%M:%S", "%H:%M"] {
            if let Ok(at) =
                chrono::NaiveDateTime::parse_from_str(&normalized, &format!("{date}, {time}"))
            {
                return Some(at);
            }
        }
    }
    None
}

/// Render the chat and filed images together without changing chat source order.
#[must_use]
pub fn timeline_with_images(
    text: &str,
    requested_page: usize,
    base: &str,
    download: &str,
    images: &[TimelineImage],
) -> String {
    use crate::html_escape::escape_attr as escape;
    use std::fmt::Write as _;
    const PAGE_SIZE: usize = 100;
    const CARD: &str = "border:1px solid #cfdae6;border-radius:.6rem;padding:1rem;margin:.8rem 0";
    enum Item<'a> {
        Chat(&'a Entry<'a>),
        Image(&'a TimelineImage),
    }
    let entries = entries(text);
    let mut images: Vec<_> = images.iter().collect();
    images.sort_by_key(|image| image.at);
    let mut images = images.into_iter().peekable();
    let mut items = Vec::with_capacity(entries.len() + images.len());
    for entry in &entries {
        if images.peek().is_some() {
            if let Some(at) = message_time(entry.timestamp) {
                while let Some(image) = images.next_if(|image| image.at.naive_utc() <= at) {
                    items.push(Item::Image(image));
                }
            }
        }
        items.push(Item::Chat(entry));
    }
    items.extend(images.map(Item::Image));
    let page_count = items.len().div_ceil(PAGE_SIZE).max(1);
    let index = requested_page.saturating_sub(1).min(page_count - 1);
    let base = escape(base);
    let download = escape(download);
    let mut body = format!(
        r#"<section><p>{} entries · Page {} of {page_count}</p><p class="muted">Chat timestamps and order come from the original export.</p>
<p><a href="{download}">Download original text export</a></p></section><nav aria-label="Timeline pages">"#,
        items.len(),
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
    if items.len() > entries.len() {
        body.push_str(r#"<p class="muted">Image times are shown in UTC. Chat timestamps are quoted as exported and compared as UTC for placement.</p>"#);
    }
    for item in items.iter().skip(index * PAGE_SIZE).take(PAGE_SIZE) {
        match item {
            Item::Chat(entry) => {
                let _ = write!(
                    body,
                    r#"<article class="whatsapp-entry" style="{CARD}"><strong>{}</strong> <span class="timestamp">{}</span><p style="white-space:pre-wrap;overflow-wrap:anywhere">{}</p></article>"#,
                    escape(entry.sender),
                    escape(entry.timestamp),
                    escape(&entry.message)
                );
            }
            Item::Image(image) => {
                let date_label = if image.original_time {
                    "Original time"
                } else {
                    "Filed"
                };
                let _ = write!(
                    body,
                    r#"<article class="whatsapp-entry" style="{CARD}"><strong>{}</strong> <span class="timestamp">{date_label} {}</span><p><img src="{}" alt="{}" loading="lazy" decoding="async" style="max-width:100%;max-height:34rem;object-fit:contain"></p><p><a href="{}">Download image</a></p></article>"#,
                    escape(&image.name),
                    image.at.format("%Y-%m-%d %H:%M UTC"),
                    escape(&image.preview),
                    escape(&image.name),
                    escape(&image.download),
                );
            }
        }
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filed_image_appears_between_chat_entries_and_carries_its_date_provenance() {
        let image = TimelineImage {
            name: "Filed <image>".into(),
            at: "2026-11-03T09:02:00Z".parse().unwrap(),
            original_time: false,
            preview: "/media/image?inline=1".into(),
            download: "/media/image".into(),
        };
        let html = timeline_with_images(
            "[11/3/26, 9:01:00 AM] Pat: Earlier\n[11/3/26, 9:03:00 AM] Sam: Later",
            1,
            "/timeline",
            "/export",
            &[image],
        );
        let image_at = html.find("Filed &lt;image&gt;").unwrap();
        assert!(html.find("Earlier").unwrap() < image_at);
        assert!(image_at < html.find("Later").unwrap());
        assert!(html.contains("Filed 2026-11-03 09:02 UTC"));
        assert!(html.contains("loading=\"lazy\""));
    }

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
