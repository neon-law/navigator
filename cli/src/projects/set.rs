//! Live-fill Project coordinates in the current repository's `navigator.yaml`.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};

use crate::{
    projects::{manifest, surfaces},
    remote,
};

const KEYS: [&str; 5] = [
    "lawyer_dri",
    "client_dri",
    "private_slack_channel",
    "private_notion_page",
    "calendar_id",
];

/// The coordinates `navigator project set` writes onto the live Project row
/// as well as, or instead of, `navigator.yaml`.
#[derive(Debug, Default, Clone, Copy)]
pub struct LiveFields<'a> {
    /// `--calendar`: written to `project.calendar_id` and the live row.
    pub calendar_id: Option<&'a str>,
    /// `--booking-url`: written to the live row only. The client lens reads
    /// it from there; the repository has no use for it.
    pub booking_url: Option<&'a str>,
}

pub async fn run(
    host_override: Option<&str>,
    project_override: Option<&str>,
    from_live: bool,
    supplied: [Option<&str>; 4],
    live_fields: LiveFields<'_>,
    firm: Option<&str>,
    brand: Option<&str>,
) -> ExitCode {
    match set(
        host_override,
        project_override,
        from_live,
        supplied,
        live_fields,
        firm,
        brand,
    )
    .await
    {
        Ok(changed) => {
            println!("updated Project settings: {}", changed.join(", "));
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator project set: {error:#}");
            ExitCode::from(1)
        }
    }
}

async fn set(
    host_override: Option<&str>,
    project_override: Option<&str>,
    from_live: bool,
    supplied: [Option<&str>; 4],
    live_fields: LiveFields<'_>,
    firm: Option<&str>,
    brand: Option<&str>,
) -> Result<Vec<String>> {
    if firm.is_some() != brand.is_some() {
        return Err(anyhow!("--firm and --brand must be supplied together"));
    }
    let live_patch = live_patch(live_fields)?;
    let path = Path::new(manifest::FILE);
    let contents = std::fs::read_to_string(path).context("read navigator.yaml")?;
    let parsed = manifest::parse(&contents).map_err(|error| anyhow!(error))?;
    let host = host_override
        .or(parsed.host.as_deref())
        .ok_or_else(|| anyhow!("navigator.yaml must declare project.host or pass --host"))?;
    let project = project_override
        .or(parsed.project.as_deref())
        .ok_or_else(|| {
            anyhow!("navigator.yaml must declare project.name or pass a Project code")
        })?;
    let [lawyer_dri, client_dri, private_slack_channel, private_notion_page] = supplied;
    let mut values: [Option<String>; 5] = [
        lawyer_dri,
        client_dri,
        private_slack_channel,
        private_notion_page,
        live_fields.calendar_id,
    ]
    .map(|value| value.map(|value| value.trim().to_string()));
    // `--calendar ""` clears the calendar: the live row through the patch,
    // the manifest by dropping the key, since `Y016` refuses an empty value.
    let clear_calendar = values[4].as_deref() == Some("");
    if from_live {
        fill_from_live(host, project, &mut values).await?;
    }
    if clear_calendar {
        values[4] = None;
    }
    if values.iter().all(Option::is_none) && live_patch.is_empty() && firm.is_none() {
        return Err(anyhow!(
            "pass --from-live, a coordinate flag, or --firm with --brand"
        ));
    }
    let mut updated = contents.clone();
    let mut changed = Vec::new();
    for (key, value) in KEYS.into_iter().zip(values) {
        let Some(value) = value else { continue };
        let current = match key {
            "lawyer_dri" => parsed.lawyer_dri.as_deref(),
            "client_dri" => parsed.client_dri.as_deref(),
            "private_slack_channel" => parsed.private_slack_channel.as_deref(),
            "private_notion_page" => parsed.private_notion_page.as_deref(),
            _ => parsed.calendar_id.as_deref(),
        };
        if current == Some(value.as_str()) {
            continue;
        }
        updated = replace_project_key(&updated, key, &value)?;
        changed.push(key.to_string());
    }
    if clear_calendar && parsed.calendar_id.is_some() {
        updated = remove_project_key(&updated, "calendar_id");
        changed.push("calendar_id".to_string());
    }
    let findings = manifest::lint_contents(path, &updated);
    if let Some(finding) = findings.into_iter().find(|finding| !finding.warning) {
        return Err(anyhow!(
            "{} ({}): {}",
            finding.code,
            finding.line,
            finding.message
        ));
    }
    if !live_patch.is_empty() {
        remote::patch_project_fields(host, project, &live_patch).await?;
        changed.extend(live_patch.keys().map(|key| format!("live Project {key}")));
    }
    if let (Some(firm), Some(brand)) = (firm, brand) {
        remote::set_project_firm_brand(host, project, firm, brand).await?;
        changed.push("live Project Firm and brand".into());
    }
    if !changed.is_empty() && updated != contents {
        std::fs::write(path, updated).context("write navigator.yaml")?;
    }
    if changed.is_empty() {
        changed.push("coordinates already current".to_string());
    }
    Ok(changed)
}

/// Fill each coordinate not supplied by a flag from the live Project row.
async fn fill_from_live(host: &str, project: &str, values: &mut [Option<String>; 5]) -> Result<()> {
    let (base, token) = remote::resolve(Some(host))?;
    let project_id = surfaces::resolve_project_id(&base, &token, project).await?;
    let live = surfaces::get_handles(&base, &token, project_id).await?;
    if !live.dangling_dri_person_ids.is_empty() {
        eprintln!(
            "navigator project set: {} DRI participation(s) on the live Project point to a \
             deleted Person and were skipped; reassign the DRI before relying on that field",
            live.dangling_dri_person_ids.len()
        );
    }
    let live_values = [
        live.lawyer_dri.first().cloned(),
        live.client_dri.first().cloned(),
        live.private_slack_channel,
        live.private_notion_page,
        live.calendar_id,
    ];
    for (target, live_value) in values.iter_mut().zip(live_values) {
        if target.is_none() {
            *target = live_value;
        }
    }
    Ok(())
}

/// The live-row fields `--calendar` and `--booking-url` name, checked before
/// any file or network write so a malformed value changes nothing. An empty
/// value clears the field on the live row.
fn live_patch(fields: LiveFields<'_>) -> Result<serde_json::Map<String, serde_json::Value>> {
    let mut patch = serde_json::Map::new();
    if let Some(calendar_id) = fields.calendar_id.map(str::trim) {
        if !calendar_id.is_empty() && !store::projects::is_valid_calendar_id(calendar_id) {
            return Err(anyhow!(store::projects::CALENDAR_ID_INVALID));
        }
        patch.insert("calendar_id".into(), calendar_id.into());
    }
    if let Some(booking_url) = fields.booking_url.map(str::trim) {
        if !booking_url.is_empty() && !store::projects::is_valid_resource_url(booking_url) {
            return Err(anyhow!(
                "--booking-url must be the appointment schedule's booking page, an http(s) address"
            ));
        }
        patch.insert("booking_url".into(), booking_url.into());
    }
    Ok(patch)
}

/// Replace or add a scalar in the nested `project:` map while retaining all
/// unrelated YAML text and comments.
fn replace_project_key(contents: &str, key: &str, value: &str) -> Result<String> {
    let serialized = serde_yaml::to_string(value)?.trim().to_string();
    let mut lines: Vec<String> = contents.lines().map(str::to_string).collect();
    let project_line = lines
        .iter()
        .position(|line| line.trim() == "project:")
        .ok_or_else(|| anyhow!("navigator.yaml needs a nested project: map"))?;
    let mut block_end = lines.len();
    for (index, line) in lines.iter().enumerate().skip(project_line + 1) {
        if !line.trim().is_empty() && !line.trim_start().starts_with('#') {
            let indent = line.len() - line.trim_start().len();
            if indent == 0 {
                block_end = index;
                break;
            }
        }
    }
    let prefix = format!("{key}:");
    if let Some(index) = (project_line + 1..block_end).find(|index| {
        let line = lines[*index].trim_start();
        line.starts_with(&prefix) && line[prefix.len()..].starts_with(char::is_whitespace)
    }) {
        let indent = lines[index].len() - lines[index].trim_start().len();
        lines[index] = format!("{}{key}: {serialized}", " ".repeat(indent));
    } else {
        lines.insert(project_line + 1, format!("  {key}: {serialized}"));
    }
    Ok(format!("{}\n", lines.join("\n")))
}

/// Drop one key from the nested `project:` map, leaving every other line.
fn remove_project_key(contents: &str, key: &str) -> String {
    let prefix = format!("{key}:");
    let mut in_project = false;
    let kept: Vec<&str> = contents
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            let indent = line.len() - trimmed.len();
            if !trimmed.is_empty() && indent == 0 {
                in_project = line.trim() == "project:";
                return true;
            }
            !(in_project && indent > 0 && trimmed.starts_with(&prefix))
        })
        .collect();
    format!("{}\n", kept.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::{live_patch, remove_project_key, replace_project_key, LiveFields};

    #[test]
    fn removes_only_the_nested_key() {
        let input = "version: 1\nproject:\n  name: acme\n  calendar_id: c_1@group.calendar.example.com\ncalendar_id: top\n";
        assert_eq!(
            remove_project_key(input, "calendar_id"),
            "version: 1\nproject:\n  name: acme\ncalendar_id: top\n"
        );
    }

    #[test]
    fn live_patch_carries_only_the_named_fields_and_refuses_bad_shapes() {
        assert!(live_patch(LiveFields::default()).unwrap().is_empty());
        let patch = live_patch(LiveFields {
            calendar_id: Some(" c_0123@group.calendar.example.com "),
            booking_url: Some("https://calendar.app.google/synthetic"),
        })
        .unwrap();
        assert_eq!(patch["calendar_id"], "c_0123@group.calendar.example.com");
        assert_eq!(
            patch["booking_url"],
            "https://calendar.app.google/synthetic"
        );
        let cleared = live_patch(LiveFields {
            booking_url: Some(""),
            ..LiveFields::default()
        })
        .unwrap();
        assert_eq!(
            cleared["booking_url"], "",
            "an empty value clears the field"
        );
        assert!(live_patch(LiveFields {
            calendar_id: Some("https://calendar.google.com/calendar/r"),
            ..LiveFields::default()
        })
        .is_err());
        assert!(live_patch(LiveFields {
            booking_url: Some("javascript:alert(1)"),
            ..LiveFields::default()
        })
        .is_err());
    }

    #[test]
    fn replaces_nested_value_and_preserves_unrelated_comments() {
        let input = "version: 1\nproject:\n  host: https://example.com # deployment\n  name: acme\n# keep\n";
        let output = replace_project_key(input, "lawyer_dri", "lawyer@example.com").unwrap();
        assert!(output.contains("  lawyer_dri: lawyer@example.com\n"));
        assert!(output.contains("# keep\n"));
        let replaced = replace_project_key(&output, "lawyer_dri", "new@example.com").unwrap();
        assert!(replaced.contains("lawyer_dri: new@example.com"));
        assert!(!replaced.contains("lawyer@example.com"));
    }
}
