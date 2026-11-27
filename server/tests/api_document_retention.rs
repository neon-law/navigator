#![allow(clippy::doc_markdown)]
//! ENG-862: soft deletion, restoration, Legal Hold, and the retention purge,
//! through the REST doors a Project's `project sync` and an operator reach.
//!
//! A lawyer-tier delete stamps the whole slug chain and hides it from clients
//! and the default lists while a lawyer still reads it; restore returns it with
//! its revision history; the purge expunges only chains past their window that
//! no Legal Hold or unarchived closed matter protects, and writes the same
//! audit record a governed expunge does.

#[path = "common/bearer.rs"]
mod bearer_common;
