//! The store rows each sample matter's declared [Matter
//! Pages](../../../docs/glossary/matter-page.md) read.
//!
//! The three sample repositories declare their pages in `navigator.yaml`
//! and carry no Portal, so every fact a page shows is a row here: the
//! litigation matter's Case, Docket Entries, Deadlines, discovery sets,
//! E-Filing Transactions, adverse party, and Authorities; the estate
//! matter's Authorities. Each fact is the one the sample repository's
//! former Portal data stated, so a page shows what that Portal showed.
//!
//! Every write is idempotent: a re-seed finds the rows it wrote and leaves
//! them alone, so a boot never doubles a docket.
//!
//! When `navigator dev sample-project` has staged a repository's source
//! ([`crate::sample_project::STAGE_ENV`]), the same pass stores the
//! manifest's `pages:` and persists each Draft under `drafts/`, opening a
//! Notation on the matter's engagement and instruments, each pinned to its
//! Draft's commit and body digest.
//!
//! **Everything here is invented.** No person, court paper, or date names a
//! real matter.

use std::sync::Arc;

use anyhow::Context as _;
use chrono::NaiveDate;
use rules::citation::{AuthorityClass, Disposition};
use uuid::Uuid;

use crate::cases::{Device, EntryKind};
use crate::projects::Project;
use crate::surreal::SurrealDb;

use super::{SAMPLE_ESTATE_CODE, SAMPLE_LITIGATION_CODE, SAMPLE_TRANSACTIONAL_CODE};

/// One Docket Entry of *Cruller v. Prine*, in date order.
struct Entry {
    on: &'static str,
    kind: EntryKind,
    title: &'static str,
    party: Option<&'static str>,
}

const PLAINTIFF: &str = "Plaintiff Dermot A. Cruller";
const DEFENDANT: &str = "Defendant Wendell Prine";

/// The docket, mirroring every filed entry of the sample's case timeline.
/// The court's date of each comes from the document or proceeding that
/// owns it; the dates still ahead are [`DEADLINES`].
const DOCKET: &[Entry] = &[
    Entry {
        on: "2026-02-06",
        kind: EntryKind::Pleading,
        title: "Complaint — trespass to land",
        party: Some(PLAINTIFF),
    },
    Entry {
        on: "2026-02-06",
        kind: EntryKind::Other,
        title: "Summons issued and served on Wendell Prine",
        party: None,
    },
    Entry {
        on: "2026-04-14",
        kind: EntryKind::Other,
        title: "The rest of the doughnut is eaten",
        party: Some(PLAINTIFF),
    },
    Entry {
        on: "2026-04-15",
        kind: EntryKind::Other,
        title: "Dermot learns of the soul term",
        party: Some(PLAINTIFF),
    },
    Entry {
        on: "2026-04-20",
        kind: EntryKind::Other,
        title: "Engagement letter signed with Neon Law",
        party: Some(PLAINTIFF),
    },
    Entry {
        on: "2026-05-02",
        kind: EntryKind::Notice,
        title: "Notice of rescission served",
        party: Some(PLAINTIFF),
    },
    Entry {
        on: "2026-06-11",
        kind: EntryKind::Other,
        title: "Affidavit of Odile Cruller drafted",
        party: Some("Plaintiff witness Odile Cruller"),
    },
    Entry {
        on: "2026-06-25",
        kind: EntryKind::DiscoveryRequest,
        title: "Plaintiff's First Set of Interrogatories served",
        party: Some(PLAINTIFF),
    },
    Entry {
        on: "2026-07-27",
        kind: EntryKind::DiscoveryResponse,
        title: "Responses and objections to Plaintiff's First Set of Interrogatories",
        party: Some(DEFENDANT),
    },
    Entry {
        on: "2026-08-03",
        kind: EntryKind::Pleading,
        title: "Amended complaint adds Count II — rescission of the doughnut instrument",
        party: Some(PLAINTIFF),
    },
    Entry {
        on: "2026-08-10",
        kind: EntryKind::Pleading,
        title: "Answer to Count II and counterclaim",
        party: Some(DEFENDANT),
    },
    Entry {
        on: "2026-08-12",
        kind: EntryKind::Other,
        title: "Counsel meet and confer about the deficient responses",
        party: None,
    },
    Entry {
        on: "2026-08-14",
        kind: EntryKind::DiscoveryRequest,
        title: "Defendant's First Set of Interrogatories served",
        party: Some(DEFENDANT),
    },
    Entry {
        on: "2026-08-18",
        kind: EntryKind::Motion,
        title: "Motion for partial summary judgment on the limitations defense",
        party: Some(PLAINTIFF),
    },
    Entry {
        on: "2026-08-31",
        kind: EntryKind::Pleading,
        title: "Answer to counterclaim",
        party: Some(PLAINTIFF),
    },
];

