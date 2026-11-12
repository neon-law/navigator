//! The pure half of Navigator's domain model: types, constants, parsers, and
//! validators that open no database, storage, or network connection.
//!
//! `store` and `cloud` re-export each module's items under their original
//! paths, so `store::documents::Signing` and `model::documents::Signing` are
//! one type. The lean `navigator` CLI depends on this crate alone, which keeps
//! `SurrealDB`, `SQLx`, Typst, and the cloud SDKs out of its build.

pub mod cases;
pub mod document_pointers;
pub mod documents;
pub mod efiling_transactions;
pub mod entity_commands;
pub mod estate_assets;
pub mod fiduciary_roles;
pub mod forge;
pub mod forgejo_governance;
pub mod matter_record_links;
pub mod nef;
pub mod person_phones;
pub mod persons;
pub mod project_deadlines;
pub mod project_references;
pub mod project_service;
pub mod project_surfaces;
pub mod projects;
pub mod question_registry;
pub mod seed;
pub mod workspace;
