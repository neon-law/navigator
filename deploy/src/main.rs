//! `navigator-deploy`: the operator CLI. The local KIND loop (`dev`),
//! production operations (`ops`), and every site, mail, notation, and firm
//! command outside the lean `navigator`. Built for Linux first; it never
//! ships in the Homebrew archive. Projects and their repositories live in
//! the `cli` library, which this binary reuses.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use chrono::TimeZone as _;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

mod assets;
mod assumptions;
mod blog;
mod clauses;
mod cut_release;
mod devx;
mod downloads;
mod firm_admin;
mod firm_forgejo;
mod firms_doctor;
mod forms_render;
mod forms_sync;
mod glossary;
mod import;
mod inbox;
mod inbox_import;
mod journal;
mod letterhead;
mod lsp_download;
mod lsp_publish;
mod mail;
mod mcp_bridge;
mod nautilus_preview;
mod notation_import;
mod notation_kinds;
mod notations_preview;
mod notices;
mod ops_private_terms;
mod postgres_backfill;
mod postgres_mirror;
mod project_preview;
mod public_snapshot;
mod release_check;
mod release_default_tag;
mod release_pins;
mod release_version;
mod sas;
mod scraper;
mod sendgrid_openapi;
mod site_asset;
mod ssh_keys;
mod surreal_archive;
mod video_prepare;

#[cfg(test)]
use cli::commands::help_headline;
use cli::commands::{
    cli_version, concise_help, fix_directory, parse_answer, parse_asset_kind, parse_calendar_date,
    parse_document_visibility, parse_evidence_confidence, parse_matter_folder, parse_notarization,
    parse_signing_method, parse_transcript_quality, print_license, print_third_party_notices,
    print_violation, published_cli_version, run_projects, run_validate, severity_counts,
    strip_frontmatter, HostOpt, ProjectGateMarkdownFilter, ProjectsCmd, CLI_STACK_BYTES,
    DOCUMENT_TRANSCRIBE_HELP, DOCUMENT_UPLOAD_KIND_HELP,
};
use cli::{
    answers_file, atomic_directory, authorities, credentials, document_layout, document_read,
    document_sync, legal_plans, login, palette, projects, release, remote,
};
use devx::brand::BrandCmd;
use devx::{CertificatesCmd, DnsCmd, GcpCmd, HostCmd, RestateCmd, StagingAction, WorktreeEnvCmd};

#[derive(Parser)]
#[command(
    name = "navigator-deploy",
    version = cli_version(),
    about = "Navigator Deploy CLI, not legal advice.",
    long_about = "Navigator Deploy CLI, not legal advice."
)]
struct Cli {
    /// Print the licence this binary is distributed under, then exit. Stands
    /// alone, like `--version`.
    #[arg(long, exclusive = true)]
    license: bool,
    /// Print the licence notices for the third-party open-source components
    /// this binary incorporates, then exit. Stands alone.
    #[arg(long, exclusive = true)]
    third_party_notices: bool,
    /// Optional only so `--license` can stand alone; every other invocation
    /// still requires one, and a bare `navigator-deploy` prints help and exits 2.
    #[command(subcommand)]
    command: Option<Command>,
}

#[cfg(test)]
mod help_tests {
    use super::*;

    #[test]
    fn project_creation_requires_firm_and_brand_together() {
        for base in [
            vec![
                "navigator",
                "project",
                "create",
                "--name",
                "Sample",
                "--code",
                "sample",
                "--client-email",
                "client@example.com",
                "--entity-name",
                "Sample LLC",
                "--attest",
            ],
            vec!["navigator", "project", "scaffold"],
        ] {
            assert!(Cli::try_parse_from(
                base.iter().copied().chain(["--firm", "Example Practice"])
            )
            .is_err());
            assert!(
                Cli::try_parse_from(base.iter().copied().chain(["--brand", "sample"])).is_err()
            );
            assert!(Cli::try_parse_from(base.iter().copied().chain([
                "--firm",
                "Example Practice",
                "--brand",
                "sample"
            ]))
            .is_ok());
        }
    }

    #[test]
    fn ship_cloud_selection_is_explicit_and_excludes_a_local_directory() {
        let args = [
            "navigator",
            "ops",
            "ship",
            "--deployment",
            "example-deployment",
            "--project-id",
            "example-project",
            "--tag",
            "26.10.21-rc.1",
            "--dry-run",
        ];
        let cli = Cli::try_parse_from(args).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Ops(OpsCmd::Ship {
                project_id: Some(_),
                ..
            }))
        ));
        assert!(
            Cli::try_parse_from(args.into_iter().chain(["--deployments-dir", "/unused"])).is_err()
        );
        let readiness = [
            "navigator",
            "ops",
            "brand-readiness",
            "--deployment",
            "example-deployment",
            "--project-id",
            "example-project",
        ];
        assert!(matches!(
            Cli::try_parse_from(readiness).unwrap().command,
            Some(Command::Ops(OpsCmd::BrandReadiness {
                project_id: Some(_),
                ..
            }))
        ));
        assert!(Cli::try_parse_from(
            readiness
                .into_iter()
                .chain(["--deployments-dir", "/unused"])
        )
        .is_err());
        let register = [
            "navigator",
            "ops",
            "restate",
            "register",
            "--deployment",
            "example-deployment",
            "--project-id",
            "example-project",
        ];
        assert!(Cli::try_parse_from(register).is_ok());
        assert!(Cli::try_parse_from(&register[..6]).is_err());
        assert!(
            Cli::try_parse_from(register.into_iter().chain(["--url", "https://w.example/"]))
                .is_err()
        );
        assert!(Cli::try_parse_from([
            "navigator",
            "ops",
            "ship",
            "--project-id",
            "example-project"
        ])
        .is_err());
    }

    #[test]
    fn publishing_configuration_requires_an_explicit_deployment_name() {
        assert!(Cli::try_parse_from(["navigator", "ops", "deployments", "--publish"]).is_err());
        assert!(Cli::try_parse_from([
            "navigator",
            "ops",
            "deployments",
            "--publish",
            "example-deployment"
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "navigator",
            "ops",
            "deployments",
            "import",
            "example-deployment",
            "--deployments-dir",
            "/tmp/example-root",
        ])
        .is_ok());
        // `--publish` is the top-level alias; it does not mix with a verb.
        assert!(Cli::try_parse_from([
            "navigator",
            "ops",
            "deployments",
            "--publish",
            "example-deployment",
            "import",
            "example-deployment",
        ])
        .is_err());
    }

    #[test]
    fn deployments_init_requires_both_capability_flags() {
        let init = [
            "navigator",
            "ops",
            "deployments",
            "init",
            "example-row",
            "--project-id",
            "example-project",
            "--location",
            "example-region",
            "--github-automation-home",
            "false",
            "--sms-test-recipients",
            "true",
            "--set",
            "NAVIGATOR_PUBLIC_HOST=navigator.example.invalid",
            "--set",
            "NAV_BASE_URL=https://navigator.example.invalid/?a=b",
        ];
        let Ok(Cli {
            command:
                Some(Command::Ops(OpsCmd::Deployments {
                    action:
                        Some(DeploymentsCmd::Init {
                            github_automation_home,
                            sms_test_recipients,
                            set,
                            ..
                        }),
                    ..
                })),
            ..
        }) = Cli::try_parse_from(init)
        else {
            panic!("init parses");
        };
        assert!(!github_automation_home);
        assert!(sms_test_recipients);
        assert_eq!(set[1].1, "https://navigator.example.invalid/?a=b");
        assert!(Cli::try_parse_from(&init[..11]).is_err());
        assert!(
            Cli::try_parse_from(
                init.iter()
                    .map(|arg| if *arg == "true" { "maybe" } else { arg })
            )
            .is_err()
        );
    }

    #[test]
    fn deployments_set_and_unset_take_coordinates_and_key_names() {
        assert!(Cli::try_parse_from([
            "navigator",
            "ops",
            "deployments",
            "set",
            "example-row",
            "--project-id",
            "example-project",
            "NAVIGATOR_LOGS_BUCKET=example-logs",
            "--secret-key",
            "HCAPTCHA_SECRET",
            "--postgres-key",
            "NAVIGATOR_POSTGRES_PASSWORD",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "navigator",
            "ops",
            "deployments",
            "set",
            "example-row",
            "--project-id",
            "example-project",
            "NOT_AN_ASSIGNMENT",
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "navigator",
            "ops",
            "deployments",
            "unset",
            "example-row",
            "--project-id",
            "example-project",
            "NAVIGATOR_LOGS_BUCKET",
            "--forgejo-key",
            "NAVIGATOR_FORGEJO_SECRET_KEY",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "navigator",
            "ops",
            "deployments",
            "set",
            "example-row",
            "NAVIGATOR_LOGS_BUCKET=example-logs",
        ])
        .is_err());
    }

    #[test]
    fn record_backed_operator_commands_take_project_id_not_a_tree() {
        for command in [
            &[
                "navigator",
                "ops",
                "secrets",
                "apply",
                "--deployment",
                "example-row",
            ][..],
            &[
                "navigator",
                "ops",
                "observability",
                "--deployment",
                "example-row",
            ][..],
            &[
                "navigator",
                "ops",
                "logs",
                "--deployment",
                "example-row",
                "--run",
                "1-1",
            ][..],
        ] {
            let with_project = command
                .iter()
                .copied()
                .chain(["--project-id", "example-project"]);
            assert!(
                Cli::try_parse_from(with_project.clone()).is_ok(),
                "{command:?}"
            );
            assert!(Cli::try_parse_from(
                with_project.chain(["--deployments-dir", "/tmp/example-root"])
            )
            .is_err());
        }
        assert!(Cli::try_parse_from([
            "navigator",
            "ops",
            "secrets",
            "apply",
            "--deployment",
            "example-row",
            "--project-id",
            "example-project",
            "--check",
        ])
        .is_err());
        // The value is read from stdin; there is no argv slot for it.
        assert!(Cli::try_parse_from([
            "navigator",
            "ops",
            "secrets",
            "set",
            "example-row",
            "--project-id",
            "example-project",
            "HCAPTCHA_SECRET",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "navigator",
            "ops",
            "secrets",
            "set",
            "example-row",
            "--project-id",
            "example-project",
            "HCAPTCHA_SECRET",
            "synthetic-value",
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "navigator",
            "ops",
            "secrets",
            "unset",
            "example-row",
            "--project-id",
            "example-project",
            "HCAPTCHA_SECRET",
        ])
        .is_ok());
    }

    fn assert_headlines(command: &clap::Command) {
        for text in [command.get_about(), command.get_long_about()]
            .into_iter()
            .flatten()
        {
            let text = text.to_string();
            assert!(
                text.split_whitespace().count() <= 10,
                "command `{}` is not terse: {text}",
                command.get_name()
            );
            assert!(text.ends_with('.'));
        }
        for argument in command.get_arguments() {
            for text in [argument.get_help(), argument.get_long_help()]
                .into_iter()
                .flatten()
            {
                let text = text.to_string();
                assert!(
                    text.split_whitespace().count() <= 10,
                    "argument `{}` on `{}` is not terse: {text}",
                    argument.get_id(),
                    command.get_name()
                );
                assert!(text.ends_with('.'));
            }
        }
        for subcommand in command.get_subcommands() {
            assert_headlines(subcommand);
        }
    }

    #[test]
    fn every_cli_help_description_is_a_ten_word_headline() {
        assert_headlines(&concise_help(Cli::command()));
    }

    #[test]
    fn a_notarization_flag_names_all_five_fields_once() {
        let parsed = parse_notarization(
            "method=remote_online,acknowledged_on=2026-09-04,state=NV,county=Clark,\
             commission_expires=2029-01-31",
        )
        .unwrap();
        assert_eq!(
            parsed.method,
            store::documents::NotarizationMethod::RemoteOnline
        );
        assert_eq!(parsed.county, "Clark");

        for (value, expected) in [
            (
                "method=in_person,acknowledged_on=2026-09-04,state=NV,county=Clark",
                "commission_expires=",
            ),
            (
                "method=in_person,acknowledged_on=2026-09-04,state=NV,county=Clark,\
                 commission_expires=2029-01-31,notary_name=Someone",
                "unknown notarization key `notary_name`",
            ),
            (
                "method=in_person,method=in_person,acknowledged_on=2026-09-04,state=NV,\
                 county=Clark,commission_expires=2029-01-31",
                "appears twice",
            ),
            (
                "method=by_mail,acknowledged_on=2026-09-04,state=NV,county=Clark,\
                 commission_expires=2029-01-31",
                "`in_person` or `remote_online`",
            ),
            (
                "method=in_person,acknowledged_on=2026-09-04,state=Nevada,county=Clark,\
                 commission_expires=2029-01-31",
                "two-letter code",
            ),
            (
                "method=in_person,acknowledged_on=2026-09-04,state=NV,county=Clark,\
                 commission_expires=2026-09-03",
                "must not expire",
            ),
        ] {
            let error = parse_notarization(value).unwrap_err();
            assert!(error.contains(expected), "{value}: {error}");
        }
    }

    #[test]
    fn project_set_requires_firm_and_brand_together() {
        let command = concise_help(Cli::command());
        assert!(command
            .clone()
            .try_get_matches_from([
                "navigator",
                "project",
                "set",
                "sample-project",
                "--firm",
                "Example Firm",
                "--brand",
                "neon",
            ])
            .is_ok());
        assert!(command
            .clone()
            .try_get_matches_from([
                "navigator",
                "project",
                "set",
                "sample-project",
                "--firm",
                "Example Firm"
            ])
            .is_err());
        assert!(command
            .try_get_matches_from([
                "navigator",
                "project",
                "set",
                "sample-project",
                "--brand",
                "neon"
            ])
            .is_err());
    }

    #[test]
    fn project_repository_sync_skills_is_a_supported_command() {
        let command = concise_help(Cli::command());
        assert!(command
            .clone()
            .try_get_matches_from([
                "navigator",
                "project",
                "repository",
                "sync-skills",
                "--dir",
                ".",
            ])
            .is_ok());
        let help = command
            .try_get_matches_from([
                "navigator",
                "project",
                "repository",
                "sync-skills",
                "--help",
            ])
            .unwrap_err()
            .to_string();
        assert!(help.contains("canonical Agent Skills"), "{help}");
    }

    #[test]
    fn mail_read_draft_and_file_are_dispatched_commands() {
        let command = concise_help(Cli::command());
        assert!(command
            .clone()
            .try_get_matches_from(["navigator", "mail", "search", "client"])
            .is_ok());
        assert!(command
            .clone()
            .try_get_matches_from(["navigator", "mail", "read", "thread-1"])
            .is_ok());
        assert!(command
            .clone()
            .try_get_matches_from([
                "navigator",
                "mail",
                "draft",
                "--to",
                "client@example.test",
                "--subject",
                "Hello",
                "--body-file",
                "draft.txt",
            ])
            .is_ok());
        assert!(command
            .try_get_matches_from([
                "navigator",
                "mail",
                "file",
                "thread-1",
                "--project",
                "matter"
            ])
            .is_ok());
    }

    #[test]
    fn headline_preserves_the_first_sentence() {
        assert_eq!(
            help_headline("One two three. Four five six."),
            "One two three."
        );
    }

    #[test]
    fn notation_render_timestamp_uses_source_commit_or_fixed_fallback() {
        let repository = tempfile::tempdir().expect("temporary repository");
        let source = repository.path().join("notation.md");
        std::fs::write(&source, "source").expect("write source");
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(repository.path())
                .args(args)
                .output()
                .expect("git runs");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "--quiet", "--initial-branch=main"]);
        git(&["config", "user.email", "notation-render@example.com"]);
        git(&["config", "user.name", "notation render"]);
        git(&["config", "commit.gpgsign", "false"]);
        git(&["add", "notation.md"]);
        let commit = std::process::Command::new("git")
            .arg("-C")
            .arg(repository.path())
            .args(["commit", "--quiet", "-m", "source"])
            .env("GIT_AUTHOR_DATE", "2024-01-02T03:04:05Z")
            .env("GIT_COMMITTER_DATE", "2024-01-02T03:04:05Z")
            .output()
            .expect("git commit runs");
        assert!(commit.status.success());

        assert_eq!(
            notation_render_timestamp(&source).timestamp(),
            chrono::DateTime::parse_from_rfc3339("2024-01-02T03:04:05Z")
                .expect("fixed test timestamp")
                .timestamp()
        );

        let non_git = tempfile::tempdir().expect("temporary input directory");
        let untracked = non_git.path().join("notation.md");
        std::fs::write(&untracked, "source").expect("write untracked source");
        assert_eq!(
            notation_render_timestamp(&untracked).timestamp(),
            NOTATION_RENDER_FALLBACK_EPOCH
        );
    }
}

/// The live clap tree `navigator-deploy` dispatches, for tests that render a
/// subcommand's help. Outside tests the `cli` library's `navigator_command`
/// (the lean tree) is the one the gate reads.
#[cfg(test)]
fn navigator_command() -> clap::Command {
    Cli::command()
}

// These Clap enums live only for the duration of one CLI invocation. Their
// explicit variants keep operator help and dispatch exhaustive and readable.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum Command {
    /// Scan tracked repository text for personal data with a local Presidio analyzer.
    Pii {
        /// Repository root containing .github/pii/allowlist.toml.
        #[arg(default_value = ".")]
        dir: PathBuf,
        /// Loopback-only analyzer origin; repository text never goes to a remote service.
        #[arg(long, default_value = "http://127.0.0.1:3000")]
        analyzer_url: String,
    },
    /// Run all of Navigator on this machine: check the host tools, bring up
    /// a KIND cluster with PostgreSQL and Forgejo on this checkout's own
    /// ports, seed the sample firm and matters, and open the website.
    ///
    /// Outside a Navigator checkout (a Homebrew install), it clones the
    /// source into `~/.navigator/quickstart` and runs from there. Re-run it
    /// after a reboot; `dev worktree-env down` removes the cluster.
    Quickstart,
    /// Firm-owned exact-quantity journal and bounded reconciliation commands.
    Journal {
        #[arg(long)]
        firm: uuid::Uuid,
        #[arg(long)]
        host: Option<String>,
        #[command(subcommand)]
        action: journal::JournalCmd,
    },
    /// A Firm's Legal Plans: where each is logged into, who to call, and its
    /// service levels.
    LegalPlans {
        /// Firm UUID, or a display name visible in the authorized Firm directory.
        #[arg(long)]
        firm: String,
        #[arg(long)]
        host: Option<String>,
        #[command(subcommand)]
        action: legal_plans::LegalPlansCmd,
    },
    /// A Firm's stated premises the CEO Digest checks actuals against.
    Assumptions {
        #[arg(long)]
        firm: uuid::Uuid,
        #[arg(long)]
        host: Option<String>,
        #[command(subcommand)]
        action: assumptions::AssumptionsCmd,
    },
    /// Search, read, draft, and file Gmail correspondence without sending it.
    Mail {
        #[command(subcommand)]
        action: mail::MailAction,
    },
    Inbox {
        #[command(subcommand)]
        action: inbox::InboxAction,
    },
    /// The legal files sitting in the lawyer's Downloads folder: scan them,
    /// propose a matter, kind, and conventional slug for each, file one
    /// onto its matter, and report what is already filed.
    Downloads {
        #[command(subcommand)]
        action: downloads::DownloadsCmd,
    },
    // ─────────────── Authoring a notation locally ───────────────
    // The notation author's workbench: everything here runs offline (or
    // against a local store), no live site required.
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
    /// Inspect the versioned legal Clause library.
    Clause {
        #[command(subcommand)]
        action: ClauseCmd,
    },
    /// Read the Neon Law Navigator glossary — the ontology, one term per
    /// file under `docs/glossary/`, embedded in this binary.
    Glossary {
        #[command(subcommand)]
        action: GlossaryCmd,
    },
    /// Interact with a project hosted on a navigator site.
    ///
    /// Singular because a checkout is one Project — the repository name *is* the Project code.
    #[command(name = "project")]
    Projects {
        #[command(subcommand)]
        action: ProjectsCmd,
    },
    /// Scrape court and program portals into the omnichannel inbox. Site
    /// adapters run on the shared framework base: pinned-Chrome sessions,
    /// Firm-store portal logins, resumable downloads, inbox intake.
    Scraper {
        #[command(subcommand)]
        action: scraper::ScraperCmd,
    },
    /// The notation author's workbench: a Project repository's
    /// `templates/<stem>.md` (the fleet layout), or Navigator's own bundled
    /// catalog under `templates/notations/`.
    #[command(visible_aliases = ["drafts", "templates"])]
    Notation {
        #[command(subcommand)]
        action: NotationCmd,
    },
    /// Vendor, pin, and inspect the blank government forms in the public assets bucket
    /// (`templates/notations/forms/`).
    Forms {
        #[command(subcommand)]
        action: FormsAction,
    },
    /// Drive a running deployment with the bearer token `navigator site login` stores.
    Site {
        #[command(subcommand)]
        action: SiteCmd,
    },
    /// Read and update a Person in the deployment's own store, directly.
    ///
    /// Distinct from `navigator site …` verbs, which speak HTTP to a running
    /// site: these open the `SurrealDB` coordinates `NAVIGATOR_SURREAL_*`
    /// names — the same handle the web app uses.
    Person {
        #[command(subcommand)]
        action: PersonCmd,
    },
    /// Download the `navigator-lsp` release archive matching this binary's
    /// own version and the host platform, and write a working
    /// `navigator-lsp` executable to the Downloads directory.
    ///
    /// Resolves the exact `navigator-lsp-<tag>-<platform>` archive
    /// `.github/workflows/deploy.yml` attaches to *this* binary's own
    /// GitHub Release — never "latest" — via `views::lsp::LSP_RELEASE_ARCHIVES`.
    /// A version mismatch or missing release asset is refused with the tag
    /// it looked for, not a silent fallback.
    ///
    /// Distinct from `ops lsp publish` (the operator upload side) and from
    /// the Zed extension's own runtime resolution — this is the door a
    /// human without operator access uses to get the binary locally. See
    /// `docs/lsp/README.md`.
    Lsp {
        /// Directory to write the extracted `navigator-lsp` executable
        /// into. Defaults to the platform's Downloads directory.
        #[arg(long)]
        dir: Option<PathBuf>,
    },

    /// The Nautilus mobile shell, served locally for a browser.
    Nautilus {
        #[command(subcommand)]
        action: nautilus_preview::NautilusCmd,
    },

    /// Render a Project checkout's declared pages on localhost, under a draft
    /// banner, reloading on each save. Reads section data from the host
    /// with the stored login, read only; never pushes anything to it.
    Preview {
        /// Port to serve on. `0` picks a free one.
        #[arg(long, default_value_t = 0)]
        port: u16,
    },

    // ─────────────── Operator ───────────────
    /// Local, reversible KIND developer loop.
    #[command(subcommand)]
    Dev(DevCmd),
    /// Production and cloud operations with operator blast radius.
    #[command(subcommand)]
    Ops(OpsCmd),
}

