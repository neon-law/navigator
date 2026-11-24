//! The workspace root `navigator sync` maintains around the Project checkouts.
//!
//! Two layers of agent instructions meet here. The generic layer ships in this
//! release: the root `AGENTS.md` and [`project_scaffold::WORKSPACE_SKILLS`].
//! Each Firm's layer is its private Firm Library, checked out at
//! `.firm/<firm-slug>/` (`AGENTS.md` plus `skills/<skill>/SKILL.md`). The host
//! names each Firm's library from the Firm's own record
//! (`GET /app/api/firm-libraries`), so no repository URL is compiled in.
//!
//! A Project checkout sees the generic layer plus its own Firm's skills, and
//! never another Firm's: its git-ignored `.claude/skills/` holds one relative
//! link per skill. A Firm skill replaces a generic skill of the same name, and
//! sync reports the override. Each Firm fails closed on its own: an unreadable
//! host or an unreachable library leaves the generic layer and every other
//! Firm in place, and sync names what is missing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use super::manifest;
use crate::remote;

/// The generic workspace `AGENTS.md`. Its first line marks it as owned, so a
/// hand-written file is never overwritten.
pub const WORKSPACE_AGENTS: &str = include_str!("workspace_agents.md");
const OWNED_MARKER: &str = "<!-- navigator: workspace AGENTS.md.";
/// Where each Firm's library is checked out, one folder per Firm slug.
pub const FIRM_DIRECTORY: &str = ".firm";
/// The names sync wrote under `.agents/skills/`, one per line: `skill:<name>`
/// for a generic skill and `firm:<slug>` for a Firm namespace. A name not
/// listed is local and never removed.
const OWNED_FILE: &str = ".navigator-owned";
const SKILLS: &str = ".agents/skills";
const CLAUDE_SKILLS: &str = ".claude/skills";
/// Relative link prefixes sync writes, and so recognizes as its own.
const ROOT_LINK: &str = "../../.agents/skills/";
const PROJECT_LINK: &str = "../../../.agents/skills/";

#[cfg(unix)]
use std::os::unix::fs::symlink;
#[cfg(windows)]
use std::os::windows::fs::symlink_dir as symlink;

/// One Project checkout under the root, as its manifest declares it.
#[derive(Debug, Clone)]
pub struct Checkout {
    pub dir: PathBuf,
    pub code: String,
    pub host: Option<String>,
    pub firm: Option<String>,
}

/// Every child folder carrying a readable `navigator.yaml`.
#[must_use]
pub fn checkouts(root: &Path) -> Vec<Checkout> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut found: Vec<Checkout> = entries
        .flatten()
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| {
            let dir = entry.path();
            let manifest = manifest::read(&dir)?;
            Some(Checkout {
                code: entry.file_name().to_string_lossy().into_owned(),
                host: manifest::non_empty(manifest.host.as_deref())
                    .map(|host| crate::credentials::base_url(&host)),
                firm: manifest::non_empty(manifest.firm.as_deref()),
                dir,
            })
        })
        .collect();
    found.sort_by(|a, b| a.code.cmp(&b.code));
    found
}

/// What one sync pass did, or in a dry run would do.
#[derive(Debug, Default)]
pub struct Report {
    pub changes: Vec<String>,
    pub notes: Vec<String>,
    /// Something a Firm or host needs before its layer is complete.
    pub missing: Vec<String>,
}

impl Report {
    pub fn print(&self, dry_run: bool) {
        let verb = if dry_run { "would " } else { "" };
        for change in &self.changes {
            eprintln!("workspace: {verb}{change}");
        }
        for note in &self.notes {
            eprintln!("workspace: {note}");
        }
        for missing in &self.missing {
            eprintln!("workspace: missing: {missing}");
        }
    }
}

