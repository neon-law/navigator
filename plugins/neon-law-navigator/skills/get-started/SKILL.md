---
name: get-started
description: Find public Neon Law Navigator templates and open signed-in intake for a Nevada LLC or business agreement.
---

When someone runs setup, call `list_templates`. Show up to three actual titles, prioritizing Nevada LLC formation, an
LLC operating agreement, and a common business agreement when available. Offer a start link for each supported choice
and [Open my Projects](https://www.neonlaw.com/app/projects).

For a specific request, show the matching choice immediately. Ask where the LLC is being formed when the state is
unknown. For an operating agreement, check the state, number and type of owners, and management structure. Offer the
Nevada single-member agreement only for a member-managed Nevada LLC owned by one individual.

Prefer the matching result’s `start_path`. Resolve a relative path beginning with `/notations/` against
`https://www.neonlaw.com` and show the result as a clickable link. For the public catalog that omits paths, the Nevada
LLC formation entry opens at [Start Nevada LLC intake](https://www.neonlaw.com/start/llc-file). The mutual NDA, advisor
agreement, and master services agreement entries open at [Start a mutual
NDA](https://www.neonlaw.com/notations/mutual-nda/start), [Start an advisor
agreement](https://www.neonlaw.com/notations/advisor-agreement/start), and [Start a master services
agreement](https://www.neonlaw.com/notations/master-services-agreement/start). Use these links only when the catalog
returns the matching entry. For unsupported agreements, offer [contact with Neon Law](https://www.neonlaw.com/contact).

Navigator handles sign-in and intake. The person chooses whether to open a Project on the website. Keep names, financial
details, and private matter facts in Navigator. Describe the step as starting intake. Formation is complete only when
the filing is confirmed. If the person asks for their Projects, give the Projects link directly.