#[derive(Subcommand)]
enum NotationCmd {
    /// Push one template as a **draft** to the Project it belongs to and
    /// open that Project's real portal at it — the production renderer,
    /// production chrome, and production questionnaire engine, rather
    /// than a second local imitation that has to be kept in step with it.
    ///
    /// Reads the Project and the deployment host from `navigator.yaml`
    /// two directories up, the way `navigator project gate` does. There
    /// is no `--project` to pass and no `--host` to require: a template
    /// is always previewed as the Project whose repository it sits in,
    /// against the host that repository targets. Refuses outside a
    /// Project repository rather than falling back to a local render.
    ///
    /// The push opens a **real notation run** on the matter (LAW-203):
    /// the pushed source is persisted and validated like any other
    /// template version, the engagement-first rule applies, and the
    /// questionnaire's state machine starts — the draft page hands off to
    /// it. What a preview never does is anything a signature or a filing
    /// does: no document is sent, signed, or filed by previewing.
    Preview {
        /// The template to push: a path, or a notation `code` looked up
        /// under `templates/` and then `templates/notations/`. Underscores
        /// and hyphens are interchangeable in a name.
        file: PathBuf,
        /// Lint locally instead of pushing a draft. A **lint**, not a
        /// preview: it prints every blocking rule violation the gate and a
        /// draft push would report and exits non-zero on any. A clean
        /// template then renders locally until Ctrl-C; nothing is pushed
        /// and nothing is stored, and the
        /// questionnaire steps only with a Dioxus client bundle
        /// (`navigator-deploy dev build-webapp`) — without one the page still
        /// renders every question, it just does not advance. Works
        /// outside a Project repository, since nothing is pushed anywhere.
        /// Always binds an OS-assigned port chosen at random — the bound
        /// URL is printed either way — so two lints can run at once.
        #[arg(long)]
        offline: bool,
        /// Override the deployment host `navigator.yaml` names — for
        /// previewing against a non-production deployment. `navigator.yaml`
        /// is still the source; this is an override, not a replacement for
        /// it. Ignored with `--offline`.
        #[arg(long)]
        host: Option<String>,
    },
    /// List every `kind:` and `output:` value a notation may declare, with
    /// the frame each gets (letterhead, plain, contract, pleading, form).
    Kinds,
    /// Render a single notation template to PDF, framed by the render
    /// profile its declared `kind:` selects.
    ///
    /// The file is validated against the same notation rule set as
    /// `validate` first — a template with any violation is refused.
    ///
    /// A notation already declares what it is, and that is enough to pick
    /// the frame: `Kind::default_output` derives it from `kind:`, so a
    /// `letter` gets letterhead and a `will` gets the unadorned
    /// instrument. A template's own `output:` frontmatter field remains
    /// the deliberate override for the kinds that legitimately render two
    /// ways, and omitting `output:` is how a template selects the plain
    /// frame. There is no `--format` flag: it chose the frame a second
    /// time, from outside the document, and `--format letter` passed out
    /// of habit put the firm's letterhead on an executed instrument with
    /// no warning (LAW-15).
    ///
    /// A `kind: pleading` template is the one case that needs a second
    /// field: court geometry is calibrated by its `jurisdiction:`. That is
    /// resolved here, and a jurisdiction with no calibration is refused
    /// rather than quietly rendered on the plain frame.
    ///
    /// Compiled in pure Rust (no shell-out). Every render is final unless
    /// `--preview` is passed: a document that leaves Navigator carries no
    /// unanswered `{{placeholder}}` (fill one with `--answer code=value`),
    /// bracketed drafting marker, stray fill-in blank, or drafting note.
    #[command(after_long_help = notation_kinds::RENDER_FRAME_HELP)]
    Pdf {
        /// Path to the notation template (`.md`).
        file: PathBuf,
        /// Where to write the rendered PDF. Must end in `.pdf`.
        #[arg(long)]
        out: PathBuf,
        /// Fill a `{{code}}` placeholder with `value`. Repeatable:
        /// `--answer counterparty_legal_name="NEON GmbH"`.
        #[arg(long = "answer", value_parser = parse_answer)]
        answers: Vec<(String, String)>,
        /// Load questionnaire Answers from a local YAML file.
        #[arg(long)]
        answers_file: Option<PathBuf>,
        /// Reuse Answers from the latest Notation for this Draft or a Notation UUID.
        #[arg(long, conflicts_with = "answers_file")]
        answers_from: Option<String>,
        /// Render a draft for review: unanswered placeholders survive,
        /// highlighted. Without it the render is final and refuses any
        /// unanswered placeholder, bracketed drafting marker, fill-in blank
        /// outside a signature block, or drafting note.
        #[arg(long)]
        preview: bool,
        /// Refuse to fetch render inputs over the network: render only
        /// from bytes already pinned in the checkout or the local blank
        /// cache (LAW-19). A form template whose blank is neither vendored
        /// nor cached is a loud refusal, never a silent download that
        /// could change the bytes between two runs.
        #[arg(long)]
        ci: bool,
    },
    /// Render a single notation template to editable Word, framed the same
    /// way as `pdf` — see its documentation for how the frame, jurisdiction,
    /// and placeholders resolve.
    #[command(after_long_help = notation_kinds::RENDER_FRAME_HELP)]
    Word {
        /// Path to the notation template (`.md`).
        file: PathBuf,
        /// Where to write the rendered Word document. Must end in `.docx`.
        #[arg(long)]
        out: PathBuf,
        /// Fill a `{{code}}` placeholder with `value`. Repeatable:
        /// `--answer counterparty_legal_name="NEON GmbH"`.
        #[arg(long = "answer", value_parser = parse_answer)]
        answers: Vec<(String, String)>,
        /// Render a draft for review: unanswered placeholders survive,
        /// highlighted. Without it the render is final and refuses any
        /// unanswered placeholder, bracketed drafting marker, fill-in blank
        /// outside a signature block, or drafting note.
        #[arg(long)]
        preview: bool,
        /// Write the render as a **redline** against this previous `.docx`:
        /// every difference from it becomes a real Word tracked change
        /// (`w:ins`/`w:del`) in the output, for the lawyer to accept or
        /// reject in Word before the version is delivered. The previous
        /// document must be a fresh `notation word` export carrying no
        /// unresolved revisions or review comments of its own. Pairs with
        /// `notation import`, which still refuses any document carrying the
        /// revisions this writes — a redline is for review, never for
        /// splicing.
        #[arg(long, value_name = "PREVIOUS.docx")]
        redline_against: Option<PathBuf>,
        /// The name the tracked changes record as their author. Defaults to
        /// `matter.counsel.name` in `navigator.yaml` — the same name the
        /// `/s/` signature line signs with.
        #[arg(long, value_name = "NAME")]
        redline_author: Option<String>,
    },
    /// Apply an edited Word document back into the notation template it was
    /// rendered from — the second half of `notation word`.
    ///
    /// The edited `.docx` carries the export's provenance record; the
    /// import verifies the template hash and refuses a template that
    /// changed since the export, refuses drafts still carrying tracked
    /// changes or review comments, and refuses any edit that drops a
    /// `{{placeholder}}` or `{{> partial}}` the template needs or invents
    /// one it never had. Tables are carried verbatim and never touched.
    /// With `--dry-run` the plan and the updated template print and
    /// nothing is written.
    Import {
        /// Path to the notation template (`.md`) the document was rendered
        /// from. Updated in place without `--dry-run`.
        file: PathBuf,
        /// Path to the edited Word document (`navigator-deploy notation word`'s
        /// output, edited in Word and re-saved). Must end in `.docx`.
        edited: PathBuf,
        /// Print the plan and the updated template without writing
        /// anything.
        #[arg(long)]
        dry_run: bool,
    },
}

/// Manage a Person directly in the store.
#[derive(Subcommand)]
enum PersonCmd {
    /// Read and update a Person's phone numbers (E.164, labeled, one
    /// primary). Numbers are keyed by their canonical E.164 spelling, so
    /// `702 555 0142` and `+17025550142` name the same row.
    Phone {
        #[command(subcommand)]
        action: PersonPhoneCmd,
    },
}

#[derive(Subcommand)]
enum PersonPhoneCmd {
    /// List a Person's phone numbers, primary first, then in the order
    /// they were written.
    List {
        /// Person UUID, or a Person email (primary or alias).
        person: String,
    },
    /// Set one of a Person's phone numbers, creating or updating its
    /// row. The first number a Person has becomes primary;
    /// `--primary` moves it, and any earlier primary is demoted in the
    /// same write.
    Set {
        /// Person UUID, or a Person email (primary or alias).
        person: String,
        /// The number, in any spelling the E.164 normalizer accepts
        /// (`702 555 0142`, `(702) 555-0142`, `+44 20 7946 0958`).
        #[arg(long)]
        number: String,
        /// Label: `mobile`, `work`, `home`, `fax`, or `other`.
        #[arg(long)]
        label: Option<String>,
        /// Make this the Person's primary number.
        #[arg(long)]
        primary: bool,
    },
    /// Remove one of a Person's phone numbers. Removing a number that
    /// is not there succeeds quietly.
    Remove {
        /// Person UUID, or a Person email (primary or alias).
        person: String,
        /// The number to remove, in any accepted spelling.
        #[arg(long)]
        number: String,
    },
}

