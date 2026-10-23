//! Shared inline-link parsing for the relative-link rules
//! ([`crate::M057RelativeLinkResolves`] and
//! [`crate::M061WebPortableLink`]).
//!
//! Both rules read the destination of inline links (`[text](target)`)
//! and reason about whether the target is a repo-relative path. Image
//! embeds (`![alt](src)`) are deliberately skipped: image sources route
//! through the asset seam (`views::assets::asset_url`), which resolves
//! against the object store rather than the repo tree.

/// The on-disk file portion of a *relative* link target, or `None` when
/// the target is not a repo-relative path — an absolute URL, `mailto:`,
/// a site-absolute `/…`, a bare `#anchor`, a `{{template}}` placeholder,
/// or empty. The returned slice has any `#fragment`/`?query` stripped.
#[must_use]
pub(crate) fn relative_file_part(target: &str) -> Option<&str> {
    let t = target.trim();
    if t.is_empty()
        || t.starts_with('#')
        || t.starts_with('/')
        || t.contains("{{")
        || t.contains("://")
        || t.starts_with("mailto:")
        || t.starts_with("tel:")
    {
        return None;
    }
    let file_part = t.split(['#', '?']).next().unwrap_or("");
    if file_part.is_empty() {
        return None;
    }
    Some(file_part)
}

/// A relative link target split into its file part and its `#fragment`,
/// or `None` when the target is not repo-relative (see
/// [`relative_file_part`]). A same-file `#anchor` yields an empty file
/// part; a target with no fragment yields `None` for it.
#[must_use]
pub(crate) fn split_relative(target: &str) -> Option<(&str, Option<&str>)> {
    let t = target.trim();
    if let Some(fragment) = t.strip_prefix('#') {
        return (!fragment.is_empty()).then_some(("", Some(fragment)));
    }
    let file_part = relative_file_part(t)?;
    let fragment = t
        .split_once('#')
        .map(|(_, f)| f.split('?').next().unwrap_or(""))
        .filter(|f| !f.is_empty());
    Some((file_part, fragment))
}

/// The target of a `CommonMark` reference-style definition (`[label]: target`),
/// or `None` when the line is not one. Footnote definitions (`[^1]: …`)
/// are not links and are skipped.
///
/// A destination is a bare unspaced run or `<…>`. Anything after it that
/// is not a complete `"…"`, `'…'`, or `(…)` title makes the line prose —
/// `[text]: this is prose` is not a definition, same as S102's `CommonMark` reader.
#[must_use]
pub(crate) fn reference_definition(line: &str) -> Option<String> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let rest = line[indent..].strip_prefix('[')?;
    if indent > 3 || rest.starts_with('^') {
        return None;
    }
    let (label, after) = rest.split_once("]:")?;
    if label.trim().is_empty() {
        return None;
    }
    let after = after.trim_start();
    let (target, remainder) = if let Some(inner) = after.strip_prefix('<') {
        let end = inner.find('>')?;
        (&inner[..end], inner[end + 1..].trim_start())
    } else {
        let dest = after.split_whitespace().next()?;
        (dest, after[dest.len()..].trim_start())
    };
    if !remainder.is_empty() && !is_complete_title(remainder) {
        return None;
    }
    Some(target.to_string())
}

/// A reference definition's optional title: `"…"`, `'…'`, or `(…)` with
/// nothing after the closer.
fn is_complete_title(title: &str) -> bool {
    let title = title.trim_end();
    let Some(opener) = title.chars().next() else {
        return false;
    };
    let closer = match opener {
        '"' => '"',
        '\'' => '\'',
        '(' => ')',
        _ => return false,
    };
    let body = &title[opener.len_utf8()..];
    let mut escaped = false;
    for (index, character) in body.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character == closer {
            return body[index + character.len_utf8()..].trim().is_empty();
        }
    }
    false
}

/// Every anchor a markdown document exposes: GitHub-style heading slugs
/// (with `-1`, `-2` suffixes for repeats) and explicit `<a id>`/`<a name>`
/// targets. Headings inside frontmatter or fenced code do not count.
#[must_use]
pub(crate) fn document_anchors(contents: &str) -> std::collections::BTreeSet<String> {
    let mut anchors = std::collections::BTreeSet::new();
    let mut seen = std::collections::BTreeMap::<String, usize>::new();
    for (_, line) in crate::frontmatter::body_lines(contents) {
        if let Some(heading) = atx_heading_text(line) {
            let slug = heading_slug(heading);
            let count = seen.entry(slug.clone()).or_insert(0);
            anchors.insert(if *count == 0 {
                slug
            } else {
                format!("{slug}-{count}")
            });
            *count += 1;
        }
        for attr in [r#"id=""#, r#"name=""#] {
            let mut rest = line;
            while let Some(start) = rest.find(attr) {
                rest = &rest[start + attr.len()..];
                if let Some(end) = rest.find('"') {
                    anchors.insert(rest[..end].to_string());
                    rest = &rest[end..];
                }
            }
        }
    }
    anchors
}

/// The text of an ATX heading (`## Title ##`), without markers.
fn atx_heading_text(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let hashes = trimmed.len() - trimmed.trim_start_matches('#').len();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &trimmed[hashes..];
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    Some(rest.trim().trim_end_matches('#').trim_end())
}

