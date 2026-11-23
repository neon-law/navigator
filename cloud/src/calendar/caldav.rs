//! Read the private calendar's iCalendar resources with the delegated owner's token.
use super::{CalendarError, GoogleCalendar};
use roxmltree::{Document, Node};

const DAV: &str = "DAV:";
const CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
const REPORT: &str = r#"<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
<d:prop><d:getetag/><c:calendar-data/></d:prop>
<c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT"/></c:comp-filter></c:filter>
</c:calendar-query>"#;

/// One `CalDAV` resource revision, including recurrence exceptions and time zones.
#[derive(Debug, Clone)]
pub struct CalendarEventBackup {
    pub href: String,
    pub etag: String,
    pub ics: String,
}

impl GoogleCalendar {
    /// Export all event resources on the verified Project Calendar as iCalendar.
    pub(super) async fn caldav_events(
        &self,
        calendar_id: &str,
    ) -> Result<Vec<CalendarEventBackup>, CalendarError> {
        let mut url = url::Url::parse("https://apidata.googleusercontent.com/caldav/v2/")
            .expect("invariant: literal CalDAV URL");
        url.path_segments_mut()
            .expect("invariant: HTTPS URL")
            .pop_if_empty()
            .extend([calendar_id, "events"]);
        let action = "backing up Project Calendar events";
        let response = Self::checked(
            self.http
                .request(
                    reqwest::Method::from_bytes(b"REPORT").expect("invariant: valid HTTP method"),
                    url,
                )
                .bearer_auth(self.token().await?)
                .header("Depth", "1")
                .header(
                    reqwest::header::CONTENT_TYPE,
                    "application/xml; charset=utf-8",
                )
                .body(REPORT)
                .send()
                .await,
            action,
        )?;
        if response.status() != reqwest::StatusCode::MULTI_STATUS {
            return Err(CalendarError::InvalidEventBackup);
        }
        let body = response
            .bytes()
            .await
            .map_err(|source| CalendarError::Response { action, source })?;
        parse_report(std::str::from_utf8(&body).map_err(|_| CalendarError::InvalidEventBackup)?)
    }
}

fn child<'a, 'input>(
    node: Node<'a, 'input>,
    ns: &str,
    name: &str,
) -> Result<Node<'a, 'input>, CalendarError> {
    node.children()
        .find(|n| n.has_tag_name((ns, name)))
        .ok_or(CalendarError::InvalidEventBackup)
}

fn parse_report(body: &str) -> Result<Vec<CalendarEventBackup>, CalendarError> {
    let doc = Document::parse(body).map_err(|_| CalendarError::InvalidEventBackup)?;
    let root = doc.root_element();
    if !root.has_tag_name((DAV, "multistatus")) {
        return Err(CalendarError::InvalidEventBackup);
    }
    let mut events = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for response in root.children().filter(Node::is_element) {
        if !response.has_tag_name((DAV, "response")) {
            return Err(CalendarError::InvalidEventBackup);
        }
        let text = |n: Node<'_, '_>| {
            n.text()
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned)
                .ok_or(CalendarError::InvalidEventBackup)
        };
        let href = text(child(response, DAV, "href")?)?;
        if !seen.insert(href.clone()) {
            return Err(CalendarError::InvalidEventBackup);
        }
        let mut etag = None;
        let mut data = None;
        for propstat in response
            .children()
            .filter(|n| n.has_tag_name((DAV, "propstat")))
        {
            let status = text(child(propstat, DAV, "status")?)?;
            if status.split_ascii_whitespace().nth(1) != Some("200") {
                return Err(CalendarError::InvalidEventBackup);
            }
            for prop in child(propstat, DAV, "prop")?.children() {
                if prop.has_tag_name((DAV, "getetag")) {
                    etag = Some(text(prop)?);
                }
                if prop.has_tag_name((CALDAV, "calendar-data")) {
                    data = Some(text(prop)?);
                }
            }
        }
        let ics = data.ok_or(CalendarError::InvalidEventBackup)?;
        if ics.lines().next() != Some("BEGIN:VCALENDAR")
            || ics.lines().last() != Some("END:VCALENDAR")
            || !ics.lines().any(|line| line == "BEGIN:VEVENT")
        {
            return Err(CalendarError::InvalidEventBackup);
        }
        // XML normalizes line endings. Restore iCalendar's CRLF without unfolding content lines.
        let mut ics = ics.lines().collect::<Vec<_>>().join("\r\n");
        ics.push_str("\r\n");
        events.push(CalendarEventBackup {
            href,
            etag: etag.ok_or(CalendarError::InvalidEventBackup)?,
            ics,
        });
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_private_event_revisions_and_refuses_partial_results() {
        let report = r#"<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
<D:response><D:href>/item.ics</D:href><D:propstat><D:status>HTTP/1.1 200 OK</D:status><D:prop>
<D:getetag>"v2"</D:getetag><C:calendar-data>BEGIN:VCALENDAR
BEGIN:VEVENT
UID:series
RRULE:FREQ=WEEKLY
SUMMARY:A &amp; B
END:VEVENT
END:VCALENDAR
</C:calendar-data></D:prop></D:propstat></D:response></D:multistatus>"#;
        let result = parse_report(report).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].href, "/item.ics");
        assert_eq!(result[0].etag, "\"v2\"");
        assert!(result[0]
            .ics
            .contains("RRULE:FREQ=WEEKLY\r\nSUMMARY:A & B\r\n"));
        assert!(parse_report(&report.replace("200 OK", "403 Forbidden")).is_err());
        assert!(
            parse_report(r#"<d:multistatus xmlns:d="DAV:"><d:error/></d:multistatus>"#).is_err()
        );
        assert!(parse_report(r#"<d:multistatus xmlns:d="DAV:"/>"#)
            .unwrap()
            .is_empty());
    }
}
