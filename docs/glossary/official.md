---
title: "Official"
description: >-
  Official is the onboarding reading of a matter whose onboarding document completed a signature loop Navigator sent.
---

The [Onboarding](onboarding.md) reading of a matter whose onboarding notation has a completed envelope on a signature
loop [Navigator](matter.md) itself sent — every recipient signed, recorded by the [Signature
provider](fake-signature-provider.md). It is the `official` value of the closed enum
[`OnboardingStatus`](../../store/src/projects.rs); every other reading — nothing sent yet, an envelope still
outstanding, one declined — is [Pitch](pitch.md).

Official is additive to the matter's lifecycle, which keeps `pitch`/`active`/`closed` — a matter can be active and still
a pitch. The reading surfaces on the matter page, through `navigator project lifecycle`, and in the client portal, which
reads [Pitch](pitch.md) until onboarding completes.
