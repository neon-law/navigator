---
title: "Durable Object Handover"
description: "A Durable Object Handover records how PostgreSQL acquired workflow ownership."
---

A Durable Object Handover records how PostgreSQL acquired ownership of Notation and Project Slack workflows. The receipt
identifies fresh database provisioning or a cutover that left retained source state behind. Historical receipts may also
contain inventory counts and a digest. The receipt remains independent of individual Project deletion.

The table is `durable_object_handover`.

Schema: [`durable_object_handover`](../../store/postgres/schema.sql).
