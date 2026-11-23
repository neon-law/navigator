//! `navigator project` — the Project workspace group.
//!
//! Project is the organizing noun of the whole product: a matter's Drive
//! ingest folder and its one source repository are both addressed by one
//! `projects.code`. The verbs that operate on that pair live here rather than
//! under `crate::remote`, grouped by the noun they act on rather than by
//! mechanism — some ([`doctor`], [`repository`]) resolve deployment-owned
//! coordinates from [`model::workspace`] and inspect the operator's own
//! machine, while others ([`drift`], [`surfaces`]) are pure clients of the
//! logged-in deployment's admin API, exactly like every command in
//! `crate::remote`. Neither `drift` nor `surfaces` opens a database
//! connection of its own, even against a local deployment: the site does
//! that work and the CLI only ever holds a bearer token.

pub(crate) mod calendar;
pub mod cli_docs;
pub mod communication;
pub mod deadline;
pub mod deadline_brief;
pub mod deadline_proposal;
pub mod docket;
pub mod doctor;
pub mod document_check;
pub mod drift;
pub mod drive_ingest;
pub mod efiling;
pub mod feedback_check;
pub mod finance;
pub mod forge;
pub mod forge_forgejo;
pub mod forge_github;
pub mod gate;
pub mod ics;
pub mod invite;
pub mod jj;
pub mod jj_billing;
pub mod jj_commands;
pub mod jj_log;
pub mod lifecycle;
pub mod lifecycle_read;
pub mod local_bytes;
pub mod main_push;
pub mod manifest;
pub mod matter;
pub mod mirror_repair;
pub mod pages_init;
pub mod participant;
pub mod pinned_gate;
pub mod push;
pub mod reference;
pub mod repository;
pub mod scaffold;
pub mod search;
pub mod service;
pub mod set;
pub mod setup;
pub mod skill;
pub mod skill_feedback;
pub mod source_import;
pub mod status;
pub mod surfaces;
pub mod surfaces_reconcile;
pub mod watch;
mod workspace;
pub mod workspace_root;

/// Resolve the explicit matter scope or the enclosing Project checkout.
pub(crate) fn project_target(
    host: Option<&str>,
    project: Option<&str>,
) -> anyhow::Result<(Option<String>, String)> {
    if let (Some(host), Some(project)) = (host, project) {
        return Ok((Some(host.to_owned()), project.to_owned()));
    }
    let cwd = std::env::current_dir()?;
    for dir in cwd.ancestors() {
        if dir.join(manifest::FILE).is_file() {
            let (local_project, local_host) = crate::document_sync::read_manifest(dir)?;
            return Ok((
                host.map(str::to_owned).or(local_host),
                project.unwrap_or(&local_project).to_owned(),
            ));
        }
        if dir.join(".git").exists() || dir.join(".jj").exists() {
            break;
        }
    }
    Ok((
        host.map(str::to_owned),
        project
            .ok_or_else(|| anyhow::anyhow!("run in a Project checkout or provide --project"))?
            .to_owned(),
    ))
}
