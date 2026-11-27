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

pub mod access;
pub mod addresses;
pub mod answers;
pub mod assets;
pub mod assistant_turns;
pub mod attestations;
pub mod attorney_rates;
pub mod authorities;
pub mod billed_calls;
pub mod billing_batches;
pub mod billing_report;
pub mod brand_domains;
pub mod brand_letterheads;
pub mod brand_phone_numbers;
pub mod brands;
pub mod cap_table_holdings;
/// Firm-authored Case Stage vocabulary and audited per-matter transitions.
pub mod case_stages;
pub mod cases;
pub mod clauses;
pub mod cm_ecf_stamp;
pub mod communications;
pub mod config;
pub mod conflicts;
pub mod contact_sync;
pub mod contingency_terms;
pub mod contract_reviews;
pub mod court_seeds;
pub mod courts;
pub mod credentials;
/// The Deadline mirror onto a Project's shared Google Calendar.
pub mod deadline_calendar;
pub mod delegations;
pub mod deployment;
pub mod deployment_secret_changes;
pub mod disclosures;
pub mod document_comments;
pub mod docusign_completion;
pub mod evidence_shares;
pub mod project_envelopes;
pub mod project_labels;
pub use model::document_pointers;
pub use model::pleading;
pub mod attorney_calendars;
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
pub mod firm_assumptions;
pub mod firm_capability;
pub mod firm_digest;
pub mod firm_esign;
pub mod firm_journal;
pub mod firm_library;
pub mod firm_secrets;
pub mod firm_side;
pub mod firms;
pub mod git_access_tokens;
pub mod git_repositories;
pub mod glossary;
pub mod inbox;
pub mod inbox_alerts;
pub mod inbox_import;
pub mod inbox_triage;
pub mod iolta_accounts;
pub mod iolta_withdrawals;
pub mod jurisdictions;
pub mod leads;
pub mod legal_plans;
pub mod letters;
pub mod listing;
pub mod mail_signatures;
pub mod mailrooms;
pub mod matter_captions;
pub mod matter_financials;
pub mod matter_outcomes;
pub mod matter_pages;
pub mod matter_projections;
pub(crate) mod matter_record_links;
pub use model::correspondence;
pub use model::mail_attachments;
pub use model::nef;
pub mod court_date_forgejo;
pub mod deadline_proposals;
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
pub mod onboarding;
pub mod outcome_calibration;
pub mod outcome_estimates;
pub mod participation;
pub mod people_commands;
pub mod person_merge;
pub mod person_phones;
pub mod personal_projects;
pub mod persons;
pub mod plan_assignment;
pub mod plan_claims;
pub mod plan_intake;
pub mod plan_metrics;
pub mod plan_seeds;
pub mod playbooks;
pub mod postgres;
/// Resumable batches that copy rows the PostgreSQL mirror never saw.
/// The AGE relationship graph, projected from the mirrors and parity-checked
/// against the Surreal conflict traversal.
pub mod postgres_graph;
/// Read-only graph views over the AGE projection: the shapes behind the
/// graph REST endpoints.
pub mod postgres_graph_views;
/// Best-effort PostgreSQL mirror of Surreal writes, with parity and reconcile.
/// PostgreSQL schema history and deterministic parity artifacts.
pub mod postgres_schema;
pub mod posts;
/// The private-terms record checks the Pre-Filing PII Check refuses on, run
/// in report mode for the admin-tier reconciliation door.
pub mod private_terms;
pub mod project_archives;
pub mod project_calendar;
pub mod project_closeout;
pub mod project_costs;
pub mod project_deadlines;
pub mod project_folder_reconcile;
pub mod project_folders;
pub mod project_handoffs;
pub mod project_modules;
pub mod project_reconcile;
pub mod project_references;
pub mod project_section_counts;
pub mod project_service;
pub mod project_source_imports;
pub mod project_surfaces;
pub mod projected_cash;
pub mod projects;
pub mod purchases;
pub use model::question_registry;
pub mod questions;
pub mod reask;
pub mod relationship_logs;
pub mod relationships;
pub mod review_documents;
/// Publishing a built sample-project bundle into the applications bucket.
pub mod sample_project;
/// The metered-credit ledger behind the Same Day Legal sandbox.
pub mod sandbox_usage;
/// The schema, applied as one idempotent `DEFINE` file (#1093).
pub mod seed;
pub mod sent_emails;

/// The store connection, over the `NAVIGATOR_SURREAL_*` contract.
pub mod application_store;
pub mod billing_annotations;
pub mod signature_webhook_journal;
pub mod signatures;
pub mod social_account_selections;
pub mod social_accounts;
pub mod social_handles;
pub mod social_oauth_attempts;
pub mod social_publications;
pub mod source_pages;
pub mod ssh_public_keys;
pub mod statutory_deadlines;
pub mod summary_sender_approvals;
pub mod synthetic_portfolio;
pub mod task_handoffs;
pub mod tasks;
pub mod template_source;
pub mod templates;
pub mod testimonials;
pub mod time_entries;
pub mod timers;
#[path = "runtime/tls.rs"]
pub mod tls;
pub mod trust;
pub mod xero_invoices;

#[cfg(feature = "test-support")]
pub mod test_support;
pub mod verifications;

pub use config::{
    sample_matters, sample_matters_from, DeploymentEnvironment, DeploymentEnvironmentError,
    SampleMattersError, NAVIGATOR_ENVIRONMENT, NAVIGATOR_SIMULATED_MATTERS,
};

pub mod communication_dashboard;

pub mod fi_import;

pub use model::disabled_billing;
pub mod durable_object_handover;
pub mod project_slack_deliveries;
