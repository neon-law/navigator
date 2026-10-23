//! Litigation-record CLI verbs as one test binary.
//!
//! Each module is its own suite. They share one binary because the merge
//! gate's instrumented test build is bound by runner disk, and every CLI
//! integration binary links the navigator graph.

mod communication;
mod docket;
mod pleading;
mod service;