/// A date still ahead on the sample's case timeline.
struct Ahead {
    due_at: &'static str,
    authority_kind: &'static str,
    authority: &'static str,
    calendar_role: &'static str,
    client_lead_days: i64,
    key: &'static str,
}

const DEADLINES: &[Ahead] = &[
    Ahead {
        due_at: "2026-09-14T00:00:00-07:00",
        authority_kind: "court_rule",
        authority: "Answers to Defendant's First Set of Interrogatories due (NRCP 33(b)(2))",
        calendar_role: "firm",
        client_lead_days: 7,
        key: "interrogatory-answers",
    },
    Ahead {
        due_at: "2026-09-22T00:00:00-07:00",
        authority_kind: "court_order",
        authority: "Hearing on the motion for partial summary judgment, Department XVII",
        calendar_role: "hearing",
        client_lead_days: 7,
        key: "msj-hearing",
    },
    Ahead {
        due_at: "2026-10-06T00:00:00-07:00",
        authority_kind: "court_rule",
        authority: "Deposition of Dermot A. Cruller, noticed by the defense",
        calendar_role: "opponent",
        client_lead_days: 14,
        key: "deposition",
    },
    Ahead {
        due_at: "2027-03-08T00:00:00-08:00",
        authority_kind: "court_order",
        authority: "Trial setting",
        calendar_role: "hearing",
        client_lead_days: 30,
        key: "trial",
    },
];

/// The court papers the sample filed, as E-Filing Transactions.
const FILINGS: &[(&str, &str, &str)] = &[
    (
        "sample-litigation-amended-complaint",
        "2026-08-03",
        "Amended complaint (Count II)",
    ),
    (
        "sample-litigation-msj",
        "2026-08-18",
        "Motion for partial summary judgment",
    ),
    (
        "sample-litigation-answer-to-counterclaim",
        "2026-08-31",
        "Answer to counterclaim",
    ),
];

struct Interrogatory {
    number: i32,
    asked: &'static str,
    answer: Option<&'static str>,
    objections: Option<&'static str>,
}

struct LitAuthority {
    cite: &'static str,
    year: &'static str,
    leaning: &'static str,
    quote: &'static str,
    application: &'static str,
    url: &'static str,
}

struct EstateAuthority {
    caption: &'static str,
    citation: &'static str,
    year: u16,
    court: &'static str,
    holding: &'static str,
}

const LITIGATION_AUTHORITIES: &[LitAuthority] = &[
    LitAuthority { cite: "Sharp Elecs. Corp. v. Deutsche Fin. Servs. Corp., 216 F.3d 388 (4th Cir. 2000)", year: "2000", leaning: "neutral", quote: "When an offeree, who has performed partly, continues performance requested by the offer after learning of the offer, it accepts the offer by completing the requested performance, even if it did not know of the offer when it first began to perform.", application: "The closest authority to the two bites, and it cuts both ways. Acceptance lands on completion — the second bite, not the first. But the rule it states is about an offeree who completes *after learning*, and Dermot completed before he learned anything. Cite it for the timing; distinguish it on knowledge.", url: "https://app.midpage.ai/document/sharp-electronics-corporation-v-deutsche-769164" },
    LitAuthority { cite: "Knight v. Seattle-First Nat’l Bank, 589 P.2d 1279 (Wash. Ct. App. 1979)", year: "1979", leaning: "dermot", quote: "If an offer for a unilateral contract is made, and part of the consideration requested in the offer is given or tendered by the offeree in response thereto, the offeror is bound by a contract, the duty of immediate performance of which is conditional on the full consideration being given or tendered within the time stated in the offer, or, if no time is stated therein, within a reasonable time.", application: "The formation argument in one sentence. A first bite is the beginning of a performance, not its completion — so nothing closed on 1 April 2025. Note the sting in the tail: § 45 conditions the deal on full consideration within a *reasonable time*, and Prine will argue a year in a refrigerator is not one.", url: "https://app.midpage.ai/document/knight-v-seattle-first-national-1418102" },
    LitAuthority { cite: "D.R. Horton, Inc. v. Green, 120 Nev. 549, 96 P.3d 1159 (2004)", year: "2004", leaning: "dermot", quote: "Generally, both procedural and substantive unconscionability must be present in order for a court to exercise its discretion to refuse to enforce a . . . clause as unconscionable. . . . A clause is procedurally unconscionable when a party lacks a meaningful opportunity to agree to the clause terms either because of unequal bargaining power, as in an adhesion contract, or because the clause and its effects are not readily ascertainable upon a review of the contract.", application: "The unconscionability ground in the rescission notice needs this rather than instinct. A term for one doughnut placed where the offeree cannot read it before accepting is a textbook case for the procedural half — not ascertainable on review — and the sliding scale means the one-sidedness that follows from trading a soul for a doughnut does the rest.", url: "https://www.courtlistener.com/opinion/2623589/dr-horton-inc-v-green/" },
    LitAuthority { cite: "Friendly Irishman, Inc. v. Ronnow, 330 P.2d 497 (Nev. 1958)", year: "1958", leaning: "dermot", quote: "Since rescission is sought upon the ground of fraud, knowledge of the material facts constituting the fraud must appear if the payments are to be held to constitute waiver or election.", application: "The case the count is built on, and it is binding Nevada authority. The buyer kept paying for the car and still rescinded, because she did not yet know the car was used. Dermot kept eating and still rescinds, because he did not yet know about the soul.", url: "https://app.midpage.ai/document/friendly-irishman-inc-v-ronnow-1127259" },
    LitAuthority { cite: "Continental Ins. Co. v. Kingston, 114 P.3d 1158 (Utah Ct. App. 2005)", year: "2005", leaning: "prine", quote: "if the party[ ]defrauded continues to receive benefits under the contract after he had become aware of the fraud, or if he otherwise conducts himself with respect to it as though it were a subsisting and binding engagement, he will be deemed to have affirmed the contract and waived his right to rescind", application: "The best statement of the defense — and it carries its own answer. Every formulation of the rule is conditioned on awareness. Prine has to put knowledge in Dermot’s hands before 14 April 2026, and on the pleaded facts he cannot.", url: "https://app.midpage.ai/document/continental-insurance-co-v-kingston-2590758" },
    LitAuthority { cite: "Nev. Rev. Stat. § 11.190(3)(d)", year: "in force", leaning: "dermot", quote: "an action for relief on the ground of fraud or mistake, but the cause of action in such a case shall be deemed to accrue upon the discovery by the aggrieved party of the facts constituting the fraud or mistake.", application: "Disposes of the limitations defense outright. Measured from the first bite the claim is a year and four months old; measured from discovery, four months. Either way it is inside three years — the gap is a ratification problem wearing a limitations costume.", url: "https://app.midpage.ai/laws/nv-nrs/t2/ch11/s11_190-a4d843" },
];

