//! Live-fill Project coordinates in the current repository's `navigator.yaml`.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};

use crate::{
    projects::{manifest, surfaces},
    remote,
};

const KEYS: [&str; 6] = [
    "lawyer_dri",
    "client_dri",
    "private_slack_channel",
    "private_notion_page",
    "calendar_id",
    "drive_folder_id",
];

/// The coordinates `navigator project set` writes onto the live Project row
/// as well as, or instead of, `navigator.yaml`.
#[derive(Debug, Default, Clone, Copy)]
pub struct LiveFields<'a> {
    /// `--calendar`: written to `project.calendar_id` and the live row.
    pub calendar_id: Option<&'a str>,
    /// `--drive-folder`: written to `project.drive_folder_id` and the live row.
    pub drive_folder_id: Option<&'a str>,
    /// `--booking-url`: written to the live row only. The client lens reads
    /// it from there; the repository has no use for it.
    pub booking_url: Option<&'a str>,
    /// `--jurisdiction`: written to the live row only. The onboarding Clause
    /// binding (`project.onboarding_jurisdiction`) reads the live value at
    /// render time; the repository has no use for it.
    pub jurisdiction: Option<&'a str>,
}

pub async fn run(
    host_override: Option<&str>,
    project_override: Option<&str>,
    from_live: bool,
    supplied: [Option<&str>; 4],
    live_fields: LiveFields<'_>,
    firm: Option<&str>,
    brand: Option<&str>,
    dry_run: bool,
) -> ExitCode {
    match set(
        host_override,
        project_override,
        from_live,
        supplied,
        live_fields,
        firm,
        brand,
        dry_run,
    )
    .await
    {
        Ok(changed) => {
            if dry_run {
                println!(
                    "dry run — nothing written; would update: {}",
                    changed.join(", ")
                );
            } else {
                println!("updated Project settings: {}", changed.join(", "));
            }
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
    dry_run: bool,
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
    let mut values: [Option<String>; 6] = [
        lawyer_dri,
        client_dri,
        private_slack_channel,
        private_notion_page,
        live_fields.calendar_id,
        live_fields.drive_folder_id,
    ]
    .map(|value| value.map(|value| value.trim().to_string()));
    // `--calendar ""` clears the calendar and `--drive-folder ""` clears the
    // folder: the live row through the patch, the manifest by dropping the
    // key, since `Y016` refuses an empty value.
    let clear_calendar = values[4].as_deref() == Some("");
    let clear_drive_folder = values[5].as_deref() == Some("");
    if from_live {
        fill_from_live(host, project, &mut values).await?;
    }
    if clear_calendar {
        values[4] = None;
    }
    if clear_drive_folder {
        values[5] = None;
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
            "drive_folder_id" => parsed.drive_folder_id.as_deref(),
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
    if clear_drive_folder && parsed.drive_folder_id.is_some() {
        updated = remove_project_key(&updated, "drive_folder_id");
        changed.push("drive_folder_id".to_string());
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
    if dry_run {
        // Everything past this point is a write: the manifest file, the live
        // row patch, the Firm reassignment. The plan names all of them and
        // stops, so the preview is exactly what a non-dry run would do.
        return Ok(dry_run_plan(changed, &live_patch, firm.is_some()));
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
async fn fill_from_live(host: &str, project: &str, values: &mut [Option<String>; 6]) -> Result<()> {
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
        live.drive_folder_id,
    ];
    for (target, live_value) in values.iter_mut().zip(live_values) {
        if target.is_none() {
            *target = live_value;
        }
    }
    Ok(())
}

/// The plan a `--dry-run` run reports: every field the run would have
/// touched, manifest keys and live-row fields alike, without performing any
/// of it. Pure so the preview and the writing path cannot drift apart.
fn dry_run_plan(
    changed: Vec<String>,
    live_patch: &serde_json::Map<String, serde_json::Value>,
    firm_and_brand: bool,
) -> Vec<String> {
    let mut plan = changed;
    plan.extend(live_patch.keys().map(|key| format!("live Project {key}")));
    if firm_and_brand {
        plan.push("live Project Firm and brand".to_string());
    }
    if plan.is_empty() {
        plan.push("coordinates already current".to_string());
    }
    plan
}

/// The live-row fields `--calendar`, `--booking-url`, `--jurisdiction`, and
/// `--drive-folder`
/// name, checked before any file or network write so a malformed value
/// changes nothing. An empty value clears the field on the live row.
fn live_patch(fields: LiveFields<'_>) -> Result<serde_json::Map<String, serde_json::Value>> {
    let mut patch = serde_json::Map::new();
    if let Some(calendar_id) = fields.calendar_id.map(str::trim) {
        if !calendar_id.is_empty() && !store::projects::is_valid_calendar_id(calendar_id) {
            return Err(anyhow!(store::projects::CALENDAR_ID_INVALID));
        }
        patch.insert("calendar_id".into(), calendar_id.into());
    }
    if let Some(drive_folder_id) = fields.drive_folder_id.map(str::trim) {
        if !drive_folder_id.is_empty()
            && !store::projects::is_valid_drive_folder_id(drive_folder_id)
        {
            return Err(anyhow!(store::projects::DRIVE_FOLDER_ID_INVALID));
        }
        patch.insert("drive_folder_id".into(), drive_folder_id.into());
    }
    if let Some(booking_url) = fields.booking_url.map(str::trim) {
        if !booking_url.is_empty() && !store::projects::is_valid_resource_url(booking_url) {
            return Err(anyhow!(
                "--booking-url must be the appointment schedule's booking page, an http(s) address"
            ));
        }
        patch.insert("booking_url".into(), booking_url.into());
    }
    if let Some(jurisdiction) = fields.jurisdiction.map(str::trim) {
        patch.insert("onboarding_jurisdiction".into(), jurisdiction.into());
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
            drive_folder_id: Some(" 1QaBcD_2-efG "),
            booking_url: Some("https://calendar.app.google/synthetic"),
            ..LiveFields::default()
        })
        .unwrap();
        assert_eq!(patch["calendar_id"], "c_0123@group.calendar.example.com");
        assert_eq!(patch["drive_folder_id"], "1QaBcD_2-efG");
        assert_eq!(
            patch["booking_url"],
            "https://calendar.app.google/synthetic"
        );
        let cleared = live_patch(LiveFields {
            booking_url: Some(""),
            drive_folder_id: Some(""),
            ..LiveFields::default()
        })
        .unwrap();
        assert_eq!(
            cleared["booking_url"], "",
            "an empty value clears the field"
        );
        assert_eq!(
            cleared["drive_folder_id"], "",
            "an empty value clears the folder, too"
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
        for bad_drive_folder in [
            // Not an id: a link, a path, a query, whitespace, a comma
            // (the Drive list separator).
            "https://drive.google.com/drive/folders/1QaBcD",
            "folders/1QaBcD",
            "1QaBcD?resourcekey=x",
            "1QaBcD_2-efG,1AbCdE",
        ] {
            assert!(
                live_patch(LiveFields {
                    drive_folder_id: Some(bad_drive_folder),
                    ..LiveFields::default()
                })
                .is_err(),
                "{bad_drive_folder} must be refused"
            );
        }
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

    /// LAW-213: `--drive-folder` follows `--calendar` into the nested
    /// `project:` map, replacing an existing value in place and preserving
    /// every unrelated line and comment.
    #[test]
    fn the_drive_folder_is_written_into_the_manifest_like_the_calendar() {
        let input = "version: v26.10.6\nproject:\n  host: https://example.com # deployment\n  name: acme\n  drive_folder_id: 1OLDFOLDER\n# keep\n";
        let parsed = crate::projects::manifest::parse(input).unwrap();
        let output = replace_project_key(input, "drive_folder_id", "1QaBcD_2-efG").unwrap();
        assert!(output.contains("  drive_folder_id: 1QaBcD_2-efG\n"));
        assert!(output.contains("# keep\n"));
        assert!(!output.contains("1OLDFOLDER"));
        let reparsed = crate::projects::manifest::parse(&output).unwrap();
        assert_eq!(reparsed.drive_folder_id.as_deref(), Some("1QaBcD_2-efG"));
        assert_eq!(parsed.host, reparsed.host);
    }

    /// The dry-run plan names every write the real run would make — manifest
    /// keys, each live-row field, the Firm reassignment — and reports a
    /// no-op run as already current rather than an empty success.
    #[test]
    fn the_dry_run_plan_names_every_write_the_real_run_would_make() {
        let mut patch = serde_json::Map::new();
        patch.insert(
            "calendar_id".into(),
            "c_1@group.calendar.example.com".into(),
        );
        patch.insert("drive_folder_id".into(), "1QaBcD_2-efG".into());
        assert_eq!(
            super::dry_run_plan(
                vec!["lawyer_dri".to_string(), "drive_folder_id".to_string()],
                &patch,
                true,
            ),
            [
                "lawyer_dri",
                "drive_folder_id",
                "live Project calendar_id",
                "live Project drive_folder_id",
                "live Project Firm and brand",
            ]
        );
        assert_eq!(
            super::dry_run_plan(Vec::new(), &serde_json::Map::new(), false),
            ["coordinates already current"],
            "a run with nothing to change says so"
        );
    }
}