#[derive(Subcommand)]
enum SitePersonCmd {
    /// Merge a duplicate Person into the Person to keep.
    ///
    /// An address change imported without `previous_email:` leaves two
    /// Persons for one human. This moves every relationship the dropped
    /// Person holds (Conflict-Check Graph edges, Project participation,
    /// Relationship Log rows) to the kept Person, keeps both email
    /// addresses on the kept Person as aliases, and deletes the duplicate
    /// last. Only a client Person can be dropped.
    Merge {
        /// Email (primary or alias) of the Person to keep.
        keep: String,
        /// Email (primary or alias) of the duplicate to remove.
        drop: String,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Manage your own SSH public keys. Navigator adds them to your account
    /// on the installation's Forgejo and removes them when you remove them
    /// here or leave your Firm. Only ed25519 and ECDSA public keys are
    /// accepted; a private key is refused before it leaves this machine.
    SshKey {
        #[command(subcommand)]
        action: SshKeyCmd,
    },
}

#[derive(Subcommand)]
enum SshKeyCmd {
    /// Register one public key, read from a `.pub` file (or `-` for stdin).
    Add {
        /// The public key file, for example `~/.ssh/id_ed25519.pub`.
        file: PathBuf,
        /// A label for the key; defaults to the key's comment.
        #[arg(long)]
        label: Option<String>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// List your keys by id, fingerprint, type, and label.
    List {
        /// Another Person's id, whose keys you may revoke as their Firm's
        /// administrator.
        #[arg(long)]
        person: Option<uuid::Uuid>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Revoke a key by id or fingerprint; it is removed from Forgejo too.
    Remove {
        /// The key id or `SHA256:` fingerprint `list` prints.
        key: String,
        /// Another Person's id, whose key you may revoke as their Firm's
        /// administrator.
        #[arg(long)]
        person: Option<uuid::Uuid>,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Reconcile your Forgejo account now, for example after your first
    /// Forgejo sign-in.
    Sync {
        #[command(flatten)]
        host: HostOpt,
    },
}

#[derive(Subcommand)]
enum SiteFirmCmd {
    /// Create or reconcile the Firm's Forgejo organization.
    ///
    /// The deployment's provisioning identity creates the private
    /// organization the Firm's slug names, and adds the account the Firm's
    /// own Forgejo access token authenticates to its owners team, so the
    /// Firm's Project repositories can be created or copied there without
    /// anyone holding site administration. Safe to re-run: an existing
    /// organization and owner are left as they are. Owner, or an Admin of
    /// the Firm.
    ProvisionForgejo {
        /// The Firm's slug, as `GET /app/api/firms` lists it.
        slug: String,
        #[command(flatten)]
        host: HostOpt,
    },
    /// List the Firms this login may administer: slug, name, and the Brands
    /// each wears, one Firm per tab-separated line.
    List {
        #[command(flatten)]
        host: HostOpt,
    },
    /// Rename a Firm. Its slug, and the Forgejo organization the slug names,
    /// is unchanged. Owner, or an Admin of the Firm.
    Rename {
        /// The Firm's slug, as `navigator-deploy site firm list` prints it.
        slug: String,
        /// The Firm's new display name.
        name: String,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Delete a Firm and its memberships. Refused while the Firm owns any
    /// Project or Brand: move each Brand with `navigator-deploy site brand move`
    /// first. Owner, or an Admin of the Firm.
    Delete {
        /// The Firm's slug, as `navigator-deploy site firm list` prints it.
        slug: String,
        #[command(flatten)]
        host: HostOpt,
    },
}

#[derive(Subcommand)]
enum SiteBrandCmd {
    /// Move a Brand to another Firm, so its public pages name that Firm's
    /// legal entity. Owner only.
    Move {
        /// The Brand's key, as `navigator-deploy site firm list` prints it.
        key: String,
        /// The slug of the Firm the Brand moves to.
        #[arg(long)]
        to: String,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Stop the Firm that wears a Brand from wearing it. The Brand row
    /// remains. Owner, or an Admin of that Firm.
    Detach {
        /// The Brand's key, as `navigator-deploy site firm list` prints it.
        key: String,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Delete a Brand. Refused while any Firm wears it (`detach` it first)
    /// or any Project names it. Owner, or the owning Firm's Admin DRI.
    Delete {
        /// The Brand's key.
        key: String,
        #[command(flatten)]
        host: HostOpt,
    },
}

#[derive(Subcommand)]
enum SiteCmd {
    /// Publish Brand Posts and record social links through a site login.
    Blog {
        #[arg(long, global = true)]
        host: Option<String>,
        #[command(subcommand)]
        action: BlogCmd,
    },
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
    /// Manage Persons through the logged-in deployment.
    Person {
        #[command(subcommand)]
        action: SitePersonCmd,
    },
    /// Manage a Firm through the logged-in deployment.
    Firm {
        #[command(subcommand)]
        action: SiteFirmCmd,
    },
    /// Move, detach, or delete a Brand through the logged-in deployment.
    Brand {
        #[command(subcommand)]
        action: SiteBrandCmd,
    },
    /// File, read, and verify a Project's documents — every document is
    /// scoped to one Project, and every Project to one brand deployment
    /// (`--host`, defaulting to the sole stored login).
    Document {
        #[command(subcommand)]
        action: DocumentAction,
    },
    /// File a global Authority — the citation apparatus' shared legal
    /// reference data (#890) — with an archived artifact. Unlike `document`,
    /// an Authority carries no `--project`.
    Authorities {
        #[command(subcommand)]
        action: AuthoritiesAction,
    },
    /// Publish one local public-safe asset (a brand mark, a hero image, a
    /// font file) to a deployment's public assets bucket
    /// (`POST /app/api/assets`). Unlike `document`, a public asset carries
    /// no `--project`: it is deployment-wide, not matter-scoped, and needs
    /// no bucket name or GCP credential — only a `navigator site login`.
    Asset {
        #[command(subcommand)]
        action: AssetAction,
    },
    /// File an inbound email's attachments on a live site.
    Mail {
        #[command(subcommand)]
        action: MailAction,
    },
    /// Authenticate to a live Neon Law Navigator site via a browser-loopback
    /// flow and store a short-lived (1h) bearer token at
    /// `~/.navigator.json` (mode `0600`).
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
    /// Seed the workspace-owned template and question catalog from clean files.
    Seed {
        /// Directory to walk.
        dir: PathBuf,
    },
    /// Plan or apply the versioned synthetic staging portfolio fixture:
    /// stable synthetic people, entity, matter, document, invoice mirror,
    /// trust movement, IOLTA pool, allocation, and portal bundle
    /// (`store::synthetic_portfolio`). Connects to whatever store and
    /// storage the environment names — the same seam `site seed` uses —
    /// so applying to persistent staging is an explicit operator action
    /// with the right `NAVIGATOR_SURREAL_*`/storage env pointed there.
    ///
    /// Refuses before any write unless `--target staging` is given exactly
    /// and the deployment has already disclosed
    /// `NAVIGATOR_SIMULATED_MATTERS=true`; see `docs/environments.md`.
    /// Idempotent: a repeat `--apply` inserts nothing new.
    SyntheticPortfolio {
        /// The only accepted value is `staging`.
        #[arg(long)]
        target: Option<String>,
        /// Perform the writes. Omit for a dry-run report with zero writes.
        #[arg(long)]
        apply: bool,
    },
    /// Forget the stored token for a host (or the sole logged-in host).
    Logout {
        /// Host to log out of. Optional when exactly one host is stored.
        #[arg(long)]
        host: Option<String>,
    },
    /// Print the stored identity and how long the token has left.
    Whoami {
        /// Host to inspect. Optional when exactly one host is stored.
        #[arg(long)]
        host: Option<String>,
    },
    /// Serve the Navigator MCP tool catalog to Claude as a local MCP server over
    /// stdio, dispatching each call to the host's A2A endpoint with the
    /// stored bearer token.
    ///
    /// Claude speaks MCP and has no A2A client; A2A is where the
    /// lawyer-tier check and the `audit` trail live. This bridges the two
    /// so Claude picks the tool and Navigator authorizes it.
    ///
    /// Only tools that run without a human approving them are offered:
    /// every read, plus the CRM writers (person, project, participation,
    /// bulk contact import). A tool that needs an explicit approval is
    /// not advertised, because MCP cannot pause a call to ask a person
    /// and a confirmation the model supplies to itself is not one.
    ///
    /// Speaks protocol on stdout and diagnostics on stderr. Run it from a
    /// client's server configuration, not by hand.
    Mcp {
        /// Host to serve. Optional when exactly one host is stored.
        #[arg(long)]
        host: Option<String>,
    },
    /// Inspect or drive a notation's workflow on a live site.
    Notation {
        #[command(subcommand)]
        action: NotationAction,
    },
}

#[derive(Subcommand)]
enum DevCmd {
    /// Install the pinned host dependencies the native local tier runs
    /// (`SurrealDB`, Restate, `SeaweedFS`) via Homebrew. macOS
    /// only — the cluster lane (`--runtime kind`) is the fallback
    /// everywhere else. Idempotent: a converged host does one `brew
    /// list` and stops, so `worktree-env up` can call it every time.
    Install,
    /// Build the current checkout's workflow worker, load it into the KIND
    /// dependency stack (`SurrealDB`, Rauthy, `SeaweedFS`, Restate, `OpenObserve`),
    /// open host port-forwards, and write `.devx/env` — the
    /// developer-loop entry point for editing `web` on the host.
    Up {
        /// The checkout to bring up. Defaults to the workspace containing the
        /// current directory.
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Kill the port-forwards and delete the KIND cluster.
    Down {
        /// The checkout whose tier to tear down. Defaults to the workspace
        /// containing the current directory.
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Print env vars (one KEY=VALUE per line) for a host-side `web`.
    Env,
    /// Show whether the cluster and port-forwards are up.
    Status,
    /// Rebuild the current checkout's `workflows-service` image, load it
    /// into KIND, and restart the in-cluster worker. Run after changing
    /// worker or shared workflow code; `dev up` does this automatically
    /// while creating a dependency tier.
    WorkerReload,
    /// Serve the local website, rebuild Rust/catalog changes, and refresh browsers.
    Serve,
    /// Build the Dioxus client bundle (issue #641): drive `dx` to compile the
    /// `webapp` crate to `wasm32-unknown-unknown` and stage it under
    /// `server/public/dioxus`, where `web` serves it same-origin to hydrate the
    /// `/dioxus-demo` page. A build artifact — gitignored, never committed;
    /// `images/Containerfile.web` runs this at image build time.
    BuildWebapp {
        /// Build with optimizations (the deploy and CI default). Omit for a
        /// faster debug build during local iteration.
        #[arg(long)]
        release: bool,
    },
    /// Build the Nautilus web preview bundle: drive `dx` to compile the
    /// `nautilus-preview` crate (its own workspace, with its own Dioxus.toml)
    /// to `wasm32-unknown-unknown` and stage it under
    /// `server/public/nautilus`, where `web` serves it same-origin for the
    /// Nautilus mobile-shell preview. A build artifact — gitignored, never
    /// committed.
    BuildNautilus {
        /// Build with optimizations (the deploy default). Omit for a faster
        /// debug build during local iteration.
        #[arg(long)]
        release: bool,
    },
    /// Guarded create/reset/status/down lifecycle for the local KIND staging boundary.
    #[command(subcommand)]
    Staging(StagingAction),
    /// KIND cluster-only helpers.
    #[command(subcommand)]
    Kind(KindCmd),
    /// The opt-in Forgejo in the KIND tier: retained PostgreSQL, Forgejo,
    /// one Actions runner, and sign-in through the tier's Rauthy. See
    /// `docs/forgejo-kind.md`.
    #[command(subcommand)]
    Forgejo(ForgejoCmd),
    /// Per-worktree dev checkout — stand up (or tear down) the host state
    /// scoped to the current git worktree. The default mode runs the current
    /// checkout's worker in KIND and gives each worktree its own host `web`
    /// port; `--demo` runs the full stack in-cluster from published Artifact
    /// Registry images. `up
    /// --branch <topic>` prepares a supplied agent worktree in place or
    /// creates a sibling worktree when no checkout was supplied.
    #[command(subcommand)]
    WorktreeEnv(WorktreeEnvCmd),
    /// Pull published Artifact Registry images, `kind load` them, then
    /// `kubectl apply -k k8s/overlays/kind` — the full stack
    /// including navigator-web. CI publishes the images; this never builds
    /// them. Pin a release with `NAVIGATOR_IMAGE_TAG`, else the
    /// latest published `YY.M.D` tag is pulled. Ends with the
    /// navigator-web rollout settling.
    Deploy,
    /// `kubectl delete namespace navigator`. Removes every Neon Law Navigator
    /// resource without touching the cluster itself.
    Undeploy,
    /// Smoke-test the deployed stack: wait for every rollout, hit
    /// `/health` through the ingress, assert the embedded Rego policy decisions,
    /// and confirm the seed data populated. Native Rust.
    E2e,
    /// Bootstrap `SeaweedFS` against an already-applied stack: create the
    /// `navigator-seaweedfs-s3` Secret with random bucket-scoped credentials
    /// and the startup authentication config, wait for the `StatefulSet`,
    /// then create the buckets. `dev up`/`dev deploy` run this inline;
    /// CI calls it after `kubectl apply -k`.
    SeaweedfsBootstrap,
    /// Pre-seed the Lawyer demo user (`lawyer@neonlaw.com`) with the
    /// `lawyer` role so the browser e2e's admin-gated walk can run.
    /// Native Rust.
    GrantLawyer,
    /// Refresh and stage each sample matter's reference application for the
    /// next local web boot. The checkouts and builds happen in temporary
    /// directories; each built `dist/` and `navigator.yaml` survives under
    /// `.devx/sample-projects/<code>`, and the generated `.devx/env` points
    /// `web` at the parent. Needs `git` and `pnpm`. Native Rust.
    SampleProject {
        /// Refresh only this Project's application. Defaults to all of them;
        /// naming one is the fast loop while iterating on a single app.
        #[arg(long)]
        project: Option<String>,
        /// Repository to clone. Defaults to the URL recorded on the Project;
        /// override to build a fork or a local mirror. Requires `--project`,
        /// since one URL cannot serve every matter.
        #[arg(long, requires = "project")]
        repo: Option<String>,
        /// Branch or tag to build. Defaults to the repository's default
        /// branch.
        #[arg(long = "ref")]
        git_ref: Option<String>,
        /// Keep the temporary checkout and build tree instead of removing
        /// it, to debug a failed build.
        #[arg(long)]
        keep: bool,
    },
    /// Drive the real inbound-email summary pipeline on this worktree's KIND
    /// cluster: signed synthetic webhook requests through the host `web`
    /// listener, the real Restate broker, `SurrealDB`, `SeaweedFS` and `ClamAV`, and
    /// a branch-built `workflows-service` image. Vertex, Gemini and Slack are
    /// task-owned synthetic HTTP fixtures. Fails when `OrbStack`, the cluster,
    /// the branch worker or the `EmailSummary` registration is missing; it
    /// never skips. Bring the cluster up first with
    /// `dev worktree-env up --runtime kind`.
    EmailSummaryE2e,
    /// Reproduce `deploy.yml`'s browser gate locally: resolve the
    /// pinned Chrome for Testing build, start its chromedriver, verify
    /// the host `web` is reachable, grant Lawyer in the worktree store,
    /// then run the `browser_e2e` +
    /// `accessibility_e2e` suites with `NAV_REQUIRE_HARNESS=1` (a
    /// self-skip fails instead of passing green). Bring the fixture up
    /// (`dev worktree-env up`) and start `web` first.
    BrowserE2e {
        /// Base URL of the running web server. Defaults to
        /// `$NAV_BASE_URL`, else `http://localhost:$PORT` from the
        /// worktree's sourced `.devx/env`.
        #[arg(long, env = "NAV_BASE_URL")]
        base_url: Option<String>,
    },
    /// Loop one package's tests N times and print the pass/fail wall-time
    /// distribution. Each run is classified from nextest's Summary line (or
    /// cucumber's `test result:` / scenarios line), never from a pipe's
    /// exit status.
    FlakeHunt {
        /// Cargo package to run (`store`, `portal`, `features`, …).
        package: String,
        /// Optional nextest filter expression (`-E`). For `features`, the
        /// cucumber binary name passed to `cargo test --test`.
        test_filter: Option<String>,
        /// How many consecutive runs to record.
        #[arg(long, default_value_t = 10)]
        runs: u32,
    },
    /// Tail `navigator-web` logs (`kubectl logs -f deployment/navigator-web`).
    Logs,
    /// Render Kubernetes overlays locally.
    #[command(subcommand)]
    Kustomize(KustomizeCmd),
    /// Verify or deterministically regenerate the pinned `SendGrid` Mail API
    /// client input. No schema is fetched during ordinary builds.
    SendgridOpenapi {
        /// Verify the vendored contract and generated adapter (default).
        #[arg(long)]
        verify: bool,
        /// Run the offline regeneration check.
        #[arg(long)]
        regenerate: bool,
        /// Workspace root containing vendor/sendgrid.
        #[arg(default_value = ".")]
        root: PathBuf,
    },
}

#[derive(Subcommand)]
enum ForgejoCmd {
    /// Converge Forgejo into the selected KIND tier: PostgreSQL with its
    /// roles, Forgejo's own login and database, Forgejo, the runner, and a
    /// port-forward at `NAVIGATOR_KIND_FORGEJO_PORT` (default 3300).
    /// Idempotent; generates synthetic credentials once per tier.
    Up,
    /// Prove the tier's Forgejo end to end with synthetic data: TLS and
    /// database isolation, closed registration, git clone/push, an Actions
    /// run, retention across pod replacement and scale-to-zero, and
    /// email-domain admission through Rauthy — and, on a Linux host, a real
    /// push landing as a journaled, analyzed receipt through an in-cluster
    /// receiver.
    Smoke {
        /// Internal: serve the push receiver instead of running the proof.
        /// The smoke stages this same binary and a JSON config inside the
        /// cluster, where Forgejo's delivery can reach it.
        #[arg(hide = true, long)]
        receiver_config: Option<std::path::PathBuf>,
    },
}

#[derive(Subcommand)]
enum KindCmd {
    /// Create the KIND cluster and install nginx-ingress + the
    /// Restate Operator. Does not apply any application manifests.
    /// Use this when you want the cluster prepared but plan to apply
    /// k8s/ manifests by hand.
    Up,
    /// Delete the KIND cluster. Does not touch the local Docker
    /// images or the host port-forward state file.
    Down,
}

#[derive(Subcommand)]
enum KustomizeCmd {
    /// `kubectl kustomize k8s/overlays/kind` — render the full local
    /// stack to stdout for inspection. Useful when debugging a
    /// kustomize overlay before applying it.
    Kind,
    /// `kubectl kustomize k8s/overlays/gke` — render the production
    /// overlay to stdout for inspection. Config Sync owns the actual
    /// apply in production; this is the local equivalent of "what
    /// will the cluster see?"
    Gke,
}

// See `Command`: boxing nested Clap subcommands would make every dispatch
// pattern noisier without reducing steady-state application memory.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum OpsCmd {
    /// Reconcile one named GitHub repository's merge protections and policy.
    /// Reads `GITHUB_TOKEN`; the required repository argument is never inferred
    /// from the environment or a checkout's `origin` remote.
    #[command(subcommand)]
    Github(GithubCmd),
    /// Verify a Git backup, reconcile Forgejo policy, or schedule a governed PR.
    #[command(subcommand)]
    Forgejo(ForgejoPolicyCmd),
    /// One-shot deployment reconciler — the "do everything" button documented
    /// in `docs/cloud-operations.md`. CI (`deploy.yml`) builds and publishes
    /// the images to Artifact Registry under an immutable release tag; `ops ship` reconciles
    /// the cluster. Flow: take the `--tag` (`YY.M.D` or
    /// `YY.M.D-hotfix.N`) →
    /// reconcile the manifests (render the embedded GKE tree with the selected
    /// deployment's `NAVIGATOR_*` coordinates into a temp dir and `kubectl
    /// apply -k` it) → confirm the selected deployment's Secret satisfies the
    /// new binary's boot invariants → roll out its runtime workloads at that
    /// tag → pin every trigger `CronJob` to the same tag → re-register the
    /// worker with Restate, so every navigator image ends in sync at one
    /// immutable release tag. Reads installation coordinates from the explicit
    /// --project-id's cloud configuration, or from a local deployment tree.
    /// --deployment always selects the installation; ambient GCP configuration
    /// never selects the target. Never builds images
    /// locally and needs no external overlay folder.
    Ship {
        /// Installation name in cloud configuration or the local deployment tree.
        /// Required explicitly; there is no environment fallback.
        #[arg(long)]
        deployment: String,
        /// The directory CONTAINING `deployments/` — the same directory
        /// `.sops.yaml` sits in. Point it at a checkout that carries the
        /// deployment tree and nothing else; the GKE manifests are compiled
        /// into this binary, so no source tree has to be present. Defaults to
        /// `NAVIGATOR_DEPLOYMENTS_DIR`, then to the discovered workspace root.
        #[arg(long, value_name = "DIR")]
        deployments_dir: Option<PathBuf>,
        /// Read versioned deployment configuration from this explicit GCP project.
        /// Requires ADC; does not read a local deployment tree. With --dry-run,
        /// only configuration is fetched; rollout preflights remain offline.
        #[arg(long, conflicts_with = "deployments_dir")]
        project_id: Option<String>,
        /// Render an offline plan without live deployment checks.
        /// --project-id first fetches cloud configuration using ADC; a local tree needs no network.
        /// The plan uses the selected configuration and rendered manifests, and does
        /// not check live access, drift, images, secrets, IAM, assets, Restate,
        /// or public endpoints.
        #[arg(long)]
        dry_run: bool,
        /// No-rebuild path: restart the selected deployment's runtime
        /// workloads so the pods re-read a rotated Secret value, then exit.
        /// Use after rotating a key in the K8s Secret.
        #[arg(long)]
        restart_only: bool,
        /// Move every navigator image to `--tag` and change nothing else —
        /// the lane an automated deploy runs under a credential that can do
        /// no more than bump a version. Refuses when the rendered manifests
        /// differ from the cluster, because it applies none of that diff.
        #[arg(long)]
        image_only: bool,
        /// Immutable registry tag to roll onto: `YY.M.D`
        /// or `YY.M.D-hotfix.N`. Required for a roll; omit only with
        /// `--restart-only`.
        #[arg(long)]
        tag: Option<String>,
        /// JSON mapping image repositories to their published SHA-256 digests.
        #[arg(long, value_name = "FILE")]
        image_digests: Option<PathBuf>,
        /// Verify the web service account's self-signing IAM binding instead
        /// of establishing it: when the binding is absent the roll stops and
        /// prints the `gcloud` command that would grant it, rather than
        /// running that command itself. The lane for an operator who holds
        /// the release tag but not `iam.serviceAccounts.setIamPolicy` — the
        /// binding is still asserted, since a web pod without it 500s every
        /// document download; only the grant moves to someone permitted it.
        #[arg(long)]
        assert_signing_iam: bool,
        /// Run the ship with its whole transcript — including what `kubectl`
        /// and `gcloud` print — sent to `--log-sink`, and print only a
        /// one-line verdict (ok or failed, exit code, an id, a byte count).
        /// For a runner whose log is public: masking covers only values the
        /// runner knows are secret. The ship's exit code is kept.
        #[arg(long, requires = "log_sink")]
        quiet: bool,
        /// Where `--quiet` writes the transcript: `gs://BUCKET[/PREFIX]`, or
        /// `file:///DIR` for a local run. The CI identity needs only
        /// `storage.objects.create` on it. Also read from
        /// `NAVIGATOR_SHIP_LOG_SINK`.
        #[arg(long, env = "NAVIGATOR_SHIP_LOG_SINK", requires = "quiet")]
        log_sink: Option<String>,
        /// When a brand host change alters the shared Firm certificate's
        /// domains, the roll requests the new certificate before the
        /// release and waits for it after the rollouts. This bounds that
        /// wait, in minutes.
        #[arg(long, default_value_t = devx::DEFAULT_CERTIFICATE_TIMEOUT_MINUTES, value_parser = clap::value_parser!(u32).range(1..))]
        certificate_timeout_minutes: u32,
    },
    /// Read a saved ship transcript under the operator's own credentials.
    /// Each deployment resolves its own private logs bucket from its row.
    Logs {
        /// Deployment directory under `deployments/`.
        #[arg(long)]
        deployment: String,
        /// GitHub run id and attempt (for example `991-2`).
        #[arg(long)]
        run: String,
        /// Optional prefix passed to `ops ship --log-sink` after the bucket.
        #[arg(long, default_value = "")]
        prefix: String,
        /// Directory containing `deployments/`; defaults to the workspace or
        /// `NAVIGATOR_DEPLOYMENTS_DIR`.
        #[arg(long, value_name = "DIR")]
        deployments_dir: Option<PathBuf>,
        /// Read the row from this project's published record.
        #[arg(long, conflicts_with = "deployments_dir")]
        project_id: Option<String>,
    },
    /// Export `SurrealDB` into the firm-controlled object-storage archive, or
    /// prove one stored export can be restored into a disposable namespace.
    #[command(subcommand)]
    SurrealArchive(SurrealArchiveCmd),
    /// PostgreSQL mirror parity and reconcile against `SurrealDB`.
    ///
    /// Reads the same `NAVIGATOR_SURREAL_*` and `NAVIGATOR_POSTGRES_*`
    /// variables `web` reads. Reports name rows by UUID only.
    #[command(subcommand)]
    Postgres(postgres_mirror::PostgresCmd),
    /// Check a `deployments/` tree without changing anything or decrypting
    /// anything. Every row loads, no decrypted file sits beside an encrypted
    /// one, every `.sops.yaml` rule agrees with the key its row declares, and
    /// every provisioned row satisfies the boot requirements that apply to it
    /// and supplies every object its pod's Secret projects.
    ///
    /// The gate the tree's own CI runs. The workspace suite asserts the same
    /// things against `deploy/tests/fixtures/deployment-tree/`, which is a
    /// fixture — this is how the real rows, which live in a private
    /// repository, are held to it too.
    ///
    /// By default, checks local files without credentials or network access.
    /// The subcommands create and edit a row's published record in its GCP
    /// project's Secret Manager — the only source `ops ship --project-id`
    /// reads. `--publish` is an alias of `import`; none of them decrypts.
    #[command(args_conflicts_with_subcommands = true)]
    Deployments {
        #[command(subcommand)]
        action: Option<DeploymentsCmd>,
        /// The directory CONTAINING `deployments/` — the same flag the other
        /// tree commands take. Defaults to `NAVIGATOR_DEPLOYMENTS_DIR`, then
        /// to the discovered workspace root.
        #[arg(long, value_name = "DIR")]
        deployments_dir: Option<PathBuf>,
        /// Alias of `import`: publish one local row's record.
        #[arg(long, value_name = "DEPLOYMENT")]
        publish: Option<String>,
    },
    /// Deployment key material. The `deployments/` tree is the operator
    /// source: plaintext coordinates beside SOPS-encrypted values, decrypted
    /// only by `apply` and written into that deployment's own Secret Manager.
    #[command(subcommand)]
    Secrets(SecretsCmd),
    /// Operator recovery for the inbound email-summary Restate workflow.
    #[command(subcommand)]
    EmailSummary(EmailSummaryCmd),
    /// GCP project provisioning. The actual REST plumbing lives in
    /// `deploy/src/devx/gcp/`; this is the entry point operators reach for
    /// when standing up (or re-running) Neon Law Navigator on a fresh GCP
    /// project.
    #[command(subcommand)]
    Gcp(GcpCmd),
    /// Restate Cloud CLI wrappers. Saves operators from memorizing
    /// the `restate deployment register …` invocation. Assumes the
    /// caller has already run `restate -y cloud login` and
    /// `restate -y cloud env config --env <your-env-name>` (or set
    /// `RESTATE_CLOUD_TOKEN`/`RESTATE_ENVIRONMENT` in CI).
    #[command(subcommand)]
    Restate(RestateCmd),
    /// Diagnose ongoing scheduled-job health: surface trigger Jobs wedged in
    /// `ImagePullBackOff`/`CrashLoopBackOff` (which, under a `CronJob`'s
    /// `concurrencyPolicy: Forbid`, silently skip every subsequent run) and
    /// workloads that aren't fully ready, each with the command that fixes it.
    /// Read-only `kubectl get` against the current context.
    Doctor {
        /// Namespace to inspect. Defaults to `NAVIGATOR_K8S_NAMESPACE` / `navigator`.
        #[arg(long)]
        namespace: Option<String>,
    },
    /// Reconcile a Twilio receiving number's Call Script: the greeting a
    /// caller hears, the extensions that forward to a person's phone, and
    /// the message everyone else hears. Renders the number's Studio Flow
    /// from the flags and compares it with the published one; read-only
    /// until `--apply`. Reads `TWILIO_ACCOUNT_SID` / `TWILIO_AUTH_TOKEN`
    /// from the environment, or from a deployment with `--project-id`.
    /// Full recipe: `docs/third-party-integrations.md`.
    CallScript {
        /// The receiving number, E.164. Its Studio Flow is the one edited.
        #[arg(long)]
        number: String,
        /// The opening sentence, before the extensions are offered.
        #[arg(long)]
        greeting: String,
        /// One person a caller can reach (repeatable): the digits to key,
        /// the name the greeting speaks, and the E.164 phone to forward to.
        #[arg(long = "extension", value_name = "DIGITS=NAME=NUMBER", required = true)]
        extensions: Vec<devx::call_script::Extension>,
        /// Said to a caller who keys nothing or an unknown extension.
        #[arg(long)]
        fallback: String,
        /// Read the Twilio credentials from this GCP project's Secret
        /// Manager for this run only, in memory. Requires ADC.
        #[arg(long)]
        project_id: Option<String>,
        /// Publish the script. Without it the command only reports drift.
        #[arg(long)]
        apply: bool,
    },
    /// DNS provisioning for a public deploy — reachability, the apex→www
    /// redirect, and both mail lanes — via the configured DNS provider
    /// (`DNSimple` today). Reads `DNS_ZONE` / `DNS_ACCT` / `DNS_SIMPLE`
    /// (`DNSIMPLE_API_TOKEN` remains a legacy alias). Idempotent: matching
    /// records are no-ops.
    #[command(subcommand)]
    Dns(DnsCmd),
    /// Bring one registered brand host online for a deployment: DNS, the TLS
    /// certificate check, and the per-host sign-in check.
    #[command(subcommand)]
    Host(HostCmd),
    /// Bounded per-host release-readiness check (ENG-808): for every host
    /// the release inventory (`views::brand::release_brand_hosts`) covers on
    /// the selected deployment's environment, perform a real TLS handshake
    /// through the ordinary trust store (never `-k`) and confirm the actual
    /// response — a live brand's own `og:site_name` at `200`, a held-out
    /// brand's `404`, and (production only) every live brand's apex
    /// redirect. Never mutates anything; exits nonzero and names every
    /// failing host when any check is not ready. Safe to rerun as often as
    /// needed — this is a point-in-time receipt, not a wait-until-ready loop.
    BrandReadiness {
        /// Deployment directory under `deployments/`, such as `neon-law-stg`.
        #[arg(long)]
        deployment: String,
        /// The directory CONTAINING `deployments/` — the same flag `ops ship`
        /// takes. Defaults to `NAVIGATOR_DEPLOYMENTS_DIR`, then to the
        /// discovered workspace root.
        #[arg(long, value_name = "DIR")]
        deployments_dir: Option<PathBuf>,
        /// Read the deployment's configuration from this explicit GCP project
        /// (its published record, from `ops deployments`) instead of a local
        /// tree. Requires ADC.
        #[arg(long, conflicts_with = "deployments_dir")]
        project_id: Option<String>,
        /// Per-host `curl` timeout, in seconds. Bounds the whole run to at
        /// most this many seconds times the number of hosts checked.
        #[arg(long, default_value_t = 15)]
        timeout_seconds: u32,
    },
    /// Shared-certificate maintenance for brand host changes.
    #[command(subcommand)]
    Certificates(CertificatesCmd),
    /// Deprecated: use the white-label bundle workflow documented in
    /// `docs/oss-install.md`, whose worked manifest is
    /// `deploy/tests/fixtures/navigator.example.yaml`.
    #[command(subcommand)]
    Rebrand(BrandCmd),
    /// Stand up the `OTel` Collector seam in prod and wire the binaries to
    /// it: ensure the `navigator-otel` GSA + telemetry-write IAM +
    /// Workload Identity, apply the Collector + self-monitoring
    /// manifests, and `envFrom` the shared `navigator-otel-env` `ConfigMap`
    /// onto `navigator-web` + `workflows-service` so
    /// `OTEL_EXPORTER_OTLP_ENDPOINT` reaches `telemetry::init`. Idempotent;
    /// reads project/region/cluster/context from the selected deployment's
    /// published record (`--project-id`) or `deployments/<name>/config.toml`.
    /// Run once per cluster, then `ops ship` (or rollout-restart) the binaries.
    Observability {
        /// Deployment directory under `deployments/`, such as `neon-law-stg`.
        #[arg(long)]
        deployment: String,
        /// The directory CONTAINING `deployments/` — the same flag `ops ship`
        /// and `ops secrets apply` take. Defaults to
        /// `NAVIGATOR_DEPLOYMENTS_DIR`, then to the discovered workspace root.
        #[arg(long, value_name = "DIR")]
        deployments_dir: Option<PathBuf>,
        /// Read the row from this project's published record.
        #[arg(long, conflicts_with = "deployments_dir")]
        project_id: Option<String>,
        /// Print every command instead of running it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Distribute the `navigator-lsp` editor binary.
    Lsp {
        #[command(subcommand)]
        action: LspAction,
    },
    /// Transcode curated source photos into responsive web variants.
    Assets {
        #[command(subcommand)]
        action: AssetsAction,
    },
    /// Name the next release and write it as the workspace version.
    ///
    /// Today's UTC `YY.M.D` until that is published, then the next
    /// unpublished date's `YY.M.D-rc.N`. Delegates the write to `ops release
    /// version`. A tag far ahead of the calendar or an operational error
    /// exits 2. Hotfixes and other explicit names use `ops release version
    /// --tag`.
    CutRelease {
        /// Git checkout supplying the release tags, workspace files, and commit.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Root Cargo.toml in --repo; relative paths resolve from its worktree root.
        #[arg(long, default_value = "Cargo.toml")]
        manifest_path: PathBuf,
        /// Compare against the tags already in this clone instead of fetching
        /// from `origin` first. Offline, and only as current as the clone.
        #[arg(long)]
        no_fetch: bool,
        /// Write the manifest but create no commit.
        #[arg(long)]
        no_commit: bool,
        /// Print the next release's tag and write nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Write a history-free root commit that is never pushed.
    ///
    /// The commit's tree is the checked-out ref's tree minus the paths its
    /// `.public-snapshot-exclude` lists.
    ///
    /// Refuses unless the worktree is clean, `--ref` is the checked-out
    /// commit and is reachable from `origin/main`, and `navigator validate`
    /// passes on it. The commit's identity and date come from the source
    /// commit unless overridden, so two runs on one ref print one SHA. Never
    /// pushes, adds a remote, or changes a hosted repository; the remaining
    /// steps are the manual checklist in `docs/public-snapshot.md`.
    PublicSnapshot {
        /// Source checkout.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Commit to publish. It must be the checked-out commit.
        #[arg(long = "ref", default_value = "HEAD")]
        git_ref: String,
        /// New or empty directory the snapshot repository is written to.
        #[arg(long, value_name = "DIR")]
        out: PathBuf,
        /// Author and committer name. Defaults to the source committer's.
        #[arg(long)]
        author_name: Option<String>,
        /// Author and committer email. Defaults to the source committer's.
        #[arg(long)]
        author_email: Option<String>,
        /// Author and committer date, in any form Git accepts. Defaults to
        /// the source commit's committer date.
        #[arg(long)]
        date: Option<String>,
        /// Sign the snapshot commit. The signature is part of the commit, so
        /// a signed SHA is not reproducible; compare tree hashes instead.
        #[arg(long)]
        sign: bool,
    },
    /// Probe the next release's name against published tags.
    ///
    /// Prints today's UTC `YY.M.D` until that is published, then the next
    /// unpublished date's `YY.M.D-rc.N`. When a tag far ahead of the
    /// calendar leaves nothing to cut, prints only a reason on stderr and
    /// exits 0.
    /// `ops cut-release` writes a daily cut; `ops release version --tag`
    /// writes an explicitly named version.
    ReleaseDefaultTag {
        /// Git checkout whose tags are the record of what has been released.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Compare against the tags already in this clone instead of fetching
        /// from `origin` first. Offline, and only as current as the clone.
        #[arg(long)]
        no_fetch: bool,
    },
    /// Release versioning and release preflight checks.
    #[command(subcommand)]
    Release(ReleaseCmd),
    /// Regenerate `THIRD-PARTY-NOTICES.txt` from `Cargo.lock` — the licence
    /// texts the downloadable binary must carry, deduplicated so each distinct
    /// text appears once with the crates that use it. Every permissive licence
    /// in the tree requires its notice to travel with the distributed work, so
    /// a release must not ship without this file being current. Reads crate
    /// sources from
    /// `$CARGO_HOME/registry/src`; run `cargo fetch` first on a cold machine.
    Notices {
        /// Where to write the notices. Defaults to the repository root file
        /// that `cli/src/commands.rs` embeds into the binary.
        #[arg(long, default_value = "THIRD-PARTY-NOTICES.txt")]
        out: PathBuf,
        /// Regenerate in memory and fail if the file on disk differs, instead
        /// of rewriting it. The drift gate for CI.
        #[arg(long)]
        check: bool,
    },
    /// Firm-scoped diagnostics.
    Firms {
        #[command(subcommand)]
        action: FirmsAction,
    },
    /// Every matter whose pre-filing PII check would be refused, with the
    /// record that breaks it (LAW-301). Reads the admin-tier
    /// `GET /app/api/project-private-terms-reconciliation` door over the
    /// `site login` session — no local database coordinates — and never
    /// repairs a row: a finding names the work, and a human decides.
    PrivateTerms {
        #[command(flatten)]
        host: HostOpt,
    },
    /// Inspect the fixed Solana devnet SAS program account.
    #[command(subcommand)]
    Sas(SasCmd),
}

#[derive(Subcommand)]
enum SasCmd {
    /// Read the SAS program at finalized commitment.
    Program,
}

#[derive(Subcommand)]
enum FirmsAction {
    /// Report every active Firm's Admin-DRI standing (ENG-499): missing,
    /// multiple, or ineligible designations. Read-only — never appoints,
    /// clears, or otherwise repairs a row; `store::firms::appoint_admin_dri`
    /// is the only writer. Connects to the `SurrealDB` the sourced
    /// `.devx/env` names, the same store `web` reads.
    Doctor,
}

#[derive(Subcommand)]
enum ReleaseCmd {
    /// Decide whether the workspace version is a new release.
    Check {
        /// The workspace manifest whose version is the candidate release.
        #[arg(long, default_value = "Cargo.toml")]
        manifest_path: PathBuf,
        /// Git checkout whose tags are the record of what has been released.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Compare against tags already in this clone instead of fetching.
        #[arg(long)]
        no_fetch: bool,
        /// Append release fields to `$GITHUB_OUTPUT` for workflow jobs.
        #[arg(long)]
        github_output: bool,
    },
    /// Check that every self-referencing action pin names the workspace version.
    ///
    /// The reusable workflows and composite actions under `.github/` name this
    /// repository's own actions by an absolute tag, so a pin left behind ships
    /// a gate no consumer can run. `ci.yml` runs this on every pull request and
    /// the `cut-release` preflight runs it again before the bump is pushed;
    /// both reach the one rule in `release_pins` rather than re-deriving it.
    Pins {
        /// Repository root whose checked-in GitHub configuration is scanned.
        #[arg(long, default_value = ".")]
        root: PathBuf,
    },
    /// Write a release version into the workspace manifest.
    Version {
        /// Release version to write, e.g. `26.8.20` or `26.8.21-hotfix.3`.
        #[arg(long)]
        tag: String,
        /// The workspace manifest to rewrite.
        #[arg(long, default_value = "Cargo.toml")]
        manifest_path: PathBuf,
        /// Write the manifest but create no commit.
        #[arg(long)]
        no_commit: bool,
    },
}

#[derive(Subcommand)]
enum SurrealArchiveCmd {
    /// Export the configured Surreal namespace and database to object storage.
    Export,
    /// Download one export, restore it into a disposable namespace, apply the
    /// current schema, and verify it against its export-time receipt when present.
    RestoreDrill {
        /// Object-storage key printed by `surreal-archive export`.
        #[arg(long)]
        key: String,
    },
}

#[derive(Subcommand)]
enum SecretsCmd {
    /// Import an owner-exported encrypted change into production SOPS; does not apply or deploy.
    ImportSocialChanges {
        #[arg(long)]
        deployment: String,
        #[arg(long)]
        deployments_dir: Option<PathBuf>,
        #[arg(long)]
        request: PathBuf,
    },
    /// Decrypt one deployment's `deployments/<name>/secrets.enc.yaml` and write
    /// `versions/latest` into that deployment's own Secret Manager, which the
    /// Secret Manager CSI driver projects into the pod.
    ///
    /// The repository is the operator source: plaintext coordinates in
    /// `config.toml`, key material encrypted per value against that
    /// deployment's own Cloud KMS key. Rotating a value means rotating it at
    /// the provider first and re-encrypting here second — re-encrypting alone
    /// revokes nothing, because anyone holding repository history and the KMS
    /// key can still read every prior ciphertext.
    Apply {
        /// Deployment directory under `deployments/`, such as `neon-law-stg`.
        #[arg(long)]
        deployment: String,
        /// Opt-in infrastructure service whose file Secret is provisioned.
        /// Omit to preserve the existing web/workflow env Secret behavior.
        #[arg(long, value_parser = ["postgres", "forgejo"], conflicts_with = "check")]
        service: Option<String>,
        /// The directory CONTAINING `deployments/` — the same directory
        /// `.sops.yaml` sits in, and the same flag `ops ship` takes. Defaults
        /// to `NAVIGATOR_DEPLOYMENTS_DIR`, then to the discovered workspace
        /// root.
        #[arg(long, value_name = "DIR")]
        deployments_dir: Option<PathBuf>,
        /// Print the target project and the object names without decrypting
        /// anything or changing Secret Manager. Needs no KMS permission.
        #[arg(long, conflicts_with = "check")]
        dry_run: bool,
        /// Decrypt in-process (the same trust `apply` uses) and compare every
        /// object's SOPS value against Secret Manager `versions/latest` and
        /// the deployment's Kubernetes Secret, by constant-time equality.
        /// Prints names and status only — `match`, `differs`, `missing in
        /// Secret Manager`, or `missing in K8s Secret` — never a value or a
        /// digest, and exits non-zero on any drift. Changes nothing.
        #[arg(long, conflicts_with = "dry_run")]
        check: bool,
        /// Project Secrets from this project's record and Secret Manager.
        #[arg(long, conflicts_with_all = ["deployments_dir", "check"])]
        project_id: Option<String>,
    },
    /// Write one secret value, read from stdin, to Secret Manager.
    ///
    /// The value is never taken from argv and never printed. One trailing
    /// newline is stripped unless `--raw` is passed. The key name is added to
    /// the row's published record if it is not already there.
    Set {
        /// Deployment row name.
        deployment: String,
        /// GCP project holding the row's published record.
        #[arg(long)]
        project_id: String,
        /// Secret key name, such as `HCAPTCHA_SECRET`.
        key: String,
        /// Key inventory; defaults to the web env Secret.
        #[arg(long, value_parser = ["postgres", "forgejo"])]
        service: Option<String>,
        /// Store stdin byte for byte, keeping a trailing newline.
        #[arg(long)]
        raw: bool,
    },
    /// Remove one secret key name from a row's published record.
    ///
    /// Secret Manager keeps the stored versions; nothing projects them once
    /// the name is gone from the record.
    Unset {
        /// Deployment row name.
        deployment: String,
        /// GCP project holding the row's published record.
        #[arg(long)]
        project_id: String,
        /// Secret key name to remove.
        key: String,
    },
}

/// Edit a deployment row's published record.
#[derive(Subcommand)]
enum DeploymentsCmd {
    /// Create a row's published record; refuses an existing one.
    ///
    /// Stores the project, location, both capability flags, and every
    /// `--set` coordinate. Everything derivable — cluster, namespace, network,
    /// service accounts, buckets, KMS key, GKE context — is filled in by every
    /// reader and need not be set.
    Init {
        /// Deployment row name.
        deployment: String,
        /// GCP project that owns the row.
        #[arg(long)]
        project_id: String,
        /// GCP location (region) of the row's cluster.
        #[arg(long)]
        location: String,
        /// Whether this row is the GitHub automation home.
        #[arg(long, action = clap::ArgAction::Set, required = true)]
        github_automation_home: bool,
        /// Whether this row may text SMS test recipients.
        #[arg(long, action = clap::ArgAction::Set, required = true)]
        sms_test_recipients: bool,
        /// Store a coordinate, `KEY=VALUE`; repeatable.
        #[arg(long = "set", value_name = "KEY=VALUE", value_parser = parse_assignment)]
        set: Vec<(String, String)>,
    },
    /// Store coordinates or secret key names in a row's record.
    Set {
        /// Deployment row name.
        deployment: String,
        /// GCP project holding the row's published record.
        #[arg(long)]
        project_id: String,
        /// Coordinates to store, `KEY=VALUE`.
        #[arg(value_name = "KEY=VALUE", value_parser = parse_assignment)]
        assignments: Vec<(String, String)>,
        #[command(flatten)]
        keys: SecretKeyNames,
    },
    /// Drop coordinates or secret key names from a row's record.
    Unset {
        /// Deployment row name.
        deployment: String,
        /// GCP project holding the row's published record.
        #[arg(long)]
        project_id: String,
        /// Stored coordinates to drop; derived values return.
        #[arg(value_name = "KEY")]
        coordinates: Vec<String>,
        #[command(flatten)]
        keys: SecretKeyNames,
    },
    /// Publish one private-tree row as its record, once.
    ///
    /// Refuses a row missing the service key files the ordered release
    /// requires. Decrypts nothing; uploads coordinates and key names only.
    Import {
        /// Deployment row name under `deployments/`.
        deployment: String,
        /// The directory CONTAINING `deployments/`.
        #[arg(long, value_name = "DIR")]
        deployments_dir: Option<PathBuf>,
    },
}

/// Secret key names an edit adds or removes, by inventory.
#[derive(clap::Args)]
struct SecretKeyNames {
    /// A web env Secret key name; repeatable.
    #[arg(long = "secret-key", value_name = "NAME")]
    web: Vec<String>,
    /// A PostgreSQL service key name; repeatable.
    #[arg(long = "postgres-key", value_name = "NAME")]
    postgres: Vec<String>,
    /// A Forgejo service key name; repeatable.
    #[arg(long = "forgejo-key", value_name = "NAME")]
    forgejo: Vec<String>,
}

/// Parse a `KEY=VALUE` coordinate. The value may itself contain `=`.
fn parse_assignment(raw: &str) -> Result<(String, String), String> {
    let (key, value) = raw
        .split_once('=')
        // The input is not echoed: a mistaken secret must not reach a log.
        .ok_or_else(|| "expected KEY=VALUE".to_owned())?;
    if key.is_empty() {
        return Err("expected KEY=VALUE with a non-empty KEY".into());
    }
    Ok((key.to_owned(), value.to_owned()))
}

#[derive(Subcommand)]
enum EmailSummaryCmd {
    /// Read-only pipeline check: fail on overdue receipts or failed/unknown delivery.
    Health,

    /// Re-run a completed `EmailSummary` Restate workflow for one receipt —
    /// the recovery path for a run that completed with a bounded provider
    /// failure (e.g. `input_digest_mismatch`) before intake can re-POST,
    /// since `SendGrid` never retries a message that already got a 202.
    ///
    /// Refuses when the receipt's Slack delivery is already `confirmed`, so
    /// this can never risk a second post, unless `--after-failed-result`
    /// vouches that the confirmed post was a failure notice. Never creates a second receipt,
    /// letter, or archive: those are digest-keyed in `SurrealDB` already, and
    /// this command only purges the retained invocation and resubmits the
    /// identical `EmailSummaryRequest` under the same workflow key (the
    /// receipt id). Prints the receipt id, the invocation id, and status
    /// only — never a summary, a letter, or any client content. Reads
    /// `NAVIGATOR_SURREAL_*` (the deployment's database), the
    /// `NAVIGATOR_SUMMARY_*` / `RESTATE_BROKER_URL` summary-lane
    /// configuration, and `RESTATE_ADMIN_URL` / `RESTATE_ADMIN_TOKEN` /
    /// `RESTATE_AUTH_TOKEN` — the same environment `navigator-deploy dev up`/`ops
    /// ship` already source for the target deployment, so there is no
    /// separate `--deployment` flag to keep in sync with it.
    Redrive {
        /// The receipt to redrive.
        #[arg(long)]
        receipt: uuid::Uuid,
        /// Vouch that the receipt's `confirmed` Slack post was a
        /// provider-failure notice, and name the operator. Reopens the
        /// delivery after the purge so the redriven run posts the real
        /// summary; without it a `confirmed` delivery is refused.
        #[arg(long, value_name = "ACTOR")]
        after_failed_result: Option<String>,
    },
    /// List every delivery currently quarantined in `UNKNOWN`, the state
    /// `deliver_summary` records when Slack's response is lost, times out,
    /// or comes back a retryable HTTP status — an operator can't reconcile
    /// what they can't see. Reads `NAVIGATOR_SURREAL_*` only; changes
    /// nothing.
    Quarantined,
    /// Record that a quarantined delivery's Slack post actually succeeded,
    /// moving it to `confirmed` so replay stops treating it as in doubt.
    /// Use only after confirming the post landed in Slack out of band (for
    /// example, by looking at the channel).
    ReconcileConfirmed {
        /// The quarantined receipt to reconcile.
        #[arg(long)]
        receipt: uuid::Uuid,
        /// The operator recording this decision, stored as `reconciled_by`.
        #[arg(long)]
        actor: String,
        /// The Slack channel the confirmed post landed in.
        #[arg(long)]
        channel: String,
        /// The confirmed post's Slack message timestamp.
        #[arg(long)]
        timestamp: String,
    },
    /// Record operator authorization to resend a quarantined delivery,
    /// moving it back to `not_attempted` so the next `deliver_summary` call
    /// is admitted again. Use only after confirming the original post never
    /// reached Slack, since this permits a second post attempt.
    AuthorizeResend {
        /// The quarantined receipt to authorize a resend for.
        #[arg(long)]
        receipt: uuid::Uuid,
        /// The operator recording this decision, stored as `reconciled_by`.
        #[arg(long)]
        actor: String,
    },
}

#[derive(Subcommand)]
enum ForgejoPolicyCmd {
    /// Reconcile Firm users' SSH public keys onto their Forgejo accounts.
    SshKeys {
        #[command(subcommand)]
        action: ForgejoSshKeysCmd,
    },
    /// Run one repository-scoped Actions job in a disposable, network-isolated Kubernetes pod.
    #[command(
        after_help = "Run only from a trusted operator host. FORGEJO_TOKEN must never enter a pull-request-controlled job."
    )]
    RunnerOnce(ForgejoRunnerArgs),
    /// Start a disposable runner for every queued Actions job, forever. Runs in-cluster.
    #[command(
        after_help = "FORGEJO_TOKEN holds the environment's operator token; it never enters a job pod. \
Each runner is the same isolated, repository-scoped one-job Job `runner-once` starts. \
See docs/forgejo-deployment.md."
    )]
    RunnerController {
        /// Public HTTPS Forgejo origin.
        #[arg(long)]
        url: String,
        /// Existing dedicated runner namespace.
        #[arg(long)]
        runner_namespace: String,
        /// Operate through kubectl on this context instead of the pod's own service account.
        #[arg(long)]
        context: Option<String>,
        /// Hosts a job may reach on TCP 443, resolved to public IPv4 at each launch: the Forgejo
        /// origin and the Navigator host.
        #[arg(long)]
        egress_host: Vec<String>,
        /// Extra exact public IPv4 destinations on TCP 443.
        #[arg(long)]
        egress_ip: Vec<String>,
        /// `NodeLocal` DNS IPv4 permitted on TCP/UDP 53 beside `kube-dns`.
        #[arg(long, default_value = "169.254.20.10")]
        dns_ip: String,
        /// Seconds between ticks.
        #[arg(long, default_value_t = 20)]
        interval: u64,
        /// Most runners alive at once.
        #[arg(long, default_value_t = 4)]
        max_runners: usize,
        /// Run one tick, wait for its runners, and exit.
        #[arg(long)]
        once: bool,
    },
    /// Print every job log of one Actions run, read from the Forgejo volume.
    #[command(
        after_help = "Needs FORGEJO_TOKEN on the trusted operator host and kubectl exec on the Forgejo pod. \
A queued job has no log yet; a running job's log appears when it finishes."
    )]
    Logs {
        /// Explicit owner/repository.
        repository: String,
        /// The run number Forgejo shows (`/actions/runs/<n>`).
        #[arg(long)]
        run: u64,
        /// HTTPS Forgejo origin.
        #[arg(long)]
        url: String,
        /// Explicit Kubernetes context of the Forgejo installation.
        #[arg(long)]
        context: String,
        /// Namespace of the `navigator-forgejo` `StatefulSet`.
        #[arg(long, default_value = "navigator")]
        namespace: String,
    },
    /// Mirror published navigator releases into this Forgejo's package registry.
    Release {
        #[command(subcommand)]
        action: ForgejoReleaseCmd,
    },
    /// Back up a local mirror and prove every ref restores. Excludes LFS and forge metadata.
    Bundle {
        /// Local bare mirror made with `git clone --mirror`.
        mirror: std::path::PathBuf,
        /// New bundle file; an existing destination is never replaced.
        output: std::path::PathBuf,
    },
    /// Create a private immutable GCS bundle, read back its exact generation,
    /// and write a receipt only after Git restore verification passes.
    Archive {
        /// Local bare mirror made with `git clone --mirror`.
        mirror: std::path::PathBuf,
        /// Opaque object key in `NAVIGATOR_FORGEJO_ARCHIVES_BUCKET`.
        object: String,
        /// New private receipt path; existing files are never replaced.
        receipt: std::path::PathBuf,
    },
    /// Restore a bundle from the exact GCS generation named by its receipt.
    Restore {
        /// Private receipt produced by `ops forgejo archive`.
        receipt: std::path::PathBuf,
        /// New bare mirror destination; existing paths are never replaced.
        output: std::path::PathBuf,
    },
    /// Reconcile protected main and squash-only repository settings.
    Setup {
        /// Explicit owner/repository; never inferred from a checkout.
        repository: String,
        /// HTTPS Forgejo origin, or HTTP loopback for KIND.
        #[arg(long)]
        url: String,
        /// Exact required check context observed on this Forgejo installation.
        #[arg(long)]
        check_context: String,
        /// Report drift using GET requests only.
        #[arg(long)]
        dry_run: bool,
    },
    /// Schedule squash auto-merge after verifying protection and PR readiness.
    AutoMerge {
        repository: String,
        #[arg(long)]
        url: String,
        #[arg(long)]
        check_context: String,
        #[arg(long)]
        pull_request: u64,
    },
    /// Discover open pull requests from an explicitly selected repository and schedule eligible ones.
    #[command(
        after_help = "Run only from a trusted operator host. FORGEJO_TOKEN must never enter a pull-request-controlled job."
    )]
    ScheduleOpen {
        /// Explicit owner/repository; never inferred from a checkout or event payload.
        repository: String,
        /// HTTPS Forgejo origin, or HTTP loopback for KIND.
        #[arg(long)]
        url: String,
        /// Exact required check context observed on this Forgejo installation.
        #[arg(long)]
        check_context: String,
    },
}

#[derive(Subcommand)]
enum ForgejoReleaseCmd {
    /// Download one release's Linux archive and digest from GitHub, verify the SHA-256, and
    /// publish both as the public `navigator` organization's generic package. Idempotent.
    #[command(
        after_help = "Needs FORGEJO_TOKEN for a site administrator on the trusted operator host. \
Forgejo workflows install from <url>/api/packages/navigator/generic/navigator/<version>/."
    )]
    Mirror {
        /// HTTPS Forgejo origin, or HTTP loopback for KIND.
        #[arg(long)]
        url: String,
        /// Exact release tag, as `navigator.yaml` pins it.
        #[arg(long)]
        version: String,
    },
}

