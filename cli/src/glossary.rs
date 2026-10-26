//! `navigator glossary ...` — the ontology from the command line.
//!
//! The glossary is one Markdown file per term under `docs/glossary/`,
//! embedded in the binary by [`store::glossary::GLOSSARY`]. The website
//! publishes the same terms on one page at `/glossary`, so the CLI and the
//! page read one source and cannot drift.

use std::io::IsTerminal;
use std::process::ExitCode;

use store::glossary::{link_target, terms, LinkTarget, Term, GLOSSARY_LABEL};

use crate::palette;

/// `navigator glossary list`: one reader-facing title and summary per term.
#[must_use]
pub fn list() -> ExitCode {
    let mut entries: Vec<_> = terms().iter().collect();
    entries.sort_by(|a, b| a.title.cmp(&b.title));
    if std::io::stdout().is_terminal() {
        let title_width = entries
            .iter()
            .map(|term| term.title.chars().count())
            .max()
            .unwrap_or_default();
        for term in entries {
            let padding = " ".repeat(title_width.saturating_sub(term.title.chars().count()));
            println!(
                "{}{}  {}",
                palette::header(&term.title),
                padding,
                term.description
            );
        }
    } else {
        for term in entries {
            println!("{} — {}", term.title, term.description);
        }
    }
    ExitCode::SUCCESS
}

/// `navigator glossary show <term>`: the one term its title names.
#[must_use]
pub fn show(needle: &str) -> ExitCode {
    if let Some(term) = store::glossary::find(needle) {
        print_term(term);
        ExitCode::SUCCESS
    } else {
        // The term is not echoed: CodeQL reads every value parsed out of the
        // CLI's command enum as a secret (one variant is `Secrets`), and the
        // reader already has the term they typed on screen.
        eprintln!("navigator: glossary show: unknown term");
        eprintln!(
            "Run `navigator glossary list` to list every term, then \
                  `navigator glossary show \"<Term>\"` to read its definition."
        );
        ExitCode::from(1)
    }
}

fn print_term(term: &Term) {
    println!("## {}", palette::header(&term.title));
    println!();
    println!("{}", term.body.trim());
    println!();
}

/// `navigator glossary notion`: the glossary as one Markdown page a Notion
/// page can hold.
///
/// Notion has no notion of this repository's working directory, so the
/// link shapes an entry uses are resolved before the text leaves the
/// tree: a repository path (`../../store/src/persons.rs`) becomes a
/// `blob`/`tree` URL at the release tag, and a sibling term (`matter.md`) is
/// unlinked down to its label, because Notion has no heading anchors to
/// point it at.
#[must_use]
pub fn notion() -> ExitCode {
    print!("{}", notion_markdown(store::glossary::preamble(), terms()));
    ExitCode::SUCCESS
}

/// The provenance blockquote every push writes as the page's first
/// block, so a reader who arrives at the Notion copy first learns which
/// way the sync runs before they start typing into it.
fn notion_preface() -> String {
    format!(
        "> Generated from [`{GLOSSARY_LABEL}/`]({url}) by \
         `navigator glossary notion`. The repository is the source of truth: an edit made \
         here reaches the product only once it lands in that directory through a pull request. \
         Links between terms are dropped because Notion cannot resolve them.\n\n",
        url = store::glossary::github_source_url(&format!("{GLOSSARY_LABEL}/"), None)
    )
}

fn notion_markdown(preamble: &str, terms: &[Term]) -> String {
    let mut page = String::new();
    page.push_str(preamble.trim());
    page.push_str("\n\n");
    for term in terms {
        page.push_str("## ");
        page.push_str(&term.title);
        page.push_str("\n\n");
        page.push_str(term.description.trim());
        page.push_str("\n\n");
        page.push_str(term.body.trim());
        page.push_str("\n\n");
    }
    let body = unwrap_paragraphs(&rewrite_links(page.trim_end()));
    format!("{preface}{body}", preface = notion_preface())
}

/// Undo the repository's 120-character hard wrap.
///
/// The wrap exists so a definition reviews cleanly in a diff; Notion
/// lays its own text out and renders every one of those newlines as a
/// break, which turns a paragraph into a ladder. Joining here is also
/// what makes the push idempotent: the Notion page then holds exactly
/// this command's output, so a later push can be compared against it
/// instead of guessed at.
///
/// Fenced code blocks, tables, and thematic breaks are copied verbatim —
/// their line structure is the content.
fn unwrap_paragraphs(markdown: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut fenced = false;
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            out.push(line.to_string());
            continue;
        }
        if fenced {
            out.push(line.to_string());
            continue;
        }
        match out.last() {
            Some(previous) if continues(previous, line) => {
                let joined = format!(
                    "{previous} {next}",
                    previous = previous.trim_end(),
                    next = line.trim_start().trim_start_matches("> ")
                );
                let last = out.len() - 1;
                out[last] = joined;
            }
            _ => out.push(line.to_string()),
        }
    }
    let mut joined = out.join("\n");
    joined.push('\n');
    joined
}

