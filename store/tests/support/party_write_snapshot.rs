//! Whole canonical row snapshot for duplicate-write isolation.
use serde_json::Value;
use store::postgres::PostgresDb;

pub async fn snapshot(pg: &PostgresDb) -> Vec<Value> {
    let mut rows = Vec::new();
    for table in [
        "public.person",
        "public.project",
        "public.firm",
        "public.entity",
        "public.person_mailbox",
        "public.person_project_role",
        "public.person_firm_role",
        "public.entity_role",
        "public.cap_table_holding",
        "public.fiduciary_role",
        "public.relationship",
        "public.relationship_log",
        "navigator_graph.\"Person\"",
        "navigator_graph.\"Entity\"",
        "navigator_graph.\"Project\"",
        "navigator_graph.\"Firm\"",
        "navigator_graph.\"HAS_ROLE\"",
        "navigator_graph.\"RELATIONSHIP\"",
        "navigator_graph.\"WORKS_ON\"",
        "navigator_graph.\"MEMBER_OF\"",
        "navigator_graph.\"HANDLES\"",
    ] {
        rows.push(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]'::jsonb) FROM {table} t")))
            .fetch_one(pg.pool()).await.unwrap());
    }
    rows
}
