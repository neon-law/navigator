//! Interactive Project opening followed by the canonical local repository
//! scaffold and, optionally, the Project's source repository on its forge.
//!
//! The flow is resumable. A fresh run prompts, checks the attestation, opens
//! the Project through the existing admission door, writes the canonical
//! source-only tree, and asks the server to create or adopt the repository.
//! If a step fails after the Project opened, `--resume <code>` skips the
//! prompts and the open, writes or repairs the tree, and retries provisioning.
//! A resumed empty directory takes its client DRI from the live Project.
//! `--client-email` is optional and must name that live contact. When the live
//! row has a repository URL, resume sets `origin` and its push URL to it.
//! `--no-repository` keeps the Project repository-less. No client fact enters
//! the tree beyond the manifest's DRI handles, and nothing is copied from a
//! sibling matter.
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;

use anyhow::{bail, Context, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaffoldPlan {
    pub name: String,
    pub code: String,
    pub client_name: String,
    pub client_email: String,
    pub entity_name: Option<String>,
    pub jurisdiction: Option<String>,
}

trait Prompter {
    fn ask(&mut self, label: &str) -> Result<Option<String>>;
}

struct TerminalPrompter;
impl Prompter for TerminalPrompter {
    fn ask(&mut self, label: &str) -> Result<Option<String>> {
        print!("{label}: ");
        io::stdout().flush().context("write prompt")?;
        let mut value = String::new();
        if io::stdin().read_line(&mut value)? == 0 {
            return Ok(None);
        }
        Ok(Some(value.trim().to_string()))
    }
}

fn collect(prompter: &mut impl Prompter) -> Result<Option<ScaffoldPlan>> {
    let Some(name) = prompter.ask("Project name")? else {
        return Ok(None);
    };
    let Some(code) = prompter.ask("Immutable Project code")? else {
        return Ok(None);
    };
    let Some(client_name) = prompter.ask("Client contact name")? else {
        return Ok(None);
    };
    let Some(client_email) = prompter.ask("Client contact email")? else {
        return Ok(None);
    };
    let Some(entity) = prompter.ask("Existing Entity name (blank creates a Human Entity)")? else {
        return Ok(None);
    };
    let (entity_name, jurisdiction) = if entity.is_empty() {
        let Some(jurisdiction) = prompter.ask("Client home jurisdiction")? else {
            return Ok(None);
        };
        (None, Some(jurisdiction))
    } else {
        (Some(entity), None)
    };
    let Some(attestation) = prompter.ask("I checked conflicts and either none prevent opening or this is not legal advice. Type yes to attest")? else {
        return Ok(None);
    };
    if !attestation.eq_ignore_ascii_case("yes") {
        return Ok(None);
    }
    let plan = ScaffoldPlan {
        name,
        code,
        client_name,
        client_email,
        entity_name,
        jurisdiction,
    };
    validate(&plan)?;
    Ok(Some(plan))
}

fn validate(plan: &ScaffoldPlan) -> Result<()> {
    if !model::projects::is_valid_code(&plan.code) {
        bail!("invalid immutable Project code `{}`", plan.code);
    }
    if plan.name.trim().is_empty() || plan.client_name.trim().is_empty() {
        bail!("Project and client names must not be blank");
    }
    if !plan.client_email.contains('@') {
        bail!("client DRI must be an email address");
    }
    if plan.entity_name.is_none() && plan.jurisdiction.as_deref().is_none_or(str::is_empty) {
        bail!("a jurisdiction is required when creating a Human Entity");
    }
    Ok(())
}

/// What a scaffold run does after the Project exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaffoldOptions {
    /// Ask the server to create or adopt the Project's source repository.
    pub provision_repository: bool,
    pub firm: Option<String>,
    pub brand: Option<String>,
    /// The client contact a resumed scaffold may write as the manifest's client
    /// DRI. When omitted, an empty directory uses the live Project's client
    /// contact. A supplied address must be that contact.
    pub client_email: Option<String>,
}

/// Whether `directory` holds nothing yet.
fn is_empty_dir(directory: &Path) -> Result<bool> {
    if !directory.exists() {
        return Ok(true);
    }
    Ok(directory
        .read_dir()
        .context("inspect scaffold destination")?
        .next()
        .is_none())
}

