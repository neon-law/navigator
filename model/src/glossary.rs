//! The authored glossary as reference data: the embedded `docs/glossary/`
//! directory and its parsed terms. `store::glossary` materializes these
//! into rows; the `navigator glossary` verbs read them with no connection.

use std::sync::LazyLock;

use include_dir::{include_dir, Dir};

use crate::workspace::NAVIGATOR_REPOSITORY_URL;

/// The authored glossary directory, embedded at compile time so a
/// deployed binary materializes the same vocabulary it was built from —
/// no runtime lookup of a file that might have drifted. One Markdown file
/// per term, `<slug>.md`, whose frontmatter carries the `title:`; the
/// directory's `README.md` is its preamble, not a term.
pub static GLOSSARY: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../docs/glossary");

/// The repository-relative glossary path used in diagnostics and links.
pub const GLOSSARY_LABEL: &str = "docs/glossary";

/// The directory's preamble file — prose about the glossary, not a term.
pub const README: &str = "README.md";

/// The glossary preamble: the reader-facing opening of [`README`] — below
/// its `# Glossary` heading, which every surface replaces with its own
/// title, and above its first `## ` section, which is for authors.
#[must_use]
pub fn preamble() -> &'static str {
    let raw = GLOSSARY
        .get_file(README)
        .and_then(|file| file.contents_utf8())
        .unwrap_or("");
    let body = raw.trim_start().strip_prefix("# Glossary").unwrap_or(raw);
    body.split("\n## ").next().unwrap_or(body).trim()
}

/// Every authored term, alphabetical by slug for stable reference output.
///
/// # Panics
///
/// Panics when an embedded entry is malformed — no `title:`, or a file
/// name that is not its title's slug. The unit tests hold every
/// committed entry to that shape, so a panic here means a binary was
/// built from a tree the gate would have refused.
#[must_use]
pub fn terms() -> &'static [Term] {
    static TERMS: LazyLock<Vec<Term>> = LazyLock::new(|| {
        let mut terms: Vec<Term> = GLOSSARY
            .files()
            .filter_map(|file| {
                let name = file.path().file_name()?.to_str()?;
                let stem = name.strip_suffix(".md").filter(|_| name != README)?;
                let raw = file.contents_utf8()?;
                Some(
                    parse_entry(stem, raw)
                        .unwrap_or_else(|error| panic!("{GLOSSARY_LABEL}/{name}: {error}")),
                )
            })
            .collect();
        terms.sort_by(|a, b| a.slug.cmp(&b.slug));
        terms
    });
    &TERMS
}

/// The term a reader names — by title (any case) or by slug.
#[must_use]
pub fn find(needle: &str) -> Option<&'static Term> {
    terms()
        .iter()
        .find(|term| term.title.eq_ignore_ascii_case(needle))
}

/// Why one glossary file could not be read as a term.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EntryError {
    /// The file carries no leading frontmatter block.
    #[error("no frontmatter; an entry opens with `---`, a `title:` line, and `---`")]
    NoFrontmatter,
    /// The frontmatter has no usable `title:`.
    #[error("frontmatter has no `title:`")]
    NoTitle,
    /// The frontmatter has no one-sentence plain-text `description:`.
    #[error("frontmatter has no one-sentence plain-text `description:`")]
    NoDescription,
    /// The description contains more than one sentence or a Markdown link.
    #[error("`description:` must be one plain sentence with no Markdown links")]
    InvalidDescription,
    /// The file name is not the slug of its title, so the anchor a link
    /// uses and the file a reader opens would disagree.
    #[error("file name `{stem}.md` must be `{slug}.md`, the slug of its title")]
    MisnamedFile { stem: String, slug: String },
}

#[derive(serde::Deserialize)]
struct EntryFrontmatter {
    title: Option<String>,
    description: Option<String>,
}

