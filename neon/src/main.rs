//! The `neon-server` binary: `www.neonlaw.com`.
//!
//! The composition it serves lives in the crate's library, so this binary and
//! the tests that exercise its router cannot disagree. See [`neon::brand`].
//!
//! `NAVIGATOR_DBOS_ENABLED=true` also hosts the DBOS durable-execution
//! executor in this process, launched before the server serves and shut down
//! after it drains; unset or `false`, the server boots exactly as without it.
//! See `docs/dbos-durability.md`.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    portal::hosting::run_with_dbos(neon::brand()).await
}
