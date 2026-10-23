//! The server half the estate and closing sections share: each Notation on a
//! matter, read once, with the workflow, open terms, holds, and execution
//! records its sections are built from.
//!
//! Nothing here invents a fact. A formality is a state in the Notation's own
//! workflow; an open term is a `{{question}}` its pinned Draft carries that no
//! answer on the Notation fills; a hold is a paragraph its Draft writes wholly
//! in square brackets. A Notation whose Draft cannot be read (no object
//! storage, or a body that fails its pinned digest) shows its workflow from
//! the bundled catalog when it has one and carries no open terms or holds,
//! rather than failing the page.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use uuid::Uuid;

/// A `{{question}}` an instrument's Draft carries that no answer fills.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenTerm {
    /// The code of the question that answers it, e.g. `person__successor_agent`.
    pub question: String,
    /// The question's prompt, or the code in words when none is stored.
    pub label: String,
    /// The fragment of the first outline unit that carries it.
    pub fragment: String,
}

/// A paragraph the Draft writes wholly in square brackets: a parked drafting
/// decision.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hold {
    pub text: String,
    pub fragment: String,
}

/// One Notation on the matter, with everything its sections read.
#[derive(Clone, Debug, Default)]
pub struct Instrument {
    pub id: Uuid,
    /// The Draft's code, e.g. `estate__poa`.
    pub code: String,
    /// The Draft's title.
    pub title: String,
    /// The Draft's declared `kind:`, e.g. `trust`; empty when it declares none.
    pub kind: String,
    /// The Notation's current workflow state.
    pub state: String,
    /// The workflow's states along its forward path, `BEGIN` and `END`
    /// excluded. Empty when no workflow could be read.
    pub workflow: Vec<String>,
    pub open_terms: Vec<OpenTerm>,
    pub holds: Vec<Hold>,
    /// Every signature request, oldest first.
    pub signatures: Vec<store::signatures::Signature>,
    /// Every notarization request, oldest first.
    pub notarizations: Vec<store::notarizations::Notarization>,
    /// Every outbound submission, oldest first.
    pub filings: Vec<store::filings::Filing>,
    /// Each answer as `(key, value)`, latest last. The key is the answer's
    /// questionnaire state (`person__agent`) when it records one, else its
    /// question code.
    pub answers: Vec<(String, serde_json::Value)>,
    /// The terms the Draft carries that an answer fills, in Draft order.
    pub agreed_terms: Vec<OpenTerm>,
    /// Each signature request's signer by name, parallel to `signatures`;
    /// empty when the request names no signer.
    pub signer_names: Vec<String>,
    /// The latest contract review, read only for the lawyer lens.
    pub review: Option<store::contract_reviews::ContractReview>,
}

impl Instrument {
    /// Whether the Notation has reached its terminal state.
    #[must_use]
    pub fn closed(&self) -> bool {
        self.state == workflows::StateName::END
    }

    /// The latest answer that fills the open-term `question`: its state, or
    /// either half of a `<type>__<role>` state.
    #[must_use]
    pub fn term_answer(&self, question: &str) -> Option<&serde_json::Value> {
        self.answer(|key| {
            key == question
                || key
                    .split_once("__")
                    .is_some_and(|(kind, role)| kind == question || role == question)
        })
    }

    /// The latest answer whose key satisfies `matches`.
    pub fn answer(&self, matches: impl Fn(&str) -> bool) -> Option<&serde_json::Value> {
        self.answers
            .iter()
            .rev()
            .find(|(code, _)| matches(code))
            .map(|(_, value)| value)
    }
}

/// A word or code in plain words: underscores spaced, first letter capital.
#[must_use]
pub fn words(raw: &str) -> String {
    let spaced = raw.replace("__", " ").replace('_', " ");
    let mut chars = spaced.trim().chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + chars.as_str()
    })
}

/// The conditions a forward path prefers, in order. Anything else is taken
/// only when none of these leaves a state, and never a rejection.
const FORWARD: &[&str] = &[
    "_",
    "approved",
    "accepted",
    "signed",
    "completed",
    "complete",
    "notarized",
    "filed",
    "sent",
    "received",
    "done",
    "yes",
];

