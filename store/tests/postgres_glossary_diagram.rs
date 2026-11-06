//! The published ERD names real columns, including tables with multiline constraints.

use std::collections::BTreeSet;

#[test]
fn multiline_constraints_and_references_do_not_become_columns() {
    let schema = concat!(
        "CREATE TABLE project (\n    id UUID PRIMARY KEY\n);\n",
        "CREATE TABLE item (\n",
        "    id UUID PRIMARY KEY,\n",
        "    project_id UUID\n        REFERENCES project(id),\n",
        "    status TEXT NOT NULL,\n",
        "    CONSTRAINT valid_status CHECK (\n",
        "        status IN ('open', 'closed')\n",
        "    )\n",
        ");\n",
    );
    let tables = store::postgres_schema::parse_schema(schema).unwrap();
    let item = &tables["item"];
    assert_eq!(
        item.columns
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["id", "project_id", "status"])
    );
    assert_eq!(item.columns["status"], "TEXT");
    assert!(item
        .foreign_keys
        .contains(&("project_id".into(), "project".into())));
}

#[test]
fn the_canonical_diagram_has_only_sql_identifiers_for_columns() {
    let tables =
        store::postgres_schema::parse_schema(store::postgres_schema::canonical_schema()).unwrap();
    for table in tables.values() {
        for column in table.columns.keys() {
            assert!(
                column
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
                "{}.{} is a constraint continuation, not a column",
                table.name,
                column
            );
        }
    }
}
