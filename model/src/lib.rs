//! The pure half of Navigator's domain model: types, constants, parsers, and
//! validators that open no database, storage, or network connection.
//!
//! `store` and `cloud` re-export each module's items under their original
//! paths, so `store::documents::Signing` and `model::documents::Signing` are
//! one type. The `navigator` CLI uses these models without linking database clients or
//! workflow runtimes; its local document renderer links Typst separately.

pub mod billing_report;
pub mod cases;
pub mod correspondence;
pub mod deadline_proposals;
pub mod deadline_schedule;
pub mod document_pointers;
pub mod documents;
pub mod efiling_transactions;
pub mod entity_commands;
pub mod estate_assets;
pub mod fiduciary_roles;
pub mod forge;
pub mod forgejo_governance;
pub mod glossary;
pub mod mail_attachments;
pub mod matter_record_links;
pub mod nef;
pub mod person_phones;
pub mod persons;
pub mod plan_benefit;
pub mod pleading;
pub mod project_deadlines;
pub mod project_references;
pub mod project_service;
pub mod project_surfaces;
pub mod projects;
pub mod question_registry;
pub mod seed;
pub mod workspace;

pub mod brand_letterheads;
pub mod fi_import;

pub mod disabled_billing;
pub mod stripe_keys;
