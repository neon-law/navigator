//! Bound raw source rows before decoding and encoded buffers before allocation growth.
use anyhow::{ensure, Result};

pub(crate) struct ExportBounds {
    pub generation_bytes: usize,
    pub object_bytes: usize,
    pub rows_per_table: usize,
}
impl ExportBounds {
    pub(super) async fn check_source(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        schema: &str,
        table: &str,
    ) -> Result<()> {
        let query=format!("SELECT count(*)::bigint, coalesce(sum(octet_length(to_jsonb(t)::text)),0)::bigint FROM ONLY {}.{} AS t",super::quote_ident(schema),super::quote_ident(table));
        let (rows, bytes): (i64, i64) = sqlx::query_as(sqlx::AssertSqlSafe(query))
            .fetch_one(&mut **tx)
            .await?;
        ensure!(
            usize::try_from(rows)? <= self.rows_per_table
                && usize::try_from(bytes)? <= self.object_bytes,
            "raw archive source bound exceeded"
        );
        Ok(())
    }
}
pub(super) struct Buffer {
    bytes: Vec<u8>,
    limit: usize,
}
impl Buffer {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }
    pub(super) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}
impl std::io::Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self
            .bytes
            .len()
            .checked_add(bytes.len())
            .is_none_or(|size| size > self.limit)
        {
            return Err(std::io::Error::other("encoded archive byte bound exceeded"));
        }
        self.bytes.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.bytes.flush()
    }
}
