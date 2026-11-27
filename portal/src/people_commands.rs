//! Web adapter for the People command boundary.
//!
//! The DTOs, validation, and the create/update/delete writes live in
//! `store::people_commands` so `web`, `cli`, and `mcp` share one command
//! boundary without duplicating persistence. This module re-exports that
//! surface for the JSON `/app/api/people*` routes and the browser lawyer forms.
//!
//! `send_welcome` — the one command that needs the mailer — lives in
//! `workflows::email::welcome` and is re-exported here. It moved down a
//! crate so the `send_welcome_email` MCP tool can reach the same
//! command: `portal` depends on `mcp`, so a command defined here was one
//! the agent door structurally could not call, and it grew its own path
//! instead (ENG-317).

pub use store::people_commands::{
    create_person, delete_person, parse_role, update_person, CreatePersonCommand,
    PeopleCommandError, UpdateContext, UpdatePersonCommand,
};
pub use workflows::email::welcome::send_welcome;

#[cfg(test)]
mod tests {}
