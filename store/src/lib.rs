#![allow(clippy::doc_markdown)]
//! Neon Law Navigator CRM data layer.
//!
//! Owns the SurrealDB schema and the canonical seed. Every workspace
//! crate that touches the store — `web`, `cli`, `mcp` — depends on this
//! crate; nothing here depends on axum, reqwest, or any HTTP machinery.
//!
//! One table is one top-level module: `store::persons` owns `person`,
//! `store::projects` owns `project`, and so on. The schema itself is a
//! statement of the present rather than a history — one idempotent
//! `DEFINE` file at `store/src/schema/navigator.surql`, applied whole on
//! every boot, plus a `schema_version` record. You change it by editing
//! that file, not by appending a step.
//!
//! # Accounting
//!
//! [`firm_journal`] owns the Firm's authoritative, immutable journal.
//! [`xero_invoices`] and the trust imports preserve external evidence and links.
//! Navigator also stores rate and quoted usage provenance for Notations.
//! [`cap_table_holdings`] records a Project's current holdings; each cap-table
//! ledger remains outside Navigator.

#[cfg(test)]
pub(crate) mod test_tracing {
    use tracing::span::{Attributes, Id, Record};
    use tracing::subscriber::Interest;
    use tracing::{Event, Metadata, Subscriber};

    /// A globally-installed subscriber that claims interest in every callsite
    /// but records nothing itself.
    ///
    /// `tracing` computes and caches each callsite's interest from the
    /// *globally registered* dispatchers only — a thread-local `set_default`
    /// (how the capture tests install their subscriber) is invisible to that
    /// cache. With no global dispatcher, a callsite first seen — or rebuilt —
    /// while the global default is `NoSubscriber` caches as `Interest::never()`,
    /// and the event a capturing test is asserting on is dropped before its
    /// per-event `enabled` check ever runs against the thread-local subscriber.
    /// Returning `Interest::sometimes()` from a globally-registered dispatcher
    /// keeps every callsite deferring to the current dispatcher, so the
    /// thread-local capture is consulted per event; `enabled` returns `false`
    /// so this global itself records nothing on threads without a capturing
    /// default. Mirrors `portal::test_tracing`.
    struct AlwaysInterested;

    impl Subscriber for AlwaysInterested {
        fn register_callsite(&self, _: &'static Metadata<'static>) -> Interest {
            Interest::sometimes()
        }
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            false
        }
        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }
        fn record(&self, _: &Id, _: &Record<'_>) {}
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn event(&self, _: &Event<'_>) {}
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
    }

    static INSTALL: std::sync::Once = std::sync::Once::new();

    /// Installs [`AlwaysInterested`] as the process-global default the first
    /// time any capture test runs, so callsite interest can never cache as
    /// `never`. Idempotent, and a no-op if some other global default is
    /// already set.
    pub(crate) fn ensure_callsite_interest() {
        INSTALL.call_once(|| {
            let _ = tracing::subscriber::set_global_default(AlwaysInterested);
        });
    }
}

pub mod access;
pub mod addresses;
pub mod answers;
pub mod assets;
pub mod attestations;
pub mod authorities;
pub mod brand_domains;
pub mod brands;
pub mod cap_table_holdings;
pub mod cases;
pub mod clauses;
pub mod cm_ecf_stamp;
pub mod communications;
pub mod config;
pub mod conflicts;
pub mod contract_reviews;
pub mod courts;
pub mod credentials;
pub mod delegations;
pub mod deployment;
pub mod deployment_secret_changes;
pub mod disclosures;
pub mod document_comments;
pub mod document_pointers;
pub mod document_retention;
pub mod documents;
pub mod drafts;
pub mod drive_import;
pub mod efiling_transactions;
pub mod email_conversations;
pub mod email_deliveries;
pub mod email_receipts;
pub mod email_tokens;
pub mod entities;
pub mod entity_commands;
pub mod entity_roles;
pub mod entity_types;
pub mod estate_assets;
pub mod expunge_records;
pub mod expunge_requests;
pub mod external_identities;
pub mod fiduciary_roles;
pub mod filings;
pub mod finance_aggregate;
pub mod firm_capability;
pub mod firm_journal;
pub mod firm_library;
pub mod firm_secrets;
pub mod firms;
pub mod git_access_tokens;
pub mod git_repositories;
pub mod glossary;
pub mod inbox;
pub mod inbox_alerts;
pub mod iolta_accounts;
pub mod iolta_withdrawals;
pub mod journal_startup_migration;
pub mod jurisdictions;
pub mod leads;
pub mod letters;
pub mod mailrooms;
pub mod matter_captions;
pub mod matter_pages;
pub(crate) mod matter_record_links;
pub mod notarizations;
pub mod notation_clauses;
pub mod notation_documents;
pub mod notation_drafts;
pub mod notation_events;
pub mod notation_machine_state;
pub mod notation_packages;
pub mod notation_pricing;
pub mod notation_reset;
pub mod notation_sources;
pub mod notations;
pub mod participation;
pub mod people_commands;
pub mod person_merge;
pub mod persons;
pub mod playbooks;
pub mod postgres;
/// Resumable batches that copy rows the PostgreSQL mirror never saw.
pub mod postgres_backfill;
/// The AGE relationship graph, projected from the mirrors and parity-checked
/// against the Surreal conflict traversal.
pub mod postgres_graph;
/// Best-effort PostgreSQL mirror of Surreal writes, with parity and reconcile.
pub mod postgres_mirror;
/// PostgreSQL schema history and deterministic parity artifacts.
pub mod postgres_schema;
/// Sampled comparisons of SurrealDB reads with their PostgreSQL equivalents.
pub mod postgres_shadow;
pub mod posts;
pub mod project_archives;
pub mod project_calendar;
pub mod project_closeout;
pub mod project_costs;
pub mod project_deadlines;
pub mod project_handoffs;
pub mod project_modules;
pub mod project_reconcile;
pub mod project_references;
pub mod project_section_counts;
pub mod project_service;
pub mod project_source_imports;
pub mod project_surfaces;
pub mod projects;
pub mod purchases;
pub mod question_registry;
pub mod questions;
pub mod reask;
pub mod relationship_logs;
pub mod relationships;
pub mod review_documents;
/// Publishing a built sample-project bundle into the applications bucket.
pub mod sample_project;
/// The schema, applied as one idempotent `DEFINE` file (#1093).
pub mod schema;
pub mod seed;
pub mod sent_emails;
pub mod signatures;
pub mod social_account_selections;
pub mod social_accounts;
pub mod social_handles;
pub mod social_oauth_attempts;
pub mod social_publications;
pub mod source_pages;
pub mod statutory_deadlines;
pub mod summary_sender_approvals;
/// The store connection, over the `NAVIGATOR_SURREAL_*` contract.
pub mod surreal;
pub mod synthetic_portfolio;
pub mod template_source;
pub mod templates;
pub mod testimonials;
pub mod trust;
pub mod visitor_analytics;
pub mod xero_invoices;

#[cfg(feature = "test-support")]
pub mod test_support;
pub mod verifications;

pub use config::{
    sample_matters, sample_matters_from, DeploymentEnvironment, DeploymentEnvironmentError,
    SampleMattersError, NAVIGATOR_ENVIRONMENT, NAVIGATOR_SIMULATED_MATTERS,
};
