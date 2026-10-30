---
title: "Email Receipt"
description: "An Email Receipt records authenticated inbound summary intake and keys its durable delivery workflow."
---

An Email Receipt identifies one inbound message within a deployment and receiving mailbox. Its raw digest makes
redelivery reuse the archive, letter, and workflow key. It exists before archiving finishes, so a pending receipt can
identify interrupted intake.

The receipt ID correlates operational telemetry without exposing the sender or message. `email_delivery.state` is the
authoritative Slack delivery status. Receipt lookups resolve that status when a delivery record exists; before delivery,
the receipt reports its initial `not_attempted` value.

- Store: [`store::email_receipts`](../../store/src/email_receipts.rs)
- Delivery: [`store::email_deliveries`](../../store/src/email_deliveries.rs)
- Operations: [Email pipeline](../observability.md#email-pipeline)

The table is `email_receipt`.

Schema: [`email_receipt`](../../store/src/schema/navigator.surql#L1441).