/// Whether the manifest in `directory` is this Project's.
fn manifest_names(directory: &Path, code: &str) -> bool {
    std::fs::read_to_string(directory.join(crate::projects::repository::PROJECT_MANIFEST))
        .ok()
        .and_then(|text| serde_yaml::from_str::<serde_yaml::Value>(&text).ok())
        .and_then(|manifest| {
            manifest
                .get("project")
                .and_then(|project| project.get("name"))
                .and_then(serde_yaml::Value::as_str)
                .map(|name| name == code)
        })
        .unwrap_or(false)
}

/// `navigator project scaffold [--resume <code>] [--no-repository]`.
pub async fn run(
    host: Option<&str>,
    directory: &Path,
    resume: Option<&str>,
    options: ScaffoldOptions,
) -> ExitCode {
    match run_inner(host, directory, resume, options).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("navigator: {error:#}");
            ExitCode::from(2)
        }
    }
}

fn validate_scaffold_start(
    directory: &Path,
    resume: Option<&str>,
    options: &ScaffoldOptions,
) -> Result<bool> {
    // A checkout opened with an empty source holds only untracked remnants, so
    // a resume writes the whole scaffold into it as it would an empty directory.
    let empty = is_empty_dir(directory)?
        || (resume.is_some() && crate::projects::repository::is_empty_source(directory));
    if let Some(code) = resume {
        if !model::projects::is_valid_code(code) {
            bail!("invalid immutable Project code `{code}`");
        }
        if options
            .client_email
            .as_deref()
            .is_some_and(|email| !email.contains('@'))
        {
            bail!("client DRI must be an email address");
        }
        if !empty && !manifest_names(directory, code) {
            bail!(
                "{} holds something other than Project `{code}`'s scaffold; resume in its own \
                 checkout or an empty directory",
                directory.display()
            );
        }
    } else if !empty {
        bail!(
            "scaffold destination is not empty: {}; use --resume <code> to finish an \
             interrupted scaffold",
            directory.display()
        );
    }
    if resume.is_none() {
        if !io::stdin().is_terminal() {
            bail!(
                "interactive Project scaffold requires a terminal. For non-interactive use, run \
                 `navigator project create`, then `navigator project scaffold --resume <code>`."
            );
        }
        if options.client_email.is_some() {
            bail!(
                "--client-email requires --resume; the interactive form asks for the client email"
            );
        }
    }
    Ok(empty)
}

async fn run_inner(
    host: Option<&str>,
    directory: &Path,
    resume: Option<&str>,
    options: ScaffoldOptions,
) -> Result<ExitCode> {
    let empty = validate_scaffold_start(directory, resume, &options)?;
    let (base, _) = crate::remote::resolve(host)?;
    let code = if let Some(code) = resume {
        if !crate::remote::project_code_exists(host, code).await? {
            bail!("Project `{code}` is not open on this deployment; run scaffold without --resume");
        }
        code.to_string()
    } else {
        let Some(plan) = collect(&mut TerminalPrompter)? else {
            eprintln!("navigator: Project scaffold cancelled before writes");
            return Ok(ExitCode::SUCCESS);
        };
        if crate::remote::project_code_exists(host, &plan.code).await? {
            bail!(
                "Project code `{}` already exists; codes are immutable (use --resume {} to \
                 finish its scaffold)",
                plan.code,
                plan.code
            );
        }
        let opened = crate::remote::projects_create_named(
            host,
            &plan.name,
            &plan.code,
            &plan.client_name,
            &plan.client_email,
            plan.entity_name.as_deref(),
            plan.jurisdiction.as_deref(),
            true,
            None,
            options.firm.as_deref(),
            options.brand.as_deref(),
        )
        .await;
        if opened != ExitCode::SUCCESS {
            return Ok(opened);
        }
        write_tree(&base, directory, &plan.code, Some(&plan.client_email))?;
        plan.code
    };
    let mut repository_url = None;
    let mut legal_plan = None;
    if resume.is_some() {
        let handles = live_handles(host, &code).await?;
        repository_url = handles.repository_url.filter(|url| !url.is_empty());
        legal_plan = handles.legal_plan;
        let supplied = options.client_email.as_deref();
        if !empty && supplied.is_some() {
            client_dri_for_resume(&handles.client_dri, supplied)?;
        }
        if empty {
            let email = client_dri_for_resume(&handles.client_dri, supplied)?;
            if crate::projects::repository::is_empty_source(directory) {
                for name in crate::projects::repository::remove_root_licence_files(directory)? {
                    println!("removed {name}: a Project repository carries no licence file (Y028)");
                }
            }
            write_tree(&base, directory, &code, Some(&email))?;
        } else {
            let outcome = crate::projects::repository::validate_gate(directory, Some(&code), true);
            if outcome.errors != 0 {
                bail!(
                    "the existing scaffold fails its gate ({} errors)",
                    outcome.errors
                );
            }
        }
    }
    write_live_identity(host, directory, &code, legal_plan.as_deref()).await?;
    println!(
        "Generated the canonical Project repository at {}",
        directory.display()
    );
    if !options.provision_repository {
        ensure_origin(directory, repository_url.as_deref(), false)?;
        println!("Repository provisioning skipped; the Project stays repository-less.");
        return Ok(ExitCode::SUCCESS);
    }
    let provisioned = provision(host, &code).await?;
    let Provisioned::Ready(provisioned_url) = provisioned else {
        return Ok(ExitCode::from(2));
    };
    if provisioned_url.as_ref().is_some_and(|url| !url.is_empty()) {
        repository_url = provisioned_url;
    }
    ensure_origin(directory, repository_url.as_deref(), true)?;
    let imported = crate::projects::source_import::run(host, &code, directory, None, true).await;
    if imported != ExitCode::SUCCESS {
        return Ok(imported);
    }
    super::watch::sync_project(host, &code, None).await?;
    Ok(ExitCode::SUCCESS)
}

