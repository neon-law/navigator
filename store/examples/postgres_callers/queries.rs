//! Syntactic database call sites, including composed/local-constant arguments.
use std::collections::BTreeMap;

use quote::ToTokens;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub(super) struct Call {
    pub file: String,
    pub line: usize,
    pub function: String,
    pub operation: String,
    pub sql_operations: Vec<String>,
    pub tables: Vec<String>,
    pub transaction: bool,
}

pub(super) fn inventory(
    file: &str,
    contents: &str,
    tables: &[String],
) -> anyhow::Result<Vec<Call>> {
    let parsed = syn::parse_file(contents)?;
    let constants = parsed
        .items
        .iter()
        .filter_map(|item| {
            let syn::Item::Const(item) = item else {
                return None;
            };
            Some((
                item.ident.to_string(),
                item.expr.to_token_stream().to_string(),
            ))
        })
        .collect();
    let mut scanner = Scanner {
        file,
        tables,
        constants,
        function: "module".into(),
        calls: Vec::new(),
    };
    scanner.visit_file(&parsed);
    scanner.calls.sort();
    scanner.calls.dedup();
    Ok(scanner.calls)
}

struct Scanner<'a> {
    file: &'a str,
    tables: &'a [String],
    constants: BTreeMap<String, String>,
    function: String,
    calls: Vec<Call>,
}

impl Scanner<'_> {
    fn call(&mut self, operation: &str, argument: &str, line: usize) {
        let mut expanded = argument.to_string();
        for (name, value) in &self.constants {
            if super::contains_identifier(argument, name) {
                expanded.push(' ');
                expanded.push_str(value);
            }
        }
        let targets = if matches!(
            operation,
            "query"
                | "execute"
                | "execute_many"
                | "batch_execute"
                | "simple_query"
                | "query_as"
                | "query_scalar"
                | "raw_sql"
        ) {
            sql_targets(&expanded)
        } else {
            expanded.clone()
        };
        let tables = self
            .tables
            .iter()
            .filter(|table| super::contains_identifier(&targets, table))
            .cloned()
            .collect();
        let uppercase = expanded.to_ascii_uppercase();
        let transaction = matches!(operation, "begin" | "commit" | "rollback")
            || ["BEGIN", "COMMIT", "ROLLBACK"]
                .iter()
                .any(|word| super::contains_identifier(&uppercase, word));
        let sql_operations = [
            "SELECT", "CREATE", "INSERT", "UPDATE", "UPSERT", "DELETE", "RELATE",
        ]
        .iter()
        .filter(|word| super::contains_identifier(&uppercase, word))
        .map(|word| (*word).into())
        .collect();
        self.calls.push(Call {
            file: self.file.into(),
            line,
            function: self.function.clone(),
            operation: operation.into(),
            sql_operations,
            tables,
            transaction,
        });
    }
}

impl<'ast> Visit<'ast> for Scanner<'_> {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        let previous = std::mem::replace(&mut self.function, node.sig.ident.to_string());
        visit::visit_item_fn(self, node);
        self.function = previous;
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        let previous = std::mem::replace(&mut self.function, node.sig.ident.to_string());
        visit::visit_impl_item_fn(self, node);
        self.function = previous;
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        let previous = std::mem::replace(&mut self.function, node.sig.ident.to_string());
        visit::visit_trait_item_fn(self, node);
        self.function = previous;
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let operation = node.method.to_string();
        if matches!(
            operation.as_str(),
            "query"
                | "select"
                | "create"
                | "insert"
                | "update"
                | "upsert"
                | "delete"
                | "relate"
                | "begin"
                | "commit"
                | "rollback"
                | "execute"
                | "execute_many"
                | "batch_execute"
                | "simple_query"
        ) {
            self.call(
                &operation,
                &node.args.to_token_stream().to_string(),
                node.span().start().line,
            );
        }
        if operation == "bind"
            && node
                .args
                .to_token_stream()
                .to_string()
                .contains("record_id")
            && node
                .receiver
                .to_token_stream()
                .to_string()
                .contains("query")
        {
            self.call(
                "bind",
                &node.args.to_token_stream().to_string(),
                node.span().start().line,
            );
        }
        visit::visit_expr_method_call(self, node);
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = &*node.func {
            if let Some(segment) = path.path.segments.last() {
                let operation = segment.ident.to_string();
                if matches!(
                    operation.as_str(),
                    "query" | "query_as" | "query_scalar" | "raw_sql" | "execute"
                ) {
                    self.call(
                        &operation,
                        &node.args.to_token_stream().to_string(),
                        node.span().start().line,
                    );
                }
            }
        }
        visit::visit_expr_call(self, node);
    }
}

