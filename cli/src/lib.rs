//! The `navigator` library: Projects, their Jujutsu repositories, the offline
//! `validate` gate, and the site client those commands speak through.
//!
//! The `navigator` binary is a thin `main` over [`commands::main`], so a host
//! other than the terminal (the menu-bar app) links the same rules.
//! `navigator-deploy` and `scorpio` reuse the shared modules from here rather
//! than keep second copies.

pub mod answers_file;
pub mod atomic_directory;
pub mod authorities;
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
pub mod host;
pub mod legal_plans;
pub mod letterhead;
pub mod login;
pub mod palette;
pub mod pii_audit;
pub mod projects;
pub mod release;
pub mod remote;
pub mod scraper_config;
pub mod snapshot_audit;
pub mod upload_slug;

pub use commands::{
    append_skill_pin_findings, cli_version, diagnostic_line, fix_directory, gate_root,
    navigator_command, published_cli_version, run_gate, run_tracked_files_gate,
};