/// Record the opened row's ownership and fill a missing Legal Plan on a resumed scaffold.
async fn write_live_identity(
    host: Option<&str>,
    directory: &Path,
    code: &str,
    legal_plan: Option<&str>,
) -> Result<()> {
    let (base, token) = crate::remote::resolve(host)?;
    let project = super::surfaces::resolve_visible_project(&base, &token, code).await?;
    let firm = project
        .firm_id
        .context("opened Project has no owning Firm")?;
    let brand = project.brand.context("opened Project has no brand")?;
    let path = directory.join(super::manifest::FILE);
    let contents = std::fs::read_to_string(&path)?;
    let contents = super::set::replace_project_key(&contents, "firm", &firm.to_string())?;
    let mut contents = super::set::replace_project_key(&contents, "brand", &brand)?;
    let current = super::manifest::parse(&contents).map_err(anyhow::Error::msg)?;
    if let (None, Some(name)) = (current.legal_plan, legal_plan) {
        contents = super::set::replace_project_key(&contents, "legal_plan", name)?;
    }
    std::fs::write(&path, contents)?;
    Ok(())
}

/// Write the canonical tree with `client_email` as the client DRI.
fn write_tree(base: &str, directory: &Path, code: &str, client_email: Option<&str>) -> Result<()> {
    let manifest_host = url::Url::parse(base)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .context("deployment URL has no host")?;
    if crate::letterhead::resolve_brand(None, Some(&manifest_host)).is_err() {
        eprintln!(
            "navigator: warning: deployment host is missing from the Brand registry; \
             continuing with the configured host"
        );
    }
    let lawyer_email = crate::credentials::load(&crate::credentials::default_credentials_path())
        .ok()
        .and_then(|credentials| {
            credentials
                .get(base)
                .and_then(|credential| credential.person_email.clone())
        })
        .context(
            "the saved Navigator login has no lawyer identity; run `navigator site login` again",
        )?;
    crate::projects::repository::emit_scaffold_for(
        directory,
        code,
        &manifest_host,
        &lawyer_email,
        client_email.context("a scaffold needs the client contact email")?,
        crate::projects::repository::forge_backend_from_env()?,
    )
    .context("Project opened but repository scaffold failed; rerun with --resume")
}

/// The client DRI a resumed scaffold may write. A supplied address is accepted
/// only when the live Project already names it.
fn client_dri_for_resume(live: &[String], supplied: Option<&str>) -> Result<String> {
    if let Some(supplied) = supplied {
        if !supplied.contains('@') {
            bail!("client DRI must be an email address");
        }
        if !live.iter().any(|email| email == supplied) {
            bail!(
                "the supplied client contact is not a client DRI on the live Project; \
                 the manifest keeps the live client contact"
            );
        }
        return Ok(supplied.to_string());
    }
    match live {
        [email] => Ok(email.clone()),
        [] => {
            bail!("the live Project has no client contact; the manifest cannot name a client DRI")
        }
        _ => bail!(
            "the live Project has more than one client contact; pass --client-email for the \
             contact the manifest should name"
        ),
    }
}