#[derive(Subcommand)]
enum ForgejoSshKeysCmd {
    /// Reconcile every Person holding a key (or one Person) through the
    /// key-sync service identity. Drift is reported, never removed; exits
    /// nonzero while any account holds drift or failed.
    #[command(
        after_help = "Needs NAVIGATOR_FORGEJO_ROOT_URL, NAVIGATOR_FORGEJO_KEY_SYNC_LOGIN and NAVIGATOR_FORGEJO_KEY_SYNC_TOKEN, plus the SurrealDB and PostgreSQL settings."
    )]
    Sync {
        /// One Person, by id or email; every Person holding a key otherwise.
        #[arg(long)]
        person: Option<String>,
        /// Report the plan without writing to Forgejo.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(clap::Args)]
struct ForgejoRunnerArgs {
    /// Explicit owner/repository; organization-wide and global runners are refused.
    repository: String,
    /// Public HTTPS Forgejo origin. `FORGEJO_TOKEN` stays on this trusted operator host.
    #[arg(long)]
    url: String,
    /// Explicit Kubernetes context; never inferred from the active context.
    #[arg(long)]
    context: String,
    /// Existing dedicated runner namespace, separate from Navigator's application namespace.
    #[arg(long)]
    runner_namespace: String,
    /// Exact public IPv4 destinations permitted on TCP 443.
    #[arg(
        long,
        required_unless_present_any = ["cleanup_registration", "egress_host"],
        conflicts_with = "cleanup_registration"
    )]
    egress_ip: Vec<String>,
    /// Hosts whose public IPv4 addresses are resolved now and permitted on TCP 443: the Forgejo
    /// origin and the Navigator host. Releases install from Forgejo's own package registry.
    #[arg(long, conflicts_with = "cleanup_registration")]
    egress_host: Vec<String>,
    /// `NodeLocal` DNS IPv4 permitted on TCP/UDP 53 beside `kube-dns`; defaults to GKE's `NodeLocal` DNS cache.
    #[arg(long, default_value = "169.254.20.10")]
    dns_ip: String,
    /// Recover an interrupted invocation's exact numeric runner registration.
    #[arg(long, requires = "cleanup_job")]
    cleanup_registration: Option<u64>,
    /// Exact forgejo-job-<UUID> Job from the interrupted invocation; never a namespace-wide cleanup.
    #[arg(long, requires = "cleanup_registration")]
    cleanup_job: Option<String>,
}

#[derive(Subcommand)]
enum GithubCmd {
    /// Reconcile a repository's GitHub policy. Any repository in an
    /// admissible organization on the host `NAVIGATOR_GIT_HOST` names may be
    /// reconciled — the public organization holding Navigator, and this
    /// deployment's own `NAVIGATOR_GITHUB_ORG`; anything else is refused
    /// before a token is read. Idempotent: a re-run reads the live rulesets
    /// and labels and writes only a difference.
    Setup {
        /// Repository to reconcile as `owner/name`. Defaults to
        /// `GITHUB_REPOSITORY`, then to this checkout's `origin` remote.
        repository: Option<String>,
        /// Print the reconciliation plan without writing GitHub.
        #[arg(long)]
        dry_run: bool,
        /// Exact release tag a confirmed Project repository's reconciled
        /// `ci.yml`/`cd.yml` callers pin Navigator's reusable workflows to.
        /// Defaults to this binary's own version when — and only when — that
        /// version is one this repository has actually published. Unused,
        /// and never validated, against a repository this content
        /// reconciliation does not apply to.
        #[arg(long, default_value = published_cli_version())]
        action_version: String,
        /// `owner/repo` a confirmed Project repository's reconciled
        /// `ci.yml`/`cd.yml` callers pin the reusable project-gate/publish
        /// workflows to. Defaults to this repository. Once this repository is
        /// private, a public Project repository cannot call a reusable
        /// workflow hosted here (ENG-931), so the cutover repoints this at an
        /// always-public home. Pass it only against a Project fixture proving
        /// that home, never against a real repository before that home
        /// carries the relocated workflows.
        #[arg(long, default_value = projects::repository::DEFAULT_WORKFLOW_HOME)]
        workflow_home: String,
    },
    /// Put a pull request on the path to `main` under the `production`
    /// ruleset: arm squash auto-merge, or — when every required check has
    /// already passed, so GitHub has nothing left to queue — squash-merge it
    /// now, pinned to the head commit whose checks were read. Drafts, closed
    /// pull requests, and ones already armed are left alone. Reads `GITHUB_TOKEN`;
    /// in CI that is the merge App's token, never the workflow's own.
    AutoMerge {
        /// Repository as `owner/name`. Defaults to `GITHUB_REPOSITORY`, then
        /// to this checkout's `origin` remote.
        repository: Option<String>,
        /// The pull request number.
        #[arg(long)]
        pull_request: u64,
        /// Seconds to wait for the merge-queue App's `automerge` workflow to
        /// arm the pull request before arming it as the caller. The App needs
        /// roughly ten to fifteen seconds after a pull request opens, so an
        /// operator opening Project pull requests passes about 90 here and
        /// arms by hand only as the fallback. 0 arms straight away.
        #[arg(long, value_name = "SECONDS", default_value_t = 0)]
        await_app: u64,
    },
    /// Refuse a revision range that contains an unsigned commit. Cloud Agent
    /// pull-request heads carry an HSM `gpgsig`; a session that turned
    /// `commit.gpgsign` off does not. Squash-merge still writes a GitHub-signed
    /// commit on `main`, so this is the check that the PR head itself is signed.
    CheckSignatures {
        /// Exclusive start of the range (`base..head`).
        #[arg(long)]
        base: String,
        /// Inclusive end of the range.
        #[arg(long, default_value = "HEAD")]
        head: String,
        /// Git directory to inspect.
        #[arg(long, default_value = ".")]
        git_dir: PathBuf,
    },
}

#[derive(Subcommand)]
enum AssetsAction {
    /// Resize + re-encode every manifest photo into AVIF + WebP + JPEG
    /// width variants under `<out>/img/<slug>/`. Run after editing the
    /// `views::assets::GALLERY` manifest or replacing a source photo;
    /// variant paths are stable, so a bounded cache TTL serves the new
    /// bytes once the old ones expire (no cache-bust token).
    Build {
        /// Directory holding the source photos, named by each
        /// manifest entry's `source` field.
        #[arg(long, default_value = "/tmp/nav-photo-work/assets_src/jpeg")]
        src: PathBuf,
        /// Output root; variants land under `<out>/img/<slug>/`.
        /// Defaults to the crate-bundled `/public` mount so a local
        /// dev loop / `cargo test` serves the variants from `/public`.
        #[arg(long, default_value = "server/public")]
        out: PathBuf,
        /// Build only these manifest slugs (repeatable). Adding one photo
        /// otherwise needs every other photo's source JPEG on disk, since
        /// the build walks the whole manifest. An unknown slug is an
        /// error, not an empty run.
        #[arg(long = "only")]
        only: Vec<String>,
    },
    /// Push the built variant tree to the public assets bucket via the
    /// `cloud` crate's `StorageService` (never the GCP SDK directly).
    /// Each file lands under key `img/<slug>/<slug>-<w>w.<ext>` with a
    /// bounded `Cache-Control` (~1 week, no `immutable`). Run after
    /// `navigator-deploy ops assets build`. Auth is ADC; the emulator endpoint is honored
    /// via `NAVIGATOR_STORAGE_ENDPOINT`.
    Upload {
        /// Directory holding the built variant tree.
        #[arg(long, default_value = "server/public/img")]
        dir: PathBuf,
        /// Target bucket. Defaults to `NAVIGATOR_ASSETS_BUCKET` — the
        /// public `<project>-assets` bucket, deliberately distinct from
        /// the app's documents bucket (`NAVIGATOR_DOCUMENTS_BUCKET`) so an
        /// upload never writes photos into the documents lane.
        #[arg(long, env = "NAVIGATOR_ASSETS_BUCKET")]
        bucket: Option<String>,
    },
    /// Restore the gitignored `server/public/img/` tree from the public
    /// assets bucket — the inverse of `upload`, for local development.
    /// A fresh clone has empty photo slots (`server/public/img/` is in
    /// `.gitignore`); this downloads every variant under the bucket's
    /// `img/` prefix so the `/public` mount serves the photos again,
    /// without the original source JPEGs. Read-only against the bucket;
    /// auth is ADC, the emulator endpoint is honored via
    /// `NAVIGATOR_STORAGE_ENDPOINT`.
    Pull {
        /// Output root; variants land under `<out>/<slug>/<file>` (the
        /// bucket's `img/` prefix is stripped). Defaults to the `/public`
        /// mount so a local dev loop serves them immediately.
        #[arg(long, default_value = "server/public/img")]
        out: PathBuf,
        /// Source bucket. Defaults to `NAVIGATOR_ASSETS_BUCKET` — the
        /// public `<project>-assets` bucket.
        #[arg(long, env = "NAVIGATOR_ASSETS_BUCKET")]
        bucket: Option<String>,
    },
    /// Report objects in the assets bucket that nothing on the site
    /// reaches — the inverse of `verify`, which only catches the opposite
    /// failure. `upload` never deletes, so an image or clip dropped from a
    /// page stays publicly fetchable at its URL indefinitely; this names
    /// those. Reachability is the union of markdown `](img/…)` references
    /// and the `views::assets::GALLERY` variants (referenced from Rust
    /// views, never from markdown), and only the `img/` prefix is
    /// considered, so `fonts/` is never reported. Report-only by design:
    /// it never deletes, because a wrong reachable set in a pruning tool
    /// would remove live production photographs.
    Orphans {
        /// Content root scanned for markdown image references.
        #[arg(long, default_value = "server/content")]
        content: PathBuf,
        /// Bucket to inspect. Defaults to `NAVIGATOR_ASSETS_BUCKET`.
        #[arg(long, env = "NAVIGATOR_ASSETS_BUCKET")]
        bucket: Option<String>,
        /// Also post the report to the ops Slack channel via the
        /// `SLACK_WEBHOOK_URL` incoming webhook — the same seam the
        /// durable workflows' heartbeat uses. The report names bucket
        /// object keys only, which are already public URLs.
        #[arg(long)]
        slack: bool,
    },
    /// Check that every `![](img/…)` image referenced by the content
    /// tree is actually published at the public origin. `server/public/img/`
    /// is gitignored and no CI step uploads it, so a post can merge and
    /// deploy with a hero that 404s; this gate catches that by fetching
    /// each referenced URL (auth-free `HEAD`, exactly as a browser would)
    /// and failing if any is missing. Run it after `assets upload`,
    /// before shipping.
    Verify {
        /// Content root scanned for markdown image references.
        #[arg(long, default_value = "server/content")]
        content: PathBuf,
        /// Public origin the images are served from. Defaults to
        /// `NAVIGATOR_ASSET_BASE_URL` (the bucket's public origin in
        /// production); pass `--base-url http://localhost:PORT/public`
        /// to check a running local dev loop.
        #[arg(long, env = "NAVIGATOR_ASSET_BASE_URL")]
        base_url: Option<String>,
    },
    /// Download every content-referenced `img/…` object from a public HTTP
    /// origin into `server/public/` for a local run that should serve the
    /// real published bytes. Auth-free — no GCP ADC — using the same
    /// origin `verify` probes.
    FetchReferenced {
        /// Content root scanned for markdown image references.
        #[arg(long, default_value = "server/content")]
        content: PathBuf,
        /// Output root; each `img/slug/file` reference lands at
        /// `<out>/img/slug/file` (default `server/public`).
        #[arg(long, default_value = "server/public")]
        out: PathBuf,
        /// Public origin to download from. Defaults to
        /// `NAVIGATOR_ASSET_BASE_URL` (the bucket's public HTTPS origin).
        #[arg(long, env = "NAVIGATOR_ASSET_BASE_URL")]
        base_url: Option<String>,
    },
    /// Materialize tiny placeholder bytes for every content-referenced
    /// `img/…` path and for the licensed GORP faces. This is for ephemeral
    /// CI image builds that already verified the real public origin and only
    /// need the KIND `/public` mount to serve decodable files at the same
    /// paths; real photos and licensed fonts stay in the public assets
    /// bucket and out of git.
    StubReferenced {
        /// Content root scanned for markdown image references.
        #[arg(long, default_value = "server/content")]
        content: PathBuf,
        /// Output root; each `img/slug/file` reference lands at
        /// `<out>/img/slug/file` (default `server/public`).
        #[arg(long, default_value = "server/public")]
        out: PathBuf,
    },
    /// Publish licensed webfonts from an operator-controlled directory to the
    /// public assets bucket. The font bytes stay out of git; see
    /// `docs/assets.md`.
    Fonts {
        #[command(subcommand)]
        action: FontAction,
    },
}

