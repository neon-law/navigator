---
kind: workshop
title: "Navigator: open-source cloud-native services for justice"
description: Follow a fictional probate workflow and see how Navigator makes infrastructure decisions reusable.
---

# Navigator: open-source cloud-native services for justice

Cloud Native PDX · Portland, Oregon

## Sample Navigator

### What Navigator does

- MCP tools for agentic lawyering
- Project repositories, with Jujutsu for local matter work
- An omnichannel inbox for firm communications
- Private matter records and participation-scoped access

---

Navigator is an open-source platform for running a law firm. What can we automate today to save time tomorrow?

### Live demo — Oregon probate

- In this fictional example, Bigfoot died.
- One of their four children, Littlefoot, contacts our firm.

---

Littlefoot was the primary caregiver and worries about what his siblings will inherit. What facts would a lawyer need
before advising him? Use synthetic answers and an unsigned draft. The packet's eligibility requirements need separate
review; caregiving alone does not establish inheritance rights.

[Oregon Judicial Department packet](https://www.courts.oregon.gov/forms/Documents/ProbateSimpleEstate.pdf)

### Booking the appointment

- Review a synthetic message on `/app/inbox`.
- Use the firm's approved booking link.
- Obtain the required consent before recording a real meeting.
- Keep the transcript on the Project's approved private Notion page.

---

Show the information the fictional contact actually supplied. Do not imply that every connected service automatically
shares the matter's data.

### Intake & Retainer

Intake and Retainer are Notations

---

Our computable contract format uses Markdown with YAML frontmatter to declare required questions and workflow steps.

### Fill a notation questionnaire from a transcript

Use a synthetic transcript to prepare answers to the notation's questions. Leave missing facts unanswered for the lawyer
to resolve before deciding whether the Oregon procedure fits.

---

We use LLMs where they help. Smaller models can reduce cost and make more computing environments practical; choose a
model only after checking its capability and the provider's privacy terms for the task.

We build ordinary Rust automation for repeatable work so it does not need an LLM call.

### Convention over Configuration

- File & folder naming
- jj branch and pr
- Clause library & glossary

---

I learned this from Ruby on Rails: shared conventions help the next person find the work. Project repositories keep
document pointers; the legal-document bytes stay in Navigator-managed storage.

### Navigator, the CLI

`brew install neon-law/navigator/navigator`

---

The CLI exposes authorized operations against the selected Navigator site. Access still depends on the caller's role and
Project participation. Show a supported read command; do not imply that installing the CLI enables court filing.

### navigator.yaml

```yaml
version: 26.11.18
project:
  name: sample-estate
  host: staging.example.test
```

---

This illustrates the nested manifest shape. Keep the sample checkout's actual pinned CLI version and deployment host.
The host selects the site; it does not by itself establish the Firm, Brand, scope of representation, or access rights.

### Navigator, the MCP

```text
Draft correspondence to fictional client Littlefoot about the probate questions we still need answered.
```

- Prepare a first-pass email.
- Review it through the legal council lenses.
- Leave the draft for the lawyer to review and send.
- File correspondence through the matter's approved communication workflow.

---

Show the draft and the facts supporting it. The council is a drafting review, not an independent lawyer or a substitute
for the responsible lawyer's judgment. Keep the demonstration unsent.

## Please leave a star

### github.com/neon-law/navigator

Browse the repo. Borrow what fits.

---

Open the public repository and point to one convention the audience can inspect and reuse. Keep the fictional matter's
documents and private deployment configuration out of the public repository.

### Thank you

If it helps you build something, we’d appreciate a GitHub star.

[github.com/neon-law/navigator](https://github.com/neon-law/navigator)

[![Scan to open the Navigator repository](/public/brand/navigator-github-qr.svg)](https://github.com/neon-law/navigator)

---

I put this in the open so you can point your own agents at it. Ask one to find a pattern you can use in your own system.
If something we learned helps you build, that’s what I hope comes out of sharing it.
