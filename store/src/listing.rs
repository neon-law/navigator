//! The list contract every collection read shares: JSON:API `filter[...]` and
//! `sort`, and keyset pages through the JSON:API cursor pagination profile
//! (`page[size]`, `page[after]`, `page[before]`).
//!
//! Keyset, never `OFFSET`: a page seeks to the last row it showed through the
//! sort's index, so page 200 costs what page 1 does and a row inserted mid-read
//! never shifts the next page. Every sort ends on `id`, which makes the order
//! total. A cursor is opaque to the caller; it names the sort it was cut for,
//! so it is refused under any other sort.

use std::collections::BTreeMap;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};

/// Rows a page holds when the caller names no `page[size]`.
pub const DEFAULT_PAGE_SIZE: u32 = 50;
/// The largest `page[size]` a caller may ask for.
pub const MAX_PAGE_SIZE: u32 = 100;

/// What one collection accepts: the filter members and sort fields it
/// advertises, and the order it falls back to.
#[derive(Debug, Clone, Copy)]
pub struct ListSpec {
    pub filters: &'static [&'static str],
    pub sorts: &'static [&'static str],
    /// `(field, descending)` pairs used when the caller names no `sort`.
    pub default_sort: &'static [(&'static str, bool)],
}

/// Which side of the cursor a page reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    After,
    Before,
}

/// A decoded `page[after]` / `page[before]`: the sort values and `id` of the
/// row the page starts beyond, in sort order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    pub direction: Direction,
    pub values: Vec<String>,
}

/// A validated list request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListParams {
    pub filters: BTreeMap<String, String>,
    /// Never empty; `id` is appended by the queries, not listed here.
    pub sort: Vec<(String, bool)>,
    pub size: u32,
    pub cursor: Option<Cursor>,
}

/// Why a list request was refused. Each is a client error (HTTP 400).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ListError {
    #[error("sort field `{0}` is not supported")]
    UnknownSort(String),
    #[error("filter `{0}` is not supported")]
    UnknownFilter(String),
    #[error("page parameter `{0}` is not supported")]
    UnknownPage(String),
    #[error("page[size] must be a whole number from 1 to {MAX_PAGE_SIZE}")]
    PageSize,
    #[error("the page cursor is not valid for this list and sort")]
    Cursor,
    #[error("page[after] and page[before] cannot be combined")]
    BothCursors,
}

#[derive(Serialize, Deserialize)]
struct CursorWire {
    /// The canonical sort the cursor was cut for.
    s: String,
    v: Vec<String>,
}

impl ListParams {
    /// Parse a raw query string against a collection's [`ListSpec`].
    /// Parameters outside the `filter`, `sort`, and `page` families are left
    /// to the caller (a page's own flash parameters, for instance).
    ///
    /// # Errors
    ///
    /// [`ListError`] for any member the spec does not advertise, an
    /// out-of-range size, or a cursor cut for another sort.
    pub fn parse(query: &str, spec: &ListSpec) -> Result<Self, ListError> {
        let mut filters = BTreeMap::new();
        let mut sort_raw = None;
        let mut size = DEFAULT_PAGE_SIZE;
        let (mut after, mut before) = (None, None);
        for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
            if key == "sort" {
                sort_raw = Some(value.into_owned());
            } else if let Some(member) = bracketed(&key, "filter") {
                if !spec.filters.contains(&member) {
                    return Err(ListError::UnknownFilter(member.to_string()));
                }
                let value = value.trim();
                if !value.is_empty() {
                    filters.insert(member.to_string(), value.to_string());
                }
            } else if let Some(member) = bracketed(&key, "page") {
                match member {
                    "size" => {
                        size = value
                            .parse::<u32>()
                            .ok()
                            .filter(|n| (1..=MAX_PAGE_SIZE).contains(n))
                            .ok_or(ListError::PageSize)?;
                    }
                    "after" => after = Some(value.into_owned()),
                    "before" => before = Some(value.into_owned()),
                    other => return Err(ListError::UnknownPage(other.to_string())),
                }
            }
        }
        let sort = parse_sort(sort_raw.as_deref().unwrap_or(""), spec)?;
        let cursor = match (after, before) {
            (Some(_), Some(_)) => return Err(ListError::BothCursors),
            (Some(raw), None) => Some(decode_cursor(&raw, Direction::After, &sort)?),
            (None, Some(raw)) => Some(decode_cursor(&raw, Direction::Before, &sort)?),
            (None, None) => None,
        };
        Ok(Self {
            filters,
            sort,
            size,
            cursor,
        })
    }

    /// The trimmed filter value, or `""` when the caller named none.
    #[must_use]
    pub fn filter(&self, member: &str) -> &str {
        self.filters.get(member).map_or("", String::as_str)
    }

    /// The sort in its `sort=` spelling (`name,-email`).
    #[must_use]
    pub fn sort_param(&self) -> String {
        canonical_sort(&self.sort)
    }

    /// Rows to fetch: one more than the page, so the read itself says whether
    /// another page exists.
    #[must_use]
    pub fn fetch_limit(&self) -> i64 {
        i64::from(self.size) + 1
    }

    /// Whether the read walks the sort backwards (a `page[before]` read).
    #[must_use]
    pub fn reversed(&self) -> bool {
        self.cursor
            .as_ref()
            .is_some_and(|cursor| cursor.direction == Direction::Before)
    }
}