const OUTBOUND_INTERROGATORIES: &[Interrogatory] = &[
    Interrogatory { number: 1, asked: "State verbatim each word You spoke to Cruller at the hedge between the Cruller and Prine properties on 1 April 2025.", answer: Some("I said hello, and I said the doughnut was neat. I offered it to him the way I would offer anybody a doughnut. After a year and some months I cannot swear to every other word, but nothing I said was about a soul, because I would not say that to a neighbor."), objections: None },
    Interrogatory { number: 2, asked: "State whether, at the time of the offer described in Paragraph 6 of the Amended Complaint, You appeared to Cruller in a horned aspect.", answer: Some("Subject to and without waiving that objection: no. I have never appeared to Cruller, or to anyone, in a horned aspect. I was wearing a sweater."), objections: Some("Vague and ambiguous: The phrase ") },
    Interrogatory { number: 3, asked: "Identify each term of the agreement alleged in the Amended Complaint that was not spoken aloud on 1 April 2025, including any writing borne by or contained within the doughnut.", answer: None, objections: Some("Calls for a legal conclusion: The interrogatory asks Defendant to characterize ; Overbroad: To the extent the interrogatory seeks every unspoken term of an agreement Defendant denies exists, it is not proportional to the needs of the case.") },
    Interrogatory { number: 4, asked: "State the date on which You first contended that the soul of Cruller had been conveyed to You, and identify each person to whom You stated that contention.", answer: Some("Subject to that objection, and excluding anything I discussed with my lawyer: 14 April 2026, in my own kitchen, to my wife Verity. I did not say it to Cruller, and I have not said it to anybody else."), objections: Some("Attorney–client privilege and work product: Defendant objects to the extent the interrogatory seeks the date or content of communications with counsel, or materials prepared in anticipation of litigation.") },
    Interrogatory { number: 5, asked: "Identify each person present at the hedge on 1 April 2025, and each person present when Cruller consumed the remainder of the doughnut on 14 April 2026.", answer: Some("On 1 April 2025: myself, Cruller, my wife Verity, and my boys Ambrose and Errol, who were in the yard. On 14 April 2026 I was not present and I do not know who was."), objections: None },
    Interrogatory { number: 6, asked: "Describe each communication between You and Cruller between 1 April 2025 and 14 April 2026 concerning the doughnut, the alleged agreement, or the soul of Cruller.", answer: Some("There were none. We talked over that hedge maybe forty times in the year — about the leaf blower, about a casserole, about his rain gutters twice. The doughnut never came up. I assumed he had eaten it."), objections: None },
    Interrogatory { number: 7, asked: "Identify each document in Your possession, custody, or control that reflects, refers to, or evidences the alleged agreement.", answer: Some("I do not have a contract. There is a box in the seaweedfs with papers from around then and anyone is welcome to look through it."), objections: Some("Answer by production of records: Pursuant to NRCP 33(d), Defendant will make available for inspection the contents of a box in his seaweedfs in which he retains correspondence, receipts, and church bulletins from the relevant period.") },
    Interrogatory { number: 8, asked: "State each fact upon which You base the contention that describing the doughnut as \"neat\" conveyed every term of the alleged agreement.", answer: None, objections: Some("Premature contention interrogatory: The interrogatory seeks Defendant’s contentions before the close of discovery and before any deposition has been taken. Defendant will respond at the time the court directs.") },
];

