//! The matter-work `site` verbs: a Project's documents, the attachments an
//! inbound email filed, and a notation's workflow on a live site. They live
//! in this library so the `navigator` binary carries them; `hudson` keeps a
//! deprecated alias for one release.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Subcommand;

use crate::commands::{
    parse_asset_kind, parse_calendar_date, parse_document_visibility, parse_evidence_confidence,
    parse_matter_folder, parse_notarization, parse_signing_method, parse_transcript_quality,
    HostOpt, DOCUMENT_TRANSCRIBE_HELP, DOCUMENT_UPLOAD_KIND_HELP,
};
use crate::{document_read, projects, remote};

/// How an executed document was executed, as `site document upload` records
/// it. Every flag is optional.
#[derive(clap::Args)]
pub struct ExecutionArgs {
    /// A YAML file with the expense amount, currency, payee, category, and source ID.
    #[arg(long, value_parser = read_receipt_metadata, requires = "file")]
    receipt: Option<model::documents::ReceiptMetadata>,
    /// How the executed document was signed: `wet_ink` or `docusign`.
    /// Optional; given together with `--signed-on`.
    #[arg(long, value_parser = parse_signing_method, requires = "signed_on")]
    signing_method: Option<model::documents::SigningMethod>,
    /// The date it was signed, `YYYY-MM-DD`. Given together with
    /// `--signing-method`.
    #[arg(long, value_parser = parse_calendar_date, requires = "signing_method")]
    signed_on: Option<chrono::NaiveDate>,
    /// One notarial acknowledgment, repeatable for a packet with several:
    /// `method=in_person|remote_online,acknowledged_on=YYYY-MM-DD,state=NV,county=Clark,commission_expires=YYYY-MM-DD`.
    /// Never the notary's name or commission number.
    #[arg(long, value_parser = parse_notarization)]
    notarization: Vec<model::documents::Notarization>,
    /// The exhibit metadata the matter's evidence index reads (LAW-185).
    #[command(flatten)]
    evidence: EvidenceArgs,
}

/// The exhibit metadata `site document upload` records for the evidence
/// index. Every flag is optional; the block is recorded only when at least
/// one is given.
#[derive(clap::Args)]
pub struct EvidenceArgs {
    /// The date the exhibit itself bears, `YYYY-MM-DD` — a letter's own
    /// date, a photograph's capture date — not the filing date.
    #[arg(long = "evidence-dated-at", value_parser = parse_calendar_date)]
    dated_at: Option<chrono::NaiveDate>,
    /// Where the exhibit came from, one line — a person, an agency, a URL.
    #[arg(long = "evidence-source")]
    source: Option<String>,
    /// How confidently the exhibit is tied to what it supports:
    /// `exact`, `inferred`, or `unknown`.
    #[arg(long = "evidence-confidence", value_parser = parse_evidence_confidence)]
    confidence: Option<model::documents::EvidenceConfidence>,
    /// A claim reference the exhibit bears on, repeatable — exactly the
    /// reference the reader should follow, `¶ 12` or
    /// `Count II — Conversion`, not prose.
    #[arg(long = "evidence-supports")]
    supports: Vec<String>,
}

impl EvidenceArgs {
    fn record(self) -> Option<model::documents::Evidence> {
        let any = self.dated_at.is_some()
            || self.source.is_some()
            || self.confidence.is_some()
            || !self.supports.is_empty();
        any.then_some(model::documents::Evidence {
            dated_at: self.dated_at,
            source: self.source,
            confidence: self.confidence,
            supports: self.supports,
        })
    }
}

fn read_receipt_metadata(path: &str) -> Result<model::documents::ReceiptMetadata, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("read receipt metadata: {error}"))?;
    let receipt: model::documents::ReceiptMetadata =
        serde_yaml::from_slice(&bytes).map_err(|_| "invalid receipt metadata YAML".to_string())?;
    receipt
        .validate("receipt")
        .map_err(|error| error.to_string())?;
    Ok(receipt)
}

impl ExecutionArgs {
    fn record(self) -> remote::DocumentExecution {
        remote::DocumentExecution {
            receipt: self.receipt,
            signing: self
                .signing_method
                .zip(self.signed_on)
                .map(|(method, signed_on)| model::documents::Signing { method, signed_on }),
            notarization: self.notarization,
            evidence: self.evidence.record(),
        }
    }
}