/// The workflow's states from `BEGIN` to `END` along the forward edge each
/// state offers, both markers excluded. A loop back (a rejection to `reask`)
/// is never taken, and a cycle ends the path.
#[must_use]
pub fn forward_path(spec: &workflows::WorkflowSpec) -> Vec<String> {
    let mut path = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut here = workflows::StateName::begin();
    seen.insert(here.as_str().to_string());
    while let Some(transitions) = spec.states.get(&here) {
        let unseen = |state: &workflows::StateName| !seen.contains(state.as_str());
        let next = FORWARD
            .iter()
            .filter_map(|condition| transitions.lookup(condition))
            .find(|state| unseen(state))
            .or_else(|| {
                transitions
                    .conditions()
                    .filter(|condition| {
                        !condition.contains("reject") && !condition.contains("declin")
                    })
                    .filter_map(|condition| transitions.lookup(condition))
                    .find(|state| unseen(state))
            })
            .cloned();
        let Some(next) = next else { break };
        if next.as_str() == workflows::StateName::END {
            break;
        }
        seen.insert(next.as_str().to_string());
        path.push(next.as_str().to_string());
        here = next;
    }
    path
}

/// Every key an answer fills: its question code and its full questionnaire
/// state, plus each half of a `<type>__<role>` state.
#[must_use]
pub fn answered_keys(answers: &[(String, Option<String>)]) -> HashSet<String> {
    let mut keys = HashSet::new();
    for (code, state) in answers {
        keys.insert(code.clone());
        if let Some(state) = state {
            keys.insert(state.clone());
            if let Some((kind, role)) = state.split_once("__") {
                keys.insert(kind.to_string());
                keys.insert(role.to_string());
            }
        }
    }
    keys
}

/// What a parsed Draft carries: its open terms, its holds, and the terms an
/// answer already fills.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Drafting {
    pub open_terms: Vec<OpenTerm>,
    pub holds: Vec<Hold>,
    pub agreed_terms: Vec<OpenTerm>,
}

/// The terms and holds in a parsed Draft. A term is open when no key in
/// `answered` names its question, and agreed when one does; each question is
/// listed once, at the first unit that carries it. `label` is left as the
/// code in words.
#[must_use]
pub fn drafting_state<S: std::hash::BuildHasher>(
    doc: &views::harvard_outline::OutlineDocument,
    answered: &HashSet<String, S>,
) -> Drafting {
    use views::harvard_outline::{self as outline, Run, UnitKind};

    let mut terms: Vec<OpenTerm> = Vec::new();
    let mut agreed: Vec<OpenTerm> = Vec::new();
    let mut holds = Vec::new();
    for unit in &doc.units {
        if unit.kind == UnitKind::Hold {
            if let Some(text) = outline::hold_body(&unit.markdown) {
                holds.push(Hold {
                    text: text.to_string(),
                    fragment: unit.fragment.clone(),
                });
            }
        }
        for run in outline::runs(&unit.markdown) {
            let Run::OpenTerm { question, .. } = run else {
                continue;
            };
            if terms.iter().chain(&agreed).any(|t| t.question == question) {
                continue;
            }
            let term = OpenTerm {
                label: words(question.rsplit("__").next().unwrap_or(&question)),
                fragment: unit.fragment.clone(),
                question,
            };
            if answered.contains(&term.question) {
                agreed.push(term);
            } else {
                terms.push(term);
            }
        }
    }
    Drafting {
        open_terms: terms,
        holds,
        agreed_terms: agreed,
    }
}

/// The workflow a Notation runs: the bundled catalog's when its Draft code
/// has one, else the `workflow:` block of its pinned Draft.
fn workflow_of(code: &str, frontmatter: Option<&str>) -> Vec<String> {
    let spec = workflows::catalog_spec_yaml(code)
        .and_then(|yaml| workflows::workflow_spec_from_yaml(yaml).ok())
        .or_else(|| frontmatter.and_then(|yaml| workflows::workflow_spec_from_yaml(yaml).ok()));
    spec.as_ref().map(forward_path).unwrap_or_default()
}