/// Write the generic layer, link each present Firm library, and scope every
/// checkout's `.claude/skills/`. `project_firms` maps a checkout's code to its
/// Firm slug; a checkout absent from it gets the generic layer only.
pub fn materialize(
    root: &Path,
    generic: &[(&str, &str)],
    project_firms: &BTreeMap<String, String>,
    dry_run: bool,
) -> Result<Report> {
    let mut report = Report::default();
    let mut io = Writer {
        dry_run,
        report: &mut report,
    };
    restore_agents(root, &mut io)?;
    let skills = root.join(SKILLS);
    let owned = read_owned(&skills);
    let mut now_owned = BTreeSet::new();

    for (name, contents) in generic {
        io.write(
            &skills.join(name).join("SKILL.md"),
            contents,
            &format!("{SKILLS}/{name}"),
        )?;
        now_owned.insert(format!("skill:{name}"));
    }

    let codes: Vec<String> = checkouts(root).into_iter().map(|c| c.code).collect();
    let firms = firm_skills(root, &codes, io.report);
    for (slug, names) in &firms {
        let namespace = skills.join(slug);
        let wanted: BTreeMap<String, String> = names
            .iter()
            .map(|name| {
                (
                    name.clone(),
                    format!("../../../{FIRM_DIRECTORY}/{slug}/skills/{name}"),
                )
            })
            .collect();
        io.links(
            &namespace,
            &wanted,
            "../../../",
            &format!("{SKILLS}/{slug}"),
        )?;
        now_owned.insert(format!("firm:{slug}"));
    }
    for stale in owned.difference(&now_owned) {
        if let Some((_, name)) = stale.split_once(':') {
            io.remove_dir(&skills.join(name), &format!("{SKILLS}/{name}"))?;
        }
    }
    if owned != now_owned {
        let mut listing: Vec<&str> = now_owned.iter().map(String::as_str).collect();
        listing.push("");
        io.write(&skills.join(OWNED_FILE), &listing.join("\n"), "")?;
    }

    let mut root_links: BTreeMap<String, String> = generic
        .iter()
        .map(|(name, _)| ((*name).to_string(), format!("{ROOT_LINK}{name}")))
        .collect();
    for (slug, names) in &firms {
        for name in names {
            root_links.insert(
                format!("{slug}-{name}"),
                format!("{ROOT_LINK}{slug}/{name}"),
            );
        }
    }
    io.links(
        &root.join(CLAUDE_SKILLS),
        &root_links,
        ROOT_LINK,
        CLAUDE_SKILLS,
    )?;

    for checkout in checkouts(root) {
        let firm = project_firms.get(&checkout.code);
        let mut links: BTreeMap<String, String> = generic
            .iter()
            .map(|(name, _)| ((*name).to_string(), format!("{PROJECT_LINK}{name}")))
            .collect();
        if let Some(names) = firm.and_then(|slug| firms.get(slug).map(|names| (slug, names))) {
            let (slug, names) = names;
            for name in names {
                if links.contains_key(name) {
                    io.report.notes.push(format!(
                        "{}: Firm skill {slug}/{name} overrides the generic {name}",
                        checkout.code
                    ));
                }
                links.insert(name.clone(), format!("{PROJECT_LINK}{slug}/{name}"));
            }
        }
        if !ignores_claude_skills(&checkout.dir) {
            io.report.missing.push(format!(
                "{}: `.gitignore` lacks `{CLAUDE_SKILLS}/`; run `navigator project repository sync-skills` there",
                checkout.code
            ));
            continue;
        }
        let label = format!("{}/{CLAUDE_SKILLS}", checkout.code);
        io.links(
            &checkout.dir.join(CLAUDE_SKILLS),
            &links,
            PROJECT_LINK,
            &label,
        )?;
    }
    Ok(report)
}

/// Each present Firm library's skills, by slug. A skill without a `SKILL.md`,
/// or one naming a Project code in this workspace, is not linked.
fn firm_skills(
    root: &Path,
    codes: &[String],
    report: &mut Report,
) -> BTreeMap<String, Vec<String>> {
    let mut firms = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(root.join(FIRM_DIRECTORY)) else {
        return firms;
    };
    for entry in entries.flatten() {
        let slug = entry.file_name().to_string_lossy().into_owned();
        if !model::workspace::is_valid_slug(&slug) || !entry.path().is_dir() {
            continue;
        }
        let mut names = Vec::new();
        for skill in std::fs::read_dir(entry.path().join("skills"))
            .into_iter()
            .flatten()
            .flatten()
        {
            let name = skill.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !skill.path().is_dir() {
                continue;
            }
            let Ok(contents) = std::fs::read_to_string(skill.path().join("SKILL.md")) else {
                report.missing.push(format!(
                    "{FIRM_DIRECTORY}/{slug}/skills/{name} has no SKILL.md"
                ));
                continue;
            };
            if let Some(code) = codes.iter().find(|code| contents.contains(code.as_str())) {
                report.missing.push(format!(
                    "{FIRM_DIRECTORY}/{slug}/skills/{name} names Project `{code}`; not linked"
                ));
                continue;
            }
            names.push(name);
        }
        names.sort();
        firms.insert(slug, names);
    }
    firms
}

