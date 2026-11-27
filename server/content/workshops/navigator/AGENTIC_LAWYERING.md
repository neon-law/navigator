---
kind: workshop
title: "Agentic Lawyering"
description: Use Neon Law Navigator to guide an LLM to solve legal problems.
---

# Agentic Lawyering

A talk for the DBOS and cloud-native communities, and programmers in general. Three stories, in order: one matter, one
firm, and the people both of them are for. Presenter notes stay below each divider. Claims checked against the
repository November 26, 2026.

- I. One matter: an Oregon probate
- II. One firm: firm management
- III. The people it is for

## Neon Law Navigator

### What Navigator does

- MCP tools for agentic lawyering
- Per-Project repositories
- An omnichannel inbox for firm communications
- Matter Pages for every client

---

My name is Nick. Navigator is the open-source platform my firm runs on. Every day we ask, what can we automate today to
improve tomorrow?

### Standing on the Shoulders of Giants

- Rust
- Git/Jujutsu CI/CD
- Postgres with DBOS & Apache AGE
- Kubernetes

---

DBOS keeps durable workflows in the same Postgres that holds the matter. A step that crashes resumes from its last
completed step instead of starting over. Watch for that idea; it comes back on almost every slide.

## I. One matter: an Oregon probate

### An Oregon Probate

- Bigfoot passed away.
- One of their four children, Littlefoot, contacts our firm.
- Bigfoot had no will, what happens next?

---

Littlefoot was the primary caregiver and worries about what his siblings will inherit. What facts would a lawyer need
before advising him? Use synthetic answers and an unsigned draft. The packet's eligibility requirements need separate
review; caregiving alone does not establish inheritance rights.

