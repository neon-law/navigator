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
    pub timezone: Option<&'a str>,
    /// `--jurisdiction`: written to the live row only. The onboarding Clause
    /// binding (`project.onboarding_jurisdiction`) reads the live value at
    /// render time; the repository has no use for it.
    pub jurisdiction: Option<&'a str>,
}

impl<'a> LiveFields<'a> {
    /// The same fields with `--drive-folder` replaced by its resolved id.
    fn with_drive_folder<'b>(self, drive_folder_id: Option<&'b str>) -> LiveFields<'b>
    where
        'a: 'b,
    {
        LiveFields {
            drive_folder_id,
            ..self
        }
    }
}

/// Everything one `navigator project set` invocation was asked to do, as
/// a single argument bundle — the LAW-213 flags pushed the bare parameter
/// list past `clippy::too_many_arguments`.
pub struct SetRequest<'a> {
    pub host_override: Option<&'a str>,
    pub project_override: Option<&'a str>,
    pub from_live: bool,
    pub apply_legal_plan: bool,
    pub supplied: [Option<&'a str>; 4],
    pub live_fields: LiveFields<'a>,
    pub firm: Option<&'a str>,
    pub brand: Option<&'a str>,
    pub dry_run: bool,
}

pub async fn run(request: SetRequest<'_>) -> ExitCode {
    let dry_run = request.dry_run;
    match set(request).await {
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

async fn set(request: SetRequest<'_>) -> Result<Vec<String>> {
    set_at(Path::new("."), request).await
}

pub(super) async fn set_at(dir: &Path, request: SetRequest<'_>) -> Result<Vec<String>> {
    let SetRequest {
        host_override,
        project_override,
        from_live,
        apply_legal_plan,
        supplied,
        live_fields,
        firm,
        brand,
        dry_run,
    } = request;
    if firm.is_some() != brand.is_some() {
        return Err(anyhow!("--firm and --brand must be supplied together"));
    }
    let [lawyer_dri, client_dri, private_slack_channel, private_notion_page] = supplied;
    let drive_folder = drive_folder_id(live_fields.drive_folder_id)?;
    let live_fields = live_fields.with_drive_folder(drive_folder.as_deref());
    let live_patch = live_patch(live_fields, private_slack_channel)?;
    let path = dir.join(manifest::FILE);
    let contents = std::fs::read_to_string(&path).context("read navigator.yaml")?;
    let parsed = manifest::parse(&contents).map_err(|error| anyhow!(error))?;
    let (host, project) = manifest_target(&parsed, host_override, project_override)?;
    if apply_legal_plan {
        anyhow::ensure!(
            !from_live
                && firm.is_none()
                && supplied.iter().all(Option::is_none)
                && live_patch.is_empty(),
            "--apply-legal-plan cannot be combined with coordinate changes"
        );
        return apply_manifest_plan(host, project, &parsed, dry_run).await;
    }
    let ownership = manifest_ownership(host, project, firm.zip(brand), from_live).await?;
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
    let live_plan = if from_live {
        fill_from_live(host, project, &mut values).await?
    } else {
        None
    };
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
    let (mut updated, mut changed) = apply_manifest_values(
        &parsed,
        &contents,
        values,
        clear_calendar,
        clear_drive_folder,
    )?;
    if let Some((firm, brand)) = ownership {
        let before_ownership = updated.clone();
        let diff = ownership_diff(&parsed, firm, &brand);
        updated = replace_project_key(&updated, "firm", &firm.to_string())?;
        updated = replace_project_key(&updated, "brand", &brand)?;
        if updated != before_ownership {
            changed.push("manifest Firm and brand".into());
        }
        if from_live {
            diff.print();
            if let Some((_, new_host)) = &diff.host {
                updated = replace_project_key(&updated, "host", new_host)?;
                changed.push("manifest host".into());
            }
        }
    }
    if let (None, Some(name)) = (&parsed.legal_plan, live_plan) {
        updated = replace_project_key(&updated, "legal_plan", &name)?;
        changed.push("legal_plan".into());
    }
    validate_manifest_update(&path, &updated)?;
    if dry_run {
        // Everything past this point is a write: the manifest file, the live
        // row patch, the Firm reassignment. The plan names all of them and
        // stops, so the preview is exactly what a non-dry run would do.
        return Ok(dry_run_plan(changed, &live_patch, firm.is_some()));
    }
    if !live_patch.is_empty() {
        patch_live_row(host, project, &live_patch, drive_folder.as_deref()).await?;
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

/// What adopting the live Firm and brand would change in the manifest, as
/// `(manifest value, live value)` pairs; a key already current is `None`.
#[derive(Debug, PartialEq, Eq)]
struct OwnershipDiff {
    firm: Option<(String, String)>,
    brand: Option<(String, String)>,
    /// Set only when the manifest host is registered to a different brand than
    /// the live one: the same-environment host the live brand is registered
    /// for, since CI mints a token only when Project and host brand agree.
    host: Option<(String, String)>,
}

fn ownership_diff(declared: &manifest::Manifest, firm: uuid::Uuid, brand: &str) -> OwnershipDiff {
    let pair = |old: Option<&str>, new: &str| (old.unwrap_or("none").to_owned(), new.to_owned());
    let declared_firm = declared.firm.as_deref();
    let firm_current = declared_firm
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .is_some_and(|id| id == firm);
    let host = declared.host.as_deref().and_then(|host| {
        let name = host
            .split_once("://")
            .map_or(host, |(_, rest)| rest)
            .split(['/', ':'])
            .next()
            .unwrap_or(host);
        let registered = views::brand::registered_brand_key(name)?;
        let live = views::brand::BrandKey::parse(brand)?;
        if registered == live {
            return None;
        }
        let staging = views::brand::is_staging_host(name);
        let new_host = live
            .hosts()
            .iter()
            .find(|candidate| views::brand::is_staging_host(candidate) == staging)?;
        Some((host.to_owned(), (*new_host).to_owned()))
    });
    OwnershipDiff {
        firm: (!firm_current).then(|| pair(declared_firm, &firm.to_string())),
        brand: (declared.brand.as_deref() != Some(brand))
            .then(|| pair(declared.brand.as_deref(), brand)),
        host,
    }
}

impl OwnershipDiff {
    fn print(&self) {
        for (key, change) in [
            ("firm", &self.firm),
            ("brand", &self.brand),
            ("host", &self.host),
        ] {
            if let Some((old, new)) = change {
                println!("  manifest {key}: {old} -> {new}");
            }
        }
    }
}

fn manifest_target<'a>(
    manifest: &'a manifest::Manifest,
    host_override: Option<&'a str>,
    project_override: Option<&'a str>,
) -> Result<(&'a str, &'a str)> {
    let host = host_override
        .or(manifest.host.as_deref())
        .ok_or_else(|| anyhow!("navigator.yaml must declare project.host or pass --host"))?;
    let project = project_override
        .or(manifest.project.as_deref())
        .ok_or_else(|| {
            anyhow!("navigator.yaml must declare project.name or pass a Project code")
        })?;
    Ok((host, project))
}

fn validate_manifest_update(path: &Path, contents: &str) -> Result<()> {
    let findings = manifest::lint_contents(path, contents);
    if let Some(finding) = findings.into_iter().find(|finding| !finding.warning) {
        return Err(anyhow!(
            "{} ({}): {}",
            finding.code,
            finding.line,
            finding.message
        ));
    }
    Ok(())
}

async fn apply_manifest_plan(
    host: &str,
    code: &str,
    declared: &manifest::Manifest,
    dry_run: bool,
) -> Result<Vec<String>> {
    let local_host = declared
        .host
        .as_deref()
        .context("navigator.yaml has no project.host")?;
    anyhow::ensure!(
        crate::credentials::base_url(host) == crate::credentials::base_url(local_host)
            && declared.project.as_deref() == Some(code),
        "--apply-legal-plan must target the Project named in navigator.yaml"
    );
    let Some(name) = declared.legal_plan.as_deref() else {
        return Ok(vec!["no Legal Plan declared".into()]);
    };
    let (base, token) = remote::resolve(Some(host))?;
    let project = surfaces::resolve_visible_project(&base, &token, code).await?;
    declared
        .check_live_ownership(project.firm_id, project.brand.as_deref())
        .map_err(anyhow::Error::msg)?;
    let firm = project.firm_id.context("live Project has no Firm")?;
    let brand = project.brand.context("live Project has no brand")?;
    let route = format!("{base}/app/api/projects/{}/legal-plan", project.id);
    let response = remote::authenticated_client_builder(&base)?
        .timeout(std::time::Duration::from_secs(30))
        .build()?
        .patch(route)
        .bearer_auth(token)
        .json(&serde_json::json!({
            "manifest_legal_plan": { "name": name, "firm_id": firm, "brand": brand, "dry_run": dry_run }
        }))
        .send()
        .await?;
    let status = response.status();
    if status != reqwest::StatusCode::NO_CONTENT {
        let detail = response
            .json::<serde_json::Value>()
            .await
            .unwrap_or_default();
        let message = detail["message"].as_str().unwrap_or("request refused");
        anyhow::bail!("apply the manifest Legal Plan: {status} {message}");
    }
    Ok(vec!["live Project Legal Plan".into()])
}

async fn manifest_ownership(
    host: &str,
    project: &str,
    explicit: Option<(&str, &str)>,
    from_live: bool,
) -> Result<Option<(uuid::Uuid, String)>> {
    let live = if from_live {
        let (base, token) = remote::resolve(Some(host))?;
        let row = surfaces::list_visible_projects(&base, &token)
            .await?
            .into_iter()
            .find(|row| row.code == project)
            .context("no live Project")?;
        // `--from-live` is the reconcile: it adopts the live Firm and brand
        // whatever the manifest says, and reports the difference.
        Some((
            row.firm_id.context("live Project has no Firm")?,
            row.brand.context("live Project has no brand")?,
        ))
    } else {
        None
    };
    if let Some((firm, brand)) = explicit {
        let (base, token) = remote::resolve(Some(host))?;
        return Ok(Some((
            remote::resolve_firm(&base, &token, firm, Some(brand)).await?,
            brand.to_string(),
        )));
    }
    Ok(live)
}

/// The navigator.yaml text and the names of the changed keys after the
/// supplied coordinate values and explicit clears are applied to the parsed
/// manifest. Pure, so the dry-run plan and the real write share one path.
fn apply_manifest_values(
    parsed: &manifest::Manifest,
    contents: &str,
    values: [Option<String>; 6],
    clear_calendar: bool,
    clear_drive_folder: bool,
) -> Result<(String, Vec<String>)> {
    let mut updated = contents.to_string();
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
    Ok((updated, changed))
}

/// Fill each coordinate not supplied by a flag from the live Project row.
async fn fill_from_live(
    host: &str,
    project: &str,
    values: &mut [Option<String>; 6],
) -> Result<Option<String>> {
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
    Ok(live.legal_plan)
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

/// The id `--drive-folder` names: a `drive.google.com` folder link or a bare
/// id, normalised to the id before anything is written. An empty value stays
/// empty, which clears the folder.
fn drive_folder_id(raw: Option<&str>) -> Result<Option<String>> {
    let Some(raw) = raw.map(str::trim) else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Ok(Some(String::new()));
    }
    model::projects::drive_folder_id_from_input(raw)
        .map(Some)
        .ok_or_else(|| anyhow!(model::projects::DRIVE_FOLDER_INPUT_INVALID))
}

/// Send the live-row patch, then report how the deployment checked a Drive
/// folder the patch names.
async fn patch_live_row(
    host: &str,
    project: &str,
    live_patch: &serde_json::Map<String, serde_json::Value>,
    drive_folder: Option<&str>,
) -> Result<()> {
    let drive_folder_check = remote::patch_project_fields(host, project, live_patch).await?;
    if let Some(folder) = drive_folder.filter(|id| !id.is_empty()) {
        println!(
            "drive folder {folder}: {}",
            drive_folder_check_note(drive_folder_check.as_deref())
        );
    }
    Ok(())
}

/// What the deployment's answer in
/// [`model::project_surfaces::DRIVE_FOLDER_CHECK_HEADER`] means for the
/// folder just recorded.
fn drive_folder_check_note(check: Option<&str>) -> &'static str {
    if check == Some(model::project_surfaces::DriveFolderCheck::Verified.as_str()) {
        "confirmed with Drive as a live folder in the Firm's Projects shared drive"
    } else {
        "only the id's shape was checked; the deployment has no Drive credential, so Drive validation was skipped"
    }
}

/// Build the live-row patch before any file or network write.
/// Empty `LiveFields` values clear their corresponding fields. The private
/// Slack channel takes a resource URL; its not-provisioned sentinel is manifest-only.
fn live_patch(
    fields: LiveFields<'_>,
    private_slack_channel: Option<&str>,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    let mut patch = serde_json::Map::new();
    if let Some(url) = private_slack_channel
        .map(str::trim)
        .filter(|url| !manifest::is_not_provisioned(url))
    {
        if !model::projects::is_valid_resource_url(url) {
            return Err(anyhow!(model::projects::RESOURCE_URL_INVALID));
        }
        patch.extend([("internal_slack_channel_url".into(), url.into())]);
    }
    if let Some(calendar_id) = fields.calendar_id.map(str::trim) {
        if !calendar_id.is_empty() && !model::projects::is_valid_calendar_id(calendar_id) {
            return Err(anyhow!(model::projects::CALENDAR_ID_INVALID));
        }
        patch.insert("calendar_id".into(), calendar_id.into());
    }
    if let Some(drive_folder_id) = fields.drive_folder_id.map(str::trim) {
        if !drive_folder_id.is_empty()
            && !model::projects::is_valid_drive_folder_id(drive_folder_id)
        {
            return Err(anyhow!(model::projects::DRIVE_FOLDER_ID_INVALID));
        }
        patch.insert("drive_folder_id".into(), drive_folder_id.into());
    }
    if let Some(booking_url) = fields.booking_url.map(str::trim) {
        if !booking_url.is_empty() && !model::projects::is_valid_resource_url(booking_url) {
            return Err(anyhow!(
                "--booking-url must be the appointment schedule's booking page, an http(s) address"
            ));
        }
        patch.insert("booking_url".into(), booking_url.into());
    }
    if let Some(zone) = fields.timezone.map(str::trim) {
        anyhow::ensure!(
            zone.is_empty() || zone.parse::<chrono_tz::Tz>().is_ok(),
            "--timezone must be an IANA time zone"
        );
        patch.insert("calendar_timezone".into(), zone.into());
    }
    if let Some(jurisdiction) = fields.jurisdiction.map(str::trim) {
        patch.insert("onboarding_jurisdiction".into(), jurisdiction.into());
    }
    Ok(patch)
}

/// Replace or add a scalar in the nested `project:` map while retaining all
/// unrelated YAML text and comments.
pub fn replace_project_key(contents: &str, key: &str, value: &str) -> Result<String> {
    let serialized = if value.contains(['\r', '\n']) {
        serde_json::to_string(value)?
    } else {
        serde_yaml::to_string(value)?.trim().to_string()
    };
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
        let indent = lines[project_line + 1..block_end]
            .iter()
            .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
            .map(|line| line.len() - line.trim_start().len())
            .min()
            .unwrap_or(2);
        lines.insert(
            project_line + 1,
            format!("{}{key}: {serialized}", " ".repeat(indent)),
        );
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
    use super::{
        drive_folder_check_note, drive_folder_id, live_patch, remove_project_key,
        replace_project_key, LiveFields,
    };

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
        assert!(live_patch(LiveFields::default(), None).unwrap().is_empty());
        let patch = live_patch(
            LiveFields {
                calendar_id: Some(" c_0123@group.calendar.example.com "),
                drive_folder_id: Some(" 1QaBcD_2-efG "),
                booking_url: Some("https://calendar.app.google/synthetic"),
                ..LiveFields::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(patch["calendar_id"], "c_0123@group.calendar.example.com");
        assert_eq!(patch["drive_folder_id"], "1QaBcD_2-efG");
        assert_eq!(
            patch["booking_url"],
            "https://calendar.app.google/synthetic"
        );
        let cleared = live_patch(
            LiveFields {
                booking_url: Some(""),
                drive_folder_id: Some(""),
                ..LiveFields::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(
            cleared["booking_url"], "",
            "an empty value clears the field"
        );
        assert_eq!(
            cleared["drive_folder_id"], "",
            "an empty value clears the folder, too"
        );
        assert!(live_patch(
            LiveFields {
                calendar_id: Some("https://calendar.google.com/calendar/r"),
                ..LiveFields::default()
            },
            None
        )
        .is_err());
        assert!(live_patch(
            LiveFields {
                booking_url: Some("javascript:alert(1)"),
                ..LiveFields::default()
            },
            None
        )
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
                live_patch(
                    LiveFields {
                        drive_folder_id: Some(bad_drive_folder),
                        ..LiveFields::default()
                    },
                    None
                )
                .is_err(),
                "{bad_drive_folder} must be refused"
            );
        }
    }

    /// `--drive-folder` takes the folder's link as Drive shares it, or the
    /// bare id, and the live patch and the manifest both receive the id.
    #[test]
    fn drive_folder_accepts_a_folder_link_or_an_id_and_yields_the_id() {
        for input in [
            "1SyntheticFolderId",
            "https://drive.google.com/drive/folders/1SyntheticFolderId?usp=sharing",
            "https://drive.google.com/drive/u/1/folders/1SyntheticFolderId",
            "https://drive.google.com/open?id=1SyntheticFolderId",
        ] {
            assert_eq!(
                drive_folder_id(Some(input)).unwrap().as_deref(),
                Some("1SyntheticFolderId"),
                "{input}"
            );
        }
        assert_eq!(drive_folder_id(Some("  ")).unwrap().as_deref(), Some(""));
        assert_eq!(drive_folder_id(None).unwrap(), None);
        let refused = drive_folder_id(Some("https://example.com/drive/folders/1Synthetic"))
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("drive.google.com folder link"),
            "{refused}"
        );
    }

    /// The report names whether Drive confirmed the folder, and says plainly
    /// when only the id's shape was checked.
    #[test]
    fn the_drive_folder_note_says_whether_drive_confirmed_the_folder() {
        assert!(drive_folder_check_note(Some("verified")).contains("confirmed with Drive"));
        for check in [Some("skipped"), None] {
            assert!(
                drive_folder_check_note(check).contains("validation was skipped"),
                "{check:?}"
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

    use super::{set_at, SetRequest};
    use crate::projects::test_login::{answer, site};
    use serde_json::json;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, ResponseTemplate};

    const CODE: &str = "acme";
    const FIRM: &str = "11111111-1111-4111-8111-111111111111";

    /// A lint-clean manifest for `acme` on `host`.
    fn manifest_on(host: &str) -> String {
        format!(
            "version: 26.9.14\nproject:\n  host: {host}\n  name: acme\n  \
             lawyer_dri: lawyer@example.com\n  client_dri: client@example.com\n  \
             private_slack_channel: https://slack.example.com/private\n  \
             private_notion_page: https://notion.example.com/private\n  \
             calendar_id: c_0123@group.calendar.example.com\n"
        )
    }

    fn request(host: &str) -> SetRequest<'_> {
        SetRequest {
            host_override: Some(host),
            project_override: None,
            from_live: false,
            apply_legal_plan: false,
            supplied: [None; 4],
            live_fields: LiveFields::default(),
            firm: None,
            brand: None,
            dry_run: false,
        }
    }

    fn checkout(manifest: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("navigator.yaml");
        std::fs::write(&path, manifest).unwrap();
        (dir, path)
    }

    async fn outcome(dir: &std::path::Path, request: SetRequest<'_>) -> Result<String, String> {
        set_at(dir, request)
            .await
            .map(|changed| changed.join(", "))
            .map_err(|error| format!("{error:#}"))
    }

    #[tokio::test]
    async fn a_coordinate_flag_rewrites_only_its_manifest_key_and_a_dry_run_writes_nothing() {
        let original = manifest_on("site.example.test");
        let (dir, path) = checkout(&original);
        let dir = dir.path();
        let host = "site.example.test";
        let dri = |dry_run| SetRequest {
            supplied: [Some(" second@example.com "), None, None, None],
            dry_run,
            ..request(host)
        };
        assert_eq!(outcome(dir, dri(true)).await.unwrap(), "lawyer_dri");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert_eq!(outcome(dir, dri(false)).await.unwrap(), "lawyer_dri");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original.replace("lawyer@example.com", "second@example.com")
        );
        assert_eq!(
            outcome(dir, dri(false)).await.unwrap(),
            "coordinates already current"
        );

        for (refused, expected) in [
            (request(host), "pass --from-live, a coordinate flag"),
            (
                SetRequest {
                    firm: Some("Sample Firm"),
                    ..request(host)
                },
                "--firm and --brand must be supplied together",
            ),
            (
                SetRequest {
                    supplied: [Some("not-an-email"), None, None, None],
                    ..request(host)
                },
                "lawyer_dri",
            ),
            (
                SetRequest {
                    apply_legal_plan: true,
                    from_live: true,
                    ..request(host)
                },
                "--apply-legal-plan cannot be combined with coordinate changes",
            ),
            (
                SetRequest {
                    apply_legal_plan: true,
                    project_override: Some("another"),
                    ..request(host)
                },
                "--apply-legal-plan must target the Project named in navigator.yaml",
            ),
        ] {
            let error = outcome(dir, refused).await.unwrap_err();
            assert!(error.contains(expected), "{error}");
        }
        let plan = SetRequest {
            apply_legal_plan: true,
            ..request(host)
        };
        assert_eq!(outcome(dir, plan).await.unwrap(), "no Legal Plan declared");

        let empty = tempfile::tempdir().unwrap();
        let error = outcome(empty.path(), dri(false)).await.unwrap_err();
        assert!(error.starts_with("read navigator.yaml"), "{error}");
        std::fs::write(empty.path().join("navigator.yaml"), "version: 26.9.14\n").unwrap();
        let unbound = SetRequest {
            host_override: None,
            ..dri(false)
        };
        let error = outcome(empty.path(), unbound).await.unwrap_err();
        assert!(error.contains("must declare project.host or pass --host"));
        let error = outcome(empty.path(), dri(false)).await.unwrap_err();
        assert!(error.contains("must declare project.name or pass a Project code"));
        assert_eq!(
            super::run(request(host)).await,
            std::process::ExitCode::from(1)
        );
    }

    #[tokio::test]
    async fn from_live_fills_unsupplied_coordinates_ownership_and_the_legal_plan() {
        let id = uuid::Uuid::new_v4();
        let live = json!([{"id": id, "code": CODE, "firm_id": FIRM, "brand": "sample"}]);
        let (server, _login) = site(live).await;
        let host = server.uri();
        let handles = json!({
            "code": CODE,
            "repository_url": null,
            "lawyer_dri": ["live-lawyer@example.com"],
            "client_dri": ["client@example.com"],
            "private_slack_channel": "https://slack.example.com/private",
            "private_notion_page": "https://notion.example.com/private",
            "calendar_id": "c_0123@group.calendar.example.com",
            "legal_plan": "Sample Panel",
            "dangling_dri_person_ids": [uuid::Uuid::new_v4()],
        });
        let door = format!("/app/api/project-surfaces/{id}");
        answer(&server, "GET", &door, 200, handles).await;
        let original = manifest_on("site.example.test");
        let (dir, path) = checkout(&original);
        let from_live = SetRequest {
            from_live: true,
            ..request(&host)
        };
        assert_eq!(
            outcome(dir.path(), from_live).await.unwrap(),
            "lawyer_dri, manifest Firm and brand, legal_plan"
        );
        let written = std::fs::read_to_string(&path).unwrap();
        for line in [
            "  lawyer_dri: live-lawyer@example.com\n",
            &format!("  firm: {FIRM}\n"),
            "  brand: sample\n",
            "  legal_plan: Sample Panel\n",
        ] {
            assert!(written.contains(line), "{written}");
        }

        let absent = SetRequest {
            from_live: true,
            project_override: Some("absent"),
            ..request(&host)
        };
        let error = outcome(dir.path(), absent).await.unwrap_err();
        assert_eq!(error, "no live Project");
    }

    #[tokio::test]
    async fn live_fields_patch_the_project_row_and_an_empty_calendar_clears_both_records() {
        let id = uuid::Uuid::new_v4();
        let (server, _login) = site(json!([{"id": id, "code": CODE}])).await;
        let host = server.uri();
        Mock::given(method("PATCH"))
            .and(path(format!("/app/api/projects/{id}")))
            .and(body_json(json!({
                "calendar_id": "",
                "drive_folder_id": "1QaBcD_2-efGhIjKlMnOpQrStUv",
                "booking_url": "https://calendar.example.com/book",
                "onboarding_jurisdiction": "NV",
                "calendar_timezone": "America/Denver",
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let original = manifest_on("site.example.test");
        let (dir, path) = checkout(&original);
        let fields = SetRequest {
            live_fields: LiveFields {
                calendar_id: Some(""),
                drive_folder_id: Some("1QaBcD_2-efGhIjKlMnOpQrStUv"),
                booking_url: Some("https://calendar.example.com/book"),
                jurisdiction: Some("NV"),
                timezone: Some("America/Denver"),
            },
            ..request(&host)
        };
        assert_eq!(
            outcome(dir.path(), fields).await.unwrap(),
            "drive_folder_id, calendar_id, live Project booking_url, live Project calendar_id, \
             live Project calendar_timezone, live Project drive_folder_id, live Project onboarding_jurisdiction"
        );
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(!written.contains("calendar_id"), "{written}");
        assert!(written.contains("  drive_folder_id: 1QaBcD_2-efGhIjKlMnOpQrStUv\n"));
    }

    #[tokio::test]
    async fn the_declared_legal_plan_is_applied_to_the_live_project_it_names() {
        let id = uuid::Uuid::new_v4();
        let live = json!([{"id": id, "code": CODE, "firm_id": FIRM, "brand": "sample"}]);
        let (server, _login) = site(live).await;
        let host = server.uri();
        let declared = format!(
            "{}  legal_plan: Sample Panel\n  firm: {FIRM}\n  brand: sample\n",
            manifest_on(&host)
        );
        let (dir, _path) = checkout(&declared);
        let apply = |dry_run| SetRequest {
            apply_legal_plan: true,
            dry_run,
            ..request(&host)
        };
        let error = outcome(dir.path(), apply(false)).await.unwrap_err();
        assert_eq!(
            error,
            "apply the manifest Legal Plan: 404 Not Found request refused"
        );
        Mock::given(method("PATCH"))
            .and(path(format!("/app/api/projects/{id}/legal-plan")))
            .and(body_json(json!({"manifest_legal_plan": {
                "name": "Sample Panel", "firm_id": FIRM, "brand": "sample", "dry_run": true,
            }})))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        assert_eq!(
            outcome(dir.path(), apply(true)).await.unwrap(),
            "live Project Legal Plan"
        );

        let (other, _path) = checkout(&declared.replace("brand: sample", "brand: another"));
        let error = outcome(other.path(), apply(true)).await.unwrap_err();
        assert!(
            error.starts_with("navigator.yaml Firm and brand differ from the live Project: "),
            "{error}"
        );
        assert!(error.contains("brand another"), "{error}");
        assert!(error.contains("brand sample"), "{error}");
    }

    #[tokio::test]
    async fn an_explicit_firm_and_brand_reassign_the_live_project_and_the_manifest() {
        let id = uuid::Uuid::new_v4();
        let (server, _login) = site(json!([{"id": id, "code": CODE}])).await;
        let host = server.uri();
        let firms = json!([{"id": FIRM, "name": "Sample Firm", "brands": ["sample"]}]);
        answer(&server, "GET", "/app/api/firms", 200, firms).await;
        Mock::given(method("PATCH"))
            .and(path(format!("/app/api/projects/{id}")))
            .and(body_json(json!({"firm_id": FIRM, "brand": "sample"})))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let (dir, path) = checkout(&manifest_on("site.example.test"));
        let owned = |dry_run| SetRequest {
            firm: Some("sample firm"),
            brand: Some("sample"),
            dry_run,
            ..request(&host)
        };
        let planned = "manifest Firm and brand, live Project Firm and brand";
        assert_eq!(outcome(dir.path(), owned(true)).await.unwrap(), planned);
        assert_eq!(outcome(dir.path(), owned(false)).await.unwrap(), planned);
        let written = std::fs::read_to_string(&path).unwrap();
        let owned = crate::projects::manifest::parse(&written).unwrap();
        assert_eq!(owned.firm.as_deref(), Some(FIRM));
        assert_eq!(owned.brand.as_deref(), Some("sample"));
    }

    /// ENG-1417: `--from-live` is the reconcile, so a manifest whose Firm and
    /// brand drifted is repaired by it rather than refused. The dry run shows
    /// the edit, and once it is applied the gate's ownership check passes.
    #[tokio::test]
    async fn from_live_repairs_a_drifted_firm_and_brand_and_turns_the_check_green() {
        const OTHER: &str = "22222222-2222-4222-8222-222222222222";
        let id = uuid::Uuid::new_v4();
        let live = json!([{"id": id, "code": CODE, "firm_id": FIRM, "brand": "sample"}]);
        let (server, _login) = site(live).await;
        let host = server.uri();
        let handles = json!({
            "code": CODE, "repository_url": null,
            "lawyer_dri": ["lawyer@example.com"], "client_dri": ["client@example.com"],
            "private_slack_channel": "https://slack.example.com/private",
            "private_notion_page": "https://notion.example.com/private",
            "calendar_id": "c_0123@group.calendar.example.com",
            "dangling_dri_person_ids": [],
        });
        answer(
            &server,
            "GET",
            &format!("/app/api/project-surfaces/{id}"),
            200,
            handles,
        )
        .await;
        let drifted = format!(
            "{}  firm: {OTHER}\n  brand: another\n",
            manifest_on("site.example.test")
        );
        let (dir, path) = checkout(&drifted);
        let from_live = |dry_run| SetRequest {
            from_live: true,
            dry_run,
            ..request(&host)
        };
        let plan = outcome(dir.path(), from_live(true)).await.unwrap();
        assert!(plan.contains("manifest Firm and brand"), "{plan}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            drifted,
            "a dry run writes nothing"
        );

        outcome(dir.path(), from_live(false)).await.unwrap();
        let repaired =
            crate::projects::manifest::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            repaired.check_live_ownership(Some(FIRM.parse().unwrap()), Some("sample")),
            Ok(())
        );
    }

    /// The diff names each differing key, and proposes `host` only when the
    /// manifest host is registered to a different brand, in the same
    /// environment as the manifest.
    #[test]
    fn the_ownership_diff_names_each_key_and_proposes_a_host_only_for_a_registered_mismatch() {
        let parse = |host: &str, extra: &str| {
            crate::projects::manifest::parse(&format!(
                "project:\n  host: {host}\n  name: acme\n{extra}"
            ))
            .unwrap()
        };
        let firm: uuid::Uuid = FIRM.parse().unwrap();
        let drifted = parse(
            "staging.neonlaw.com",
            &format!("  firm: {FIRM}\n  brand: neon\n"),
        );
        let diff = super::ownership_diff(&drifted, firm, "vesta");
        assert_eq!(diff.brand, Some(("neon".to_owned(), "vesta".to_owned())));
        assert_eq!(diff.firm, None, "an equal Firm is not a change");
        assert_eq!(
            diff.host,
            Some((
                "staging.neonlaw.com".to_owned(),
                "staging.vestaestateplanning.com".to_owned()
            ))
        );
        let production = parse(
            "www.neonlaw.com",
            &format!("  firm: {FIRM}\n  brand: neon\n"),
        );
        assert_eq!(
            super::ownership_diff(&production, firm, "vesta")
                .host
                .map(|pair| pair.1),
            Some("www.vestaestateplanning.com".to_owned())
        );
        for host in [
            "staging.vestaestateplanning.com",
            "site.example.test",
            "127.0.0.1:8080",
        ] {
            let diff = super::ownership_diff(&parse(host, ""), firm, "vesta");
            assert_eq!(diff.host, None, "{host}");
        }
        let bare = super::ownership_diff(&parse("site.example.test", ""), firm, "vesta");
        assert_eq!(bare.firm, Some(("none".to_owned(), FIRM.to_owned())));
        assert_eq!(bare.brand, Some(("none".to_owned(), "vesta".to_owned())));
    }
}