fn restore_agents(root: &Path, io: &mut Writer<'_>) -> Result<()> {
    let path = root.join("AGENTS.md");
    match std::fs::read_to_string(&path) {
        Ok(current) if current == WORKSPACE_AGENTS => Ok(()),
        Ok(current) if !current.starts_with(OWNED_MARKER) => {
            io.report.notes.push(
                "AGENTS.md is hand-written and left alone; move its Firm rules to the Firm Library's AGENTS.md, then delete it so sync can restore the generic copy".into(),
            );
            Ok(())
        }
        _ => io.write(&path, WORKSPACE_AGENTS, "AGENTS.md"),
    }
}

fn read_owned(skills: &Path) -> BTreeSet<String> {
    std::fs::read_to_string(skills.join(OWNED_FILE))
        .unwrap_or_default()
        .lines()
        .filter(|line| line.starts_with("skill:") || line.starts_with("firm:"))
        .filter(|line| {
            line.split_once(':')
                .is_some_and(|(_, name)| model::workspace::is_valid_slug(name))
        })
        .map(str::to_string)
        .collect()
}

fn ignores_claude_skills(checkout: &Path) -> bool {
    std::fs::read_to_string(checkout.join(".gitignore"))
        .unwrap_or_default()
        .lines()
        .any(|line| matches!(line.trim(), ".claude/skills/" | "/.claude/skills/"))
}

struct Writer<'a> {
    dry_run: bool,
    report: &'a mut Report,
}

impl Writer<'_> {
    /// Write `contents` unless the file already holds them. An empty `label`
    /// writes without reporting (sync's own bookkeeping).
    fn write(&mut self, path: &Path, contents: &str, label: &str) -> Result<()> {
        if std::fs::read_to_string(path).is_ok_and(|current| current == contents) {
            return Ok(());
        }
        if !label.is_empty() {
            let verb = if path.exists() { "restore" } else { "add" };
            self.report.changes.push(format!("{verb} {label}"));
        }
        if self.dry_run {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, contents).with_context(|| format!("write {}", path.display()))
    }

    fn remove_dir(&mut self, path: &Path, label: &str) -> Result<()> {
        if std::fs::symlink_metadata(path).is_err() {
            return Ok(());
        }
        self.report.changes.push(format!("remove stale {label}"));
        if !self.dry_run {
            std::fs::remove_dir_all(path).with_context(|| format!("remove {}", path.display()))?;
        }
        Ok(())
    }

    /// Make `dir` hold exactly `wanted` (name → relative target) among the
    /// links sync owns: those whose target starts with `ours`. A real folder
    /// or a foreign link of the same name is local and stays.
    fn links(
        &mut self,
        dir: &Path,
        wanted: &BTreeMap<String, String>,
        ours: &str,
        label: &str,
    ) -> Result<()> {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let Ok(target) = std::fs::read_link(entry.path()) else {
                    continue;
                };
                let target = target.to_string_lossy();
                if target.starts_with(ours) && !wanted.contains_key(&name) {
                    self.report.changes.push(format!("unlink {label}/{name}"));
                    if !self.dry_run {
                        std::fs::remove_file(entry.path())?;
                    }
                }
            }
        }
        for (name, target) in wanted {
            let path = dir.join(name);
            match std::fs::read_link(&path) {
                Ok(current) if current == Path::new(target) => continue,
                Ok(current) if current.to_string_lossy().starts_with(ours) => {
                    self.report.changes.push(format!("relink {label}/{name}"));
                    if !self.dry_run {
                        std::fs::remove_file(&path)?;
                    }
                }
                Ok(_) => {
                    self.report.notes.push(format!(
                        "{label}/{name} is a local link and shadows the synced skill"
                    ));
                    continue;
                }
                Err(_) if std::fs::symlink_metadata(&path).is_ok() => {
                    self.report.notes.push(format!(
                        "{label}/{name} is a local skill and shadows the synced one"
                    ));
                    continue;
                }
                Err(_) => self.report.changes.push(format!("link {label}/{name}")),
            }
            if self.dry_run {
                continue;
            }
            std::fs::create_dir_all(dir)?;
            symlink(target, &path).with_context(|| format!("link {}", path.display()))?;
            if !path.join("SKILL.md").is_file() {
                self.report
                    .missing
                    .push(format!("{label}/{name} does not resolve to a SKILL.md"));
            }
        }
        Ok(())
    }
}

/// A Firm and its library on one host, as `GET /app/api/firm-libraries` lists it.
#[derive(Debug, Deserialize)]
struct FirmLibrary {
    slug: String,
    repository_url: Option<String>,
}