/// Which web font family `assets fonts upload` publishes. Each variant names
/// one entry of [`assets::BUCKET_FONT_FAMILIES`] — its own bucket directory
/// and filename stem — so a brand's typeface is a new variant here plus a new
/// row in that table, never a second command.
///
/// `font_family_arg_names_every_bucket_family` holds the two together: a
/// family the table publishes and verifies but `--family` cannot name is one
/// an operator has no way to upload.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum FontFamilyArg {
    /// The firm's licensed serif, from `TrashType`.
    GorpSerif,
    /// DeleteYourData.com's OFL-1.1 sans, self-hosted on the same
    /// operator-upload lane as GORP's licensed delivery.
    PlusJakartaSans,
    /// Vesta Estate Planning's heading and body face.
    EbGaramond,
    /// Misericordia Injury Law's body face.
    // Both `Source` values are spelled out because clap's derivation drops the
    // hyphen before a trailing digit, and the value has to equal the bucket
    // directory.
    #[value(name = "source-sans-3")]
    SourceSans3,
    /// Misericordia Injury Law's display face.
    #[value(name = "source-serif-4")]
    SourceSerif4,
    /// Abhaya Immigration's face.
    Mukta,
    /// DeleteYourDebt.com's face.
    PublicSans,
    /// The NYC summons practice's face.
    LibreFranklin,
    /// `CyberInjuryLaw` display face.
    BarlowCondensed,
    /// `CyberInjuryLaw` body face.
    DmSans,
}

impl FontFamilyArg {
    fn resolve(self) -> &'static assets::FontFamily {
        match self {
            Self::GorpSerif => &assets::GORP_SERIF,
            Self::PlusJakartaSans => &assets::PLUS_JAKARTA_SANS,
            Self::EbGaramond => &assets::EB_GARAMOND,
            Self::SourceSans3 => &assets::SOURCE_SANS_3,
            Self::SourceSerif4 => &assets::SOURCE_SERIF_4,
            Self::Mukta => &assets::MUKTA,
            Self::PublicSans => &assets::PUBLIC_SANS,
            Self::LibreFranklin => &assets::LIBRE_FRANKLIN,
            Self::BarlowCondensed => &assets::BARLOW_CONDENSED,
            Self::DmSans => &assets::DM_SANS,
        }
    }
}

#[derive(Subcommand)]
enum FontAction {
    /// Upload every required WOFF2 face for a licensed web font family to
    /// its bucket prefix in the public assets bucket. Auth is ADC; this is
    /// an operator action and the source directory is never committed.
    Upload {
        /// Directory containing every required `<Family>-<Weight>.woff2`
        /// file from the operator's delivery.
        #[arg(long)]
        dir: PathBuf,
        /// Target bucket. Defaults to `NAVIGATOR_ASSETS_BUCKET` — the public
        /// `<project>-assets` bucket.
        #[arg(long, env = "NAVIGATOR_ASSETS_BUCKET")]
        bucket: Option<String>,
        /// Which font family to publish. Defaults to GORP Serif, the
        /// original single-family command's behavior.
        #[arg(long, value_enum, default_value_t = FontFamilyArg::GorpSerif)]
        family: FontFamilyArg,
    },
    /// Package the licensed GORP Serif `.otf` desktop family into one ZIP and
    /// upload it to `fonts/gorp-serif/gorp-serif-otf.zip`, where the policy-gated
    /// `/app/team/fonts/gorp-serif.zip` route serves it. Unlike `upload` (public
    /// WOFF2 web faces), the installable family is a restricted download and
    /// goes to the *private* documents bucket; the `.otf` source is never
    /// committed.
    UploadDesktop {
        /// Directory of GORP Serif `.otf` faces from the licensed delivery.
        #[arg(long)]
        dir: PathBuf,
        /// Target bucket. Defaults to `NAVIGATOR_DOCUMENTS_BUCKET` — the
        /// private `<project>-documents` bucket, so a direct object URL can
        /// never bypass the route's authorization.
        #[arg(long, env = "NAVIGATOR_DOCUMENTS_BUCKET")]
        bucket: Option<String>,
    },
}

#[derive(Subcommand)]
enum FormsAction {
    /// Vendor + verify the blank government forms in the assets
    /// bucket. For each registry form: a local working copy at
    /// `templates/notations/<object_path>` (untracked) is uploaded and its
    /// repo `.sha256` pin rewritten; without one, the bucket object
    /// is pulled and verified against the pin. A missing object or a
    /// pin mismatch fails loudly. Auth is ADC; the emulator endpoint
    /// is honored via `NAVIGATOR_STORAGE_ENDPOINT`.
    Sync {
        /// Target bucket. Defaults to `NAVIGATOR_ASSETS_BUCKET`.
        #[arg(long, env = "NAVIGATOR_ASSETS_BUCKET")]
        bucket: Option<String>,
    },
    /// Print a blank's `AcroForm` `/T` field names, one per line,
    /// pulled from the assets bucket and verified against the repo
    /// `.sha256` pin first — the ground truth for authoring a
    /// `.fields.toml` or re-authoring the field layer (`/T` name =
    /// question code). No guessing: these are the names on the exact
    /// bytes the workflows fill.
    Fields {
        /// Form code, e.g. `nv__llc_formation`.
        code: String,
        /// Source bucket. Defaults to `NAVIGATOR_ASSETS_BUCKET`.
        #[arg(long, env = "NAVIGATOR_ASSETS_BUCKET")]
        bucket: Option<String>,
    },
    /// Re-author a blank's field layer so its `AcroForm` `/T` names
    /// *are* questionnaire state paths (#256): the form's
    /// `.fields.toml` — the recorded human mapping judgment — drives
    /// every rename, checkbox-pair → radio merge, and pre-printed
    /// literal; unmapped fields land in the `unmapped__` namespace.
    /// Writes the transformed working copy to `templates/notations/<object_path>`
    /// plus its diffable `.fields` manifest; visual QA, `forms sync`,
    /// and deleting the consumed `.fields.toml` remain human steps.
    ReAuthor {
        /// Form code, e.g. `nv__llc_formation`.
        code: String,
        /// Source bucket. Defaults to `NAVIGATOR_ASSETS_BUCKET`.
        #[arg(long, env = "NAVIGATOR_ASSETS_BUCKET")]
        bucket: Option<String>,
    },
}

#[derive(Subcommand)]
enum BlogCmd {
    /// Record an already-published website Post without sending to social providers.
    Record { file: std::path::PathBuf },
    /// Check stored destinations and server configuration without uploading or sending.
    Preflight {
        brand: String,
        slug: String,
        /// Requested providers, separated by commas (only these destinations are checked).
        #[arg(long, value_delimiter = ',', required = true, num_args = 1..)]
        providers: Vec<String>,
    },
    /// Publish or schedule a Post from a YAML file.
    Publish { file: std::path::PathBuf },
    /// Read the Post and its provider publication receipts.
    Status { brand: String, slug: String },
    /// Record the public handle a Brand holds on a social provider.
    Handle {
        /// The registered Brand key, e.g. `neon`.
        brand: String,
        /// `facebook`, `instagram`, `threads`, `youtube`, `linkedin`, `x`, or `tiktok`.
        provider: String,
        /// The provider's own public name, without the leading `@`.
        handle: String,
    },
    /// Encode a Post's link to a provider after the fact.
    Link {
        brand: String,
        /// The Post's slug.
        slug: String,
        provider: String,
        /// The public provider post URL.
        url: String,
    },
}

#[derive(Subcommand)]
enum ClauseCmd {
    /// List pinned Clause sources and their notation kinds.
    List,
    /// Read the wording and required inputs of one pinned source.
    Show { source: String },
}

#[derive(Subcommand)]
enum GlossaryCmd {
    /// List every term by its title and one-sentence description.
    List,
    /// Print one term's definition. Accepts its title in any case.
    Show {
        /// Term title, e.g. `"Lawyer Review"`.
        term: String,
    },
    /// Print the glossary as one Markdown page Notion can hold: every
    /// repository-relative link resolved to a public GitHub URL and
    /// sibling-term links unlinked. The push half of the Notion round trip.
    Notion,
}

#[derive(Subcommand)]
enum LspAction {
    /// Push prebuilt `navigator-lsp` binaries to the public assets
    /// bucket at `lsp/<triple>/navigator-lsp`. `--dir` is the cross-build
    /// output root laid out as `<dir>/<triple>/navigator-lsp` (see
    /// `docs/lsp/README.md`); a target whose binary is absent is skipped,
    /// not an error. Auth is ADC; the emulator endpoint is honored via
    /// `NAVIGATOR_STORAGE_ENDPOINT`.
    Publish {
        /// Directory holding the per-target binaries
        /// (`<dir>/<triple>/navigator-lsp`).
        #[arg(long, default_value = "target/lsp-dist")]
        dir: PathBuf,
        /// Target bucket. Defaults to `NAVIGATOR_ASSETS_BUCKET` — the
        /// public `<project>-assets` bucket, distinct from the documents
        /// lane so the product binary never lands among confidential
        /// client documents.
        #[arg(long, env = "NAVIGATOR_ASSETS_BUCKET")]
        bucket: Option<String>,
    },
}

/// `navigator-deploy site authorities create` — the citation apparatus' write path
/// (ENG-712): file a global Authority (no `--project`; it carries no
/// `project_id`) with its archived artifact, through
/// `POST /app/api/authorities`.
#[derive(Subcommand)]
enum AuthoritiesAction {
    /// Create (or find) the global Authority for `--citation`, archiving
    /// `--file` as its artifact. Repeating an already-recorded citation
    /// returns the existing Authority untouched — its original archive and
    /// metadata are never replaced.
    Create {
        #[command(flatten)]
        host: HostOpt,
        /// One of `case_law`, `statute`, `regulation`, `administrative`, `secondary`.
        #[arg(long)]
        class: String,
        /// The citation — this is the Authority's identity: a second
        /// `create` with the same citation returns the first row.
        #[arg(long)]
        citation: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        short_cite: Option<String>,
        #[arg(long)]
        publisher: Option<String>,
        #[arg(long)]
        issued_on: Option<String>,
        #[arg(long)]
        canonical_url: Option<String>,
        #[arg(long)]
        checked_on: Option<String>,
        /// Path to the artifact to archive (the full text — a PDF or slip
        /// opinion, never committed to a Project repository).
        #[arg(long)]
        file: PathBuf,
        /// MIME type. Defaults to `application/octet-stream`.
        #[arg(long)]
        content_type: Option<String>,
    },
    /// Correct a field on an existing Authority, found by `<ID>` or
    /// `--citation`. Fields left out stay unchanged. `--citation` and
    /// `--class` are immutable — they are the Authority's identity;
    /// changing either means a new Authority, not an update of this one.
    Update {
        #[command(flatten)]
        host: HostOpt,
        /// The Authority's id. Give this or `--citation`, not both.
        id: Option<uuid::Uuid>,
        /// Look up the Authority by citation instead of id.
        #[arg(long)]
        citation: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        short_cite: Option<String>,
        #[arg(long)]
        publisher: Option<String>,
        #[arg(long)]
        issued_on: Option<String>,
        #[arg(long)]
        canonical_url: Option<String>,
        #[arg(long)]
        checked_on: Option<String>,
        /// A new artifact version to archive in place of the current one.
        #[arg(long)]
        file: Option<PathBuf>,
        /// MIME type of `--file`. Defaults to `application/octet-stream`.
        #[arg(long)]
        content_type: Option<String>,
    },
}

/// `navigator-deploy site asset upload` — the OAuth-backed sibling of the
/// ADC-backed `navigator-deploy ops assets upload` batch command (`ops assets`
/// above): one asset, one or more deployments, no bucket credential.
#[derive(Subcommand)]
enum AssetAction {
    /// Prepare a separate upload-ready MP4 locally. Requires ffmpeg and ffprobe.
    /// Keeps the original, refuses output replacement, and verifies decoding.
    PrepareVideo {
        /// Original local MP4, up to 2 GiB and 3–600 seconds.
        input: PathBuf,
        /// New MP4 path; inspect the result before uploading it separately.
        output: PathBuf,
    },
    /// Upload one local public-safe asset to one or more logged-in hosts,
    /// through `POST /app/api/assets`. `ASSET_NAME` is the local path to
    /// read; when it lives under `server/public/`, its relative path is
    /// also the bucket key it is stored — and served back — under, e.g.
    /// `img/vesta-home/logo.svg` or `brand/rabbit.svg`. Content type is
    /// always derived from the key's extension.
    Upload {
        /// Target host. Repeatable — pass `--host` once per deployment
        /// (e.g. staging and production) to publish to each independently.
        /// Each host's result is reported on its own line; the command
        /// exits non-zero if any requested host fails.
        #[arg(long = "host", required = true)]
        hosts: Vec<String>,
        /// Local path to the asset. Under `server/public/`, or paired with
        /// `--key`.
        asset_name: PathBuf,
        /// Explicit bucket key. Required when `ASSET_NAME` lives outside
        /// `server/public/`; overrides the default relative-path key.
        #[arg(long)]
        key: Option<String>,
    },
}

/// How an executed document was executed, as `site document upload` records
/// it. Every flag is optional.
#[derive(clap::Args)]
struct ExecutionArgs {
    /// A YAML file with the expense amount, currency, payee, category, and source ID.
    #[arg(long, value_parser = read_receipt_metadata, requires = "file")]
    receipt: Option<store::documents::ReceiptMetadata>,
    /// How the executed document was signed: `wet_ink` or `docusign`.
    /// Optional; given together with `--signed-on`.
    #[arg(long, value_parser = parse_signing_method, requires = "signed_on")]
    signing_method: Option<store::documents::SigningMethod>,
    /// The date it was signed, `YYYY-MM-DD`. Given together with
    /// `--signing-method`.
    #[arg(long, value_parser = parse_calendar_date, requires = "signing_method")]
    signed_on: Option<chrono::NaiveDate>,
    /// One notarial acknowledgment, repeatable for a packet with several:
    /// `method=in_person|remote_online,acknowledged_on=YYYY-MM-DD,state=NV,county=Clark,commission_expires=YYYY-MM-DD`.
    /// Never the notary's name or commission number.
    #[arg(long, value_parser = parse_notarization)]
    notarization: Vec<store::documents::Notarization>,
    /// The exhibit metadata the matter's evidence index reads (LAW-185).
    #[command(flatten)]
    evidence: EvidenceArgs,
}

/// The exhibit metadata `site document upload` records for the evidence
/// index. Every flag is optional; the block is recorded only when at least
/// one is given.
#[derive(clap::Args)]
struct EvidenceArgs {
    /// The date the exhibit itself bears, `YYYY-MM-DD` — a letter's own
    /// date, a photograph's capture date — not the filing date.
    #[arg(long = "evidence-dated-at", value_parser = parse_calendar_date)]
    dated_at: Option<chrono::NaiveDate>,
    /// Where the exhibit came from, one line — a person, an agency, a URL.
    #[arg(long = "evidence-source")]
    source: Option<String>,
    /// How confidently the exhibit is tied to what it supports:
    /// `exact`, `inferred`, or `unknown`.
    #[arg(long = "evidence-confidence", value_parser = parse_evidence_confidence)]
    confidence: Option<store::documents::EvidenceConfidence>,
    /// A claim reference the exhibit bears on, repeatable — exactly the
    /// reference the reader should follow, `¶ 12` or
    /// `Count II — Conversion`, not prose.
    #[arg(long = "evidence-supports")]
    supports: Vec<String>,
}

impl EvidenceArgs {
    fn record(self) -> Option<store::documents::Evidence> {
        let any = self.dated_at.is_some()
            || self.source.is_some()
            || self.confidence.is_some()
            || !self.supports.is_empty();
        any.then_some(store::documents::Evidence {
            dated_at: self.dated_at,
            source: self.source,
            confidence: self.confidence,
            supports: self.supports,
        })
    }
}

fn read_receipt_metadata(path: &str) -> Result<store::documents::ReceiptMetadata, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("read receipt metadata: {error}"))?;
    let receipt: store::documents::ReceiptMetadata =
        serde_yaml::from_slice(&bytes).map_err(|_| "invalid receipt metadata YAML".to_string())?;
    receipt
        .validate("receipt")
        .map_err(|error| error.to_string())?;
    Ok(receipt)
}

impl ExecutionArgs {
    fn record(self) -> remote::DocumentExecution {
        remote::DocumentExecution {
            receipt: self.receipt,
            signing: self
                .signing_method
                .zip(self.signed_on)
                .map(|(method, signed_on)| store::documents::Signing { method, signed_on }),
            notarization: self.notarization,
            evidence: self.evidence.record(),
        }
    }
}

/// A matter document is only ever reached through a Project on a site, so
/// every verb here is either a write against a named `--project` on a
/// `--host` brand deployment (`upload`), or a read that resolves both from
/// the checkout's own `navigator.yaml` — a pointer path below `blobs/` (the
/// committed `.yaml`), or the bytes below `documents/` it names, is enough for
/// a lawyer to name a file rather than an id or a host.
// The `upload` variant flattens every flag clap cannot box, so it is far
// larger than the read variants; the enum is built once per process run.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum DocumentAction {
    /// File a local document into a matter (`POST /app/api/projects/{id}/documents`).
    ///
    /// Writes the returned `blobs/<slug>.yaml` pointer when the file belongs
    /// to a checkout for this Project. `navigator project gate --check`
    /// reconciles the other pointers this login can see.
    #[command(after_long_help = DOCUMENT_UPLOAD_KIND_HELP)]
    Upload {
        #[command(flatten)]
        host: HostOpt,
        /// Matter code (human-facing) to file into. Resolved against the
        /// matters this login can see.
        #[arg(long)]
        project: String,
        /// Path to the file to upload.
        #[arg(long, required_unless_present = "dir", conflicts_with = "dir")]
        file: Option<PathBuf>,
        /// Upload regular files in this directory, sorted by name, without recursing.
        #[arg(long, required_unless_present = "file", conflicts_with_all = ["file", "slug"])]
        dir: Option<PathBuf>,
        /// Required asset-lane kind. An invalid value prints the accepted enum.
        #[arg(long, value_parser = parse_asset_kind)]
        kind: String,
        /// `client` makes the document client-visible; default is `internal`.
        #[arg(long, value_parser = parse_document_visibility, default_value = "internal")]
        visibility: String,
        /// Optional description stored with the document.
        #[arg(long)]
        description: Option<String>,
        /// MIME type. Defaults to `application/octet-stream`.
        #[arg(long)]
        content_type: Option<String>,
        /// Stable document identity: a path relative to `documents/`, mirrored
        /// under `blobs/` where the pointer lands (`--slug memos/x.md` writes
        /// `blobs/memos/x.md.yaml`; a bare `x.md` writes it at the root).
        /// It must keep the source file's extension (`--slug motion.pdf` for
        /// `motion.pdf`). When omitted, a file in a folder below
        /// `documents/` defaults to its own path there, so the pointer
        /// mirrors the source; any other file defaults to its filename. A
        /// pointer already in `blobs/` for `--file` names the slug, and a
        /// different `--slug` is refused. A matter folder is not part of the
        /// slug: `documents/Discovery/set-one/rogs.pdf` is `set-one/rogs.pdf`.
        #[arg(long)]
        slug: Option<String>,
        /// The matter folder `navigator project sync` downloads the bytes
        /// into: `Client Production`, `Correspondence (Served)`, `Discovery`,
        /// `Docket Filings`, or `Work Product`. When omitted, the folder
        /// `--file` sits in, else the one `--kind` maps to.
        #[arg(long, value_parser = parse_matter_folder)]
        folder: Option<store::document_pointers::MatterFolder>,
        /// Transcript quality marker. Only accepted when `--kind transcript`.
        #[arg(long, value_parser = parse_transcript_quality)]
        quality: Option<String>,
        #[command(flatten)]
        execution: ExecutionArgs,
    },
    /// OCR a PDF revision into a linked transcript document, without sending
    /// the source bytes to an OCR provider. Requires local Poppler (`pdftoppm`)
    /// and Tesseract with English and orientation data installed.
    #[command(after_long_help = DOCUMENT_TRANSCRIBE_HELP)]
    Transcribe {
        /// Source document pointer under `blobs/`, or its bytes under `documents/`.
        pointer: PathBuf,
    },
    /// Set `slug` on a live row that has none
    /// (`PATCH /app/api/projects/{id}/documents/{asset_id}`).
    ///
    /// Optionally sets `kind` in the same write. Refuses a row that already
    /// has a slug, and a slug that already names a revision chain. After it
    /// succeeds, `navigator project gate --check` can write the pointer.
    #[command(after_long_help = DOCUMENT_UPLOAD_KIND_HELP)]
    Slug {
        #[command(flatten)]
        host: HostOpt,
        /// Matter code (human-facing). Resolved against the matters this login can see.
        #[arg(long)]
        project: String,
        /// Asset id of the slugless row.
        asset_id: uuid::Uuid,
        /// Document identity. Must carry the stored filename's extension.
        #[arg(long)]
        slug: String,
        /// Asset-lane kind. Omit to leave the stored kind unchanged; pass it
        /// whenever the stored kind is not an accepted one, because a slugged
        /// row's kind otherwise needs `navigator-deploy site document kind`.
        #[arg(long, value_parser = parse_asset_kind)]
        kind: Option<String>,
        /// Validate and print the result. The row is left unchanged.
        #[arg(long)]
        dry_run: bool,
    },
    /// Replace a legacy `kind` the asset lane rejects on a live row and its
    /// slug chain (`PATCH /app/api/projects/{id}/documents/{asset_id}`).
    ///
    /// Refuses a row whose kind is already accepted, so it never changes one
    /// valid classification into another. `sha256`, the storage key, and the
    /// slug stay unchanged. After it succeeds, `navigator project gate
    /// --check` can rewrite the pointer.
    #[command(after_long_help = DOCUMENT_UPLOAD_KIND_HELP)]
    Kind {
        #[command(flatten)]
        host: HostOpt,
        /// Matter code (human-facing). Resolved against the matters this login can see.
        #[arg(long)]
        project: String,
        /// Asset id of the row whose kind the asset lane rejects.
        asset_id: uuid::Uuid,
        /// Asset-lane kind to write.
        #[arg(long, value_parser = parse_asset_kind)]
        kind: String,
        /// Validate and print the result. The row is left unchanged.
        #[arg(long)]
        dry_run: bool,
    },
    /// Change a document chain from one accepted kind to another
    /// (`POST /app/api/projects/{id}/documents/{asset_id}/reclassify`).
    ///
    /// The correction for a document filed under the wrong valid kind. Every
    /// revision of the slug chain moves together; `sha256`, the storage key,
    /// byte size, and the slug stay unchanged. Refuses a row whose kind is not
    /// accepted (use `navigator-deploy site document kind`) and a target equal to the
    /// current kind. After it succeeds, `navigator project gate --check` can
    /// rewrite the pointer.
    #[command(after_long_help = DOCUMENT_UPLOAD_KIND_HELP)]
    Reclassify {
        #[command(flatten)]
        host: HostOpt,
        /// Matter code (human-facing). Resolved against the matters this login can see.
        #[arg(long)]
        project: String,
        /// Asset id of any revision in the chain to correct.
        asset_id: uuid::Uuid,
        /// Accepted asset-lane kind the chain moves to.
        #[arg(long, value_parser = parse_asset_kind)]
        kind: String,
        /// Validate and print the result. The row is left unchanged.
        #[arg(long)]
        dry_run: bool,
    },
    /// Change a filed document's visibility
    /// (`PATCH /app/api/projects/{id}/documents/{asset_id}`).
    ///
    /// Re-uploading the same bytes returns the existing revision unchanged, so
    /// this is the way to move a filed document between `internal` and
    /// `client`. No new revision is written; the slug, `sha256`, and storage
    /// key stay as they were. After it succeeds, `navigator project gate
    /// --check` rewrites the committed pointer's `visibility`.
    Visibility {
        #[command(flatten)]
        host: HostOpt,
        /// Matter code (human-facing). Resolved against the matters this login can see.
        #[arg(long)]
        project: String,
        /// Asset id of the revision whose visibility changes.
        asset_id: uuid::Uuid,
        /// `client` shows the document to the client; `internal` hides it.
        #[arg(long, value_parser = parse_document_visibility)]
        visibility: String,
    },
    /// Move a document identity to another slug valid for its filename, such
    /// as `notes/a.pdf` to `filed/forms/a.pdf`
    /// (`PATCH /app/api/projects/{id}/documents/{asset_id}`).
    ///
    /// Refuses a target equal to the current slug and a target that already
    /// names a chain on this matter. The whole revision chain moves together;
    /// `sha256`, the storage key, and `kind` stay unchanged. After a real
    /// move, the committed pointer under `--dir` and its sibling bytes follow
    /// the slug together, or the command leaves both in place.
    Reslug {
        #[command(flatten)]
        host: HostOpt,
        /// Matter code (human-facing). Resolved against the matters this login can see.
        #[arg(long)]
        project: String,
        /// Asset id of any revision of the document identity to move.
        asset_id: uuid::Uuid,
        /// Target slug. Must carry the stored filename's extension and must
        /// not already name a chain on this matter.
        #[arg(long)]
        slug: String,
        /// Project checkout whose `blobs/` pointer and `documents/` copy follow the move.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Validate and print the result. The row and the checkout are left unchanged.
        #[arg(long)]
        dry_run: bool,
    },
    /// Retire a mistaken document chain without a pointer-removal commit
    /// (`POST /app/api/projects/{id}/documents/{asset_id}/delete`).
    ///
    /// This keeps the chain recoverable for 180 days. It requires an explicit
    /// `--yes-retire`; afterward, remove any local pointer and byte copy for the retired slug.
    Retire {
        #[command(flatten)]
        host: HostOpt,
        /// Matter code (human-facing). Resolved against the matters this login can see.
        #[arg(long)]
        project: String,
        /// Asset id of any revision of the mistaken document chain.
        asset_id: uuid::Uuid,
        /// Validate the target and report the chain without changing it.
        #[arg(long)]
        dry_run: bool,
        /// Confirm soft-deletion of the entire revision chain for 180 days.
        #[arg(long)]
        yes_retire: bool,
    },
    /// Restore a soft-deleted document with its whole revision history
    /// (`POST /app/api/projects/{id}/documents/{asset_id}/restore`).
    ///
    /// Deleting a document's pointer and running `navigator project sync`
    /// soft-deletes it for 180 days; this undoes that within the window. Find
    /// the asset id with `navigator-deploy site document log <pointer>`. The next
    /// `project sync` writes the pointer again.
    Restore {
        #[command(flatten)]
        host: HostOpt,
        /// Matter code (human-facing). Resolved against the matters this login can see.
        #[arg(long)]
        project: String,
        /// Asset id of any revision of the soft-deleted document.
        asset_id: uuid::Uuid,
    },
    /// Place or release a Legal Hold on a document
    /// (`POST /app/api/projects/{id}/documents/{asset_id}/hold`).
    ///
    /// A held document is never purged after its 180-day soft-deletion window.
    Hold {
        #[command(flatten)]
        host: HostOpt,
        /// Matter code (human-facing). Resolved against the matters this login can see.
        #[arg(long)]
        project: String,
        /// Asset id of any revision of the document.
        asset_id: uuid::Uuid,
        /// Release the hold instead of placing it.
        #[arg(long)]
        release: bool,
    },
    /// Restore a missing storage object from a same-hash sibling in the matter
    /// (`POST /app/api/projects/{id}/documents/{asset_id}/storage`).
    ///
    /// Admin only. Copies the sibling's bytes onto the content-addressed key,
    /// or re-points the row at that key, then the server verifies the sha256.
    /// No sibling changes nothing.
    Repair {
        #[command(flatten)]
        host: HostOpt,
        /// Matter code (human-facing). Resolved against the matters this login can see.
        #[arg(long)]
        project: String,
        /// Asset id whose storage object is missing.
        asset_id: uuid::Uuid,
        /// Report the repair. Storage and the row are left unchanged.
        #[arg(long)]
        dry_run: bool,
    },
    /// The revision chain, newest first, marking the operative row.
    Log {
        /// Pointer below `blobs/`, such as `blobs/pleadings/motion.pdf.yaml`,
        /// or its bytes below `documents/`. The retired `.yml` spelling is
        /// still read.
        pointer: PathBuf,
    },
    /// Fetch one revision to a local path, verified by `sha256` and size
    /// before success is reported. Refuses a destination inside `documents/`
    /// except the pointer's own sync-managed copy, which it refreshes with the
    /// pointer's current revision.
    Get {
        pointer: PathBuf,
        /// Revision number to fetch. Defaults to the operative revision under
        /// your lens.
        #[arg(long)]
        version: Option<usize>,
        /// Where to write the fetched bytes. Outside `documents/`, or the
        /// pointer's own copy (`documents/<name>` for `blobs/<name>.yaml`).
        #[arg(long)]
        out: PathBuf,
    },
    /// A text redline between two revision numbers. PDF and plain text only;
    /// any other type is reported unsupported rather than diffed as bytes.
    Diff {
        pointer: PathBuf,
        a: usize,
        b: usize,
    },
}

