//! Read WhatsApp text exports in source order and preserve their timestamp labels.

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