fn sql_targets(source: &str) -> String {
    let words: Vec<&str> = source
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .filter(|word| !word.is_empty())
        .collect();
    let mut targets = Vec::new();
    for pair in words.windows(2) {
        if matches!(
            pair[0].to_ascii_uppercase().as_str(),
            "FROM" | "JOIN" | "INTO" | "UPDATE" | "RELATE" | "DELETE" | "CREATE" | "UPSERT"
        ) {
            targets.push(pair[1]);
        }
    }
    targets.join(" ")
}

#[cfg(test)]
mod tests {
    use super::inventory;

    #[test]
    fn composed_calls_keep_function_line_operation_and_constant_targets() {
        let source = "const TABLE: &str = \"project\";\nasync fn list(db: Db) { db.select(TABLE); }\nasync fn write(db: Db) { db.query(format!(\"BEGIN TRANSACTION; UPDATE project SET name = $name; COMMIT TRANSACTION;\")); }";
        let calls = inventory("store/src/synthetic.rs", source, &["project".into()]).unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].function, "list");
        assert_eq!(calls[0].line, 2);
        assert_eq!(calls[0].tables, vec!["project"]);
        assert_eq!(calls[0].operation, "select");
        assert_eq!(calls[1].function, "write");
        assert!(calls[1].transaction);
    }

    #[test]
    fn runtime_arguments_are_retained_for_shared_backend_review() {
        let calls = inventory(
            "store/src/surreal.rs",
            "impl Db { fn run(&self, sql: String) { self.query(sql); } }",
            &["project".into()],
        )
        .unwrap();
        assert!(calls[0].tables.is_empty());
        assert_eq!(calls[0].function, "run");
    }

    #[test]
    fn executor_calls_and_default_trait_methods_preserve_scope_without_literals() {
        let calls = inventory(
            "store/src/synthetic.rs",
            "trait Repo { fn load(&mut self) { self.execute(\"SELECT * FROM project WHERE name = 'synthetic-secret'\"); } }",
            &["project".into()],
        ).unwrap();
        assert_eq!(calls[0].function, "load");
        assert_eq!(calls[0].operation, "execute");
        assert_eq!(calls[0].sql_operations, vec!["SELECT"]);
        assert_eq!(calls[0].tables, vec!["project"]);
        assert!(!serde_json::to_string(&calls)
            .unwrap()
            .contains("synthetic-secret"));
    }

    #[test]
    fn bound_record_targets_are_inventoried_without_treating_fields_as_tables() {
        let source = r#"
            const TABLE: &str = "notation_source";
            async fn attach(db: Db) {
                db.query("CREATE $id SET schema_version = $version")
                    .bind(("id", record_id(TABLE, id)))
                    .await;
            }
        "#;
        let calls = inventory(
            "store/src/synthetic.rs",
            source,
            &["notation_source".into(), "schema_version".into()],
        )
        .unwrap();
        assert!(calls.iter().any(|call| call.tables == ["notation_source"]));
        assert!(calls
            .iter()
            .all(|call| !call.tables.contains(&"schema_version".into())));
    }

    #[test]
    fn sql_table_positions_are_distinct_from_same_named_fields() {
        let source = r#"
            async fn versions(db: Db) {
                db.query("SELECT id, schema_version FROM notation_document_version WHERE id = $id");
            }
        "#;
        let calls = inventory(
            "store/src/synthetic.rs",
            source,
            &["notation_document_version".into(), "schema_version".into()],
        )
        .unwrap();
        assert_eq!(calls[0].tables, ["notation_document_version"]);
    }
}