#[derive(Subcommand)]
enum MailAction {
    /// File one inbound message's attachments into a matter, without the
    /// bytes ever touching this checkout.
    #[command(after_long_help = DOCUMENT_UPLOAD_KIND_HELP)]
    File {
        #[command(flatten)]
        host: HostOpt,
        /// Matter code (human-facing) to file into.
        #[arg(long)]
        project: String,
        /// `email_conversation_message` row id naming the inbound hop.
        #[arg(long)]
        message: uuid::Uuid,
        /// Required asset-lane kind, applied to every attachment.
        #[arg(long, value_parser = parse_asset_kind)]
        kind: String,
        /// `client` makes every filed attachment client-visible; default `internal`.
        #[arg(long, value_parser = parse_document_visibility, default_value = "internal")]
        visibility: String,
        /// List what would be filed, with size and content type; write nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum NotationAction {
    /// List the private notation inventory for one matter. The output includes
    /// the template code, workflow state, and respondent so an author can
    /// check for an existing instrument before opening another one.
    List {
        #[command(flatten)]
        host: HostOpt,
        /// Matter code whose notation inventory to inspect.
        #[arg(long)]
        project: String,
        /// Emit the raw JSON inventory.
        #[arg(long)]
        json: bool,
    },
    /// Create a questionnaire-driven notation on an existing matter and
    /// leave its questionnaire ready for the site intake flow.
    ///
    /// Every notation hangs on an already-existing Project, so `--project`
    /// (the matter code) is required — open the matter first with
    /// `navigator project create`. The template is read from that
    /// Project's git repo when authored there, else from the bundled firm
    /// catalog; the notation opens pinned to it
    /// (`POST /app/projects/<id>/notations/new`).
    #[command(hide = true)]
    Create {
        /// Template code, e.g. `onboarding__letter`,
        /// `offboarding__letter`, or `nv__llc_formation`.
        template_code: String,
        #[command(flatten)]
        host: HostOpt,
        /// Client email — the notation's bound client (signer). Must be an
        /// existing client on the matter.
        #[arg(long)]
        client_email: String,
        /// Matter **code** (human-facing, from `project create`) to open the
        /// notation inside. Resolved to the Project id against the matters
        /// you can see. Required — the matter must already exist.
        #[arg(long)]
        project: String,
        /// A saved answers file (as `site notation answers --save-answers`
        /// writes) to prefill the new notation from: every question the
        /// template asks is answered from the file's `state: value` map, so
        /// the notation's interpolated fields match the notation the file
        /// was saved from. A question the file does not answer fails the
        /// create — prefill is whole, never partial.
        #[arg(long = "answers-from", alias = "answers-file")]
        answers_from: Option<PathBuf>,
    },
    /// Print a notation's workflow state + signature request id
    /// (`GET …/review?format=json`).
    Status {
        /// Notation UUID.
        notation_id: uuid::Uuid,
        #[command(flatten)]
        host: HostOpt,
        /// Emit the raw JSON status body.
        #[arg(long)]
        json: bool,
    },
    /// Read the filed answers and their source provenance for one notation.
    Answers {
        /// Notation UUID.
        notation_id: uuid::Uuid,
        #[command(flatten)]
        host: HostOpt,
        /// Emit the raw JSON answers.
        #[arg(long)]
        json: bool,
        /// Save the filed answers to this path as a `state: value` YAML map
        /// for `--answers-from` / `--answers-file` prefill. Intended on a
        /// completed notation — typically an onboarding whose answers a
        /// follow-on notation should repeat.
        #[arg(long)]
        save_answers: Option<PathBuf>,
    },
    /// Render + park a notation's document for review (`POST
    /// …/approve-send`) — fills the bound packet (a formation's official
    /// Secretary-of-State form, or a retainer PDF). Idempotent once
    /// rendered.
    Approve {
        /// Notation UUID.
        notation_id: uuid::Uuid,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Send a notation parked at `lawyer_review` back for changes: flag the
    /// wrong answers so they get re-collected, then return to review
    /// (`POST …/request-changes`). A rejected review re-collects the wrong
    /// answers instead of dead-ending; declining the matter is separate.
    RequestChanges {
        /// Notation UUID.
        notation_id: uuid::Uuid,
        #[command(flatten)]
        host: HostOpt,
        /// A question code to flag for re-collection, e.g. `person__client`.
        /// Repeatable — one per answer to re-collect.
        #[arg(long = "question")]
        questions: Vec<String>,
        /// An optional note to the re-collector — what to fix.
        #[arg(long)]
        note: Option<String>,
    },
    /// Re-collect the flagged answers on a notation parked at
    /// `reask__client` (lawyer on the client's behalf) and resubmit for
    /// review (`POST …/reask`). Only the flagged answers are re-collected;
    /// every other answer stays as it was — answers and questions are
    /// decoupled, so a correction never re-walks the questionnaire.
    Update {
        /// Notation UUID.
        notation_id: uuid::Uuid,
        #[command(flatten)]
        host: HostOpt,
        /// A corrected answer as `code=value`, e.g.
        /// `person__client=Libra Jones`. Repeatable; each code must have
        /// been flagged by the review.
        #[arg(long = "answer")]
        answers: Vec<String>,
    },
    /// Download a notation's rendered document (the filled packet) to a
    /// local file (`GET …/documents/document`).
    Document {
        /// Notation UUID.
        notation_id: uuid::Uuid,
        /// Path to write the PDF to.
        #[arg(long)]
        out: PathBuf,
        #[command(flatten)]
        host: HostOpt,
    },
}

fn main() -> ExitCode {
    std::thread::Builder::new()
        .name("navigator-deploy".into())
        .stack_size(CLI_STACK_BYTES)
        .spawn(run)
        .expect("spawn the CLI thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

#[allow(clippy::too_many_lines)] // one flat dispatch match; splitting it hurts readability
fn run() -> ExitCode {
    // `.env` is picked up before `clap` reads its `env = "..."`
    // defaults. No-op when no file is present, so CI/cluster deploys
    // that inject env vars another way continue to work. The
    // `.devx/env` overlay carries values `devx up` derives at port-
    // forward time; `from_path` skips keys already set, so `.env`
    // wins. Deployment coordinates never ride this path: `ops ship`,
    // `ops observability`, and `ops secrets apply` read the repository's
    // `deployments/<name>/` tree through an explicit `--deployment` flag.
    let _ = dotenvy::dotenv();
    let _ = dotenvy::from_path(".devx/env");
    let runtime = || tokio::runtime::Runtime::new().expect("tokio runtime");
    let cli = Cli::from_arg_matches(&concise_help(Cli::command()).get_matches())
        .expect("Clap matches come from Cli's command tree");
    // `--license` is `exclusive`, so reaching here means it was the only
    // argument. Print the embedded terms and stop before any dispatch.
    // `write_all` rather than `print!`: these two are the only commands whose
    // output a reader routinely pipes into a pager and quits early, and `print!`
    // panics on the resulting broken pipe. An unread tail is not an error.
    if cli.license {
        print_license();
        return ExitCode::SUCCESS;
    }
    if cli.third_party_notices {
        print_third_party_notices();
        return ExitCode::SUCCESS;
    }
    let Some(cli_command) = cli.command else {
        // A bare `navigator-deploy` is a usage error, so the help goes to stderr and
        // the exit code matches what clap returns for a missing subcommand.
        eprint!("{}", concise_help(Cli::command()).render_help());
        return ExitCode::from(2);
    };
    match cli_command {
        Command::Pii { dir, analyzer_url } => {
            let Ok(findings) = runtime().block_on(cli::pii_audit::audit_tree(&dir, &analyzer_url))
            else {
                eprintln!("navigator: PII scan could not complete; check the local analyzer and repository allowlist");
                return ExitCode::from(2);
            };
            for finding in &findings {
                eprintln!("{}:{}: P002: {} detected; replace with synthetic data or review the PII allowlist",
                    finding.path, finding.line, finding.entity);
            }
            println!("PII scan: {} finding(s)", findings.len());
            if findings.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Command::Person { action } => runtime().block_on(run_person(action)),
        Command::Journal { firm, host, action } => {
            runtime().block_on(journal::run(firm, host, action))
        }
        Command::Assumptions { firm, host, action } => {
            runtime().block_on(assumptions::run(firm, host, action))
        }
        Command::LegalPlans { firm, host, action } => {
            runtime().block_on(legal_plans::run(firm, host, action))
        }
        Command::Mail { action } => runtime().block_on(mail::run(action)),
        Command::Inbox { action } => runtime().block_on(inbox::run(action)),
        Command::Downloads { action } => runtime().block_on(downloads::run(action)),
        Command::Validate {
            dir,
            fix,
            errors_only,
            ci: _,
        } => run_validate(&dir, fix, errors_only),
        Command::Projects { action } => runtime().block_on(run_projects(action)),
        Command::Preview { port } => runtime().block_on(project_preview::run(port)),
        Command::Scraper { action } => runtime().block_on(scraper::run(action)),
        // The docs reference helpers need no cluster, so they are handled
        // here rather than routed into the KIND dispatcher with the rest
        // of `dev`.
        Command::Clause { action } => match action {
            ClauseCmd::List => clauses::list(),
            ClauseCmd::Show { source } => clauses::show(&source),
        },
        Command::Glossary { action } => match action {
            GlossaryCmd::List => glossary::list(),
            GlossaryCmd::Show { term } => glossary::show(&term),
            GlossaryCmd::Notion => glossary::notion(),
        },
        Command::Forms { action } => match action {
            FormsAction::Sync { bucket } => forms_sync::run_sync(bucket.as_deref()),
            FormsAction::Fields { code, bucket } => {
                forms_sync::run_fields(&code, bucket.as_deref())
            }
            FormsAction::ReAuthor { code, bucket } => {
                forms_sync::run_reauthor(&code, bucket.as_deref())
            }
        },
        Command::Site { action } => match action {
            SiteCmd::Person {
                action: SitePersonCmd::Merge { keep, drop, host },
            } => runtime().block_on(remote::merge_people(host.host, &keep, &drop)),
            SiteCmd::Person {
                action: SitePersonCmd::SshKey { action },
            } => match action {
                SshKeyCmd::Add { file, label, host } => {
                    runtime().block_on(ssh_keys::add(host.host, &file, label.as_deref()))
                }
                SshKeyCmd::List { person, host } => {
                    runtime().block_on(ssh_keys::list(host.host, person))
                }
                SshKeyCmd::Remove { key, person, host } => {
                    runtime().block_on(ssh_keys::remove(host.host, &key, person))
                }
                SshKeyCmd::Sync { host } => runtime().block_on(ssh_keys::sync(host.host)),
            },
            SiteCmd::Firm { action } => match action {
                SiteFirmCmd::ProvisionForgejo { slug, host } => {
                    runtime().block_on(firm_forgejo::provision(host.host.as_deref(), &slug))
                }
                SiteFirmCmd::List { host } => runtime().block_on(firm_admin::run(
                    host.host.as_deref(),
                    firm_admin::Action::List,
                )),
                SiteFirmCmd::Rename { slug, name, host } => runtime().block_on(firm_admin::run(
                    host.host.as_deref(),
                    firm_admin::Action::Rename {
                        slug: &slug,
                        name: &name,
                    },
                )),
                SiteFirmCmd::Delete { slug, host } => runtime().block_on(firm_admin::run(
                    host.host.as_deref(),
                    firm_admin::Action::Delete { slug: &slug },
                )),
            },
            SiteCmd::Brand { action } => match action {
                SiteBrandCmd::Move { key, to, host } => runtime().block_on(firm_admin::run(
                    host.host.as_deref(),
                    firm_admin::Action::MoveBrand { key: &key, to: &to },
                )),
                SiteBrandCmd::Detach { key, host } => runtime().block_on(firm_admin::run(
                    host.host.as_deref(),
                    firm_admin::Action::DetachBrand { key: &key },
                )),
                SiteBrandCmd::Delete { key, host } => runtime().block_on(firm_admin::run(
                    host.host.as_deref(),
                    firm_admin::Action::DeleteBrand { key: &key },
                )),
            },
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
            SiteCmd::Login { host, no_browser } => {
                runtime().block_on(login::run_login(&host, no_browser))
            }
            SiteCmd::Seed { dir } => runtime().block_on(run_catalog_seed(&dir)),
            SiteCmd::Blog { host, action } => match action {
                BlogCmd::Preflight {
                    brand,
                    slug,
                    providers,
                } => runtime().block_on(blog::run_preflight(
                    host.as_deref(),
                    &brand,
                    &slug,
                    &providers,
                )),
                BlogCmd::Record { file } => {
                    runtime().block_on(blog::run_record(host.as_deref(), &file))
                }
                BlogCmd::Publish { file } => {
                    runtime().block_on(blog::run_publish(host.as_deref(), &file))
                }
                BlogCmd::Status { brand, slug } => {
                    runtime().block_on(blog::run_status(host.as_deref(), &brand, &slug))
                }
                BlogCmd::Handle {
                    brand,
                    provider,
                    handle,
                } => runtime().block_on(blog::run_handle(
                    host.as_deref(),
                    &brand,
                    &provider,
                    &handle,
                )),
                BlogCmd::Link {
                    brand,
                    slug,
                    provider,
                    url,
                } => runtime().block_on(blog::run_link(
                    host.as_deref(),
                    &brand,
                    &slug,
                    &provider,
                    &url,
                )),
            },
            SiteCmd::SyntheticPortfolio { target, apply } => {
                runtime().block_on(run_synthetic_portfolio(target, apply))
            }
            SiteCmd::Logout { host } => login::run_logout(host.as_deref()),
            SiteCmd::Whoami { host } => login::run_whoami(host.as_deref()),
            SiteCmd::Mcp { host } => runtime().block_on(mcp_bridge::run(host.as_deref())),
            SiteCmd::Document { action } => runtime().block_on(run_document(action)),
            SiteCmd::Mail { action } => match action {
                MailAction::File {
                    host,
                    project,
                    message,
                    kind,
                    visibility,
                    dry_run,
                } => runtime().block_on(remote::mail_file(
                    std::path::Path::new("."),
                    host.host.as_deref(),
                    &project,
                    message,
                    &kind,
                    &visibility,
                    dry_run,
                )),
            },
            SiteCmd::Notation { action } => runtime().block_on(run_notation(action)),
            SiteCmd::Authorities { action } => match action {
                AuthoritiesAction::Create {
                    host,
                    class,
                    citation,
                    title,
                    short_cite,
                    publisher,
                    issued_on,
                    canonical_url,
                    checked_on,
                    file,
                    content_type,
                } => runtime().block_on(authorities::create(
                    host.host.as_deref(),
                    &class,
                    &citation,
                    &title,
                    short_cite.as_deref(),
                    publisher.as_deref(),
                    issued_on.as_deref(),
                    canonical_url.as_deref(),
                    checked_on.as_deref(),
                    &file,
                    content_type.as_deref(),
                )),
                AuthoritiesAction::Update {
                    host,
                    id,
                    citation,
                    title,
                    short_cite,
                    publisher,
                    issued_on,
                    canonical_url,
                    checked_on,
                    file,
                    content_type,
                } => runtime().block_on(authorities::update(
                    host.host.as_deref(),
                    id,
                    citation.as_deref(),
                    title.as_deref(),
                    short_cite.as_deref(),
                    publisher.as_deref(),
                    issued_on.as_deref(),
                    canonical_url.as_deref(),
                    checked_on.as_deref(),
                    file.as_deref(),
                    content_type.as_deref(),
                )),
            },
            SiteCmd::Asset { action } => match action {
                AssetAction::PrepareVideo { input, output } => {
                    runtime().block_on(video_prepare::run_prepare(&input, &output))
                }
                AssetAction::Upload {
                    hosts,
                    asset_name,
                    key,
                } => runtime().block_on(site_asset::upload(&hosts, &asset_name, key.as_deref())),
            },
        },
        Command::Lsp { dir } => lsp_download::run_download(cli_version(), dir),
        Command::Nautilus { action } => match action {
            nautilus_preview::NautilusCmd::Preview { port } => {
                devx_result(runtime().block_on(nautilus_preview::run(port)))
            }
        },
        Command::Notation { action } => match action {
            NotationCmd::Preview {
                file,
                offline,
                host,
            } => devx_result(runtime().block_on(notations_preview::run(
                &file,
                offline,
                0,
                host.as_deref(),
            ))),
            NotationCmd::Kinds => notation_kinds::run(),
            NotationCmd::Pdf {
                file,
                out,
                answers,
                answers_file,
                answers_from,
                preview,
                ci,
            } => {
                let answers = match answers_file {
                    Some(path) => match answers_file::pdf_answers(&path, answers) {
                        Ok(answers) => answers,
                        Err(error) => {
                            eprintln!("navigator: {error:#}");
                            return ExitCode::from(2);
                        }
                    },
                    None => match runtime().block_on(remote::pdf_project_answers(
                        &file,
                        answers_from.as_deref(),
                        answers,
                    )) {
                        Ok(answers) => answers,
                        Err(error) => {
                            eprintln!("navigator: {error:#}");
                            return ExitCode::from(2);
                        }
                    },
                };
                run_render(&file, &out, &answers, "pdf", preview, ci)
            }
            NotationCmd::Word {
                file,
                out,
                answers,
                preview,
                redline_against,
                redline_author,
            } => match redline_against {
                Some(previous) => run_word_redline(
                    &file,
                    &out,
                    &answers,
                    preview,
                    &previous,
                    redline_author.as_deref(),
                ),
                None => run_render(&file, &out, &answers, "docx", preview, false),
            },
            NotationCmd::Import {
                file,
                edited,
                dry_run,
            } => notation_import::run(&file, &edited, dry_run),
        },
        // `lsp publish` and the `assets` pipeline
        // carry operator blast radius but are not cluster lifecycle, so they
        // are handled here rather than routed into the KIND/cloud dispatcher
        // below.
        Command::Ops(
            action @ (OpsCmd::Lsp { .. }
            | OpsCmd::Assets { .. }
            | OpsCmd::CutRelease { .. }
            | OpsCmd::PublicSnapshot { .. }
            | OpsCmd::ReleaseDefaultTag { .. }
            | OpsCmd::Release { .. }
            | OpsCmd::Notices { .. }
            | OpsCmd::Firms { .. }
            | OpsCmd::PrivateTerms { .. }
            | OpsCmd::Postgres(_)
            | OpsCmd::Sas(_)),
        ) => match action {
            OpsCmd::Postgres(command) => postgres_mirror::run(command),
            OpsCmd::Firms { action } => match action {
                FirmsAction::Doctor => firms_doctor::run(),
            },
            OpsCmd::PrivateTerms { host } => {
                runtime().block_on(ops_private_terms::run(host.host.as_deref()))
            }
            OpsCmd::Sas(SasCmd::Program) => match runtime().block_on(sas::program()) {
                Ok(code) => code,
                Err(error) => {
                    eprintln!("navigator: SAS: {error:#}");
                    ExitCode::FAILURE
                }
            },
            OpsCmd::Notices { out, check } => notices::run(&out, check),
            OpsCmd::CutRelease {
                repo,
                manifest_path,
                no_fetch,
                no_commit,
                dry_run,
            } => cut_release::run(
                chrono::Utc::now(),
                &repo,
                !no_fetch,
                &manifest_path,
                no_commit,
                dry_run,
            ),
            OpsCmd::PublicSnapshot {
                repo,
                git_ref,
                out,
                author_name,
                author_email,
                date,
                sign,
            } => public_snapshot::run(&public_snapshot::Request {
                repo: &repo,
                git_ref: &git_ref,
                out: &out,
                identity: public_snapshot::Identity {
                    name: author_name,
                    email: author_email,
                    date,
                },
                sign,
            }),
            OpsCmd::ReleaseDefaultTag { repo, no_fetch } => {
                release_default_tag::run(chrono::Utc::now(), &repo, !no_fetch)
            }
            OpsCmd::Release(ReleaseCmd::Version {
                tag,
                manifest_path,
                no_commit,
            }) => release_version::run(&manifest_path, &tag, no_commit),
            OpsCmd::Release(ReleaseCmd::Check {
                manifest_path,
                repo,
                no_fetch,
                github_output,
            }) => release_check::run(&manifest_path, &repo, !no_fetch, github_output),
            OpsCmd::Release(ReleaseCmd::Pins { root }) => release_pins::run(&root),
            OpsCmd::Lsp { action } => match action {
                LspAction::Publish { dir, bucket } => lsp_publish::run_publish(&dir, bucket),
            },
            OpsCmd::Assets { action } => match action {
                AssetsAction::Build { src, out, only } => assets::run_build(&src, &out, &only),
                AssetsAction::Upload { dir, bucket } => assets::run_upload(&dir, bucket),
                AssetsAction::Pull { out, bucket } => assets::run_pull(&out, bucket),
                AssetsAction::Orphans {
                    content,
                    bucket,
                    slack,
                } => assets::run_orphans(&content, bucket, slack),
                AssetsAction::Verify { content, base_url } => {
                    assets::run_verify(&content, base_url)
                }
                AssetsAction::FetchReferenced {
                    content,
                    out,
                    base_url,
                } => assets::run_fetch_referenced(&content, &out, base_url),
                AssetsAction::StubReferenced { content, out } => {
                    assets::run_stub_referenced(&content, &out)
                }
                AssetsAction::Fonts { action } => match action {
                    FontAction::Upload {
                        dir,
                        bucket,
                        family,
                    } => assets::run_upload_fonts(&dir, bucket, family.resolve()),
                    FontAction::UploadDesktop { dir, bucket } => {
                        assets::run_upload_desktop_fonts(&dir, bucket)
                    }
                },
            },
            _ => unreachable!("guarded by the outer pattern"),
        },
        Command::Ops(OpsCmd::Rebrand(action)) => {
            eprintln!("navigator: `ops rebrand` is deprecated");
            devx_result(devx::dispatch(Command::Ops(OpsCmd::Rebrand(action))))
        }
        // Local reversible loops and prod/cloud operations route through the
        // same handler that owns the cluster and operator behavior.
        c @ (Command::Dev(_) | Command::Ops(_) | Command::Quickstart) => {
            devx_result(devx::dispatch(c))
        }
    }
}

/// Map an orchestration command's `anyhow::Result<()>` onto a process
/// `ExitCode`. The former `devx` binary printed the error chain and exited
/// non-zero; keep that behavior now that it runs under `navigator`.
fn devx_result(result: anyhow::Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("Error: {err:?}");
            ExitCode::FAILURE
        }
    }
}

/// The person store the CLI reads and writes. `persons` moved to
/// `SurrealDB` with ENG-19, so every command that resolves a person opens
/// this handle. The endpoint comes from
/// `NAVIGATOR_SURREAL_*`, the same coordinates `web` uses.
async fn open_surreal() -> Result<store::surreal::SurrealDb, ExitCode> {
    match store::surreal::connect_from_env().await {
        Ok(db) => Ok(db),
        Err(e) => {
            eprintln!("navigator: surreal: {e}");
            Err(ExitCode::from(2))
        }
    }
}

/// Resolve the `person` argument — a Person UUID or an email (primary or
/// alias) — to the Person's id.
async fn resolve_person_id(
    db: &store::surreal::SurrealDb,
    spec: &str,
) -> Result<uuid::Uuid, String> {
    if let Ok(id) = spec.parse::<uuid::Uuid>() {
        return Ok(id);
    }
    match store::persons::find_by_any_email(db, spec).await {
        Ok(Some(person)) => Ok(person.id),
        Ok(None) => Err(format!("no Person answers to the email {spec}")),
        Err(e) => Err(format!("looking up {spec}: {e}")),
    }
}

/// Run `navigator-deploy person` — direct store access for one Person's phones.
async fn run_person(action: PersonCmd) -> ExitCode {
    let PersonCmd::Phone { action } = action;
    let db = match open_surreal().await {
        Ok(db) => db,
        Err(exit) => return exit,
    };
    let result = match action {
        PersonPhoneCmd::List { person } => match resolve_person_id(&db, &person).await {
            Ok(person_id) => match store::person_phones::list_phones(&db, person_id).await {
                Ok(phones) => {
                    if phones.is_empty() {
                        println!("no phone numbers on record");
                    }
                    for phone in phones {
                        let primary = if phone.is_primary { " (primary)" } else { "" };
                        println!("{} [{}]{primary}", phone.e164, phone.label.as_str());
                    }
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            },
            Err(message) => Err(message),
        },
        PersonPhoneCmd::Set {
            person,
            number,
            label,
            primary,
        } => {
            // An unlabeled number is `other`; a wrong label word is a
            // usage error, refused before the store is touched.
            let parsed_label = match label
                .as_deref()
                .map(store::person_phones::PhoneLabel::parse)
            {
                Some(Some(parsed)) => parsed,
                Some(None) => {
                    eprintln!(
                        "navigator: label must be mobile, work, home, fax, or other; got {}",
                        label.as_deref().unwrap_or("")
                    );
                    return ExitCode::from(2);
                }
                None => store::person_phones::PhoneLabel::Other,
            };
            let person_id = match resolve_person_id(&db, &person).await {
                Ok(id) => id,
                Err(message) => {
                    eprintln!("navigator: {message}");
                    return ExitCode::from(2);
                }
            };
            // `set_phone` normalizes internally, before any write, so a
            // typo is refused rather than half-written.
            match store::person_phones::set_phone(&db, person_id, &number, parsed_label, primary)
                .await
            {
                Ok(phone) => {
                    println!(
                        "{} [{}]{} saved",
                        phone.e164,
                        phone.label.as_str(),
                        if phone.is_primary { " (primary)" } else { "" }
                    );
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            }
        }
        PersonPhoneCmd::Remove { person, number } => {
            let person_id = match resolve_person_id(&db, &person).await {
                Ok(id) => id,
                Err(message) => {
                    eprintln!("navigator: {message}");
                    return ExitCode::from(2);
                }
            };
            match store::person_phones::remove_phone(&db, person_id, &number).await {
                // The removal echoes the store's canonical E.164 form,
                // not the raw argument, so no un-normalized input is
                // written back to the terminal.
                Ok(Some(removed_e164)) => {
                    println!("{removed_e164} removed");
                    Ok(())
                }
                Ok(None) => {
                    println!("no matching phone to remove");
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            }
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("navigator: {message}");
            ExitCode::FAILURE
        }
    }
}

async fn run_notation(action: NotationAction) -> ExitCode {
    match action {
        NotationAction::List {
            host,
            project,
            json,
        } => remote::notation_list(host.host.as_deref(), &project, json).await,
        NotationAction::Create {
            template_code,
            host,
            client_email: _,
            project: _,
            answers_from,
        } => {
            if answers_from.is_some() && !template_code.starts_with("onboarding") {
                eprintln!(
                    "navigator-deploy site notation create: --answers-from prefills an onboarding; offboarding and other templates take no answers file"
                );
                return ExitCode::FAILURE;
            }
            match template_code.as_str() {
                "onboarding" | "onboarding__letter" => {
                    projects::lifecycle::onboard(
                        host.host.as_deref(),
                        Path::new("."),
                        answers_from.as_deref(),
                        None,
                    )
                    .await
                }
                "offboarding" | "offboarding__letter" => {
                    projects::lifecycle::offboard(host.host.as_deref(), Path::new("."), None).await
                }
                _ => {
                    eprintln!(
                        "navigator-deploy site notation create: use `navigator project onboard` or `navigator project offboard`"
                    );
                    ExitCode::FAILURE
                }
            }
        }
        NotationAction::Status {
            notation_id,
            host,
            json,
        } => remote::notation_status(host.host.as_deref(), notation_id, json).await,
        NotationAction::Answers {
            notation_id,
            host,
            json,
            save_answers,
        } => {
            remote::notation_answers(
                host.host.as_deref(),
                notation_id,
                json,
                save_answers.as_deref(),
            )
            .await
        }
        NotationAction::Approve { notation_id, host } => {
            remote::notation_approve(host.host.as_deref(), notation_id).await
        }
        NotationAction::RequestChanges {
            notation_id,
            host,
            questions,
            note,
        } => {
            remote::notation_request_changes(
                host.host.as_deref(),
                notation_id,
                &questions,
                note.as_deref(),
            )
            .await
        }
        NotationAction::Update {
            notation_id,
            host,
            answers,
        } => remote::notation_update(host.host.as_deref(), notation_id, &answers).await,
        NotationAction::Document {
            notation_id,
            out,
            host,
        } => remote::notation_document(host.host.as_deref(), notation_id, &out).await,
    }
}

#[allow(clippy::too_many_lines)] // one flat dispatch match; splitting it hurts readability
async fn run_document(action: DocumentAction) -> ExitCode {
    match action {
        DocumentAction::Upload {
            host,
            project,
            file,
            dir,
            kind,
            visibility,
            description,
            content_type,
            slug,
            folder,
            quality,
            execution,
        } => {
            if let Some(dir) = dir {
                return remote::document_upload_directory(
                    host.host.as_deref(),
                    &project,
                    &dir,
                    &kind,
                    Some(&visibility),
                    description.as_deref(),
                    content_type.as_deref(),
                    quality.as_deref(),
                    &execution.record(),
                    folder,
                )
                .await;
            }
            let Some(file) = file else {
                return ExitCode::from(2);
            };
            remote::document_upload(
                host.host.as_deref(),
                &project,
                &file,
                &kind,
                Some(&visibility),
                description.as_deref(),
                content_type.as_deref(),
                slug.as_deref(),
                quality.as_deref(),
                &execution.record(),
                folder,
            )
            .await
        }
        DocumentAction::Transcribe { pointer } => remote::document_transcribe(&pointer).await,
        DocumentAction::Slug {
            host,
            project,
            asset_id,
            slug,
            kind,
            dry_run,
        } => {
            remote::document_slug(
                host.host.as_deref(),
                &project,
                asset_id,
                &slug,
                kind.as_deref(),
                dry_run,
            )
            .await
        }
        DocumentAction::Kind {
            host,
            project,
            asset_id,
            kind,
            dry_run,
        } => remote::document_kind(host.host.as_deref(), &project, asset_id, &kind, dry_run).await,
        DocumentAction::Reclassify {
            host,
            project,
            asset_id,
            kind,
            dry_run,
        } => {
            remote::document_reclassify(host.host.as_deref(), &project, asset_id, &kind, dry_run)
                .await
        }
        DocumentAction::Visibility {
            host,
            project,
            asset_id,
            visibility,
        } => {
            remote::document_visibility(host.host.as_deref(), &project, asset_id, &visibility).await
        }
        DocumentAction::Reslug {
            host,
            project,
            asset_id,
            slug,
            dir,
            dry_run,
        } => {
            remote::document_reslug(
                host.host.as_deref(),
                &project,
                asset_id,
                &slug,
                &dir,
                dry_run,
            )
            .await
        }
        DocumentAction::Retire {
            host,
            project,
            asset_id,
            dry_run,
            yes_retire,
        } => {
            remote::document_retire(
                host.host.as_deref(),
                &project,
                asset_id,
                dry_run,
                yes_retire,
            )
            .await
        }
        DocumentAction::Restore {
            host,
            project,
            asset_id,
        } => remote::document_restore(host.host.as_deref(), &project, asset_id).await,
        DocumentAction::Hold {
            host,
            project,
            asset_id,
            release,
        } => remote::document_hold(host.host.as_deref(), &project, asset_id, !release).await,
        DocumentAction::Repair {
            host,
            project,
            asset_id,
            dry_run,
        } => remote::document_repair(host.host.as_deref(), &project, asset_id, dry_run).await,
        DocumentAction::Log { pointer } => document_read::log(&pointer).await,
        DocumentAction::Get {
            pointer,
            version,
            out,
        } => document_read::get(&pointer, version, &out).await,
        DocumentAction::Diff { pointer, a, b } => document_read::diff(&pointer, a, b).await,
    }
}

/// Render one notation template to PDF or editable Word (`pdf`/`word`,
/// `output_extension` `"pdf"`/`"docx"` respectively). Validates the file
/// against the notation rule set, resolves the render frame (`output:`
/// frontmatter → the `kind:`-derived default → plain), fills any
/// `{{code}}` placeholders from `answers`, and writes the compiled document
/// to `out`, which must carry the matching extension.
/// The render profile a template selects by declaring no `output:` and no
/// `kind:` with a frame of its own. Never a declarable `output:` value —
/// omitting the key is how a template selects it.
const PLAIN_PROFILE: &str = "plain";

/// The one profile `pdf::OutputFormat::parse` cannot construct from its
/// name, because its calibration comes from the template's
/// `jurisdiction:`. `run_render` resolves it itself.
const PLEADING_PROFILE: &str = "pleading";

fn rendered_answers(
    contents: &str,
    answers: &[(String, String)],
) -> std::collections::BTreeMap<String, String> {
    let choices = rules::frontmatter::extract(contents)
        .and_then(|fm| workflows::choices_from_yaml(fm).ok())
        .unwrap_or_default();
    answers
        .iter()
        .map(|(code, value)| {
            let display =
                workflows::choice_label(&choices, code, value).unwrap_or_else(|| value.clone());
            // A multiline answer is Markdown structure (a table, a list), not
            // one inline fact. Wrapping it in `**…**` would keep the pipes
            // and breaks from parsing as blocks.
            let rendered = if display.contains('\n') {
                display
            } else {
                pdf::markdown::bold_answer(&display)
            };
            (code.clone(), rendered)
        })
        .collect()
}

/// Run the validation gate for a render: print every violation and return
/// the exit code to stop with, or `None` when rendering may proceed.
fn render_validation_gate(
    file: &std::path::Path,
    contents: &str,
    preview: bool,
) -> Option<ExitCode> {
    // Gate on validation: render only when there are no blocking
    // (Error-severity) violations. Use the same DB-free classified rule
    // set as `validate`. Yellow advisories (e.g. N112, "step allowed but
    // not built yet") are printed but must not block rendering, mirroring
    // `validate` / `site seed`.
    let source = rules::SourceFile {
        path: file.to_path_buf(),
        contents: contents.to_string(),
    };
    let violations: Vec<rules::Violation> =
        rules::navigator_classified_rules_with_codes(&source, &[])
            .iter()
            .flat_map(|r| r.lint(&source))
            .collect();
    // A preview exists to show a draft before it is final, so the one rule
    // that refuses a draft for being a draft, N131 (drafting markers), is
    // reported but does not block it; the markers render as highlighted
    // flags instead. A final render keeps the refusal.
    let blocking = |v: &&rules::Violation| !(preview && v.code == "N131");
    let blocking_violations: Vec<rules::Violation> =
        violations.iter().filter(blocking).cloned().collect();
    let (error_count, _) = severity_counts(&blocking_violations);
    if !violations.is_empty() {
        for v in &violations {
            print_violation(&v.path.display().to_string(), v.line, v.code, &v.message);
        }
    }
    if error_count > 0 {
        eprintln!("navigator: {error_count} validation error(s); not rendering");
        return Some(ExitCode::from(1));
    }
    None
}

fn resolve_render_format(
    field: impl Fn(&str) -> Option<String>,
) -> Result<(String, pdf::OutputFormat), ExitCode> {
    let profile = field("output")
        .or_else(|| {
            field("kind")
                .and_then(|k| rules::Kind::parse(&k))
                .map(|k| k.default_output().to_string())
        })
        .unwrap_or_else(|| PLAIN_PROFILE.to_string());
    // `pleading` is the one profile a bare name cannot construct: court
    // geometry is calibrated by the template's `jurisdiction:`, a second
    // field `OutputFormat::parse` never sees, so it returns `None` for the
    // name by design. Feeding it through that parser and taking
    // `unwrap_or_default()` put a validation-passing motion on the plain
    // frame — no numbered rail, wrong margins, wrong typeface — and said
    // nothing. Court paper rendered to the wrong geometry is a filing a
    // clerk can reject, so resolve the calibration here and refuse when
    // the table has not been extended to that jurisdiction:
    // `variant_for_jurisdiction` returning `None` means a template that
    // cannot render as a pleading yet, never a reason to guess.
    let format = if profile == PLEADING_PROFILE {
        let jurisdiction = field("jurisdiction").unwrap_or_default();
        if let Some(variant) = pdf::pleading::variant_for_jurisdiction(&jurisdiction) {
            pdf::OutputFormat::Pleading(variant)
        } else {
            // Neither the template's `jurisdiction:` nor its path is echoed
            // back. Both reach this line from caller-supplied input — the
            // one from a parsed document, the other from the command line —
            // and `rust/cleartext-logging` flags a new log of either. There
            // is nothing to lose by leaving them out: `notation pdf`/`notation
            // word` take exactly one file, named on the command line a moment
            // earlier, and what the author cannot already see is which
            // calibrations exist.
            eprintln!(
                "navigator: this template declares `kind: pleading`, but its `jurisdiction:` \
                 has no court-paper calibration; the calibrated jurisdictions are {}. Add one \
                 to `pdf::pleading::variant_for_jurisdiction` before rendering it",
                pdf::pleading::CALIBRATED_JURISDICTIONS.join(", ")
            );
            return Err(ExitCode::from(2));
        }
    } else {
        pdf::OutputFormat::parse(&profile).unwrap_or_default()
    };
    Ok((profile, format))
}

fn run_render(
    file: &std::path::Path,
    out: &std::path::Path,
    answers: &[(String, String)],
    output_extension: &str,
    preview: bool,
    ci: bool,
) -> ExitCode {
    if !has_extension(out, output_extension) {
        eprintln!("navigator: output file extension does not match the render format");
        return ExitCode::from(2);
    }
    let contents = match std::fs::read_to_string(file) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("navigator: could not read the template file: {}", e.kind());
            return ExitCode::from(2);
        }
    };

    if let Some(code) = render_validation_gate(file, &contents, preview) {
        return code;
    }

    // Resolve the render frame from the document itself: the template's
    // own `output:` override first, else the default `Kind::default_output`
    // derives from its declared `kind:`, else plain.
    //
    // There is no third input. `--format` used to win over both, which
    // meant the frame was chosen twice and the outside choice silently
    // beat a correct header — `--format letter` on a will put the firm's
    // letterhead on an executed instrument with nothing to warn the
    // author (LAW-15). `output:` is now the only override, and it lives
    // in the document it frames.
    //
    // A template without a declared `kind:` or `output:` uses the plain frame.
    // N109 refuses an unknown output profile before rendering.
    let field = |key: &str| {
        rules::frontmatter::extract(&contents)
            .and_then(|fm| rules::frontmatter::field(fm, key))
            .filter(|value| !value.is_empty())
    };
    let (profile, format) = match resolve_render_format(field) {
        Ok(resolved) => resolved,
        Err(code) => return code,
    };

    // `form` is the AcroForm fill mode, not a Typst frame: it fills the
    // pinned blank from the vendored forms registry (or refuses loudly)
    // and never reaches the body renderer below. `Kind::Filing`'s
    // default output is this profile, so a `kind: filing` template that
    // declares no `output:` fills too — `kind:` is not a second silent
    // path into the old plain fallback.
    if profile == forms_render::FORM_PROFILE {
        return forms_render::run(file, &contents, out, answers, output_extension, ci);
    }

    // Body is everything after the frontmatter block; fill placeholders
    // through the same evaluator as preview and final document generation.
    // Bold each answer as it goes in, so a reader can find every fact
    // particular to their matter without reading the boilerplate. The other
    // half of the same idea lives in `pdf::markdown`: a placeholder that no
    // answer filled gets a yellow wash instead, so an unfinished document
    // is unmistakably unfinished.
    // A choice answer is *stored* as its declared key (`nevada`), but the
    // body interpolates it as prose ("the law of Nevada"). Resolve the key
    // back to its label through the template's own `choices:` /
    // `choices.<key>` frontmatter — the same map
    // `portal::retainer_walk::render_context_from_answers` resolves a
    // generated document against, so a preview renders the document the
    // matter will actually get rather than a second, differently-worded
    // one. A state with no declared options, or a value that is not one of
    // them, keeps its answer verbatim (`choice_label` returns `None`), so
    // free text is untouched.
    // An unanswered Signer Box prints as an empty rule for the signer's
    // hand, in the preview and the final render alike.
    let mut answer_context = rendered_answers(&contents, answers);
    fill_firm_legal_name(file, &mut answer_context);
    fill_firm_office(file, &contents, &mut answer_context);
    let Ok(body) = pleading_body(file, &contents, &profile, &answer_context, preview)
        .map_err(|message| eprintln!("navigator: {message}"))
    else {
        return ExitCode::from(2);
    };
    if !preview && !final_body_is_finished(&body) {
        return ExitCode::from(1);
    }

    let Ok(rendered) =
        project_letterhead(file, &format, ci).map_err(|message| eprintln!("navigator: {message}"))
    else {
        return ExitCode::from(2);
    };
    // Every letter render names the letterhead it wore: which source
    // framed it, which revision of that source (LAW-266).
    eprintln!("{}", rendered.provenance);
    for warning in &rendered.warnings {
        eprintln!("navigator: {warning}");
    }
    let letterhead = rendered.letterhead;
    // A memo is the Firm's analysis for its client, so every page of it
    // says it is privileged, whatever its author typed (LAW-224).
    let privileged = field("kind").as_deref() == Some("memo");
    let body = flag_open_markers_in_preview(body, preview);
    let format_debug = format!("{format:?}");
    let bytes = match render_notation_artifact(
        file,
        &body,
        contents.as_bytes(),
        output_extension,
        format,
        &letterhead,
        preview,
        privileged,
    ) {
        Ok(bytes) => bytes,
        Err(_error) => {
            eprintln!("navigator: render failed");
            return ExitCode::from(2);
        }
    };
    if let Err(e) = std::fs::write(out, &bytes) {
        eprintln!("navigator: write {}: {e}", out.display());
        return ExitCode::from(2);
    }
    println!(
        "{}",
        palette::dim(format!(
            "Rendered {} ({format_debug}, {} bytes) → {}",
            file.display(),
            bytes.len(),
            out.display()
        ))
    );
    ExitCode::SUCCESS
}

/// `notation word --redline-against`: render exactly as `notation word`
/// does, then turn the difference from the previous version into real Word
/// tracked changes ([`word::redline::against`]). The render stages in a
/// temporary file first, so a refusal never leaves an un-redlined document
/// sitting at `--out` looking final.
fn run_word_redline(
    file: &std::path::Path,
    out: &std::path::Path,
    answers: &[(String, String)],
    preview: bool,
    previous: &std::path::Path,
    author: Option<&str>,
) -> ExitCode {
    let staging = match tempfile::Builder::new()
        .prefix("navigator-redline-")
        .suffix(".docx")
        .tempfile_in(out.parent().unwrap_or(std::path::Path::new(".")))
    {
        Ok(staging) => staging,
        Err(error) => {
            eprintln!("navigator: could not stage the render: {error}");
            return ExitCode::from(2);
        }
    };
    let code = run_render(file, staging.path(), answers, "docx", preview, false);
    if code != ExitCode::SUCCESS {
        return code;
    }
    let rendered = match std::fs::read(staging.path()) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("navigator: read the staged render: {error}");
            return ExitCode::from(2);
        }
    };
    let previous_bytes = match std::fs::read(previous) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("navigator: read {}: {error}", previous.display());
            return ExitCode::from(2);
        }
    };
    let author = match author {
        Some(name) => name.to_string(),
        None => match resolve_redline_author(file) {
            Ok(name) => name,
            Err(message) => {
                eprintln!("navigator: {message}");
                return ExitCode::from(2);
            }
        },
    };
    let redlined = match word::redline::against(&rendered, &previous_bytes, &author) {
        Ok(redline) => redline,
        Err(error) => {
            eprintln!("navigator: {error}");
            return ExitCode::from(2);
        }
    };
    if let Err(error) = std::fs::write(out, &redlined.bytes) {
        eprintln!("navigator: write {}: {error}", out.display());
        return ExitCode::from(2);
    }
    let counts = redlined.counts;
    if counts.total() == 0 {
        println!(
            "{}",
            palette::dim("No differences from the previous version; the render is unchanged.")
        );
    } else {
        println!(
            "{}",
            palette::dim(format!(
                "Redlined {} insertion(s), {} deletion(s), {} rewrite(s), authored by \
                 {author} → {}",
                counts.insertions,
                counts.deletions,
                counts.replacements,
                out.display()
            ))
        );
    }
    ExitCode::SUCCESS
}

