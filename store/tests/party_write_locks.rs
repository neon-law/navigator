//! Native financial party writes and relational Person-tag merge integrity.
#[path = "support/live_party_writes.rs"]
mod live_party_writes;
#[path = "support/party_write_snapshot.rs"]
mod merge_fixture;
#[path = "support/native_project.rs"]
mod native_project;

#[path = "support/financial_party_merge.rs"]
mod financial_party_merge;
