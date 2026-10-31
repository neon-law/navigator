//! `navigator project docket` — a Project's court dockets: the Docket
//! Entries recorded through the portal API ([`entries`]), the CM/ECF
//! notices the court mails for each entry ([`ingest`]), which file the
//! documents and record an entry per notice through the same door, and the
//! claims agents' notices of the same case ([`claims_agent`]), whose links
//! fall back to the court's free look when the agent's site blocks a
//! scripted download.

pub mod case_timeline;
pub mod claims_agent;
pub mod entries;
pub mod ingest;
pub mod nef;
pub mod renumber;
pub mod timeline_write;
