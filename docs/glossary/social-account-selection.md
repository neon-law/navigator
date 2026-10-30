---
title: "Social Account Selection"
description: "A Social Account Selection saves one Brand’s intended provider destination before authorization."
---

A Social Account Selection holds a Brand’s chosen provider account identity without credentials or publishing authority.
It survives cancelled authorization and provider failure. Saving another identity does not change an authorized [Social
Account](social-account.md) or a scheduled Post. Connect verifies the saved identity through provider consent.

- Persistence: `store::social_account_selections`

The table is `social_account_selection`.

Schema: [`social_account_selection`](../../store/src/schema/navigator.surql#L2650).