/// The signing lawyer's name, for the revision author of every tracked
/// change a redline writes. The same source the pleading `/s/` line signs
/// with: `matter.counsel.name` in the Project's manifest.
///
/// # Errors
///
/// When the manifest is missing or malformed, naming both outs — the flag
/// and the manifest key.
fn resolve_redline_author(file: &std::path::Path) -> Result<String, String> {
    projects::matter::counsel_name(file).map_err(|error| {
        format!(
            "a redline records who made each change: pass --redline-author, or add \
             `matter.counsel.name` to navigator.yaml ({error})"
        )
    })
}

/// Final documents only: the filled text carries nothing a person is still
/// meant to complete, and no draft counsel signature. Missing facts route to
/// the questionnaire state that answers them. Prints each refusal.
fn final_body_is_finished(body: &str) -> bool {
    if word::paper::has_draft_counsel_signature(body) {
        eprintln!(
            "navigator: a counsel signature is signed from the Project's `matter.counsel.name`, \
             which only a pleading reads; render this draft with `--preview`"
        );
        return false;
    }
    let unfinished = rules::final_document::scan(body);
    if unfinished.is_empty() {
        return true;
    }
    for finding in &unfinished {
        eprintln!("navigator: rendered {finding}");
    }
    eprintln!(
        "navigator: a final render carries no unfinished text; answer the questionnaire, \
         or pass `--preview` for a highlighted draft"
    );
    false
}

