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
`hudson glossary show <term>`. The page links the full PostgreSQL diagram at `/public/glossary/postgres/ERD.svg`. Each
stored term has a collapsed **Table definition** showing its canonical PostgreSQL declarations, alterations, and indexes
in migration order. The page selects these from `store/postgres/schema.sql`; definitions do not copy SQL into Markdown.

To add a term, create `<slug>.md` whose name is the slug of its `title:` (`Lawyer Review` → `lawyer-review.md`). Its
frontmatter requires a `description:` with one plain-text sentence ending in a period and no Markdown links. The CLI
lists the title and description; `show` accepts the quoted title, regardless of case. Link other entries as siblings
(`[Matter](matter.md)`). When a table matters to a definition, link to its PostgreSQL `CREATE TABLE` definition and the
migrations that maintain it. Keep SQL and rollout instructions in the storage docs.

Each table ownership attribution begins `The table is`, followed by its name in backticks and a period. Other entries
may mention a table without claiming it. The PostgreSQL glossary coverage test requires exactly one owning definition
for every table. Compatibility-only records use an explicit SurrealDB compatibility attribution; see [SurrealDB
compatibility](../surreal-compatibility.md#model-references-and-coverage).

Regenerate the linked SVG after changing `store/postgres/postgres-erd.mmd`. Start a local ChromeDriver, set
`WEBDRIVER_URL` to its loopback URL and `CHROME_BINARY` to the pinned Chrome for Testing binary, then run:

```bash
cargo run -p portal --example glossary_erd
```

The exporter uses the repository's pinned Mermaid asset. The glossary test checks the SVG's source digest against the
canonical diagram, so a schema change requires a fresh export.
