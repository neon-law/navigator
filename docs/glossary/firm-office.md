---
title: "Firm Office"
description: >-
  A Firm Office is one office a Firm publishes for a state: its address, under the state it serves, with at most one
  marked as the Firm's default.
---

One office a [Firm](firm.md) publishes through its [Brand](brand.md): the street address, published under the state it
serves. The public footer lists every one. A notation fills `{{firm.office.address}}`, `{{firm.office.phone}}`, and
`{{firm.office.email}}` from the office in the template's `jurisdiction:`, and a pleading's [Matter
Caption](matter-caption.md) takes the same office for any counsel firm, address, telephone, or email it leaves out. A
jurisdiction where the Firm has no office takes its default office, and the render names the office it used.

- Shape: [`views::brand::FirmOffice`](../../views/src/brand.rs) · Resolver:
  [`cli::letterhead::office_for_project`](../../cli/src/letterhead.rs)

Firm Offices are compiled into the Brand registry and a deployment's brand bundle (`brand.firm_offices`); there is no
table.
