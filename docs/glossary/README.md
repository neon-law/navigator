# Glossary

Neon Law Navigator's vocabulary. Most terms name a table in [`store`](../../store/). Definitions cite the PostgreSQL
relational model in [`store/postgres/schema.sql`](../../store/postgres/schema.sql) and its canonical Rust accessor. Use
singular table names, such as `person.role` and `person_project_role.participation`; `store::persons` and
`store::projects` name Rust modules.

A schema link describes the model. The accessor and deployment configuration determine the active write path. Prefer
PostgreSQL for new models and supported writes; see [Storage models and writes](../storage-models.md). Keep
engine-specific mechanics in [SurrealDB compatibility](../surreal-compatibility.md).

## Authoring

One file per term. Each `<slug>.md` beside this README is one entry: its `title:` frontmatter is the term as a reader
says it, the file name is the stable reference key, and the body is the definition. The web publishes every entry on one
page at `/glossary`, anchored at `#<slug>`, and the CLI reads the same files through `navigator glossary list` and
`navigator glossary show <term>`. That page appends `store/postgres/postgres-erd.mmd` after the opening above. The site
renders this PostgreSQL diagram with its pinned Mermaid version.

To add a term, create `<slug>.md` whose name is the slug of its `title:` (`Lawyer Review` → `lawyer-review.md`). Its
frontmatter requires a `description:` with one plain-text sentence ending in a period and no Markdown links. The CLI
lists the title and description; `show` accepts the quoted title, regardless of case. Link other entries as siblings
(`[Matter](matter.md)`). When a table matters to a definition, link to its PostgreSQL `CREATE TABLE` definition and the
migrations that maintain it. Keep SQL and rollout instructions in the storage docs.

Each table ownership attribution begins `The table is`, followed by its name in backticks and a period. Other entries
may mention a table without claiming it. Compatibility schema coverage is documented in [SurrealDB
compatibility](../surreal-compatibility.md#model-references-and-coverage).
