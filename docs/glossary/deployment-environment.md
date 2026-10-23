---
title: "Deployment Environment"
description: "A Deployment Environment is the infrastructure profile selected by NAVIGATOR_ENVIRONMENT."
---

The infrastructure profile selected by `NAVIGATOR_ENVIRONMENT`. Exact `dev` serves local KIND; exact `production`,
empty, or unset serves production. The parser reports every other value as an error. `NAVIGATOR_CI_HARNESS` adds fake
providers to the `dev` profile for automated tests.

Every profile applies the canonical seed and the [Brand Seed](brand-seed.md). Whether the [Sample Matter
Fixture](sample-matter-fixture.md) is applied on top is a *separate* selector, `NAVIGATOR_SIMULATED_MATTERS`, which
defaults to following this one and can be set explicitly either way. The combination that needs the second selector is
the persistent staging deployment: it runs the `production` profile deliberately, so nothing in the process could
otherwise tell it apart from the deployment holding real matters. Slack from that row ends with `from Staging`.

`NAVIGATOR_STAGING` identifies staging independently of its data. Set it to exact `true` on staging: social publishing
and replies are denied even when simulated matters are disabled. Unset preserves existing behavior; exact `false` still
requires all existing production and real-matter checks. Any other value, including empty, denies social writes. This
flag does not seed simulated Projects, change existing keys, or replicate Posts. The deployment renderer passes it to
both the web application and publisher, defaulting to `false` when omitted from the deployment configuration.