/// A matter document is only ever reached through a Project on a site, so
/// every verb here is either a write against a named `--project` on a
/// `--host` brand deployment (`upload`), or a read that resolves both from
/// the checkout's own `navigator.yaml` — a pointer path below `blobs/` (the
/// committed `.yaml`), or the bytes below `documents/` it names, is enough for
/// a lawyer to name a file rather than an id or a host.
// The `upload` variant flattens every flag clap cannot box, so it is far
// larger than the read variants; the enum is built once per process run.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
pub enum DocumentAction {
    /// File a local document into a matter (`POST /app/api/projects/{id}/documents`).
    ///
    /// Writes the returned `blobs/<slug>.yaml` pointer when the file belongs
    /// to a checkout for this Project. `navigator project gate --check`
    /// reconciles the other pointers this login can see.
    #[command(after_long_help = DOCUMENT_UPLOAD_KIND_HELP)]
    Upload {
        #[command(flatten)]
        host: HostOpt,
        /// Matter code (human-facing) to file into. Resolved against the
        /// matters this login can see.
        #[arg(long)]
        project: String,
        /// Path to the file to upload.
        #[arg(long, required_unless_present = "dir", conflicts_with = "dir")]
        file: Option<PathBuf>,
        /// Upload regular files in this directory, sorted by name, without recursing.
        #[arg(long, required_unless_present = "file", conflicts_with_all = ["file", "slug"])]
        dir: Option<PathBuf>,
        /// Required asset-lane kind. An invalid value prints the accepted enum.
        #[arg(long, value_parser = parse_asset_kind)]
        kind: String,
        /// `client` makes the document client-visible; default is `internal`.
        #[arg(long, value_parser = parse_document_visibility, default_value = "internal")]
        visibility: String,
        /// Optional description stored with the document.
        #[arg(long)]
        description: Option<String>,
        /// MIME type. Defaults to `application/octet-stream`.
        #[arg(long)]
        content_type: Option<String>,
        /// Stable document identity: a path relative to `documents/`, mirrored
        /// under `blobs/` where the pointer lands (`--slug memos/x.md` writes
        /// `blobs/memos/x.md.yaml`; a bare `x.md` writes it at the root).
        /// It must keep the source file's extension (`--slug motion.pdf` for
        /// `motion.pdf`). When omitted, a file in a folder below
        /// `documents/` defaults to its own path there, so the pointer
        /// mirrors the source; any other file defaults to its filename. A
        /// pointer already in `blobs/` for `--file` names the slug, and a
        /// different `--slug` is refused. A matter folder is not part of the
        /// slug: `documents/Discovery/set-one/rogs.pdf` is `set-one/rogs.pdf`.
        #[arg(long)]
        slug: Option<String>,
        /// The matter folder `navigator project sync` downloads the bytes
        /// into: `Client Production`, `Correspondence (Served)`, `Discovery`,
        /// `Docket Filings`, or `Work Product`. When omitted, the folder
        /// `--file` sits in, else the one `--kind` maps to.
        #[arg(long, value_parser = parse_matter_folder)]
        folder: Option<model::document_pointers::MatterFolder>,
        /// Transcript quality marker. Only accepted when `--kind transcript`.
        #[arg(long, value_parser = parse_transcript_quality)]
        quality: Option<String>,
        #[command(flatten)]
        execution: ExecutionArgs,
    },
    /// OCR a PDF revision locally or file a reviewed transcript.
    #[command(after_long_help = DOCUMENT_TRANSCRIBE_HELP)]
    Transcribe {
        /// Source document pointer under `blobs/`, or its bytes under `documents/`.
        pointer: PathBuf,
        /// File a UTF-8 transcript already reviewed against this source PDF.
        #[arg(long)]
        text_file: Option<PathBuf>,
    },
    /// Set `slug` on a live row that has none
    /// (`PATCH /app/api/projects/{id}/documents/{asset_id}`).
    ///
    /// Optionally sets `kind` in the same write. Refuses a row that already
    /// has a slug, and a slug that already names a revision chain. After it
    /// succeeds, `navigator project gate --check` can write the pointer.
    #[command(after_long_help = DOCUMENT_UPLOAD_KIND_HELP)]
    Slug {
        #[command(flatten)]
        host: HostOpt,
        /// Matter code (human-facing). Resolved against the matters this login can see.
        #[arg(long)]
        project: String,
        /// Asset id of the slugless row.
        asset_id: uuid::Uuid,
        /// Document identity. Must carry the stored filename's extension.
        #[arg(long)]
        slug: String,
        /// Asset-lane kind. Omit to leave the stored kind unchanged; pass it
        /// whenever the stored kind is not an accepted one, because a slugged
        /// row's kind otherwise needs `navigator site document kind`.
        #[arg(long, value_parser = parse_asset_kind)]
        kind: Option<String>,
        /// Validate and print the result. The row is left unchanged.
        #[arg(long)]
        dry_run: bool,
    },
    /// Replace a legacy `kind` the asset lane rejects on a live row and its
    /// slug chain (`PATCH /app/api/projects/{id}/documents/{asset_id}`).
    ///
    /// Refuses a row whose kind is already accepted, so it never changes one
    /// valid classification into another. `sha256`, the storage key, and the
    /// slug stay unchanged. After it succeeds, `navigator project gate
    /// --check` can rewrite the pointer.
    #[command(after_long_help = DOCUMENT_UPLOAD_KIND_HELP)]
    Kind {
        #[command(flatten)]
        host: HostOpt,
        /// Matter code (human-facing). Resolved against the matters this login can see.
        #[arg(long)]
        project: String,
        /// Asset id of the row whose kind the asset lane rejects.
        asset_id: uuid::Uuid,
        /// Asset-lane kind to write.
        #[arg(long, value_parser = parse_asset_kind)]
        kind: String,
        /// Validate and print the result. The row is left unchanged.
        #[arg(long)]
        dry_run: bool,
    },
    /// Change a document chain from one accepted kind to another
    /// (`POST /app/api/projects/{id}/documents/{asset_id}/reclassify`).
    ///
    /// The correction for a document filed under the wrong valid kind. Every
    /// revision of the slug chain moves together; `sha256`, the storage key,
    /// byte size, and the slug stay unchanged. Refuses a row whose kind is not
    /// accepted (use `navigator site document kind`) and a target equal to the
    /// current kind. After it succeeds, `navigator project gate --check` can
    /// rewrite the pointer.
    #[command(after_long_help = DOCUMENT_UPLOAD_KIND_HELP)]
    Reclassify {
        #[command(flatten)]
        host: HostOpt,
        /// Matter code (human-facing). Resolved against the matters this login can see.
        #[arg(long)]
        project: String,
        /// Asset id of any revision in the chain to correct.
        asset_id: uuid::Uuid,
        /// Accepted asset-lane kind the chain moves to.
        #[arg(long, value_parser = parse_asset_kind)]
        kind: String,
        /// Validate and print the result. The row is left unchanged.
        #[arg(long)]
        dry_run: bool,
    },
    /// Change a filed document's visibility
    /// (`PATCH /app/api/projects/{id}/documents/{asset_id}`).
    ///
    /// Re-uploading the same bytes returns the existing revision unchanged, so
    /// this is the way to move a filed document between `internal` and
    /// `client`. No new revision is written; the slug, `sha256`, and storage
    /// key stay as they were. After it succeeds, `navigator project gate
    /// --check` rewrites the committed pointer's `visibility`.
    Visibility {
        #[command(flatten)]
        host: HostOpt,
        /// Matter code (human-facing). Resolved against the matters this login can see.
        #[arg(long)]
        project: String,
        /// Asset id of the revision whose visibility changes.
        asset_id: uuid::Uuid,
        /// `client` shows the document to the client; `internal` hides it.
        #[arg(long, value_parser = parse_document_visibility)]
        visibility: String,
    },
    /// Move a document identity to another slug valid for its filename, such
    /// as `notes/a.pdf` to `filed/forms/a.pdf`
    /// (`PATCH /app/api/projects/{id}/documents/{asset_id}`).
    ///
    /// Refuses a target equal to the current slug and a target that already
    /// names a chain on this matter. The whole revision chain moves together;
    /// `sha256`, the storage key, and `kind` stay unchanged. After a real
    /// move, the committed pointer under `--dir` and its sibling bytes follow
    /// the slug together, or the command leaves both in place.
    Reslug {
        #[command(flatten)]
        host: HostOpt,
        /// Matter code (human-facing). Resolved against the matters this login can see.
        #[arg(long)]
        project: String,
        /// Asset id of any revision of the document identity to move.
        asset_id: uuid::Uuid,
        /// Target slug. Must carry the stored filename's extension and must
        /// not already name a chain on this matter.
        #[arg(long)]
        slug: String,
        /// Project checkout whose `blobs/` pointer and `documents/` copy follow the move.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Validate and print the result. The row and the checkout are left unchanged.
        #[arg(long)]
        dry_run: bool,
    },
    /// Retire a mistaken document chain without a pointer-removal commit
    /// (`POST /app/api/projects/{id}/documents/{asset_id}/delete`).
    ///
    /// This keeps the chain recoverable for 180 days. It requires an explicit
    /// `--yes-retire`; afterward, remove any local pointer and byte copy for the retired slug.
    Retire {
        #[command(flatten)]
        host: HostOpt,
        /// Matter code (human-facing). Resolved against the matters this login can see.
        #[arg(long)]
        project: String,
        /// Asset id of any revision of the mistaken document chain.
        asset_id: uuid::Uuid,
        /// Validate the target and report the chain without changing it.
        #[arg(long)]
        dry_run: bool,
        /// Confirm soft-deletion of the entire revision chain for 180 days.
        #[arg(long)]
        yes_retire: bool,
    },
    /// Slug or retire the live rows an inbound email thread filed without a
    /// slug, which `project gate --check` fails a Project on.
    ///
    /// Signature logos, inline images, and rows whose bytes another live row
    /// already holds are retired (soft-deleted, recoverable for 180 days);
    /// every other emailed row is slugged `mail/<asset id>/<YYYY.MM.DD>_<name>-0.<ext>`.
    /// Rows from other channels are listed and left alone. Run `--dry-run`
    /// first: it validates every slug against the server and writes nothing.
    Slugless {
        #[command(flatten)]
        host: HostOpt,
        /// Matter code (human-facing). Resolved against the matters this login can see.
        #[arg(long)]
        project: String,
        /// Report the plan and validate each slug without changing anything.
        #[arg(long)]
        dry_run: bool,
        /// Confirm the slugs and soft-deletions the dry run reported.
        #[arg(long)]
        yes: bool,
    },
    /// Restore a soft-deleted document with its whole revision history
    /// (`POST /app/api/projects/{id}/documents/{asset_id}/restore`).
    ///
    /// Deleting a document's pointer and running `navigator project sync`
    /// soft-deletes it for 180 days; this undoes that within the window. Find
    /// the asset id with `navigator site document log <pointer>`. The next
    /// `project sync` writes the pointer again.
    Restore {
        #[command(flatten)]
        host: HostOpt,
        /// Matter code (human-facing). Resolved against the matters this login can see.
        #[arg(long)]
        project: String,
        /// Asset id of any revision of the soft-deleted document.
        asset_id: uuid::Uuid,
    },
    /// Place or release a Legal Hold on a document
    /// (`POST /app/api/projects/{id}/documents/{asset_id}/hold`).
    ///
    /// A held document is never purged after its 180-day soft-deletion window.
    Hold {
        #[command(flatten)]
        host: HostOpt,
        /// Matter code (human-facing). Resolved against the matters this login can see.
        #[arg(long)]
        project: String,
        /// Asset id of any revision of the document.
        asset_id: uuid::Uuid,
        /// Release the hold instead of placing it.
        #[arg(long)]
        release: bool,
    },
    /// Restore a missing storage object from a same-hash sibling in the matter
    /// (`POST /app/api/projects/{id}/documents/{asset_id}/storage`).
    ///
    /// Admin only. Copies the sibling's bytes onto the content-addressed key,
    /// or re-points the row at that key, then the server verifies the sha256.
    /// No sibling changes nothing.
    Repair {
        #[command(flatten)]
        host: HostOpt,
        /// Matter code (human-facing). Resolved against the matters this login can see.
        #[arg(long)]
        project: String,
        /// Asset id whose storage object is missing.
        asset_id: uuid::Uuid,
        /// Report the repair. Storage and the row are left unchanged.
        #[arg(long)]
        dry_run: bool,
    },
    /// The revision chain, newest first, marking the operative row.
    Log {
        /// Pointer below `blobs/`, such as `blobs/pleadings/motion.pdf.yaml`,
        /// or its bytes below `documents/`. The retired `.yml` spelling is
        /// still read.
        pointer: PathBuf,
    },
    /// Fetch one revision to a local path, verified by `sha256` and size
    /// before success is reported. Refuses a destination inside `documents/`
    /// except the pointer's own sync-managed copy, which it refreshes with the
    /// pointer's current revision.
    Get {
        pointer: PathBuf,
        /// Revision number to fetch. Defaults to the operative revision under
        /// your lens.
        #[arg(long)]
        version: Option<usize>,
        /// Where to write the fetched bytes. Outside `documents/`, or the
        /// pointer's own copy (`documents/<name>` for `blobs/<name>.yaml`).
        #[arg(long)]
        out: PathBuf,
    },
    /// A text redline between two revision numbers. PDF and plain text only;
    /// any other type is reported unsupported rather than diffed as bytes.
    Diff {
        pointer: PathBuf,
        a: usize,
        b: usize,
    },
}