/// Resolve every checkout's Firm, check out or fast-forward each Firm's
/// library, then [`materialize`]. Network failures land in the report, never
/// abort the pass; only a local write failure is an error.
pub async fn sync(root: &Path, default_host: &str, dry_run: bool) -> Result<Report> {
    let mut report = Report::default();
    let all = checkouts(root);
    let mut hosts: BTreeMap<String, Vec<&Checkout>> = BTreeMap::new();
    for checkout in &all {
        let host = checkout
            .host
            .clone()
            .unwrap_or_else(|| crate::credentials::base_url(default_host));
        hosts.entry(host).or_default().push(checkout);
    }

    let mut project_firms = BTreeMap::new();
    let mut libraries: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut every_host_read = true;
    for (host, members) in &hosts {
        let listed = match firm_libraries(host).await {
            Ok(listed) => listed,
            Err(error) => {
                every_host_read = false;
                report
                    .missing
                    .push(format!("could not read {host}: {error:#}"));
                continue;
            }
        };
        for checkout in members {
            let firm = match (&checkout.firm, listed.as_slice()) {
                (Some(slug), _) if listed.iter().any(|firm| &firm.slug == slug) => slug.clone(),
                (Some(slug), _) => {
                    report.missing.push(format!(
                        "{}: Firm `{slug}` is not one of your Firms on {host}",
                        checkout.code
                    ));
                    continue;
                }
                (None, [only]) => only.slug.clone(),
                (None, _) => {
                    report.missing.push(format!(
                        "{}: navigator.yaml names no `firm` and {host} lists {} Firms for you",
                        checkout.code,
                        listed.len()
                    ));
                    continue;
                }
            };
            let url = listed
                .iter()
                .find(|library| library.slug == firm)
                .and_then(|library| library.repository_url.clone());
            match libraries.get(&firm) {
                Some(seen) if *seen != url => report.missing.push(format!(
                    "Firm `{firm}` names two different libraries across hosts; using the first"
                )),
                Some(_) => {}
                None => {
                    libraries.insert(firm.clone(), url);
                }
            }
            project_firms.insert(checkout.code.clone(), firm);
        }
    }

    let firm_root = root.join(FIRM_DIRECTORY);
    for (slug, url) in &libraries {
        if !model::workspace::is_valid_slug(slug) {
            report
                .missing
                .push(format!("Firm slug `{slug}` is not a valid folder name"));
            continue;
        }
        let Some(url) = url else {
            report.missing.push(format!(
                "Firm `{slug}` has no Firm Library repository on its host"
            ));
            continue;
        };
        if dry_run {
            continue;
        }
        if let Err(error) = checkout_library(&firm_root, slug, url).await {
            report
                .missing
                .push(format!("Firm `{slug}` library {url}: {error:#}"));
        }
    }
    if every_host_read {
        remove_unused_libraries(&firm_root, &libraries, dry_run, &mut report).await;
    }

    let mut materialized = materialize(
        root,
        project_scaffold::WORKSPACE_SKILLS,
        &project_firms,
        dry_run,
    )?;
    report.changes.append(&mut materialized.changes);
    report.notes.append(&mut materialized.notes);
    report.missing.append(&mut materialized.missing);
    Ok(report)
}

async fn firm_libraries(host: &str) -> Result<Vec<FirmLibrary>> {
    let (base, token) = remote::resolve(Some(host))?;
    let response = remote::authenticated_client_builder(&base)?
        .timeout(std::time::Duration::from_secs(30))
        .build()?
        .get(format!("{base}/app/api/firm-libraries"))
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?;
    Ok(response.json().await?)
}

/// Clone a Firm library into `.firm/<slug>`, or fast-forward an existing one
/// whose working copy has no local edits.
async fn checkout_library(firm_root: &Path, slug: &str, url: &str) -> Result<()> {
    use super::watch::command;
    anyhow::ensure!(
        url.starts_with("https://") || url.starts_with("ssh://") || url.starts_with("git@"),
        "unsupported repository transport"
    );
    let dir = firm_root.join(slug);
    if std::fs::symlink_metadata(&dir).is_err() {
        std::fs::create_dir_all(firm_root)?;
        let staging = tempfile::tempdir_in(firm_root)?;
        command(
            staging.path(),
            "jj",
            &[
                "git",
                "clone",
                "--colocate",
                "--branch",
                "main",
                "--",
                url,
                "checkout",
            ],
        )
        .await?;
        std::fs::rename(staging.path().join("checkout"), &dir).context("publish Firm library")?;
        eprintln!("Cloned Firm library {slug}");
        return Ok(());
    }
    command(&dir, "jj", &["git", "fetch", "--branch", "main"]).await?;
    let edits = command(&dir, "jj", &["diff", "--summary", "-r", "@"]).await?;
    anyhow::ensure!(
        edits.trim().is_empty(),
        "has local edits; land them through a reviewed change, then sync again"
    );
    let parent = command(
        &dir,
        "jj",
        &["log", "--no-graph", "-r", "@-", "-T", "commit_id"],
    )
    .await?;
    let main = command(
        &dir,
        "jj",
        &["log", "--no-graph", "-r", "main@origin", "-T", "commit_id"],
    )
    .await?;
    if parent.trim() != main.trim() {
        command(&dir, "jj", &["new", "main@origin"]).await?;
    }
    Ok(())
}

