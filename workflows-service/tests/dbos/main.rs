//! DBOS durable execution against a disposable PostgreSQL database.
//!
//! One binary for every DBOS suite. Each test creates its own synthetic
//! database, so the suites never share DBOS system rows. Without
//! `NAVIGATOR_TEST_POSTGRES_PORT` they skip, unless `NAV_REQUIRE_POSTGRES=1`.

mod archives;
mod billing_canary;
mod billing_digest;
mod dri_digest;
mod effects;
mod executors;
mod finance_aggregate;
mod general_nag;
mod heartbeat;
mod nightly_project_update;
mod notation_reset;
mod project_recovery;
mod reconcile;
mod retention_purge;
mod schedules;
mod source_imports;
mod support;
