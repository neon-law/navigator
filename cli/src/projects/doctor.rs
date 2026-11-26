//! `navigator project doctor` — read-only Project workspace diagnostics.
//!
//! Verify a machine and a Project workspace *before* Navigator is allowed to
//! create anything. Every check here reads: no folder is created, no file is
//! written, no repository is provisioned, and no network call is made. The
//! command exists so that provisioning (`projects surfaces reconcile`) can
//! assume a verified deployment instead of discovering a misconfiguration
//! halfway through a partially created Drive tree.
//!
//! The diagnosis is a pure function of three inputs — an environment lookup,
//! the stored credentials, and a clock — so the production and staging
//! configurations are both testable without mutating process-global
//! environment variables (which races under parallel tests) and without
//! touching a real Drive. No local Drive mount is read or required.
//!
//! Every missing deployment setting is named in one run. Drive ingest is
//! optional: a deployment that sets none of its settings has it disabled, a
//! warning that leaves the machine healthy. A deployment that sets some of them
//! is misconfigured, and the report names every missing key at once. A
//! Workspace, Drive, or identity mismatch is a hard failure: the command exits
//! nonzero rather than reporting a warning an operator might scroll past.

use std::process::ExitCode;

use model::workspace::{
    DriveSetup, WorkspaceConfig, WorkspaceConfigError, NAVIGATOR_GITHUB_ORG, NAVIGATOR_GIT_HOST,
};

use crate::credentials::{self, Credentials};
use crate::palette;

/// The outcome of one check.
///
/// `Warn` is reserved for configuration that is genuinely optional — a Drive
/// ingest a deployment does not use, or a site the operator has not logged
/// into.
/// Anything that would make provisioning write to the wrong Workspace is
/// `Fail`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

/// One named diagnostic and what it found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// Owned rather than `&'static str` because a check's name may carry the
    /// Project code it is about.
    pub name: String,
    pub status: Status,
    pub detail: String,
}

impl Check {
    fn ok(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: Status::Ok,
            detail: detail.into(),
        }
    }

    fn warn(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: Status::Warn,
            detail: detail.into(),
        }
    }

    fn fail(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: Status::Fail,
            detail: detail.into(),
        }
    }
}

/// Every check the doctor ran, in report order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnosis {
    pub checks: Vec<Check>,
}

impl Diagnosis {
    /// True when nothing failed. Warnings do not make a machine unhealthy.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        !self.checks.iter().any(|c| c.status == Status::Fail)
    }

    /// Look one check up by name — the accessor the tests assert through.
    /// The report itself prints every check in order and never looks one up.
    #[cfg(test)]
    #[must_use]
    pub fn check(&self, name: &str) -> Option<&Check> {
        self.checks.iter().find(|c| c.name == name)
    }
}

/// The read-only inputs a diagnosis is computed from.
///
/// Bundled into one struct so the pure [`diagnose`] entry point keeps a short
/// signature as checks are added, and so a caller cannot accidentally pass a
/// live environment to one input and a fixture to another.
pub struct Probe<'a> {
    /// Environment lookup — `NAVIGATOR_GCP_PROJECT_ID`, the Drive coordinates,
    /// and `NAVIGATOR_GITHUB_ORG`.
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// The stored `site login` bearer tokens.
    pub credentials: &'a Credentials,
    /// Unix epoch seconds, for token-expiry math.
    pub now: i64,
    /// The site whose login to check. `None` selects the sole stored host.
    pub host: Option<&'a str>,
    /// The Project code to resolve the folder path for.
    /// `None` reports deployment-wide configuration only.
    pub project_code: Option<&'a str>,
}

/// Compute the diagnosis. Pure: reads nothing but the supplied [`Probe`].
#[must_use]
pub fn diagnose(probe: &Probe<'_>) -> Diagnosis {
    let mut checks = Vec::new();

    // Everything downstream is scoped by the deployment, so an unresolved
    // deployment stops the report rather than letting later checks silently
    // describe some other Workspace. Every missing setting is named at once.
    let workspace = match WorkspaceConfig::from_lookup(|k| (probe.env)(k)) {
        Ok(workspace) => {
            checks.push(Check::ok(
                "deployment",
                format!(
                    "{:?} serving {:?} Projects ({} from environment; {} from environment)",
                    workspace.deployment,
                    workspace.customer,
                    model::workspace::NAVIGATOR_GCP_PROJECT_ID,
                    NAVIGATOR_GITHUB_ORG,
                ),
            ));
            workspace
        }
        Err(e) => {
            let missing = WorkspaceConfig::missing_settings(|k| (probe.env)(k));
            let detail = if missing.len() > 1 {
                format!(
                    "{} are unset or blank; set each before operating this deployment",
                    missing.join(", ")
                )
            } else {
                deployment_error_detail(&e)
            };
            checks.push(Check::fail("deployment", detail));
            return Diagnosis { checks };
        }
    };

    if workspace.deployment.is_deprecated_selection() {
        checks.push(Check::warn(
            "projects folder",
            format!(
                "this deployment's Projects folder is selected by its project id, which is \
                 deprecated; set {} (and {} unless the folder is named {:?})",
                model::workspace::NAVIGATOR_DRIVE_PROJECTS_ROOT_FOLDER_ID,
                model::workspace::NAVIGATOR_DRIVE_PROJECTS_ROOT_NAME,
                model::workspace::DEFAULT_PROJECTS_ROOT_NAME,
            ),
        ));
    }

    checks.push(Check::ok(
        "google workspace",
        format!(
            "{:?}, expecting the Projects root to be named {:?}",
            workspace.google_workspace, workspace.expected_projects_root_name
        ),
    ));

    let drive = workspace.drive_setup(|k| (probe.env)(k));
    checks.push(drive_check(&workspace, &drive));
    checks.push(calendar_delegation_check(probe));
    checks.push(site_login_check(probe));

    if let Some(code) = probe.project_code {
        checks.extend(project_checks(&drive, code));
    }

    Diagnosis { checks }
}