/// GitHub's heading slug: link markup reduced to its text, lowercased,
/// punctuation other than `-` and `_` dropped, each space a `-`.
#[must_use]
pub(crate) fn heading_slug(heading: &str) -> String {
    let mut text = String::with_capacity(heading.len());
    let mut chars = heading.chars().peekable();
    while let Some(c) = chars.next() {
        // `[label](url)` contributes only `label`.
        if c == ']' && chars.peek() == Some(&'(') {
            let mut depth = 0usize;
            for inner in chars.by_ref() {
                match inner {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            continue;
        }
        text.push(c);
    }
    text.to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

/// Inline-link targets (`[text](target)`) on a single line. Image
/// embeds (`![alt](src)`) are skipped. A trailing `"title"` inside the
/// parentheses is dropped so only the destination is returned.
#[must_use]
pub(crate) fn link_targets(line: &str) -> Vec<String> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b']' && bytes[i + 1] == b'(' {
            // Find the matching `)` of this `](...)`, honoring nesting.
            let mut depth = 1usize;
            let mut j = i + 2;
            while j < bytes.len() && depth > 0 {
                match bytes[j] {
                    b'(' => depth += 1,
                    b')' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            let inner_end = if depth == 0 { j - 1 } else { j };
            let inner = &line[i + 2..inner_end];
            if !is_image_link(bytes, i) {
                out.push(strip_title(inner).to_string());
            }
            i = j;
            continue;
        }
        i += 1;
    }
    out
}

/// Whether the `]` at `bracket` closes an image link — i.e. its matching
/// opening `[` is immediately preceded by `!`.
fn is_image_link(bytes: &[u8], bracket: usize) -> bool {
    let mut depth = 1usize;
    let mut k = bracket;
    while k > 0 {
        k -= 1;
        match bytes[k] {
            b']' => depth += 1,
            b'[' => {
                depth -= 1;
                if depth == 0 {
                    return k > 0 && bytes[k - 1] == b'!';
                }
            }
            _ => {}
        }
    }
    false
}

/// Drop a trailing `"title"` (or `'title'`) from a link destination,
/// returning just the URL/path portion.
fn strip_title(inner: &str) -> &str {
    match inner.find(['"', '\'']) {
        Some(idx) => inner[..idx].trim(),
        None => inner.trim(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        document_anchors, heading_slug, link_targets, reference_definition, relative_file_part,
        split_relative,
    };

    #[test]
    fn heading_slug_matches_github() {
        assert_eq!(
            heading_slug("Restate handler authoring"),
            "restate-handler-authoring"
        );
        assert_eq!(
            heading_slug("One workflow owns publishing — `deploy.yml`"),
            "one-workflow-owns-publishing--deployyml"
        );
        assert_eq!(
            heading_slug("The branch → PR → auto-merge flow"),
            "the-branch--pr--auto-merge-flow"
        );
        assert_eq!(heading_slug("See [the docs](x.md) now"), "see-the-docs-now");
        assert_eq!(heading_slug("snake_case and 42"), "snake_case-and-42");
    }

    #[test]
    fn document_anchors_numbers_repeats_and_reads_html_ids() {
        let doc = "# Title\n## Notes\n## Notes\n```\n## Fenced\n```\n<a id=\"custom\"></a>\n";
        let anchors = document_anchors(doc);
        for expected in ["title", "notes", "notes-1", "custom"] {
            assert!(
                anchors.contains(expected),
                "{expected} missing from {anchors:?}"
            );
        }
        assert!(
            !anchors.contains("fenced"),
            "fenced headings are not anchors"
        );
    }

    #[test]
    fn split_relative_separates_file_and_fragment() {
        assert_eq!(split_relative("g.md#frag"), Some(("g.md", Some("frag"))));
        assert_eq!(split_relative("g.md"), Some(("g.md", None)));
        assert_eq!(split_relative("#local"), Some(("", Some("local"))));
        assert_eq!(split_relative("#"), None);
        assert_eq!(split_relative("https://x.com/a#b"), None);
    }

    #[test]
    fn reference_definition_reads_targets_and_skips_footnotes() {
        assert_eq!(
            reference_definition("[a]: ../x/SKILL.md#start \"Title\""),
            Some("../x/SKILL.md#start".to_string())
        );
        assert_eq!(
            reference_definition("[b]: <spaced.md>"),
            Some("spaced.md".to_string())
        );
        assert_eq!(reference_definition("[^1]: a footnote"), None);
        assert_eq!(reference_definition("    [c]: indented-code.md"), None);
        assert_eq!(reference_definition("plain [text](x.md)"), None);
        assert_eq!(
            reference_definition("[text]: this is prose"),
            None,
            "leftover words that are not a title make the line prose"
        );
    }

    #[test]
    fn link_targets_reads_inline_links_and_skips_images() {
        assert_eq!(
            link_targets("see [a](one.md) and ![img](pic.png) then [b](two.md)"),
            vec!["one.md".to_string(), "two.md".to_string()]
        );
    }

    #[test]
    fn link_targets_drops_a_title_suffix() {
        assert_eq!(
            link_targets(r#"[x](path.md "A title")"#),
            vec!["path.md".to_string()]
        );
    }

    #[test]
    fn relative_file_part_strips_anchor_and_query() {
        assert_eq!(relative_file_part("g.md#frag"), Some("g.md"));
        assert_eq!(relative_file_part("g.md?v=1"), Some("g.md"));
    }

    #[test]
    fn relative_file_part_rejects_non_relative_targets() {
        for t in [
            "https://x.com",
            "mailto:x@mail.example",
            "/docs/glossary",
            "#anchor",
            "{{url}}",
            "",
        ] {
            assert_eq!(relative_file_part(t), None, "{t} should be skipped");
        }
    }
}
