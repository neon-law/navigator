//! Search text extracted from the local Project repository, reporting both hits and coverage.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use regex::{Regex, RegexBuilder};
use serde::Serialize;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Record {
    Court,
    Correspondence,
    ClientStatement,
    WorkProduct,
    Other,
}

impl Record {
    fn as_str(self) -> &'static str {
        match self {
            Self::Court => "court record",
            Self::Correspondence => "correspondence",
            Self::ClientStatement => "client statement",
            Self::WorkProduct => "work product",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Serialize)]
struct Hit {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
    record_class: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    from: Option<String>,
    #[serde(rename = "match")]
    matched: String,
    context: Vec<String>,
}

#[derive(Debug, Serialize)]
struct Missed {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<usize>,
    kind: String,
    reason: String,
}

#[derive(Debug, Default, Serialize)]
struct Coverage {
    searched_by_kind: BTreeMap<String, usize>,
    not_searched: Vec<Missed>,
}

#[derive(Debug, Serialize)]
struct Report {
    hits: Vec<Hit>,
    coverage: Coverage,
}

pub(crate) async fn run(
    pattern: &str,
    dir: &Path,
    ignore_case: bool,
    context: usize,
    json: bool,
    court_only: bool,
) -> ExitCode {
    match search(pattern, dir, ignore_case, context, court_only).await {
        Ok(report) => {
            if json {
                match serde_json::to_string_pretty(&report) {
                    Ok(rendered) => println!("{rendered}"),
                    Err(error) => {
                        eprintln!("navigator: could not render search report: {error}");
                        return ExitCode::from(2);
                    }
                }
            } else {
                for hit in &report.hits {
                    let location = hit.page.map_or_else(
                        || format!("line {}", hit.line.unwrap_or_default()),
                        |page| format!("page {page}"),
                    );
                    println!("{}:{location}: {}", hit.path, hit.matched);
                }
                println!("searched: {:?}", report.coverage.searched_by_kind);
                for missed in &report.coverage.not_searched {
                    println!("not searched: {} ({})", missed.path, missed.reason);
                }
            }
            if !report.coverage.not_searched.is_empty() {
                ExitCode::from(2)
            } else if report.hits.is_empty() {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(error) => {
            eprintln!("navigator: project search: {error:#}");
            ExitCode::from(2)
        }
    }
}

async fn search(
    pattern: &str,
    root: &Path,
    ignore_case: bool,
    context: usize,
    court_only: bool,
) -> Result<Report> {
    let matcher = RegexBuilder::new(pattern)
        .case_insensitive(ignore_case)
        .build()
        .context("invalid regular expression")?;
    let mut report = Report {
        hits: Vec::new(),
        coverage: Coverage::default(),
    };
    let metadata = fs::metadata(root)
        .with_context(|| format!("Project root {} is unavailable", root.display()))?;
    anyhow::ensure!(
        metadata.is_dir(),
        "Project root {} is not a directory",
        root.display()
    );
    let mut cache = BTreeMap::new();
    for corpus in ["documents", "notes"] {
        let corpus_root = root.join(corpus);
        if !corpus_root.exists() {
            continue;
        }
        let walker = WalkDir::new(&corpus_root).sort_by_file_name();
        for item in walker {
            let entry = match item {
                Ok(entry) => entry,
                Err(error) => {
                    if let Some(path) = error.path() {
                        report.coverage.not_searched.push(Missed {
                            path: path
                                .strip_prefix(root)
                                .unwrap_or(path)
                                .display()
                                .to_string(),
                            page: None,
                            kind: extension(path),
                            reason: format!("walk error: {error}"),
                        });
                    }
                    continue;
                }
            };
            if entry.file_type().is_file() {
                search_file(
                    entry.path(),
                    root,
                    &matcher,
                    context,
                    court_only,
                    &mut cache,
                    &mut report,
                )
                .await?;
            }
        }
    }
    Ok(report)
}

async fn search_file(
    path: &Path,
    root: &Path,
    matcher: &regex::Regex,
    context: usize,
    court_only: bool,
    cache: &mut BTreeMap<(String, [u8; 32]), Extracted>,
    report: &mut Report,
) -> Result<()> {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let record = classify(relative);
    if court_only && record != Record::Court {
        return Ok(());
    }
    let kind = extension(path);
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            report.coverage.not_searched.push(Missed {
                path: relative.display().to_string(),
                page: None,
                kind,
                reason: format!("read error: {error}"),
            });
            return Ok(());
        }
    };
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    let cache_key = (kind.clone(), digest);
    if !cache.contains_key(&cache_key) {
        cache.insert(cache_key.clone(), extract(path, &bytes).await);
    }
    let Some(extracted) = cache.get(&cache_key) else {
        return Ok(());
    };
    let Some(parts) = searchable_parts(extracted, relative, &kind, report) else {
        return Ok(());
    };
    *report
        .coverage
        .searched_by_kind
        .entry(kind.clone())
        .or_default() += 1;
    let (message_date, from) = if kind == "eml" {
        email_metadata(&bytes)
    } else {
        (None, None)
    };
    let parts = parts.into_iter().map(|mut part| {
        if part.message_date.is_none() {
            part.message_date.clone_from(&message_date);
        }
        if part.from.is_none() {
            part.from.clone_from(&from);
        }
        part
    });
    append_hits(report, relative, record, &kind, matcher, context, parts);
    Ok(())
}