[Oregon Judicial Department packet](https://www.courts.oregon.gov/forms/Documents/ProbateSimpleEstate.pdf)

### Intake

- Littlefoot's email lands in the inbox at `/app/inbox`.
- Navigator matches the sender to a Person.
- He books a consultation through the firm's booking link.
- With his consent, the recording becomes the transcript.

![The inbox, with Littlefoot's email at the top](img/agentic-lawyering/inbox-list.jpg)

---

Every channel arrives in one queue. Automation records who wrote and when; it never invents a relationship that a
conflict check relies on. The transcript stays on the Project's private Notion page.

### Littlefoot's email

![Littlefoot's email, waiting to be assigned to a Project](img/agentic-lawyering/inbox-email.jpg)

---

The message arrived through the Vesta address and is not yet assigned to a matter. Nobody can reply until it is assigned
to a Project with a participating lawyer or clerk.

### Creating the Project

```bash
navigator project create --name "Estate of Bigfoot" --code bigfoot-probate-or \
  --client-email littlefoot@example.com --jurisdiction Oregon --attest
```

- The code is permanent and names the repository.
- `--attest` records the conflict check; it is never defaulted.
- The client must already exist as a Person.

![The Estate of Bigfoot Project page, with its Matter Pages](img/agentic-lawyering/estate-of-bigfoot.jpg)

---

Creating a Project opens the matter, its repository, and its `navigator.yaml`. The lawyer attests that the conflict
check is clear; the command refuses to guess.

### Notations: onboarding

```yaml
kind: onboarding
workflow:
  BEGIN: intake_persisted__client
  intake_persisted__client: lawyer_review
  lawyer_review:
    approved: generate_pdf__retainer_pdf
    changes_requested: reask__client
  generate_pdf__retainer_pdf: sent_for_signature__pending
  sent_for_signature__pending:
    signature_received: END
```

---

A Notation is our computable document: Markdown with YAML frontmatter that declares the questions to ask and the
workflow to run. This is the shape of the onboarding letter. Every path to a signature runs through lawyer review.

### Notations: the court's PDF form

```yaml
kind: filing
output: form
origin_url: https://www.courts.oregon.gov/forms/Documents/ProbateSimpleEstate.pdf
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: person__decedent
```

---

The same format fills a court's own PDF. Our New York probate petition already works this way: each question maps to a
field on the official form. Oregon is not in the catalog yet. Bigfoot's matter is how the Oregon form notation gets
written, reviewed, and kept.

### Claude fills the questionnaire

```text
Using the consultation transcript, answer the onboarding questions for Littlefoot. Leave anything he did not tell us
unanswered.
```

```bash
navigator project onboard --delivery emailed --answers-from answers.yaml
```

---

Claude drafts the answers over Navigator's MCP tools; the CLI refuses a partial prefill. A missing fact is a question
for the lawyer, not a guess for the model. We use LLMs where they help and ordinary Rust for repeatable work, so it does
not need an LLM call.

### Sending the retainer (DBOS)

```bash
navigator notation envelope prepare <notation-id>
navigator notation envelope send <notation-id>
navigator notation envelope status <notation-id>
```

- Prepare a draft envelope and review the signing fields with the lawyer.
- Send only after the lawyer approves the document, recipients, and fields.
- DocuSign's completion webhook signals the notation's DBOS workflow.

---

Each notation state transition is its own durable DBOS workflow. When Littlefoot signs, the webhook delivers
`signature_received`, and Navigator files the signed PDF and the Certificate of Completion into the Project. A daily
DBOS reconciliation also finds onboarding that stalled.

### Filing with the court

```text
Prepare the Oregon probate packet as a draft notation from the facts Littlefoot gave us. List every fact we still need
and add the filing deadlines to the Project.
```

- `create_notation`, `answer_notation`, and `create_deadline` over MCP.
- `spawn_legal_council` reviews the draft before the lawyer does.
- The lawyer signs and submits; `navigator project efiling record` files the court's response.

---

The agent prepares; the lawyer files. The council is a drafting review, not an independent lawyer. Navigator keeps each
e-filing transaction and the court-stamped copy with the matter, so the next deadline comes from what the court actually
accepted.

### Google Calendar & Slack

- Every Deadline lands on the Project's calendar.
- Each open matter gets a private Slack channel.
- A nightly DBOS workflow repairs both.

![The deadlines on the Estate of Bigfoot](img/agentic-lawyering/bigfoot-deadlines.jpg)

---

`NightlyProjectUpdate` fans out one child workflow per open Project: membership, calendar, Slack, and repository. A
missed calendar write or a renamed channel is repaired the next night, and a crash halfway through the firm resumes
where it stopped. The DocuSign completion posts to the matter's channel too.

### Matter Pages

```yaml
pages:
  - slug: plan
    kind: estate_plan_console
  - slug: instruments
    kind: document_workbench
  - slug: engagement
    kind: engagement_billing_records
```

---

Each matter declares its Matter Pages in `navigator.yaml`. The lawyer and the client see different lenses on the same
record. Littlefoot signs in on the Brand he hired and sees his own matter, not a generic dashboard.

## II. One firm: firm management

### Daily reconciliation (DBOS)

Every day we publish a new version of Navigator. How do we update each repo?

- A nightly DBOS workflow compares each Project's `version:` pin to the running release.
- A stale pin gets a one-line pull request with auto-merge armed.
- A re-run finds its own pull request instead of opening another.

![A nightly version-bump pull request set to auto-merge](img/agentic-lawyering/forgejo-bump-pr.jpg)

---

Reconciliation, not orchestration: every night the system compares what is to what should be and closes the gap. The
same pattern repairs calendars, channels, membership, and onboarding.

### navigator.yaml

```yaml
version: 26.11.26
project:
  name: sample-estate
  host: staging.example.test
```

![The sample-estate repository on staging Forgejo](img/agentic-lawyering/forgejo-sample-estate.jpg)

---

This is the pin the nightly workflow bumps. The host selects the site; it does not by itself establish the Firm, Brand,
scope of representation, or access rights.

### Claude as CEO

- Every morning we ask Claude to be CEO for the day.
- It reads the firm's numbers and tells us where to post.
- The model writes around numbers it was given, never instead of them.

---

The durable half runs on DBOS: `CeoDigest` emails each Firm's Owners a weekly brief, and `CeoDigestAlert` posts on
weekdays when the Firm owes replies. That digest writes its prose with Gemini on Vertex. The morning conversation is
Claude. Each Firm's numbers stay with that Firm.

### Marketing

- Claude drafts the post; a person approves it.
- Blog Posts are scheduled as durable DBOS workflows.
- A replayed step never posts twice.

---

The social publisher journals each destination as a step and claims the one public write in the database first. If the
worker dies after Instagram accepted the post, the retry knows. Attorney advertising still gets human review.

### Convention over Configuration

- File & folder naming
- Jujutsu branches and pull requests
- Clause library & glossary

---

I learned this from Ruby on Rails: shared conventions help the next person find the work, and the next agent too.

### The more work we do, the better we get

- A reviewed notation becomes a Template.
- A corrected procedure becomes a Project Skill.
- A repeated step becomes Rust instead of a prompt.

---

Bigfoot's estate is the first Oregon probate; it should not be the hardest one. Each matter the lawyer reviews leaves
the next agent run a better place to start.

## III. The people it is for

### Our north star

Improving access to justice.

- Every hour we automate is an hour with a client.
- Lower cost means more people we can help.

---

The technology is the means. A grieving family deserves a lawyer who has time to listen.

### Empathy is the job

- Littlefoot is grieving, not filing.
- The agent drafts; the lawyer listens.
- Plain language, clear next steps, no surprises.

---

Nothing on the earlier slides matters if the client feels processed. We automate the paperwork so the conversation can
be human.

### Why I chose a cofounder

- [FILL IN]

---

[FILL IN — your reason in your own words.]

### Need a lawyer

[![Guieb & Shook](/public/brand/guieb-shook/logo.svg)](https://guiebshook.com)

[![Lex Tecnica](/public/brand/lex-tecnica/logo.svg)](https://lextecnica.com)

**Divorce:** Daybridge Divorce Law · [daybridgedivorce.com](https://daybridgedivorce.com)

**Estates & probate:** Vesta Estate Planning · [vestaestateplanning.com](https://vestaestateplanning.com)

Contact us to learn more. Attorney advertisement.

---

Guieb & Shook and Lex Tecnica both run on Navigator. Say where each one takes clients; Bigfoot's Oregon matter is
fictional and does not mean either firm handles Oregon probate.

## Star our repo

### github.com/neon-law/navigator

If it helps you build something, we’d appreciate a GitHub star.

[github.com/neon-law/navigator](https://github.com/neon-law/navigator)

[![Scan to open the Navigator repository](/public/brand/navigator-github-qr.svg)](https://github.com/neon-law/navigator)

---

I put this in the open so you can point your own agents at it. Browse the repo, borrow what fits, and leave a star.
