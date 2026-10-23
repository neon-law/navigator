//! Live-fill Project coordinates in the current repository's `navigator.yaml`.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};

use crate::{
    projects::{manifest, surfaces},
    remote,
};

const KEYS: [&str; 4] = [
    "lawyer_dri",
    "client_dri",
    "private_slack_channel",
    "private_notion_page",
];

pub async fn run(
    host_override: Option<&str>,
    project_override: Option<&str>,
    from_live: bool,
    supplied: [Option<&str>; 4],
    firm: Option<&str>,
    brand: Option<&str>,
) -> ExitCode {
    match set(
        host_override,
        project_override,
        from_live,
        supplied,
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
    firm: Option<&str>,
    brand: Option<&str>,
) -> Result<Vec<String>> {
    if firm.is_some() != brand.is_some() {
        return Err(anyhow!("--firm and --brand must be supplied together"));
    }
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
    let mut values: [Option<String>; 4] = supplied.map(|value| value.map(str::to_string));
    if from_live {
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
        ];
        for (target, live_value) in values.iter_mut().zip(live_values) {
            if target.is_none() {
                *target = live_value;
            }
        }
    }
    if values.iter().all(Option::is_none) && firm.is_none() {
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
            _ => parsed.private_notion_page.as_deref(),
        };
        if current == Some(value.as_str()) {
            continue;
        }
        updated = replace_project_key(&updated, key, &value)?;
        changed.push(key.to_string());
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

#[cfg(test)]
mod tests {
    use super::replace_project_key;

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
