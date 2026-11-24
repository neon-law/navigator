//! Queue the next published stable release for a Project whose main refs agree.
use chrono::NaiveDate;
use dbos::{EnqueueOptions, DBOS};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const APP: &str = "navigator-project-maintenance";
pub const RELEASE_QUEUE: &str = "project-release-updates";
pub const REPAIR_QUEUE: &str = "project-release-repairs";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub project: Uuid,
    pub code: String,
    pub primary: String,
    pub mirror: String,
    pub base: String,
    pub release: String,
}
impl Job {
    #[must_use]
    pub fn key(&self, stage: &str) -> String {
        format!("{stage}:{}:{}:{}", self.project, self.release, self.base)
    }
}

pub(super) fn stable_date(tag: &str) -> Option<NaiveDate> {
    let mut parts = tag.split('.');
    let year = parts.next()?;
    if year.len() != 2 {
        return None;
    }
    let date = NaiveDate::from_ymd_opt(
        2000 + year.parse::<i32>().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    )?;
    (parts.next().is_none() && date.format("%y.%-m.%-d").to_string() == tag).then_some(date)
}

/// A candidate may advance to the stable release published for the same day.
pub(super) fn release_baseline(pin: &str) -> Option<NaiveDate> {
    let pin = pin.strip_prefix('v').unwrap_or(pin);
    if let Some((tag, candidate)) = pin.split_once("-rc.") {
        candidate.parse::<u32>().ok()?;
        return stable_date(tag)?.pred_opt();
    }
    stable_date(pin)
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    published_at: Option<String>,
}

pub(super) async fn releases_after(oldest: NaiveDate) -> anyhow::Result<Vec<String>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent("navigator-project-maintenance")
        .build()?;
    let mut releases = Vec::new();
    for page in 1.. {
        let page_rows = client
            .get("https://api.github.com/repos/neon-law/navigator/releases")
            .query(&[("per_page", 100), ("page", page)])
            .send()
            .await?
            .error_for_status()?
            .json::<Vec<Release>>()
            .await?;
        let exhausted = page_rows.len() < 100;
        for row in page_rows {
            if row.draft || row.prerelease || row.published_at.is_none() {
                continue;
            }
            if let Some(date) = stable_date(&row.tag_name) {
                if date >= oldest {
                    releases.push((date, row.tag_name));
                }
            }
        }
        if exhausted {
            break;
        }
    }
    releases.sort();
    releases.dedup();
    Ok(releases.into_iter().map(|(_, tag)| tag).collect())
}

pub(super) async fn enqueue(dbos: &DBOS, job: &Job) -> dbos::Result<String> {
    let id = job.key("release");
    let options = EnqueueOptions {
        app_name: Some(APP),
        workflow_id: Some(&id),
        ..EnqueueOptions::new(RELEASE_QUEUE)
    };
    dbos.enqueue_with::<_, (), dbos::EngineOnly>("ApplyProjectRelease", job, options)
        .await?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn production_only_accepts_canonical_published_date_tags() {
        assert!(stable_date("26.11.24").is_some());
        assert_eq!(release_baseline("v26.11.24-rc.1"), stable_date("26.11.23"));
        for tag in ["26.11.24-rc.1", "26.02.31", "latest", "26.1.1/other"] {
            assert!(stable_date(tag).is_none());
        }
    }
}