async fn live_handles(
    host: Option<&str>,
    code: &str,
) -> Result<model::project_surfaces::ProjectHandles> {
    let (base, token) = crate::remote::resolve(host)?;
    let project_id = super::surfaces::resolve_project_id(&base, &token, code).await?;
    super::surfaces::get_handles(&base, &token, project_id).await
}

/// Set `origin` and its push URL from the live repository URL.
fn configure_repository_remote(directory: &Path, url: &str) -> Result<()> {
    let remotes = git_stdout(directory, &["remote"])?;
    if remotes.lines().any(|name| name == "origin") {
        // Read the stored value. `git remote get-url` applies insteadOf rewrites
        // and can substitute credentials that must not be compared or printed.
        let current = git_stdout(
            directory,
            &["config", "--local", "--get", "remote.origin.url"],
        )?;
        if current != url {
            bail!("origin is `{current}`, which differs from the live repository URL `{url}`");
        }
    } else {
        git_ok(directory, &["remote", "add", "origin", url])?;
    }
    git_ok(
        directory,
        &["config", "--local", "remote.origin.pushurl", url],
    )?;
    println!("origin: {url}");
    Ok(())
}

fn ensure_origin(directory: &Path, repository_url: Option<&str>, provisioned: bool) -> Result<()> {
    match repository_url {
        Some(url) => configure_repository_remote(directory, url),
        None if provisioned => {
            println!(
                "the live Project has no repository URL; add an origin remote by hand before pushing"
            );
            Ok(())
        }
        None => Ok(()),
    }
}

