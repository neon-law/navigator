# Licensing

Navigator's first-party code, CLI, build tooling, documentation, and original template material are licensed under `MIT
OR Apache-2.0`, at your option. [LICENSE](../LICENSE) states that choice. [LICENSE-MIT](../LICENSE-MIT) and
[LICENSE-APACHE-2](../LICENSE-APACHE-2) are the standard texts. [NOTICE](../NOTICE) preserves Neon Law IP LLC's
attribution and trademark information. Contributor copyright remains with its holders except where a separate signed
agreement provides otherwise.

Either license permits commercial use subject to its terms. Neither requires a separate Navigator production license or
a delayed conversion. Third-party code, assets, fonts, and government material keep their own terms; the root license
does not establish rights in material owned by others.

## Warranties and liability

Apache-2.0 section 7 provides the software and contributions as is, disclaims express and implied warranties, including
title, non-infringement, merchantability, and fitness for a particular purpose, and places suitability and use-risk
decisions with the recipient. Section 8 limits contributor liability subject to applicable law and written agreements.
Section 9 addresses support, warranties, and additional liability offered by a redistributor on its own behalf.

Those sections cover AI-assisted code on the same terms as other contributions. They do not promise immunity from
liability or displace applicable law. Keep the standard license intact rather than adding AI-specific restrictions to
it. Review, tests, provenance checks, and accurate notices reduce avoidable risk; they are not warranties.

The software and its generated text are not legal advice. Human review of AI-assisted changes is recommended, not
required for merge into `main`. A merge promises no review or verification. Legal work product needs review by an
appropriately licensed professional. Using the software alone creates no attorney-client relationship. Separate service
agreements are not amended by the source-code license.

## Integrating with third-party services

Calling a provider's public API from first-party code does not change that code's license. The code stays `MIT OR
Apache-2.0`, because the obligations of a license attach to what the project incorporates and distributes, not to the
services it talks to. That holds for Slack, Xero, Twilio, SendGrid, DocuSign, and for a credit and privacy monitoring
provider such as Array.

Three things keep it true:

1. **Write the client from the provider's public documentation.** Do not copy the provider's SDK, sample code,
   documentation, schema files, or assets into the repository, and do not vendor an SDK whose license forbids
   redistribution. Load a provider's own script from the provider at run time if one is unavoidable.
2. **Keep credentials and returned data out of the repository.** Each deployment supplies its own account and key and
   accepts the provider's terms. What a provider returns, such as a consumer report, is not project material.
3. **Read the provider's agreement before the integration lands.** A provider's terms bind whoever holds the account,
   not the source license. Some providers gate their documentation and require a contract first. Confirm that its terms
   permit an open-source client before merging, and record the answer in the integration's pull request.

Provider names and marks stay with their owners; see [THIRD-PARTY-NOTICES.txt](../THIRD-PARTY-NOTICES.txt).

## Contributions and copyright notices

[CONTRIBUTORS.md](../CONTRIBUTORS.md) explains the MIT OR Apache-2.0 contribution terms: contributors retain copyright,
and no separate CLA is required. Existing signed agreements remain separate. [CONTRIBUTING.md](../CONTRIBUTING.md)
records whether outside submissions are accepted.

AI agents must not invent copyright owners, years, assignments, or permissions. Contributors are encouraged to check
authority, identify copied third-party material, retain accurate notices, and flag uncertainty. These recommendations
are not a guarantee of review or correctness. Apache section 4 permits appropriate modification notices and requires
preservation of applicable notices; contributor ownership claims must reflect the actual rights rather than an agent's
guess.

## Third-party material

| Material | Repository evidence | Notice handling |
| --- | --- | --- |
| Rust dependencies | [THIRD-PARTY-NOTICES.txt](../THIRD-PARTY-NOTICES.txt) | Preserve each component's terms |
| SendGrid contract | [vendor license](../vendor/sendgrid/LICENSE) | Preserve Twilio's MIT notice |
| PDF Tinos and Noto Serif | `pdf/assets/fonts/*/OFL.txt` | Preserve font copyright and OFL terms |
| Other web font records | `server/public/fonts/*/OFL.txt` | Verify provenance and delivered notices |
| GORP Serif | [font record](../server/public/fonts/gorp-serif/LICENSE.txt) | Separate proprietary operator license |
| PostgreSQL image components | [image NOTICE](../images/postgres/NOTICE) | Preserve upstream component licenses |
| Government form material | `templates/notations/forms/` | Verify issuing-agency terms separately |

This grant does not clear unresolved ownership, contributor, font, asset, or dependency questions. A rights inventory
records evidence, not permission to relicense another owner's work. A published version's separate existing grants and
agreements are not rewritten by this document.

## Trademarks

NEON LAW, U.S. Reg. No. 6,325,650, is owned by Neon Law IP LLC. DELETEYOURDATA.COM and LAWYER SHOOK are marks of Shook
Law PC. Neither license grants general trademark permission; Apache-2.0 section 6 states that limit for the Apache
option. Configure a fork's branding through `views::brand_bundle`; copyright permission does not authorize presenting a
deployment as Neon Law.

## Distributed notices

Cargo member manifests inherit `MIT OR Apache-2.0` from the workspace. The editor extension declares it in its own
manifest. Published Navigator images carry that SPDX expression as an OCI label and stage LICENSE, LICENSE-MIT,
LICENSE-APACHE-2, and NOTICE beside their binaries. Release archives include those files, and `navigator --license`
prints them. `navigator --third-party-notices` prints the retained dependency notices.

Regenerate dependency notices when the dependency graph changes:

```bash
cargo fetch
cargo run -p cli -- ops notices
```

`navigator ops notices --check` verifies the retained file against the unpacked dependency sources. The command refuses
to publish notices when sources are missing; font and other non-Cargo notices require their own review.

For private vulnerability reporting, see [SECURITY.md](../SECURITY.md).