const INBOUND_INTERROGATORIES: &[&str] = &[
    "State each fact upon which You contend that consuming the remainder of the doughnut on 14 April 2026 did not affirm the agreement alleged in the Amended Complaint.",
    "State the date, time, and manner in which You first learned of the term alleged in Paragraph 8 of the Amended Complaint, and identify each person who communicated it to You.",
    "Itemize each element of damage You claim in this action, stating the amount claimed and the method by which each amount was computed.",
    "Identify each document in Your possession, custody, or control concerning the doughnut or the alleged agreement, including any notebook, diary, or journal.",
    "State whether You consumed any portion of the doughnut with knowledge that Prine expected anything in return, and if so, identify what You understood was expected.",
    "State each fact upon which You base the allegation that a writing was contained within the doughnut, and identify each person who observed that writing.",
    "Identify each person to whom You have stated that Your soul had been conveyed to Prine, and state what You said to each.",
    "State the substance of each communication between You and Neon Law concerning the alleged agreement, including the date of each communication and the advice given.",
];

const ESTATE_AUTHORITIES: &[EstateAuthority] = &[
    EstateAuthority { caption: "In re Estate of Borghi", citation: "167 Wash. 2d 480", year: 2009, court: "Supreme Court", holding: "Whether property is separate or community is fixed at acquisition, and an intestate death distributes it as it stands." },
    EstateAuthority { caption: "Olver v. Fowler", citation: "161 Wash. 2d 655", year: 2007, court: "Supreme Court", holding: "A committed intimate partner may take an equitable share of what the couple built, but never a spouse’s intestate share." },
    EstateAuthority { caption: "In re Estate of Little", citation: "106 Wash. 2d 269", year: 1986, court: "Supreme Court", holding: "RCW 11.04.035 limits half-blood kindred among collateral heirs only — the statute the residue would fall to here." },
    EstateAuthority { caption: "Gonzales v. Cowen", citation: "76 Wash. App. 277", year: 1994, court: "Court of Appeals", holding: "An intestate with no issue passes to surviving parents, and a parent may prove the relationship after the death." },
    EstateAuthority { caption: "Bentzen v. Demmons", citation: "68 Wash. App. 339", year: 1993, court: "Court of Appeals", holding: "A nephew took as sole heir by intestate succession, and an unmarried partner had to sue the estate for her share." },
    EstateAuthority { caption: "In re Binge's Estate", citation: "5 Wash. 2d 446", year: 1940, court: "Supreme Court", holding: "An intestate without issue: the widow’s community half, the separate residue, and the homestead award ahead of the heirs." },
    EstateAuthority { caption: "France v. Freeze", citation: "4 Wash. 2d 120", year: 1940, court: "Supreme Court", holding: "Title vests in the heirs at the moment of death, and a decree of distribution binds nobody who was not before the court." },
    EstateAuthority { caption: "In re Graley's Estate", citation: "183 Wash. 268", year: 1935, court: "Supreme Court", holding: "“Leaving no heirs” means none has appeared and claimed; the State carries the burden before an estate escheats." },
    EstateAuthority { caption: "In re Lyons' Estate", citation: "175 Wash. 115", year: 1933, court: "Supreme Court", holding: "Escheat follows the situs of the property, but the law of the decedent’s domicil distributes the personalty." },
    EstateAuthority { caption: "State ex rel. Cowley v. Superior Court", citation: "158 Wash. 546", year: 1930, court: "Supreme Court", holding: "“Next of kin” entitled to administer means those who would inherit — the right to administer follows the property." },
    EstateAuthority { caption: "In re Estate of Fields", citation: "141 Wash. 526", year: 1927, court: "Supreme Court", holding: "No issue, no parents, no surviving siblings: what nieces and nephews take by representation, and what they do not." },
];

/// The general reference shelf: real law that answers nothing the case
/// turns on.
const LIBRARY: LitAuthority = LitAuthority {
    cite: "State v. Morros, 104 Nev. 709, 766 P.2d 263 (1988)",
    year: "1988",
    leaning: "neutral",
    quote: "Accordingly, we conclude that no absolute diversion requirement precludes the \
            granting of an in situ water right to the water of Blue Lake.",
    application: "Nevada water law, not contract or rescission. A general reference, not an \
                  authority for this matter.",
    url: "https://www.courtlistener.com/opinion/1349617/state-v-morros/",
};