/// The optional Drive ingest: disabled, misconfigured with every missing key
/// named, or configured. Credentials are never printed.
fn drive_check(workspace: &WorkspaceConfig, drive: &DriveSetup) -> Check {
    match drive {
        DriveSetup::Disabled => Check::warn(
            "drive ingest",
            format!(
                "optional and disabled for this deployment: none of {} is set; Projects work \
                 without a Drive folder",
                workspace.drive_settings().join(", ")
            ),
        ),
        // Fail closed. Falling back to another deployment's root is how a
        // staging run writes into the production Drive.
        DriveSetup::Incomplete(missing) => Check::fail(
            "drive ingest",
            format!(
                "partially configured; set {} or unset every Drive setting to disable ingest \
                 (another deployment's settings are never borrowed)",
                missing.join(", ")
            ),
        ),
        DriveSetup::Configured(coordinates) => Check::ok(
            "drive ingest",
            format!(
                "shared drive {}, Projects root folder {} ({})",
                coordinates.shared_drive_id,
                coordinates.projects_root_folder_id,
                coordinates.expected_projects_root_name
            ),
        ),
    }
}

/// The Workspace delegation Attorney Calendars and the Project Calendar share.
/// Absent keys are a warning, the same way an unused Drive ingest is. A
/// partial pair is a failure. The live per-attorney grant is `navigator
/// calendar access`; this check only sees the keys the scope rides on.
fn calendar_delegation_check(probe: &Probe<'_>) -> Check {
    let present = |key| (probe.env)(key).is_some_and(|value| !value.trim().is_empty());
    let user = model::workspace::NAVIGATOR_DRIVE_NEON_LAW_DELEGATED_USER;
    let account = model::workspace::NAVIGATOR_DRIVE_NEON_LAW_SERVICE_ACCOUNT;
    let scope = "https://www.googleapis.com/auth/calendar.freebusy and https://www.googleapis.com/auth/calendar.readonly";
    match (present(user), present(account)) {
        (true, true) => Check::ok(
            "calendar delegation",
            format!(
                "Workspace delegation is set. Grant {scope}. navigator calendar access reports each attorney. A missing grant is never free time"
            ),
        ),
        (false, false) => Check::warn(
            "calendar delegation",
            format!(
                "optional until scheduling: {user} and {account} are unset. Grant {scope} after they are set"
            ),
        ),
        (user_set, account_set) => {
            let missing = [
                (!user_set).then_some(user),
                (!account_set).then_some(account),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ");
            Check::fail(
                "calendar delegation",
                format!(
                    "partially configured; set {missing}. Scheduling cannot treat a missing grant as free time"
                ),
            )
        }
    }
}

/// Spell the deployment-resolution failures as operator instructions rather
/// than as the bare `Display` of the error.
fn deployment_error_detail(error: &WorkspaceConfigError) -> String {
    match error {
        WorkspaceConfigError::MissingDeployment => format!(
            "{} is unset — set it to the GCP project of the deployment you are operating",
            model::workspace::NAVIGATOR_GCP_PROJECT_ID
        ),
        WorkspaceConfigError::MissingCoordinate(key) if *key == NAVIGATOR_GITHUB_ORG => {
            format!(
                "{key} is unset; a named deployment must configure the organization its own \
                 automation occupies, and there is no default"
            )
        }
        WorkspaceConfigError::MissingCoordinate(key) if *key == NAVIGATOR_GIT_HOST => {
            format!(
                "{key} is set to a blank value; unset it to take the default host, or name the \
                 host this deployment's organization lives on"
            )
        }
        other @ WorkspaceConfigError::MissingCoordinate(_) => other.to_string(),
    }
}

/// Whether this machine holds a usable bearer token for the site. A missing
/// login is a warning (the doctor still reports every coordinate); an expired
/// one is a failure, because a command that assumed it would work will not.
fn site_login_check(probe: &Probe<'_>) -> Check {
    let base = match probe.host {
        Some(host) => Some(credentials::base_url(host)),
        None => probe.credentials.sole_host().map(str::to_owned),
    };
    let Some(base) = base else {
        return Check::warn(
            "site login",
            "not logged in to any site — run `navigator site login --host …`",
        );
    };
    match probe.credentials.get(&base) {
        None => Check::warn(
            "site login",
            format!("no stored token for {base} — run `navigator site login --host {base}`"),
        ),
        Some(cred) if cred.is_expired(probe.now) => Check::fail(
            "site login",
            format!("the stored token for {base} has expired — run `navigator site login --host {base}`"),
        ),
        Some(cred) => Check::ok(
            "site login",
            format!(
                "{} as {}, {} left",
                base,
                cred.person_email.as_deref().unwrap_or("unknown identity"),
                credentials::humanize_remaining(cred.seconds_remaining(probe.now)),
            ),
        ),
    }
}

/// The per-Project coordinates: where the folder belongs.
fn project_checks(drive: &DriveSetup, code: &str) -> Vec<Check> {
    let mut checks = Vec::new();

    match drive {
        DriveSetup::Configured(drive) => {
            checks.push(Check::ok("project folder", drive.project_path(code)));
        }
        DriveSetup::Disabled => checks.push(Check::ok(
            "project folder",
            format!("Drive ingest is disabled; {code} needs no Drive folder"),
        )),
        DriveSetup::Incomplete(_) => checks.push(Check::fail(
            "project folder",
            format!("cannot place {code}: this deployment's Drive settings are incomplete"),
        )),
    }

    // A Project's source repository is a whole URL stored on the Project, not
    // a coordinate composed from deployment configuration, so there is nothing
    // for a configuration doctor to derive or check here. `navigator project gate`
    // inspects an actual checkout instead.
    checks
}

/// Print one diagnosis and return the process exit code.
fn report(diagnosis: &Diagnosis) -> ExitCode {
    for check in &diagnosis.checks {
        let marker = match check.status {
            Status::Ok => palette::highlight("ok  "),
            Status::Warn => palette::dim("warn"),
            Status::Fail => palette::highlight("FAIL"),
        };
        println!("{marker}  {:<24}  {}", check.name, check.detail);
    }

    if diagnosis.is_healthy() {
        println!();
        println!(
            "{}",
            palette::dim("workspace verified; nothing was modified")
        );
        ExitCode::SUCCESS
    } else {
        println!();
        println!(
            "{}",
            palette::highlight(
                "workspace NOT verified — fix the failures above before provisioning"
            ),
        );
        ExitCode::FAILURE
    }
}

/// `navigator project doctor [--host h] [--project code]`.
///
/// Reads the live environment and the stored credentials. Writes nothing, and makes no network or database call: the
/// diagnosis is now a pure function of its [`Probe`] all the way out to the
/// command, because there is no registry left to read.
#[must_use]
pub fn run(host: Option<&str>, project_code: Option<&str>) -> ExitCode {
    let creds = match credentials::load(&credentials::default_credentials_path()) {
        Ok(creds) => creds,
        Err(e) => {
            eprintln!("could not read the credential store: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    let mut diagnosis = diagnose(&Probe {
        env: &|key| std::env::var(key).ok(),
        credentials: &creds,
        now: now_secs(),
        host,
        project_code,
    });
    let local = crate::projects::manifest::read(std::path::Path::new("."));
    diagnosis
        .checks
        .extend(project_code.and_then(|code| drive_link_check(code, local.as_ref())));
    diagnosis.checks.extend(
        project_code
            .and_then(|code| firm_link_check(code, local.as_ref(), std::path::Path::new("."))),
    );
    report(&diagnosis)
}

/// The Drive folder a checkout of `code` links in its `navigator.yaml`. A
/// matter whose folders exist but are never named is found only by a title
/// search, so an unlinked Project warns. Outside a checkout of `code` there
/// is nothing local to read and no check is reported.
fn drive_link_check(
    code: &str,
    local: Option<&crate::projects::manifest::Manifest>,
) -> Option<Check> {
    let local = local.filter(|manifest| manifest.project.as_deref() == Some(code))?;
    Some(
        match local
            .drive_folder_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            Some(id) => Check::ok(
                "drive folder link",
                format!("https://drive.google.com/drive/folders/{id}"),
            ),
            None => Check::warn(
                "drive folder link",
                format!(
                    "navigator.yaml links no Drive folder; link the matter's folder with \
                     `navigator project set {code} --drive-folder <link>`"
                ),
            ),
        },
    )
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// The Firm a checkout's materialized links point at, read from the
/// sync-written links that reach into a Firm library: the rules link into
/// `.firm/` and each Firm skill link into the `.agents/skills/<slug>/`
/// namespace. Generic skill links (no Firm slug) contribute nothing. Empty
/// when no Firm link is present.
fn linked_firms(checkout: &std::path::Path) -> Vec<String> {
    use crate::projects::workspace_skills::{FIRM_DIRECTORY, PROJECT_LINK};
    let mut slugs = std::collections::BTreeSet::new();
    for dir in [".claude/rules", ".claude/skills"] {
        for entry in std::fs::read_dir(checkout.join(dir))
            .into_iter()
            .flatten()
            .flatten()
        {
            let Ok(target) = std::fs::read_link(entry.path()) else {
                continue;
            };
            let target = target.to_string_lossy();
            if let Some(rest) = target.split(&format!("{FIRM_DIRECTORY}/")).nth(1) {
                if let Some(slug) = rest.split('/').next().filter(|slug| !slug.is_empty()) {
                    slugs.insert(slug.to_string());
                }
            } else if let Some(rest) = target.strip_prefix(PROJECT_LINK) {
                let mut segments = rest.split('/');
                if let (Some(slug), Some(_)) = (segments.next(), segments.next()) {
                    slugs.insert(slug.to_string());
                }
            }
        }
    }
    slugs.into_iter().collect()
}

/// Whether a checkout's materialized Firm links agree with the `firm:` its
/// `navigator.yaml` names. Sync derives both from the manifest, so a
/// disagreement means a stale link set or a hand-edited manifest — and the
/// checkout would load another Firm's rules or skills. Fixing is one command
/// or one manifest edit, and the check only reports when run inside the
/// checkout of `code`.
fn firm_link_check(
    code: &str,
    local: Option<&crate::projects::manifest::Manifest>,
    checkout: &std::path::Path,
) -> Option<Check> {
    let local = local.filter(|manifest| manifest.project.as_deref() == Some(code))?;
    let declared = crate::projects::manifest::non_empty(local.firm.as_deref());
    let linked = linked_firms(checkout);
    match (declared, linked.as_slice()) {
        // Outside a Firm workspace there is nothing to compare.
        (None, []) => None,
        (Some(declared), [linked]) if linked == &declared => Some(Check::ok(
            "firm links",
            format!("skills and rules point at Firm `{declared}`, matching navigator.yaml"),
        )),
        (Some(declared), [linked]) => Some(Check::fail(
            "firm links",
            format!(
                "the checkout's Firm links point at Firm `{linked}` but navigator.yaml names \
                 `{declared}` — the checkout would load the wrong Firm's rules. Re-run \
                 `navigator sync` in the workspace root, or correct the manifest `firm:` if it \
                 is wrong"
            ),
        )),
        (Some(declared), []) => Some(Check::warn(
            "firm links",
            format!(
                "navigator.yaml names Firm `{declared}` but no Firm rules are linked at \
                 `.claude/rules/AGENTS.md`; run `navigator sync` in the workspace root"
            ),
        )),
        (Some(_), linked) => Some(Check::fail(
            "firm links",
            format!(
                "the checkout's Firm links point at more than one Firm ({}) but navigator.yaml \
                 names one — re-run `navigator sync` in the workspace root",
                linked.join(", ")
            ),
        )),
        (None, [linked]) => Some(Check::warn(
            "firm links",
            format!(
                "Firm links point at Firm `{linked}` but navigator.yaml names no `firm`; set \
                 `firm: {linked}` in the manifest, or re-run `navigator sync` in the workspace \
                 root"
            ),
        )),
        (None, linked) => Some(Check::warn(
            "firm links",
            format!(
                "Firm links point at more than one Firm ({}) but navigator.yaml names no `firm`; \
                 re-run `navigator sync` in the workspace root",
                linked.join(", ")
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::HostCredential;
    use std::collections::HashMap;

    const DRIVE_ID: &str = "NAVIGATOR_DRIVE_NEON_LAW_PROJECTS_DRIVE_ID";
    const GCP: &str = "NAVIGATOR_GCP_PROJECT_ID";

    /// The synthetic forge coordinate every fixture configures.
    ///
    /// The organization is *configuration*, so this module spells no real
    /// organization: a fixture that did would be a second vocabulary alongside
    /// the configured one.
    const AN_ORGANIZATION: &str = "an-organization";

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    /// A deployment's full fixture: its identity plus the organization a named
    /// deployment must carry.
    fn deployment(project_id: &'static str) -> Vec<(&'static str, &'static str)> {
        vec![
            (GCP, project_id),
            (DRIVE_ID, "drive-neon"),
            (root_key(project_id), "root-folder"),
            (
                cloud::workspace::NAVIGATOR_DRIVE_NEON_LAW_DELEGATED_USER,
                "ingest@example.test",
            ),
            (
                cloud::workspace::NAVIGATOR_DRIVE_NEON_LAW_SERVICE_ACCOUNT,
                "drive@example.iam.gserviceaccount.com",
            ),
            (NAVIGATOR_GITHUB_ORG, AN_ORGANIZATION),
        ]
    }

    /// A deployment that does not use Drive ingest at all.
    fn without_drive(project_id: &'static str) -> Vec<(&'static str, &'static str)> {
        vec![(GCP, project_id), (NAVIGATOR_GITHUB_ORG, AN_ORGANIZATION)]
    }

    fn logged_in(base: &str, expires_at: i64) -> Credentials {
        let mut creds = Credentials::default();
        creds.set(
            base,
            HostCredential {
                token: "tok".into(),
                person_email: Some("nick@neonlaw.com".into()),
                role: Some("admin".into()),
                expires_at,
            },
        );
        creds
    }

    /// The root-folder env key each deployment reads — deliberately spelled
    /// out here rather than derived, so a change to the mapping has to be
    /// made twice on purpose.
    fn root_key(project_id: &str) -> &'static str {
        match project_id {
            "neon-law" => "NAVIGATOR_DRIVE_NEON_LAW_PRODUCTION_PROJECTS_ROOT_FOLDER_ID",
            "neon-law-stg" => "NAVIGATOR_DRIVE_NEON_LAW_STAGING_PROJECTS_ROOT_FOLDER_ID",
            _ => cloud::workspace::NAVIGATOR_DRIVE_PROJECTS_ROOT_FOLDER_ID,
        }
    }

    /// One repository coordinate per Project, in the configured organization.
    #[test]
    fn every_deployment_resolves_its_own_drive_and_one_repository_coordinate() {
        for (project_id, root_name) in [
            ("example-project", "Projects"),
            ("neon-law", "Projects"),
            ("neon-law-stg", "Staging Projects"),
        ] {
            let creds = logged_in("https://www.neonlaw.com", 10_000);
            let lookup = env(&deployment(project_id));
            let diagnosis = diagnose(&Probe {
                env: &lookup,
                credentials: &creds,
                now: 0,
                host: None,
                project_code: Some("acme"),
            });

            assert!(
                diagnosis.is_healthy(),
                "{project_id} should be healthy: {:?}",
                diagnosis.checks
            );
            let deployment_detail = &diagnosis.check("deployment").unwrap().detail;
            assert!(
                deployment_detail.contains(cloud::workspace::NAVIGATOR_GCP_PROJECT_ID),
                "{deployment_detail}"
            );
            assert!(
                deployment_detail.contains(NAVIGATOR_GITHUB_ORG),
                "{deployment_detail}"
            );
            let drive_detail = &diagnosis.check("drive ingest").unwrap().detail;
            assert!(drive_detail.contains("drive-neon"), "{drive_detail}");
            assert!(
                !drive_detail.contains("ingest@example.test"),
                "credentials stay out of the report: {drive_detail}"
            );
            assert_eq!(
                diagnosis.check("project folder").unwrap().detail,
                format!("{root_name}/acme"),
                "{project_id}"
            );
            assert!(
                diagnosis.check("portal mount").is_none(),
                "the Portal is retired, so there is no mount to report"
            );
            // A Project's repository is a URL stored on the matter, so a
            // configuration doctor has nothing to derive and must not report
            // one. It holds no database connection to read the real value.
            assert!(
                diagnosis.check("project repository").is_none(),
                "{project_id} must report no derived repository coordinate"
            );
        }
    }

    /// A named deployment missing its organization fails closed.
    ///
    /// The organization is this deployment's own, and there is no default: a
    /// value that silently appeared would let a staging run describe production.
    /// Its paired host is read here too, to the different rule the pair's two
    /// halves carry — see
    /// [`a_named_deployment_whose_host_is_blank_fails_and_names_the_key`].
    #[test]
    fn a_named_deployment_without_its_organization_fails_and_names_the_key() {
        let creds = Credentials::default();
        let missing = NAVIGATOR_GITHUB_ORG;
        let mut pairs = deployment("neon-law");
        pairs.retain(|(key, _)| *key != missing);
        let lookup = env(&pairs);
        let diagnosis = diagnose(&Probe {
            env: &lookup,
            credentials: &creds,
            now: 0,
            host: None,
            project_code: Some("acme"),
        });

        assert!(!diagnosis.is_healthy(), "{missing} must fail the report");
        let detail = &diagnosis.check("deployment").unwrap().detail;
        assert!(detail.contains(missing), "{detail}");
        assert!(
            detail.contains("no default"),
            "the report must say there is no fallback: {detail}"
        );
        // And nothing may be reported for a deployment that did not resolve.
        assert!(diagnosis.check("project folder").is_none());
    }

    /// A named deployment whose host is blank fails closed naming that key.
    ///
    /// The host is now half of this deployment's forge coordinate, so the
    /// diagnosis reads it. It carries a default, which is why *absence* stays
    /// healthy — every fixture in this module omits it — and why a
    /// templated-and-never-filled value is the failure worth reporting.
    #[test]
    fn a_named_deployment_whose_host_is_blank_fails_and_names_the_key() {
        let creds = Credentials::default();
        let mut pairs = deployment("neon-law");
        pairs.push((NAVIGATOR_GIT_HOST, "   "));
        let lookup = env(&pairs);
        let diagnosis = diagnose(&Probe {
            env: &lookup,
            credentials: &creds,
            now: 0,
            host: None,
            project_code: Some("acme"),
        });

        assert!(
            !diagnosis.is_healthy(),
            "{NAVIGATOR_GIT_HOST} must fail the report"
        );
        let detail = &diagnosis.check("deployment").unwrap().detail;
        assert!(detail.contains(NAVIGATOR_GIT_HOST), "{detail}");
        assert!(
            detail.contains("blank"),
            "the report must say what is wrong with the value: {detail}"
        );
        assert!(diagnosis.check("project folder").is_none());
    }

    #[test]
    fn a_missing_deployment_fails_and_stops_the_report() {
        let creds = Credentials::default();
        let diagnosis = diagnose(&Probe {
            env: &env(&[]),
            credentials: &creds,
            now: 0,
            host: None,
            project_code: Some("acme"),
        });

        assert!(!diagnosis.is_healthy());
        assert_eq!(diagnosis.checks.len(), 1, "{:?}", diagnosis.checks);
        assert_eq!(diagnosis.checks[0].name, "deployment");
        // One run names every missing deployment setting.
        for key in [GCP, NAVIGATOR_GITHUB_ORG] {
            assert!(
                diagnosis.checks[0].detail.contains(key),
                "{}",
                diagnosis.checks[0].detail
            );
        }
        // No Drive or repository coordinate may be reported for an
        // unresolved deployment.
        assert!(diagnosis.check("project repository").is_none());
        assert!(diagnosis.check("project folder").is_none());
    }

    #[test]
    fn selection_by_project_id_is_reported_as_deprecated() {
        let creds = Credentials::default();
        let warned = |project_id: &'static str| {
            diagnose(&Probe {
                env: &env(&without_drive(project_id)),
                credentials: &creds,
                now: 0,
                host: None,
                project_code: None,
            })
            .check("projects folder")
            .map(|check| (check.status, check.detail.clone()))
        };
        let (status, detail) = warned("neon-law-stg").expect("the deprecated path is reported");
        assert_eq!(status, Status::Warn);
        assert!(
            detail.contains(cloud::workspace::NAVIGATOR_DRIVE_PROJECTS_ROOT_FOLDER_ID),
            "{detail}"
        );
        assert!(warned("example-project").is_none());
    }

    #[test]
    fn staging_does_not_borrow_productions_root_folder() {
        let creds = Credentials::default();
        // Production's root is present; staging's is not. Staging must fail
        // rather than silently write into the production Drive.
        let lookup = env(&[
            (GCP, "neon-law-stg"),
            (DRIVE_ID, "drive-neon"),
            (
                "NAVIGATOR_DRIVE_NEON_LAW_PRODUCTION_PROJECTS_ROOT_FOLDER_ID",
                "root-production",
            ),
            (NAVIGATOR_GITHUB_ORG, AN_ORGANIZATION),
        ]);
        let diagnosis = diagnose(&Probe {
            env: &lookup,
            credentials: &creds,
            now: 0,
            host: None,
            project_code: Some("acme"),
        });

        assert!(!diagnosis.is_healthy());
        let drive = diagnosis.check("drive ingest").unwrap();
        assert_eq!(drive.status, Status::Fail);
        // Every missing key, in one run.
        for key in [
            "NAVIGATOR_DRIVE_NEON_LAW_STAGING_PROJECTS_ROOT_FOLDER_ID",
            cloud::workspace::NAVIGATOR_DRIVE_NEON_LAW_DELEGATED_USER,
            cloud::workspace::NAVIGATOR_DRIVE_NEON_LAW_SERVICE_ACCOUNT,
        ] {
            assert!(drive.detail.contains(key), "{}", drive.detail);
        }
        // And the Project folder cannot be placed without them.
        assert_eq!(
            diagnosis.check("project folder").unwrap().status,
            Status::Fail
        );
    }

    #[test]
    fn calendar_delegation_follows_the_workspace_keys() {
        let creds = Credentials::default();
        let bare = diagnose(&Probe {
            env: &env(&without_drive("neon-law")),
            credentials: &creds,
            now: 0,
            host: None,
            project_code: None,
        });
        assert!(bare.is_healthy(), "{:?}", bare.checks);
        let calendar = bare.check("calendar delegation").unwrap();
        assert_eq!(calendar.status, Status::Warn);
        assert!(calendar
            .detail
            .contains("https://www.googleapis.com/auth/calendar.freebusy"));

        let ready = diagnose(&Probe {
            env: &env(&deployment("neon-law")),
            credentials: &creds,
            now: 0,
            host: None,
            project_code: None,
        });
        assert_eq!(
            ready.check("calendar delegation").unwrap().status,
            Status::Ok
        );

        let mut partial = deployment("neon-law");
        partial
            .retain(|(key, _)| *key != cloud::workspace::NAVIGATOR_DRIVE_NEON_LAW_SERVICE_ACCOUNT);
        let diagnosis = diagnose(&Probe {
            env: &env(&partial),
            credentials: &creds,
            now: 0,
            host: None,
            project_code: None,
        });
        assert!(!diagnosis.is_healthy());
        let calendar = diagnosis.check("calendar delegation").unwrap();
        assert_eq!(calendar.status, Status::Fail);
        assert!(calendar
            .detail
            .contains(cloud::workspace::NAVIGATOR_DRIVE_NEON_LAW_SERVICE_ACCOUNT));
    }

    #[test]
    fn a_deployment_without_drive_is_healthy_and_needs_no_folder() {
        let creds = Credentials::default();
        for project_id in ["example-project", "neon-law-stg"] {
            let lookup = env(&without_drive(project_id));
            let diagnosis = diagnose(&Probe {
                env: &lookup,
                credentials: &creds,
                now: 0,
                host: None,
                project_code: Some("acme"),
            });
            assert!(diagnosis.is_healthy(), "{:?}", diagnosis.checks);
            assert_eq!(
                diagnosis.check("drive ingest").unwrap().status,
                Status::Warn
            );
            let folder = diagnosis.check("project folder").unwrap();
            assert_eq!(folder.status, Status::Ok);
            assert!(folder.detail.contains("disabled"), "{}", folder.detail);
            // Not logged in is likewise only a warning, and no mount is read.
            assert_eq!(diagnosis.check("site login").unwrap().status, Status::Warn);
            assert!(diagnosis.check("drive mount").is_none());
        }
    }

    #[test]
    fn an_expired_token_fails_while_a_live_one_reports_its_identity() {
        let lookup = env(&deployment("neon-law"));

        let expired = logged_in("https://www.neonlaw.com", 100);
        let diagnosis = diagnose(&Probe {
            env: &lookup,
            credentials: &expired,
            now: 1_000,
            host: None,
            project_code: None,
        });
        assert!(!diagnosis.is_healthy());
        assert_eq!(diagnosis.check("site login").unwrap().status, Status::Fail);

        let live = logged_in("https://www.neonlaw.com", 10_000);
        let diagnosis = diagnose(&Probe {
            env: &lookup,
            credentials: &live,
            now: 1_000,
            host: None,
            project_code: None,
        });
        assert!(diagnosis.is_healthy(), "{:?}", diagnosis.checks);
        let login = diagnosis.check("site login").unwrap();
        assert!(
            login.detail.contains("nick@neonlaw.com"),
            "{}",
            login.detail
        );
    }

    #[test]
    fn an_explicit_host_selects_that_login_rather_than_the_sole_one() {
        let lookup = env(&deployment("neon-law"));
        let creds = logged_in("https://www.neonlaw.com", 10_000);

        // Asking about a host with no stored token warns, even though
        // exactly one other host is logged in.
        let diagnosis = diagnose(&Probe {
            env: &lookup,
            credentials: &creds,
            now: 0,
            host: Some("staging.example"),
            project_code: None,
        });
        let login = diagnosis.check("site login").unwrap();
        assert_eq!(login.status, Status::Warn);
        assert!(
            login.detail.contains("https://staging.example"),
            "{}",
            login.detail
        );
    }

    #[test]
    fn without_a_project_code_no_per_project_check_is_reported() {
        let creds = Credentials::default();
        let lookup = env(&deployment("neon-law"));
        let diagnosis = diagnose(&Probe {
            env: &lookup,
            credentials: &creds,
            now: 0,
            host: None,
            project_code: None,
        });
        assert!(diagnosis.check("project folder").is_none());
        assert!(diagnosis.check("project repository").is_none());
        assert!(diagnosis.check("portal mount").is_none());
    }

    #[test]
    fn a_checkout_without_a_drive_folder_link_warns() {
        let manifest = |yaml: &str| {
            let root = tempfile::TempDir::new().unwrap();
            std::fs::write(root.path().join("navigator.yaml"), yaml).unwrap();
            crate::projects::manifest::read(root.path()).unwrap()
        };
        let unlinked =
            manifest("version: 26.11.1\nproject:\n  host: https://example.test\n  name: acme\n");
        let check = drive_link_check("acme", Some(&unlinked)).unwrap();
        assert_eq!(check.status, Status::Warn);
        assert!(check
            .detail
            .contains("navigator project set acme --drive-folder <link>"));
        let linked = manifest(
            "version: 26.11.1\nproject:\n  host: https://example.test\n  name: acme\n  drive_folder_id: synthetic-folder\n",
        );
        let check = drive_link_check("acme", Some(&linked)).unwrap();
        assert_eq!(check.status, Status::Ok);
        assert_eq!(
            check.detail,
            "https://drive.google.com/drive/folders/synthetic-folder"
        );
        assert!(drive_link_check("other", Some(&linked)).is_none());
        assert!(drive_link_check("acme", None).is_none());
    }

    /// The manifest a `firm_link_check` reads, written to a tempdir the way a
    /// checkout of `acme` carries one.
    fn checkout_manifest(yaml: &str) -> crate::projects::manifest::Manifest {
        let root = tempfile::TempDir::new().unwrap();
        std::fs::write(root.path().join("navigator.yaml"), yaml).unwrap();
        crate::projects::manifest::read(root.path()).unwrap()
    }

    fn linked_checkout(
        root: &tempfile::TempDir,
        rules_target: Option<&str>,
        skill_targets: &[&str],
    ) {
        let rules = root.path().join(".claude/rules");
        let skills = root.path().join(".claude/skills");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::create_dir_all(&skills).unwrap();
        if let Some(target) = rules_target {
            symlink(target, rules.join("AGENTS.md")).unwrap();
        }
        for (index, target) in skill_targets.iter().enumerate() {
            symlink(target, skills.join(format!("skill-{index}"))).unwrap();
        }
    }

    use std::os::unix::fs::symlink;

    #[test]
    fn a_checkout_whose_firm_links_disagree_with_its_manifest_fails_with_the_fix() {
        let root = tempfile::TempDir::new().unwrap();
        linked_checkout(
            &root,
            Some("../../../.firm/other-firm/AGENTS.md"),
            &["../../../.agents/skills/other-firm/intake"],
        );
        let manifest =
            checkout_manifest("version: 26.11.1\nproject:\n  host: https://example.test\n  name: acme\n  firm: firm-a\n");

        let check = firm_link_check("acme", Some(&manifest), root.path()).unwrap();
        assert_eq!(check.status, Status::Fail);
        assert!(check.detail.contains("`other-firm`"), "{}", check.detail);
        assert!(check.detail.contains("`firm-a`"), "{}", check.detail);
        assert!(check.detail.contains("navigator sync"), "{}", check.detail);
        assert!(check.detail.contains("`firm:`"), "{}", check.detail);
        // A different Project code reports nothing.
        assert!(firm_link_check("other", Some(&manifest), root.path()).is_none());
    }

    #[test]
    fn matching_firm_links_are_ok_and_missing_ones_warn_toward_sync() {
        let linked = tempfile::TempDir::new().unwrap();
        linked_checkout(&linked, Some("../../../.firm/firm-a/AGENTS.md"), &[]);
        let manifest =
            checkout_manifest("version: 26.11.1\nproject:\n  host: https://example.test\n  name: acme\n  firm: firm-a\n");
        let check = firm_link_check("acme", Some(&manifest), linked.path()).unwrap();
        assert_eq!(check.status, Status::Ok);

        let bare = tempfile::TempDir::new().unwrap();
        let check = firm_link_check("acme", Some(&manifest), bare.path()).unwrap();
        assert_eq!(check.status, Status::Warn);
        assert!(check.detail.contains(".claude/rules/AGENTS.md"));
        assert!(check.detail.contains("navigator sync"));
    }

    #[test]
    fn links_pointing_at_two_firms_fail_even_when_the_manifest_names_one_of_them() {
        let root = tempfile::TempDir::new().unwrap();
        linked_checkout(
            &root,
            Some("../../../.firm/firm-a/AGENTS.md"),
            &["../../../.agents/skills/firm-b/intake"],
        );
        let manifest =
            checkout_manifest("version: 26.11.1\nproject:\n  host: https://example.test\n  name: acme\n  firm: firm-a\n");
        let check = firm_link_check("acme", Some(&manifest), root.path()).unwrap();
        assert_eq!(check.status, Status::Fail);
        assert!(check.detail.contains("more than one Firm"));
    }

    #[test]
    fn a_checkout_with_no_firm_anywhere_reports_nothing() {
        let root = tempfile::TempDir::new().unwrap();
        let manifest = checkout_manifest(
            "version: 26.11.1\nproject:\n  host: https://example.test\n  name: acme\n",
        );
        assert!(firm_link_check("acme", Some(&manifest), root.path()).is_none());
    }
}