/// Every Notation on `project_id`, oldest first, each with its Draft's
/// workflow, terms, holds, answers, and execution records. The contract
/// review is the firm's work product, so only a `lawyer` read loads it.
///
/// # Errors
///
/// A store read failure, as text.
#[allow(clippy::too_many_lines)]
pub async fn load(
    surreal: &store::surreal::SurrealDb,
    storage: Option<&Arc<dyn cloud::StorageService>>,
    project_id: Uuid,
    lawyer: bool,
) -> Result<Vec<Instrument>, String> {
    let mut notations = store::notations::list_by_project(surreal, project_id)
        .await
        .map_err(|e| e.to_string())?;
    notations.reverse();
    let template_ids: Vec<Uuid> = notations.iter().map(|n| n.template_id).collect();
    let templates = store::templates::find_by_ids(surreal, &template_ids)
        .await
        .map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(notations.len());
    let mut asked: Vec<String> = Vec::new();
    for notation in notations {
        let template = templates.iter().find(|t| t.id == notation.template_id);
        let answers = store::answers::for_notation(surreal, notation.id)
            .await
            .map_err(|e| e.to_string())?;
        let question_ids: Vec<Uuid> = answers.iter().map(|a| a.question_id).collect();
        let codes: HashMap<Uuid, String> = store::questions::find_by_ids(surreal, &question_ids)
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|q| (q.id, q.code))
            .collect();
        let keyed: Vec<(String, Option<String>)> = answers
            .iter()
            .map(|a| {
                (
                    codes.get(&a.question_id).cloned().unwrap_or_default(),
                    a.state_name.clone(),
                )
            })
            .collect();
        let body = match storage {
            Some(storage) => store::notations::pinned_body(surreal, storage, &notation)
                .await
                .ok(),
            None => None,
        };
        let doc = body.as_deref().map(views::harvard_outline::parse);
        let drafting = doc
            .as_ref()
            .map(|doc| drafting_state(doc, &answered_keys(&keyed)))
            .unwrap_or_default();
        asked.extend(
            drafting
                .open_terms
                .iter()
                .chain(&drafting.agreed_terms)
                .map(|t| t.question.clone()),
        );
        let workflow = workflow_of(
            template.map(|t| t.code.as_str()).unwrap_or_default(),
            doc.as_ref().and_then(|d| d.frontmatter.as_deref()),
        );
        let signatures = store::signatures::for_notation(surreal, notation.id)
            .await
            .map_err(|e| e.to_string())?;
        let signer_ids: Vec<Uuid> = signatures
            .iter()
            .filter_map(|s| s.signer_person_id)
            .collect();
        let signers = store::persons::find_by_ids(surreal, &signer_ids)
            .await
            .map_err(|e| e.to_string())?;
        let signer_names = signatures
            .iter()
            .map(|s| {
                s.signer_person_id
                    .and_then(|id| signers.iter().find(|p| p.id == id))
                    .map(|p| p.name.clone())
                    .unwrap_or_default()
            })
            .collect();
        let review = if lawyer {
            store::contract_reviews::latest_for_notation(surreal, notation.id)
                .await
                .map_err(|e| e.to_string())?
        } else {
            None
        };
        out.push(Instrument {
            id: notation.id,
            code: template.map(|t| t.code.clone()).unwrap_or_default(),
            title: template.map(|t| t.title.clone()).unwrap_or_default(),
            kind: template.and_then(|t| t.kind.clone()).unwrap_or_default(),
            state: notation.state.clone(),
            workflow,
            open_terms: drafting.open_terms,
            holds: drafting.holds,
            agreed_terms: drafting.agreed_terms,
            signatures,
            signer_names,
            review,
            notarizations: store::notarizations::for_notation(surreal, notation.id)
                .await
                .map_err(|e| e.to_string())?,
            filings: store::filings::for_notation(surreal, notation.id)
                .await
                .map_err(|e| e.to_string())?,
            answers: answers
                .into_iter()
                .zip(keyed)
                .map(|(answer, (code, state))| {
                    (
                        state.filter(|s| !s.is_empty()).unwrap_or(code),
                        answer.value,
                    )
                })
                .collect(),
        });
    }
    // Name each open term by its question's prompt where one is stored.
    asked.sort_unstable();
    asked.dedup();
    let prompts: HashMap<String, String> = store::questions::find_by_codes(surreal, &asked)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|q| (q.code, q.prompt))
        .collect();
    for term in out
        .iter_mut()
        .flat_map(|i| i.open_terms.iter_mut().chain(i.agreed_terms.iter_mut()))
    {
        if let Some(prompt) = prompts.get(&term.question).filter(|p| !p.trim().is_empty()) {
            term.label.clone_from(prompt);
        }
    }
    Ok(out)
}

