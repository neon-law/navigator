---
title: "Official"
description: >-
  Official means Navigator holds the onboarding letter's DocuSign envelope UUID and verified its completion.
---

A [Project](project.md) is official when its [Onboarding](onboarding.md) letter was executed through the owning Firm's
DocuSign account and Navigator holds that envelope's UUID. Navigator verifies that the envelope is completed. The
envelope may originate in Navigator or be adopted from the Firm's DocuSign web application.

An open official Project reads `active`. Until execution is verified, it reads [Pitch](pitch.md). A filed PDF, client
visibility, or signing-method declaration alone provides no execution evidence.

The reading surfaces on the matter page, through `navigator project status` and `navigator project lifecycle`, and in
batched lifecycle reads.

For an uploaded onboarding document, the server stamps `verified_onboarding_envelope_id` in Asset metadata after
checking completion, alongside `docusign_envelope_id`. Upload callers cannot set either evidence key through free-form
metadata. The pointer carries `docusign_envelope_id`; editing that pointer alone supplies no provider verification.
