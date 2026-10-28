//! `navigator project` — the Project workspace group.
//!
//! Project is the organizing noun of the whole product: a matter's Drive
//! ingest folder and its one source repository are both addressed by one
//! `projects.code`. The verbs that operate on that pair live here rather than
//! under `crate::remote`, grouped by the noun they act on rather than by
//! mechanism — some ([`doctor`], [`repository`]) resolve deployment-owned
//! coordinates from [`cloud::workspace`] and inspect the operator's own
//! machine, while others ([`drift`], [`surfaces`]) are pure clients of the
//! logged-in deployment's admin API, exactly like every command in
//! `crate::remote`. Neither `drift` nor `surfaces` opens a database
//! connection of its own, even against a local deployment: the site does
//! that work and the CLI only ever holds a bearer token.

pub mod cli_docs;
pub mod communication;
pub mod deadline;
pub mod docket;
pub mod doctor;
pub mod document_check;
pub mod drift;
pub mod efiling;
pub mod finance;
pub mod gate;
pub mod ics;
pub mod lifecycle;
pub mod local_bytes;
pub mod manifest;
pub mod matter;
pub mod pages_init;
pub mod participant;
pub(crate) mod pinned_gate;
pub mod preview;
pub mod reference;
pub mod repository;
pub mod scaffold;
pub mod search;
pub mod service;
pub mod set;
pub mod setup;
pub mod skill;
pub mod source_import;
pub mod status;
pub mod surfaces;