/// Where on the page a Notation's units can be linked: the Notation the
/// page's `notation_document` shows, when the page carries that section.
#[derive(Clone, Copy, Debug, Default)]
pub struct OnPage<'a> {
    /// The page's `draft:` path.
    pub draft: Option<&'a str>,
    /// Whether this lens of the page shows `notation_document`.
    pub document_on_page: bool,
}

/// The Notation `notation_document` shows: the newest whose Draft code the
/// page's `draft:` names, else the newest — the rule
/// [`super::notations::load_notations`] applies.
#[must_use]
pub fn document_notation(instruments: &[Instrument], draft: Option<&str>) -> Option<Uuid> {
    let code = draft.and_then(super::notations::draft_code);
    instruments
        .iter()
        .rev()
        .find(|i| code.as_ref().is_none_or(|code| &i.code == code))
        .map(|i| i.id)
}

/// Point every link into `notation_id`'s outline at the same unit of the
/// `notation_document` on this page instead: `…/outline#frag` becomes
/// `#frag`.
pub fn relink(href: &mut String, code: &str, notation_id: Uuid) {
    let prefix = format!("{}#", outline_href(code, notation_id, ""));
    if let Some(fragment) = href.strip_prefix(&prefix) {
        *href = format!("#{fragment}");
    }
}

/// Build the Notation-backed sections into `cx`, scoped to its lens. The
/// clerk lens never reaches this: it loads none of a matter's legal work.
/// An open term or hold on the Notation this page's `notation_document`
/// shows links to its paragraph on the page; any other links to the
/// Notation's outline.
///
/// # Errors
///
/// A store read failure, as text.
pub async fn load_sections(
    surreal: &store::surreal::SurrealDb,
    storage: Option<&Arc<dyn cloud::StorageService>>,
    project: &store::projects::Project,
    on_page: OnPage<'_>,
    cx: &mut super::LoadContext,
) -> Result<(), String> {
    use super::estate::{derive, EstateViews};

    let lawyer = cx.lens == super::PageLens::Lawyer;
    let instruments = load(surreal, storage, project.id, lawyer).await?;
    let records = ModuleRecords::load(surreal, project.id, lawyer).await?;
    let name = |party: &store::project_references::Party| records.name(party);
    cx.estate = EstateViews {
        instruments: derive::instrument_cards(&instruments, &project.code, lawyer),
        ceremony: derive::ceremony(&instruments, &cx.deadlines),
        distribution: derive::distribution(&instruments),
        stages: derive::stages(&instruments),
        fiduciaries: derive::fiduciaries(&records.fiduciaries, name),
        assets: derive::assets(&records.assets),
    };
    let assets = store::assets::for_project(surreal, project.id)
        .await
        .map_err(|e| e.to_string())?;
    cx.closing = super::transactional::derive::views(&super::transactional::derive::Inputs {
        instruments: &instruments,
        code: &project.code,
        lawyer,
        holdings: &records.holdings,
        name: &name,
        assets: &assets,
    });
    let shown = on_page
        .document_on_page
        .then(|| document_notation(&instruments, on_page.draft))
        .flatten();
    if let Some(id) = shown {
        let blockers = cx
            .estate
            .instruments
            .iter_mut()
            .flat_map(|card| card.blockers.iter_mut().map(|b| &mut b.href));
        let checklist = cx.closing.checklist.iter_mut().map(|item| &mut item.href);
        let terms = cx.closing.open_terms.iter_mut().map(|row| &mut row.href);
        for href in blockers.chain(checklist).chain(terms) {
            relink(href, &project.code, id);
        }
    }
    Ok(())
}

