#[path = "../support/native_database.rs"]
mod native_database;
pub use native_database::{database, database_with_activation, TestDatabase, APP_VERSION};

/// A hung durable test must fail rather than hold CI.
pub const DEADLINE: std::time::Duration = std::time::Duration::from_mins(1);

#[path = "../support/native_project.rs"]
pub mod native_project;

#[path = "../support/application_process.rs"]
pub mod application_process;
