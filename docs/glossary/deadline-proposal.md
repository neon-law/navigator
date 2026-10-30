---
title: "Deadline Proposal"
description: "A Deadline Proposal is a cited date for a lawyer to review before recording a Deadline."
---

A date calculated from a trigger and a rule, labelled as a proposal until a lawyer confirms it. It is separate from the
stored [Deadline](deadline.md). Showing or changing a proposal never writes a Deadline.

The rule names the trigger, signed day offset, counting basis, authority kind, and a filed [Authority](authority.md) by
id. Navigator resolves the Authority's canonical citation and requires its archived bytes to exist. Calendar days mean
adding the signed offset to the trigger date without holiday, service-method, or time-of-day adjustments. A court-day or
holiday basis returns no computed date; the lawyer supplies the reviewed `due_on`.

Confirmation requires a stable `replay_key` and a lawyer-supplied `due_at` with an explicit offset whose local date
matches the reviewed date. The existing Deadline write stores that instant, the canonical citation, and a proposal audit
in its description containing the Authority id and trigger. That description is an audit reference, not a new Authority
relationship. Reusing the key updates the same Deadline only after another explicit confirmation, preserving its owner,
lead times, visibility, calendar role, source Document, source, status, and description notes.

The CLI is [`navigator project deadline propose`](../deadline-proposals.md). Rules and proposals containing matter facts
stay in private storage outside Git.