/// The rows behind the `estate` and `cap_table` Modules this reader sees,
/// scoped the way their read APIs are: the firm reads every row, and a client
/// reads a Module's rows only while it is enabled on the matter.
struct ModuleRecords {
    fiduciaries: Vec<store::fiduciary_roles::FiduciaryRole>,
    assets: Vec<store::estate_assets::EstateAsset>,
    holdings: Vec<store::cap_table_holdings::CapTableHolding>,
    names: HashMap<Uuid, String>,
}

impl ModuleRecords {
    async fn load(
        surreal: &store::surreal::SurrealDb,
        project_id: Uuid,
        lawyer: bool,
    ) -> Result<Self, String> {
        use store::project_modules::{is_enabled, Module};

        let reads = |module| async move {
            if lawyer {
                return Ok(true);
            }
            is_enabled(surreal, project_id, module)
                .await
                .map_err(|e| e.to_string())
        };
        let (fiduciaries, assets) = if reads(Module::Estate).await? {
            (
                store::fiduciary_roles::list(surreal, project_id)
                    .await
                    .map_err(|e| e.to_string())?,
                store::estate_assets::list(surreal, project_id)
                    .await
                    .map_err(|e| e.to_string())?,
            )
        } else {
            (Vec::new(), Vec::new())
        };
        let holdings = if reads(Module::CapTable).await? {
            store::cap_table_holdings::list(surreal, project_id)
                .await
                .map_err(|e| e.to_string())?
        } else {
            Vec::new()
        };
        let parties: Vec<store::project_references::Party> = fiduciaries
            .iter()
            .map(|r| r.fiduciary)
            .chain(holdings.iter().map(|h| h.holder))
            .collect();
        let ids = |kind: store::relationships::Endpoint| -> Vec<Uuid> {
            parties
                .iter()
                .filter(|p| p.kind == kind)
                .map(|p| p.id)
                .collect()
        };
        let mut names = HashMap::new();
        for person in
            store::persons::find_by_ids(surreal, &ids(store::relationships::Endpoint::Person))
                .await
                .map_err(|e| e.to_string())?
        {
            names.insert(person.id, person.name);
        }
        for entity in
            store::entities::find_by_ids(surreal, &ids(store::relationships::Endpoint::Entity))
                .await
                .map_err(|e| e.to_string())?
        {
            names.insert(entity.id, entity.name);
        }
        Ok(Self {
            fiduciaries,
            assets,
            holdings,
            names,
        })
    }

    /// A Party's stored name, or a plain placeholder when it no longer reads.
    fn name(&self, party: &store::project_references::Party) -> String {
        self.names
            .get(&party.id)
            .cloned()
            .unwrap_or_else(|| "Name not on file".to_string())
    }
}

/// How a step of a Notation's execution stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepState {
    Done,
    /// The Notation sits at this step now.
    Now,
    Pending,
    Declined,
}

impl StepState {
    /// The state in plain words.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Done => "Done",
            Self::Now => "In progress",
            Self::Pending => "Not started",
            Self::Declined => "Declined",
        }
    }
}

/// One execution formality from a Notation's workflow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Formality {
    pub label: String,
    pub state: StepState,
    /// When it was done, `YYYY-MM-DD`; empty otherwise.
    pub on: String,
}