/// Binds `firm.legal_name` to the Project's Firm legal name unless the
/// answers already supply one.
fn fill_firm_legal_name(
    file: &Path,
    answer_context: &mut std::collections::BTreeMap<String, String>,
) {
    if let Some(name) = project_firm_legal_name(file) {
        answer_context
            .entry("firm.legal_name".to_string())
            .or_insert_with(|| pdf::markdown::bold_answer(&name));
    }
}

/// Binds `{{firm.office.address}}`, `{{firm.office.phone}}`, and
/// `{{firm.office.email}}` to the Firm's office in the template's
/// `jurisdiction:` (LAW-329), unless the answers already supply them, and
/// says on stderr which office it used — a fallback to the default office
/// is never silent. A template that names none of them reads nothing.
fn fill_firm_office(
    file: &Path,
    contents: &str,
    answer_context: &mut std::collections::BTreeMap<String, String>,
) {
    if !contents.contains("firm.office.") {
        return;
    }
    let jurisdiction = projects::matter::jurisdiction_of(contents);
    let office = match projects::matter::find_manifest(file) {
        Some(path) => std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| projects::manifest::parse(&text).ok())
            .and_then(|manifest| {
                cli::letterhead::office_for_project(
                    manifest.brand.as_deref(),
                    manifest.host.as_deref(),
                    &jurisdiction,
                )
                .ok()
                .flatten()
            }),
        None => cli::letterhead::office_for_brand(views::brand::BrandKey::Neon, &jurisdiction),
    };
    let Some(office) = office else {
        eprintln!(
            "navigator: the Project's Brand publishes no office; {{{{firm.office.*}}}} stays open"
        );
        return;
    };
    if office.fallback {
        eprintln!(
            "navigator: the Firm has no office in this template's jurisdiction; \
             {{{{firm.office.*}}}} is filled from its default office ({})",
            office.state
        );
    } else {
        eprintln!(
            "navigator: {{{{firm.office.*}}}} is filled from the Firm's {} office",
            office.state
        );
    }
    for (key, value) in [
        ("firm.office.address", office.address.join(", ")),
        ("firm.office.phone", office.phone),
        ("firm.office.email", office.email),
    ] {
        answer_context
            .entry(key.to_string())
            .or_insert_with(|| pdf::markdown::bold_answer(&value));
    }
}

/// The Firm legal name behind the Project's declared Brand, when `file` sits
/// in a Project whose `navigator.yaml` resolves to one. A template outside
/// any Project names the default Brand's Firm, the same Brand whose
/// letterhead frames it; a Project that resolves to no Brand names none.
fn project_firm_legal_name(file: &Path) -> Option<String> {
    let letterhead = match projects::matter::find_manifest(file) {
        Some(path) => {
            let contents = std::fs::read_to_string(&path).ok()?;
            let manifest = projects::manifest::parse(&contents).ok()?;
            letterhead::for_project(manifest.brand.as_deref(), manifest.host.as_deref()).ok()?
        }
        None => letterhead::for_brand(views::brand::BrandKey::Neon).ok()?,
    };
    let name = letterhead.entity.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// The letterhead for a render of `file` framed as `format`, with the
/// provenance the render reports on stderr.
///
/// The Project's Brand frames the letter (LAW-265, ENG-1148):
/// `letterhead` resolves `project.brand`, else `project.host`, from
/// the nearest `navigator.yaml`. Which letterhead that Brand wears comes
/// from one of two sources. When the deployment's letterhead door is
/// reachable, the Brand's runtime letterhead row — the same rows
/// `/app/admin/brands` edits, served over HTTPS with the `site login`
/// bearer — frames the letter; when it is not, the compiled registry
/// does, and the fallback is announced, never silent: a template
/// re-render reflects the letterhead its Brand currently publishes, while
/// executed and filed artifacts keep their stored bytes (ENG-1148), so a
/// re-render may differ from what was sent — on purpose. A Brand the
/// registry does not know is a refusal, never the default letterhead.
/// Only the letter frame carries one, so no other frame reads the
/// manifest; a template outside any Project renders on the default
/// Brand's letterhead.
struct ProjectLetterhead {
    letterhead: pdf::Letterhead,
    provenance: String,
    warnings: Vec<String>,
}

fn project_letterhead(
    file: &Path,
    format: &pdf::OutputFormat,
    ci: bool,
) -> Result<ProjectLetterhead, String> {
    if !matches!(format, pdf::OutputFormat::Letter(_)) {
        return Ok(ProjectLetterhead {
            letterhead: pdf::Letterhead::default(),
            provenance: String::new(),
            warnings: Vec::new(),
        });
    }
    let Some(path) = projects::matter::find_manifest(file) else {
        return Ok(ProjectLetterhead {
            letterhead: pdf::Letterhead::default(),
            provenance: String::new(),
            warnings: Vec::new(),
        });
    };
    let contents = std::fs::read_to_string(&path)
        .map_err(|error| format!("read {}: {error}", projects::manifest::FILE))?;
    let manifest = projects::manifest::parse(&contents)?;
    let key = letterhead::resolve_brand(manifest.brand.as_deref(), manifest.host.as_deref())
        .map_err(|error| error.to_string())?;
    attempt_runtime_letterhead(key, ci)
}

/// The runtime source, tried within a bounded budget so a hung deployment
/// cannot hang the render. Any failure — no session, connect, read, or
/// budget — falls back to the compiled registry, announced; under `--ci`
/// the same failure refuses, because a CI render's bytes must not depend
/// on which answer a live read would have given (LAW-19).
fn attempt_runtime_letterhead(
    key: views::brand::BrandKey,
    ci: bool,
) -> Result<ProjectLetterhead, String> {
    let budget = std::time::Duration::from_secs(3);
    // The timeout's timer is built under the runtime, not as a `block_on`
    // argument — `tokio::time::timeout` constructs its `Sleep` eagerly, and
    // eager construction needs the time driver it is about to run under.
    let attempted = tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(async move { tokio::time::timeout(budget, runtime_letterhead(key, ci)).await });
    match attempted {
        Ok(Ok(rendered)) => Ok(rendered),
        Ok(Err(reason)) => letterhead_failure(key, &reason, ci),
        Err(elapsed) => letterhead_failure(
            key,
            &format!(
                "the deployment's letterhead door did not answer within {budget:?}: {elapsed:?}"
            ),
            ci,
        ),
    }
}

/// A letterhead read that could not answer: under `--ci` it refuses — a CI
/// render must render pinned bytes or not at all — and anywhere else it
/// falls back to the compiled registry, announced on stderr.
fn letterhead_failure(
    key: views::brand::BrandKey,
    reason: &str,
    ci: bool,
) -> Result<ProjectLetterhead, String> {
    if ci {
        return Err(format!(
            "--ci refuses a letter whose letterhead row could not be read from the deployment \
             ({reason}); warm the letterhead cache with a non---ci render, or repair the \
             `site login` session, before rendering under --ci"
        ));
    }
    compiled_fallback(key, reason)
}

/// Test-harness override for the letterhead door's base URL: points the
/// row read at a double instead of the Brand's signed-in deployment. The
/// double serves the row itself, so no `site login` session is resolved.
const LETTERHEAD_API_BASE_ENV: &str = "NAVIGATOR_LETTERHEAD_API_BASE";

/// The deployment base and bearer the letterhead row is read with: the
/// Brand's own host, as `navigator site login` stored the lawyer's
/// session. The `NAVIGATOR_LETTERHEAD_API_BASE` override points the read
/// at a test double or harness endpoint instead, and skips the session —
/// such an endpoint serves the row itself.
fn letterhead_endpoint(key: views::brand::BrandKey) -> Result<(String, Option<String>), String> {
    if let Some(base) = std::env::var_os(LETTERHEAD_API_BASE_ENV).filter(|v| !v.is_empty()) {
        return Ok((base.to_string_lossy().into_owned(), None));
    }
    let compiled = letterhead::for_brand(key).map_err(|error| error.to_string())?;
    match remote::matching_stored_session(&compiled.web)
        .map_err(|error| format!("the `site login` session could not be read: {error}"))?
    {
        remote::MatchingSession::Valid(base, token) => Ok((base, Some(token))),
        remote::MatchingSession::Absent => Err(format!(
            "no `navigator site login` session for {} — run `navigator site login --host {}`",
            compiled.web,
            key.apex().unwrap_or_else(|| key.as_str())
        )),
        remote::MatchingSession::Expired => Err(format!(
            "the `site login` session for {} has expired — run `navigator site login --host {}`",
            compiled.web,
            key.apex().unwrap_or_else(|| key.as_str())
        )),
    }
}

/// The Brand's letterhead from the signed-in deployment's letterhead door —
/// the same rows `/app/admin/brands` edits, served by
/// `GET /app/api/brands/{key}/letterhead` with the `site login` bearer
/// (LAW-341). `Err` names why the door could not answer; the caller falls
/// back to the compiled registry and announces it.
async fn runtime_letterhead(
    key: views::brand::BrandKey,
    ci: bool,
) -> Result<ProjectLetterhead, String> {
    let (base, token) = letterhead_endpoint(key)?;
    let runtime = letterhead::for_brand_runtime(key, &base, token.as_deref(), ci)
        .await
        .map_err(|error| error.to_string())?;
    let Some(runtime) = runtime else {
        // A Brand with no runtime row wears its compiled frame — the
        // compiled mark included — but the render still names the source.
        return compiled_letterhead(key);
    };
    let mark = match runtime.letterhead.mark {
        pdf::LetterheadMark::Png(_) => "logo",
        pdf::LetterheadMark::Wordmark => "wordmark",
        pdf::LetterheadMark::Compiled => "compiled",
    };
    let revision = runtime
        .revision
        .clone()
        .unwrap_or_else(|| "n/a".to_string());
    let mut provenance = format!(
        "letterhead: source=runtime brand={} revision={revision} mark={mark}",
        key.as_str()
    );
    if let Some(font) = runtime.font_object_key.as_deref() {
        provenance.push_str(" font=");
        provenance.push_str(provenance_font(font));
    }
    Ok(ProjectLetterhead {
        provenance,
        letterhead: runtime.letterhead,
        warnings: runtime.warnings,
    })
}

/// The compiled frame for `key`, with its provenance line.
fn compiled_letterhead(key: views::brand::BrandKey) -> Result<ProjectLetterhead, String> {
    let compiled = letterhead::for_brand(key).map_err(|error| error.to_string())?;
    let mark = match compiled.mark {
        pdf::LetterheadMark::Compiled => "compiled",
        _ => "wordmark",
    };
    Ok(ProjectLetterhead {
        provenance: format!(
            "letterhead: source=compiled brand={} revision=n/a mark={mark}",
            key.as_str()
        ),
        letterhead: compiled,
        warnings: Vec::new(),
    })
}

/// The compiled fallback, announced: a plain connectivity miss renders on
/// the compiled registry rather than refusing, and says so.
fn compiled_fallback(
    key: views::brand::BrandKey,
    reason: &str,
) -> Result<ProjectLetterhead, String> {
    let mut compiled = compiled_letterhead(key)?;
    compiled.warnings.push(format!(
        "letterhead: source=compiled brand={} revision=n/a (deployment unreachable: {reason}; \
         using compiled registry — re-render may differ from admin-set letterhead)",
        key.as_str()
    ));
    Ok(compiled)
}

/// A font object's bare file name for the provenance line.
fn provenance_font(font: &str) -> &str {
    font.rsplit('/').next().unwrap_or(font)
}

/// A preview's drafting markers, rewritten as visible flags (LAW-183): each
/// bracketed marker such as `[CONFIRM: amount and purpose]` becomes
/// `«OPEN: amount and purpose»`, which the PDF render washes yellow.
fn flag_open_markers_in_preview(body: String, preview: bool) -> String {
    if preview {
        flag_open_markers(&body)
    } else {
        body
    }
}

/// [`flag_open_markers_in_preview`]'s rewrite, applied unconditionally.
fn flag_open_markers(body: &str) -> String {
    let mut flagged = body.to_string();
    for finding in rules::final_document::drafting_markers(body) {
        if let rules::final_document::Marker::Bracket(text) = finding.marker {
            let inner = text.trim_start_matches('[').trim_end_matches(']').trim();
            let inner = inner
                .strip_prefix("CONFIRM:")
                .map_or(inner, str::trim_start);
            flagged = flagged.replace(
                &text,
                &format!(
                    "{}OPEN: {inner}{}",
                    pdf::markdown::OPEN_FLAG_START,
                    pdf::markdown::OPEN_FLAG_END
                ),
            );
        }
    }
    flagged
}

/// The Signer Box states a template declares, or none: the validation gate
/// above already refused a frontmatter that does not parse.
fn declared_signer_boxes(contents: &str) -> Vec<String> {
    rules::frontmatter::extract(contents)
        .and_then(|frontmatter| rules::signer_boxes(frontmatter).ok())
        .unwrap_or_default()
}

/// The filled body: every answer substituted and each unanswered Signer Box
/// printed as the rule marker, in the preview and the final render alike. A
/// pleading's caption and footer title are composed from the Project's
/// `matter:` and the notation's frontmatter, never from its body, so both
/// renders print one caption that a manifest edit changes everywhere. Its
/// certificate of service is composed the same way, from the Service
/// records on the pointer its `served_document:` names: a preview omits the
/// certificate while none is recorded, and a final render is refused. Any
/// other profile's body passes through.
fn pleading_body(
    file: &Path,
    contents: &str,
    profile: &str,
    answers: &std::collections::BTreeMap<String, String>,
    preview: bool,
) -> Result<String, String> {
    let composed = rules::clauses::compile(contents)?;
    let contents = composed.as_str();
    let signer_boxes = declared_signer_boxes(contents);
    // Each `{{<signer>.signature|initials|date}}` becomes a signing line with
    // the provider's anchor hidden on it, in every frame (LAW-344).
    let body = word::paper::mark_signing_lines(&views::notation::fill_signer_boxes(
        strip_frontmatter(contents),
        answers,
        signer_boxes.iter().map(String::as_str),
    ));
    if profile != PLEADING_PROFILE {
        return Ok(body);
    }
    let caption = if preview {
        projects::matter::caption_for_preview(file, contents)?
    } else {
        projects::matter::caption_for_final(file, contents)?
    };
    let certificate = projects::service::certificate_for(file, contents, preview)?
        .map(|certificate| format!("\n\n{certificate}"))
        .unwrap_or_default();
    // A draft's counsel signature prints `/s/ DRAFT`; a final render signs
    // it `/s/ <counsel>` from `matter.counsel.name`.
    let body = pleading_signature_dates(&body, file, preview)?;
    let body = if preview {
        body
    } else {
        word::paper::sign_counsel_signatures(&body, &projects::matter::counsel_name(file)?)
    };
    Ok(format!("{caption}{body}{certificate}"))
}

/// The token a preview's date line carries while `matter.signed_on` is
/// unset: an unfilled placeholder, which both renders wash yellow. It is
/// inserted at fill time, so it never sits in the template for `F115` to
/// flag, and a final render — which never inserts it — never carries it.
const UNSIGNED_DATE_PLACEHOLDER: &str = "{{matter.signed_on}}";

/// Insert the signature block's date line (LAW-242): `Dated this …`,
/// sourced only from `matter.signed_on`, above every counsel signature
/// block. A preview with no `signed_on:` prints the highlighted
/// [`UNSIGNED_DATE_PLACEHOLDER`]; a final render with none is refused — an
/// undated signature page is not filable.
///
/// # Errors
///
/// No manifest above the pleading, a `signed_on:` that is not a real
/// calendar date, or a final pleading whose signature block has no
/// `signed_on:` to date it.
fn pleading_signature_dates(body: &str, file: &Path, preview: bool) -> Result<String, String> {
    if !word::paper::has_counsel_signature(body) {
        return Ok(body.to_string());
    }
    let line = match projects::matter::signed_on(file)? {
        Some(date) => word::matter::date_line(&date)
            .ok_or_else(|| format!("`matter.signed_on` is not a calendar date: {date}"))?,
        None if preview => UNSIGNED_DATE_PLACEHOLDER.to_string(),
        // LAW-242: a final pleading never prints an undated signature page.
        None => {
            return Err(
                "this final pleading has a signature block, but its `matter:` block declares \
                 no `signed_on:` signing date; set it before final export"
                    .to_string(),
            )
        }
    };
    Ok(insert_before_counsel_signatures(body, &line))
}

/// `body` with `line` inserted as its own paragraph above each counsel
/// signature block. The marker is authored alone on its line, so the line
/// start — not the comment itself — is where the date goes.
fn insert_before_counsel_signatures(body: &str, line: &str) -> String {
    let mut out = String::with_capacity(body.len() + line.len() + 2);
    let mut rest = body;
    while let Some(start) = rest.find("<!--") {
        let Some(length) = rest[start..].find("-->").map(|end| end + "-->".len()) else {
            break;
        };
        if word::paper::counsel_signature(&rest[start..start + length]).is_some() {
            let line_start = rest[..start].rfind('\n').map_or(0, |at| at + 1);
            out.push_str(&rest[..line_start]);
            out.push_str(line);
            out.push_str("\n\n");
            out.push_str(&rest[line_start..start + length]);
        } else {
            out.push_str(&rest[..start + length]);
        }
        rest = &rest[start + length..];
    }
    out.push_str(rest);
    out
}

fn has_extension(out: &Path, expected: &str) -> bool {
    out.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
}

#[allow(clippy::too_many_arguments)]
fn render_notation_artifact(
    file: &Path,
    body: &str,
    template_bytes: &[u8],
    output_extension: &str,
    format: pdf::OutputFormat,
    letterhead: &pdf::Letterhead,
    preview: bool,
    privileged: bool,
) -> Result<Vec<u8>, String> {
    if output_extension == "docx" {
        let word_letterhead =
            matches!(&format, pdf::OutputFormat::Letter(_)).then(|| word::RenderLetterhead {
                name: &letterhead.name,
                entity: &letterhead.entity,
                phone: &letterhead.phone,
                email: &letterhead.email,
                web: &letterhead.web,
                postal_address: &letterhead.postal_address,
                font_family: &letterhead.font_family,
                logo_png: match &letterhead.mark {
                    pdf::LetterheadMark::Png(bytes) => bytes.as_slice(),
                    pdf::LetterheadMark::Compiled => pdf::firm_logo_png(),
                    pdf::LetterheadMark::Wordmark => &[],
                },
            });
        // A pleading's Word render takes the same calibrated court-paper
        // geometry as its PDF, so both carry the same frame and rail.
        let pleading = match &format {
            pdf::OutputFormat::Pleading(variant) => Some(variant.word_frame()),
            _ => None,
        };
        let source_revision = notation_render_revision(file);
        // Every Word render is a draft a lawyer may edit and import back,
        // so it embeds the export provenance the import verifies: the
        // template's file name and the hash of the template's bytes at
        // export time. The name — not the full path — keeps renders
        // byte-identical wherever the template lives; `notation import`
        // takes the template path explicitly and verifies it by hash.
        let template_name = file.file_name().map_or_else(
            || file.to_string_lossy().into_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        let template_hash = word::template_hash(template_bytes);
        let provenance = word::Provenance {
            template_path: template_name.as_str(),
            template_hash: template_hash.as_str(),
        };
        return word::render_notation(
            body,
            word_letterhead.as_ref(),
            &word::RenderOptions {
                source_revision: source_revision.as_deref(),
                pleading,
                provenance: Some(provenance),
                preview,
            },
        )
        .map_err(|error| format!("render Word document: {error}"));
    }

    pdf::render_document_with_options(
        body,
        format,
        letterhead,
        &pdf::RenderOptions {
            creation_timestamp: notation_render_timestamp(file),
            preview,
            privileged,
        },
    )
    .map_err(|error| format!("render {}: {error}", file.display()))
}

fn notation_render_revision(file: &Path) -> Option<String> {
    let parent = file.parent()?;
    let name = file.file_name()?;
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(parent)
        .args(["log", "-1", "--format=%H", "--"])
        .arg(name)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|revision| !revision.is_empty())
}

/// The fixed timestamp for notation files that have no Git history.
///
/// A source file outside Git still needs a stable artifact, so the renderer uses
/// the Unix epoch rather than the wall clock. Tracked files use their latest
/// commit timestamp, which makes the artifact follow the source's own history.
const NOTATION_RENDER_FALLBACK_EPOCH: i64 = 0;

fn notation_render_timestamp(file: &Path) -> chrono::DateTime<chrono::Utc> {
    let timestamp = file
        .parent()
        .and_then(|parent| {
            let name = file.file_name()?;
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(parent)
                .args(["log", "-1", "--format=%ct", "--"])
                .arg(name)
                .output()
                .ok()?;
            if !output.status.success() {
                return None;
            }
            String::from_utf8(output.stdout)
                .ok()?
                .trim()
                .parse::<i64>()
                .ok()
        })
        .and_then(|seconds| chrono::Utc.timestamp_opt(seconds, 0).single());
    timestamp.unwrap_or_else(|| {
        chrono::Utc
            .timestamp_opt(NOTATION_RENDER_FALLBACK_EPOCH, 0)
            .single()
            .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH)
    })
}

async fn run_catalog_seed(dir: &std::path::Path) -> ExitCode {
    let surreal = match open_surreal().await {
        Ok(s) => s,
        Err(code) => return code,
    };
    let storage = match cloud::from_env().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("navigator: storage: {e}");
            return ExitCode::from(2);
        }
    };
    let report = match import::import_directory(&surreal, &storage, dir).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("navigator: site seed: {e}");
            return ExitCode::from(2);
        }
    };
    for v in &report.violations {
        print_violation(&v.path.display().to_string(), v.line, v.code, &v.message);
    }
    println!(
        "{}",
        palette::dim(format!(
            "Seeded {} workspace-shared template(s), {} question catalog row(s); skipped {} file(s) with error-level rule violations.",
            report.templates_created,
            report.questions_created,
            report.files_skipped_due_to_violations,
        ))
    );
    if report.files_skipped_due_to_violations > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

async fn run_synthetic_portfolio(target: Option<String>, apply: bool) -> ExitCode {
    let environment = match store::DeploymentEnvironment::from_env() {
        Ok(environment) => environment,
        Err(e) => {
            eprintln!("navigator: {e}");
            return ExitCode::from(2);
        }
    };
    let surreal = match open_surreal().await {
        Ok(s) => s,
        Err(code) => return code,
    };
    let storage = match cloud::from_env().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("navigator: storage: {e}");
            return ExitCode::from(2);
        }
    };
    let result = if apply {
        store::synthetic_portfolio::apply(&surreal, &storage, environment, target.as_deref()).await
    } else {
        store::synthetic_portfolio::dry_run(&surreal, &storage, environment, target.as_deref())
            .await
    };
    let plan = match result {
        Ok(plan) => plan,
        Err(e) => {
            eprintln!("navigator: synthetic portfolio: {e}");
            return ExitCode::from(2);
        }
    };
    for item in &plan.items {
        println!(
            "{}",
            palette::dim(format!("{:?} {} {}", item.action, item.kind, item.key))
        );
    }
    println!(
        "{}",
        palette::dim(format!(
            "synthetic portfolio v{} against `{}`: {} to create, {} unchanged{}.",
            plan.version,
            plan.target,
            plan.created(),
            plan.unchanged(),
            if apply {
                ""
            } else {
                " (dry run; nothing written)"
            },
        ))
    );
    ExitCode::SUCCESS
}

#[cfg(test)]
mod batch_upload_args_tests {
    use super::*;
    #[test]
    fn upload_requires_exactly_one_source_and_slug_is_single_file_only() {
        let base = [
            "navigator",
            "site",
            "document",
            "upload",
            "--project",
            "acme",
            "--kind",
            "exhibit",
        ];
        let parse =
            |extra: &[&str]| Cli::try_parse_from(base.iter().copied().chain(extra.iter().copied()));
        assert!(parse(&["--dir", "exhibits"]).is_ok());
        assert!(parse(&["--file", "a.pdf"]).is_ok());
        assert!(parse(&[]).is_err());
        assert!(parse(&["--file", "a.pdf", "--dir", "exhibits"]).is_err());
        assert!(parse(&["--dir", "exhibits", "--slug", "a.pdf"]).is_err());
    }
}

#[cfg(test)]
mod deploy_helpers_tests {
    use super::*;

    #[test]
    fn parse_assignment_requires_a_key_and_a_value_side() {
        let (key, value) = parse_assignment("KEY=VALUE").expect("parses");
        assert_eq!((key.as_str(), value.as_str()), ("KEY", "VALUE"));
        // A value may itself contain `=`.
        assert_eq!(
            parse_assignment("a=b=c").unwrap(),
            ("a".into(), "b=c".into())
        );
        assert_eq!(
            parse_assignment("no_equals").unwrap_err(),
            "expected KEY=VALUE"
        );
        assert_eq!(
            parse_assignment("=VALUE").unwrap_err(),
            "expected KEY=VALUE with a non-empty KEY"
        );
    }

    #[test]
    fn has_extension_compares_case_insensitively() {
        assert!(has_extension(std::path::Path::new("out.pdf"), "pdf"));
        assert!(has_extension(std::path::Path::new("out.PDF"), "pdf"));
        assert!(!has_extension(std::path::Path::new("out.docx"), "pdf"));
        assert!(!has_extension(std::path::Path::new("out"), "pdf"));
    }
}