fn searchable_parts(
    extracted: &Extracted,
    path: &Path,
    kind: &str,
    report: &mut Report,
) -> Option<Vec<Part>> {
    match extracted {
        Extracted::Pages(pages) => {
            let mut parts = Vec::new();
            for (index, text) in pages.iter().enumerate() {
                if text.trim().is_empty() {
                    report.coverage.not_searched.push(Missed {
                        path: path.display().to_string(),
                        page: Some(index + 1),
                        kind: kind.to_owned(),
                        reason: "page has no searchable text layer; OCR was not run".into(),
                    });
                } else {
                    parts.push(Part {
                        text: text.clone(),
                        message_date: None,
                        from: None,
                        page: Some(index + 1),
                    });
                }
            }
            (!parts.is_empty()).then_some(parts)
        }
        Extracted::Text(text) => Some(vec![Part {
            text: text.clone(),
            message_date: None,
            from: None,
            page: None,
        }]),
        Extracted::Parts(parts) => Some(parts.clone()),
        Extracted::Error(reason) => {
            report.coverage.not_searched.push(Missed {
                path: path.display().to_string(),
                page: None,
                kind: kind.to_owned(),
                reason: reason.clone(),
            });
            None
        }
    }
}

fn append_hits<'a>(
    report: &mut Report,
    path: &Path,
    record: Record,
    kind: &str,
    matcher: &Regex,
    context: usize,
    parts: impl Iterator<Item = Part> + 'a,
) {
    for part in parts {
        for (page_index, page_text) in part.text.split('\u{c}').enumerate() {
            let lines: Vec<_> = page_text.lines().collect();
            for found in matcher.find_iter(page_text) {
                let line_offset = page_text[..found.start()].matches('\n').count();
                let start = line_offset.saturating_sub(context);
                let end = line_offset
                    .saturating_add(context)
                    .saturating_add(1)
                    .min(lines.len());
                report.hits.push(Hit {
                    path: path.display().to_string(),
                    page: (kind == "pdf").then(|| part.page.unwrap_or(page_index + 1)),
                    line: (kind != "pdf").then_some(line_offset + 1),
                    record_class: record.as_str(),
                    message_date: part.message_date.clone(),
                    from: part.from.clone(),
                    matched: found.as_str().to_string(),
                    context: lines[start..end]
                        .iter()
                        .map(|line| (*line).to_string())
                        .collect(),
                });
            }
        }
    }
}

#[derive(Debug)]
enum Extracted {
    Text(String),
    Pages(Vec<String>),
    Parts(Vec<Part>),
    Error(String),
}

#[derive(Debug, Clone)]
struct Part {
    text: String,
    message_date: Option<String>,
    from: Option<String>,
    page: Option<usize>,
}