/// What a workflow state asks of the signing: a signature, witnesses, a
/// notary, or the firm's countersignature. `None` for any other state.
fn formality_label(state: &str) -> Option<String> {
    let name = workflows::StateName::from(state);
    let prefix = name.prefix();
    if prefix == "sent_for_signature" {
        return Some("Signer signs".to_string());
    }
    if prefix == "witnesses" {
        return Some("Witnesses sign".to_string());
    }
    match workflows::step_kind_for(&name)? {
        workflows::StepKind::Notarization => Some("Notarized".to_string()),
        workflows::StepKind::FirmSignature => Some("Firm countersigns".to_string()),
        workflows::StepKind::Signature => {
            let role = prefix
                .trim_end_matches("_signatures")
                .trim_end_matches("_signature");
            Some(format!("{} signs", words(role)))
        }
        _ => None,
    }
}

fn day(raw: &str) -> String {
    raw.chars().take(10).collect()
}

/// The index of the Notation's current state on its forward path.
fn position(instrument: &Instrument) -> Option<usize> {
    let current = workflows::StateName::from(instrument.state.as_str());
    instrument
        .workflow
        .iter()
        .position(|state| workflows::StateName::from(state.as_str()).prefix() == current.prefix())
}

/// The execution formalities a Notation's workflow declares, in workflow
/// order, one per state prefix. A step behind the current state is done;
/// a completed signature, a notarized request, or a closed Notation marks
/// its steps done regardless, and a declined envelope marks the signing
/// steps declined.
#[must_use]
pub fn formalities(instrument: &Instrument) -> Vec<Formality> {
    use store::signatures::SignatureState;

    let at = position(instrument);
    let signed = instrument
        .signatures
        .iter()
        .rev()
        .find(|s| s.state == SignatureState::Completed);
    let declined = instrument
        .signatures
        .last()
        .is_some_and(|s| s.state == SignatureState::Declined);
    let notarized = instrument
        .notarizations
        .iter()
        .rev()
        .find_map(|n| n.notarized_at.clone());
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (index, state) in instrument.workflow.iter().enumerate() {
        let Some(label) = formality_label(state) else {
            continue;
        };
        if !seen.insert(
            workflows::StateName::from(state.as_str())
                .prefix()
                .to_string(),
        ) {
            continue;
        }
        let is_notary = label == "Notarized";
        let is_firm = label == "Firm countersigns";
        let mut step = if instrument.closed() || at.is_some_and(|at| index < at) {
            StepState::Done
        } else if at == Some(index) {
            StepState::Now
        } else {
            StepState::Pending
        };
        let mut on = String::new();
        if is_notary {
            if let Some(when) = &notarized {
                step = StepState::Done;
                on = day(when);
            }
        } else if !is_firm {
            if let Some(signature) = signed {
                step = StepState::Done;
                on = signature.signed_at.as_deref().map(day).unwrap_or_default();
            } else if declined {
                step = StepState::Declined;
            }
        }
        out.push(Formality {
            label,
            state: step,
            on,
        });
    }
    out
}

/// Where an instrument stands, in the order its card reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    Drafting,
    Approved,
    AtCeremony,
    Blocked,
    Executed,
}

impl Standing {
    /// The standing in plain words.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Drafting => "Drafting",
            Self::Approved => "Approved",
            Self::AtCeremony => "At ceremony",
            Self::Blocked => "Blocked",
            Self::Executed => "Executed",
        }
    }
}

/// Where `instrument` stands:
///
/// - **Executed** once the Notation is closed, or every formality its
///   workflow declares is done;
/// - **Blocked** while its Draft carries an open term or a hold;
/// - **At ceremony** while it sits at a signing, witnessing, or notary step;
/// - **Approved** once it is past review and the document is prepared;
/// - **Drafting** before that.
#[must_use]
pub fn standing(instrument: &Instrument) -> Standing {
    let steps = formalities(instrument);
    if instrument.closed()
        || (!steps.is_empty() && steps.iter().all(|s| s.state == StepState::Done))
    {
        return Standing::Executed;
    }
    if !instrument.open_terms.is_empty() || !instrument.holds.is_empty() {
        return Standing::Blocked;
    }
    if formality_label(&instrument.state).is_some() {
        return Standing::AtCeremony;
    }
    let reviewed = instrument.workflow.iter().position(|state| {
        matches!(
            workflows::StateName::from(state.as_str()).prefix(),
            "lawyer_review" | "client_review"
        )
    });
    let prefix = workflows::StateName::from(instrument.state.as_str())
        .prefix()
        .to_string();
    let past_review = match (reviewed, position(instrument)) {
        (Some(review), Some(at)) => at > review,
        _ => matches!(prefix.as_str(), "generate_pdf" | "document_drafts"),
    };
    if past_review {
        Standing::Approved
    } else {
        Standing::Drafting
    }
}

