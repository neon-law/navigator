---
title: "Official"
description: >-
  Official means Navigator holds verified evidence that the Onboarding letter was executed.
---

A [Project](project.md) is official when Navigator holds verified execution of its [Onboarding](onboarding.md) letter.
Navigator checks the completed DocuSign envelope through the owning Firm's account. For a manually exported Dropbox Sign
retainer, the lawyer verifies the request and audit trail against the current filed revision and records that
verification in Navigator. See the [import steps][import].

An open official Project reads `active`. Until execution is verified, it reads [Pitch](pitch.md). A filed PDF, client
visibility, or signing-method declaration alone provides no execution evidence.

The matter page, `navigator project status`, `navigator project lifecycle`, and batched lifecycle reads use the same
execution evidence.

DocuSign upload verification is server-stamped in Asset metadata. Verification of a Dropbox Sign import is recorded in
`onboarding_signature_evidence` against the Firm, Project, and exact document revision.

[import]: ../project-repositories.md#import-a-completed-clio-or-dropbox-sign-retainer
