//! Print a review draft of one database's archive coverage manifest.
//!
//! Reads the database's relations through [`archives::coverage::inventory`]
//! and prints [`archives::coverage::draft`] as JSON. Every ordinary view comes
//! out with an empty reason, which the archive refuses until a reviewer writes
//! how it is rebuilt. Connect as a login that can read every relation (the
//! bootstrap superuser of a synthetic database), never a production one:
//!
//! ```bash
//! cargo run -p archives --example coverage_inventory -- \
//!   postgres://postgres:synthetic@127.0.0.1:25499/forgejo
//! ```
//!
//! Copy the reviewed result into `k8s/postgres/archive/coverage.json` under
//! `navigator` or `forgejo`. See `docs/iceberg-archive.md`.

use anyhow::Context as _;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let url = std::env::args()
        .nth(1)
        .context("usage: coverage_inventory <postgres-url>")?;
    let pool = sqlx::PgPool::connect(&url).await?;
    let mut tx = pool.begin().await?;
    let observed = archives::coverage::inventory(&mut tx).await?;
    tx.rollback().await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&archives::coverage::draft(observed))?
    );
    Ok(())
}