fn bracketed<'a>(key: &'a str, family: &str) -> Option<&'a str> {
    key.strip_prefix(family)?
        .strip_prefix('[')?
        .strip_suffix(']')
}

fn parse_sort(raw: &str, spec: &ListSpec) -> Result<Vec<(String, bool)>, ListError> {
    let mut sort = Vec::new();
    for segment in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (field, descending) = match segment.strip_prefix('-') {
            Some(field) => (field, true),
            None => (segment, false),
        };
        if !spec.sorts.contains(&field) {
            return Err(ListError::UnknownSort(field.to_string()));
        }
        if !sort.iter().any(|(seen, _)| seen == field) {
            sort.push((field.to_string(), descending));
        }
    }
    if sort.is_empty() {
        sort = spec
            .default_sort
            .iter()
            .map(|(field, descending)| ((*field).to_string(), *descending))
            .collect();
    }
    Ok(sort)
}

fn canonical_sort(sort: &[(String, bool)]) -> String {
    sort.iter()
        .map(|(field, descending)| {
            if *descending {
                format!("-{field}")
            } else {
                field.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn decode_cursor(
    raw: &str,
    direction: Direction,
    sort: &[(String, bool)],
) -> Result<Cursor, ListError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(raw.trim())
        .map_err(|_| ListError::Cursor)?;
    let wire: CursorWire = serde_json::from_slice(&bytes).map_err(|_| ListError::Cursor)?;
    let id_is_uuid = wire
        .v
        .last()
        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok());
    if wire.s != canonical_sort(sort) || wire.v.len() != sort.len() + 1 || !id_is_uuid {
        return Err(ListError::Cursor);
    }
    Ok(Cursor {
        direction,
        values: wire.v,
    })
}

fn encode_cursor(sort: &[(String, bool)], values: Vec<String>) -> String {
    let wire = CursorWire {
        s: canonical_sort(sort),
        v: values,
    };
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(&wire).unwrap_or_default())
}

/// One page of a list, with the cursors that reach its neighbours.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// `page[after]` for the next page; `None` on the last page.
    pub next: Option<String>,
    /// `page[before]` for the previous page; `None` on the first page.
    pub prev: Option<String>,
}

impl<T> Page<T> {
    /// Cut a page from rows fetched with [`ListParams::fetch_limit`] in the
    /// read's order (reversed for a `page[before]` read). `key` yields a row's
    /// value for a sort field, and for `"id"`.
    pub fn from_rows(
        mut rows: Vec<T>,
        params: &ListParams,
        key: impl Fn(&T, &str) -> String,
    ) -> Self {
        let size = usize::try_from(params.size).unwrap_or(usize::MAX);
        let more = rows.len() > size;
        rows.truncate(size);
        let reversed = params.reversed();
        if reversed {
            rows.reverse();
        }
        let cursor_for = |row: &T| {
            let mut values: Vec<String> = params
                .sort
                .iter()
                .map(|(field, _)| key(row, field))
                .collect();
            values.push(key(row, "id"));
            encode_cursor(&params.sort, values)
        };
        // Reading forward, the page beyond exists when the read overflowed;
        // the page behind exists whenever a cursor brought us here. A
        // backward read mirrors that.
        let (has_next, has_prev) = if reversed {
            (true, more)
        } else {
            (more, params.cursor.is_some())
        };
        Self {
            next: has_next.then(|| rows.last().map(&cursor_for)).flatten(),
            prev: has_prev.then(|| rows.first().map(&cursor_for)).flatten(),
            items: rows,
        }
    }