/// Whether `line` is a soft-wrapped continuation of `previous`.
fn continues(previous: &str, line: &str) -> bool {
    if previous.trim().is_empty() || line.trim().is_empty() {
        return false;
    }
    // A block whose shape is its line structure never absorbs the line
    // beneath it.
    if previous.starts_with('#') || previous.starts_with('|') || previous.starts_with("---") {
        return false;
    }
    let opener = line.trim_start();
    if opener.starts_with('#')
        || opener.starts_with('|')
        || opener.starts_with("---")
        || opener.starts_with("- ")
        || opener.starts_with("* ")
        || opener
            .split_once(". ")
            .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    {
        return false;
    }
    // A blockquote continues only another blockquote, and vice versa.
    previous.starts_with("> ") == opener.starts_with("> ")
}

/// Rewrite every `[label](destination)` in the document.
///
/// Whole-document rather than line-by-line: the source is hard-wrapped
/// at 120 characters, so a link label routinely straddles a newline and
/// a per-line scan would strip the half that carries the destination.
///
/// Destinations in the glossary hold no parentheses, so the first `)`
/// after a `](` closes the link; a destination that ever grows one would
/// be left verbatim rather than mangled.
fn rewrite_links(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("](") {
        let Some(close) = rest[at + 2..].find(')') else {
            break;
        };
        let dest = &rest[at + 2..at + 2 + close];
        let head = &rest[..at];
        rest = &rest[at + 2 + close + 1..];
        match notion_link(dest) {
            Some(resolved) => {
                out.push_str(head);
                out.push_str("](");
                out.push_str(&resolved);
                out.push(')');
            }
            // A sibling term: drop the brackets, keep the label.
            None => match head.rfind('[') {
                Some(open) => {
                    out.push_str(&head[..open]);
                    out.push_str(&head[open + 1..]);
                }
                None => out.push_str(head),
            },
        }
    }
    out.push_str(rest);
    out
}

/// Resolve one link destination, or `None` for a link between terms.
fn notion_link(dest: &str) -> Option<String> {
    match link_target(dest) {
        LinkTarget::Term(_) => None,
        LinkTarget::Repository { path, anchor } => {
            Some(store::glossary::github_source_url(&path, anchor))
        }
        LinkTarget::Verbatim => Some(dest.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use store::glossary::{preamble, terms};

    #[test]
    fn notion_markdown_resolves_every_link_shape() {
        let rendered = super::notion_markdown(preamble(), terms());
        assert!(
            rendered.starts_with("> Generated from [`docs/glossary/`]"),
            "the page must open with its provenance"
        );
        assert!(
            rendered.contains(
                "A postal address attached to a Person, to an Entity, or to neither (the \
                 mailroom placeholder). The person/entity exclusivity is enforced by the \
                 application, not the schema."
            ),
            "a hard-wrapped paragraph must reach Notion as one line"
        );
        assert!(
            !rules::frontmatter::body_lines(&rendered)
                .iter()
                .any(|(_, line)| line.starts_with("title: ")),
            "entry frontmatter stays home"
        );
        for (at, _) in rendered.match_indices("](") {
            let dest = &rendered[at + 2..];
            assert!(
                dest.starts_with("http://")
                    || dest.starts_with("https://")
                    || dest.starts_with("mailto:"),
                "a relative link cannot resolve from Notion: {}",
                &dest[..dest.len().min(60)]
            );
        }
        let version = env!("CARGO_PKG_VERSION");
        let repo = cloud::workspace::NAVIGATOR_REPOSITORY_URL;
        assert!(rendered.contains(&format!("({repo}/blob/{version}/store/src/persons.rs)")));
        assert!(rendered.contains(&format!("({repo}/tree/{version}/store)")));
        assert!(
            !rendered.contains("/blob/main/") && !rendered.contains("/tree/main/"),
            "notion source links name the release tag"
        );
        assert!(rendered.contains("\n## Workshop\n"));
    }

    #[test]
    fn notion_markdown_preserves_title_fields_inside_fenced_yaml() {
        let example =
            "```yaml\ntitle: Synthetic argument\npoint: |\n  First line.\n  Second line.\n```";
        let term = store::glossary::Term {
            slug: "synthetic-example".into(),
            title: "Synthetic Example".into(),
            description: "A synthetic export example.".into(),
            body: example.into(),
        };
        let rendered = super::notion_markdown("Glossary", &[term]);
        assert!(rendered.contains("## Synthetic Example\n\nA synthetic export example."));
        assert!(
            rendered.contains(example),
            "fenced YAML retains its fields and line breaks"
        );
    }

    /// `rust/cleartext-logging` flags any logging sink fed a value parsed
    /// out of the command enum, so the not-found diagnostic stays generic.
    #[test]
    fn the_unknown_term_diagnostic_does_not_echo_the_argument() {
        let source = include_str!("glossary.rs");
        let needle = ["{", "needle", "}"].concat();
        assert!(
            !source
                .lines()
                .any(|line| line.contains("eprintln!") && line.contains(&needle)),
            "an eprintln! interpolates the parsed term"
        );
    }

    #[test]
    fn a_sibling_term_link_keeps_its_label() {
        assert_eq!(
            super::rewrite_links("See [Asset](asset.md) and [Address](address.md)."),
            "See Asset and Address."
        );
    }

    #[test]
    fn a_repository_link_resolves_to_github() {
        let expected = format!(
            "{}/blob/{}/docs/notation.md#template",
            cloud::workspace::NAVIGATOR_REPOSITORY_URL,
            env!("CARGO_PKG_VERSION")
        );
        assert_eq!(
            super::notion_link("../notation.md#template").as_deref(),
            Some(expected.as_str())
        );
    }
}
