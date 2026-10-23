//! Interactive Project opening followed by the canonical local repository
//! scaffold and, optionally, the Project's source repository on its forge.
//!
//! The flow is resumable. A fresh run prompts, checks the attestation, opens
//! the Project through the existing admission door, writes the canonical
//! source-only tree, and asks the server to create or adopt the repository.
//! If a step fails after the Project opened, `--resume <code>` skips the
//! prompts and the open, writes or repairs the tree, and retries provisioning.
//! `--no-repository` keeps the Project repository-less. No client fact enters
//! the tree beyond the manifest's DRI handles, and nothing is copied from a
//! sibling matter.
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use anyhow::{bail, Context, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScaffoldPlan {
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
    if !store::projects::is_valid_code(&plan.code) {
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
pub(crate) struct ScaffoldOptions {
    /// Ask the server to create or adopt the Project's source repository.
    pub provision_repository: bool,
    /// The client contact a resumed scaffold writes as the manifest's client
    /// DRI. Required only when `--resume` rebuilds the tree in an empty
    /// directory, where the original prompt answer is gone.
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

async fn run_inner(
    host: Option<&str>,
    directory: &Path,
    resume: Option<&str>,
    options: ScaffoldOptions,
) -> Result<ExitCode> {
    let empty = is_empty_dir(directory)?;
    if let Some(code) = resume {
        if !store::projects::is_valid_code(code) {
            bail!("invalid immutable Project code `{code}`");
        }
        if empty
            && !options
                .client_email
                .as_deref()
                .is_some_and(|email| email.contains('@'))
        {
            bail!(
                "resuming `{code}` into an empty directory rebuilds its manifest; pass \
                 --client-email <email> for the client DRI"
            );
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
        )
        .await;
        if opened != ExitCode::SUCCESS {
            return Ok(opened);
        }
        write_tree(&base, directory, &plan.code, Some(&plan.client_email))?;
        plan.code
    };
    if resume.is_some() {
        if empty {
            write_tree(&base, directory, &code, options.client_email.as_deref())?;
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
    println!(
        "Generated the canonical Project repository at {}",
        directory.display()
    );
    if !options.provision_repository {
        println!("Repository provisioning skipped; the Project stays repository-less.");
        return Ok(ExitCode::SUCCESS);
    }
    let provisioned = provision(host, &code).await?;
    if provisioned != ExitCode::SUCCESS {
        return Ok(provisioned);
    }
    Ok(crate::projects::source_import::run(host, &code, directory, None, false).await)
}

/// Write the canonical tree with `client_email` as the client DRI.
fn write_tree(base: &str, directory: &Path, code: &str, client_email: Option<&str>) -> Result<()> {
    let manifest_host = url::Url::parse(base)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .context("deployment URL has no host")?;
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

/// Ask the server to create or adopt the repository, and report each
/// resource's outcome as the server recorded it.
async fn provision(host: Option<&str>, code: &str) -> Result<ExitCode> {
    let (base, token) = crate::remote::resolve(host)?;
    let project_id = crate::projects::surfaces::resolve_project_id(&base, &token, code).await?;
    match crate::projects::surfaces::post_reconcile(&base, &token, project_id).await {
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
            Ok(ExitCode::SUCCESS)
        }
        Err(error) => {
            eprintln!(
                "navigator: the Project and its scaffold exist, but provisioning failed: \
                 {error:#}\nretry with `navigator project scaffold --resume {code}`"
            );
            Ok(ExitCode::from(2))
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

    #[tokio::test]
    async fn resuming_into_an_empty_directory_needs_the_client_email() {
        let root = tempfile::tempdir().unwrap();
        for client_email in [None, Some("not-an-email".to_string())] {
            let error = run_inner(
                None,
                root.path(),
                Some("matter-one"),
                ScaffoldOptions {
                    provision_repository: false,
                    client_email,
                },
            )
            .await
            .unwrap_err();
            assert!(error.to_string().contains("--client-email"), "{error}");
        }
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
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
                provision_repository: false,
                client_email: None,
            },
        )
        .await;
        assert_eq!(code, ExitCode::from(2));
    }
}
