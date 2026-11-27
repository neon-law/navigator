---
name: push-changes
description: >-
  Land a signed Jujutsu change through review on GitHub or Forgejo and verify deployment of the merge commit.
---

# Push changes

Follow the **Find the term first** guidance in `AGENTS.md` when naming Project concepts.

Review `jj status`, `jj log`, and `jj diff` in the Project checkout. Initialize an existing Git checkout with `navigator
jj setup` when needed. Fetch current main and rebase the intended change if main advanced. Resolve conflicts, check the
final signature, and run `navigator project gate`. Review every client-visible Matter Page and document Pointer before
delivery. Keep document bytes, private notes, and credentials outside the change.

For each outgoing change by the signed-in author, check for a Billing Annotation. If it is missing, ask the author for
the actual hours in tenths and the work date before pushing. After confirmation, add `Billing-Time: <hours> <YYYY-MM-DD>
<uuid>` as the final trailer on the signed change. Use a fresh UUID for each entry and keep the same UUID on retry.

Run `navigator jj push` from the checkout root. To select a particular change, use `--revision <change-id>`. The command
stages the signed-in author's explicit Billing Annotations for lawyer review and opens or resumes the review request on
the configured forge. Navigator follows the required checks, the merge, and the deployment run for the exact merge
commit. A configured secondary mirror must also synchronize.

Report the review URL, merge commit, deployment URL, and result. If interrupted, resume with the same change ID. Fix a
failed check on its existing change. A missing deployment, an unsuccessful job, or an unresolved mirror leaves the
delivery incomplete. Fetch the updated remote bookmarks after success. Preserve unrelated work in the checkout.