async fn extract(path: &Path, bytes: &[u8]) -> Extracted {
    match extension(path).as_str() {
        "pdf" => {
            // The parser can panic on malformed numeric objects. Keep that failure local
            // to this file and cache it like an ordinary extraction error.
            match std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem_by_pages(bytes)) {
                Ok(result) => result.map_or_else(
                    |error| Extracted::Error(format!("PDF decode error: {error}")),
                    Extracted::Pages,
                ),
                Err(_) => Extracted::Error("PDF extraction panicked; file was not searched".into()),
            }
        }
        "eml" => match mail_parser::MessageParser::default().parse(bytes) {
            Some(message) => {
                let bodies = (0..message.text_body().len())
                    .filter_map(|index| message.body_text(index))
                    .map(std::borrow::Cow::into_owned)
                    .collect::<Vec<_>>();
                if bodies.is_empty() {
                    Extracted::Error("email has no extractable text body".into())
                } else {
                    Extracted::Text(bodies.join("\n\n[message part]\n\n"))
                }
            }
            None => Extracted::Error("email decode error".into()),
        },
        "docx" => match word::parse(path.to_string_lossy().as_ref(), bytes).await {
            Ok(document) => Extracted::Text(document.accepted_view_text()),
            Err(error) => Extracted::Error(format!("DOCX decode error: {error}")),
        },
        "json" => match serde_json::from_slice::<serde_json::Value>(bytes) {
            Ok(value) => json_parts(&value),
            Err(error) => Extracted::Error(format!("JSON decode error: {error}")),
        },
        "txt" | "md" | "markdown" | "yaml" | "yml" | "csv" | "html" | "xml" => {
            String::from_utf8(bytes.to_vec()).map_or_else(
                |error| Extracted::Error(format!("UTF-8 decode error: {error}")),
                Extracted::Text,
            )
        }
        other => Extracted::Error(format!("unsupported file type: .{other}")),
    }
}

fn json_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(values) => {
            values.iter().map(json_text).collect::<Vec<_>>().join("\n")
        }
        serde_json::Value::Object(values) => values
            .values()
            .map(json_text)
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

fn json_parts(value: &serde_json::Value) -> Extracted {
    let messages = value.as_array().or_else(|| {
        value
            .get("messages")
            .or_else(|| value.get("emails"))
            .or_else(|| value.get("mail"))
            .and_then(serde_json::Value::as_array)
    });
    if let Some(messages) = messages {
        if messages.iter().all(serde_json::Value::is_object) {
            return Extracted::Parts(
                messages
                    .iter()
                    .map(|message| {
                        let (message_date, from) = json_metadata_value(message);
                        Part {
                            text: json_text(message),
                            message_date,
                            from,
                            page: None,
                        }
                    })
                    .collect(),
            );
        }
        return Extracted::Error(
            "JSON mail export has a non-object message; provenance is ambiguous".into(),
        );
    }
    let (message_date, from) = json_metadata_value(value);
    Extracted::Parts(vec![Part {
        text: json_text(value),
        message_date,
        from,
        page: None,
    }])
}

fn email_metadata(bytes: &[u8]) -> (Option<String>, Option<String>) {
    let Some(message) = mail_parser::MessageParser::default().parse(bytes) else {
        return (None, None);
    };
    let date = message
        .date()
        .map(|date| format!("{:04}-{:02}-{:02}", date.year, date.month, date.day));
    let from = message
        .from()
        .and_then(|address| address.first())
        .and_then(|address| address.address())
        .map(str::to_owned);
    (date, from)
}

fn json_metadata_value(value: &serde_json::Value) -> (Option<String>, Option<String>) {
    fn find<'a>(value: &'a serde_json::Value, names: &[&str]) -> Option<&'a str> {
        match value {
            serde_json::Value::Object(values) => values.iter().find_map(|(key, value)| {
                if names.iter().any(|name| key.eq_ignore_ascii_case(name)) {
                    value.as_str()
                } else {
                    find(value, names)
                }
            }),
            serde_json::Value::Array(values) => values.iter().find_map(|value| find(value, names)),
            _ => None,
        }
    }
    fn direct(value: &serde_json::Value, names: &[&str]) -> Option<String> {
        value.as_object()?.iter().find_map(|(key, value)| {
            names
                .iter()
                .any(|name| key.eq_ignore_ascii_case(name))
                .then(|| value.as_str().map(str::to_owned))
                .flatten()
        })
    }
    let date = direct(value, &["date", "sent_at", "timestamp"])
        .or_else(|| find(value, &["date", "sent_at", "timestamp"]).map(str::to_owned));
    let from = direct(value, &["from", "sender", "from_address"])
        .or_else(|| find(value, &["from", "sender", "from_address"]).map(str::to_owned));
    (date, from)
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|value| value.to_str())
        .unwrap_or("[none]")
        .to_ascii_lowercase()
}

fn classify(path: &Path) -> Record {
    let components: Vec<_> = path
        .components()
        .filter_map(|part| part.as_os_str().to_str())
        .collect();
    if components
        .iter()
        .any(|part| matches!(*part, "filings" | "rulings" | "dockets"))
    {
        Record::Court
    } else if components.contains(&"correspondence") {
        Record::Correspondence
    } else if components.contains(&"transcripts") {
        Record::ClientStatement
    } else if components
        .iter()
        .any(|part| matches!(*part, "memos" | "drafts" | "notes"))
    {
        Record::WorkProduct
    } else {
        Record::Other
    }
}

