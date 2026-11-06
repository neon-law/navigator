//! Table declarations selected from the canonical migration assembly.

use std::collections::BTreeMap;

pub(super) fn table_sql(schema: &str) -> BTreeMap<String, String> {
    let mut tables: BTreeMap<String, String> = BTreeMap::new();
    let mut statement = String::new();
    for line in schema.lines() {
        // These declarations have a controlled format in schema.sql. Function
        // bodies and data migrations are deliberately outside this display.
        if statement.is_empty()
            && ![
                "CREATE TABLE ",
                "ALTER TABLE ",
                "CREATE INDEX ",
                "CREATE UNIQUE INDEX ",
            ]
            .iter()
            .any(|prefix| line.starts_with(prefix))
        {
            continue;
        }
        if line.trim_start().starts_with("--") {
            continue;
        }
        statement.push_str(line);
        statement.push('\n');
        if !line.trim_end().ends_with(';') {
            continue;
        }
        let words: Vec<_> = statement.split_whitespace().collect();
        let table = if statement.starts_with("CREATE TABLE ") {
            words.get(if words.get(2) == Some(&"IF") { 5 } else { 2 })
        } else if statement.starts_with("ALTER TABLE ") {
            words.get(2)
        } else {
            words
                .iter()
                .position(|word| *word == "ON")
                .and_then(|index| words.get(index + 1))
        };
        if let Some(table) = table {
            let name = table
                .split('(')
                .next()
                .unwrap_or_default()
                .trim_matches('"');
            let sql = tables.entry(name.to_string()).or_default();
            if !sql.is_empty() {
                sql.push('\n');
            }
            sql.push_str(statement.trim_end());
            sql.push('\n');
        }
        statement.clear();
    }
    tables
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_table_keeps_its_constraints_alterations_and_indexes_without_other_tables() {
        let schema = concat!(
            "-- A schema comment.\n",
            "CREATE TABLE \"holding\" (\n",
            "    id UUID PRIMARY KEY,\n",
            "    shares BIGINT NOT NULL CHECK (shares >= 0)\n",
            ");\n",
            "CREATE TABLE person (\n    id UUID PRIMARY KEY\n);\n",
            "ALTER TABLE holding ADD COLUMN owner_id UUID REFERENCES person(id);\n",
            "CREATE UNIQUE INDEX holding_owner\n    ON holding (owner_id);\n",
            "ALTER TABLE holding ADD CONSTRAINT positive CHECK (shares > 0);\n",
            "ALTER TABLE holding DROP CONSTRAINT positive;\n",
        );
        let tables = table_sql(schema);
        let holding = &tables["holding"];
        assert!(holding.contains("shares BIGINT NOT NULL CHECK (shares >= 0)"));
        assert!(holding.contains("owner_id UUID REFERENCES person(id)"));
        assert!(holding.contains("CREATE UNIQUE INDEX holding_owner\n    ON holding"));
        assert!(holding.contains("DROP CONSTRAINT positive"));
        assert!(!holding.contains("CREATE TABLE person"));
        assert!(!holding.contains("schema comment"));
        assert!(holding.find("ADD CONSTRAINT").unwrap() < holding.find("DROP CONSTRAINT").unwrap());
    }

    #[test]
    fn optional_create_guard_and_exact_table_names_are_supported() {
        let tables = table_sql(concat!(
            "CREATE TABLE IF NOT EXISTS item (\n    id UUID PRIMARY KEY\n);\n",
            "CREATE TABLE item_event (\n    item_id UUID REFERENCES item(id)\n);\n",
            "CREATE INDEX item_event_item ON item_event(item_id);\n",
        ));
        assert_eq!(tables.len(), 2);
        assert!(!tables["item"].contains("CREATE INDEX"));
        assert!(tables["item_event"].contains("CREATE INDEX"));
    }
}