/// Parse one authored entry: `stem` is its file name without `.md`,
/// `raw` the whole file.
///
/// # Errors
///
/// [`EntryError`] when the frontmatter is missing, lacks a valid title or
/// description, or the file name is not the title's slug.
pub fn parse_entry(stem: &str, raw: &str) -> Result<Term, EntryError> {
    let (frontmatter, body) = rules::frontmatter::split(raw).ok_or(EntryError::NoFrontmatter)?;
    let fm =
        serde_yaml::from_str::<EntryFrontmatter>(frontmatter).map_err(|_| EntryError::NoTitle)?;
    let title = fm
        .title
        .map(|title| title.trim().to_string())
        .filter(|title| !title.is_empty())
        .ok_or(EntryError::NoTitle)?;
    let description = fm
        .description
        .map(|description| description.trim().to_string())
        .filter(|description| !description.is_empty())
        .ok_or(EntryError::NoDescription)?;
    if !valid_description(&description) {
        return Err(EntryError::InvalidDescription);
    }
    let slug = slugify(&title);
    if slug != stem {
        return Err(EntryError::MisnamedFile {
            stem: stem.to_string(),
            slug,
        });
    }
    Ok(Term {
        slug,
        title,
        description,
        body: body.trim().to_string(),
    })
}

/// Whether `description` is one plain sentence with no Markdown link.
#[must_use]
pub fn valid_description(description: &str) -> bool {
    if description.contains('\n') || description.contains("](") || !description.ends_with('.') {
        return false;
    }
    !description.char_indices().any(|(index, ch)| {
        matches!(ch, '.' | '!' | '?')
            && description[index + ch.len_utf8()..]
                .strip_prefix(' ')
                .and_then(|tail| tail.chars().next())
                .is_some_and(char::is_uppercase)
    })
}

/// Where one link in an entry body points, resolved from the entry's own
/// directory (`docs/glossary/`) so every surface that renders the body
/// agrees on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget<'a> {
    /// A sibling entry, `matter.md` → the term `matter`; on the one-page
    /// glossary that is the in-page anchor `#matter`.
    Term(String),
    /// A file or directory elsewhere in the repository, as a
    /// repository-relative path (`store/src/persons.rs`, `store/`), plus
    /// the anchor it carried.
    Repository {
        path: String,
        anchor: Option<&'a str>,
    },
    /// An absolute URL, a site path, an in-page anchor, or a relative path
    /// that climbs out of the repository — passed through verbatim.
    Verbatim,
}

/// Resolve a link destination written in an entry body.
#[must_use]
pub fn link_target(dest: &str) -> LinkTarget<'_> {
    if dest.contains("://") || dest.starts_with("mailto:") || dest.starts_with('/') {
        return LinkTarget::Verbatim;
    }
    let (path, anchor) = dest
        .split_once('#')
        .map_or((dest, None), |(p, a)| (p, Some(a)));
    if path.is_empty() {
        return LinkTarget::Verbatim;
    }
    if let Some(stem) = path.strip_suffix(".md").filter(|s| !s.contains('/')) {
        if stem != "README" {
            return LinkTarget::Term(stem.to_string());
        }
    }
    let mut parts: Vec<&str> = GLOSSARY_LABEL.split('/').collect();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return LinkTarget::Verbatim;
                }
            }
            other => parts.push(other),
        }
    }
    let mut resolved = parts.join("/");
    if path.ends_with('/') {
        resolved.push('/');
    }
    LinkTarget::Repository {
        path: resolved,
        anchor,
    }
}

/// A GitHub URL for `repo_path` at this build's release tag.
///
/// The tag is `[workspace.package].version`. A glossary citation names the
/// code this binary was built from. A directory path (trailing `/`) is a
/// tree URL; anything else is a blob. `anchor` is appended when present.
#[must_use]
pub fn github_source_url(repo_path: &str, anchor: Option<&str>) -> String {
    let kind = if repo_path.ends_with('/') {
        "tree"
    } else {
        "blob"
    };
    let url = format!(
        "{repo}/{kind}/{version}/{path}",
        repo = NAVIGATOR_REPOSITORY_URL,
        version = env!("CARGO_PKG_VERSION"),
        path = repo_path.trim_end_matches('/')
    );
    match anchor {
        Some(anchor) => format!("{url}#{anchor}"),
        None => url,
    }
}

/// One parsed glossary term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    /// The published anchor (`lawyer-review`) — the stable reference key.
    pub slug: String,
    /// The heading text (`Lawyer Review`).
    pub title: String,
    /// The one-sentence plain-text summary shown by the CLI and glossary page.
    pub description: String,
    /// The Markdown body beneath the heading.
    pub body: String,
}

/// The published-docs anchor for a heading (`Lawyer Review` →
/// `lawyer-review`).
#[must_use]
pub fn slugify(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if c == ' ' {
            out.push('-');
        } else if c == '-' || c == '_' {
            out.push(c);
        }
    }
    out
}
