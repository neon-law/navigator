//! `hudson mail packet`: hand a rendered, signed packet to the
//! Firm's mail-center vendor for printing and physical mail (LAW-311).
//!
//! Phase 1 of the mailroom: the job goes to the vendor as an **unsent**
//! Gmail draft — the PDFs attached, and a job sheet in the body naming the
//! recipient's address block, the service level, the printing instructions,
//! and the mail-by date — for the lawyer to review and send. The draft is
//! logged on the matter as an outbound Communication with the role `other`.
//! Navigator never sends it.
//!
//! A packet must be the signed revision. A PDF still stamped as a preview,
//! still carrying a `/s/ DRAFT` counsel signature, or carrying signing-line
//! anchors (`nlsig-…`) its document pointer does not record as signed is
//! refused before anything is drafted.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use chrono::NaiveDate;

/// How the vendor mails the packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Service {
    /// USPS Certified Mail, with its receipt.
    Certified,
    /// USPS First-Class Mail.
    FirstClass,
}

impl Service {
    fn phrase(self) -> &'static str {
        match self {
            Self::Certified => "USPS Certified Mail",
            Self::FirstClass => "USPS First-Class Mail",
        }
    }
}

/// One mail job for the vendor.
#[derive(Debug, Clone)]
pub struct MailJob {
    pub pdfs: Vec<PathBuf>,
    /// The vendor's address, from its Person or Entity row; never compiled in.
    pub vendor: String,
    /// The recipient's address block, one printed line each.
    pub recipient: Vec<String>,
    pub service: Service,
    pub return_receipt: bool,
    pub mail_by: NaiveDate,
}

impl MailJob {
    /// The draft's subject line.
    #[must_use]
    pub fn subject(&self) -> String {
        format!(
            "Mail job: {} to {} by {}",
            self.service.phrase(),
            self.recipient.first().map_or("", String::as_str),
            self.mail_by.format("%B %-d, %Y")
        )
    }

    /// The job sheet the vendor works from: what to print, how, where it
    /// goes, how it goes, and by when.
    #[must_use]
    pub fn sheet(&self) -> String {
        let mut out =
            String::from("Please print and mail the attached packet.\n\nDocuments, in order:\n");
        for pdf in &self.pdfs {
            let name = pdf.file_name().map_or_else(
                || pdf.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            let _ = writeln!(out, "  - {name}");
        }
        out.push_str("\nRecipient:\n");
        for line in &self.recipient {
            let _ = writeln!(out, "  {line}");
        }
        let _ = writeln!(out, "\nService: {}", self.service.phrase());
        let _ = writeln!(
            out,
            "Return receipt: {}",
            if self.return_receipt { "yes" } else { "no" }
        );
        out.push_str("Printing: single-sided, no staples.\n");
        let _ = writeln!(out, "Mail by: {}", self.mail_by.format("%B %-d, %Y"));
        out.push_str(
            "\nPlease reply with the certified-mail receipt and tracking number, and the \
             return-receipt card when it comes back.\n",
        );
        out
    }
}

/// Check every field of a job before anything is drafted.
///
/// # Errors
///
/// A missing PDF, an empty recipient block, a blank vendor, or a packet that
/// is not the signed revision.
pub fn check(job: &MailJob) -> Result<()> {
    if job.pdfs.is_empty() {
        bail!("a mail job needs at least one PDF");
    }
    if !job.vendor.contains('@') {
        bail!("--vendor must be the vendor's email address, from its Person or Entity row");
    }
    if job.recipient.iter().all(|line| line.trim().is_empty()) {
        bail!("--recipient needs the recipient's address block, one line per flag");
    }
    for pdf in &job.pdfs {
        if pdf
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
            != Some("pdf")
        {
            bail!("{} is not a PDF", pdf.display());
        }
        let bytes = std::fs::read(pdf).with_context(|| format!("read {}", pdf.display()))?;
        if let Some(reason) = unsigned_reason(pdf, &bytes)? {
            bail!(
                "{} is not the signed revision: {reason}; mail only the signed copy",
                pdf.display()
            );
        }
    }
    Ok(())
}

/// Why `bytes` (read from `path`) is not a packet's signed revision, or
/// `None` when it is.
fn unsigned_reason(path: &Path, bytes: &[u8]) -> Result<Option<String>> {
    let count = |needle: &str| {
        pdf::occurrence_count(bytes, needle)
            .map_err(|error| anyhow!("read the text of {}: {error}", path.display()))
    };
    if count(pdf::PREVIEW_LABEL)? > 0 {
        return Ok(Some("it is a preview render".into()));
    }
    if count("/s/ DRAFT")? > 0 {
        return Ok(Some("its counsel signature is still `/s/ DRAFT`".into()));
    }
    if count("nlsig-")? > 0 && !pointer_records_signing(path) {
        return Ok(Some(
            "it carries signing lines, and its document pointer records no `signing:`".into(),
        ));
    }
    Ok(None)
}

/// Whether `pdf`, checked out under a Project's `documents/`, has a pointer
/// under `blobs/` that records how it was signed.
fn pointer_records_signing(pdf: &Path) -> bool {
    let Ok(pdf) = std::fs::canonicalize(pdf) else {
        return false;
    };
    let Some(root) =
        cli::projects::matter::find_manifest(&pdf).and_then(|m| m.parent().map(Path::to_path_buf))
    else {
        return false;
    };
    let Ok(slug) = pdf.strip_prefix(root.join("documents")) else {
        return false;
    };
    let pointer = root.join("blobs").join(format!("{}.yaml", slug.display()));
    std::fs::read_to_string(pointer)
        .ok()
        .and_then(|text| serde_yaml::from_str::<serde_yaml::Value>(&text).ok())
        .is_some_and(|value| value.get("signing").is_some_and(|s| !s.is_null()))
}

#[cfg(test)]
mod tests {
    use super::{check, MailJob, Service};

