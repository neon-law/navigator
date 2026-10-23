---
title: "Analysis"
description: "The analysis state waits for contract findings before the workflow continues."
---

The workflow prefix `analysis` is a system wait state for review-in matters: the web app performs the contract analysis,
persists the findings, and signals the workflow when `analysis_ready` is available. See
[`notation-authoring`](../notation-authoring.md#what-runs-after-intake--the-step-registry) and
[`workflows::step::STEP_PREFIXES`](../../workflows/src/step.rs).