    pub fn map<U>(self, f: impl FnMut(T) -> U) -> Page<U> {
        Page {
            items: self.items.into_iter().map(f).collect(),
            next: self.next,
            prev: self.prev,
        }
    }
}

/// Escape `%`, `_`, and `\` so a caller's text matches literally inside a
/// `LIKE` / `ILIKE` pattern (PostgreSQL's default escape is `\`).
#[must_use]
pub fn like_contains(needle: &str) -> String {
    let mut pattern = String::with_capacity(needle.len() + 2);
    pattern.push('%');
    for ch in needle.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    pattern.push('%');
    pattern
}

/// The keyset predicate for PostgreSQL: rows strictly beyond the cursor in
/// the read's order. `columns` gives each sort field's SQL expression and
/// cast, in sort order, then `id`'s, e.g. `[("email", "citext"), ("id", "uuid")]`. Binds start at
/// `$first_bind`; the caller binds the cursor's values in order.
///
/// A uniform direction becomes one row comparison, `(a, b, id) > ($1, $2,
/// $3)`, which PostgreSQL answers from a `(a, b, id)` index. A mixed one
/// expands to `a > $1 OR (a = $1 AND …)`.
#[must_use]
pub fn pg_keyset(
    params: &ListParams,
    columns: &[(&str, &str)],
    first_bind: usize,
) -> Option<String> {
    params.cursor.as_ref()?;
    let directions = sort_directions(params);
    let reversed = params.reversed();
    let op = |descending: bool| if descending == reversed { ">" } else { "<" };
    let bind = |i: usize, cast: &str| format!("${}::{cast}", first_bind + i);
    if directions.iter().all(|d| *d == directions[0]) {
        let lhs = columns
            .iter()
            .map(|(sql, _)| *sql)
            .collect::<Vec<_>>()
            .join(", ");
        let rhs = columns
            .iter()
            .enumerate()
            .map(|(i, (_, cast))| bind(i, cast))
            .collect::<Vec<_>>()
            .join(", ");
        return Some(format!("({lhs}) {} ({rhs})", op(directions[0])));
    }
    let mut arms = Vec::new();
    for (i, (sql, cast)) in columns.iter().enumerate() {
        let mut terms: Vec<String> = columns[..i]
            .iter()
            .enumerate()
            .map(|(j, (prior, prior_cast))| format!("{prior} = {}", bind(j, prior_cast)))
            .collect();
        terms.push(format!("{sql} {} {}", op(directions[i]), bind(i, cast)));
        arms.push(format!("({})", terms.join(" AND ")));
    }
    Some(format!("({})", arms.join(" OR ")))
}

