//! The `navigator` command tree: Projects, their Jujutsu repositories, the
//! offline `validate` gate, and the deployment session `project` reads.
//!
//! The tree, its dispatch, and every helper it calls live in this library so
//! a host other than the `navigator` binary (the menu-bar app) can link the
//! same rules. `navigator-deploy` and `scorpio` reuse the shared helpers.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use serde::Deserialize;

use crate::projects::pinned_gate::GateMode;
use crate::projects::repository::is_project_repository;
use crate::{
    closing_trailer, document_layout, document_sync, legal_plans, login, palette, projects, remote,
};

/// The version `build.rs` baked into this build, with no runtime override:
/// the version whose bytes (skills, templates) this binary carries.
pub const CLI_BUILD_VERSION: &str = env!("NAVIGATOR_CLI_VERSION");

#[derive(Parser)]
#[command(
    name = "navigator",
    version = cli_version(),
    about = "Navigator CLI, not legal advice.",
    long_about = "Navigator CLI, not legal advice."
)]
pub struct Cli {
    /// Print the licence this binary is distributed under, then exit. Stands
    /// alone, like `--version`.
    #[arg(long, exclusive = true)]
    license: bool,
    /// Print the licence notices for the third-party open-source components
    /// this binary incorporates, then exit. Stands alone.
    #[arg(long, exclusive = true)]
    third_party_notices: bool,
    /// Optional only so `--license` can stand alone; every other invocation
    /// still requires one, and a bare `navigator` prints help and exits 2.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)] // parsed once per process
enum Command {
    /// Validate Markdown and YAML files in `<dir>` (default `.`). In a Project
    /// checkout with pointers, a current login for its manifest host also
    /// checks live document revisions using read-only, bounded requests.
    ///
    /// Walks an arbitrary directory with no assumption about the surrounding
    /// repository. A tree that is neither a Navigator checkout nor a Project
    /// repository still has this command. `project gate` remains the check
    /// over a recognised repository root.
    Validate {
        /// Directory to walk.
        #[arg(default_value = ".")]
        dir: PathBuf,
        /// Apply every safe-by-construction rule autofix
        /// (whitespace, ATX heading spacing, blockquote spacing, S102
        /// paragraph packing) to the files in place, re-scanning each
        /// file until it stops changing, then re-validate.
        /// Diagnostic-only rules (N-family notation-template except N108,
        /// M024 duplicate headings, M026 trailing punctuation) are still
        /// reported but not auto-fixed. N108 safely removes retired
        /// frontmatter `code:` metadata. The autofixed-source view is
        /// what the `navigator-lsp` `source.fixAll` action ships in
        /// editors.
        #[arg(long)]
        fix: bool,
        /// Print only the findings that fail the gate, hiding the
        /// Warning-severity advisories. The summary line still counts
        /// both and the exit code is unchanged: this narrows the
        /// listing for a CI-triage read, not the gate itself. Rejected
        /// with `--fix`, where a remaining warning still has to be
        /// resolved before the run passes and so must stay on screen.
        #[arg(long, conflicts_with = "fix")]
        errors_only: bool,
        /// Accepted for compatibility and ignored. It once held a built-bundle
        /// scan to a tree CI had built; Project repositories commit only
        /// source now, so there is nothing built to hold it to.
        #[arg(long, hide = true)]
        ci: bool,
    },
    /// Preview the Jujutsu checkouts needed for active Projects on a host.
    Sync {
        /// Target Navigator host.
        #[arg(long)]
        host: String,
        /// Directory containing Project checkouts. Defaults to ~/Work.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Report checkout state without changing files or contacting the forge.
        #[arg(long, required = true)]
        dry_run: bool,
    },
    /// Interact with a project hosted on a navigator site.
    ///
    /// Singular because a checkout is one Project — the repository name *is* the Project code.
    #[command(name = "project")]
    Projects {
        #[command(subcommand)]
        action: ProjectsCmd,
    },
    /// The deployment session `project` commands use, and the `seeds/`
    /// import a Project repository's CI runs.
    Site {
        #[command(subcommand)]
        action: SiteCmd,
    },
}

/// The site verbs a Project checkout and its CI need: the stored session and
/// the `seeds/` import. `navigator-deploy site` carries the rest of the site
/// client.
#[derive(Subcommand)]
pub enum SiteCmd {
    /// Import a seed-shaped YAML document through the logged-in deployment.
    ///
    /// With neither `MODEL_NAME` nor `SEED_FILE`, imports every supported
    /// seed document in `seeds/` at the repository root — the convention
    /// every Project repository already follows. With no `seeds/` directory,
    /// prints a notice and succeeds.
    Import {
        /// Singular glossary term, such as `person`, `entity`,
        /// `person_project_role`, `person_entity_role`, `address`, or
        /// `relationship_log`.
        /// Requires `SEED_FILE`; omit both to import `seeds/` at the
        /// repository root.
        #[arg(requires = "seed_file")]
        model_name: Option<String>,
        /// YAML document using the standard `lookup_fields` / `records`
        /// shape. Requires `MODEL_NAME`; omit both to import `seeds/` at the
        /// repository root.
        #[arg(requires = "model_name")]
        seed_file: Option<PathBuf>,
        /// Replace every field represented in each matching seed record,
        /// including a Person record's `role`.
        #[arg(long)]
        overwrite: bool,
        /// Show the per-record reconciliation plan without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Exchange a GitHub Actions OIDC token for a project-scoped seed session
        ///
        /// Requires `--host`. Does not read or write `~/.navigator.json`.
        #[arg(long, requires = "host")]
        ci: bool,
        /// Plan under the scoped-session rules CI carries, for the Project
        /// this checkout's `navigator.yaml` names, so the plan says whether
        /// CI could perform the same write. Requires `--dry-run`; a scope
        /// violation fails here instead of on `main`.
        #[arg(long, requires = "dry_run", conflicts_with = "ci")]
        as_ci: bool,
        #[command(flatten)]
        host: HostOpt,
    },

