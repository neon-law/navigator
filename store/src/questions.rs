//! The questionnaire's question catalog — one prompt presented to a
//! respondent during template traversal — and every query against it.
//!
//! PostgreSQL holds the catalog. Explicit Source reads support historical
//! import, while application writes use the admitted catalog authority.

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::catalog;

/// `questions.audience` — only lawyers see this question.
pub const AUDIENCE_LAWYER: &str = "lawyer";
/// `questions.audience` — only the client sees this question (magic link).
pub const AUDIENCE_CLIENT: &str = "client";
/// `questions.audience` — both sides may answer this question.
pub const AUDIENCE_BOTH: &str = "both";

/// Whether a question with `audience` is shown to the client on the
/// self-serve magic-link surface (`client` or `both`).
///
/// Total rather than fallible on purpose: the schema ASSERT keeps the
/// stored set closed, and a value from outside it — a hand-written row, a
/// future audience this build predates — must not be shown to the client
/// by accident. Unknown means lawyer-only.
#[must_use]
pub fn is_client_facing(audience: &str) -> bool {
    audience == AUDIENCE_CLIENT || audience == AUDIENCE_BOTH
}

/// One question.
///
/// The application-facing shape: plain Rust types, no engine handles.
/// [`QuestionRow`] is the seam that turns it into (and back out of) what
/// the SDK reads and writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Question {
    pub id: Uuid,
    /// Canonical identifier from template frontmatter (`client_name`).
    /// Unique.
    pub code: String,
    /// The prompt as the respondent reads it.
    pub prompt: String,
    /// `string`, `int`, `bool`, `choice`, … — the widget to render.
    pub answer_type: String,
    /// `lawyer` | `client` | `both` — which side of the intake sees this
    /// question. The schema ASSERTs the closed set. Read it through
    /// [`is_client_facing`] rather than comparing by hand.
    pub audience: String,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Errors reading or writing a question.
