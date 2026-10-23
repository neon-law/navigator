//! The litigation records (LAW-133, LAW-124, LAW-140, LAW-126), the estate and
//! cap-table records, the Matter Pages that render them, and the Notation
//! sections, as one test binary.
//!
//! Each module is its own suite with its own fixture. They share one binary
//! because the merge gate's instrumented test build is bound by runner disk,
//! and every portal integration binary links the whole server; a second
//! copy of that graph is the growth `ci.yml` warns about.

mod communications_api;
mod docket_api;
mod matter_page_authorities;
mod matter_page_closing;
mod matter_page_deadlines;
mod matter_page_discovery;
mod matter_page_docket;
mod matter_page_estate;
mod matter_page_notations;
mod matter_page_service;
mod matter_page_timeline;
mod matter_pages;
mod module_records_api;
mod sample_matter_pages;
mod service_api;