/// Write the rows `project`'s declared pages read, then, when a source is
/// staged for it, its pages and Drafts.
pub(super) async fn seed(
    surreal: &SurrealDb,
    storage: &Arc<dyn cloud::StorageService>,
    project: &Project,
    lawyer_id: Uuid,
    jurisdiction_id: Uuid,
    get: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<()> {
    match project.code.as_str() {
        SAMPLE_LITIGATION_CODE => litigation(surreal, project, jurisdiction_id).await?,
        SAMPLE_ESTATE_CODE => estate(surreal, project).await?,
        _ => {}
    }
    if let Some(root) = crate::sample_project::staged_source_dir(&project.code, get) {
        let source = crate::sample_project::read_staged_source(&root, &project.code)
            .with_context(|| format!("staged source for `{}`", project.code))?;
        staged(surreal, storage, project, lawyer_id, &source).await?;
    }
    Ok(())
}

async fn litigation(
    surreal: &SurrealDb,
    project: &Project,
    jurisdiction_id: Uuid,
) -> anyhow::Result<()> {
    if crate::cases::for_project(surreal, project.id)
        .await?
        .is_empty()
    {
        let case = crate::cases::open_case(
            surreal,
            &crate::cases::NewCase {
                project_id: project.id,
                caption: "Cruller v. Prine",
                forum: Some("District Court, Clark County, Nevada"),
                jurisdiction: Some("Nevada"),
                docket_number: Some("A-26-874219-C"),
                judge: Some("Hon. Marisol T. Quell"),
                posture: "Plaintiff",
            },
        )
        .await?;
        let mut served = Vec::new();
        for (index, entry) in DOCKET.iter().enumerate() {
            let number = (index + 1).to_string();
            let recorded = crate::cases::record_entry(
                surreal,
                &crate::cases::NewDocketEntry {
                    case_id: case.id,
                    entry_number: &number,
                    kind: entry.kind,
                    title: entry.title,
                    party: entry.party,
                    filed_or_served_on: Some(entry.on),
                    scheduled_on: None,
                    supersedes: None,
                    document_asset_id: None,
                    notation_id: None,
                },
            )
            .await?;
            if entry.kind == EntryKind::DiscoveryRequest {
                served.push(recorded.id);
            }
        }
        discovery(surreal, case.id, &served).await?;
    }
    for ahead in DEADLINES {
        crate::project_deadlines::create(
            surreal,
            &crate::project_deadlines::NewDeadline {
                project_id: project.id,
                title: None,
                description: None,
                due_at: ahead.due_at.to_string(),
                authority_kind: ahead.authority_kind.to_string(),
                authority: ahead.authority.to_string(),
                source: "lawyer".to_string(),
                source_document_id: None,
                owner_person_id: None,
                calendar_role: ahead.calendar_role.to_string(),
                internal_lead_days: Some(ahead.client_lead_days + 7),
                client_lead_days: Some(ahead.client_lead_days),
                replay_key: Some(format!("{SAMPLE_LITIGATION_CODE}:{}", ahead.key)),
            },
        )
        .await?;
    }
    for (transaction_id, filed_on, note) in FILINGS {
        let filed_on = NaiveDate::parse_from_str(filed_on, "%Y-%m-%d")?.to_string();
        crate::efiling_transactions::record(
            surreal,
            &crate::efiling_transactions::EfilingReport {
                project_id: project.id,
                transaction_id: (*transaction_id).to_string(),
                status: crate::efiling_transactions::STATUS_ACCEPTED.to_string(),
                efsp: None,
                court: Some("District Court, Clark County, Nevada".to_string()),
                case_number: Some("A-26-874219-C".to_string()),
                submitted_document_id: None,
                filed_on: Some(filed_on.clone()),
                accepted_on: Some(filed_on),
                conformed_document_id: None,
                needs_entered_order: None,
                note: Some((*note).to_string()),
            },
        )
        .await?;
    }
    adverse_party(surreal, project, jurisdiction_id).await?;
    for authority in LITIGATION_AUTHORITIES {
        lit_authority(surreal, project, authority, Disposition::ReliedOn).await?;
    }
    lit_authority(surreal, project, &LIBRARY, Disposition::ReviewedNotUsed).await?;
    Ok(())
}

/// The two interrogatory sets: ours with the defendant's responses, and
/// theirs with our answers still due.
async fn discovery(surreal: &SurrealDb, case_id: Uuid, served: &[Uuid]) -> anyhow::Result<()> {
    let outbound = crate::cases::serve_discovery(
        surreal,
        &crate::cases::NewDiscoveryRequest {
            case_id,
            docket_entry_id: served.first().copied(),
            device: Device::Interrogatories,
            set_number: 1,
            propounding_party: PLAINTIFF,
            responding_party: DEFENDANT,
            served_on: Some("2026-06-25"),
            responses_due_on: Some("2026-07-27"),
        },
    )
    .await?;
    for rog in OUTBOUND_INTERROGATORIES {
        let item = crate::cases::add_item(surreal, outbound.id, rog.number, rog.asked).await?;
        if rog.answer.is_some() || rog.objections.is_some() {
            crate::cases::answer_item(
                surreal,
                item.id,
                rog.answer.unwrap_or_default(),
                rog.objections,
            )
            .await?;
        }
    }
    let inbound = crate::cases::serve_discovery(
        surreal,
        &crate::cases::NewDiscoveryRequest {
            case_id,
            docket_entry_id: served.get(1).copied(),
            device: Device::Interrogatories,
            // A case numbers its interrogatory sets once across both sides,
            // so the defendant's first set is the case's second.
            set_number: 2,
            propounding_party: DEFENDANT,
            responding_party: PLAINTIFF,
            served_on: Some("2026-08-14"),
            responses_due_on: Some("2026-09-14"),
        },
    )
    .await?;
    for (index, asked) in INBOUND_INTERROGATORIES.iter().enumerate() {
        let number = i32::try_from(index + 1)?;
        crate::cases::add_item(surreal, inbound.id, number, asked).await?;
    }
    Ok(())
}

/// The defendant, as the party the client is adverse to: the edge the
/// party roster and the relationship graph read.
async fn adverse_party(
    surreal: &SurrealDb,
    project: &Project,
    jurisdiction_id: Uuid,
) -> anyhow::Result<()> {
    let human = crate::entity_types::find_by_name(surreal, "Human")
        .await?
        .context("seed: entity_type `Human` must be seeded first")?;
    let mut report = super::SeedReport::default();
    let prine = super::ensure_dev_entity(
        surreal,
        &mut report,
        "Wendell Prine",
        human.id,
        jurisdiction_id,
    )
    .await?;
    crate::project_references::reference(
        surreal,
        project.id,
        crate::project_references::Party::entity(prine),
        crate::project_references::REASON_ADVERSE_PARTY,
    )
    .await?;
    Ok(())
}

/// Record one Authority, its use on the matter, and its verbatim quote.
async fn lit_authority(
    surreal: &SurrealDb,
    project: &Project,
    authority: &LitAuthority,
    disposition: Disposition,
) -> anyhow::Result<()> {
    let class = if authority.cite.starts_with("Nev. Rev. Stat.") {
        AuthorityClass::Statute
    } else {
        AuthorityClass::CaseLaw
    };
    let position = match authority.leaning {
        "dermot" => "ours",
        "prine" => "adverse",
        _ => "neutral",
    };
    let issued_on = authority.year.parse::<u16>().ok().map(|y| y.to_string());
    let row = crate::authorities::record(
        surreal,
        &crate::authorities::NewAuthority {
            class,
            citation: authority.cite,
            short_cite: None,
            title: authority.cite,
            publisher: None,
            issued_on: issued_on.as_deref(),
            canonical_url: Some(authority.url),
            checked_on: None,
            archived_asset_id: None,
        },
    )
    .await?;
    let authority_use = crate::authorities::cite_in_matter(
        surreal,
        project.id,
        row.id,
        position,
        disposition,
        None,
    )
    .await?;
    if crate::authorities::citations_for_use(surreal, authority_use.id)
        .await?
        .is_empty()
    {
        crate::authorities::cite(
            surreal,
            &crate::authorities::NewCitation {
                authority_use_id: authority_use.id,
                quote: authority.quote,
                why: authority.application,
                source_pin: None,
                draft_pin: None,
            },
        )
        .await?;
    }
    Ok(())
}

/// The estate plan's research: the intestacy cases the plan exists to
/// avoid. No treatment has been verified, so each use is open for review.
async fn estate(surreal: &SurrealDb, project: &Project) -> anyhow::Result<()> {
    for authority in ESTATE_AUTHORITIES {
        let citation = format!(
            "{}, {} ({})",
            authority.caption, authority.citation, authority.year
        );
        let publisher = format!("Washington {}", authority.court);
        let year = authority.year.to_string();
        let row = crate::authorities::record(
            surreal,
            &crate::authorities::NewAuthority {
                class: AuthorityClass::CaseLaw,
                citation: &citation,
                short_cite: Some(authority.caption),
                title: authority.caption,
                publisher: Some(&publisher),
                issued_on: Some(&year),
                canonical_url: None,
                checked_on: None,
                archived_asset_id: None,
            },
        )
        .await?;
        crate::authorities::cite_in_matter(
            surreal,
            project.id,
            row.id,
            "neutral",
            Disposition::OpenReview,
            Some(authority.holding),
        )
        .await?;
    }
    Ok(())
}

/// The Drafts each sample matter has opened a Notation on: its engagement,
/// and the instruments its former Portal listed. Every staged Draft is
/// persisted; only these are activated.
const NOTATION_DRAFTS: &[(&str, &str)] = &[
    (SAMPLE_LITIGATION_CODE, "sample_litigation__engagement"),
    (
        SAMPLE_LITIGATION_CODE,
        "motion_partial_summary_judgment__nevada",
    ),
    (SAMPLE_ESTATE_CODE, "sample_estate__engagement"),
    (SAMPLE_ESTATE_CODE, "last_will_and_testament"),
    (SAMPLE_ESTATE_CODE, "revocable_living_trust"),
    (SAMPLE_ESTATE_CODE, "health_care_proxy"),
    (SAMPLE_ESTATE_CODE, "statutory_financial_power_of_attorney"),
    (
        SAMPLE_TRANSACTIONAL_CODE,
        "sample_transactional__engagement",
    ),
];

/// The Draft whose Notation is signed, with the date it was signed: the
/// sample litigation's engagement letter.
const SIGNED_DRAFTS: &[(&str, &str, &str)] = &[(
    SAMPLE_LITIGATION_CODE,
    "sample_litigation__engagement",
    "2026-04-20T00:00:00Z",
)];

/// Store a staged source's pages, persist its Drafts, and open a Notation
/// on each of [`NOTATION_DRAFTS`].
async fn staged(
    surreal: &SurrealDb,
    storage: &Arc<dyn cloud::StorageService>,
    project: &Project,
    lawyer_id: Uuid,
    source: &crate::sample_project::StagedSource,
) -> anyhow::Result<()> {
    crate::matter_pages::replace_for_project(surreal, project.id, &source.pages)
        .await
        .with_context(|| format!("store the pages `{}` declares", project.code))?;
    let commit = source
        .commit
        .clone()
        .unwrap_or_else(|| "unrecorded".to_string());
    for (code, contents) in &source.drafts {
        let persisted = match crate::template_source::persist_contents(
            surreal,
            storage,
            project.id,
            code,
            contents.clone(),
            commit.clone(),
        )
        .await
        {
            Ok(persisted) => persisted,
            Err(error) => {
                tracing::warn!(project = %project.code, draft = %code, %error, "seed: staged Draft refused");
                continue;
            }
        };
        let activated = NOTATION_DRAFTS
            .iter()
            .any(|(matter, draft)| *matter == project.code && draft == code);
        if !activated {
            continue;
        }
        let notation = match crate::notations::find_by_project_template_person(
            surreal,
            project.id,
            persisted.template.id,
            lawyer_id,
        )
        .await?
        {
            Some(notation) => notation,
            None => {
                crate::notations::create(
                    surreal,
                    &crate::notations::NewNotation::new(
                        persisted.template.id,
                        lawyer_id,
                        project.id,
                        "BEGIN",
                    ),
                )
                .await?
            }
        };
        if let Some((_, _, signed_at)) = SIGNED_DRAFTS
            .iter()
            .find(|(matter, draft, _)| *matter == project.code && draft == code)
        {
            sign(
                surreal,
                notation.id,
                &format!("{}-{code}", project.code),
                signed_at,
            )
            .await?;
        }
    }
    Ok(())
}

/// Record a completed signature on `notation_id` and close the Notation.
async fn sign(
    surreal: &SurrealDb,
    notation_id: Uuid,
    envelope: &str,
    signed_at: &str,
) -> anyhow::Result<()> {
    use crate::signatures::SignatureProvider;
    crate::signatures::record_request(surreal, notation_id, SignatureProvider::DocuSign, envelope)
        .await?;
    crate::signatures::stamp_signed(surreal, SignatureProvider::DocuSign, envelope, signed_at)
        .await?;
    crate::notations::update_state(surreal, notation_id, "END").await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{mem_surreal, seed_entity};

    /// A Draft the default rules accept, under the sample engagement's code.
    const ENGAGEMENT: &str =
        include_str!("../../../templates/notations/neon_law/onboarding__letter.md");

    async fn project(surreal: &SurrealDb, code: &str) -> Project {
        crate::projects::create(
            surreal,
            &crate::projects::NewProject {
                code: code.into(),
                name: code.into(),
                status: "open".into(),
                entity_id: seed_entity(surreal).await,
                ..Default::default()
            },
        )
        .await
        .expect("project")
    }

    fn write(root: &std::path::Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }

    /// The canonical seed, then two passes of [`seed`] over the litigation
    /// and estate matters with a litigation source staged.
    async fn seeded() -> (SurrealDb, Project, Project, tempfile::TempDir) {
        let surreal = mem_surreal().await;
        let blobs = tempfile::tempdir().expect("tempdir");
        let storage: Arc<dyn cloud::StorageService> = Arc::new(
            cloud::FsStorage::new(blobs.path().to_path_buf())
                .await
                .expect("storage"),
        );
        super::super::seed_canonical(&surreal, &storage)
            .await
            .expect("canonical seed");
        let nevada = crate::jurisdictions::find_by_name(&surreal, "Nevada")
            .await
            .expect("lookup")
            .expect("Nevada");
        let lawyer = crate::persons::create(
            &surreal,
            &crate::persons::NewPerson::with_role(
                "Sample Lawyer",
                "sample-records-lawyer@example.com",
                crate::persons::Role::Lawyer,
            ),
        )
        .await
        .expect("lawyer")
        .id;
        let litigation = project(&surreal, SAMPLE_LITIGATION_CODE).await;
        let estate = project(&surreal, SAMPLE_ESTATE_CODE).await;

        let stage = tempfile::tempdir().expect("tempdir");
        let root = stage.path().join(SAMPLE_LITIGATION_CODE);
        write(
            &root,
            "navigator.yaml",
            "project:\n  host: h.example\n  name: sample-litigation\npages:\n  - slug: timeline\n    \
             kind: docket_deadline_board\n    title: Case timeline\n    lenses:\n      lawyer: \
             [docket_timeline, deadline_board, boundary_note, provenance_statement]\n      client: \
             [docket_timeline, deadline_board, boundary_note, provenance_statement]\n",
        );
        write(&root, "drafts/sample_litigation__engagement.md", ENGAGEMENT);
        write(&root, "drafts/unlisted_memo.md", ENGAGEMENT);
        write(&root, "source-commit", "abc123\n");
        let staged_root = stage.path().display().to_string();
        let get =
            |key: &str| (key == crate::sample_project::STAGE_ENV).then(|| staged_root.clone());

        for _ in 0..2 {
            for matter in [&litigation, &estate] {
                seed(&surreal, &storage, matter, lawyer, nevada.id, get)
                    .await
                    .expect("seed");
            }
        }

        (surreal, litigation, estate, blobs)
    }

    /// Two passes over the sample matters write each row once: the litigation
    /// docket mirrors the case timeline, both interrogatory sets carry their
    /// items, the Deadlines and filings and Authorities are there, and the
    /// staged source's page and signed engagement Notation are stored.
    #[tokio::test]
    async fn the_sample_records_seed_once_and_carry_the_staged_source() {
        let (surreal, litigation, estate, _blobs) = seeded().await;
        let docket = crate::cases::docket_for_project(&surreal, litigation.id)
            .await
            .expect("docket");
        assert_eq!(docket.len(), DOCKET.len());
        assert_eq!(
            docket.len() + DEADLINES.len(),
            19,
            "the timeline's 19 entries"
        );
        let case = &crate::cases::for_project(&surreal, litigation.id)
            .await
            .expect("cases")[0];
        let sets = crate::cases::discovery_requests(&surreal, case.id)
            .await
            .expect("sets");
        assert_eq!(sets.len(), 2);
        for set in &sets {
            let items = crate::cases::items(&surreal, set.id).await.expect("items");
            assert_eq!(items.len(), 8);
        }
        assert_eq!(
            crate::project_deadlines::list(&surreal, litigation.id)
                .await
                .expect("deadlines")
                .len(),
            DEADLINES.len()
        );
        assert_eq!(
            crate::efiling_transactions::list(&surreal, litigation.id)
                .await
                .expect("filings")
                .len(),
            FILINGS.len()
        );
        assert_eq!(
            crate::authorities::uses_for_project(&surreal, litigation.id)
                .await
                .expect("uses")
                .len(),
            LITIGATION_AUTHORITIES.len() + 1
        );
        assert_eq!(
            crate::authorities::uses_for_project(&surreal, estate.id)
                .await
                .expect("uses")
                .len(),
            ESTATE_AUTHORITIES.len()
        );
        assert_eq!(
            crate::project_references::for_project(&surreal, litigation.id)
                .await
                .expect("references")
                .len(),
            1
        );

        let pages = crate::matter_pages::list_for_project(&surreal, litigation.id)
            .await
            .expect("pages");
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].entry.slug, "timeline");
        let notations = crate::notations::list_by_project(&surreal, litigation.id)
            .await
            .expect("notations");
        assert_eq!(notations.len(), 1, "the staged engagement is activated");
        assert_eq!(notations[0].state, "END");
        assert!(
            crate::templates::resolve_exact(&surreal, Some(litigation.id), "unlisted_memo")
                .await
                .expect("lookup")
                .is_some(),
            "every staged Draft is persisted, activated or not"
        );
        assert_eq!(
            notations[0].template_source_commit_sha.as_deref(),
            Some("abc123"),
            "the Notation pins the staged commit"
        );
        let signature = crate::signatures::latest_for_notation(&surreal, notations[0].id)
            .await
            .expect("signature")
            .expect("a signature");
        assert_eq!(
            signature.state,
            crate::signatures::SignatureState::Completed
        );
        assert!(crate::matter_pages::list_for_project(&surreal, estate.id)
            .await
            .expect("pages")
            .is_empty());
    }
}