#[derive(Debug, thiserror::Error)]
pub enum QuestionError {
    /// A database operation failed.
    /// The write collided with `question_code` — another row already holds
    /// this code.
    #[error("that question code is already in use")]
    CodeTaken,
    /// A write reported success but returned no row, or returned one this
    /// module could not read back — see [`QuestionRow::into_question`].
    #[error("writing a question returned no usable row")]
    WriteReturnedNothing,
    #[error("question writes require PostgreSQL authority")]
    AuthorityUnavailable,
    /// The PostgreSQL catalog authority could not serve the call.
    #[error(transparent)]
    Postgres(#[from] catalog::CatalogError),
}

impl From<sqlx::Error> for QuestionError {
    fn from(error: sqlx::Error) -> Self {
        Self::Postgres(error.into())
    }
}

const PG_BY_ID: &str = "SELECT id, code, prompt, answer_type, audience, inserted_at, updated_at \
     FROM question WHERE id = $1";
const PG_BY_CODE: &str = "SELECT id, code, prompt, answer_type, audience, inserted_at, updated_at \
     FROM question WHERE code = $1";
const PG_BY_CODES: &str =
    "SELECT id, code, prompt, answer_type, audience, inserted_at, updated_at \
     FROM question WHERE code = ANY($1) ORDER BY code ASC";
const PG_BY_IDS: &str = "SELECT id, code, prompt, answer_type, audience, inserted_at, updated_at \
     FROM question WHERE id = ANY($1) ORDER BY code ASC";
const PG_ALL: &str = "SELECT id, code, prompt, answer_type, audience, inserted_at, updated_at \
     FROM question ORDER BY code ASC";
const PG_INSERT: &str = "INSERT INTO question (id, code, prompt, answer_type, audience) \
     VALUES ($1, $2, $3, $4, $5) \
     RETURNING id, code, prompt, answer_type, audience, inserted_at, updated_at";
const PG_SET_AUDIENCE: &str = "UPDATE question SET audience = $2, updated_at = now() \
     WHERE code = $1 \
     RETURNING id, code, prompt, answer_type, audience, inserted_at, updated_at";

fn from_pg(row: &sqlx::postgres::PgRow) -> Result<Question, sqlx::Error> {
    use sqlx::Row;
    Ok(Question {
        id: row.try_get("id")?,
        code: row.try_get("code")?,
        prompt: row.try_get("prompt")?,
        answer_type: row.try_get("answer_type")?,
        audience: row.try_get("audience")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// The fields a new question row carries.
#[derive(Debug, Clone)]
pub struct NewQuestion {
    pub code: String,
    pub prompt: String,
    pub answer_type: String,
    /// `lawyer` | `client` | `both`; anything else is refused by the schema
    /// ASSERT. [`NewQuestion::new`] defaults it to [`AUDIENCE_BOTH`], which
    /// is what the column's `'both'` default gave every row that
    /// predates an author narrowing it.
    pub audience: String,
}

impl NewQuestion {
    #[must_use]
    pub fn new(
        code: impl Into<String>,
        prompt: impl Into<String>,
        answer_type: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            prompt: prompt.into(),
            answer_type: answer_type.into(),
            audience: AUDIENCE_BOTH.to_string(),
        }
    }

    /// Narrow this question to one side of the intake.
    #[must_use]
    pub fn with_audience(mut self, audience: impl Into<String>) -> Self {
        self.audience = audience.into();
        self
    }
}

/// Resolve a question by id.
///
/// # Errors
///
/// [`QuestionError::Db`] if the lookup fails.
pub async fn find_by_id(
    _db: &ApplicationStore,
    id: Uuid,
) -> Result<Option<Question>, QuestionError> {
    let pg = catalog::authority().await?;
    Ok(catalog::one(&pg, PG_BY_ID, id, from_pg).await?)
}

/// Resolve a question by its canonical `code`. Exact match — codes are
/// template-frontmatter identifiers, never user-typed prose.
///
/// # Errors
///
/// [`QuestionError::Db`] if the lookup fails.
pub async fn find_by_code(
    _db: &ApplicationStore,
    code: &str,
) -> Result<Option<Question>, QuestionError> {
    let pg = catalog::authority().await?;
    Ok(catalog::one(&pg, PG_BY_CODE, code.trim().to_string(), from_pg).await?)
}

/// Resolve many questions by code in one round trip — what a walker does
/// once per template rather than once per step. Codes that match nothing
/// are simply absent from the result, so the caller keys on `code` rather
/// than on position.
///
/// # Errors
///
/// [`QuestionError::Db`] if the lookup fails.
pub async fn find_by_codes(
    _db: &ApplicationStore,
    codes: &[String],
) -> Result<Vec<Question>, QuestionError> {
    if codes.is_empty() {
        return Ok(Vec::new());
    }
    let pg = catalog::authority().await?;
    Ok(catalog::all(&pg, PG_BY_CODES, Some(codes.to_vec()), from_pg).await?)
}

/// Resolve many questions by id in one round trip — what a listing does
/// after reading a page of answers, rather than one lookup per row.
///
/// # Errors
///
/// [`QuestionError::Db`] if the lookup fails.
pub async fn find_by_ids(
    _db: &ApplicationStore,
    ids: &[Uuid],
) -> Result<Vec<Question>, QuestionError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let pg = catalog::authority().await?;
    Ok(catalog::all(&pg, PG_BY_IDS, Some(ids.to_vec()), from_pg).await?)
}

/// Every question, ordered by code — the lawyer transparency listing and
/// the importer's "what is already catalogued" read.
///
/// # Errors
///
/// [`QuestionError::Db`] if the lookup fails.
pub async fn list_all(_db: &ApplicationStore) -> Result<Vec<Question>, QuestionError> {
    let pg = catalog::authority().await?;
    Ok(catalog::all(&pg, PG_ALL, None::<String>, from_pg).await?)
}

/// Write a new question row.
///
/// # Errors
///
/// [`QuestionError::CodeTaken`] when another row already holds this code,
/// and [`QuestionError::Db`] for anything else — including the schema
/// ASSERT refusing an `audience` outside `lawyer`/`client`/`both`.
pub async fn create(
    _db: &ApplicationStore,
    input: &NewQuestion,
) -> Result<Question, QuestionError> {
    let pg = catalog::authority().await?;
    let row = catalog::row(
        sqlx::query(PG_INSERT)
            .bind(Uuid::now_v7())
            .bind(input.code.trim().to_string())
            .bind(input.prompt.clone())
            .bind(input.answer_type.clone())
            .bind(input.audience.clone()),
        &pg,
    )
    .await
    .map_err(catalog::CatalogError::from);
    match row {
        Ok(row) => Ok(from_pg(&row)?),
        Err(error) if error.violates("question_code_key") => Err(QuestionError::CodeTaken),
        Err(error) => Err(error.into()),
    }
}

/// Narrow the question holding `code` to `audience`, the seed's path for
/// reconciling a row written before its canonical audience was declared.
///
/// # Errors
///
/// [`QuestionError::Db`] if the write fails, including the schema ASSERT
/// refusing an `audience` outside `lawyer`/`client`/`both`.
pub async fn set_audience(
    _db: &ApplicationStore,
    code: &str,
    audience: &str,
) -> Result<Option<Question>, QuestionError> {
    let pg = catalog::authority().await?;
    let row = catalog::maybe_row(
        sqlx::query(PG_SET_AUDIENCE)
            .bind(code.to_string())
            .bind(audience.to_string()),
        &pg,
    )
    .await?;
    Ok(row.as_ref().map(from_pg).transpose()?)
}

/// Find the question holding `input.code`, creating it if absent.
///
/// Race-safe without a lock: a concurrent creator that wins the
/// `question_code` unique index turns this call's insert into
/// [`QuestionError::CodeTaken`], which is re-read as the winner's row.
/// `cli import` and the seed both run this on every boot, and the cucumber
/// suite runs concurrent scenarios against one engine, so idempotence under
/// contention is the contract rather than a nicety.
///
/// The existing row is returned **unchanged**: a question's prompt and
/// answer type are template-authored, and silently rewriting them here
/// would make an import reorder the catalog rather than extend it.
///
/// # Errors
///
/// [`QuestionError::Db`] if the lookup or the write fails.
pub async fn find_or_create(
    db: &ApplicationStore,
    input: &NewQuestion,
) -> Result<Question, QuestionError> {
    if let Some(found) = find_by_code(db, &input.code).await? {
        return Ok(found);
    }
    match create(db, input).await {
        Ok(created) => Ok(created),
        Err(QuestionError::CodeTaken) => find_by_code(db, &input.code)
            .await?
            .ok_or(QuestionError::WriteReturnedNothing),
        Err(other) => Err(other),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        create, find_by_code, find_by_codes, find_by_id, find_by_ids, find_or_create,
        is_client_facing, list_all, NewQuestion, QuestionError, AUDIENCE_BOTH, AUDIENCE_CLIENT,
        AUDIENCE_LAWYER,
    };
    use crate::test_support;

    #[test]
    fn audience_filters_the_client_visible_set() {
        assert!(is_client_facing(AUDIENCE_CLIENT));
        assert!(is_client_facing(AUDIENCE_BOTH));
        assert!(!is_client_facing(AUDIENCE_LAWYER));
        // An unknown/garbage audience is not shown to the client.
        assert!(!is_client_facing("nonsense"));
    }

    #[tokio::test]
    async fn a_created_question_reads_back_whole() {
        test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let written = create(
                &db,
                &NewQuestion::new("client_name", "Your name?", "string"),
            )
            .await
            .unwrap();

            assert_eq!(written.code, "client_name");
            assert_eq!(written.prompt, "Your name?");
            assert_eq!(written.answer_type, "string");
            assert_eq!(
                written.audience, AUDIENCE_BOTH,
                "a question defaults to both sides of the intake"
            );
            assert_eq!(find_by_id(&db, written.id).await.unwrap(), Some(written));
        })
        .await;
    }

