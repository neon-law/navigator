---
title: "Journal Posting"
description: "A Journal Posting is one exact account movement within a balanced Journal Entry."
---

A Journal Posting carries an account, asset unit, signed integer quantity, category, beneficial ownership, restrictions,
and optional Project allocation. Client and Firm ownership remain distinct even for the same asset type. Historical
unspecified ownership stays explicit; new postings require reviewed ownership. Postings balance separately for each unit
within their [Journal Entry](journal-entry.md). Their records live in `journal_posting`. A Project allocation does not
change which Firm owns the entry.