fn git_ok(directory: &Path, args: &[&str]) -> Result<()> {
    let output = super::lifecycle::git(directory, args)?;
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "git {} failed ({}): {}",
        args.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

fn git_stdout(directory: &Path, args: &[&str]) -> Result<String> {
    let output = super::lifecycle::git(directory, args)?;
    if !output.status.success() {
        bail!(
            "git {} failed ({}): {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

enum Provisioned {
    Ready(Option<String>),
    Failed,
}

/// Ask the server to create or adopt the repository, and report each
/// resource's outcome as the server recorded it.
async fn provision(host: Option<&str>, code: &str) -> Result<Provisioned> {
    let (base, token) = crate::remote::resolve(host)?;
    let project_id = super::surfaces::resolve_project_id(&base, &token, code).await?;
    match super::surfaces::post_reconcile(&base, &token, project_id).await {
        Ok(surfaces) => {
            println!(
                "repository: {:?}{}",
                surfaces.repository_status,
                surfaces
                    .repository_url
                    .as_deref()
                    .map(|url| format!(" ({url})"))
                    .unwrap_or_default()
            );
            if let Some(detail) = surfaces.repository_detail.as_deref() {
                println!("repository detail: {detail}");
            }
            println!("drive: {:?}", surfaces.drive_status);
            Ok(Provisioned::Ready(surfaces.repository_url))
        }
        Err(error) => {
            eprintln!(
                "navigator: the Project and its scaffold exist, but provisioning failed: \
                 {error:#}\nretry with `navigator project scaffold --resume {code}`"
            );
            Ok(Provisioned::Failed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct ScriptedPrompter {
        answers: VecDeque<Option<String>>,
        seen: Vec<String>,
    }
    impl Prompter for ScriptedPrompter {
        fn ask(&mut self, label: &str) -> Result<Option<String>> {
            self.seen.push(label.to_string());
            Ok(self.answers.pop_front().flatten())
        }
    }

    #[test]
    fn cancellation_happens_before_any_write() {
        let root = tempfile::tempdir().unwrap();
        let mut prompts = ScriptedPrompter {
            answers: [Some("Matter".into()), None].into(),
            seen: Vec::new(),
        };
        assert!(collect(&mut prompts).unwrap().is_none());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn attestation_is_the_last_prompt_before_a_plan_can_be_opened() {
        let mut prompts = ScriptedPrompter {
            answers: [
                "Matter",
                "matter-one",
                "Client Name",
                "client@example.com",
                "Acme LLC",
                "yes",
            ]
            .into_iter()
            .map(|s| Some(s.to_string()))
            .collect(),
            seen: Vec::new(),
        };
        let plan = collect(&mut prompts).unwrap().unwrap();
        assert_eq!(
            prompts.seen.last().map(String::as_str),
        Some("I checked conflicts and either none prevent opening or this is not legal advice. Type yes to attest")
        );
        assert!(plan.entity_name.is_some());
    }

    #[test]
    fn missing_attestation_cancels_without_a_plan() {
        let mut prompts = ScriptedPrompter {
            answers: [
                "Matter",
                "matter-one",
                "Client Name",
                "client@example.com",
                "Acme LLC",
                "no",
            ]
            .into_iter()
            .map(|s| Some(s.to_string()))
            .collect(),
            seen: Vec::new(),
        };
        assert!(collect(&mut prompts).unwrap().is_none());
    }

    #[tokio::test]
    async fn resume_refuses_a_directory_holding_another_projects_scaffold() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("navigator.yaml"),
            "version: 1\nproject:\n  name: sibling-matter\n",
        )
        .unwrap();
        assert!(manifest_names(root.path(), "sibling-matter"));
        assert!(!manifest_names(root.path(), "matter-one"));
        let options = ScaffoldOptions {
            firm: None,
            brand: None,
            provision_repository: false,
            client_email: None,
        };
        let error = run_inner(None, root.path(), Some("matter-one"), options.clone())
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("something other than"),
            "{error}"
        );
        // A fresh scaffold never writes into a used directory either.
        let error = run_inner(None, root.path(), None, options)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("--resume"), "{error}");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn jurisdiction_is_required_for_a_new_human_entity() {
        let plan = ScaffoldPlan {
            name: "Matter".into(),
            code: "matter-one".into(),
            client_name: "Client".into(),
            client_email: "client@example.com".into(),
            entity_name: None,
            jurisdiction: Some("Nevada".into()),
        };
        assert!(validate(&plan).is_ok());
        assert!(validate(&ScaffoldPlan {
            jurisdiction: None,
            ..plan
        })
        .is_err());
    }

    fn scripted(answers: &[Option<&str>]) -> ScriptedPrompter {
        ScriptedPrompter {
            answers: answers.iter().map(|a| a.map(str::to_string)).collect(),
            seen: Vec::new(),
        }
    }

    #[test]
    fn a_blank_entity_asks_for_the_home_jurisdiction_of_a_new_human_entity() {
        let mut prompts = scripted(&[
            Some("Matter"),
            Some("matter-one"),
            Some("Pat Example"),
            Some("pat@example.com"),
            Some(""),
            Some("Nevada"),
            Some("YES"),
        ]);
        let plan = collect(&mut prompts).unwrap().unwrap();
        assert_eq!(plan.entity_name, None);
        assert_eq!(plan.jurisdiction.as_deref(), Some("Nevada"));
        assert_eq!(prompts.seen[5], "Client home jurisdiction");
        assert_eq!(prompts.seen.len(), 7);
    }

    #[test]
    fn closing_input_at_any_prompt_cancels_without_a_plan() {
        let full = [
            "Matter",
            "matter-one",
            "Pat Example",
            "pat@example.com",
            "",
            "Nevada",
            "yes",
        ];
        for cut in 0..full.len() {
            let answers: Vec<Option<&str>> = full[..cut].iter().copied().map(Some).collect();
            let mut prompts = scripted(&answers);
            assert!(
                collect(&mut prompts).unwrap().is_none(),
                "input closed after {cut} answers"
            );
            assert_eq!(prompts.seen.len(), cut + 1);
        }
    }

    #[test]
    fn an_attested_plan_is_still_validated() {
        let mut prompts = scripted(&[
            Some("Matter"),
            Some("Not A Code"),
            Some("Pat Example"),
            Some("pat@example.com"),
            Some("Example LLC"),
            Some("yes"),
        ]);
        let error = collect(&mut prompts).unwrap_err();
        assert!(error.to_string().contains("invalid immutable Project code"));
    }

    #[test]
    fn validation_names_each_unusable_answer() {
        let plan = ScaffoldPlan {
            name: "Matter".into(),
            code: "matter-one".into(),
            client_name: "Pat Example".into(),
            client_email: "pat@example.com".into(),
            entity_name: Some("Example LLC".into()),
            jurisdiction: None,
        };
        assert!(validate(&plan).is_ok());
        for (broken, message) in [
            (
                ScaffoldPlan {
                    name: "  ".into(),
                    ..plan.clone()
                },
                "must not be blank",
            ),
            (
                ScaffoldPlan {
                    client_name: String::new(),
                    ..plan.clone()
                },
                "must not be blank",
            ),
            (
                ScaffoldPlan {
                    client_email: "pat.example.com".into(),
                    ..plan.clone()
                },
                "email address",
            ),
            (
                ScaffoldPlan {
                    entity_name: None,
                    jurisdiction: Some(String::new()),
                    ..plan.clone()
                },
                "jurisdiction is required",
            ),
        ] {
            let error = validate(&broken).unwrap_err().to_string();
            assert!(error.contains(message), "{error}");
        }
    }

    #[test]
    fn a_missing_or_empty_destination_is_empty_and_a_used_one_is_not() {
        let root = tempfile::tempdir().unwrap();
        assert!(is_empty_dir(&root.path().join("not-yet")).unwrap());
        assert!(is_empty_dir(root.path()).unwrap());
        std::fs::write(root.path().join("notes.txt"), "x").unwrap();
        assert!(!is_empty_dir(root.path()).unwrap());
    }

    #[test]
    fn only_a_parsed_manifest_naming_the_code_claims_a_directory() {
        let root = tempfile::tempdir().unwrap();
        assert!(!manifest_names(root.path(), "matter-one"), "no manifest");
        let manifest = root.path().join("navigator.yaml");
        std::fs::write(&manifest, "version: [unclosed\n").unwrap();
        assert!(!manifest_names(root.path(), "matter-one"), "malformed");
        std::fs::write(&manifest, "version: 1\n").unwrap();
        assert!(!manifest_names(root.path(), "matter-one"), "no project");
        std::fs::write(&manifest, "version: 1\nproject:\n  name: 7\n").unwrap();
        assert!(!manifest_names(root.path(), "matter-one"), "non-string");
        std::fs::write(&manifest, "version: 1\nproject:\n  name: matter-one\n").unwrap();
        assert!(manifest_names(root.path(), "matter-one"));
    }

    #[tokio::test]
    async fn resume_validates_the_code_before_reading_any_login() {
        let root = tempfile::tempdir().unwrap();
        let error = run_inner(
            None,
            root.path(),
            Some("Not A Code"),
            ScaffoldOptions {
                firm: None,
                brand: None,
                provision_repository: true,
                client_email: None,
            },
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("invalid immutable Project code"),
            "{error}"
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn resume_into_an_empty_directory_allows_a_missing_client_email() {
        let root = tempfile::tempdir().unwrap();
        let empty = validate_scaffold_start(
            root.path(),
            Some("matter-one"),
            &ScaffoldOptions {
                firm: None,
                brand: None,
                provision_repository: false,
                client_email: None,
            },
        )
        .unwrap();
        assert!(empty);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn resume_rejects_a_client_email_that_is_not_an_address() {
        let root = tempfile::tempdir().unwrap();
        let error = run_inner(
            None,
            root.path(),
            Some("matter-one"),
            ScaffoldOptions {
                firm: None,
                brand: None,
                provision_repository: false,
                client_email: Some("not-an-email".to_string()),
            },
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("email address"), "{error}");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn resume_client_dri_uses_the_single_live_contact() {
        let live = vec!["client@example.com".to_string()];
        assert_eq!(
            client_dri_for_resume(&live, None).unwrap(),
            "client@example.com"
        );
        assert_eq!(
            client_dri_for_resume(&live, Some("client@example.com")).unwrap(),
            "client@example.com"
        );
        let error = client_dri_for_resume(&live, Some("other@example.com")).unwrap_err();
        assert!(error.to_string().contains("not a client DRI"), "{error}");
        assert!(!error.to_string().contains("other@example.com"));
        let missing = client_dri_for_resume(&[], None).unwrap_err();
        assert!(
            missing.to_string().contains("no client contact"),
            "{missing}"
        );
    }

    #[test]
    fn resume_client_dri_requires_a_choice_when_several_contacts_exist() {
        let live = vec!["one@example.com".to_string(), "two@example.com".to_string()];
        let error = client_dri_for_resume(&live, None).unwrap_err();
        assert!(error.to_string().contains("--client-email"), "{error}");
        assert_eq!(
            client_dri_for_resume(&live, Some("two@example.com")).unwrap(),
            "two@example.com"
        );
    }

    #[test]
    fn configure_repository_remote_sets_origin_and_pushurl() {
        let root = tempfile::tempdir().unwrap();
        let url = "https://github.com/example/matter";
        git_ok(root.path(), &["init", "--quiet", "--initial-branch=main"]).unwrap();
        configure_repository_remote(root.path(), url).unwrap();
        assert_eq!(
            git_stdout(
                root.path(),
                &["config", "--local", "--get", "remote.origin.url"]
            )
            .unwrap(),
            url
        );
        assert_eq!(
            git_stdout(
                root.path(),
                &["config", "--local", "--get", "remote.origin.pushurl"]
            )
            .unwrap(),
            url
        );
        configure_repository_remote(root.path(), url).unwrap();
        let error = configure_repository_remote(root.path(), "https://github.com/example/other")
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("differs from the live repository URL"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn run_maps_a_refusal_to_exit_two() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("notes.txt"), "x").unwrap();
        let code = run(
            None,
            root.path(),
            None,
            ScaffoldOptions {
                firm: None,
                brand: None,
                provision_repository: false,
                client_email: None,
            },
        )
        .await;
        assert_eq!(code, ExitCode::from(2));
    }

    fn git_in(dir: &Path, args: &[&str]) {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
    }

    fn resume_options() -> ScaffoldOptions {
        ScaffoldOptions {
            firm: None,
            brand: None,
            provision_repository: false,
            client_email: None,
        }
    }

    /// A repository opened with an empty source: `.git`, nothing tracked, and
    /// untracked remnants such as the root LICENSE.
    fn half_opened() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "--quiet"]);
        std::fs::write(dir.path().join("LICENSE"), "licence\n").unwrap();
        dir
    }

    #[test]
    fn resume_treats_a_checkout_with_an_empty_source_as_fresh_but_not_one_with_content() {
        let dir = half_opened();
        assert!(
            validate_scaffold_start(dir.path(), Some("pitch-acme"), &resume_options()).unwrap()
        );
        // Without --resume it is still a used directory.
        assert!(validate_scaffold_start(dir.path(), None, &resume_options()).is_err());

        git_in(dir.path(), &["add", "LICENSE"]);
        let error =
            validate_scaffold_start(dir.path(), Some("pitch-acme"), &resume_options()).unwrap_err();
        assert!(
            error.to_string().contains("something other than"),
            "{error}"
        );
    }

    /// ENG-1416 acceptance: a Project opened with its repository left empty is
    /// finished by one documented command, and its tree then passes the gate.
    #[tokio::test]
    async fn resume_finishes_a_half_opened_checkout_into_a_gate_clean_tree() {
        use crate::projects::test_login::{answer, site};
        const FIRM: &str = "11111111-1111-4111-8111-111111111111";
        let id = uuid::Uuid::new_v4();
        let (server, _login) = site(serde_json::json!([
            {"id": id, "code": "pitch-acme", "firm_id": FIRM, "brand": "neon"}
        ]))
        .await;
        answer(
            &server,
            "GET",
            &format!("/app/api/project-surfaces/{id}"),
            200,
            serde_json::json!({
                "code": "pitch-acme", "repository_url": null,
                "lawyer_dri": ["lawyer@example.test"], "client_dri": ["client@example.com"],
                "private_slack_channel": null, "private_notion_page": null,
                "calendar_id": null, "legal_plan": null, "dangling_dri_person_ids": [],
            }),
        )
        .await;
        let dir = half_opened();
        let host = server.uri();
        let code = run_inner(
            Some(&host),
            dir.path(),
            Some("pitch-acme"),
            resume_options(),
        )
        .await
        .unwrap();
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(dir.path().join("README.md").is_file());
        assert!(
            !dir.path().join("LICENSE").exists(),
            "the licence remnant the gate refuses is removed"
        );
        let manifest = crate::projects::manifest::read(dir.path()).unwrap();
        assert_eq!(manifest.firm.as_deref(), Some(FIRM));
        let outcome =
            crate::projects::repository::validate_gate(dir.path(), Some("pitch-acme"), true);
        assert_eq!(outcome.errors, 0);
        assert!(
            crate::projects::repository::needs_scaffold_resume(dir.path()).is_none(),
            "the report clears once the scaffold exists"
        );
    }
}
