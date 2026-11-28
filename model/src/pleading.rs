//! Shared caption fields and final paper checks for Project pleadings.

use word::matter::{Filing, Matter};

/// A notation's own caption fields, from its frontmatter.
#[must_use]
pub fn filing_from(contents: &str) -> Filing {
    let field = |key: &str| {
        rules::frontmatter::extract(contents)
            .and_then(|fm| rules::frontmatter::field(fm, key))
            .filter(|value| !value.is_empty())
    };
    Filing {
        short_caption: field("short_caption").as_deref() == Some("true"),
        caption_title: field("caption_title").unwrap_or_default(),
        footer_title: field("footer_title").unwrap_or_default(),
        hearing_date: field("hearing_date"),
        hearing_time: field("hearing_time"),
        reservation_id: field("reservation_id"),
        document_code: field("document_code"),
    }
}

pub fn require_assigned(matter: &Matter) -> Result<(), String> {
    let pending: Vec<_> = [
        ("matter.department", matter.department.as_str()),
        ("matter.judicial_officer", matter.judicial_officer.as_str()),
    ]
    .into_iter()
    .filter_map(|(field, value)| {
        value
            .trim()
            .eq_ignore_ascii_case(word::matter::PENDING)
            .then_some(field)
    })
    .collect();
    if pending.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "this pleading cannot be exported: {} pending; record the court's assignment or use --preview",
            pending.join(", ")
        ))
    }
}

#[must_use]
pub fn certificate(
    title: &str,
    matter: &Matter,
    records: &[crate::project_service::ServiceRecord],
) -> Option<String> {
    let served: Vec<word::service::Served> = records
        .iter()
        .map(|record| word::service::Served {
            recipient: record.recipient.clone(),
            counsel_for: record.counsel_for.clone(),
            method: record.method.certificate_phrase().to_string(),
            served_on: court_date(record.served_on),
            address: record.address.clone(),
        })
        .collect();
    let executed_on = records
        .iter()
        .map(|record| record.served_on)
        .max()
        .map(court_date)
        .unwrap_or_default();
    let declarant_name = records
        .first()
        .map(|record| record.declarant.clone())
        .unwrap_or_default();
    let executed_at = matter.counsel.address.last().cloned().unwrap_or_default();
    let declarant = word::service::Declarant {
        name: declarant_name,
        business_address: matter.counsel.address.join(", "),
        executed_on,
        executed_at,
    };
    word::service::compose(title, &served, &declarant)
}

/// `2026-03-05` as a court writes it: `March 5, 2026`.
fn court_date(date: chrono::NaiveDate) -> String {
    word::matter::display_date(&date.format("%Y-%m-%d").to_string()).unwrap_or_default()
}
