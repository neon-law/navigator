//! The `navigator` library: Projects, their Jujutsu repositories, the offline
//! `validate` gate, and the site client those commands speak through.
//!
//! The `navigator` binary is a thin `main` over [`commands::main`], so a host
//! other than the terminal (the menu-bar app) links the same rules.
//! `hudson` and `scorpio` reuse the shared modules from here rather
//! than keep second copies.

pub mod answers_file;
pub mod atomic_directory;
pub mod attorney_rates;
pub mod authorities;
pub mod billing;
pub mod closing_trailer;
pub mod commands;
pub mod credentials;
pub mod document_deletion;
pub mod document_layout;
pub mod document_ocr;
pub mod document_read;
pub mod document_reconcile;
pub mod document_sync;
pub mod feedback_scan;
pub mod forgejo_release;
pub mod github_setup;
pub mod glossary;
pub mod host;
pub mod legal_plans;
pub mod letterhead;
pub mod login;
pub mod matter_site;
pub mod palette;
pub mod pii_audit;
pub mod projects;
pub mod release;
pub mod release_check;
pub mod release_default_tag;
pub mod release_pins;
pub mod release_version;
pub mod remote;
pub mod scraper_config;
pub mod slugless_mail_repair;
pub mod snapshot_audit;
pub mod upload_slug;

pub use commands::{
    append_skill_pin_findings, cli_version, diagnostic_line, fix_directory, gate_root,
    navigator_command, published_cli_version, run_gate, run_tracked_files_gate,
};

pub mod assumptions;
pub mod downloads;
pub mod inbox;
pub mod inbox_import;
pub mod lsp_download;
pub mod mail;

pub use commands::{concise_help, parse_asset_kind, HostOpt};
pub mod firm_admin;
pub mod firm_forgejo;
pub mod forms_render;
pub mod notation_import;
pub mod notation_kinds;
pub mod notation_render;
pub mod person;
pub mod rendering_letterhead;