#[derive(Subcommand)]
pub enum SiteMailAction {
    /// File one inbound message's attachments into a matter, without the
    /// bytes ever touching this checkout.
    #[command(after_long_help = DOCUMENT_UPLOAD_KIND_HELP)]
    File {
        #[command(flatten)]
        host: HostOpt,
        /// Matter code (human-facing) to file into.
        #[arg(long)]
        project: String,
        /// `email_conversation_message` row id naming the inbound hop.
        #[arg(long)]
        message: uuid::Uuid,
        /// Required asset-lane kind, applied to every attachment.
        #[arg(long, value_parser = parse_asset_kind)]
        kind: String,
        /// `client` makes every filed attachment client-visible; default `internal`.
        #[arg(long, value_parser = parse_document_visibility, default_value = "internal")]
        visibility: String,
        /// List what would be filed, with size and content type; write nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
pub enum NotationAction {
    /// List the private notation inventory for one matter. The output includes
    /// the template code, workflow state, and respondent so an author can
    /// check for an existing instrument before opening another one.
    List {
        #[command(flatten)]
        host: HostOpt,
        /// Matter code whose notation inventory to inspect.
        #[arg(long)]
        project: String,
        /// Emit the raw JSON inventory.
        #[arg(long)]
        json: bool,
    },
    /// Create a questionnaire-driven notation on an existing matter and
    /// leave its questionnaire ready for the site intake flow.
    ///
    /// Every notation hangs on an already-existing Project, so `--project`
    /// (the matter code) is required — open the matter first with
    /// `navigator project create`. The template is read from that
    /// Project's git repo when authored there, else from the bundled firm
    /// catalog; the notation opens pinned to it
    /// (`POST /app/projects/<id>/notations/new`).
    #[command(hide = true)]
    Create {
        /// Template code, e.g. `onboarding__letter`,
        /// `offboarding__letter`, or `nv__llc_formation`.
        template_code: String,
        #[command(flatten)]
        host: HostOpt,
        /// Client email — the notation's bound client (signer). Must be an
        /// existing client on the matter.
        #[arg(long)]
        client_email: String,
        /// Matter **code** (human-facing, from `project create`) to open the
        /// notation inside. Resolved to the Project id against the matters
        /// you can see. Required — the matter must already exist.
        #[arg(long)]
        project: String,
        /// A saved answers file (as `site notation answers --save-answers`
        /// writes) to prefill the new notation from: every question the
        /// template asks is answered from the file's `state: value` map, so
        /// the notation's interpolated fields match the notation the file
        /// was saved from. A question the file does not answer fails the
        /// create — prefill is whole, never partial.
        #[arg(long = "answers-from", alias = "answers-file")]
        answers_from: Option<PathBuf>,
    },
    /// Print a notation's workflow state + signature request id
    /// (`GET …/review?format=json`).
    Status {
        /// Notation UUID.
        notation_id: uuid::Uuid,
        #[command(flatten)]
        host: HostOpt,
        /// Emit the raw JSON status body.
        #[arg(long)]
        json: bool,
    },
    /// Read the filed answers and their source provenance for one notation.
    Answers {
        /// Notation UUID.
        notation_id: uuid::Uuid,
        #[command(flatten)]
        host: HostOpt,
        /// Emit the raw JSON answers.
        #[arg(long)]
        json: bool,
        /// Save the filed answers to this path as a `state: value` YAML map
        /// for `--answers-from` / `--answers-file` prefill. Intended on a
        /// completed notation — typically an onboarding whose answers a
        /// follow-on notation should repeat.
        #[arg(long)]
        save_answers: Option<PathBuf>,
    },
    /// Render + park a notation's document for review (`POST
    /// …/approve-send`) — fills the bound packet (a formation's official
    /// Secretary-of-State form, or a retainer PDF). Idempotent once
    /// rendered.
    Approve {
        /// Notation UUID.
        notation_id: uuid::Uuid,
        #[command(flatten)]
        host: HostOpt,
    },
    /// Send a notation parked at `lawyer_review` back for changes: flag the
    /// wrong answers so they get re-collected, then return to review
    /// (`POST …/request-changes`). A rejected review re-collects the wrong
    /// answers instead of dead-ending; declining the matter is separate.
    RequestChanges {
        /// Notation UUID.
        notation_id: uuid::Uuid,
        #[command(flatten)]
        host: HostOpt,
        /// A question code to flag for re-collection, e.g. `person__client`.
        /// Repeatable — one per answer to re-collect.
        #[arg(long = "question")]
        questions: Vec<String>,
        /// An optional note to the re-collector — what to fix.
        #[arg(long)]
        note: Option<String>,
    },
    /// Re-collect the flagged answers on a notation parked at
    /// `reask__client` (lawyer on the client's behalf) and resubmit for
    /// review (`POST …/reask`). Only the flagged answers are re-collected;
    /// every other answer stays as it was — answers and questions are
    /// decoupled, so a correction never re-walks the questionnaire.
    Update {
        /// Notation UUID.
        notation_id: uuid::Uuid,
        #[command(flatten)]
        host: HostOpt,
        /// A corrected answer as `code=value`, e.g.
        /// `person__client=Libra Jones`. Repeatable; each code must have
        /// been flagged by the review.
        #[arg(long = "answer")]
        answers: Vec<String>,
    },
    /// Download a notation's rendered document (the filled packet) to a
    /// local file (`GET …/documents/document`).
    Document {
        /// Notation UUID.
        notation_id: uuid::Uuid,
        /// Path to write the PDF to.
        #[arg(long)]
        out: PathBuf,
        #[command(flatten)]
        host: HostOpt,
    },
}

pub async fn run_notation(action: NotationAction) -> ExitCode {
    match action {
        NotationAction::List {
            host,
            project,
            json,
        } => remote::notation_list(host.host.as_deref(), &project, json).await,
        NotationAction::Create {
            template_code,
            host,
            client_email: _,
            project: _,
            answers_from,
        } => {
            if answers_from.is_some() && !template_code.starts_with("onboarding") {
                eprintln!(
                    "navigator site notation create: --answers-from prefills an onboarding; offboarding and other templates take no answers file"
                );
                return ExitCode::FAILURE;
            }
            match template_code.as_str() {
                "onboarding" | "onboarding__letter" => {
                    projects::lifecycle::onboard(
                        host.host.as_deref(),
                        Path::new("."),
                        answers_from.as_deref(),
                        None,
                    )
                    .await
                }
                "offboarding" | "offboarding__letter" => {
                    projects::lifecycle::offboard(host.host.as_deref(), Path::new("."), None).await
                }
                _ => {
                    eprintln!(
                        "navigator site notation create: use `navigator project onboard` or `navigator project offboard`"
                    );
                    ExitCode::FAILURE
                }
            }
        }
        NotationAction::Status {
            notation_id,
            host,
            json,
        } => remote::notation_status(host.host.as_deref(), notation_id, json).await,
        NotationAction::Answers {
            notation_id,
            host,
            json,
            save_answers,
        } => {
            remote::notation_answers(
                host.host.as_deref(),
                notation_id,
                json,
                save_answers.as_deref(),
            )
            .await
        }
        NotationAction::Approve { notation_id, host } => {
            remote::notation_approve(host.host.as_deref(), notation_id).await
        }
        NotationAction::RequestChanges {
            notation_id,
            host,
            questions,
            note,
        } => {
            remote::notation_request_changes(
                host.host.as_deref(),
                notation_id,
                &questions,
                note.as_deref(),
            )
            .await
        }
        NotationAction::Update {
            notation_id,
            host,
            answers,
        } => remote::notation_update(host.host.as_deref(), notation_id, &answers).await,
        NotationAction::Document {
            notation_id,
            out,
            host,
        } => remote::notation_document(host.host.as_deref(), notation_id, &out).await,
    }
}

#[allow(clippy::too_many_lines)] // one flat dispatch match; splitting it hurts readability
pub async fn run_document(action: DocumentAction) -> ExitCode {
    match action {
        DocumentAction::Upload {
            host,
            project,
            file,
            dir,
            kind,
            visibility,
            description,
            content_type,
            slug,
            folder,
            quality,
            execution,
        } => {
            if let Some(dir) = dir {
                return remote::document_upload_directory(
                    host.host.as_deref(),
                    &project,
                    &dir,
                    &kind,
                    Some(&visibility),
                    description.as_deref(),
                    content_type.as_deref(),
                    quality.as_deref(),
                    &execution.record(),
                    folder,
                )
                .await;
            }
            let Some(file) = file else {
                return ExitCode::from(2);
            };
            remote::document_upload(
                host.host.as_deref(),
                &project,
                &file,
                &kind,
                Some(&visibility),
                description.as_deref(),
                content_type.as_deref(),
                slug.as_deref(),
                quality.as_deref(),
                &execution.record(),
                folder,
            )
            .await
        }
        DocumentAction::Transcribe { pointer, text_file } => {
            remote::document_transcribe(&pointer, text_file.as_deref()).await
        }
        DocumentAction::Slug {
            host,
            project,
            asset_id,
            slug,
            kind,
            dry_run,
        } => {
            remote::document_slug(
                host.host.as_deref(),
                &project,
                asset_id,
                &slug,
                kind.as_deref(),
                dry_run,
            )
            .await
        }
        DocumentAction::Kind {
            host,
            project,
            asset_id,
            kind,
            dry_run,
        } => remote::document_kind(host.host.as_deref(), &project, asset_id, &kind, dry_run).await,
        DocumentAction::Reclassify {
            host,
            project,
            asset_id,
            kind,
            dry_run,
        } => {
            remote::document_reclassify(host.host.as_deref(), &project, asset_id, &kind, dry_run)
                .await
        }
        DocumentAction::Visibility {
            host,
            project,
            asset_id,
            visibility,
        } => {
            remote::document_visibility(host.host.as_deref(), &project, asset_id, &visibility).await
        }
        DocumentAction::Reslug {
            host,
            project,
            asset_id,
            slug,
            dir,
            dry_run,
        } => {
            remote::document_reslug(
                host.host.as_deref(),
                &project,
                asset_id,
                &slug,
                &dir,
                dry_run,
            )
            .await
        }
        DocumentAction::Retire {
            host,
            project,
            asset_id,
            dry_run,
            yes_retire,
        } => {
            remote::document_retire(
                host.host.as_deref(),
                &project,
                asset_id,
                dry_run,
                yes_retire,
            )
            .await
        }
        DocumentAction::Slugless {
            host,
            project,
            dry_run,
            yes,
        } => remote::document_slugless(host.host.as_deref(), &project, dry_run, yes).await,
        DocumentAction::Restore {
            host,
            project,
            asset_id,
        } => remote::document_restore(host.host.as_deref(), &project, asset_id).await,
        DocumentAction::Hold {
            host,
            project,
            asset_id,
            release,
        } => remote::document_hold(host.host.as_deref(), &project, asset_id, !release).await,
        DocumentAction::Repair {
            host,
            project,
            asset_id,
            dry_run,
        } => remote::document_repair(host.host.as_deref(), &project, asset_id, dry_run).await,
        DocumentAction::Log { pointer } => document_read::log(&pointer).await,
        DocumentAction::Get {
            pointer,
            version,
            out,
        } => document_read::get(&pointer, version, &out).await,
        DocumentAction::Diff { pointer, a, b } => document_read::diff(&pointer, a, b).await,
    }
}

/// File one inbound message's attachments (`site mail file`).
pub async fn run_mail(action: SiteMailAction) -> ExitCode {
    match action {
        SiteMailAction::File {
            host,
            project,
            message,
            kind,
            visibility,
            dry_run,
        } => {
            remote::mail_file(
                Path::new("."),
                host.host.as_deref(),
                &project,
                message,
                &kind,
                &visibility,
                dry_run,
            )
            .await
        }
    }
}
