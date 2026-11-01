# Glossary

Neon Law Navigator's vocabulary. Most terms name a table in the [`store`](../../store/). A definition cites that store
module or the Surreal schema. [ERD.svg](/public/glossary/ERD.svg) shows every SurrealDB table.

When an entry names a table or field, it uses the schema's singular name, such as `person.role` and
`person_project_role.participation`. `store::persons` and `store::projects` are Rust module names.

## Authoring

One file per term. Each `<slug>.md` beside this README is one entry: its `title:` frontmatter is the term as a reader
says it, the file name is the stable reference key, and the body is the definition. The web publishes every entry on one
page at `/glossary`, anchored at `#<slug>`, and the CLI reads the same files through `navigator glossary list` and
`navigator glossary show <term>`. That page appends `store/postgres/surreal-source-erd.mmd` after the opening above.
`server/public/glossary/ERD.svg` is that diagram, rendered with Mermaid 11.12.0, the version the site pins.

To add a term, create `<slug>.md` whose name is the slug of its `title:` (`Lawyer Review` → `lawyer-review.md`). Its
frontmatter requires a `description:` with one plain-text sentence ending in a period and no Markdown links. The CLI
lists the title and description; `show` accepts the quoted title, regardless of case. Link other entries as siblings
(`[Matter](matter.md)`). When a table matters to a definition, link to its `DEFINE TABLE` statement in
`store/src/schema/navigator.surql` instead of copying the schema into the glossary.

Every SurrealDB table has exactly one owning entry. State its attribution on a line beginning `The table is`, followed
by the table name in backticks and a period. Include internal tables, explaining which term they serve. The store's
`glossary_table_coverage` test checks these attributions against every `DEFINE TABLE` in the schema, including views,
and rejects missing, duplicate, and unknown table names. Other entries may mention a table without claiming it.
