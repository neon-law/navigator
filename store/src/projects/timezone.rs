//! The Project calendar's local default for new scheduling decisions.

use chrono_tz::{America, Tz};

use super::{Project, ProjectStoreError};
use crate::application_store::ApplicationStore;

fn jurisdiction_zone(label: &str) -> Option<Tz> {
    let label = label.trim();
    include_str!("jurisdiction_timezones.tsv")
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .find_map(|(code, rest)| {
            let (name, tz) = rest.split_once('\t')?;
            (code.eq_ignore_ascii_case(label) || name.eq_ignore_ascii_case(label))
                .then(|| tz.parse().ok())
                .flatten()
        })
}

fn court_zone(forum: &str, jurisdiction: Option<&str>) -> Option<Tz> {
    let mut matches = crate::court_seeds::COURT_SEEDS.iter().filter(|court| {
        (court.key.eq_ignore_ascii_case(forum.trim())
            || court.name.eq_ignore_ascii_case(forum.trim()))
            && jurisdiction.is_none_or(|label| {
                label.trim().eq_ignore_ascii_case(&court.jurisdiction)
                    || crate::courts::jurisdiction_name(&court.jurisdiction)
                        .is_some_and(|name| name.eq_ignore_ascii_case(label.trim()))
            })
    });
    let court = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    match (court.jurisdiction.as_str(), court.county.as_deref()) {
        ("NV", Some("Clark" | "Washoe")) => Some(America::Los_Angeles),
        _ => jurisdiction_zone(&court.jurisdiction),
    }
}

fn venue_zone<'a>(venues: impl Iterator<Item = (Option<&'a str>, Option<&'a str>)>) -> Option<Tz> {
    let mut selected = None;
    for (forum, jurisdiction) in venues {
        let current = forum
            .and_then(|forum| court_zone(forum, jurisdiction))
            .or_else(|| jurisdiction.and_then(jurisdiction_zone))?;
        if selected.is_some_and(|other| other != current) {
            return None;
        }
        selected = Some(current);
    }
    selected
}

/// An explicit Project setting, its governing jurisdiction, the zone shared by
/// its current case venues, or Eastern Time. This only selects a default.
///
/// # Errors
/// Reports an invalid stored zone or a failed jurisdiction or case read.
pub async fn calendar_timezone(
    db: &ApplicationStore,
    project: &Project,
) -> Result<Tz, ProjectStoreError> {
    if let Some(zone) = &project.calendar_timezone {
        return zone
            .parse()
            .map_err(|_| ProjectStoreError::InvalidCalendarTimezone);
    }
    if let Some(id) = project.jurisdiction_id {
        if let Some(zone) = crate::jurisdictions::find_by_id(db, id)
            .await?
            .and_then(|jurisdiction| jurisdiction_zone(&jurisdiction.code))
        {
            return Ok(zone);
        }
    }
    let cases = crate::cases::for_project(db, project.id).await?;
    Ok(venue_zone(
        cases
            .iter()
            .filter(|case| case.status != "closed")
            .map(|case| (case.forum.as_deref(), case.jurisdiction.as_deref())),
    )
    .unwrap_or(America::New_York))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ambiguous_jurisdictions_need_consistent_specific_venues() {
        assert_eq!(jurisdiction_zone("California"), Some(America::Los_Angeles));
        assert_eq!(jurisdiction_zone("NV"), None);
        assert_eq!(jurisdiction_zone("USA"), None);
        assert_eq!(jurisdiction_zone("JPN"), Some(chrono_tz::Asia::Tokyo));
        assert_eq!(
            venue_zone([(Some("Eighth Judicial District Court"), Some("Nevada"))].into_iter()),
            Some(America::Los_Angeles)
        );
        assert_eq!(
            venue_zone([(None, Some("IL")), (None, Some("CA"))].into_iter()),
            None
        );
        assert_eq!(
            venue_zone([(None, Some("IL")), (None, None)].into_iter()),
            None
        );
    }

    #[test]
    fn every_shipped_jurisdiction_zone_is_a_supported_iana_name() {
        for line in include_str!("jurisdiction_timezones.tsv")
            .lines()
            .filter(|line| !line.starts_with('#'))
        {
            let (code, _) = line.split_once('\t').unwrap();
            assert!(jurisdiction_zone(code).is_some(), "{code}");
        }
    }
}