#[cfg(test)]
mod tests {
    use super::{classify, search, Record};
    use std::fs;

    fn two_page_pdf() -> Vec<u8> {
        use lopdf::{dictionary, Document, Object, Stream};
        let mut document = Document::with_version("1.5");
        let page_tree_id = document.new_object_id();
        let font_id = document.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica"
        });
        let mut page_ids = Vec::new();
        for text in ["first page", "second page marker", ""] {
            let content_id = document.add_object(Stream::new(
                dictionary! {},
                format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET").into_bytes(),
            ));
            let resources = dictionary! { "Font" => dictionary! { "F1" => font_id } };
            let leaf_id = document.add_object(dictionary! {
                "Type" => "Page", "Parent" => page_tree_id,
                "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
                "Resources" => resources, "Contents" => content_id
            });
            page_ids.push(leaf_id);
        }
        document.objects.insert(page_tree_id, Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => page_ids.iter().copied().map(Object::Reference).collect::<Vec<_>>(),
            "Count" => i64::try_from(page_ids.len()).unwrap()
        }));
        let catalog_id =
            document.add_object(dictionary! { "Type" => "Catalog", "Pages" => page_tree_id });
        document.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }

    fn panicking_pdf() -> Vec<u8> {
        let mut document = lopdf::Document::load_mem(&two_page_pdf()).unwrap();
        let page = *document.get_pages().values().next().unwrap();
        document
            .get_object_mut(page)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set(
                "MediaBox",
                vec![
                    lopdf::Object::Name(b"invalid-number".to_vec()),
                    0.into(),
                    612.into(),
                    792.into(),
                ],
            );
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn synthetic_pdf_reproduces_parser_panic() {
        let bytes = panicking_pdf();
        assert!(std::panic::catch_unwind(|| {
            pdf_extract::extract_text_from_mem_by_pages(&bytes)
        })
        .is_err());
    }

    #[tokio::test]
    async fn pdf_panic_preserves_hits_and_reports_each_failed_file() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("documents")).unwrap();
        fs::create_dir_all(root.path().join("notes")).unwrap();
        fs::write(root.path().join("documents/a.txt"), "marker before").unwrap();
        let bytes = panicking_pdf();
        for name in ["b.pdf", "c.pdf"] {
            fs::write(root.path().join("documents").join(name), &bytes).unwrap();
        }
        fs::write(root.path().join("documents/d.pdf"), two_page_pdf()).unwrap();
        fs::write(root.path().join("notes/after.md"), "marker after").unwrap();
        let report = search("marker", root.path(), true, 1, false).await.unwrap();
        assert_eq!(
            report
                .hits
                .iter()
                .map(|hit| hit.path.as_str())
                .collect::<Vec<_>>(),
            ["documents/a.txt", "documents/d.pdf", "notes/after.md"]
        );
        assert_eq!(report.coverage.searched_by_kind.get("pdf"), Some(&1));
        for path in ["documents/b.pdf", "documents/c.pdf"] {
            let missed = report
                .coverage
                .not_searched
                .iter()
                .find(|missed| missed.path == path)
                .unwrap();
            assert_eq!(missed.kind, "pdf");
            assert_eq!(missed.page, None);
            assert_eq!(
                missed.reason,
                "PDF extraction panicked; file was not searched"
            );
        }
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["hits"].as_array().unwrap().len(), 3);
        assert_eq!(
            super::run("marker", root.path(), true, 1, true, false).await,
            std::process::ExitCode::from(2)
        );
    }

    #[tokio::test]
    async fn pdf_decode_error_remains_a_coverage_failure() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("documents")).unwrap();
        fs::write(root.path().join("documents/broken.pdf"), b"not a PDF").unwrap();
        let report = search("marker", root.path(), false, 0, false)
            .await
            .unwrap();
        assert!(report.hits.is_empty());
        assert!(report.coverage.searched_by_kind.is_empty());
        assert_eq!(report.coverage.not_searched.len(), 1);
        assert_eq!(report.coverage.not_searched[0].path, "documents/broken.pdf");
        assert!(report.coverage.not_searched[0]
            .reason
            .starts_with("PDF decode error:"));
    }

    #[tokio::test]
    async fn searches_git_ignored_bytes_and_decodes_soft_broken_email() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("documents/correspondence")).unwrap();
        fs::create_dir_all(root.path().join("documents/dockets")).unwrap();
        fs::write(root.path().join(".gitignore"), "*.eml\n*.pdf\n").unwrap();
        fs::write(
            root.path().join("documents/correspondence/message.eml"),
            "From: Counsel <counsel@example.com>\r\nDate: Tue, 15 Oct 2026 12:00:00 +0000\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\nThe agreement states soft bro=\r\nken phrase.",
        )
        .unwrap();
        fs::write(
            root.path().join("documents/dockets/filing.txt"),
            "The court entered judgment.\n",
        )
        .unwrap();

        let report = search("soft broken phrase", root.path(), true, 0, false)
            .await
            .unwrap();
        assert_eq!(report.hits.len(), 1);
        assert_eq!(report.hits[0].record_class, "correspondence");
        assert_eq!(report.hits[0].message_date.as_deref(), Some("2026-10-15"));
        assert_eq!(report.coverage.searched_by_kind.get("eml"), Some(&1));

        fs::write(
            root.path().join("documents/dockets/ignored-scan.pdf"),
            pdf::blank_acroform(&[]),
        )
        .unwrap();
        let report = search("anything", root.path(), true, 0, false)
            .await
            .unwrap();
        assert!(report
            .coverage
            .not_searched
            .iter()
            .any(|file| file.path.ends_with("ignored-scan.pdf")));
    }

    #[tokio::test]
    async fn html_only_email_is_extracted_as_text() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("documents/correspondence")).unwrap();
        fs::write(
            root.path().join("documents/correspondence/html.eml"),
            "From: Counsel <counsel@example.com>\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>hidden searchable phrase</p>",
        )
        .unwrap();
        let report = search("hidden", root.path(), true, 0, false).await.unwrap();
        assert_eq!(report.hits.len(), 1);
        assert_eq!(report.coverage.searched_by_kind.get("eml"), Some(&1));
    }

    #[tokio::test]
    async fn json_mail_messages_keep_their_own_provenance() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("documents/correspondence")).unwrap();
        fs::write(
            root.path().join("documents/correspondence/export.json"),
            r#"[
                {"date":"2026-01-02","from":"first@example.com","body":"alpha marker","quoted":{"from":"quoted@example.com"}},
                {"date":"2026-03-04","from":"second@example.com","body":"beta marker"}
            ]"#,
        )
        .unwrap();
        let report = search("marker", root.path(), false, 0, false)
            .await
            .unwrap();
        assert_eq!(report.hits.len(), 2);
        assert_eq!(report.hits[0].message_date.as_deref(), Some("2026-01-02"));
        assert_eq!(report.hits[0].from.as_deref(), Some("first@example.com"));
        assert_eq!(report.hits[1].message_date.as_deref(), Some("2026-03-04"));
        assert_eq!(report.hits[1].from.as_deref(), Some("second@example.com"));
    }

    #[tokio::test]
    async fn reports_a_scanned_pdf_as_not_searched() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("documents/filings")).unwrap();
        fs::write(
            root.path().join("documents/filings/scanned.pdf"),
            pdf::blank_acroform(&[]),
        )
        .unwrap();
        let report = search("anything", root.path(), true, 0, false)
            .await
            .unwrap();
        assert!(report.hits.is_empty());
        assert_eq!(report.coverage.not_searched.len(), 1);
        assert!(report.coverage.not_searched[0]
            .reason
            .contains("no searchable text layer"));
    }

    #[tokio::test]
    async fn finds_text_on_second_pdf_page_and_in_ignored_pdf() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("documents/filings")).unwrap();
        fs::write(root.path().join(".gitignore"), "*.pdf\n").unwrap();
        fs::write(
            root.path().join("documents/filings/two-pages.pdf"),
            two_page_pdf(),
        )
        .unwrap();
        let report = search("second page marker", root.path(), false, 0, false)
            .await
            .unwrap();
        assert_eq!(report.hits.len(), 1);
        assert_eq!(report.hits[0].page, Some(2));
        assert_eq!(report.coverage.searched_by_kind.get("pdf"), Some(&1));
        assert!(report.coverage.not_searched.iter().any(|missed| {
            missed.page == Some(3) && missed.reason.contains("no searchable text layer")
        }));
    }

    #[test]
    fn classifies_records_and_keeps_court_search_restricted() {
        assert_eq!(
            classify(std::path::Path::new("documents/rulings/order.pdf")),
            Record::Court
        );
        assert_eq!(
            classify(std::path::Path::new("documents/transcripts/call.txt")),
            Record::ClientStatement
        );
        assert_eq!(
            classify(std::path::Path::new("notes/memo.md")),
            Record::WorkProduct
        );
    }
}