    fn job(pdfs: Vec<std::path::PathBuf>) -> MailJob {
        MailJob {
            pdfs,
            vendor: "jobs@mail-center.example".into(),
            recipient: vec![
                "Internal Revenue Service".into(),
                "Sampleton, UT 84000".into(),
            ],
            service: Service::Certified,
            return_receipt: true,
            mail_by: chrono::NaiveDate::from_ymd_opt(2026, 11, 13).unwrap(),
        }
    }

    fn pdf_of(
        dir: &std::path::Path,
        name: &str,
        markdown: &str,
        preview: bool,
    ) -> std::path::PathBuf {
        let bytes = pdf::render_document_with_options(
            markdown,
            pdf::OutputFormat::Plain,
            &pdf::Letterhead::default(),
            &pdf::RenderOptions {
                preview,
                ..pdf::RenderOptions::default()
            },
        )
        .unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// The job sheet names every fact the vendor needs.
    #[test]
    fn the_job_sheet_names_documents_recipient_service_printing_and_date() {
        let job = job(vec!["packet/2026.11.12_reply.pdf".into()]);
        let sheet = job.sheet();
        for text in [
            "2026.11.12_reply.pdf",
            "  Internal Revenue Service\n  Sampleton, UT 84000",
            "Service: USPS Certified Mail",
            "Return receipt: yes",
            "single-sided, no staples",
            "Mail by: November 13, 2026",
        ] {
            assert!(sheet.contains(text), "{text}: {sheet}");
        }
        assert_eq!(
            job.subject(),
            "Mail job: USPS Certified Mail to Internal Revenue Service by November 13, 2026"
        );
    }

    /// LAW-311: only the signed revision is mailed — a preview, a `/s/
    /// DRAFT` signature, or unsigned signing lines are refused.
    #[test]
    fn only_the_signed_revision_is_queued() {
        let dir = tempfile::tempdir().unwrap();
        let signed = pdf_of(dir.path(), "signed.pdf", "Signed reply.\n", false);
        assert!(check(&job(vec![signed])).is_ok());

        let preview = pdf_of(dir.path(), "preview.pdf", "Reply.\n", true);
        let error = check(&job(vec![preview])).unwrap_err().to_string();
        assert!(error.contains("preview"), "{error}");

        let draft = pdf_of(
            dir.path(),
            "draft.pdf",
            &format!("Reply.\n\n{}\n", word::paper::COUNSEL_SIGNATURE),
            false,
        );
        let error = check(&job(vec![draft])).unwrap_err().to_string();
        assert!(error.contains("/s/ DRAFT"), "{error}");

        let anchored = pdf_of(
            dir.path(),
            "anchored.pdf",
            &word::paper::mark_signing_lines("Reply.\n\n{{client.signature}}\n"),
            false,
        );
        let error = check(&job(vec![anchored])).unwrap_err().to_string();
        assert!(error.contains("signing:"), "{error}");

        let mut blank = job(vec![dir.path().join("signed.pdf")]);
        blank.vendor = String::new();
        assert!(check(&blank).is_err());
    }
}
