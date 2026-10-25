//! `navigator project docket` — a Project's court dockets: the Docket
//! Entries recorded through the portal API ([`entries`]), and the CM/ECF
//! notices the court mails for each entry ([`ingest`]), which file the
//! documents and record an entry per notice through the same door.

#[allow(dead_code)] // the renderer has no caller until the docket verbs write the files
pub mod case_timeline;
pub mod entries;
pub mod ingest;
pub mod nef;
