---
title: Call Script
description: "What a caller hears on a Brand Phone Number: a greeting, the extensions that forward, and a fallback."
---

A **Call Script** is what a caller hears when a [Brand Phone Number](brand-phone-number.md) picks up: an opening
greeting, one extension per person who takes calls, and the fallback message everyone else hears before the line hangs
up. An extension is the digits a caller keys, ended with the pound key, the name the greeting speaks, and the phone the
call forwards to. A forwarded call carries the caller's own number as caller ID.

The number's Twilio Studio Flow runs the script. `hudson ops call-script` renders the Flow from flags, compares it with
the published one, and publishes a new revision only with `--apply`. A forwarding destination is a person's own phone:
it is an operator's flag, never a value in this repository, and the command does not echo it.

- [`deploy/src/devx/call_script.rs`](../../deploy/src/devx/call_script.rs) ·
  [Twilio integration](../third-party-integrations.md)