/// A workflow state as a stage name.
#[must_use]
pub fn stage_label(state: &str) -> String {
    use workflows::StepKind;

    let name = workflows::StateName::from(state);
    let prefix = name.prefix().to_string();
    let named = match prefix.as_str() {
        "sent_for_signature" => Some("Sent for signature"),
        "intake_persisted" => Some("Answers in"),
        "document_drafts" => Some("Drafts prepared"),
        "extract" => Some("Inputs extracted"),
        "analysis" => Some("Analysis"),
        "witnesses" => Some("Witnesses"),
        _ => None,
    };
    if let Some(named) = named {
        return named.to_string();
    }
    match workflows::step_kind_for(&name) {
        Some(StepKind::LawyerReview) => "Lawyer review".to_string(),
        Some(StepKind::ClientReview) => "Client review".to_string(),
        Some(StepKind::Reask) => "Answers updated".to_string(),
        Some(StepKind::Notarization) => "Notarization".to_string(),
        Some(StepKind::Signature) => formality_label(state).unwrap_or_else(|| words(&prefix)),
        Some(StepKind::FirmSignature) => "Firm signature".to_string(),
        Some(StepKind::GeneratePdf) => "Document prepared".to_string(),
        Some(StepKind::DocumentIntake) => "Document received".to_string(),
        Some(StepKind::MailroomSend) => "Mailing".to_string(),
        Some(StepKind::MailroomReceive) => "Notice received".to_string(),
        Some(StepKind::CertifiedMail) => "Certified mail".to_string(),
        Some(StepKind::EmailSend) => "Email sent".to_string(),
        Some(StepKind::EFiling | StepKind::Filing) => "Filing".to_string(),
        Some(StepKind::OnChainRecord) => "Attestation recorded".to_string(),
        Some(StepKind::System) | None => words(&prefix),
    }
}

/// Whether a workflow state is an outbound submission a regulator or office
/// receives: mail, certified mail, e-filing, or a filing.
#[must_use]
pub fn is_submission(state: &str) -> bool {
    matches!(
        workflows::step_kind_for(&workflows::StateName::from(state)),
        Some(
            workflows::StepKind::MailroomSend
                | workflows::StepKind::CertifiedMail
                | workflows::StepKind::EFiling
                | workflows::StepKind::Filing
        )
    )
}

