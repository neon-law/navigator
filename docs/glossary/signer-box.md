---
title: "Signer Box"
description: "A Signer Box is a questionnaire State the signer completes by hand on the printed document."
---

A questionnaire [State](state.md) declared `completed_by: signer`: a blank the signer fills in with a pen after the
document is printed, such as the initials box beside each power in a statutory power of attorney, a wet-ink date, or a
share count written at closing. It is referenced in the body by its ordinary `{{placeholder}}`; the source never carries
a `____`. The fill prints the answer when there is one and an empty rule when there is not, in `navigator notation pdf`,
`navigator notation word`, and `--preview` alike, and `rules::final_document` passes the rule while still refusing an
unanswered placeholder or a blank typed into the prose. Only a `custom_text`, `custom_datetime`, `custom_usd`, or
`custom_phone` state may be a Signer Box, because a record or reference answer is a row the questionnaire creates or
selects, never a line on paper. A `custom_text` Signer Box collects nothing at intake, so `N117`'s allowlist does not
hold it. See [`notation-authoring`](../notation-authoring.md#signer-boxes).
