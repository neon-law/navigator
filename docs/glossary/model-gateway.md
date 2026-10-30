---
title: "Model Gateway"
description: "The one contract every Legal Sandbox model backend answers, so metered usage is provider-independent."
---

The Model Gateway is the provider-agnostic seam the [Legal Sandbox](legal-sandbox.md) runs every model completion
through: one `ModelProvider` contract — a completion plus the price of its token usage in integer cents — that every
backend answers. The usage meter and the sandbox session build on the trait, never on a vendor, so the backend can
change without touching the product promise.

The first backend is Claude Sonnet on GCP Vertex AI; a deterministic in-process stub serves dev and tests (the `billing`
crate's stub pattern), and a zero-cloud local runtime is planned behind the same trait. The gateway renders the
session's [Sandbox Skill](sandbox-skill.md) instructions into the system context through the `sandbox-skills` catalog. A
request carries the sanitized conversation, the session's skills, and the tool catalog — no person identity;
sanitization is the session layer's job.
