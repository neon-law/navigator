---
title: "Durable Object Handover"
description: "A Durable Object Handover records the input binding for retained object state."
---

A Durable Object Handover records the version, digest and inventory counts of captured Notation machines and Project
Slack posting evidence. A captured-source record binds the supplied input; its digest alone does not prove source
identity, writer quiescence or permission to activate native execution.

A fresh-native record instead identifies initialization of an exclusively owned new database, with no captured source
state. Import and parity leave captured state inactive until the separate ownership checks succeed. The record remains
independent of individual Project deletion.

The table is `durable_object_handover`.

Schema: [`durable_object_handover`](../../store/postgres/schema.sql).