    #[tokio::test]
    async fn a_question_is_found_by_its_code() {
        test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let written = create(&db, &NewQuestion::new("trustee_name", "Who?", "string"))
                .await
                .unwrap();

            assert_eq!(
                find_by_code(&db, "trustee_name").await.unwrap(),
                Some(written)
            );
            assert_eq!(find_by_code(&db, "no_such_code").await.unwrap(), None);
        })
        .await;
    }

    #[tokio::test]
    async fn a_duplicate_code_is_refused_by_the_index() {
        test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            create(
                &db,
                &NewQuestion::new("client_name", "Your name?", "string"),
            )
            .await
            .unwrap();

            let again = create(&db, &NewQuestion::new("client_name", "Again?", "string")).await;
            assert!(
                matches!(again, Err(QuestionError::CodeTaken)),
                "a second row under one code must be refused; got {again:?}"
            );
        })
        .await;
    }

    /// The seed and `cli import` both run on every boot, so a second pass
    /// must return the row rather than a conflict — and must not rewrite
    /// the template-authored prompt.
    #[tokio::test]
    async fn find_or_create_is_idempotent_and_leaves_the_existing_row_alone() {
        test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let first = find_or_create(
                &db,
                &NewQuestion::new("client_name", "Your name?", "string"),
            )
            .await
            .unwrap();
            let second = find_or_create(
                &db,
                &NewQuestion::new("client_name", "A DIFFERENT PROMPT", "int"),
            )
            .await
            .unwrap();

            assert_eq!(first, second, "the second pass returns the existing row");
            assert_eq!(second.prompt, "Your name?");
            assert_eq!(list_all(&db).await.unwrap().len(), 1);
        })
        .await;
    }

    /// The cucumber suite runs concurrent scenarios against one engine, so
    /// a seeder that assumed exclusivity would flake rather than fail.
    #[tokio::test]
    async fn concurrent_find_or_create_on_one_code_settles_on_one_row() {
        test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let racers = (0..6).map(|_| {
                let db = db.clone();
                async move {
                    find_or_create(
                        &db,
                        &NewQuestion::new("client_name", "Your name?", "string"),
                    )
                    .await
                    .expect("every racer resolves to a row")
                    .id
                }
            });
            let ids = futures::future::join_all(racers).await;
            assert!(
                ids.windows(2).all(|w| w[0] == w[1]),
                "the racers disagreed about which row won: {ids:?}"
            );
            assert_eq!(
                list_all(&db).await.unwrap().len(),
                1,
                "one question, not six"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn an_audience_outside_the_closed_set_is_refused_by_the_schema() {
        test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            // A typo'd audience would silently hide the question from the
            // client rather than fail, which is why the schema ASSERTs it.
            let refused = create(
                &db,
                &NewQuestion::new("client_name", "Your name?", "string").with_audience("clientt"),
            )
            .await;
            assert!(refused.is_err(), "the engine accepted audience `clientt`");
        })
        .await;
    }

    #[tokio::test]
    async fn a_client_only_question_keeps_its_audience() {
        test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let written = create(
                &db,
                &NewQuestion::new("client_name", "Your name?", "string")
                    .with_audience(AUDIENCE_CLIENT),
            )
            .await
            .unwrap();
            assert!(is_client_facing(&written.audience));

            let lawyer_only = create(
                &db,
                &NewQuestion::new("internal_note", "Note", "string").with_audience(AUDIENCE_LAWYER),
            )
            .await
            .unwrap();
            assert!(!is_client_facing(&lawyer_only.audience));
            let narrowed = super::set_audience(&db, "client_name", AUDIENCE_LAWYER)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(narrowed.audience, AUDIENCE_LAWYER);
            assert!(super::set_audience(&db, "missing", AUDIENCE_CLIENT)
                .await
                .unwrap()
                .is_none());
            assert!(super::set_audience(&db, "client_name", "invalid")
                .await
                .is_err());
        })
        .await;
    }

    #[tokio::test]
    async fn batched_lookups_resolve_by_code_and_by_id() {
        test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let a = create(&db, &NewQuestion::new("aaa", "A?", "string"))
                .await
                .unwrap();
            let b = create(&db, &NewQuestion::new("bbb", "B?", "string"))
                .await
                .unwrap();
            create(&db, &NewQuestion::new("ccc", "C?", "string"))
                .await
                .unwrap();

            let by_code = find_by_codes(&db, &["bbb".into(), "aaa".into(), "nope".into()])
                .await
                .unwrap();
            assert_eq!(
                by_code,
                vec![a.clone(), b.clone()],
                "an unmatched code is simply absent"
            );

            let by_id = find_by_ids(&db, &[b.id, a.id]).await.unwrap();
            assert_eq!(by_id, vec![a, b]);
        })
        .await;
    }

    #[tokio::test]
    async fn empty_batches_do_not_reach_the_engine() {
        let db = crate::test_support::uninitialized_store();
        assert!(find_by_codes(&db, &[]).await.unwrap().is_empty());
        assert!(find_by_ids(&db, &[]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_catalog_lists_in_code_order() {
        test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            for code in ["ccc", "aaa", "bbb"] {
                create(&db, &NewQuestion::new(code, "?", "string"))
                    .await
                    .unwrap();
            }
            let codes: Vec<String> = list_all(&db)
                .await
                .unwrap()
                .into_iter()
                .map(|q| q.code)
                .collect();
            assert_eq!(codes, ["aaa", "bbb", "ccc"]);
        })
        .await;
    }
}