    /// Sign in to a deployment and store its bearer token.
    Login {
        /// Host to authenticate to, e.g. `www.neonlaw.com`. A bare host
        /// gets `https://`; pass a full URL (e.g.
        /// `http://localhost:8080`) to target a local cluster.
        #[arg(long)]
        host: String,
        /// Print the login URL without opening it. Useful for headless
        /// sessions and automated loopback-flow tests.
        #[arg(long)]
        no_browser: bool,
    },
    /// Forget a stored session.
    Logout {
        /// Host to log out of. Optional when exactly one host is stored.
        #[arg(long)]
        host: Option<String>,
    },
    /// Show who a stored session signs in as.
    Whoami {
        /// Host to inspect. Optional when exactly one host is stored.
        #[arg(long)]
        host: Option<String>,
    },
}

/// The live clap tree `navigator` dispatches. Project-repository validation
/// walks documented `navigator …` invocations against it.
#[must_use]
pub fn navigator_command() -> clap::Command {
    Cli::command()
}

/// Run `navigator` on a thread with [`CLI_STACK_BYTES`] of stack.
#[must_use]
pub fn main() -> ExitCode {
    std::thread::Builder::new()
        .name("navigator".into())
        .stack_size(CLI_STACK_BYTES)
        .spawn(run)
        .expect("spawn the CLI thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

/// Print the licence texts `--license` names.
pub fn print_license() {
    // `write_all` rather than `print!`: a reader routinely pipes these into a
    // pager and quits early, and `print!` panics on the broken pipe.
    let mut out = std::io::stdout();
    let _ = out.write_all(NOTICE.as_bytes());
    let _ = out.write_all(b"\n");
    let _ = out.write_all(LICENSE.as_bytes());
    let _ = out.write_all(b"\n\n===== MIT =====\n\n");
    let _ = out.write_all(LICENSE_MIT.as_bytes());
    let _ = out.write_all(b"\n\n===== Apache-2.0 =====\n\n");
    let _ = out.write_all(LICENSE_APACHE_2.as_bytes());
}

/// Print the third-party notices `--third-party-notices` names.
pub fn print_third_party_notices() {
    let _ = std::io::stdout().write_all(THIRD_PARTY_NOTICES.as_bytes());
}

fn run() -> ExitCode {
    let _ = dotenvy::dotenv();
    let runtime = || tokio::runtime::Runtime::new().expect("tokio runtime");
    let cli = Cli::from_arg_matches(&concise_help(Cli::command()).get_matches())
        .expect("Clap matches come from Cli's command tree");
    if cli.license {
        print_license();
        return ExitCode::SUCCESS;
    }
    if cli.third_party_notices {
        print_third_party_notices();
        return ExitCode::SUCCESS;
    }
    let Some(command) = cli.command else {
        // A bare `navigator` is a usage error, so the help goes to stderr and
        // the exit code matches what clap returns for a missing subcommand.
        eprint!("{}", concise_help(Cli::command()).render_help());
        return ExitCode::from(2);
    };
    match command {
        Command::Validate {
            dir,
            fix,
            errors_only,
            ci: _,
        } => run_validate(&dir, fix, errors_only),
        Command::Sync {
            host,
            dir,
            dry_run: _,
        } => runtime().block_on(projects::watch::run_preview(dir, &host)),
        Command::Projects { action } => runtime().block_on(run_projects(action)),
        Command::Site { action } => match action {
            SiteCmd::Login { host, no_browser } => {
                runtime().block_on(login::run_login(&host, no_browser))
            }
            SiteCmd::Logout { host } => login::run_logout(host.as_deref()),
            SiteCmd::Whoami { host } => login::run_whoami(host.as_deref()),
            SiteCmd::Import {
                model_name,
                seed_file,
                overwrite,
                dry_run,
                ci,
                as_ci,
                host,
            } => {
                let credential = if ci {
                    remote::SeedCredential::Ci {
                        host: host.host.expect("clap requires --host with --ci"),
                    }
                } else {
                    remote::SeedCredential::Stored { host: host.host }
                };
                let as_ci_project = if as_ci {
                    match remote::as_ci_project_code() {
                        Ok(code) => Some(code),
                        Err(error) => {
                            eprintln!("navigator: {error:#}");
                            return ExitCode::from(2);
                        }
                    }
                } else {
                    None
                };
                let options = remote::SeedOptions {
                    overwrite,
                    dry_run,
                    as_ci_project: as_ci_project.as_deref(),
                };
                match (model_name, seed_file) {
                    (Some(model_name), Some(seed_file)) => runtime().block_on(remote::seed(
                        credential,
                        &model_name,
                        &seed_file,
                        options,
                    )),
                    (None, None) => runtime().block_on(remote::seed_directory(
                        credential,
                        Path::new("seeds"),
                        options,
                    )),
                    (Some(_), None) | (None, Some(_)) => {
                        unreachable!("clap requires MODEL_NAME and SEED_FILE together")
                    }
                }
            }
        },
    }
}

/// The version `navigator --version` / `-V` reports.
///
/// Precedence, highest first:
/// 1. A runtime `NAVIGATOR_RELEASE_TAG` — the workspace-wide convention `web`
///    and `lsp` already follow, and the seam tests assert against.
/// 2. The tag baked at build time by `build.rs` (`NAVIGATOR_CLI_VERSION`), so a
///    *downloaded* release binary self-reports its `YY.M.D` release with no
///    environment set.
/// 3. The workspace crate version on a plain local build — `0.1.0` between
///    releases, or the `YY.M.D` a release stamped into `Cargo.toml` — since
///    `build.rs` falls back to `CARGO_PKG_VERSION` when no tag is present.
#[must_use]
pub fn cli_version() -> &'static str {
    if let Ok(tag) = std::env::var("NAVIGATOR_RELEASE_TAG") {
        let tag = tag.trim();
        if !tag.is_empty() {
            // Leak the single resolved version string: it lives for the whole
            // process, and clap's `version` wants a `&'static str`.
            return Box::leak(tag.to_owned().into_boxed_str());
        }
    }
    env!("NAVIGATOR_CLI_VERSION")
}

/// The version `ops github setup`'s `--action-version` pins a reconciled
/// Project repository's generated gate to by default — [`cli_version`]
/// narrowed to the sources that name an *actually published* release, never
/// the bare `CARGO_PKG_VERSION` fallback `build.rs` uses so `--version` still
/// prints something on a plain local build.
///
/// A runtime `NAVIGATOR_RELEASE_TAG` only appears in a deployed container,
/// started from an image published under that tag, so it cannot precede the
/// tag. The build-time-baked `NAVIGATOR_CLI_VERSION` is trustworthy the same
/// way, but only when `NAVIGATOR_CLI_VERSION_IS_RELEASE` confirms `build.rs`
/// actually saw `NAVIGATOR_RELEASE_TAG` rather than falling back to the crate
/// version — which is bumped on `main` before the tag naming it exists. When
/// neither source is available this returns empty, and `is_release_tag`'s own
/// refusal then asks the operator to name `--action-version` themselves
/// rather than have the gate guess.
#[must_use]
pub fn published_cli_version() -> &'static str {
    if let Ok(tag) = std::env::var("NAVIGATOR_RELEASE_TAG") {
        let tag = tag.trim();
        if !tag.is_empty() {
            return Box::leak(tag.to_owned().into_boxed_str());
        }
    }
    option_env!("NAVIGATOR_CLI_VERSION_IS_RELEASE")
        .map(|_| env!("NAVIGATOR_CLI_VERSION"))
        .unwrap_or_default()
}

/// The dual-license choice and the MIT and unmodified Apache-2.0 texts, compiled
/// into distributed executables. `--license` prints NOTICE and these instruments
/// so recipients can read the terms even when no source archive accompanies their binary.
pub const LICENSE: &str = include_str!("../../LICENSE");

pub const LICENSE_MIT: &str = include_str!("../../LICENSE-MIT");

pub const LICENSE_APACHE_2: &str = include_str!("../../LICENSE-APACHE-2");

/// First-party attribution, separate component rights, and trademark notices.
/// NOTICE is informational and does not add conditions to Apache-2.0.
pub const NOTICE: &str = include_str!("../../NOTICE");

/// The third-party licence notices, compiled into the binary for the same
/// reason as [`LICENSE`]: a single downloaded executable has to be able to
/// show the attributions it is obliged to carry. Regenerate with
/// `navigator-deploy ops notices`.
pub const THIRD_PARTY_NOTICES: &str = include_str!("../../THIRD-PARTY-NOTICES.txt");

/// Return the first sentence of `text`, capped at ten words.
///
/// Clap derives help from the detailed Rust documentation beside each command.
/// The documentation remains the source of operational detail, while terminal
/// help is deliberately just a scan-friendly headline.
#[must_use]
pub fn help_headline(text: &str) -> String {
    let sentence_end = text
        .char_indices()
        .find_map(|(index, character)| {
            (matches!(character, '.' | '!' | '?' | ':' | ';' | '—')
                && text[index + character.len_utf8()..]
                    .chars()
                    .next()
                    .is_none_or(char::is_whitespace))
            .then_some(index)
        })
        .unwrap_or(text.len());
    let words = text[..sentence_end]
        .split_whitespace()
        .take(10)
        .collect::<Vec<_>>();
    if words.is_empty() {
        return String::new();
    }
    format!("{}.", words.join(" ").trim_end_matches(['.', '!', '?']))
}

/// Replace every Clap description with its terse terminal headline.
///
/// The command tree carries all authored help, including nested subcommands
/// and arguments, so centralizing this rule keeps every path consistent.
pub fn concise_help(mut command: clap::Command) -> clap::Command {
    let about = command.get_about().map(ToString::to_string);
    let long_about = command.get_long_about().map(ToString::to_string);
    if let Some(about) = about {
        command = command.about(help_headline(&about));
    }
    if let Some(long_about) = long_about {
        command = command.long_about(help_headline(&long_about));
    }

    let arguments = command
        .get_arguments()
        .map(|argument| {
            (
                argument.get_id().as_str().to_owned(),
                argument.get_help().map(ToString::to_string),
                argument.get_long_help().map(ToString::to_string),
            )
        })
        .collect::<Vec<_>>();
    for (id, help, long_help) in arguments {
        command = command.mut_arg(id, |argument| {
            let argument = match help {
                Some(text) => argument.help(help_headline(&text)),
                None => argument,
            };
            match long_help {
                Some(text) => argument.long_help(help_headline(&text)),
                None => argument,
            }
        });
    }

    for subcommand in command.get_subcommands_mut() {
        let original = std::mem::replace(subcommand, clap::Command::new("placeholder"));
        *subcommand = concise_help(original);
    }
    let mut names = command
        .get_subcommands()
        .map(|subcommand| subcommand.get_name().to_owned())
        .collect::<Vec<_>>();
    names.sort_by_key(|name| (name == "help", name.clone()));
    command = command.mut_subcommands(|subcommand| {
        let order = names
            .iter()
            .position(|name| name == subcommand.get_name())
            .unwrap_or(names.len());
        subcommand.display_order(order)
    });
    command
}

#[derive(Subcommand)]
pub enum FinanceCmd {
    /// Queue a run over `finance/<code>/input.json` already in storage; prints the run id.
    Run {
        /// Project code. Defaults to `project.name` in navigator.yaml.
        project_code: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Read the stored report from the latest run.
    Show {
        /// Project code. Defaults to `project.name` in navigator.yaml.
        project_code: Option<String>,
        /// Emit the stored report as JSON.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        host: HostOpt,
    },
}

#[derive(Subcommand)]
pub enum ProjectsCmd {
    /// Check a draft issue for client or matter identifiers before filing it
    /// in an outside tracker. Runs locally; the text is never sent anywhere.
    /// Exits non-zero and names each placeholder to use when it finds any.
    FeedbackCheck {
        /// Draft title and body; reads stdin when omitted.
        #[arg(long)]
        file: Option<PathBuf>,
        /// A private name or Project code to refuse; repeat for each.
        #[arg(long = "term")]
        terms: Vec<String>,
        /// A Project whose code and participants are resolved into private
        /// terms through the site API with the `site login` session; repeat
        /// for each matter in the draft.
        #[arg(long = "project")]
        projects: Vec<String>,
        /// Resolve every Project visible through firm-side participation, by
        /// stable id, including attached Person and Entity identifiers.
        #[arg(long, conflicts_with = "projects")]
        all_projects: bool,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Record e-filing transaction status and file court-stamped copies.
    Efiling {
        #[command(subcommand)]
        action: EfilingCmd,
    },
    /// List participants or change their DRI accountability markers.
    Participant {
        #[command(subcommand)]
        action: ParticipantCmd,
    },
    /// Reference a counterparty Person or Entity from this Project without
    /// granting them any standing on it.
    Reference {
        #[command(subcommand)]
        action: ReferenceCmd,
    },
    /// Record that this Project's client is adverse to a referenced party.
    Adverse {
        #[command(subcommand)]
        action: AdverseCmd,
    },
    /// List, create, update, complete, or cancel a Project deadline.
    Deadline {
        #[command(subcommand)]
        action: DeadlineCmd,
    },
    /// File the documents a court's CM/ECF notices point at into this Project.
    Docket {
        #[command(subcommand)]
        action: DocketCmd,
    },
    /// Record and read service of a filed document.
    Service {
        #[command(subcommand)]
        action: ServiceCmd,
    },
    /// Log a call as an internal-note Communication, or read the log.
    Communication {
        #[command(subcommand)]
        action: CommunicationCmd,
    },
    /// Read or set the Legal Plan this Project is paid through.
    LegalPlan {
        #[command(subcommand)]
        action: LegalPlanCmd,
    },
    /// Read the live Project lifecycle and onboarding evidence.
    Status {
        /// Project code. Defaults to project.name in navigator.yaml.
        project_code: Option<String>,
        /// Emit the complete live status as JSON.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Read the Project's onboarding status: pitch or official.
    Lifecycle {
        /// Project code. Defaults to project.name in navigator.yaml.
        project_code: Option<String>,
        /// Emit the onboarding status as JSON.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Start or read the finance aggregate run.
    Finance {
        #[command(subcommand)]
        action: FinanceCmd,
    },
    /// Open the onboarding letter for this Project, deriving its code and client from navigator.yaml.
    Onboard {
        #[command(flatten)]
        host: HostOpt,
        /// How the client receives the signing link after lawyer approval and send.
        #[arg(long, value_parser = ["embedded", "emailed"])]
        delivery: Option<String>,
        /// A saved answers file (as `site notation answers --save-answers`
        /// writes) to prefill the onboarding notation from: every question
        /// the template asks is answered from the file's `state: value` map,
        /// so the onboarding's interpolated fields match the notation the
        /// file was saved from. A question the file does not answer fails
        /// the onboard — prefill is whole, never partial.
        #[arg(long = "answers-from", alias = "answers-file")]
        answers_from: Option<PathBuf>,
    },
    /// Open the offboarding letter, then close and archive a signed-off Project.
    Offboard {
        /// Close the Project as of this RFC 3339 timestamp.
        #[arg(long)]
        effective_at: Option<chrono::DateTime<chrono::Utc>>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Move a legacy engagement letter (`kind: agreement` with a `derived_from`
    /// lineage) onto the canonical onboarding path, so `navigator project
    /// onboard` opens the notation it was drafted for.
    MigrateLetter {},
    /// Fill Project coordinates from the live matter or explicit values, and set
    /// the live Project's calendar and client booking link.
    Set {
        /// Project code. Defaults to `project.name` in the current manifest.
        project_code: Option<String>,
        /// Fill coordinates from the authorized live Project row.
        #[arg(long)]
        from_live: bool,
        /// Lawyer-side DRI email.
        #[arg(long)]
        lawyer_dri: Option<String>,
        /// Client-side DRI email.
        #[arg(long)]
        client_dri: Option<String>,
        /// Private Slack channel URL.
        #[arg(long)]
        private_slack_channel: Option<String>,
        /// Private Notion page URL.
        #[arg(long)]
        private_notion_page: Option<String>,
        /// The Project's shared Google Calendar id. Written to
        /// `project.calendar_id` and the live Project; an empty value clears
        /// both. `navigator project setup` creates the calendar.
        #[arg(long = "calendar", value_name = "ID")]
        calendar: Option<String>,
        /// An existing Drive folder for the Project, in the Firm's Projects
        /// shared drive: its drive.google.com link or its id. Written to
        /// `project.drive_folder_id` and the live Project; an empty value
        /// clears both. The deployment confirms the folder with Drive when it
        /// has a Drive credential, and says when it could only check the
        /// id's shape. `navigator project setup` then adopts this folder
        /// instead of creating one.
        #[arg(long = "drive-folder", value_name = "URL_OR_ID")]
        drive_folder: Option<String>,
        /// Preview every change without writing navigator.yaml or the live
        /// Project row.
        #[arg(long)]
        dry_run: bool,
        /// The client's booking page: the address of an appointment schedule
        /// made on the Project calendar in Google Calendar. Written to the
        /// live Project only and shown to the client; an empty value clears it.
        #[arg(long, value_name = "URL")]
        booking_url: Option<String>,
        /// The jurisdiction an onboarding Notation's Clauses bind to: the state
        /// whose law and arbitration seat an engagement letter names. Written
        /// to the live Project row and read live at render time by a Clause
        /// input bound to `project.onboarding_jurisdiction`; an empty value
        /// clears it.
        #[arg(long, value_name = "STATE")]
        jurisdiction: Option<String>,
        /// Firm name to assign to the live Project (requires --brand).
        #[arg(long, requires = "brand")]
        firm: Option<String>,
        /// Brand key the selected Firm wears (requires --firm).
        #[arg(long, requires = "firm")]
        brand: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Run the read-only document checks over a documents tree (LAW-217).
    Documents {
        #[command(subcommand)]
        action: DocumentsCmd,
    },
    /// Watch open Projects, fetch remote bookmarks, and download document revisions.
    Watch {
        /// Directory holding one checkout per Project.
        #[arg(long)]
        dir: PathBuf,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Reconcile the Git-ignored `documents/` and the committed `blobs/` with
    /// the live Project in both directions: download live revisions, upload
    /// local edits as new revisions, and ask before overwriting either side.
    /// Compares the local bytes (`documents/<slug>`), the committed pointer
    /// (`blobs/<slug>.yaml`), and the live operative revision for every
    /// document. A checkout still carrying pointers under `documents/` has
    /// them moved to `blobs/` first.
    #[command(after_long_help = DOCUMENT_SYNC_HELP)]
    Sync {
        /// Preview every action and question without writing or uploading.
        #[arg(long)]
        dry_run: bool,
        /// Answer yes to every overwrite and upload question. It never picks a
        /// side in a conflict. Without a terminal and without this flag,
        /// nothing is overwritten and the command exits non-zero.
        #[arg(long)]
        yes: bool,
        /// Answer yes to every deletion. A deleted pointer soft-deletes the
        /// live document for 180 days, and `--yes` never covers that; without
        /// a terminal and without this flag, no document is deleted.
        #[arg(long)]
        yes_delete: bool,
        /// Reconcile only this path below documents/ or blobs/ (a file or directory).
        #[arg(long, value_name = "DOCUMENT_PATH")]
        only: Option<PathBuf>,
    },
    /// Show this Project's notation runs against each current workflow template.
    /// Reads `navigator.yaml` in the current checkout to choose the live matter.
    Notations {
        /// Fail when a lawyer review or client re-ask has remained open this long.
        #[arg(long, default_value = "3d")]
        stale: String,
        /// Emit the complete workflow board as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Open a matter through the live site's `POST /app/api/projects`, the
    /// caller's own bearer token attached so the conflict attestation stays
    /// a personal act.
    Create {
        /// Human-readable matter name, e.g. `"Shook Estate"`.
        #[arg(long)]
        name: String,
        /// The matter's code, e.g. `shook-estate`, stored exactly as given.
        /// Required — lowercase letters, digits, and single hyphens. A code
        /// is chosen once at matter-open and never changes; a code already
        /// in use by another matter is refused, not disambiguated.
        #[arg(long)]
        code: String,
        /// Email of the pre-existing **client** Person this matter is
        /// opened for — its client-side DRI. Required, and must be a
        /// `role = client` person (create the client first with
        /// `navigator site import person <seed-file>`). The lawyer-side DRI
        /// is the attester, resolved from this login.
        #[arg(long)]
        client_email: String,
        /// Exact `entities.name` of an **existing** legal organization this
        /// Project tracks. Omit for an individual client with no company —
        /// this then creates a `Human` entity named for the client instead,
        /// which requires `--jurisdiction`. Conflicts with `--jurisdiction`.
        #[arg(long, conflicts_with = "jurisdiction")]
        entity_name: Option<String>,
        /// The individual client's home jurisdiction, e.g. `Nevada` —
        /// required when `--entity-name` is omitted, so the `Human` entity
        /// this command creates never silently lands in the firm's own
        /// jurisdiction. Conflicts with `--entity-name`.
        #[arg(long, conflicts_with = "entity_name")]
        jurisdiction: Option<String>,
        /// The opening attorney's conflict attestation. Required on every
        /// Project open: passing `--attest` affirms the attorney has checked
        /// for conflicts, and that either none prevent the open or this
        /// Project is not legal advice. Without it the open is refused — it
        /// is never defaulted.
        #[arg(long)]
        attest: bool,
        /// Open the matter already closed — an engagement that ended
        /// before anyone opened its row. Requires `--closed-at`.
        #[arg(long, requires = "closed_at")]
        closed: bool,
        /// The close time, required exactly when `--closed` is set and
        /// refused otherwise. May predate today — there is no prior state
        /// for it to have preceded.
        #[arg(long, requires = "closed")]
        closed_at: Option<chrono::DateTime<chrono::Utc>>,
        /// Owning Firm name. Requires --brand.
        #[arg(long, requires = "brand")]
        firm: Option<String>,
        /// Brand key worn by the Firm. Requires --firm.
        #[arg(long, requires = "firm")]
        brand: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Send a checkout's headless source to the Project's Forgejo repository
    /// as its first commit. The archive is built and checked with the same
    /// rules the server applies; rerunning the same tree joins the same import.
    ImportSource {
        /// The Project code.
        project_code: String,
        /// The checkout to send. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Idempotency key. Defaults to the code and the tree digest.
        #[arg(long)]
        operation_key: Option<String>,
        /// Wait until the import commits or is refused.
        #[arg(long)]
        wait: bool,
        #[command(flatten)]
        host: HostOpt,
    },
    /// File a Project's Drive dropbox folder into the matter through the
    /// portal's drive-import door, as internal documents under their original
    /// filenames. `--dry-run` plans the pass and writes nothing.
    DriveIngest {
        /// The Project code.
        project_code: String,
        /// Report what a real pass would do, and write nothing.
        #[arg(long)]
        dry_run: bool,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Interactively open a Project and write this release's canonical repository files.
    Scaffold {
        /// Empty directory to populate. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Finish an interrupted scaffold of an already-open Project: skip
        /// the prompts and the open, write or repair the tree, and retry
        /// repository provisioning.
        #[arg(long, value_name = "CODE")]
        resume: Option<String>,
        /// Keep the Project repository-less: write the tree but do not ask
        /// the server to create or adopt a source repository.
        #[arg(long)]
        no_repository: bool,
        /// Client contact email for the manifest's client DRI. Required when
        /// `--resume` rebuilds the tree in an empty directory.
        #[arg(long, value_name = "EMAIL")]
        client_email: Option<String>,
        /// Owning Firm name. Requires --brand.
        #[arg(long, requires = "brand", conflicts_with = "resume")]
        firm: Option<String>,
        /// Brand key worn by the Firm. Requires --firm.
        #[arg(long, requires = "firm", conflicts_with = "resume")]
        brand: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Close an existing Project through the live site's lifecycle command,
    /// then archive its repository as a `closed_repository` document: zip the
    /// working tree at HEAD (no git history), record the commit SHA, and file
    /// it. The content hash needs no separate flag — the server derives it
    /// from the uploaded bytes.
    Close {
        /// Project code, resolved only against Projects visible to the login.
        project_code: String,
        /// Why the matter closed. Pitch reasons require an offboarding document;
        /// active-matter reasons require an onboarding document.
        #[arg(long, value_enum)]
        reason: ClosureReasonArg,
        /// RFC 3339 time when the matter actually closed.
        #[arg(long)]
        effective_at: Option<chrono::DateTime<chrono::Utc>>,
        /// The local checkout to archive. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Reopen a closed Project. Unarchive its source repository separately.
    Reopen {
        /// Project code, resolved against Projects visible to the login.
        project_code: String,
        /// Why the matter reopened.
        #[arg(long, value_enum)]
        reason: ReopenReasonArg,
        /// RFC 3339 time when the matter actually reopened.
        #[arg(long)]
        effective_at: Option<chrono::DateTime<chrono::Utc>>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Verify this machine and a Project workspace before Navigator creates
    /// anything: the active deployment, its Google Workspace, Shared Drive
    /// and Projects root, an optional local Drive mount, the stored site
    /// login, and — with `--project` — that Project's folder path and its
    /// one repository coordinate.
    ///
    /// Strictly read-only: it creates no folder, writes no file, provisions
    /// no repository, and makes no network call. A Workspace, Drive, folder,
    /// or identity mismatch exits nonzero.
    ///
    /// Distinct from `ops doctor`, which diagnoses Kubernetes scheduled-job
    /// health in a running cluster.
    Doctor {
        #[command(flatten)]
        host: HostOpt,
        /// Project code to resolve folder and repository coordinates for,
        /// e.g. `acme`. Omit to check deployment-wide configuration only.
        #[arg(long)]
        project: Option<String>,
    },
    /// Reconcile Project repositories against the live Project rows, and
    /// report where the two disagree.
    ///
    /// One `projects.code` names both a repository and a row, and nothing
    /// makes the two agree. This reads every `navigator.yaml` under `--dir`,
    /// lists the live rows, and reports both directions: a repository whose
    /// code no row carries, a row recording no repository at all, a row whose
    /// `repository_url` names a repository that is not present, and a code
    /// the two sides spell differently.
    ///
    /// The row-side findings assume `--dir` holds the whole fleet — run
    /// against part of it, "no repository is present" is true but
    /// uninteresting, so each such finding names the directory it searched.
    ///
    /// A repository that is *meant* to have no row says so in its own
    /// `navigator.yaml`, with `no_live_row: <reason>`. Those are counted in
    /// the footer and listed by `--all`, never failed.
    ///
    /// Strictly read-only: it creates no row, patches none, and closes none.
    /// Reconciling a repository to a row is a decision about a matter.
    Drift {
        #[command(flatten)]
        host: HostOpt,
        /// Directory holding the Project repository checkouts, one per
        /// repository. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Also list the repositories that declare they have no live row.
        #[arg(long)]
        all: bool,
        /// Emit the report as JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Check this repository: Markdown, YAML, seeds, locales, and — when
    /// `navigator.yaml` declares a Project — its layout, mount, and origin.
    ///
    /// Runs on the whole tree from the repository root, which it identifies by
    /// the `README` and `.git` beside it, and refuses to run anywhere else.
    /// Safe-by-construction rule fixes (whitespace, ATX heading spacing,
    /// blockquote spacing, S102 paragraph packing) are applied in place; what
    /// is left needs a human. Every Project repository's CI runs this command.
    ///
    /// A Project uses the exact release pinned in `navigator.yaml`. A missing
    /// release is downloaded, verified, and cached before any content check or fix.
    ///
    /// `--check` (LAW-62) runs only the live document check, comparing
    /// committed pointers with the live record and checking transcript source
    /// revisions, and skips every offline pass above entirely — a separate CI
    /// job asks it after `verify` has already run those, including the origin
    /// pass, over the same tree.
    Gate {
        /// Run only `Y024`: every git-tracked or stageable file must be
        /// Markdown or YAML, or one of the canonical files at its release
        /// bytes. Writes no repository content; each offending path is named.
        /// Downloads the pinned release if it is not cached. The shared
        /// workflow's required `ci` job runs this.
        #[arg(long, conflicts_with_all = ["check", "deep", "ci"])]
        tracked_files: bool,
        /// Never writes to the live site.
        ///
        /// Checks only the live document record; every offline pass above is
        /// skipped (LAW-62). Rewrites a drifted pointer, writes a missing
        /// pointer, and moves pointers left under `documents/` to `blobs/`. A missing or
        /// corrupt object, or a live row with no slug, needs a person. Under
        /// `--ci` any of those fixes fails the job and names the fix.
        /// A live row with no slug is `navigator-deploy site document slug`. A missing
        /// storage object is `navigator-deploy site document repair`. Uploading a
        /// staged file is `navigator project sync`.
        #[arg(long)]
        check: bool,
        /// Re-hash every stored object while `--check` is running, and hash
        /// the local bytes under `documents/` against their pointers. Bytes
        /// that differ from their pointer fail the run; bytes never
        /// downloaded are reported and pass.
        #[arg(long, requires = "check")]
        deep: bool,
        /// Hold the run to what CI can prove. No repository content is written,
        /// so a file the gate would have fixed is a finding rather than a silent rewrite of a
        /// checkout about to be discarded. On a push to
        /// `main` (or a pull request merge ref) the live-status door opens,
        /// exchanging GitHub Actions OIDC at `POST /auth/ci/document-token` to
        /// check `navigator.yaml` against the row the deployment holds. With
        /// `--check`, a pointer or gitignore the gate would write fails the
        /// job instead. The host is the one `navigator.yaml` declares — there
        /// is nothing to pass.
        #[arg(long)]
        ci: bool,
    },
    /// Report this push to `main` to the Project's private Slack channel.
    ///
    /// Run by a Project repository's CD workflow after its document and seed
    /// jobs. Exchanges the run's OIDC token for the document-scoped session
    /// bound to this repository's live Project, then posts the short SHA, the
    /// run URL, the pusher's login and the outcome. No commit subject is sent.
    NotifyMainPush {
        /// The combined outcome of this push's document and seed jobs.
        #[arg(long, value_enum)]
        gate: projects::main_push::Outcome,
        /// Required: this runs only from CI, with the run's OIDC token.
        #[arg(long, required = true)]
        ci: bool,
    },
    /// This checkout's declared matter pages: `init` proposes a `pages:`
    /// block from the pinned Project Skills, Drafts, and live data.
    Pages {
        #[command(subcommand)]
        action: projects::pages_init::PagesCmd,
    },
    /// Complete an existing Project's Drive, GitHub, Slack, Notion, and shared
    /// Google Calendar setup. A closed Project's calendar is made read-only.
    ///
    /// Each resource is attempted through the logged-in deployment's existing
    /// authenticated door and reported separately. A failed resource is safe
    /// to retry; already-recorded provider identities are never replaced.
    Setup {
        /// Project code. Defaults to `navigator.yaml`'s `project.name` when
        /// run from a Project repository root; required otherwise, unless
        /// `--all` is supplied.
        #[arg(conflicts_with = "all")]
        project_code: Option<String>,
        /// Complete every Project visible to this login.
        #[arg(long)]
        all: bool,
        /// Emit one JSON result per Project and resource.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        host: HostOpt,
    },
    /// The Project Skill catalog — jurisdiction/practice-area playbooks
    /// compiled into this binary from `skills/` — and the pins a Project
    /// records against it in its own `navigator.yaml`.
    Skill {
        #[command(subcommand)]
        action: ProjectSkillCmd,
    },
    /// Repair this CLI release's canonical Agent Skills and `AGENTS.md`.
    Repository {
        #[command(subcommand)]
        action: ProjectRepositoryCmd,
    },
    /// The lawyer's view of a checkout tracked by Jujutsu (`jj`): history,
    /// and one day conflicts and change details, in plain terms.
    Jj {
        #[command(subcommand)]
        action: ProjectJjCmd,
    },
    /// Search extracted text in this Project's documents and notes.
    Search {
        /// Regex pattern to match in extracted text.
        pattern: String,
        /// Project repository root. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Match without regard to letter case.
        #[arg(long)]
        ignore_case: bool,
        /// Number of context lines to include around each match.
        #[arg(long, default_value_t = 0)]
        context: usize,
        /// Emit hits and coverage as JSON.
        #[arg(long)]
        json: bool,
        /// Restrict results to a record class, currently `court`.
        #[arg(long)]
        record: Option<String>,
    },
    /// Create or adopt a Project's external surfaces through the live
    /// site's admin reconcile door.
    ///
    /// `reconcile` is the idempotent backfill: for every Project visible to
    /// this login (or one `--project` code) it runs the same pass matter-open
    /// runs best-effort, so a matter opened while the forge was down gains
    /// its governed, seeded repository on the next run, and re-running
    /// changes nothing. Each Project reports its own outcome — a Firm whose
    /// Forgejo is unconfigured is skipped with the missing coordinate
    /// named, never a failed run.
    Surfaces {
        #[command(subcommand)]
        action: SurfacesCmd,
    },
}

#[derive(Subcommand)]
pub enum SurfacesCmd {
    /// Create or adopt each Project's source repository — seeded and
    /// governed when the forge is Forgejo — through
    /// `POST /app/api/project-surfaces/{id}`.
    Reconcile {
        /// Project code, e.g. `acme`. Omit to reconcile every Project
        /// visible to this login.
        #[arg(long)]
        project: Option<String>,
        /// Emit the per-Project outcomes as JSON.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        host: HostOpt,
    },
}

#[derive(Subcommand)]
pub enum ProjectJjCmd {
    /// Show the checkout's history in lawyer terms: each change's
    /// description, author, and time, with an explicit warning on a
    /// conflicted change. Requires `navigator project repository jj` on the
    /// checkout first.
    Log {
        /// Project repository root. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
}

#[derive(Subcommand)]
pub enum ProjectRepositoryCmd {
    /// Restore missing or drifted canonical Agent Skills, `AGENTS.md`, and
    /// the root `.gitignore` lines this release requires.
    SyncSkills {
        /// Existing Project repository root. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    /// Put Jujutsu (`jj`) over this Git checkout: install it with Homebrew
    /// when missing (macOS), colocate a `.jj` store beside `.git`, and copy
    /// the Git author and signing settings into jj's repo config.
    ///
    /// The forge still sees plain Git. `jj git push --change @` names its
    /// bookmark `push/<change-id>`, the namespace `push` already uses.
    /// Idempotent: a re-run rewrites the same settings.
    Jj {
        /// Project repository root. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    /// Move a checkout's pointers to `blobs/` and its document bytes into
    /// their matter folders under `documents/`.
    ///
    /// Offline and idempotent. Pointers are renamed, never rewritten, so a
    /// sha256, asset id, or version chain never changes. Every move is
    /// printed. `navigator project sync` makes the same moves first.
    MigrateDocuments {
        /// Project repository root. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Print the pointer moves without making them.
        #[arg(long)]
        dry_run: bool,
    },
    /// Reslug a checkout into the extended matter folder tree (LAW-215):
    /// refile bare `Discovery` pointers onto `Discovery (Served)` and move
    /// every pointer's bytes from the five-folder tree into the folder the
    /// pointer resolves to now.
    ///
    /// Offline and idempotent. A pointer's checksum, asset id, and version
    /// chain never change; the one pointer rewrite is the bare `Discovery`
    /// folder line. Bytes under `documents/` no pointer names are listed,
    /// never guessed at. `navigator project sync` creates the new tree, but
    /// does not refile.
    MigrateFolders {
        /// Project repository root. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Print the refiles and moves without making them.
        #[arg(long)]
        dry_run: bool,
    },
    /// Land this checkout's change and follow it through the merge loop:
    /// preflight, branch, signed commit, push, the review request ready for
    /// review, merge-when-green armed and read back, the required check
    /// watched to a verdict, the merge commit captured, and the deployment
    /// run for the merge commit watched to a verdict.
    Push {
        /// Project repository root. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// The base branch the review request merges into.
        #[arg(long, default_value = "main")]
        base: String,
        /// The branch to land. Derived from the title when omitted.
        #[arg(long)]
        branch: Option<String>,
        /// The commit and review-request title.
        #[arg(long)]
        title: String,
        /// The review-request body (Markdown).
        #[arg(long)]
        body: Option<String>,
        /// Run the preflight and print the plan without pushing anything.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
pub enum DocumentsCmd {
    /// Read-only check of a documents tree with the same matter-folder,
    /// filename, and collision rules a checkout's `documents/` lives under:
    /// nothing is uploaded, renamed, or moved (LAW-217).
    Check {
        /// Project checkout root whose committed pointers `--deep` compares
        /// against. Defaults to the current directory. Without
        /// `--documents-dir` or `--drive-local`, the tree checked is this
        /// checkout's own `documents/`.
        dir: Option<PathBuf>,
        /// Check this tree instead of the checkout's `documents/`. Any tree
        /// laid out like `documents/` works — a Google-Drive matter folder
        /// included — and no `navigator.yaml` is read without `--deep`.
        #[arg(long, value_name = "PATH")]
        documents_dir: Option<PathBuf>,
        /// Check the Project's matter folder in Google Drive for desktop
        /// instead, resolved from `~/Library/CloudStorage` by the Project
        /// code in `navigator.yaml`.
        #[arg(long, conflicts_with = "documents_dir")]
        drive_local: bool,
        /// Also compare each committed pointer under `<dir>/blobs` against
        /// its `sha256` and `size_bytes`, wherever the bytes live.
        #[arg(long)]
        deep: bool,
        /// Emit the report as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum DocketCmd {
    /// Ingest saved CM/ECF Notice of Electronic Filing emails.
    ///
    /// Run from the root of a Project repository. The Project and host come
    /// from navigator.yaml, and only notices for the case numbers it lists
    /// under `dockets:` (or `matter.case_number`) are read. Each document's
    /// bytes are written under documents/Docket Filings/ before anything else,
    /// because a free-look link is single-use.
    Ingest {
        /// One or more `.eml` notices.
        #[arg(required = true)]
        notices: Vec<PathBuf>,
        /// Print the plan without downloading, uploading, or writing anything.
        #[arg(long)]
        dry_run: bool,
        /// File only this docket entry number. Repeatable.
        #[arg(long)]
        only: Vec<String>,
        /// Skip a notice whose docket text contains this text, ignoring case.
        /// Repeatable.
        #[arg(long = "skip-pattern")]
        skip_pattern: Vec<String>,
        /// File every document as this kind instead of the suggested one.
        #[arg(long)]
        kind: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// List the cases and docket entries stored on a Project.
    List {
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// List every Docket Filings document beside the `DKT NNNN - <title>` name its
    /// docket entry gives it. Pass --apply to reconcile linked documents.
    ///
    /// A filing no entry links, or two that would share a name, are listed
    /// as UNRESOLVED for a lawyer to settle.
    Renumber {
        /// Apply names from linked Docket Entries after checking unresolved filings.
        #[arg(long)]
        apply: bool,
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Record a docket entry and print the row that was written.
    ///
    /// The case comes from navigator.yaml: `--docket` picks one of its
    /// `dockets:` case numbers, and the only one is used when there is one.
    /// A Project with no case row yet gets one opened from that number.
    Add {
        /// The court's entry number, as text (`29` or `29-1`).
        #[arg(long)]
        entry_number: String,
        /// `pleading`, `motion`, `opposition`, `reply`, `order`, `notice`,
        /// `stipulation`, `discovery_request`, `discovery_response`,
        /// `subpoena`, `expert_disclosure`, `hearing`, `trial`, `appeal`,
        /// `settlement`, or `other`.
        #[arg(long)]
        kind: String,
        /// The entry's text as the docket prints it.
        #[arg(long)]
        title: String,
        /// Who filed or served it.
        #[arg(long)]
        party: Option<String>,
        /// Date filed or served, `YYYY-MM-DD`.
        #[arg(long)]
        filed_on: Option<String>,
        /// When the court set a hearing or trial, RFC 3339 with an offset.
        /// Required for those two kinds and refused for every other.
        #[arg(long)]
        scheduled_on: Option<String>,
        /// The filed document's asset id.
        #[arg(long)]
        document: Option<uuid::Uuid>,
        /// The filed document's path below `documents/`, instead of its id.
        #[arg(long)]
        slug: Option<String>,
        /// Which `dockets:` case number the entry belongs to.
        #[arg(long)]
        docket: Option<String>,
        /// Print the request and write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Name the document on file for an entry and print the stored row.
    LinkDocument {
        /// Docket entry id from project docket list.
        id: uuid::Uuid,
        /// The filed document's asset id.
        #[arg(long)]
        document: Option<uuid::Uuid>,
        /// The filed document's path below `documents/`, instead of its id.
        #[arg(long)]
        slug: Option<String>,
        /// Print the request and write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Continue a hearing or trial: record a new entry carrying the new date
    /// and a supersedes link to this one, and print the new row.
    Supersede {
        /// The hearing or trial entry id from project docket list.
        id: uuid::Uuid,
        /// The court's entry number for the continuance.
        #[arg(long)]
        entry_number: String,
        /// The date the appearance moved to, RFC 3339 with an offset.
        #[arg(long)]
        scheduled_on: String,
        /// A new title; the prior entry's when omitted.
        #[arg(long)]
        title: Option<String>,
        /// Who filed or served the continuance.
        #[arg(long)]
        party: Option<String>,
        /// Date filed or served, `YYYY-MM-DD`.
        #[arg(long)]
        filed_on: Option<String>,
        /// The order's asset id.
        #[arg(long)]
        document: Option<uuid::Uuid>,
        /// The order's path below `documents/`, instead of its id.
        #[arg(long)]
        slug: Option<String>,
        /// Print the request and write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
pub enum CommunicationCmd {
    /// List the Communications stored on a Project (the firm view).
    List {
        /// Only this channel, such as `call`.
        #[arg(long)]
        channel: Option<String>,
        /// Only this direction: `inbound`, `outbound`, or `internal`.
        #[arg(long)]
        direction: Option<String>,
        /// Only Communications with a filed attachment.
        #[arg(long)]
        attachments: bool,
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Log a call or email and print the stored Communication.
    ///
    /// Communications stay server-only: nothing is written under
    /// `documents/`, and `project sync` writes no pointer for one.
    Create {
        /// `call`, `email_inbound`, or `email_outbound`.
        #[arg(long, default_value = "call")]
        channel: String,
        /// `inbound` or `outbound`: who placed the call or sent the email.
        #[arg(long)]
        direction: String,
        /// The other party, as an email or a name.
        #[arg(long)]
        counterparty: String,
        /// `opposing_counsel`, `client`, `court`, or `other`. A call with
        /// opposing counsel is always internal.
        #[arg(long)]
        role: Option<String>,
        /// RFC 3339 with an explicit offset: when the call or email happened.
        #[arg(long)]
        at: String,
        /// Whole minutes.
        #[arg(long)]
        duration: Option<i64>,
        /// The note's Markdown body.
        #[arg(long)]
        body_file: PathBuf,
        /// `none` or `all_party_consent`.
        #[arg(long, default_value = "none")]
        recorded: String,
        /// The issue a meet-and-confer call addressed.
        #[arg(long)]
        meet_and_confer: Option<String>,
        /// Something agreed on the call; repeatable. Stays unconfirmed until
        /// an email logged with `--confirms` names this call.
        #[arg(long)]
        agreement: Vec<String>,
        /// The call whose agreements this email confirms.
        #[arg(long)]
        confirms: Option<uuid::Uuid>,
        /// Outbound email whose receipt this inbound email acknowledges.
        #[arg(long)]
        acknowledges: Option<uuid::Uuid>,
        /// External message identity for traceability and idempotent ingest.
        #[arg(long)]
        source_ref: Option<String>,
        /// Share the note with the client. Refused for `--role opposing_counsel`.
        #[arg(long)]
        client_visible: bool,
        /// A subject line.
        #[arg(long)]
        subject: Option<String>,
        /// Print the request and write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Log a firm-internal matter note and print the stored Communication
    /// (LAW-230).
    ///
    /// A note has no counterparty and no client visibility: it is the
    /// lawyer's own observation on the matter, rendered on the workbench's
    /// Notes tab and never on a client view. Communications stay
    /// server-only: nothing is written under `documents/`, and `project
    /// sync` writes no pointer for one.
    Note {
        /// The note's Markdown body.
        #[arg(long)]
        body_file: PathBuf,
        /// A subject line.
        #[arg(long)]
        subject: Option<String>,
        /// Print the request and write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
pub enum LegalPlanCmd {
    /// The matter's plan, Plan Case Number, and plan dates.
    Show {
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Link the matter to one of the Firm's plans; only the flags given change.
    Set {
        #[command(flatten)]
        fields: legal_plans::PlanFields,
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
pub enum ServiceCmd {
    /// List the service records stored on a Project.
    List {
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Print one stored service record.
    Show {
        /// Service id from project service list.
        id: uuid::Uuid,
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Record that one served document reached one recipient by one method
    /// on one date, and print the row that was written.
    Add {
        /// The served document's asset id.
        #[arg(long)]
        document: Option<uuid::Uuid>,
        /// The served document's path below `documents/`, instead of its id.
        #[arg(long)]
        slug: Option<String>,
        /// The recipient as the certificate names them.
        #[arg(long)]
        recipient: String,
        /// The recipient's Person id, when one exists.
        #[arg(long)]
        recipient_person: Option<uuid::Uuid>,
        /// The recipient's Entity id, when one exists. Not with a Person.
        #[arg(long)]
        recipient_entity: Option<uuid::Uuid>,
        /// The party the recipient appears for, when counsel of record.
        #[arg(long)]
        counsel_for: Option<String>,
        /// `personal`, `mail`, `certified_mail`, `overnight`, `e_service`,
        /// or `electronic_by_agreement`.
        #[arg(long)]
        method: String,
        /// `YYYY-MM-DD`. Stored as given; no response deadline is computed.
        #[arg(long)]
        served_on: String,
        /// The address or e-service handle the document went to.
        #[arg(long)]
        address: String,
        /// Who served it, as the declarant block names them.
        #[arg(long)]
        declarant: String,
        /// The declarant's Person id, when one exists.
        #[arg(long)]
        declarant_person: Option<uuid::Uuid>,
        /// The outgoing Letter that is the evidence of a send Navigator made.
        #[arg(long)]
        letter: Option<uuid::Uuid>,
        /// The sent email that is the evidence of a send Navigator made.
        #[arg(long)]
        sent_email: Option<uuid::Uuid>,
        /// Print the request and write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
}

#[derive(Subcommand)]
pub enum DeadlineCmd {
    /// Show a cited date proposal; only --confirm writes a stored Deadline.
    Propose {
        #[command(flatten)]
        args: projects::deadline_proposal::ProposalArgs,
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// List the deadlines stored on a Project.
    List {
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Store a deadline and print the row that was written.
    Create {
        /// What is due, such as the pleading or response (at most 200 characters).
        #[arg(long)]
        title: String,
        /// Additional explanation of what is due. An empty update clears it.
        #[arg(long)]
        description: Option<String>,
        /// Internal or client visibility, expressed through the client lead time.
        #[arg(long, value_parser = ["internal", "client"])]
        visibility: Option<String>,
        /// Write the saved row as an RFC 5545 calendar to this private file.
        #[arg(long)]
        ics: Option<PathBuf>,
        /// Court-supplied location; included only in the exported file.
        #[arg(long, requires = "ics")]
        location: Option<String>,
        /// RFC 3339 timestamp with an explicit offset.
        #[arg(long)]
        due_at: String,
        /// `statute`, `court_rule`, `court_order`, `contract`, or `internal`.
        #[arg(long)]
        authority_kind: String,
        /// Citation. Required for `statute` and `court_rule`.
        #[arg(long)]
        authority: Option<String>,
        /// Producing workflow, or lawyer for a hand-entered date.
        #[arg(long)]
        source: Option<String>,
        /// Document that records the date.
        #[arg(long)]
        source_document: Option<uuid::Uuid>,
        /// Person responsible for the date.
        #[arg(long)]
        owner: Option<uuid::Uuid>,
        /// `firm`, `opponent`, or `hearing`.
        #[arg(long)]
        calendar_role: String,
        /// `court`, `hearing`, `discovery`, or `internal`. Defaults from the
        /// calendar role and authority kind when either settles it.
        #[arg(long)]
        category: Option<String>,
        /// Whole days. Never shorter than the client lead.
        #[arg(long)]
        internal_lead_days: Option<i64>,
        /// Whole days. Requires an internal lead.
        #[arg(long)]
        client_lead_days: Option<i64>,
        /// Updates the same row when a workflow replays this key.
        #[arg(long)]
        replay_key: Option<String>,
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Change stored fields and print the row that was written.
    Update {
        /// Deadline id from project deadline list.
        id: uuid::Uuid,
        /// What is due. An empty value clears the title.
        #[arg(long)]
        title: Option<String>,
        /// Additional explanation of what is due. An empty update clears it.
        #[arg(long)]
        description: Option<String>,
        /// Internal or client visibility, expressed through the client lead time.
        #[arg(long, value_parser = ["internal", "client"])]
        visibility: Option<String>,
        /// Write the saved row as an RFC 5545 calendar to this private file.
        #[arg(long)]
        ics: Option<PathBuf>,
        /// Court-supplied location; included only in the exported file.
        #[arg(long, requires = "ics")]
        location: Option<String>,
        #[arg(long)]
        due_at: Option<String>,
        #[arg(long)]
        authority_kind: Option<String>,
        #[arg(long)]
        authority: Option<String>,
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        source_document: Option<uuid::Uuid>,
        #[arg(long)]
        owner: Option<uuid::Uuid>,
        #[arg(long)]
        calendar_role: Option<String>,
        /// `court`, `hearing`, `discovery`, or `internal`. An empty value clears it.
        #[arg(long)]
        category: Option<String>,
        #[arg(long)]
        internal_lead_days: Option<i64>,
        #[arg(long)]
        client_lead_days: Option<i64>,
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Set status to complete and print the stored row.
    Complete {
        /// Deadline id from project deadline list.
        id: uuid::Uuid,
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Set status to cancelled and print the stored row.
    Cancel {
        /// Deadline id from project deadline list.
        id: uuid::Uuid,
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Print every deadline on the Project as one RFC 5545 calendar.
    Ics {
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Mirror the Project's deadlines into its shared matter calendar
    /// through the portal, repairing drift. Requires the Firm's calendar
    /// integration; `--dry-run` plans without writing.
    Sync {
        /// Plan the mirror without writing — the door answers what it
        /// would insert, patch, and delete.
        #[arg(long)]
        dry_run: bool,
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Reopen a cancelled deadline and print the stored row.
    Reopen {
        /// Deadline id from project deadline list.
        id: uuid::Uuid,
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
}

#[derive(Subcommand)]
pub enum EfilingCmd {
    /// List the e-filing transactions recorded on a Project.
    List {
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Record the status an EFSP email reports, filing an attached conformed copy.
    Record {
        /// The EFSP status email, saved as `.eml`.
        email: std::path::PathBuf,
        #[command(flatten)]
        details: EfilingDetails,
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// File a hand-downloaded stamped PDF and link it to its transaction.
    Pair {
        /// The court-stamped PDF the attorney downloaded.
        file: std::path::PathBuf,
        /// The EFSP transaction id the stamped copy belongs to.
        #[arg(long)]
        transaction: String,
        /// Date the clerk accepted the submission (YYYY-MM-DD).
        #[arg(long)]
        accepted_on: Option<String>,
        #[command(flatten)]
        details: EfilingDetails,
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
}

/// Transaction details that override what an email or upload says.
#[derive(clap::Args)]
pub struct EfilingDetails {
    /// Asset id of the submitted revision this transaction carried.
    #[arg(long)]
    pub submitted: Option<uuid::Uuid>,
    /// E-filing service provider name.
    #[arg(long)]
    pub efsp: Option<String>,
    /// Court the transaction was filed in.
    #[arg(long)]
    pub court: Option<String>,
    /// Court case number.
    #[arg(long)]
    pub case_number: Option<String>,
    /// Date the court deems the document filed (YYYY-MM-DD). Accepted transactions only.
    #[arg(long)]
    pub filed_on: Option<String>,
    /// The return is a judge-signed document's received copy; the entered order is still needed.
    #[arg(long)]
    pub judge_signed: bool,
}

impl EfilingDetails {
    fn into_overrides(self) -> projects::efiling::Overrides {
        projects::efiling::Overrides {
            submitted: self.submitted,
            efsp: self.efsp,
            court: self.court,
            case_number: self.case_number,
            filed_on: self.filed_on,
            judge_signed: self.judge_signed,
        }
    }
}

#[derive(Subcommand)]
pub enum ParticipantCmd {
    /// List participation rows, including their identifiers and DRI flags.
    List {
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Designate a participant as a DRI on the side their role permits.
    Dri {
        /// Participation row id from project participant list.
        participation_id: uuid::Uuid,
        /// Remove the marker. The last lawyer DRI cannot be cleared.
        #[arg(long)]
        clear: bool,
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
}

/// The party a reference or adversity verb names: exactly one of a Person by
/// email or an Entity by name, as `seeds/ProjectReference.yaml` spells them.
#[derive(clap::Args)]
pub struct PartyOpt {
    /// The referenced Person's email. A first reference creates the row with
    /// `--person-name`; no participation is written.
    #[arg(long, conflicts_with = "entity_name")]
    pub person_email: Option<String>,
    /// The display name a first reference creates the Person with.
    #[arg(long, requires = "person_email")]
    pub person_name: Option<String>,
    /// The referenced Entity's name. A first reference creates the row with
    /// `--entity-type` and `--jurisdiction`.
    #[arg(long)]
    pub entity_name: Option<String>,
    /// The Entity type name, such as `LLC`.
    #[arg(long, requires = "entity_name")]
    pub entity_type: Option<String>,
    /// The Entity's jurisdiction name, such as `Nevada`.
    #[arg(long, requires = "entity_name")]
    pub jurisdiction: Option<String>,
}

impl PartyOpt {
    fn into_args(self) -> projects::reference::PartyArgs {
        projects::reference::PartyArgs {
            person_email: self.person_email,
            person_name: self.person_name,
            entity_name: self.entity_name,
            entity_type: self.entity_type,
            jurisdiction: self.jurisdiction,
        }
    }
}

#[derive(Subcommand)]
pub enum ReferenceCmd {
    /// Reference a party, through the `project_reference` seed model.
    Add {
        /// Why the Project names them: `counterparty_officer`,
        /// `registered_agent`, `signatory`, `adverse_party`, or
        /// `counterparty_entity`.
        #[arg(long)]
        reason: String,
        #[command(flatten)]
        party: PartyOpt,
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
}

#[derive(Subcommand)]
pub enum AdverseCmd {
    /// Record a manual, full-confidence `adverse_to` edge from the client to
    /// a party this Project already references.
    Add {
        #[command(flatten)]
        party: PartyOpt,
        /// Which side of the client the edge starts from: `entity` (the
        /// Project's Entity) or `client_dri`.
        #[arg(long, default_value = "entity")]
        subject: String,
        /// Project code. Defaults to project.name in navigator.yaml.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
}

#[derive(Subcommand)]
pub enum ProjectSkillCmd {
    /// List every catalog entry's jurisdiction, practice area, and name.
    List,
    /// Print one catalog entry's full body.
    ///
    /// `jurisdiction` matches case-insensitively (`show nv estates` and
    /// `show NV estates` resolve the same entry). An unrecognized pair
    /// exits non-zero, naming the closest valid catalog entries.
    Show {
        jurisdiction: String,
        practice_area: String,
    },
    /// Pin a catalog entry's version onto the current Project's
    /// `navigator.yaml`, and scaffold each Notation it bundles into
    /// `templates/`.
    ///
    /// Reads the compiled-in catalog only — there is no network fetch.
    /// Idempotent: re-running `use` for a pair already pinned at the same
    /// version is a no-op, and a new catalog version updates the recorded
    /// pin in place rather than duplicating it. Run from the Project
    /// repository root.
    Use {
        jurisdiction: String,
        practice_area: String,
    },
    /// List the Project Skills pinned on the current Project, and whether
    /// each still resolves in the compiled-in catalog.
    ///
    /// Reuses the same resolution `navigator project gate --check` runs
    /// (ENG-879), so the two report the same verdict for the same
    /// `navigator.yaml`. Run from the Project repository root.
    Status,
    /// Prepare a Kit template defect report for the Lawyers team's issue
    /// tracker, from inside the matter.
    ///
    /// Reads the draft (a file, or stdin when `--file` is omitted), resolves
    /// this matter's private terms through the site session, and scans the
    /// draft locally with the same scanner `navigator project feedback-check`
    /// runs. A clean draft prints the prepared issue — Kit provenance from
    /// `navigator.yaml` on top, the draft under it — ready to file. A draft
    /// carrying an identifier exits non-zero naming each placeholder to use.
    /// Nothing is sent anywhere. Run from the Project repository root.
    Feedback {
        /// Draft title and body; reads stdin when omitted.
        #[arg(long)]
        file: Option<PathBuf>,
        /// Which pinned Kit to report against. Required only when several
        /// Kits are pinned.
        #[arg(long)]
        jurisdiction: Option<String>,
        /// Which pinned Kit to report against. Required only when several
        /// Kits are pinned.
        #[arg(long)]
        practice_area: Option<String>,
    },
}

/// Shared `--host` selector for the live-site commands: optional, since a
/// single stored login is used by default.
#[derive(clap::Args)]
pub struct HostOpt {
    /// Target host. Optional when exactly one host is logged in.
    #[arg(long)]
    pub host: Option<String>,
}

/// Stack for the thread that runs the CLI. The dispatch below holds every
/// subcommand's future in one frame, which once overflowed a 1 MiB main
/// thread (deploy 26.10.7), so the CLI runs on a thread with headroom rather
/// than trusting the platform default. The extra memory is only reserved
/// address space until touched.
pub const CLI_STACK_BYTES: usize = 64 * 1024 * 1024;

/// CLI spelling of [`model::projects::ClosureReason`]. The store type stays
/// free of clap; `--help` lists these `snake_case` values because the field is
/// a [`clap::ValueEnum`].
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum ReopenReasonArg {
    #[value(name = "new_matter_event")]
    NewMatterEvent,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum ClosureReasonArg {
    PitchDeclined,
    PitchLapsed,
    PitchWithdrawn,
    PitchSuperseded,
    EngagementCompleted,
    ClientTerminated,
    FirmWithdrew,
}

impl ClosureReasonArg {
    const fn into_reason(self) -> model::projects::ClosureReason {
        match self {
            Self::PitchDeclined => model::projects::ClosureReason::PitchDeclined,
            Self::PitchLapsed => model::projects::ClosureReason::PitchLapsed,
            Self::PitchWithdrawn => model::projects::ClosureReason::PitchWithdrawn,
            Self::PitchSuperseded => model::projects::ClosureReason::PitchSuperseded,
            Self::EngagementCompleted => model::projects::ClosureReason::EngagementCompleted,
            Self::ClientTerminated => model::projects::ClosureReason::ClientTerminated,
            Self::FirmWithdrew => model::projects::ClosureReason::FirmWithdrew,
        }
    }
}

#[allow(clippy::too_many_lines)]
pub async fn run_projects(action: ProjectsCmd) -> ExitCode {
    match action {
        ProjectsCmd::FeedbackCheck {
            file,
            terms,
            projects,
            all_projects,
            host,
        } => {
            projects::feedback_check::run(
                file,
                &terms,
                &projects,
                all_projects,
                host.host.as_deref(),
            )
            .await
        }
        ProjectsCmd::Docket {
            action:
                DocketCmd::Ingest {
                    notices,
                    dry_run,
                    only,
                    skip_pattern,
                    kind,
                    host,
                },
        } => {
            projects::docket::ingest::run(projects::docket::ingest::Options {
                notices,
                dry_run,
                only,
                skip_patterns: skip_pattern,
                kind,
                host: host.host,
            })
            .await
        }
        ProjectsCmd::Docket {
            action:
                DocketCmd::Renumber {
                    host,
                    project,
                    apply,
                },
        } => projects::docket::renumber::run(host.host.as_deref(), project.as_deref(), apply).await,
        ProjectsCmd::Docket {
            action: DocketCmd::List { host, project },
        } => projects::docket::entries::list(host.host.as_deref(), project.as_deref()).await,
        ProjectsCmd::Docket {
            action:
                DocketCmd::Add {
                    entry_number,
                    kind,
                    title,
                    party,
                    filed_on,
                    scheduled_on,
                    document,
                    slug,
                    docket,
                    dry_run,
                    project,
                    host,
                },
        } => {
            projects::docket::entries::add(
                host.host.as_deref(),
                project.as_deref(),
                projects::docket::entries::NewEntry {
                    docket,
                    entry_number,
                    kind,
                    title,
                    party,
                    filed_on,
                    scheduled_on,
                    document,
                    slug,
                },
                dry_run,
            )
            .await
        }
        ProjectsCmd::Docket {
            action:
                DocketCmd::LinkDocument {
                    id,
                    document,
                    slug,
                    dry_run,
                    project,
                    host,
                },
        } => {
            projects::docket::entries::link_document(
                host.host.as_deref(),
                project.as_deref(),
                id,
                document,
                slug.as_deref(),
                dry_run,
            )
            .await
        }
        ProjectsCmd::Docket {
            action:
                DocketCmd::Supersede {
                    id,
                    entry_number,
                    scheduled_on,
                    title,
                    party,
                    filed_on,
                    document,
                    slug,
                    dry_run,
                    project,
                    host,
                },
        } => {
            projects::docket::entries::supersede(
                host.host.as_deref(),
                project.as_deref(),
                id,
                projects::docket::entries::Continuance {
                    entry_number,
                    scheduled_on,
                    title,
                    party,
                    filed_on,
                    document,
                    slug,
                },
                dry_run,
            )
            .await
        }
        ProjectsCmd::Communication { action } => match action {
            CommunicationCmd::List {
                channel,
                direction,
                attachments,
                host,
                project,
            } => {
                projects::communication::list(
                    host.host.as_deref(),
                    project.as_deref(),
                    channel.as_deref(),
                    direction.as_deref(),
                    attachments,
                )
                .await
            }
            CommunicationCmd::Create {
                channel,
                direction,
                counterparty,
                role,
                at,
                duration,
                body_file,
                recorded,
                meet_and_confer,
                agreement,
                confirms,
                acknowledges,
                source_ref,
                client_visible,
                subject,
                dry_run,
                project,
                host,
            } => {
                projects::communication::create(
                    host.host.as_deref(),
                    project.as_deref(),
                    projects::communication::NewCommunication {
                        channel,
                        direction,
                        counterparty,
                        role,
                        at,
                        duration,
                        body_file,
                        recorded,
                        meet_and_confer,
                        agreements: agreement,
                        confirms,
                        acknowledges,
                        source_ref,
                        client_visible,
                        subject,
                    },
                    dry_run,
                )
                .await
            }
            CommunicationCmd::Note {
                body_file,
                subject,
                dry_run,
                project,
                host,
            } => {
                projects::communication::note(
                    host.host.as_deref(),
                    project.as_deref(),
                    projects::communication::NewNote { body_file, subject },
                    dry_run,
                )
                .await
            }
        },
        ProjectsCmd::Service { action } => match action {
            ServiceCmd::List { host, project } => {
                projects::service::list(host.host.as_deref(), project.as_deref()).await
            }
            ServiceCmd::Show { host, project, id } => {
                projects::service::show(host.host.as_deref(), project.as_deref(), id).await
            }
            ServiceCmd::Add {
                document,
                slug,
                recipient,
                recipient_person,
                recipient_entity,
                counsel_for,
                method,
                served_on,
                address,
                declarant,
                declarant_person,
                letter,
                sent_email,
                dry_run,
                project,
                host,
            } => {
                projects::service::add(
                    host.host.as_deref(),
                    project.as_deref(),
                    projects::service::NewService {
                        document,
                        slug,
                        recipient,
                        recipient_person,
                        recipient_entity,
                        counsel_for,
                        method,
                        served_on,
                        address,
                        declarant,
                        declarant_person,
                        letter,
                        sent_email,
                    },
                    dry_run,
                )
                .await
            }
        },
        ProjectsCmd::Deadline { action } => match action {
            DeadlineCmd::Propose {
                args,
                host,
                project,
            } => {
                projects::deadline_proposal::propose(host.host.as_deref(), project.as_deref(), args)
                    .await
            }
            DeadlineCmd::List { host, project } => {
                projects::deadline::list(host.host.as_deref(), project.as_deref()).await
            }
            DeadlineCmd::Create {
                host,
                project,
                title,
                description,
                visibility,
                ics,
                location,
                due_at,
                authority_kind,
                authority,
                source,
                source_document,
                owner,
                calendar_role,
                category,
                internal_lead_days,
                client_lead_days,
                replay_key,
            } => {
                projects::deadline::create(
                    host.host.as_deref(),
                    project.as_deref(),
                    Some(title),
                    description,
                    visibility,
                    due_at,
                    authority_kind,
                    authority,
                    source,
                    source_document,
                    owner,
                    calendar_role,
                    category,
                    internal_lead_days,
                    client_lead_days,
                    replay_key,
                    ics.as_deref(),
                    location.as_deref(),
                )
                .await
            }
            DeadlineCmd::Update {
                host,
                project,
                id,
                title,
                description,
                visibility,
                ics,
                location,
                due_at,
                authority_kind,
                authority,
                source,
                source_document,
                owner,
                calendar_role,
                category,
                internal_lead_days,
                client_lead_days,
            } => {
                projects::deadline::update(
                    host.host.as_deref(),
                    project.as_deref(),
                    id,
                    title,
                    description,
                    visibility,
                    due_at,
                    authority_kind,
                    authority,
                    source,
                    source_document,
                    owner,
                    calendar_role,
                    category,
                    internal_lead_days,
                    client_lead_days,
                    ics.as_deref(),
                    location.as_deref(),
                )
                .await
            }
            DeadlineCmd::Complete { host, project, id } => {
                projects::deadline::complete(host.host.as_deref(), project.as_deref(), id).await
            }
            DeadlineCmd::Cancel { host, project, id } => {
                projects::deadline::cancel(host.host.as_deref(), project.as_deref(), id).await
            }
            DeadlineCmd::Ics { host, project } => {
                projects::deadline::ics(host.host.as_deref(), project.as_deref()).await
            }
            DeadlineCmd::Sync {
                host,
                project,
                dry_run,
            } => projects::deadline::sync(host.host.as_deref(), project.as_deref(), dry_run).await,
            DeadlineCmd::Reopen { host, project, id } => {
                projects::deadline::reopen(host.host.as_deref(), project.as_deref(), id).await
            }
        },
        ProjectsCmd::Efiling { action } => match action {
            EfilingCmd::List { host, project } => {
                projects::efiling::list(host.host.as_deref(), project.as_deref()).await
            }
            EfilingCmd::Record {
                email,
                details,
                project,
                host,
            } => {
                projects::efiling::record(
                    host.host.as_deref(),
                    project.as_deref(),
                    &email,
                    details.into_overrides(),
                )
                .await
            }
            EfilingCmd::Pair {
                file,
                transaction,
                accepted_on,
                details,
                project,
                host,
            } => {
                projects::efiling::pair(
                    host.host.as_deref(),
                    project.as_deref(),
                    &file,
                    &transaction,
                    accepted_on,
                    details.into_overrides(),
                )
                .await
            }
        },
        ProjectsCmd::Reference { action } => match action {
            ReferenceCmd::Add {
                reason,
                party,
                project,
                host,
            } => {
                projects::reference::add_reference(
                    host.host.as_deref(),
                    project.as_deref(),
                    &reason,
                    party.into_args(),
                )
                .await
            }
        },
        ProjectsCmd::LegalPlan { action } => match action {
            LegalPlanCmd::Show { project, host } => {
                legal_plans::show(host.host.as_deref(), project.as_deref()).await
            }
            LegalPlanCmd::Set {
                fields,
                project,
                host,
            } => legal_plans::set(host.host.as_deref(), project.as_deref(), &fields).await,
        },
        ProjectsCmd::Adverse { action } => match action {
            AdverseCmd::Add {
                party,
                subject,
                project,
                host,
            } => {
                projects::reference::add_adversity(
                    host.host.as_deref(),
                    project.as_deref(),
                    party.into_args(),
                    &subject,
                )
                .await
            }
        },
        ProjectsCmd::Participant { action } => match action {
            ParticipantCmd::List { host, project } => {
                projects::participant::list(host.host.as_deref(), project.as_deref()).await
            }
            ParticipantCmd::Dri {
                host,
                project,
                participation_id,
                clear,
            } => {
                projects::participant::dri(
                    host.host.as_deref(),
                    project.as_deref(),
                    participation_id,
                    clear,
                )
                .await
            }
        },
        ProjectsCmd::Status {
            host,
            project_code,
            json,
        } => projects::status::run(host.host.as_deref(), project_code.as_deref(), json).await,
        ProjectsCmd::Lifecycle {
            host,
            project_code,
            json,
        } => {
            projects::lifecycle_read::run(host.host.as_deref(), project_code.as_deref(), json).await
        }
        ProjectsCmd::Finance { action } => match action {
            FinanceCmd::Run { project_code, host } => {
                projects::finance::run(host.host.as_deref(), project_code.as_deref()).await
            }
            FinanceCmd::Show {
                project_code,
                json,
                host,
            } => projects::finance::show(host.host.as_deref(), project_code.as_deref(), json).await,
        },
        ProjectsCmd::Onboard {
            host,
            answers_from,
            delivery,
        } => {
            projects::lifecycle::onboard(
                host.host.as_deref(),
                Path::new("."),
                answers_from.as_deref(),
                delivery.as_deref(),
            )
            .await
        }
        ProjectsCmd::Offboard { effective_at, host } => {
            projects::lifecycle::offboard(host.host.as_deref(), Path::new("."), effective_at).await
        }
        ProjectsCmd::MigrateLetter {} => {
            match projects::lifecycle::migrate_letter(Path::new(".")) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("navigator project migrate-letter: {error:#}");
                    ExitCode::FAILURE
                }
            }
        }
        ProjectsCmd::Set {
            project_code,
            from_live,
            lawyer_dri,
            client_dri,
            private_slack_channel,
            private_notion_page,
            calendar,
            booking_url,
            jurisdiction,
            drive_folder,
            dry_run,
            firm,
            brand,
            host,
        } => {
            projects::set::run(projects::set::SetRequest {
                host_override: host.host.as_deref(),
                project_override: project_code.as_deref(),
                from_live,
                supplied: [
                    lawyer_dri.as_deref(),
                    client_dri.as_deref(),
                    private_slack_channel.as_deref(),
                    private_notion_page.as_deref(),
                ],
                live_fields: projects::set::LiveFields {
                    calendar_id: calendar.as_deref(),
                    drive_folder_id: drive_folder.as_deref(),
                    booking_url: booking_url.as_deref(),
                    jurisdiction: jurisdiction.as_deref(),
                },
                firm: firm.as_deref(),
                brand: brand.as_deref(),
                dry_run,
            })
            .await
        }
        ProjectsCmd::Watch { dir, host } => projects::watch::run(&dir, host.host.as_deref()).await,
        ProjectsCmd::Sync {
            dry_run,
            yes,
            yes_delete,
            only,
        } => {
            document_sync::run_project_sync(
                std::path::Path::new("."),
                dry_run,
                yes,
                yes_delete,
                only.as_deref(),
            )
            .await
        }
        ProjectsCmd::Notations { stale, json } => run_project_notations(&stale, json).await,
        ProjectsCmd::Create {
            name,
            code,
            client_email,
            entity_name,
            jurisdiction,
            attest,
            closed,
            closed_at,
            firm,
            brand,
            host,
        } => {
            remote::projects_create_named(
                host.host.as_deref(),
                &name,
                &code,
                "",
                &client_email,
                entity_name.as_deref(),
                jurisdiction.as_deref(),
                attest,
                closed.then_some(closed_at).flatten(),
                firm.as_deref(),
                brand.as_deref(),
            )
            .await
        }
        ProjectsCmd::ImportSource {
            project_code,
            dir,
            operation_key,
            wait,
            host,
        } => {
            projects::source_import::run(
                host.host.as_deref(),
                &project_code,
                &dir,
                operation_key.as_deref(),
                wait,
            )
            .await
        }
        ProjectsCmd::DriveIngest {
            project_code,
            dry_run,
            host,
        } => projects::drive_ingest::run(host.host.as_deref(), &project_code, dry_run).await,
        ProjectsCmd::Scaffold {
            dir,
            resume,
            no_repository,
            firm,
            brand,
            client_email,
            host,
        } => {
            projects::scaffold::run(
                host.host.as_deref(),
                &dir,
                resume.as_deref(),
                projects::scaffold::ScaffoldOptions {
                    provision_repository: !no_repository,
                    client_email,
                    firm,
                    brand,
                },
            )
            .await
        }
        ProjectsCmd::Close {
            project_code,
            reason,
            effective_at,
            dir,
            host,
        } => {
            let closed = remote::matter_close(
                host.host.as_deref(),
                &project_code,
                reason.into_reason(),
                effective_at,
            )
            .await;
            if closed != ExitCode::SUCCESS {
                return closed;
            }
            remote::archive_repository(host.host.as_deref(), &project_code, &dir).await
        }
        ProjectsCmd::Reopen {
            project_code,
            reason,
            effective_at,
            host,
        } => {
            let ReopenReasonArg::NewMatterEvent = reason;
            remote::matter_reopen(
                host.host.as_deref(),
                &project_code,
                model::projects::ReopenReason::NewMatterEvent,
                effective_at,
            )
            .await
        }
        ProjectsCmd::Doctor { host, project } => {
            projects::doctor::run(host.host.as_deref(), project.as_deref())
        }
        ProjectsCmd::Drift {
            host,
            dir,
            all,
            json,
        } => projects::drift::run(host.host.as_deref(), &dir, all, json).await,
        ProjectsCmd::Gate {
            tracked_files: true,
            ..
        } => projects::pinned_gate::dispatch(GateMode::TrackedFiles).await,
        ProjectsCmd::Gate {
            ci, check, deep, ..
        } => projects::pinned_gate::dispatch(GateMode::Content { ci, check, deep }).await,
        ProjectsCmd::NotifyMainPush { gate, ci: _ } => {
            match projects::main_push::notify(std::path::Path::new("."), gate).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("navigator: main-push notice not sent: {error:#}");
                    ExitCode::from(1)
                }
            }
        }
        ProjectsCmd::Surfaces { action } => match action {
            SurfacesCmd::Reconcile {
                project,
                json,
                host,
            } => {
                projects::surfaces_reconcile::run(host.host.as_deref(), project.as_deref(), json)
                    .await
            }
        },
        ProjectsCmd::Pages { action } => projects::pages_init::run(action).await,
        ProjectsCmd::Setup {
            project_code,
            all,
            json,
            host,
        } => projects::setup::run(host.host.as_deref(), project_code.as_deref(), all, json).await,
        ProjectsCmd::Skill { action } => run_project_skill(action).await,
        ProjectsCmd::Jj { action } => match action {
            ProjectJjCmd::Log { dir } => match projects::jj_log::run(&dir) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("navigator: {error:#}");
                    ExitCode::from(2)
                }
            },
        },
        ProjectsCmd::Repository { action } => match action {
            ProjectRepositoryCmd::SyncSkills { dir } => {
                match projects::repository::repair_skills(&dir) {
                    Ok(repaired) => {
                        if repaired.is_empty() {
                            println!(
                                "Canonical Agent Skills and AGENTS.md already match in {}",
                                dir.display()
                            );
                        } else {
                            for relative in &repaired {
                                println!("repaired {relative}");
                            }
                        }
                        ExitCode::SUCCESS
                    }
                    Err(error) => {
                        eprintln!("navigator: {error:#}");
                        ExitCode::from(2)
                    }
                }
            }
            ProjectRepositoryCmd::Jj { dir } => match projects::jj::run(&dir) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("navigator: {error:#}");
                    ExitCode::from(2)
                }
            },
            ProjectRepositoryCmd::MigrateDocuments { dir, dry_run } => {
                match document_layout::migrate_checkout(&dir, dry_run) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("navigator: {error:#}");
                        ExitCode::from(2)
                    }
                }
            }
            ProjectRepositoryCmd::Push {
                dir,
                base,
                branch,
                title,
                body,
                dry_run,
            } => {
                match projects::push::dispatch(
                    &dir,
                    &base,
                    branch.as_deref(),
                    &title,
                    body.as_deref(),
                    dry_run,
                )
                .await
                {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("navigator: {error:#}");
                        ExitCode::from(2)
                    }
                }
            }
            ProjectRepositoryCmd::MigrateFolders { dir, dry_run } => {
                match document_layout::migrate_folder_tree(&dir, dry_run) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("navigator: {error:#}");
                        ExitCode::from(2)
                    }
                }
            }
        },
        ProjectsCmd::Documents { action } => match action {
            DocumentsCmd::Check {
                dir,
                documents_dir,
                drive_local,
                deep,
                json,
            } => run_documents_check(
                dir.as_deref().unwrap_or(Path::new(".")),
                documents_dir,
                drive_local,
                deep,
                json,
            ),
        },
        ProjectsCmd::Search {
            pattern,
            dir,
            ignore_case,
            context,
            json,
            record,
        } => {
            if record.as_deref().is_some_and(|value| value != "court") {
                eprintln!("navigator: project search: --record only accepts `court`");
                ExitCode::from(2)
            } else {
                projects::search::run(
                    &pattern,
                    &dir,
                    ignore_case,
                    context,
                    json,
                    record.as_deref() == Some("court"),
                )
                .await
            }
        }
    }
}

pub async fn run_project_notations(stale: &str, json: bool) -> ExitCode {
    let Some(manifest) = projects::manifest::read(Path::new(".")) else {
        eprintln!("navigator project notations: no navigator.yaml in the current directory");
        return ExitCode::from(2);
    };
    let (Some(host), Some(project_code)) = (manifest.host.as_deref(), manifest.project.as_deref())
    else {
        eprintln!("navigator project notations: navigator.yaml must declare host and project.name");
        return ExitCode::from(2);
    };
    let Some(stale_after) = parse_stale_duration(stale) else {
        eprintln!(
            "navigator project notations: invalid --stale duration (use e.g. 3d, 12h, or 30m)"
        );
        return ExitCode::from(2);
    };
    remote::project_notations(Some(host), project_code, stale_after, json).await
}

#[must_use]
pub fn parse_stale_duration(input: &str) -> Option<chrono::Duration> {
    let (amount, unit) = input.split_at(input.find(|ch: char| !ch.is_ascii_digit())?);
    let amount: i64 = amount.parse().ok()?;
    if amount <= 0 {
        return None;
    }
    match unit {
        "d" => chrono::Duration::try_days(amount),
        "h" => chrono::Duration::try_hours(amount),
        "m" => chrono::Duration::try_minutes(amount),
        _ => None,
    }
}

/// `navigator project skill ...` — dispatches to `cli/src/projects/skill.rs`.
/// `list` and `show` read only the compiled-in catalog; `use` and `status`
/// additionally read (and `use` writes) `./navigator.yaml`, so all four run
/// from the current directory rather than taking a `--dir` flag — the same
/// current-directory convention `navigator project gate` uses. `use` then
/// writes the pinned Kit version through to the live matter when the
/// manifest names a deployment.
pub async fn run_project_skill(action: ProjectSkillCmd) -> ExitCode {
    match action {
        ProjectSkillCmd::List => projects::skill::run_list(),
        ProjectSkillCmd::Show {
            jurisdiction,
            practice_area,
        } => projects::skill::run_show(&jurisdiction, &practice_area),
        ProjectSkillCmd::Use {
            jurisdiction,
            practice_area,
        } => {
            let code = projects::skill::run_use(Path::new("."), &jurisdiction, &practice_area);
            if code == ExitCode::SUCCESS {
                projects::skill::record_kit_version(Path::new("."), &jurisdiction, &practice_area)
                    .await;
            }
            code
        }
        ProjectSkillCmd::Status => projects::skill::run_status(Path::new(".")),
        ProjectSkillCmd::Feedback {
            file,
            jurisdiction,
            practice_area,
        } => projects::skill_feedback::run(file, jurisdiction, practice_area).await,
    }
}

pub fn is_yaml_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml"))
}

#[must_use]
pub fn include_authored_tree_entry(entry: &walkdir::DirEntry) -> bool {
    let name = entry.file_name().to_string_lossy();
    !entry.file_type().is_dir()
        || !matches!(
            name.as_ref(),
            ".git" | "target" | ".worktrees" | "node_modules" | "dist"
        )
}

/// Parse every `.yaml`/`.yml` file under `dir` as part of `validate`. Prints
/// one line per parse error plus a `Parsed N …` summary, and returns the
/// errors so the run can recapitulate them. Standalone YAML (k8s manifests,
/// config, reference catalogs) is disjoint from the markdown the classified
/// engine lints, so this is a second pass over the same tree rather than a
/// second command.
pub fn yaml_pass(dir: &std::path::Path) -> std::io::Result<Vec<GateError>> {
    let mut files_scanned = 0usize;
    let mut errors: Vec<GateError> = Vec::new();
    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_entry(include_authored_tree_entry)
    {
        let entry = entry.map_err(std::io::Error::other)?;
        if !entry.file_type().is_file() || !is_yaml_path(entry.path()) {
            continue;
        }
        files_scanned += 1;
        let raw = std::fs::read_to_string(entry.path())?;
        for document in serde_yaml::Deserializer::from_str(&raw) {
            if let Err(err) = serde_yaml::Value::deserialize(document) {
                let location = match err.location() {
                    Some(location) => format!(
                        "{}:{}:{}",
                        entry.path().display(),
                        location.line(),
                        location.column()
                    ),
                    None => entry.path().display().to_string(),
                };
                eprintln!("{location}: YAML parse error: {err}");
                errors.push(GateError::new(
                    location,
                    None,
                    format!("YAML parse error: {err}"),
                ));
                break;
            }
        }
    }
    println!(
        "Parsed {files_scanned} YAML file(s), found {} error(s)",
        errors.len()
    );
    Ok(errors)
}

/// `Y001` — a `seeds/*.yaml` document must be accepted by `navigator site import`.
pub const SEED_DOCUMENT_CODE: &str = "Y001";

/// `Y002` — a `locales/<locale>/[<brand-key>/]<page>.yaml` catalog must deserialize as that page.
pub const LOCALE_DOCUMENT_CODE: &str = "Y002";

/// `Y003` — a `blobs/**/*.yaml` pointer in a Project repository must name a valid asset revision.
pub const DOCUMENT_POINTER_CODE: &str = "Y003";

#[must_use]
pub fn document_pointer_path(
    root: &std::path::Path,
    path: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let relative = path.strip_prefix(root).ok()?;
    let mut components = relative.components();
    // Both spellings, via the one contract in `document_sync`. Matching only
    // the retired `.yml` here would skip every pointer in a renamed
    // repository, and `Y003` would report a clean pass over nothing — the
    // silent-pass failure the shared constant exists to prevent (LAW-25).
    (components.next()?.as_os_str() == crate::document_sync::POINTER_DIRECTORY
        && crate::document_sync::is_pointer_path(path))
    .then(|| relative.to_path_buf())
}

/// Validate committed document pointers only when the walked root declares a
/// Project. An unrelated tool may have its own `blobs/` YAML tree.
pub fn document_pointer_pass(dir: &std::path::Path) -> std::io::Result<Vec<GateError>> {
    let mut errors = Vec::new();
    let mut files_scanned = 0usize;
    if is_project_repository(dir) {
        for entry in walkdir::WalkDir::new(dir.join(crate::document_sync::POINTER_DIRECTORY))
            .into_iter()
            .filter_entry(|entry| entry.file_name() != ".git")
        {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error)
                    if error
                        .io_error()
                        .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
                {
                    break;
                }
                Err(error) => return Err(std::io::Error::other(error)),
            };
            let path = entry.path();
            if !entry.file_type().is_file() || document_pointer_path(dir, path).is_none() {
                continue;
            }
            files_scanned += 1;
            let raw = std::fs::read_to_string(path)?;
            let validation =
                model::document_pointers::DocumentPointer::from_yaml(&raw).and_then(|pointer| {
                    let without_yml = path.with_extension("");
                    let has_document_extension = without_yml
                        .extension()
                        .and_then(std::ffi::OsStr::to_str)
                        .is_some_and(|extension| !extension.is_empty());
                    if !has_document_extension {
                        anyhow::bail!(
                            "pointer filename must retain the document extension before its `.yaml` pointer suffix"
                        )
                    }
                    // ENG-859: the folder-keyed pointer-key rules
                    // (`onboarding`/`offboarding`/`invoices`) need the
                    // document's slug, which `without_yml` names below
                    // `blobs/` once its own `.yaml`/`.yml` suffix is stripped.
                    if let Ok(source_relative) =
                        without_yml.strip_prefix(dir.join(crate::document_sync::POINTER_DIRECTORY))
                    {
                        crate::document_sync::validate_folder_pointer_keys(
                            source_relative,
                            &pointer,
                        )?;
                    }
                    Ok(pointer)
                });
            if let Err(error) = validation {
                print_violation(
                    &path.display().to_string(),
                    1,
                    DOCUMENT_POINTER_CODE,
                    &error.to_string(),
                );
                errors.push(GateError::new(
                    format!("{}:1", path.display()),
                    Some(DOCUMENT_POINTER_CODE),
                    error.to_string(),
                ));
            }
        }
    }
    println!(
        "Validated {files_scanned} document pointer(s), found {} error(s)",
        errors.len()
    );
    Ok(errors)
}

pub fn seed_model_for_path(
    path: &std::path::Path,
) -> Option<anyhow::Result<model::seed::SeedModel>> {
    let parent = path.parent()?;
    if parent.file_name()? != "seeds" {
        return None;
    }
    let model = path.file_stem()?.to_str()?;
    let parsed = model::seed::SeedModel::parse(model);
    let is_canonical_catalog = parent
        .parent()
        .and_then(std::path::Path::file_name)
        .is_some_and(|name| name == "store");
    (!is_canonical_catalog || parsed.is_ok()).then_some(parsed)
}

/// Validate the direct `seeds/*.yaml` documents that an operator can submit
/// through `navigator site import`. The store owns the parser; this pass only
/// discovers the files and reports its refusal with the validation lint code.
pub fn seed_document_pass(dir: &std::path::Path) -> std::io::Result<Vec<GateError>> {
    let mut files_scanned = 0usize;
    let mut errors: Vec<GateError> = Vec::new();
    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_entry(include_authored_tree_entry)
    {
        let entry = entry.map_err(std::io::Error::other)?;
        let path = entry.path();
        if !entry.file_type().is_file() || !is_yaml_path(path) {
            continue;
        }
        let Some(model) = seed_model_for_path(path) else {
            continue;
        };
        files_scanned += 1;
        let raw = std::fs::read_to_string(path)?;
        let validation = model.and_then(|model| model::seed::validate_yaml(model, &raw));
        if let Err(error) = validation {
            print_violation(
                &path.display().to_string(),
                1,
                SEED_DOCUMENT_CODE,
                &error.to_string(),
            );
            errors.push(GateError::new(
                format!("{}:1", path.display()),
                Some(SEED_DOCUMENT_CODE),
                error.to_string(),
            ));
        }
    }
    println!(
        "Validated {files_scanned} seed document(s), found {} error(s)",
        errors.len()
    );
    Ok(errors)
}

/// Validate every brand locale catalog: `locales/<locale>/<page>.yaml`.
///
/// The site publishes English only. An unknown page stem or a locale directory
/// other than [`views::locales::DEFAULT_LOCALE`] is an error, and a known page
/// must deserialize as its typed catalog so a copy-only edit cannot land a
/// document the brand crate cannot load.
pub fn locale_document_pass(dir: &std::path::Path) -> std::io::Result<Vec<GateError>> {
    let mut files_scanned = 0usize;
    let mut errors: Vec<GateError> = Vec::new();
    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_entry(include_authored_tree_entry)
    {
        let entry = entry.map_err(std::io::Error::other)?;
        let path = entry.path();
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(parts) = views::locales::locale_yaml_parts(path) else {
            continue;
        };
        files_scanned += 1;
        if parts.locale != views::locales::DEFAULT_LOCALE {
            let message = format!(
                "locale directory `{}` is not published; only `{}` is allowed",
                parts.locale,
                views::locales::DEFAULT_LOCALE
            );
            print_violation(
                &path.display().to_string(),
                1,
                LOCALE_DOCUMENT_CODE,
                &message,
            );
            errors.push(GateError::new(
                format!("{}:1", path.display()),
                Some(LOCALE_DOCUMENT_CODE),
                message,
            ));
            continue;
        }
        if let Some(brand_key) = parts.brand_key {
            let known = views::brand::BrandKey::ALL
                .iter()
                .any(|key| key.as_str() == brand_key);
            if !known {
                let message = format!(
                    "brand catalog directory `{brand_key}` is not a registry key; expected one of {}",
                    views::brand::BrandKey::ALL
                        .iter()
                        .map(|key| key.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                print_violation(
                    &path.display().to_string(),
                    1,
                    LOCALE_DOCUMENT_CODE,
                    &message,
                );
                errors.push(GateError::new(
                    format!("{}:1", path.display()),
                    Some(LOCALE_DOCUMENT_CODE),
                    message,
                ));
                continue;
            }
        }
        let raw = std::fs::read_to_string(path)?;
        if let Err(error) = views::locales::parse_locale_file(parts.stem, &raw) {
            print_violation(&path.display().to_string(), 1, LOCALE_DOCUMENT_CODE, &error);
            errors.push(GateError::new(
                format!("{}:1", path.display()),
                Some(LOCALE_DOCUMENT_CODE),
                error,
            ));
        }
    }
    println!(
        "Validated {files_scanned} locale catalog(s), found {} error(s)",
        errors.len()
    );
    Ok(errors)
}

/// True for a Containerfile/Dockerfile by filename, whose `FROM` lines this
/// guard scans the same way it scans YAML `image:` values.
pub fn is_containerfile_path(path: &std::path::Path) -> bool {
    path.file_name()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|name| name.starts_with("Containerfile") || name.starts_with("Dockerfile"))
}

/// Given an image reference (the value after `image:` or `FROM`), return the
/// offending tag when it is a *mutable* tag we must never consume, else `None`.
///
/// Mutable means the `latest` family: an explicit `:latest`, a `:latest-<arch>`
/// variant, or — when `flag_implicit` — an implicit latest (no tag at all). A
/// reference pinned by digest (`@sha256:…`) or to any explicit version tag
/// (`:1.18.2`, `:v1.32.6`, `:16-alpine`, our own `:dev`/`:YY.M.D` build tags)
/// is fine. `flag_implicit` is off for workflow files, where a bare, untagged
/// `- image: navigator-web` names a build-matrix target we *publish*, not a
/// container we *run*; on-cluster manifests and Containerfiles always carry a
/// real reference, so an untagged one there is a genuine implicit-latest bug.
#[must_use]
pub fn mutable_image_tag(reference: &str, flag_implicit: bool) -> Option<String> {
    let reference = reference.trim();
    if reference.is_empty() {
        return None;
    }
    // A digest pin is immutable regardless of any tag that precedes it.
    if reference.contains("@sha256:") {
        return None;
    }
    // The tag is the segment after the last `:` that falls after the last `/`
    // — anything earlier is a registry-host port (`registry:5000/img`), not a
    // tag. No such `:` means the reference is untagged, i.e. implicit `latest`.
    let last_slash = reference.rfind('/');
    let tag = match reference.rfind(':') {
        Some(colon) if last_slash.is_none_or(|slash| colon > slash) => &reference[colon + 1..],
        _ => {
            return flag_implicit.then(|| "latest (implicit — no tag)".to_string());
        }
    };
    if tag == "latest" || tag.starts_with("latest-") {
        Some(tag.to_string())
    } else {
        None
    }
}

/// Strip a trailing ` # …` YAML/Dockerfile comment from a line, returning the
/// code portion and whether the comment marked the line pin-exempt.
#[must_use]
pub fn split_trailing_comment(line: &str) -> (&str, bool) {
    match line.find(" #") {
        Some(idx) => (&line[..idx], line[idx..].contains("pin-exempt")),
        None => (line, false),
    }
}

/// If `trimmed` is a YAML mapping entry whose key is exactly `key`, return its
/// value. Tolerates YAML's optional whitespace before the colon (`image : x`)
/// and rejects a lookalike key that merely starts with it (`imagePullPolicy:`
/// for `image`), so a consumed tag cannot slip through on formatting alone.
#[must_use]
pub fn yaml_value_for_key<'a>(trimmed: &'a str, key: &str) -> Option<&'a str> {
    let rest = trimmed.strip_prefix(key)?.trim_start();
    Some(rest.strip_prefix(':')?.trim())
}

/// Extract the image reference from a YAML `image:` entry, returning
/// `(reference, is_list_item)`. A list item (`- image: …`) is a build-matrix
/// publish target in a workflow; a plain `image:` is a runtime `container:` /
/// `services:` reference we consume — the caller uses the flag to decide
/// whether implicit-latest counts.
#[must_use]
pub fn yaml_image_entry(trimmed: &str) -> Option<(&str, bool)> {
    match trimmed.strip_prefix("- ") {
        Some(rest) => yaml_value_for_key(rest.trim_start(), "image").map(|v| (v, true)),
        None => yaml_value_for_key(trimmed, "image").map(|v| (v, false)),
    }
}

/// Extract the image reference from a Containerfile `FROM` line. The `FROM`
/// keyword is case-insensitive (`From debian:latest` is valid), and a stage may
/// carry `--platform=…` flags and a trailing `AS name` — the reference is the
/// first non-flag token.
pub fn containerfile_from_ref(trimmed: &str) -> Option<&str> {
    let (keyword, rest) = trimmed.split_once(char::is_whitespace)?;
    keyword.eq_ignore_ascii_case("FROM").then_some(rest)
}

/// Scan one file's contents for a *consumed* mutable tag, returning
/// `(line_number, message)` for each offending line. Covers three consume
/// sites: a YAML `image:` value, a Containerfile `FROM` reference, and a
/// GitHub Actions installer step's `version: latest`. A line carrying a
/// `# pin-exempt: <reason>` comment is skipped, the documented escape hatch
/// for the rare intentional case (e.g. a publish-only construct).
#[must_use]
pub fn detect_mutable_tags(path: &std::path::Path, contents: &str) -> Vec<(usize, String)> {
    let is_yaml = is_yaml_path(path);
    let is_containerfile = is_containerfile_path(path);
    // GitHub Actions workflows are the only place a `version: latest`
    // installer step is meaningful; a `version:` key elsewhere (a Helm chart
    // pin, an API version) is unrelated.
    let is_workflow = is_yaml
        && path.components().any(|c| c.as_os_str() == "workflows")
        && path.to_string_lossy().contains(".github");
    let mut findings = Vec::new();
    for (idx, raw) in contents.lines().enumerate() {
        let (code, exempt) = split_trailing_comment(raw);
        if exempt {
            continue;
        }
        let trimmed = code.trim();
        let reference = if is_yaml {
            yaml_image_entry(trimmed)
        } else if is_containerfile {
            containerfile_from_ref(trimmed).map(|r| (r, false))
        } else {
            None
        };
        if let Some((rest, is_list)) = reference {
            let refstr = rest
                .split_whitespace()
                .find(|tok| !tok.starts_with("--"))
                .unwrap_or("")
                .trim_matches(|c| c == '"' || c == '\'');
            // Implicit latest (an untagged reference) is exempt only for a
            // workflow build-matrix list item (`- image: navigator-web`), a
            // target we publish. A workflow `container:` / `services:` image,
            // and every on-cluster / Containerfile reference, is consumed —
            // an untagged one there is a genuine bug. Explicit `:latest` is
            // always caught regardless.
            let flag_implicit = !(is_workflow && is_list);
            if let Some(tag) = mutable_image_tag(refstr, flag_implicit) {
                findings.push((
                    idx + 1,
                    format!(
                        "consumed mutable image tag `{tag}` in `{refstr}` — pin an explicit \
                         version (docs/gitops.md § \"Pin every consumed image, binary, and action\")"
                    ),
                ));
            }
        } else if is_workflow {
            if let Some(value) = yaml_value_for_key(trimmed, "version") {
                let value = value.trim_matches(|c| c == '"' || c == '\'');
                if value.eq_ignore_ascii_case("latest") {
                    findings.push((
                        idx + 1,
                        "consumed mutable binary version `latest` — pin an explicit version \
                         (docs/gitops.md § \"Pin every consumed image, binary, and action\")"
                            .to_string(),
                    ));
                }
            }
        }
    }
    findings
}

/// Walk `dir` for *consumed* mutable tags — the diligence guard for
/// [navigator#540](https://github.com/neon-law-source-code/navigator/issues/540).
/// Prints one line per offence and returns them. Runs over YAML manifests,
/// Containerfiles, and workflow files alike; the `.git`, `target`, and
/// `.worktrees`, `node_modules`, and `dist` trees are skipped, as in
/// [`yaml_pass`].
pub fn mutable_tag_pass(dir: &std::path::Path) -> std::io::Result<Vec<GateError>> {
    let mut findings: Vec<GateError> = Vec::new();
    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_entry(include_authored_tree_entry)
    {
        let entry = entry.map_err(std::io::Error::other)?;
        let path = entry.path();
        if !entry.file_type().is_file() || !(is_yaml_path(path) || is_containerfile_path(path)) {
            continue;
        }
        let contents = std::fs::read_to_string(path)?;
        for (line, message) in detect_mutable_tags(path, &contents) {
            let location = format!("{}:{line}", path.display());
            eprintln!("{location}: {message}");
            findings.push(GateError::new(location, None, message));
        }
    }
    println!(
        "Checked consumed image/binary tags, found {} mutable tag(s)",
        findings.len()
    );
    Ok(findings)
}

/// The standalone passes `validate` runs over the raw tree after the markdown
/// lint — YAML syntax, seed-document shape, locale catalogs, and consumed
/// mutable tags. Every finding any of them reports fails the gate, so the four
/// lists concatenate into one.
pub fn standalone_tree_passes(dir: &std::path::Path) -> std::io::Result<Vec<GateError>> {
    let mut errors = yaml_pass(dir)?;
    errors.append(&mut seed_document_pass(dir)?);
    errors.append(&mut locale_document_pass(dir)?);
    errors.append(&mut document_pointer_pass(dir)?);
    errors.append(&mut document_filename_pass(dir)?);
    errors.extend(project_manifest_pass(dir));
    errors.append(&mut mutable_tag_pass(dir)?);
    errors.append(&mut snapshot_audit_pass(dir)?);
    Ok(errors)
}

/// `C004` (LAW-196) — every document file the checkout holds under
/// `documents/` keeps the `YYYY.MM.DD_name.ext` convention, with the same
/// shipped-folder exemptions the document walk applies. Name-only: a
/// checkout that never downloaded the bytes finds nothing here, and
/// `project gate --check` still holds the bytes it has to their pointers.
pub fn document_filename_pass(dir: &std::path::Path) -> std::io::Result<Vec<GateError>> {
    let mut errors = Vec::new();
    let mut files_scanned = 0usize;
    let documents = dir.join(crate::document_sync::DOCUMENT_DIRECTORY);
    if is_project_repository(dir) && documents.is_dir() {
        for entry in walkdir::WalkDir::new(&documents)
            .into_iter()
            .filter_entry(|entry| entry.file_name() != ".git")
        {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error)
                    if error
                        .io_error()
                        .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
                {
                    break;
                }
                Err(error) => return Err(std::io::Error::other(error)),
            };
            let path = entry.path();
            if !entry.file_type().is_file()
                || path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with('.'))
                || crate::document_sync::is_pointer_path(path)
            {
                continue;
            }
            let Ok(relative) = path.strip_prefix(&documents) else {
                continue;
            };
            // Bytes a pointer already records — a sibling legacy pointer,
            // or the `blobs/` pointer the blobs/ layout keeps for the same
            // document — are the materialization of a named document, not
            // a new file to name; the pointer pass holds that name.
            let pointer = |base: &std::path::Path| {
                let mut pointer = base.join(relative).into_os_string();
                pointer.push(format!(".{}", crate::document_sync::POINTER_EXTENSION));
                std::path::PathBuf::from(pointer)
            };
            if pointer(&documents).exists()
                || pointer(&dir.join(crate::document_sync::POINTER_DIRECTORY)).exists()
            {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            files_scanned += 1;
            if let Some(message) =
                projects::document_check::document_filename_finding(relative, path, file_name)
            {
                print_violation(&path.display().to_string(), 1, "C004", &message);
                errors.push(GateError::new(
                    path.display().to_string(),
                    Some("C004"),
                    message,
                ));
            }
        }
    }
    println!(
        "Validated {files_scanned} document filename(s), found {} error(s)",
        errors.len()
    );
    Ok(errors)
}

/// `P001` — when `dir` carries the public snapshot allowlist, every tracked
/// file is held to what a public source baseline may contain
/// (`crate::snapshot_audit`). An allowlist entry that permitted nothing prints
/// a warning so a resolved finding's entry is removed.
pub fn snapshot_audit_pass(dir: &std::path::Path) -> std::io::Result<Vec<GateError>> {
    let audit = match crate::snapshot_audit::audit_tree(dir) {
        Ok(Some(audit)) => audit,
        Ok(None) => return Ok(Vec::new()),
        Err(error) => return Err(std::io::Error::other(error)),
    };
    let code = crate::snapshot_audit::CODE;
    let mut errors = Vec::with_capacity(audit.findings.len());
    for finding in &audit.findings {
        let message = finding.message();
        print_violation(&finding.path, finding.line, code, &message);
        errors.push(GateError::new(
            format!("{}:{}", finding.path, finding.line),
            Some(code),
            message,
        ));
    }
    for entry in &audit.unused_entries {
        println!(
            "{}",
            diagnostic_line(
                rules::Severity::Warning,
                crate::snapshot_audit::ALLOWLIST,
                Some(code),
                &format!("allowlist entry permits nothing; remove it: {entry}"),
            )
        );
    }
    println!(
        "Audited {} tracked file(s) for the public snapshot, found {} error(s)",
        audit.files_scanned,
        errors.len()
    );
    Ok(errors)
}

/// `Y004`–`Y008` and `Y011`–`Y016` — a Project repository's `navigator.yaml` (or the
/// retired `.yml` spelling) holds to the closed key set and value shapes.
#[must_use]
pub fn project_manifest_pass(dir: &std::path::Path) -> Vec<GateError> {
    let findings = crate::projects::manifest::lint(dir);
    let mut errors = Vec::with_capacity(findings.len());
    for finding in findings {
        let location = format!("{}:{}", finding.path.display(), finding.line);
        if finding.warning {
            println!(
                "{}:{}: warning: {}: {}",
                finding.path.display(),
                finding.line,
                finding.code,
                finding.message
            );
        } else {
            print_violation(
                &finding.path.display().to_string(),
                finding.line,
                finding.code,
                &finding.message,
            );
            errors.push(GateError::new(
                location,
                Some(finding.code),
                finding.message,
            ));
        }
    }
    if dir.join(crate::projects::manifest::FILE).is_file()
        || dir.join(crate::projects::manifest::RETIRED_FILE).is_file()
    {
        println!(
            "Validated project manifest, found {} error(s)",
            errors.len()
        );
    }
    errors
}

/// The repository root the gate runs at: the directory holding both a `README`
/// and a `.git`. The gate takes no path argument, because one that could be
/// pointed at a subdirectory reports a clean tree over the files it never read.
pub fn gate_root() -> Result<PathBuf, String> {
    let root =
        std::env::current_dir().map_err(|error| format!("read current directory: {error}"))?;
    let readme = std::fs::read_dir(&root)
        .map_err(|error| format!("read {}: {error}", root.display()))?
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_string_lossy().starts_with("README"));
    // A worktree's `.git` is a file holding a pointer, not a directory, so this
    // asks whether the entry exists at all rather than what shape it is.
    let git = root.join(".git").exists();
    let missing = match (readme, git) {
        (true, true) => return Ok(root),
        (false, false) => "no README and no .git",
        (false, true) => "no README",
        _ => "no .git",
    };
    Err(format!(
        "{} has {missing} — the gate runs on a whole repository, so run it from the root",
        root.display()
    ))
}

/// `navigator project gate --tracked-files` — only `Y024`, over the Project
/// repository at the current root (ENG-1136, ENG-1139).
#[must_use]
pub fn run_tracked_files_gate() -> ExitCode {
    let root = match gate_root() {
        Ok(root) => root,
        Err(message) => {
            eprintln!("navigator: {message}");
            return ExitCode::from(2);
        }
    };
    if !projects::repository::is_project_repository(&root) {
        eprintln!(
            "navigator: --tracked-files checks a Project repository, and {} declares no `project` in `navigator.yaml`",
            root.display()
        );
        return ExitCode::from(2);
    }
    if projects::repository::validate_tracked_files_only(&root) == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// `navigator validate [DIR]` — the directory-scoped rule set.
///
/// Takes a path, defaults to `.`, and makes no assumption about the
/// surrounding repository. `--fix` writes every safe-by-construction
/// edit; `--errors-only` hides Warning-severity advisories.
pub fn run_validate(dir: &std::path::Path, fix: bool, errors_only: bool) -> ExitCode {
    let offline = if fix {
        run_validate_fix(dir)
    } else {
        run_validate_scan(dir, errors_only)
    };
    if !is_project_repository(dir) {
        return offline;
    }
    let live = tokio::runtime::Runtime::new()
        .map_err(anyhow::Error::from)
        .and_then(|runtime| runtime.block_on(crate::document_read::validate_live_pointers(dir)));
    if let Err(error) = live {
        eprintln!("navigator: {error:#}");
        return ExitCode::from(1);
    }
    offline
}

#[must_use]
pub fn run_validate_fix(dir: &std::path::Path) -> ExitCode {
    let question_codes = rules::canonical_question_codes();
    let fix_report = match fix_directory(
        dir,
        &rules::DefaultFileFilter::default(),
        |file| rules::navigator_classified_rules_with_codes(file, &question_codes),
        true,
    ) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("navigator: {error}");
            return ExitCode::from(2);
        }
    };
    for path in &fix_report.fixed_files {
        println!("{}", palette::dim(format!("fixed {}", path.display())));
    }
    for violation in &fix_report.remaining {
        print_violation(
            &violation.path.display().to_string(),
            violation.line,
            violation.code,
            &violation.message,
        );
    }
    println!(
        "{}",
        palette::dim(format!(
            "Fixed {} file(s); {} remaining violation(s) need a human.",
            fix_report.fixed_files.len(),
            fix_report.remaining.len(),
        ))
    );
    if fix_report.remaining.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

#[must_use]
pub fn run_validate_scan(dir: &std::path::Path, errors_only: bool) -> ExitCode {
    let question_codes = rules::canonical_question_codes();
    let mut report = match rules::ClassifiedRuleEngine::new()
        .with_question_codes(question_codes)
        .lint_directory(dir)
    {
        Ok(report) => report,
        Err(error) => {
            eprintln!("navigator: {error}");
            return ExitCode::from(2);
        }
    };
    match rules::code_uniqueness_violations(dir, &rules::DefaultFileFilter::default()) {
        Ok(mut found) => report.violations.append(&mut found),
        Err(error) => {
            eprintln!("navigator: {error}");
            return ExitCode::from(2);
        }
    }
    match rules::service_template_violations(dir, &rules::DefaultFileFilter::default()) {
        Ok(mut found) => report.violations.append(&mut found),
        Err(error) => {
            eprintln!("navigator: {error}");
            return ExitCode::from(2);
        }
    }
    for violation in &report.violations {
        if errors_only && rules::severity_for_code(violation.code) != rules::Severity::Error {
            continue;
        }
        print_violation(
            &violation.path.display().to_string(),
            violation.line,
            violation.code,
            &violation.message,
        );
    }
    let (error_count, warning_count) = severity_counts(&report.violations);
    let mut gate_errors: Vec<GateError> = report
        .violations
        .iter()
        .filter(|violation| rules::severity_for_code(violation.code) == rules::Severity::Error)
        .map(|violation| {
            GateError::new(
                format!("{}:{}", violation.path.display(), violation.line),
                Some(violation.code),
                violation.message.clone(),
            )
        })
        .collect();

    println!(
        "{}",
        palette::dim(format!(
            "Scanned {} file(s), found {error_count} error(s), {warning_count} warning(s)",
            report.files_scanned,
        ))
    );

    match standalone_tree_passes(dir) {
        Ok(mut errors) => gate_errors.append(&mut errors),
        Err(error) => {
            eprintln!("navigator: {error}");
            return ExitCode::from(2);
        }
    }

    print_error_recap(&gate_errors);

    let project_layout_failed = is_project_repository(dir)
        && projects::repository::validate_gate(dir, None, false).errors > 0;

    if gate_errors.is_empty() && !project_layout_failed {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// `navigator project gate` — the one check over one repository.
///
/// Fixes what is safe to fix, reports what needs a human, and fails on either
/// an Error-severity finding or, under `--ci`, a file it had to fix. With
/// `--ci` it also asks the deployment whether `navigator.yaml` agrees with the
/// live row, which is the one question an offline pass cannot answer.
///
/// `--check` (LAW-62) runs only the live document check, comparing committed
/// pointers against what the deployment holds: it makes no offline pass over
/// the tree at all. That offline pass — content rules, YAML, seeds, layout,
/// and, once it has built, the origin pass reading `dist/` — is `verify`'s
/// job; running it again here from a checkout that `documents` never builds
/// is both redundant and, for the origin pass specifically, impossible.
pub async fn run_gate(ci: bool, check: bool, deep: bool) -> ExitCode {
    // ENG-951: refuse first, before any manifest, layout, or live check runs.
    if let Some(refusal) = projects::gate::ci_flag_refusal(ci, std::env::var("CI").ok().as_deref())
    {
        eprintln!("{refusal}");
        return ExitCode::from(2);
    }
    let root = match gate_root() {
        Ok(root) => root,
        Err(message) => {
            eprintln!("navigator: {message}");
            return ExitCode::from(2);
        }
    };
    let dir = root.as_path();

    if check {
        return run_document_check_gate(dir, ci, deep).await;
    }

    let ownership = projects::gate::ownership_preflight(dir, ci).await;
    if ownership != ExitCode::SUCCESS {
        return ownership;
    }

    eprintln!("navigator project gate: markdown content");
    let project = is_project_repository(dir);
    // The Project's documents directory holds committed YAML pointers and
    // Git-ignored local document bodies. Keep it out of Markdown lint/fix
    // walks; `document_pointer_pass` below validates the committed pointers
    // independently, and the repository layout check still rejects tracked
    // raw document bytes.
    let markdown_filter = ProjectGateMarkdownFilter::new(dir);

    let mut report = match fix_gate_markdown(dir, &markdown_filter, project, !ci) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("navigator: {error}");
            return ExitCode::from(2);
        }
    };
    let mut violations = std::mem::take(&mut report.remaining);

    eprintln!("navigator project gate: cross-file checks");
    // Cross-file `N111`: notation template filename stems must be unique across the
    // tree. Always run — only notation templates carry a `code`, so a
    // prose-only tree simply finds nothing.
    match rules::code_uniqueness_violations(dir, &markdown_filter) {
        Ok(mut found) => violations.append(&mut found),
        Err(error) => {
            eprintln!("navigator: {error}");
            return ExitCode::from(2);
        }
    }
    // Cross-file `N124`: every notation template named by a services catalog
    // must exist under `templates/notations/`.
    match rules::service_template_violations(dir, &markdown_filter) {
        Ok(mut found) => violations.append(&mut found),
        Err(error) => {
            eprintln!("navigator: {error}");
            return ExitCode::from(2);
        }
    }
    // Cross-file `N126`/`N127`: every `skills/` catalog entry must parse and
    // declare a unique `(jurisdiction, practice_area)` pair.
    match rules::project_skill_catalog_violations(dir, &markdown_filter) {
        Ok(mut found) => violations.append(&mut found),
        Err(error) => {
            eprintln!("navigator: {error}");
            return ExitCode::from(2);
        }
    }

    let mut gate_errors = report_content_findings(dir, ci, &report, &violations);

    eprintln!("navigator project gate: tree passes");
    // Standalone raw-tree passes over the same walk: YAML parse errors, seed
    // document shape, locale catalogs, and consumed mutable image/binary tags
    // (navigator#540).
    match standalone_tree_passes(dir) {
        Ok(mut errors) => gate_errors.append(&mut errors),
        Err(error) => {
            eprintln!("navigator: {error}");
            return ExitCode::from(2);
        }
    }

    // Under `--ci` on a pull request, the body's own closing trailer: a
    // body that closes an issue while saying its rollout, apply, or caller
    // pin is still pending must use `Refs` instead (`closing_trailer`).
    if ci {
        for refusal in closing_trailer::refusals_for_this_pull_request() {
            let message = refusal.message();
            eprintln!("pull request body: {message}");
            gate_errors.push(GateError::new("pull request body", None, message));
        }
    }

    // Close by naming every failing line again in one block, so the answer
    // to "which line do I fix" is the last thing on screen rather than
    // something to be found by scrolling.
    print_error_recap(&gate_errors);

    if project {
        eprintln!("navigator project gate: repository layout");
    }
    let layout = project.then(|| projects::repository::validate_gate(dir, None, !ci));
    let layout_errors = layout.map_or(0, |outcome| outcome.errors);

    if !gate_errors.is_empty() || layout_errors > 0 {
        // `validate_gate`'s own "Validated Project repository …" line above
        // states only its own pass's tally; when the content pass is what
        // actually fails the run (LAW-86 item 4), that line alone reads as a
        // clean "0 error(s)" even though the command is about to exit 1.
        // Only a Project repository prints that second summary in the first
        // place, so only there does the combined, true reason need restating
        // last; a plain repository's existing error recapitulation already
        // ends on the failing line.
        if project {
            println!(
                "{}",
                palette::dim(format!(
                    "navigator project gate: {} content finding(s), {layout_errors} repository-layout error(s) — failing",
                    gate_errors.len(),
                ))
            );
        }
        return ExitCode::from(1);
    }
    if !project {
        return ExitCode::SUCCESS;
    }
    eprintln!("navigator project gate: live project status");
    // The live Project coordinates are read under the operator's stored
    // login locally, or a scoped OIDC document session in CI.
    projects::gate::live_status(dir, ci).await
}

pub fn fix_gate_markdown(
    dir: &std::path::Path,
    markdown_filter: &ProjectGateMarkdownFilter<'_>,
    project: bool,
    write_fixes: bool,
) -> std::io::Result<FixReport> {
    let question_codes = rules::canonical_question_codes();
    let onboarding_stub = project.then(|| {
        let (relative, stub) = project_scaffold::scaffold_draft(project_scaffold::ONBOARDING);
        (dir.join(relative), stub)
    });

    // The markdown pass and the autofix are one walk: every safe-by-construction
    // edit lands first, and what it reports is what survived them.
    fix_directory(
        dir,
        markdown_filter,
        |file| {
            let mut checks = rules::navigator_classified_rules_with_codes(file, &question_codes);
            // The generated placeholder has no engagement terms to assign to a jurisdiction.
            if onboarding_stub
                .as_ref()
                .is_some_and(|(path, stub)| &file.path == path && &file.contents == stub)
            {
                checks.retain(|rule| rule.code() != rules::F110JurisdictionPath::CODE);
            }
            checks
        },
        write_fixes,
    )
}

/// The Project gate validates authored Markdown throughout the repository,
/// while raw document bodies beneath its root `documents/` directory are
/// local ignored files rather than repository source. Keep the exclusion
/// root-relative so an application-owned `documents/` directory elsewhere
/// remains part of the authored tree.
pub struct ProjectGateMarkdownFilter<'a> {
    pub root: &'a Path,
    pub default: rules::DefaultFileFilter,
}

impl<'a> ProjectGateMarkdownFilter<'a> {
    #[must_use]
    pub fn new(root: &'a Path) -> Self {
        Self {
            root,
            default: rules::DefaultFileFilter::default(),
        }
    }

    #[must_use]
    pub fn is_project_documents_path(&self, path: &Path) -> bool {
        path.strip_prefix(self.root)
            .ok()
            .and_then(|relative| relative.components().next())
            .is_some_and(|component| component.as_os_str() == "documents")
    }
}

impl rules::FileFilter for ProjectGateMarkdownFilter<'_> {
    fn include_dir(&self, path: &Path) -> bool {
        !self.is_project_documents_path(path) && self.default.include_dir(path)
    }

    fn include_file(&self, path: &Path) -> bool {
        !self.is_project_documents_path(path) && self.default.include_file(path)
    }
}

/// Sort `local_bytes::verify_with`'s findings into check failures (modified
/// bytes) and reported-missing pointers, the checkout's ordinary state.
pub fn split_byte_findings(
    byte_findings: &[projects::local_bytes::Finding],
    findings: &mut Vec<projects::document_check::CheckFailure>,
    missing: &mut Vec<String>,
) {
    for finding in byte_findings {
        match &finding.status {
            projects::local_bytes::Status::Match => {}
            projects::local_bytes::Status::Missing => {
                // A checkout that never downloaded a document's bytes is
                // ordinary, so a missing pointer passes — reported, not failed.
                missing.push(finding.pointer.clone());
            }
            projects::local_bytes::Status::Modified { reason } => {
                findings.push(projects::document_check::CheckFailure {
                    location: finding.pointer.clone(),
                    message: format!("modified: {reason}"),
                });
            }
        }
    }
}

/// `navigator project documents check` (LAW-217) — the read-only document
/// checks over one documents tree: the checkout's own `documents/` by
/// default, or any tree laid out like it via `--documents-dir` or
/// `--drive-local`. Unlike `project gate --check` it never contacts a
/// deployment and never writes — a Drive copy is not a checkout to fix — so
/// the only failure exit but `2` is a finding. `--deep` additionally hashes
/// each committed pointer's bytes against its `sha256` and `size_bytes`,
/// reading them from whichever tree is being checked.
#[must_use]
pub fn run_documents_check(
    dir: &Path,
    documents_dir: Option<PathBuf>,
    drive_local: bool,
    deep: bool,
    json: bool,
) -> ExitCode {
    let documents = if let Some(path) = documents_dir {
        path
    } else if drive_local {
        let (project, _) = match crate::document_sync::read_manifest(dir) {
            Ok(manifest) => manifest,
            Err(error) => {
                eprintln!("navigator: --drive-local needs a Project checkout: {error:#}");
                return ExitCode::from(2);
            }
        };
        match projects::document_check::resolve_drive_local(&project) {
            Ok(path) => path,
            Err(error) => {
                eprintln!("navigator: {error:#}");
                return ExitCode::from(2);
            }
        }
    } else {
        dir.join(document_sync::DOCUMENT_DIRECTORY)
    };

    let mut findings = match projects::document_check::check_documents_root(&documents) {
        Ok(findings) => findings,
        Err(error) => {
            eprintln!("navigator: {error:#}");
            return ExitCode::from(2);
        }
    };

    // The pointer byte checks are a checkout question `--deep` answers
    // against whichever tree holds the bytes (LAW-217): the checkout's
    // `documents/` by default, or `--documents-dir` / the Drive copy.
    let checkout = dir.join(projects::manifest::FILE).is_file();
    let mut missing: Vec<String> = Vec::new();
    if deep && checkout {
        match projects::local_bytes::verify_with(dir, &documents) {
            Ok(byte_findings) => split_byte_findings(&byte_findings, &mut findings, &mut missing),
            Err(error) => {
                eprintln!("navigator: {error:#}");
                return ExitCode::from(2);
            }
        }
    }

    if json {
        let report = serde_json::json!({
            "root": documents.display().to_string(),
            "findings": findings
                .iter()
                .map(|finding| serde_json::json!({
                    "location": finding.location,
                    "message": finding.message,
                }))
                .collect::<Vec<_>>(),
            "missing": missing,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("the report is valid JSON")
        );
    } else {
        for pointer in &missing {
            println!("{}", palette::dim(format!("{pointer}: missing")));
        }
        for finding in &findings {
            println!(
                "{}",
                diagnostic_line(
                    rules::Severity::Error,
                    &finding.location,
                    None,
                    &finding.message,
                )
            );
        }
        println!(
            "{}",
            palette::dim(if findings.is_empty() {
                format!("documents check: no findings in {}", documents.display())
            } else {
                format!(
                    "documents check: {} finding(s) in {}; nothing was written",
                    findings.len(),
                    documents.display()
                )
            })
        );
    }

    if findings.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// `--check` alone (LAW-62): the live document check, and nothing an offline
/// pass could have already told the caller. `verify` runs the full offline
/// gate — including this same live-row question, at the end of [`run_gate`]
/// above — so this makes only the one request unique to `documents`:
/// comparing committed pointers against what the deployment holds.
pub async fn run_document_check_gate(dir: &std::path::Path, ci: bool, deep: bool) -> ExitCode {
    let mut gate_errors = Vec::new();
    if deep {
        if let Err(error) = append_local_byte_findings(dir, &mut gate_errors) {
            eprintln!("navigator: {error:#}");
            return ExitCode::from(2);
        }
    }
    match projects::document_check::run(dir, ci, deep).await {
        Ok(outcome) => append_document_check(ci, &outcome, &mut gate_errors),
        Err(error) => {
            eprintln!("navigator: {error:#}");
            return crate::remote::exit_code_for(&error);
        }
    }
    append_skill_pin_findings(dir, &mut gate_errors);
    print_error_recap(&gate_errors);
    if !gate_errors.is_empty() {
        return ExitCode::from(1);
    }
    if !ci || !is_project_repository(dir) {
        return ExitCode::SUCCESS;
    }
    projects::gate::live_status(dir, true).await
}

/// ENG-879: extend `--check` to resolve every `skills:` pin in this
/// Project's `navigator.yaml` against the compiled-in Project Skill catalog
/// — an offline question `--check` can answer with no live-status door,
/// unlike [`projects::gate::live_status`] above. Only a Project repository
/// carries `navigator.yaml`, and only one that pins at least one Project
/// Skill has anything to resolve; either absence is silently fine rather
/// than a finding. Shares [`projects::skill::resolve_pins`] with `navigator
/// project skill status` (ENG-880), so the two never disagree about the same
/// fixture.
pub fn append_skill_pin_findings(dir: &std::path::Path, gate_errors: &mut Vec<GateError>) {
    if !is_project_repository(dir) {
        return;
    }
    let manifest_path = dir.join(projects::manifest::FILE);
    if !manifest_path.is_file() {
        return;
    }
    let resolutions = match projects::skill::resolve_pins(dir) {
        Ok(resolutions) => resolutions,
        Err(error) => {
            eprintln!("navigator: {error}");
            return;
        }
    };
    let location = manifest_path.display().to_string();
    for resolution in resolutions.iter().filter(|resolution| !resolution.resolved) {
        let message = match &resolution.catalog_version {
            Some(catalog_version) => format!(
                "Project Skill {}/{} is pinned at version {} but the catalog now carries version {catalog_version}",
                resolution.jurisdiction, resolution.practice_area, resolution.pinned_version
            ),
            None => format!(
                "Project Skill {}/{} (pinned at version {}) is not in the catalog",
                resolution.jurisdiction, resolution.practice_area, resolution.pinned_version
            ),
        };
        println!(
            "{}",
            diagnostic_line(rules::Severity::Error, &location, None, &message)
        );
        gate_errors.push(GateError::new(location.clone(), None, message));
    }
}

/// `--check --deep`: hash the Git-ignored bytes beside each pointer and
/// compare them with the pointer's `sha256` and `size_bytes`, before any
/// request is made. A pointer whose bytes were never downloaded is reported
/// and passes; a pointer whose bytes differ fails the gate.
pub fn append_local_byte_findings(
    dir: &std::path::Path,
    gate_errors: &mut Vec<GateError>,
) -> anyhow::Result<()> {
    use projects::local_bytes::{tally, verify, Status};
    let findings = verify(dir)?;
    for finding in &findings {
        match &finding.status {
            Status::Match => {}
            Status::Missing => println!(
                "{}",
                palette::dim(format!("{}: missing locally", finding.pointer))
            ),
            Status::Modified { reason } => {
                let message = format!("modified locally: {reason}");
                println!(
                    "{}",
                    diagnostic_line(rules::Severity::Error, &finding.pointer, None, &message)
                );
                gate_errors.push(GateError::new(finding.pointer.clone(), None, message));
            }
        }
    }
    let counts = tally(&findings);
    println!(
        "{}",
        palette::dim(format!(
            "documents: local bytes: {} match, {} modified locally, {} missing locally",
            counts.matched, counts.modified, counts.missing
        ))
    );
    Ok(())
}

/// Print the live document check and keep the failures that fail the gate.
///
/// A fix is printed as `fixed` when this run may write. Under `--ci` the same
/// fix is a finding: the checkout is about to be discarded, so the job names
/// the edit and stops.
pub fn append_document_check(
    ci: bool,
    outcome: &projects::document_check::Outcome,
    gate_errors: &mut Vec<GateError>,
) {
    let notice = if ci {
        "documents: checking live records; --ci writes nothing, and a fix fails this job"
    } else {
        "documents: checking live records; fixes stay in this checkout and the live site is not written"
    };
    println!("{}", palette::dim(notice));
    for failure in &outcome.errors {
        println!(
            "{}",
            diagnostic_line(
                rules::Severity::Error,
                &failure.location,
                None,
                &failure.message,
            )
        );
        gate_errors.push(GateError::new(
            failure.location.clone(),
            None,
            failure.message.clone(),
        ));
    }
    // LAW-284: a chain extension the pointer's own revision survives is
    // advice, not drift — it never lands in `gate_errors`, so it cannot
    // fail the run.
    for warning in &outcome.warnings {
        println!(
            "{}",
            diagnostic_line(
                rules::Severity::Warning,
                &warning.location,
                None,
                &warning.message,
            )
        );
    }
    for fix in &outcome.fixes {
        if ci {
            let message = format!(
                "would {}; run `navigator project gate --check` and commit the result",
                fix.action
            );
            println!(
                "{}",
                diagnostic_line(rules::Severity::Error, &fix.path, None, &message)
            );
            gate_errors.push(GateError::new(fix.path.clone(), None, message));
        } else {
            println!(
                "{}",
                palette::dim(format!("fixed {}: {}", fix.path, fix.action))
            );
        }
    }
    let verb = if ci { "unfixed" } else { "fixed" };
    println!(
        "{}",
        palette::dim(format!(
            "documents: found {} error(s), {} warning(s), {verb} {} file(s)",
            outcome.errors.len(),
            outcome.warnings.len(),
            outcome.fixes.len(),
        ))
    );
}

/// Print everything the content pass found — the files it fixed, then each
/// surviving violation, then the counts — and return the findings that fail the
/// run. Split out of [`run_gate`] because it is the whole of what a reader sees,
/// and reads better on its own than as the middle of a longer function.
#[must_use]
pub fn report_content_findings(
    dir: &std::path::Path,
    ci: bool,
    report: &FixReport,
    violations: &[rules::Violation],
) -> Vec<GateError> {
    for path in &report.fixed_files {
        let relative = path.strip_prefix(dir).unwrap_or(path);
        if ci {
            // A formatting finding is about the file, not one line of it, so it
            // carries no line number — naming one would send a reader to a line
            // that may be nowhere near the edit.
            println!(
                "{}",
                diagnostic_line(
                    rules::severity_for_code("F001"),
                    &relative.display().to_string(),
                    Some("F001"),
                    FIXABLE_IN_CI,
                )
            );
        } else {
            println!("{}", palette::dim(format!("fixed {}", relative.display())));
        }
    }
    for violation in violations {
        print_violation(
            &violation.path.display().to_string(),
            violation.line,
            violation.code,
            &violation.message,
        );
    }

    let (error_count, warning_count) = severity_counts(violations);
    let mut gate_errors: Vec<GateError> = violations
        .iter()
        .filter(|violation| rules::severity_for_code(violation.code) == rules::Severity::Error)
        .map(|violation| {
            GateError::new(
                format!("{}:{}", violation.path.display(), violation.line),
                Some(violation.code),
                violation.message.clone(),
            )
        })
        .collect();
    if ci {
        gate_errors.extend(report.fixed_files.iter().map(|path| {
            GateError::new(
                path.strip_prefix(dir).unwrap_or(path).display().to_string(),
                Some("F001"),
                FIXABLE_IN_CI.to_string(),
            )
        }));
    }

    // Under `--ci` nothing was written, so "fixed" would be a lie: the same
    // count is what a local run would have to fix.
    let fixes = if ci { "unformatted" } else { "fixed" };
    println!(
        "{}",
        palette::dim(format!(
            "Scanned {} file(s), found {error_count} error(s), {warning_count} warning(s), {fixes} {} file(s)",
            report.files_scanned,
            report.fixed_files.len(),
        ))
    );
    gate_errors
}

/// What `--ci` says about a file the gate could have fixed but must not write.
pub const FIXABLE_IN_CI: &str = "not formatted; run `navigator project gate` and commit the result";

/// Parse a `--answer code=value` argument into its halves. The value
/// may itself contain `=`; only the first `=` splits.
pub fn parse_answer(raw: &str) -> Result<(String, String), String> {
    let (code, value) = raw
        .split_once('=')
        .ok_or_else(|| format!("expected `code=value`, got `{raw}`"))?;
    if code.is_empty() {
        return Err(format!("empty answer code in `{raw}`"));
    }
    Ok((code.to_string(), unescape_answer_value(value)))
}

/// Turn the two-character escapes a shell leaves in `--answer` into the
/// characters the notation body needs. `\n` becomes a newline so a Markdown
/// table is more than one line; `\\` stays one backslash.
#[must_use]
pub fn unescape_answer_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('\\') | None => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    out
}

pub fn parse_asset_kind(value: &str) -> Result<String, String> {
    let accepted: Vec<&str> = rules::kind::Kind::ALL
        .iter()
        .filter(|k| k.valid_for(rules::kind::Lane::Asset))
        .map(|k| k.as_str())
        .collect();
    match rules::kind::Kind::parse(value).filter(|k| k.valid_for(rules::kind::Lane::Asset)) {
        Some(k) => Ok(k.as_str().to_string()),
        None => Err(format!(
            "`{value}` is not a document kind. Accepted values are: {}.",
            accepted.join(", ")
        )),
    }
}

pub fn parse_matter_folder(value: &str) -> Result<model::document_pointers::MatterFolder, String> {
    model::document_pointers::MatterFolder::parse(value).ok_or_else(|| {
        format!(
            "`{value}` is not a matter folder. Accepted values are: {}.",
            model::document_pointers::MatterFolder::ALL
                .iter()
                .map(|folder| folder.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

pub fn parse_document_visibility(value: &str) -> Result<String, String> {
    match value {
        "client" | "internal" => Ok(value.to_string()),
        _ => Err("visibility must be `client` or `internal`".into()),
    }
}

pub fn parse_signing_method(value: &str) -> Result<model::documents::SigningMethod, String> {
    model::documents::SigningMethod::parse(value).ok_or_else(|| {
        let accepted = model::documents::SigningMethod::ALL
            .iter()
            .map(|method| format!("`{}`", method.as_str()))
            .collect::<Vec<_>>()
            .join(" or ");
        format!("signing method must be {accepted}")
    })
}

/// Parse one `--evidence-confidence` value.
pub fn parse_evidence_confidence(
    value: &str,
) -> Result<model::documents::EvidenceConfidence, String> {
    model::documents::EvidenceConfidence::parse(value).ok_or_else(|| {
        let accepted = model::documents::EvidenceConfidence::ALL
            .iter()
            .map(|confidence| format!("`{}`", confidence.as_str()))
            .collect::<Vec<_>>()
            .join(" or ");
        format!("evidence confidence must be {accepted}")
    })
}

/// Parse one `--notarization` value: five comma-separated `key=value` pairs,
/// each key exactly once, validated as the server validates it.
pub fn parse_notarization(value: &str) -> Result<model::documents::Notarization, String> {
    const KEYS: [&str; 5] = [
        "method",
        "acknowledged_on",
        "state",
        "county",
        "commission_expires",
    ];
    let mut fields = std::collections::BTreeMap::new();
    for pair in value.split(',') {
        let (key, field) = pair
            .split_once('=')
            .ok_or_else(|| format!("`{pair}` is not a key=value pair"))?;
        let key = key.trim();
        if !KEYS.contains(&key) {
            return Err(format!(
                "unknown notarization key `{key}`; expected {}",
                KEYS.join(", ")
            ));
        }
        if fields.insert(key, field.trim()).is_some() {
            return Err(format!("notarization key `{key}` appears twice"));
        }
    }
    let field = |key: &str| {
        fields
            .get(key)
            .copied()
            .ok_or_else(|| format!("notarization needs `{key}=`; expected {}", KEYS.join(", ")))
    };
    let method = field("method")?;
    let notarization = model::documents::Notarization {
        method: model::documents::NotarizationMethod::parse(method).ok_or_else(|| {
            format!("notarization method must be `in_person` or `remote_online`, not `{method}`")
        })?,
        acknowledged_on: parse_calendar_date(field("acknowledged_on")?)?,
        state: field("state")?.to_string(),
        county: field("county")?.to_string(),
        commission_expires: parse_calendar_date(field("commission_expires")?)?,
    };
    notarization.validate().map_err(|error| error.to_string())?;
    Ok(notarization)
}

pub fn parse_calendar_date(value: &str) -> Result<chrono::NaiveDate, String> {
    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| format!("`{value}` is not a calendar date in YYYY-MM-DD form"))
}

pub fn parse_transcript_quality(value: &str) -> Result<String, String> {
    match value {
        "machine" | "proofread" => Ok(value.to_string()),
        _ => Err("transcript quality must be `machine` or `proofread`".into()),
    }
}

pub const DOCUMENT_UPLOAD_KIND_HELP: &str = "Accepted --kind values: letter, filing, will, trust, directive, agreement, pleading, declaration, affidavit, onboarding, offboarding, memo, resume, trial_prep, transcript, inbound_contract, certificate_of_naturalization, exhibit, order, closed_repository, invoice, unclassified.";

pub const DOCUMENT_TRANSCRIBE_HELP: &str = "Each page is rendered locally, orientation-corrected, deskewed, and OCR'd before the transcript is uploaded as a `transcript` revision linked to the exact source asset id, version, and SHA-256. OCR requires local Poppler (`pdftoppm`) and Tesseract with the `eng` and `osd` language data. Source pages and transcript text are sent to no OCR provider and are never written into Git.";

pub const DOCUMENT_SYNC_HELP: &str = "Defaults: staged pointers are internal-visible and preserve that visibility when they already exist. Kind inference maps pleadings to filing, exhibits to exhibit, agreements to agreement, invoices to invoice, memos to memo, transcripts to transcript, and everything else to unclassified — except documents/cases/** and documents/rules/**, which are not Project documents at all: each is routed through `site authorities create` (a sidecar carrying citation/class/title/canonical_url/checked_on is required beside each capture) and its committed pointer carries an authority_id rather than a plain kind inference. documents/cases/** is case_law only; documents/rules/** is every other Authority class (statute, regulation, administrative, secondary) — a sidecar whose class disagrees with its folder is refused. A documents/invoices/** filename must match `INV-<digits>.<ext>`. Storage remains content-addressed under the existing Project documents keys; sync does not rename or migrate those keys. A folder outside those categories is therefore intentionally unclassified, not an error.";

/// Return the body of a notation file — everything after the leading
/// YAML frontmatter block. When there is no recognized frontmatter, the
/// whole string is the body.
#[must_use]
pub fn strip_frontmatter(contents: &str) -> &str {
    let Some(after_open) = contents.strip_prefix("---\n") else {
        return contents;
    };
    if let Some(end) = after_open.find("\n---\n") {
        return after_open[end + "\n---\n".len()..].trim_start_matches('\n');
    }
    // Closer at EOF with no body, or no closer at all.
    after_open.strip_suffix("\n---").map_or(contents, |_| "")
}

pub struct FixReport {
    pub files_scanned: usize,
    pub fixed_files: Vec<PathBuf>,
    pub remaining: Vec<rules::Violation>,
}

/// The most passes `fix_directory` will make over one file before
/// giving up. A pass that changes nothing ends the loop, so this bound
/// is only reached by two rules that disagree — and reaching it leaves
/// the file valid, just not finished.
pub const MAX_FIX_PASSES: usize = 8;

/// Walk `dir` honoring `filter`, apply every safe-by-construction
/// autofix to each markdown file in place, and then re-lint to
/// collect the diagnostic-only violations a human still needs to
/// address. Edits within a file are applied highest-offset-first so
/// earlier offsets stay valid; on overlap the rule with the lower
/// code string wins (deterministic), and the file is re-scanned until
/// it stops changing so a deferred or newly uncovered fix still lands
/// in the same run.
pub fn fix_directory(
    dir: &std::path::Path,
    filter: &dyn rules::FileFilter,
    rules_for_file: impl Fn(&rules::SourceFile) -> Vec<Box<dyn rules::Rule>>,
    write: bool,
) -> std::io::Result<FixReport> {
    let mut files_scanned = 0usize;
    let mut fixed_files = Vec::new();
    let mut remaining = Vec::new();
    for entry in walkdir::WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if e.file_type().is_dir() && e.depth() > 0 {
                filter.include_dir(e.path())
            } else {
                true
            }
        })
    {
        let entry = entry.map_err(std::io::Error::other)?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if !filter.include_file(path) {
            continue;
        }
        files_scanned += 1;
        let contents = std::fs::read_to_string(path)?;
        let mut file = rules::SourceFile {
            path: path.to_path_buf(),
            contents,
        };
        let original = file.contents.clone();
        // Repeat until the file stops changing: one fix routinely
        // uncovers another. Dropping an overlapping edit defers it to
        // the next pass, and a fix can create a fresh violation outright
        // — trimming a hard break off a short line hands that line to
        // `S102`. The bound only guards against a pair of rules that
        // undo each other; every pass either changes the file or is the
        // last one.
        for _ in 0..MAX_FIX_PASSES {
            let rule_set = rules_for_file(&file);
            let mut edits: Vec<(rules::TextEdit, &'static str)> = Vec::new();
            for rule in &rule_set {
                for v in rule.lint(&file) {
                    if let Some(edit) = rule.fix(&file, &v) {
                        edits.push((edit, rule.code()));
                    }
                }
            }
            if edits.is_empty() {
                break;
            }
            // Sort ascending by start; resolve overlap by keeping the
            // lower-coded edit. Then apply descending.
            edits.sort_by(|a, b| a.0.range.start.cmp(&b.0.range.start).then(a.1.cmp(b.1)));
            let mut kept: Vec<(rules::TextEdit, &'static str)> = Vec::with_capacity(edits.len());
            for (edit, code) in edits {
                if let Some(prev) = kept.last() {
                    if edit.range.start < prev.0.range.end {
                        continue;
                    }
                }
                kept.push((edit, code));
            }
            kept.sort_by_key(|edit| std::cmp::Reverse(edit.0.range.start));
            let mut new_contents = file.contents.clone();
            for (edit, _) in &kept {
                new_contents.replace_range(edit.range.clone(), &edit.new_text);
            }
            if new_contents == file.contents {
                break;
            }
            file.contents = new_contents;
        }
        if file.contents != original {
            // `write` is off under `--ci`, where the checkout is discarded the
            // moment the job ends. The fix is still computed — that is how the
            // caller knows the file needs one — but writing it would let a
            // formatting problem pass a gate and never reach `main`.
            if write {
                std::fs::write(path, &file.contents)?;
            }
            fixed_files.push(path.to_path_buf());
        }
        for rule in &rules_for_file(&file) {
            remaining.extend(rule.lint(&file));
        }
    }
    Ok(FixReport {
        files_scanned,
        fixed_files,
        remaining,
    })
}

/// Split a violation list into `(error_count, warning_count)` by each
/// code's [`rules::Severity`]. Used for the `validate` summary line so
/// blocking errors and "not built yet" advisories are tallied apart.
#[must_use]
pub fn severity_counts(violations: &[rules::Violation]) -> (usize, usize) {
    let errors = violations
        .iter()
        .filter(|v| rules::severity_for_code(v.code) == rules::Severity::Error)
        .count();
    (errors, violations.len() - errors)
}

/// Render one diagnostic: its severity marker, then `path:line` in dim
/// cyan-700, then the rule code in cyan-500 and the message.
///
/// `location` is pre-rendered rather than taken as `(path, line)` because
/// the YAML syntax pass reports a column too, and `code` is `None` for the
/// two passes that carry no rule code at all — the YAML syntax check and
/// the consumed-mutable-tag guard. Both shapes only arise in the error
/// recapitulation; the per-violation listing always has a code.
#[must_use]
pub fn diagnostic_line(
    severity: rules::Severity,
    location: &str,
    code: Option<&str>,
    message: &str,
) -> String {
    let marker = match severity {
        rules::Severity::Error => "error:",
        rules::Severity::Warning => "warning:",
    };
    match code {
        Some(code) => format!(
            "{marker} {} {}: {message}",
            palette::dim(location),
            palette::highlight(code),
        ),
        None => format!("{marker} {}: {message}", palette::dim(location)),
    }
}

/// Render a single rule violation with its severity: path/line in dim
/// cyan-700, rule code in cyan-500, message in default. Shared by validate
/// and site seed so both subcommands have the same look.
pub fn print_violation(path: &str, line: usize, code: &str, message: &str) {
    println!(
        "{}",
        diagnostic_line(
            rules::severity_for_code(code),
            &format!("{path}:{line}"),
            Some(code),
            message,
        )
    );
}

/// One finding that fails the gate, retained so a run can
/// recapitulate every error at the end.
///
/// The primary listing prints in tree order — per pass, per file, per
/// line — which is what keeps a file's violations adjacent and the body
/// readable. That order also scatters the errors: among the warnings
/// inside the markdown pass, and across the four standalone passes that
/// print after it. Collecting them here is what lets the run close with
/// a single block naming every line that failed.
pub struct GateError {
    /// `path:line`, or `path:line:column` where the pass knows one.
    pub location: String,
    /// The rule code, or `None` for the passes that have none: the YAML
    /// syntax check, the consumed-mutable-tag guard, and the pull-request
    /// closing-trailer check.
    pub code: Option<String>,
    pub message: String,
}

impl GateError {
    fn new(location: impl Into<String>, code: Option<&str>, message: impl Into<String>) -> Self {
        Self {
            location: location.into(),
            code: code.map(str::to_string),
            message: message.into(),
        }
    }

    fn render(&self) -> String {
        diagnostic_line(
            rules::Severity::Error,
            &self.location,
            self.code.as_deref(),
            &self.message,
        )
    }
}

/// Close a `validate` run by naming every error again, on its own, after
/// the passes have finished printing.
///
/// `validate` runs six passes and the four standalone ones print *after*
/// the markdown summary, so no ordering within a single pass can gather a
/// YAML error and a mutable-tag error together — only a block after every
/// pass can name every failing line. It is additive, so the primary
/// listing keeps the tree order that makes it readable in the first
/// place, and it puts the failing lines where a terminal leaves the
/// reader: at the tail.
pub fn print_error_recap(errors: &[GateError]) {
    if errors.is_empty() {
        return;
    }
    println!();
    println!(
        "{}",
        palette::header(format!("{} error(s) fail this run:", errors.len()))
    );
    for error in errors {
        println!("{}", error.render());
    }
}

/// Tests for the `project gate`/`validate` helper seams: the pure argument
/// parsers, the mutable-tag detection chain, and the standalone tree passes.
/// Each test drives a real path through the function under test and asserts
/// evidence unique to that path.
#[cfg(test)]
mod gate_helpers_tests {
    use super::*;
    use std::fmt::Write as _;

    // ------------------------------------------------------------------
    // Pure argument parsers
    // ------------------------------------------------------------------

    #[test]
    fn parse_answer_splits_on_the_first_equals_only() {
        let (code, value) = parse_answer("case_number=42=extra").expect("parses");
        assert_eq!(code, "case_number");
        assert_eq!(value, "42=extra");
        assert_eq!(
            parse_answer("no_equals").unwrap_err(),
            "expected `code=value`, got `no_equals`"
        );
        assert_eq!(
            parse_answer("=value").unwrap_err(),
            "empty answer code in `=value`"
        );
    }

    #[test]
    fn unescape_answer_value_turns_escapes_into_real_characters() {
        assert_eq!(unescape_answer_value("line1\\nline2"), "line1\nline2");
        assert_eq!(unescape_answer_value("a\\tb"), "a\tb");
        assert_eq!(unescape_answer_value("windows\\r\\n"), "windows\r\n");
        assert_eq!(unescape_answer_value("back\\\\slash"), "back\\slash");
        // A trailing backslash stays one backslash rather than vanishing.
        assert_eq!(unescape_answer_value("ends\\"), "ends\\");
        // An unknown escape is preserved verbatim, never dropped.
        assert_eq!(unescape_answer_value("q\\x?"), "q\\x?");
        assert_eq!(unescape_answer_value("plain"), "plain");
    }

    #[test]
    fn parse_stale_duration_reads_days_hours_and_minutes_only() {
        assert_eq!(parse_stale_duration("30d"), chrono::Duration::try_days(30));
        assert_eq!(parse_stale_duration("12h"), chrono::Duration::try_hours(12));
        assert_eq!(
            parse_stale_duration("90m"),
            chrono::Duration::try_minutes(90)
        );
        // Zero, negative-looking, unknown units, non-numeric, and empty
        // inputs are all rejected rather than guessed.
        assert_eq!(parse_stale_duration("0d"), None);
        assert_eq!(parse_stale_duration("5w"), None);
        assert_eq!(parse_stale_duration("abc"), None);
        assert_eq!(parse_stale_duration(""), None);
    }

    #[test]
    fn parse_document_visibility_accepts_only_the_two_published_values() {
        assert_eq!(parse_document_visibility("client").as_deref(), Ok("client"));
        assert_eq!(
            parse_document_visibility("internal").as_deref(),
            Ok("internal")
        );
        assert!(parse_document_visibility("public").is_err());
    }

    #[test]
    fn parse_transcript_quality_accepts_only_machine_or_proofread() {
        assert_eq!(
            parse_transcript_quality("machine").as_deref(),
            Ok("machine")
        );
        assert_eq!(
            parse_transcript_quality("proofread").as_deref(),
            Ok("proofread")
        );
        assert!(parse_transcript_quality("verified").is_err());
    }

    #[test]
    fn parse_calendar_date_demands_year_month_day_form() {
        assert_eq!(
            parse_calendar_date("2026-01-15").map(|d| d.to_string()),
            Ok("2026-01-15".to_string())
        );
        assert!(parse_calendar_date("01/15/2026").is_err());
        assert!(parse_calendar_date("2026-13-01").is_err());
    }

    #[test]
    fn parse_signing_method_names_both_accepted_spellings_on_error() {
        assert!(parse_signing_method("wet_ink").is_ok());
        assert!(parse_signing_method("docusign").is_ok());
        let message = parse_signing_method("carrier_pigeon").unwrap_err();
        assert!(message.contains("wet_ink"), "{message}");
        assert!(message.contains("docusign"), "{message}");
    }

    #[test]
    fn parse_evidence_confidence_accepts_the_closed_vocabulary() {
        assert!(parse_evidence_confidence("exact").is_ok());
        assert!(parse_evidence_confidence("inferred").is_ok());
        assert!(parse_evidence_confidence("unknown").is_ok());
        assert!(parse_evidence_confidence("certain").is_err());
    }

    #[test]
    fn parse_notarization_reads_all_five_fields_and_validates_them() {
        let value =
            "method=in_person,acknowledged_on=2026-01-15,state=NV,county=Washoe,commission_expires=2027-01-01";
        let notarization = parse_notarization(value).expect("a complete valid notarization");
        assert_eq!(notarization.state, "NV");
        assert_eq!(notarization.county, "Washoe");
        assert_eq!(notarization.acknowledged_on.to_string(), "2026-01-15");
        assert_eq!(notarization.commission_expires.to_string(), "2027-01-01");
    }

    #[test]
    fn parse_notarization_refuses_missing_unknown_and_duplicate_keys() {
        let missing = "method=in_person,acknowledged_on=2026-01-15,state=NV,county=Washoe";
        let message = parse_notarization(missing).unwrap_err();
        assert!(message.contains("commission_expires"), "{message}");

        let unknown = "method=in_person,acknowledged_on=2026-01-15,state=NV,county=Washoe,commission_expires=2027-01-01,county2=X";
        assert!(parse_notarization(unknown).unwrap_err().contains("county2"));

        let duplicate = "method=in_person,method=remote_online,acknowledged_on=2026-01-15,state=NV,county=Washoe,commission_expires=2027-01-01";
        assert!(parse_notarization(duplicate).unwrap_err().contains("twice"));

        // A pair without `=` never parses as a key.
        assert!(parse_notarization("method")
            .unwrap_err()
            .contains("key=value"));
        // A bad method name and a bad date are both refused.
        assert!(parse_notarization(
            "method=by_fax,acknowledged_on=2026-01-15,state=NV,county=Washoe,commission_expires=2027-01-01"
        )
        .unwrap_err()
        .contains("in_person"));
        assert!(parse_notarization(
            "method=in_person,acknowledged_on=yesterday,state=NV,county=Washoe,commission_expires=2027-01-01"
        )
        .unwrap_err()
        .contains("YYYY-MM-DD"));
        // The server-side validation runs here too: a commission already
        // expired on the acknowledgment date is refused.
        assert!(parse_notarization(
            "method=in_person,acknowledged_on=2027-01-15,state=NV,county=Washoe,commission_expires=2027-01-01"
        )
        .unwrap_err()
        .contains("expire"));
        // A non-two-letter state fails the same validation.
        assert!(parse_notarization(
            "method=in_person,acknowledged_on=2026-01-15,state=Nevada,county=Washoe,commission_expires=2027-01-01"
        )
        .unwrap_err()
        .contains("two-letter"));
    }

    // ------------------------------------------------------------------
    // Pure helpers the gate passes share
    // ------------------------------------------------------------------

    #[test]
    fn yaml_and_containerfile_detection_matches_both_spellings() {
        assert!(is_yaml_path(std::path::Path::new("k8s/a.yaml")));
        assert!(is_yaml_path(std::path::Path::new("k8s/a.YML")));
        assert!(!is_yaml_path(std::path::Path::new("k8s/a.json")));
        assert!(!is_yaml_path(std::path::Path::new("Dockerfile")));
        assert!(is_containerfile_path(std::path::Path::new(
            "Containerfile.web"
        )));
        assert!(is_containerfile_path(std::path::Path::new("Dockerfile")));
        assert!(!is_containerfile_path(std::path::Path::new("deploy.yaml")));
    }

    #[test]
    fn authored_tree_walk_skips_generated_and_vendored_directories() {
        use walkdir::WalkDir;
        let dir = tempfile::tempdir().expect("tempdir");
        for skip in [".git", "target", ".worktrees", "node_modules", "dist"] {
            std::fs::create_dir_all(dir.path().join(skip)).expect("mkdir");
            let entry = WalkDir::new(dir.path())
                .into_iter()
                .filter_map(Result::ok)
                .find(|e| e.file_name() == skip)
                .expect("walkdir sees the directory");
            assert!(
                !include_authored_tree_entry(&entry),
                "{skip} must be pruned"
            );
        }
        let keep = WalkDir::new(dir.path())
            .into_iter()
            .find_map(Result::ok)
            .expect("root entry");
        assert!(include_authored_tree_entry(&keep));
    }

    #[test]
    fn mutable_image_tag_catches_only_the_latest_family() {
        assert_eq!(
            mutable_image_tag("ghcr.io/neon-law/web:latest", false).as_deref(),
            Some("latest")
        );
        assert_eq!(
            mutable_image_tag("ghcr.io/neon-law/web:latest-arm64", false).as_deref(),
            Some("latest-arm64")
        );
        // An untagged reference is implicit latest only when asked.
        assert!(mutable_image_tag("ghcr.io/neon-law/web", true).is_some());
        assert!(mutable_image_tag("ghcr.io/neon-law/web", false).is_none());
        // Registry-host ports are not tags.
        assert!(mutable_image_tag("registry:5000/img:26.11.3", false).is_none());
        assert!(mutable_image_tag("registry:5000/img", true).is_some());
        // Digest pins and version tags are immutable.
        assert!(mutable_image_tag("repo/img:1.0@sha256:abc", false).is_none());
        assert!(mutable_image_tag("repo/img:16-alpine", false).is_none());
        assert!(mutable_image_tag("repo/img:26.11.3", false).is_none());
        assert!(mutable_image_tag("", false).is_none());
    }

    #[test]
    fn split_trailing_comment_reports_pin_exempt_lines() {
        let (code, exempt) =
            split_trailing_comment("image: repo/img:latest # pin-exempt: publish-only");
        assert_eq!(code, "image: repo/img:latest");
        assert!(exempt);
        let (code, exempt) = split_trailing_comment("image: repo/img:latest # not exempt");
        assert_eq!(code, "image: repo/img:latest");
        assert!(!exempt);
        let (code, exempt) = split_trailing_comment("image: repo/img:latest");
        assert_eq!(code, "image: repo/img:latest");
        assert!(!exempt);
    }

    #[test]
    fn yaml_value_for_key_requires_the_exact_key() {
        assert_eq!(
            yaml_value_for_key("image: repo/img", "image"),
            Some("repo/img")
        );
        assert_eq!(
            yaml_value_for_key("image : repo/img", "image"),
            Some("repo/img")
        );
        // A lookalike key that merely starts with it is not a match.
        assert_eq!(yaml_value_for_key("imagePullPolicy: Always", "image"), None);
        assert_eq!(yaml_value_for_key("container: x", "image"), None);
    }

    #[test]
    fn yaml_image_entry_distinguishes_list_items_from_plain_images() {
        assert_eq!(
            yaml_image_entry("image: ghcr.io/neon-law/web:26.11.3"),
            Some(("ghcr.io/neon-law/web:26.11.3", false))
        );
        assert_eq!(
            yaml_image_entry("- image: navigator-web"),
            Some(("navigator-web", true))
        );
        assert_eq!(yaml_image_entry("kind: Deployment"), None);
    }

    #[test]
    fn containerfile_from_ref_is_case_insensitive_and_skips_flags() {
        assert_eq!(
            containerfile_from_ref("FROM repo/img:1.2"),
            Some("repo/img:1.2")
        );
        assert_eq!(
            containerfile_from_ref("from repo/img:1.2"),
            Some("repo/img:1.2")
        );
        assert_eq!(
            containerfile_from_ref("FROM --platform=linux/amd64 repo/img:1.2 AS build"),
            Some("--platform=linux/amd64 repo/img:1.2 AS build")
        );
        assert_eq!(containerfile_from_ref("RUN cargo build"), None);
        assert_eq!(containerfile_from_ref("FROM"), None);
    }

    #[test]
    fn detect_mutable_tags_finds_consumed_latest_and_honors_pin_exempt() {
        let manifest = std::path::Path::new("k8s/deploy.yaml");
        let findings = detect_mutable_tags(
            manifest,
            "image: ghcr.io/neon-law/web:latest\nimage: ghcr.io/neon-law/web:26.11.3\nimage: repo/img:latest # pin-exempt: publish-only construct\n",
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].0, 1);
        assert!(findings[0].1.contains("web:latest"), "{}", findings[0].1);

        // A Containerfile FROM is scanned the same way.
        let containerfile = std::path::Path::new("Dockerfile");
        let findings = detect_mutable_tags(containerfile, "FROM ghcr.io/neon-law/web:latest\n");
        assert_eq!(findings.len(), 1);

        // A workflow build-matrix list item is a published target: an
        // untagged name there is exempt, but an explicit :latest is not.
        let workflow = std::path::Path::new(".github/workflows/release.yml");
        let findings = detect_mutable_tags(
            workflow,
            "- image: navigator-web\nimage: navigator-web\n- image: navigator-web:latest\n",
        );
        assert_eq!(findings.len(), 2);
        assert_eq!(
            findings[0].0, 2,
            "the plain (consumed) untagged image is flagged"
        );
        assert_eq!(
            findings[1].0, 3,
            "the explicit :latest list item is flagged"
        );

        // `version: latest` counts only inside a GitHub workflow.
        let findings = detect_mutable_tags(workflow, "  version: latest\n");
        assert_eq!(findings.len(), 1);
        let chart = std::path::Path::new("charts/values.yaml");
        assert!(detect_mutable_tags(chart, "version: latest\n").is_empty());
    }

    #[test]
    fn mutable_tag_pass_walks_the_tree_and_reports_offences_with_locations() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bad_manifest = dir.path().join("k8s");
        std::fs::create_dir_all(&bad_manifest).expect("mkdir");
        std::fs::write(
            bad_manifest.join("deploy.yaml"),
            "image: ghcr.io/neon-law/web:latest\n",
        )
        .expect("write");
        std::fs::write(dir.path().join("Containerfile"), "FROM repo/img:26.11.3\n").expect("write");
        // Generated trees are pruned the same way every other pass prunes.
        std::fs::create_dir_all(dir.path().join("target")).expect("mkdir");
        std::fs::write(
            dir.path().join("target").join("deploy.yaml"),
            "image: ghcr.io/neon-law/web:latest\n",
        )
        .expect("write");

        let findings = mutable_tag_pass(dir.path()).expect("walks");
        assert_eq!(findings.len(), 1);
        assert!(findings[0].location.ends_with("k8s/deploy.yaml:1"));
        assert!(findings[0].message.contains("pin an explicit version"));
    }

    #[test]
    fn yaml_pass_reports_parse_errors_with_locations_and_clean_trees_pass() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("good.yaml"), "a: 1\nb:\n  - two\n").expect("write");
        std::fs::write(dir.path().join("bad.yaml"), "key: [unclosed\n").expect("write");
        std::fs::write(dir.path().join("notes.txt"), "key: [unclosed\n").expect("write");

        let errors = yaml_pass(dir.path()).expect("walks");
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0].code.is_none(),
            "the YAML syntax pass carries no rule code"
        );
        assert!(
            errors[0].message.contains("YAML parse error"),
            "{}",
            errors[0].message
        );
        assert!(
            errors[0].location.contains("bad.yaml"),
            "{}",
            errors[0].location
        );
    }

    #[test]
    fn seed_document_pass_checks_only_seeds_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("seeds")).expect("mkdir");
        std::fs::write(
            dir.path().join("seeds").join("Person.yaml"),
            "lookup_fields:\n  - email\nrecords:\n  - email: person@example.com\n    name: Sample Person\n",
        )
        .expect("write");
        // An unknown model name is refused with Y001.
        std::fs::write(
            dir.path().join("seeds").join("Widget.yaml"),
            "records: []\n",
        )
        .expect("write");
        // A YAML file outside `seeds/` is nobody's seed document.
        std::fs::write(dir.path().join("Widget.yaml"), "records: []\n").expect("write");

        let errors = seed_document_pass(dir.path()).expect("walks");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code.as_deref(), Some("Y001"));
        assert!(
            errors[0].message.contains("Widget"),
            "{}",
            errors[0].message
        );
    }

    #[test]
    fn seed_model_for_path_skips_only_a_bad_canonical_catalog() {
        let seeds = std::path::Path::new("store/seeds/Person.yaml");
        assert!(seed_model_for_path(seeds).is_some());
        // Outside the canonical store catalog every seeds/*.yaml is checked.
        let project_seeds = std::path::Path::new("acme/seeds/Widget.yaml");
        let model = seed_model_for_path(project_seeds).expect("checked");
        assert!(model.is_err());
        // Not a seed: the path lives outside any `seeds` directory.
        assert!(seed_model_for_path(std::path::Path::new("k8s/deploy.yaml")).is_none());
        // The extension is the caller's filter, not this helper's — a
        // non-YAML file under `seeds/` still resolves to its model.
        assert!(seed_model_for_path(std::path::Path::new("seeds/Person.json")).is_some());
    }

    #[test]
    fn locale_document_pass_refuses_unpublished_locales_and_unknown_pages() {
        let dir = tempfile::tempdir().expect("tempdir");
        let en = dir.path().join("locales").join("en");
        std::fs::create_dir_all(&en).expect("mkdir");
        // A valid shared catalog: every required key present, one line each.
        let mut shared = String::from("catalog_version: 1\n\nentries:\n");
        for key in views::locales::shared::REQUIRED_KEYS {
            let _ = writeln!(shared, "  {key}: Sample words");
        }
        std::fs::write(en.join("shared.yaml"), shared).expect("write");

        let fr = dir.path().join("locales").join("fr");
        std::fs::create_dir_all(&fr).expect("mkdir");
        std::fs::write(fr.join("home.yaml"), "a: 1\n").expect("write");
        std::fs::write(en.join("no-such-page.yaml"), "a: 1\n").expect("write");

        let errors = locale_document_pass(dir.path()).expect("walks");
        assert_eq!(errors.len(), 2);
        // WalkDir yields entries in no defined order, so match the two
        // rejections as a set rather than assuming which came first.
        let messages: Vec<&str> = errors.iter().map(|error| error.message.as_str()).collect();
        assert!(
            errors
                .iter()
                .all(|error| error.code.as_deref() == Some("Y002")),
            "both rejections carry the locale-document code: {messages:?}"
        );
        assert!(
            errors.iter().any(|error| error
                .message
                .contains("locale directory `fr` is not published")),
            "the unpublished locale is rejected: {messages:?}"
        );
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("unknown locale page `no-such-page`")),
            "the unknown page is rejected: {messages:?}"
        );
    }

    #[test]
    fn document_pointer_pass_reads_pointers_only_inside_a_project_repository() {
        let dir = tempfile::tempdir().expect("tempdir");
        let blobs = dir.path().join("blobs");
        std::fs::create_dir_all(&blobs).expect("mkdir");
        std::fs::write(
            blobs.join("sample.pdf.yaml"),
            "kind: exhibit\nvisibility: internal\ncurrent_version:\n  version: 1\n  asset_id: 11111111-2222-3333-4444-555555555555\n  created_at: 2026-01-15T00:00:00Z\n  sha256: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n  size_bytes: 1234\n",
        )
        .expect("write");

        // Without a `navigator.yaml` declaring a `project`, the tree is
        // ordinary source: nothing is validated, nothing is scanned.
        let outside = document_pointer_pass(dir.path()).expect("walks");
        assert!(outside.is_empty());

        std::fs::write(dir.path().join("navigator.yaml"), "project: acme\n").expect("write");
        let inside = document_pointer_pass(dir.path()).expect("walks");
        assert!(inside.is_empty());
        assert!(inside.iter().all(|e| e.code.as_deref() == Some("Y003")));

        // A pointer that breaks its contract is reported at the pointer path.
        std::fs::write(
            blobs.join("broken.pdf.yaml"),
            "kind: exhibit\nvisibility: everyone\ncurrent_version:\n  version: 1\n  asset_id: 11111111-2222-3333-4444-555555555555\n  created_at: 2026-01-15T00:00:00Z\n  sha256: aaaaaaaa\n  size_bytes: 1234\n",
        )
        .expect("write");
        let errors = document_pointer_pass(dir.path()).expect("walks");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code.as_deref(), Some("Y003"));
        assert!(errors[0].location.contains("broken.pdf.yaml:1"));
        assert!(
            errors[0].message.contains("visibility"),
            "{}",
            errors[0].message
        );
    }

    #[test]
    fn document_filename_pass_fails_a_non_conventional_name_and_passes_the_rename() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("navigator.yaml"), "project: acme\n").expect("write");
        let documents = dir.path().join("documents");
        std::fs::create_dir_all(documents.join("Client Production")).expect("mkdir");

        // Nothing downloaded: the name-only pass finds nothing.
        let empty = document_filename_pass(dir.path()).expect("walks");
        assert!(empty.is_empty());

        // A non-conventional document name fails, naming C004 and the
        // convention.
        std::fs::write(
            documents
                .join("Client Production")
                .join("complaint-annulment-2026-10-12.pdf"),
            "bytes",
        )
        .expect("write");
        let errors = document_filename_pass(dir.path()).expect("walks");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code.as_deref(), Some("C004"));
        assert!(errors[0].message.contains("C004"), "{}", errors[0].message);

        // Renamed to the convention, the same tree passes.
        std::fs::rename(
            documents
                .join("Client Production")
                .join("complaint-annulment-2026-10-12.pdf"),
            documents
                .join("Client Production")
                .join("2026.10.12_complaint-annulment.pdf"),
        )
        .expect("rename");
        let renamed = document_filename_pass(dir.path()).expect("walks");
        assert!(renamed.is_empty());

        // A docket filing keeps its own shipped convention, and a pointer
        // left under documents/ is not a filename finding.
        std::fs::create_dir_all(documents.join("Docket Filings")).expect("mkdir");
        std::fs::write(
            documents
                .join("Docket Filings")
                .join("DKT 1234 - answer.pdf"),
            "bytes",
        )
        .expect("write");
        std::fs::write(
            documents.join("2026.10.12_declaration.pdf.yaml"),
            "kind: pleading\n",
        )
        .expect("write");
        let exempt = document_filename_pass(dir.path()).expect("walks");
        assert!(exempt.is_empty());
    }

    #[test]
    fn document_pointer_path_matches_only_the_blobs_tree() {
        let root = std::path::Path::new("/repo");
        let pointer = std::path::Path::new("/repo/blobs/exhibit.sample.pdf.yaml");
        let relative = document_pointer_path(root, pointer).expect("a pointer path");
        assert_eq!(
            relative,
            std::path::Path::new("blobs/exhibit.sample.pdf.yaml")
        );
        // A nested file under `blobs/` is still a pointer.
        assert!(document_pointer_path(
            root,
            std::path::Path::new("/repo/blobs/onboarding/doc.pdf.yaml")
        )
        .is_some());
        // A path under another root is not ours to read.
        assert!(document_pointer_path(root, std::path::Path::new("/other/blobs/a.yaml")).is_none());
        assert!(
            document_pointer_path(root, std::path::Path::new("/repo/k8s/deploy.yaml")).is_none()
        );
    }

    #[test]
    fn severity_counts_splits_errors_from_warnings_by_rule_code() {
        let make = |code: &'static str, line: usize| rules::Violation {
            code,
            path: std::path::PathBuf::from("t.md"),
            line,
            range: 0..0,
            message: String::new(),
        };
        // N112 is a declared advisory; every other code here is an error.
        let violations = vec![make("N001", 1), make("N112", 2), make("N109", 3)];
        let (errors, warnings) = severity_counts(&violations);
        assert_eq!(errors, 2, "N001 and N109 are errors");
        assert_eq!(warnings, 1, "N112 is the one advisory");
        assert_eq!(errors + warnings, violations.len());
    }

    #[test]
    fn diagnostic_line_carries_the_severity_marker_and_optional_code() {
        let with_code = diagnostic_line(
            rules::Severity::Error,
            "t.md:3",
            Some("N001"),
            "something broke",
        );
        assert!(with_code.contains("error:"), "{with_code}");
        assert!(with_code.contains("N001"), "{with_code}");
        let without_code =
            diagnostic_line(rules::Severity::Warning, "t.yaml:1:7", None, "parse error");
        assert!(without_code.contains("warning:"), "{without_code}");
    }

    #[test]
    fn strip_frontmatter_returns_the_body_after_the_closing_marker() {
        let with_frontmatter = "---\nkind: letter\n---\n\nBody starts here.";
        assert_eq!(strip_frontmatter(with_frontmatter), "Body starts here.");
        assert_eq!(
            strip_frontmatter("no frontmatter at all"),
            "no frontmatter at all"
        );
        // A closer at EOF with no body yields an empty body.
        assert_eq!(strip_frontmatter("---\nkind: letter\n---"), "");
        // An unterminated opener is not frontmatter: the whole text stays.
        assert_eq!(strip_frontmatter("---\nkind: letter"), "---\nkind: letter");
    }
}