/// The outline page for one Notation, optionally at a fragment.
#[must_use]
pub fn outline_href(code: &str, notation_id: Uuid, fragment: &str) -> String {
    let base = crate::notation_outline::notation_outline_href(code, &notation_id.to_string());
    if fragment.is_empty() {
        base
    } else {
        format!("{base}#{fragment}")
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{
        answered_keys, drafting_state, formalities, forward_path, stage_label, standing,
        Instrument, Standing, StepState,
    };

    pub(crate) fn spec(yaml: &str) -> workflows::WorkflowSpec {
        workflows::workflow_spec_from_yaml(yaml).unwrap()
    }

    const WILL: &str = "workflow:\n  BEGIN:\n    _: lawyer_review\n  lawyer_review:\n    approved: generate_pdf\n    rejected: reask\n  reask:\n    _: lawyer_review\n  generate_pdf:\n    _: client_signature\n  client_signature:\n    _: witnesses\n  witnesses:\n    _: notarization\n  notarization:\n    _: END\n  END: {}\n";

    pub(crate) fn will(state: &str) -> Instrument {
        Instrument {
            id: uuid::Uuid::now_v7(),
            title: "Pour-over will".to_string(),
            kind: "will".to_string(),
            state: state.to_string(),
            workflow: forward_path(&spec(WILL)),
            ..Instrument::default()
        }
    }

    #[test]
    fn the_forward_path_skips_the_rejection_loop() {
        assert_eq!(
            forward_path(&spec(WILL)),
            [
                "lawyer_review",
                "generate_pdf",
                "client_signature",
                "witnesses",
                "notarization"
            ]
        );
    }

    #[test]
    fn formalities_come_from_the_workflow_and_follow_the_state() {
        let steps = formalities(&will("witnesses"));
        let labels: Vec<&str> = steps.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["Client signs", "Witnesses sign", "Notarized"]);
        let states: Vec<StepState> = steps.iter().map(|s| s.state).collect();
        assert_eq!(
            states,
            [StepState::Done, StepState::Now, StepState::Pending]
        );
    }

    #[test]
    fn each_standing_follows_its_rule() {
        assert_eq!(standing(&will("lawyer_review")), Standing::Drafting);
        assert_eq!(standing(&will("generate_pdf")), Standing::Approved);
        assert_eq!(standing(&will("client_signature")), Standing::AtCeremony);
        assert_eq!(standing(&will("END")), Standing::Executed);
        let mut blocked = will("generate_pdf");
        blocked.open_terms.push(super::OpenTerm {
            question: "person__successor_agent".into(),
            label: "Successor agent".into(),
            fragment: "section-iv-p1".into(),
        });
        assert_eq!(standing(&blocked), Standing::Blocked);
    }

    #[test]
    fn an_answered_term_is_not_open_and_a_hold_is_found() {
        let doc = views::harvard_outline::parse(
            "# Durable Power of Attorney\n\n## I. Agent\n\n{{person__agent.name}} serves, then {{person__successor_agent.name}}.\n\n[Confirm the gifting power.]\n",
        );
        let answered = answered_keys(&[("person".into(), Some("person__agent".into()))]);
        let drafting = drafting_state(&doc, &answered);
        let (terms, holds) = (drafting.open_terms, drafting.holds);
        assert_eq!(drafting.agreed_terms.len(), 1);
        assert_eq!(drafting.agreed_terms[0].question, "person__agent");
        assert_eq!(terms.len(), 1, "{terms:?}");
        assert_eq!(terms[0].question, "person__successor_agent");
        assert_eq!(terms[0].label, "Successor agent");
        assert_eq!(holds.len(), 1);
        assert_eq!(holds[0].text, "Confirm the gifting power.");
        assert!(!holds[0].fragment.is_empty());
    }

    #[test]
    fn a_link_into_the_notation_on_the_page_becomes_a_fragment() {
        let id = uuid::Uuid::now_v7();
        let mut on_page = super::outline_href("m", id, "section-i-p1");
        super::relink(&mut on_page, "m", id);
        assert_eq!(on_page, "#section-i-p1");
        let mut elsewhere = super::outline_href("m", uuid::Uuid::now_v7(), "section-i-p1");
        let before = elsewhere.clone();
        super::relink(&mut elsewhere, "m", id);
        assert_eq!(elsewhere, before);
        let mut older = will("END");
        older.code = "estate__will".into();
        let mut newer = will("lawyer_review");
        newer.code = "estate__poa".into();
        let both = [older.clone(), newer.clone()];
        assert_eq!(super::document_notation(&both, None), Some(newer.id));
        assert_eq!(
            super::document_notation(&both, Some("drafts/estate__will.md")),
            Some(older.id)
        );
    }

    #[test]
    fn stages_read_in_plain_words() {
        assert_eq!(stage_label("lawyer_review"), "Lawyer review");
        assert_eq!(stage_label("client_signature"), "Client signs");
        assert_eq!(stage_label("mailroom_send"), "Mailing");
        assert_eq!(stage_label("filing__secretary_of_state"), "Filing");
    }
}