/// The `ORDER BY` list for PostgreSQL, `id` last, flipped for a backward read.
#[must_use]
pub fn pg_order(params: &ListParams, columns: &[(&str, &str)]) -> String {
    let reversed = params.reversed();
    columns
        .iter()
        .zip(sort_directions(params))
        .map(|((sql, _), descending)| {
            format!(
                "{sql} {}",
                if descending == reversed {
                    "ASC"
                } else {
                    "DESC"
                }
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Each sort field's direction, then `id`'s, which follows the last field so
/// a uniform sort stays one index scan.
fn sort_directions(params: &ListParams) -> Vec<bool> {
    let mut directions: Vec<bool> = params.sort.iter().map(|(_, d)| *d).collect();
    directions.push(directions.last().copied().unwrap_or(false));
    directions
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: ListSpec = ListSpec {
        filters: &["name", "email"],
        sorts: &["name", "email"],
        default_sort: &[("name", false)],
    };

    fn parse(query: &str) -> Result<ListParams, ListError> {
        ListParams::parse(query, &SPEC)
    }

    #[test]
    fn defaults_apply_when_the_caller_names_nothing() {
        let params = parse("").unwrap();
        assert_eq!(params.sort, vec![("name".to_string(), false)]);
        assert_eq!(params.size, DEFAULT_PAGE_SIZE);
        assert!(params.filters.is_empty() && params.cursor.is_none());
    }

    #[test]
    fn json_api_members_parse_and_unknown_ones_are_refused() {
        let params =
            parse("filter%5Bname%5D=%20ann%20&sort=-email,name&page[size]=10&error=x").unwrap();
        assert_eq!(params.filter("name"), "ann");
        assert_eq!(params.filter("email"), "");
        assert_eq!(params.sort_param(), "-email,name");
        assert_eq!(params.size, 10);
        assert_eq!(
            parse("sort=role"),
            Err(ListError::UnknownSort("role".into()))
        );
        assert_eq!(
            parse("filter[role]=x"),
            Err(ListError::UnknownFilter("role".into()))
        );
        assert_eq!(
            parse("page[number]=2"),
            Err(ListError::UnknownPage("number".into()))
        );
        assert_eq!(parse("page[size]=0"), Err(ListError::PageSize));
        assert_eq!(parse("page[size]=101"), Err(ListError::PageSize));
    }

    #[test]
    fn a_cursor_is_bound_to_the_sort_it_was_cut_for() {
        let params = parse("").unwrap();
        let nil = uuid::Uuid::nil().to_string();
        let page = Page::from_rows(
            vec!["a", "b", "c"],
            &ListParams { size: 2, ..params },
            |row, field| {
                if field == "id" {
                    nil.clone()
                } else {
                    (*row).to_string()
                }
            },
        );
        let next = page.next.expect("an overflowing read has a next page");
        assert!(page.prev.is_none());
        assert!(parse(&format!("page[after]={next}")).is_ok());
        assert_eq!(
            parse(&format!("sort=-name&page[after]={next}")),
            Err(ListError::Cursor)
        );
        assert_eq!(parse("page[after]=not-a-cursor"), Err(ListError::Cursor));
        assert_eq!(
            parse(&format!("page[after]={next}&page[before]={next}")),
            Err(ListError::BothCursors)
        );
    }

    #[test]
    fn a_backward_read_restores_sort_order_and_links_both_ways() {
        let next = encode_cursor(
            &[("name".into(), false)],
            vec!["m".into(), uuid::Uuid::nil().to_string()],
        );
        let mut params = parse(&format!("page[before]={next}")).unwrap();
        params.size = 2;
        // A backward read arrives in reverse order and overflowed by one.
        let page = Page::from_rows(vec!["l", "k", "j"], &params, |row, _| (*row).to_string());
        assert_eq!(page.items, vec!["k", "l"]);
        assert!(page.next.is_some() && page.prev.is_some());
    }

    #[test]
    fn keyset_is_one_row_comparison_for_a_uniform_sort_and_expands_when_mixed() {
        let cols = |p: &ListParams| -> Vec<(&'static str, &'static str)> {
            let mut c: Vec<_> = p
                .sort
                .iter()
                .map(|(f, _)| (if f == "name" { "name" } else { "email" }, "text"))
                .collect();
            c.push(("id", "uuid"));
            c
        };
        let cursor = encode_cursor(
            &[("name".into(), true)],
            vec!["m".into(), uuid::Uuid::nil().to_string()],
        );
        let params = parse(&format!("sort=-name&page[after]={cursor}")).unwrap();
        assert_eq!(
            pg_keyset(&params, &cols(&params), 3).unwrap(),
            "(name, id) < ($3::text, $4::uuid)"
        );
        assert_eq!(pg_order(&params, &cols(&params)), "name DESC, id DESC");

        let cursor = encode_cursor(
            &[("name".into(), false), ("email".into(), true)],
            vec!["m".into(), "e".into(), uuid::Uuid::nil().to_string()],
        );
        let params = parse(&format!("sort=name,-email&page[before]={cursor}")).unwrap();
        assert_eq!(
            pg_keyset(&params, &cols(&params), 1).unwrap(),
            "((name < $1::text) OR (name = $1::text AND email > $2::text) \
             OR (name = $1::text AND email = $2::text AND id > $3::uuid))"
        );
        assert_eq!(
            pg_order(&params, &cols(&params)),
            "name DESC, email ASC, id ASC"
        );
        assert!(pg_keyset(&parse("").unwrap(), &cols(&parse("").unwrap()), 1).is_none());
    }

    #[test]
    fn like_patterns_match_the_callers_text_literally() {
        assert_eq!(like_contains("50%_a\\b"), "%50\\%\\_a\\\\b%");
    }
}