/// Remove a `.firm/<slug>` whose Firm no longer backs any checkout, unless it
/// holds local edits.
async fn remove_unused_libraries(
    firm_root: &Path,
    wanted: &BTreeMap<String, Option<String>>,
    dry_run: bool,
    report: &mut Report,
) {
    let Ok(entries) = std::fs::read_dir(firm_root) else {
        return;
    };
    for entry in entries.flatten() {
        let slug = entry.file_name().to_string_lossy().into_owned();
        if slug.starts_with('.') || wanted.contains_key(&slug) || !entry.path().is_dir() {
            continue;
        }
        let edits =
            super::watch::command(&entry.path(), "jj", &["diff", "--summary", "-r", "@"]).await;
        if !edits.is_ok_and(|edits| edits.trim().is_empty()) {
            report.notes.push(format!(
                "{FIRM_DIRECTORY}/{slug} backs no checkout but has local edits; left in place"
            ));
            continue;
        }
        report.changes.push(format!(
            "remove {FIRM_DIRECTORY}/{slug}: no checkout uses this Firm"
        ));
        if !dry_run {
            if let Err(error) = std::fs::remove_dir_all(entry.path()) {
                report
                    .missing
                    .push(format!("remove {FIRM_DIRECTORY}/{slug}: {error}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GENERIC: &[(&str, &str)] = &[
        (
            "records-subpoena",
            "---\nname: records-subpoena\n---\nScope widely.\n",
        ),
        (
            "email-draft",
            "---\nname: email-draft\n---\nGeneric voice.\n",
        ),
    ];

    fn project(root: &Path, code: &str, firm: &str) {
        let dir = root.join(code);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("navigator.yaml"),
            format!("project: {code}\nhost: https://example.test\nfirm: {firm}\n"),
        )
        .unwrap();
        std::fs::write(dir.join(".gitignore"), ".claude/skills/\n").unwrap();
    }

    fn firm_skill(root: &Path, slug: &str, name: &str, body: &str) {
        let dir = root
            .join(FIRM_DIRECTORY)
            .join(slug)
            .join("skills")
            .join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), body).unwrap();
    }

    fn firms(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(code, slug)| ((*code).to_string(), (*slug).to_string()))
            .collect()
    }

    fn skill_text(path: &Path) -> String {
        std::fs::read_to_string(path.join("SKILL.md")).unwrap()
    }

    /// A checkout bound to `host` and `firm`, or one naming neither.
    fn hosted(root: &Path, code: &str, binding: Option<(&str, Option<&str>)>) {
        let dir = root.join(code);
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = match binding {
            Some((host, Some(firm))) => {
                format!("project:\n  name: {code}\n  host: {host}\n  firm: {firm}\n")
            }
            Some((host, None)) => format!("project:\n  name: {code}\n  host: {host}\n"),
            None => format!("project: {code}\n"),
        };
        std::fs::write(dir.join("navigator.yaml"), manifest).unwrap();
        std::fs::write(dir.join(".gitignore"), ".claude/skills/\n").unwrap();
    }

    #[tokio::test]
    async fn sync_names_what_each_firm_or_host_is_missing_and_a_dry_run_writes_nothing() {
        use crate::projects::test_login::{answer, site};
        let (server, _login) = site(serde_json::json!([])).await;
        let host = server.uri();
        let libraries = serde_json::json!([
            {"slug": "sample-firm", "repository_url": "https://forge.example.test/sample-firm/library"},
            {"slug": "bare-firm", "repository_url": null},
            {"slug": "invalid slug", "repository_url": null},
        ]);
        answer(&server, "GET", "/app/api/firm-libraries", 200, libraries).await;
        let root = tempfile::tempdir().unwrap();
        let here = host.as_str();
        hosted(root.path(), "alpha", Some((here, Some("sample-firm"))));
        hosted(root.path(), "beta", Some((here, Some("unlisted-firm"))));
        hosted(root.path(), "gamma", Some((here, None)));
        hosted(root.path(), "delta", Some((here, Some("bare-firm"))));
        hosted(root.path(), "epsilon", Some((here, Some("invalid slug"))));
        hosted(root.path(), "eta", None);
        let elsewhere = ("signed-out.example.test", Some("sample-firm"));
        hosted(root.path(), "zeta", Some(elsewhere));

        let report = sync(root.path(), &host, true).await.unwrap();
        report.print(true);
        for expected in [
            "could not read https://signed-out.example.test: ".to_string(),
            format!("beta: Firm `unlisted-firm` is not one of your Firms on {host}"),
            format!("gamma: navigator.yaml names no `firm` and {host} lists 3 Firms for you"),
            format!("eta: navigator.yaml names no `firm` and {host} lists 3 Firms for you"),
            "Firm `bare-firm` has no Firm Library repository on its host".to_string(),
            "Firm slug `invalid slug` is not a valid folder name".to_string(),
        ] {
            assert!(
                report
                    .missing
                    .iter()
                    .any(|line| line.starts_with(&expected)),
                "missing `{expected}` in {:?}",
                report.missing
            );
        }
        assert!(!report.changes.is_empty());
        assert!(!root.path().join("AGENTS.md").exists());
        assert!(!root.path().join(FIRM_DIRECTORY).exists());
    }

    #[tokio::test]
    async fn a_firm_library_is_cloned_fast_forwarded_and_removed_only_without_local_edits() {
        use crate::projects::lifecycle::git_text;
        use crate::projects::test_login::{jj_stand_in, site};
        use wiremock::matchers::{method, path};
        let _jj = jj_stand_in();
        let temp = tempfile::tempdir().unwrap();
        let bare = temp.path().join("library.git");
        let seed = temp.path().join("seed");
        let skill = seed.join("skills/intake/SKILL.md");
        std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
        let publish = |body: &str| {
            std::fs::write(&skill, body).unwrap();
            git_text(&seed, &["add", "--all"]).unwrap();
            let commit = [
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--no-gpg-sign",
                "--quiet",
                "-m",
                "Library",
            ];
            git_text(&seed, &commit).unwrap();
            git_text(&seed, &["push", "--quiet", bare.to_str().unwrap(), "main"]).unwrap();
        };
        git_text(temp.path(), &["init", "--quiet", "--bare", "library.git"]).unwrap();
        git_text(&seed, &["init", "--quiet", "--initial-branch=main"]).unwrap();
        publish("---\nname: intake\n---\nFirst.\n");
        // Git reads the hosted library URL from the local bare repository.
        let url = "https://forge.example.test/sample-firm/library";
        std::env::set_var("GIT_CONFIG_COUNT", "1");
        std::env::set_var(
            "GIT_CONFIG_KEY_0",
            format!("url.{}.insteadOf", bare.display()),
        );
        std::env::set_var("GIT_CONFIG_VALUE_0", url);

        let (server, _login) = site(serde_json::json!([])).await;
        let host = server.uri();
        let root = temp.path().join("checkouts");
        hosted(&root, "alpha", None);
        let library = root.join(FIRM_DIRECTORY).join("sample-firm");
        let linked = root.join("alpha").join(CLAUDE_SKILLS).join("intake");
        {
            let listed = serde_json::json!([{"slug": "sample-firm", "repository_url": url}]);
            let _libraries = wiremock::Mock::given(method("GET"))
                .and(path("/app/api/firm-libraries"))
                .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(listed))
                .mount_as_scoped(&server)
                .await;
            let report = sync(&root, &host, false).await.unwrap();
            assert!(report.missing.is_empty(), "{:?}", report.missing);
            assert!(skill_text(&linked).ends_with("First.\n"));

            publish("---\nname: intake\n---\nSecond.\n");
            sync(&root, &host, false).await.unwrap();
            assert!(skill_text(&linked).ends_with("Second.\n"));

            std::fs::write(library.join(".jj/diff"), "M skills/intake/SKILL.md\n").unwrap();
            let report = sync(&root, &host, false).await.unwrap();
            assert_eq!(
                report.missing,
                vec![format!(
                    "Firm `sample-firm` library {url}: has local edits; land them through a \
                     reviewed change, then sync again"
                )]
            );
        }

        // The host lists no Firm now, so the library backs no checkout.
        let none = serde_json::json!([]);
        crate::projects::test_login::answer(&server, "GET", "/app/api/firm-libraries", 200, none)
            .await;
        let kept = sync(&root, &host, false).await.unwrap();
        assert!(kept.notes.iter().any(|note| note.contains("left in place")));
        assert!(library.is_dir());
        std::fs::remove_file(library.join(".jj/diff")).unwrap();
        let planned = sync(&root, &host, true).await.unwrap();
        let removal = format!("remove {FIRM_DIRECTORY}/sample-firm: no checkout uses this Firm");
        assert!(planned.changes.contains(&removal), "{:?}", planned.changes);
        assert!(library.is_dir());
        let removed = sync(&root, &host, false).await.unwrap();
        assert!(removed.changes.contains(&removal));
        assert!(!library.exists());
    }

    #[tokio::test]
    async fn the_sole_firm_backs_an_unnamed_checkout_and_an_unreachable_library_is_reported() {
        use crate::projects::test_login::{answer, site};
        let (server, _login) = site(serde_json::json!([])).await;
        let host = server.uri();
        let libraries =
            serde_json::json!([{"slug": "sample-firm", "repository_url": "/srv/git/library"}]);
        answer(&server, "GET", "/app/api/firm-libraries", 200, libraries).await;
        let root = tempfile::tempdir().unwrap();
        hosted(root.path(), "alpha", None);
        let stale = root.path().join(FIRM_DIRECTORY).join("former-firm");
        std::fs::create_dir_all(&stale).unwrap();

        let report = sync(root.path(), &host, false).await.unwrap();
        report.print(false);
        assert_eq!(
            report.missing,
            vec!["Firm `sample-firm` library /srv/git/library: unsupported repository transport"]
        );
        assert!(report.notes.contains(&format!(
            "{FIRM_DIRECTORY}/former-firm backs no checkout but has local edits; left in place"
        )));
        assert!(stale.is_dir());
        assert!(root.path().join("AGENTS.md").is_file());
    }

    #[test]
    fn an_empty_root_gets_agents_skills_and_relative_claude_links() {
        let root = tempfile::tempdir().unwrap();
        let report = materialize(root.path(), GENERIC, &BTreeMap::new(), false).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join("AGENTS.md")).unwrap(),
            WORKSPACE_AGENTS
        );
        let link = root.path().join(CLAUDE_SKILLS).join("records-subpoena");
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            Path::new("../../.agents/skills/records-subpoena")
        );
        assert_eq!(skill_text(&link), GENERIC[0].1);
        assert!(report.missing.is_empty(), "{:?}", report.missing);
        let again = materialize(root.path(), GENERIC, &BTreeMap::new(), false).unwrap();
        assert!(again.changes.is_empty(), "{:?}", again.changes);
    }

    #[test]
    fn a_drifted_owned_skill_is_restored_a_stale_one_removed_and_a_local_one_kept() {
        let root = tempfile::tempdir().unwrap();
        materialize(root.path(), GENERIC, &BTreeMap::new(), false).unwrap();
        let skills = root.path().join(SKILLS);
        std::fs::write(skills.join("records-subpoena/SKILL.md"), "edited").unwrap();
        std::fs::create_dir_all(skills.join("my-notes")).unwrap();
        std::fs::write(skills.join("my-notes/SKILL.md"), "mine").unwrap();

        let dry = materialize(root.path(), &GENERIC[..1], &BTreeMap::new(), true).unwrap();
        assert!(dry
            .changes
            .contains(&format!("restore {SKILLS}/records-subpoena")));
        assert!(dry
            .changes
            .contains(&format!("remove stale {SKILLS}/email-draft")));
        assert_eq!(
            std::fs::read_to_string(skills.join("records-subpoena/SKILL.md")).unwrap(),
            "edited"
        );

        materialize(root.path(), &GENERIC[..1], &BTreeMap::new(), false).unwrap();
        assert_eq!(skill_text(&skills.join("records-subpoena")), GENERIC[0].1);
        assert!(!skills.join("email-draft").exists());
        assert!(
            std::fs::symlink_metadata(root.path().join(CLAUDE_SKILLS).join("email-draft")).is_err()
        );
        assert_eq!(
            std::fs::read_to_string(skills.join("my-notes/SKILL.md")).unwrap(),
            "mine"
        );
    }

    #[test]
    fn a_hand_written_agents_file_is_left_alone() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("AGENTS.md"), "Firm rules\n").unwrap();
        let report = materialize(root.path(), GENERIC, &BTreeMap::new(), false).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join("AGENTS.md")).unwrap(),
            "Firm rules\n"
        );
        assert!(report
            .notes
            .iter()
            .any(|note| note.contains("hand-written")));
    }

    #[test]
    fn each_checkout_sees_only_its_own_firm_and_a_firm_skill_overrides_generic() {
        let root = tempfile::tempdir().unwrap();
        project(root.path(), "alpha-matter", "firm-a");
        project(root.path(), "beta-matter", "firm-b");
        firm_skill(root.path(), "firm-a", "email-draft", "A voice\n");
        firm_skill(root.path(), "firm-a", "certified-mail", "A vendor\n");
        firm_skill(root.path(), "firm-b", "certified-mail", "B vendor\n");
        let report = materialize(
            root.path(),
            GENERIC,
            &firms(&[("alpha-matter", "firm-a"), ("beta-matter", "firm-b")]),
            false,
        )
        .unwrap();
        assert!(report.missing.is_empty(), "{:?}", report.missing);

        let alpha = root.path().join("alpha-matter").join(CLAUDE_SKILLS);
        assert_eq!(skill_text(&alpha.join("email-draft")), "A voice\n");
        assert_eq!(skill_text(&alpha.join("certified-mail")), "A vendor\n");
        assert_eq!(skill_text(&alpha.join("records-subpoena")), GENERIC[0].1);
        let beta = root.path().join("beta-matter").join(CLAUDE_SKILLS);
        assert_eq!(skill_text(&beta.join("certified-mail")), "B vendor\n");
        assert_eq!(skill_text(&beta.join("email-draft")), GENERIC[1].1);
        assert!(report
            .notes
            .iter()
            .any(|note| note.contains("firm-a/email-draft overrides the generic email-draft")));

        let namespaced = root
            .path()
            .join(SKILLS)
            .join("firm-b")
            .join("certified-mail");
        assert_eq!(skill_text(&namespaced), "B vendor\n");
        assert_eq!(
            skill_text(
                &root
                    .path()
                    .join(CLAUDE_SKILLS)
                    .join("firm-a-certified-mail")
            ),
            "A vendor\n"
        );
    }

    #[test]
    fn a_removed_firm_library_takes_its_namespace_and_links_with_it() {
        let root = tempfile::tempdir().unwrap();
        project(root.path(), "alpha-matter", "firm-a");
        firm_skill(root.path(), "firm-a", "certified-mail", "A vendor\n");
        let scope = firms(&[("alpha-matter", "firm-a")]);
        materialize(root.path(), GENERIC, &scope, false).unwrap();
        std::fs::remove_dir_all(root.path().join(FIRM_DIRECTORY).join("firm-a")).unwrap();
        materialize(root.path(), GENERIC, &scope, false).unwrap();
        assert!(!root.path().join(SKILLS).join("firm-a").exists());
        let alpha = root.path().join("alpha-matter").join(CLAUDE_SKILLS);
        assert!(std::fs::symlink_metadata(alpha.join("certified-mail")).is_err());
        assert!(std::fs::symlink_metadata(
            root.path()
                .join(CLAUDE_SKILLS)
                .join("firm-a-certified-mail")
        )
        .is_err());
    }

    #[test]
    fn a_firm_skill_naming_a_project_code_or_lacking_skill_md_is_not_linked() {
        let root = tempfile::tempdir().unwrap();
        project(root.path(), "alpha-matter", "firm-a");
        firm_skill(
            root.path(),
            "firm-a",
            "leaky",
            "See alpha-matter for the facts.\n",
        );
        std::fs::create_dir_all(root.path().join(FIRM_DIRECTORY).join("firm-a/skills/empty"))
            .unwrap();
        let report = materialize(
            root.path(),
            GENERIC,
            &firms(&[("alpha-matter", "firm-a")]),
            false,
        )
        .unwrap();
        let alpha = root.path().join("alpha-matter").join(CLAUDE_SKILLS);
        assert!(std::fs::symlink_metadata(alpha.join("leaky")).is_err());
        assert!(report
            .missing
            .iter()
            .any(|m| m.contains("names Project `alpha-matter`")));
        assert!(report
            .missing
            .iter()
            .any(|m| m.contains("empty has no SKILL.md")));
    }

    #[test]
    fn a_checkout_without_the_ignore_line_is_reported_not_written() {
        let root = tempfile::tempdir().unwrap();
        project(root.path(), "alpha-matter", "firm-a");
        std::fs::write(root.path().join("alpha-matter/.gitignore"), "/documents/\n").unwrap();
        let report = materialize(root.path(), GENERIC, &BTreeMap::new(), false).unwrap();
        assert!(!root
            .path()
            .join("alpha-matter")
            .join(CLAUDE_SKILLS)
            .exists());
        assert!(report
            .missing
            .iter()
            .any(|m| m.contains("lacks `.claude/skills/`")));
    }
}
