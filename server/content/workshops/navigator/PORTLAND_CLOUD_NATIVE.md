---
kind: workshop
title: "Navigator: open-source cloud-native services for justice"
description: Follow a fictional probate workflow and see how Navigator makes infrastructure decisions reusable.
---

# Navigator: open-source cloud-native services for justice

Cloud Native PDX · Portland, Oregon

## A law firm as a software problem

### What Navigator does

- Intake before a matter opens
- Case management, documents, and Project repositories
- Communications and publishing
- An AI agent harness with tools and rules for working across those systems

---

Navigator is an open-source platform for running a law firm. We want lawyers to spend more time with clients by making
the repeatable work around a matter easier to do.

Think of the Project repository as the agent’s mise en place. Agents organize source material and prepare drafts.
Lawyers review the work and advise clients. Each Project has a Git repository. Jujutsu (`jj`) can work with Git
repositories, so it can be part of a contributor’s local workflow.

### Live demo — Oregon probate

- Open a fictional Vesta Estate Planning Project in staging.
- Fill out a questionnaire together and generate an unsigned draft Notation.
- Follow the draft into the Project for lawyer review.

---

Use fictional answers. Open the Oregon Judicial Department packet as the source for the example. Keep a completed
staging example ready for the walkthrough.

[Oregon Judicial Department packet](https://www.courts.oregon.gov/forms/Documents/ProbateSimpleEstate.pdf)

### Project access

- Add a demo participant.
- Open the Project from their session.
- Remove them and check access again.

---

Show the same Project repository on this machine and in staging Forgejo. Rehearse the questionnaire and access paths
from a fresh test session before sharing attendee links.

## The machine is made of decisions

### Follow the draft through the stack

- PostgreSQL holds durable application records.
- OpenTelemetry helps us trace work across services.
- Lawyers review the draft before it goes further.

---

Trace the probate example through the stack. Show what the trace can tell us without recording client answers.

### Rehearse a restore

- Keep each account's records identifiable and exportable.
- Consider storing backups in AWS for a service running in Google Cloud.
- Rehearse a restore and inventory the credentials and services it needs.

---

Use the cross-cloud backup as a design scenario. A backup gives us something to restore. A rehearsal tells us what else
the service needs to come back.

### Pass the decisions on

- Put the lessons in agent instructions, shared tools, and checks.
- Give the next agent a concrete example it can inspect.
- Give the next engineer a way to verify the result.

---

I spent years learning these decisions by doing the work. This repository is where I put them so the next person or
agent can start there. Show one rule from the demo and the check that would catch a change to it.

### Choose what to build

- Own the legal workflow and the records it produces.
- Use existing services for banking, payroll, accounting, invoicing, and social scheduling.
- Use the receiving email address or phone number to identify the brand. Let a person assign the Project.

---

If the staging Inbox is ready, show a synthetic message. The audience should see where it arrived and who decides which
Project it belongs to.

## Take something with you

### Point your agent at Navigator

Browse the repo. Borrow what fits.

If it helps you build something, I’d appreciate a GitHub star.

[github.com/neon-law/navigator](https://github.com/neon-law/navigator)

[![Scan to open the Navigator repository](/public/brand/navigator-github-qr.svg)](https://github.com/neon-law/navigator)

---

I put this in the open so you can point your own agents at it. Ask one to find a pattern you can use in your own system.
If something we learned helps you build, that’s what I hope comes out of sharing it.
