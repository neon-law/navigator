# Redistribution evidence audit

This is a repository evidence inventory, not a rights clearance or legal opinion. An entry marked declared records what
a file says; it does not establish ownership, enforceability, compatibility, or permission to relicense. Only repository
source and synthetic material were inspected. No client data or signed private instruments were used.

The detailed asset and package matrices below preserve evidence collected at baseline
`de21fae2fc3c12073750fcb0a0e85925c06ae650`. They are historical inventories, not current package-by-package clearance
evidence. The current source review below was checked against `origin/main` at
`4858d6da7284a5625e5931dc668e58655af1eb31` on 2026-10-21. Historical ownership assertions are not a current assignment
requirement. No signed private instruments or external clearance records were reviewed.

## Current publication-readiness snapshot

This source review records what the tree declares and what the automated checks prove. It does not establish title,
permission for third-party material, or publication clearance. Recheck the exact source commit and generated artifacts
at the release candidate.

First-party terms: the root [LICENSE](../../LICENSE) offers `MIT OR Apache-2.0`, at the recipient's option, and points
to the unmodified [MIT](../../LICENSE-MIT) and [Apache-2.0](../../LICENSE-APACHE-2) texts. The [NOTICE](../../NOTICE)
carries the same SPDX expression. The workspace manifest sets `license = "MIT OR Apache-2.0"` and every member crate
inherits it. The editor extension declares it, and the `neon`, `trigger`, and `workflows-service` images label it and
copy all three files. The [license test](../../cli/tests/license_of_record.rs) checks these declarations and paths. The
PostgreSQL image context carries its own Apache-2.0 [LICENSE](../../images/postgres/LICENSE) and
[NOTICE](../../images/postgres/NOTICE). The image workflow checks those files against the pinned source repository.
Owner and counsel must confirm that the recipe terms cover the image build context and its redistribution. The root
declaration applies to the rest of the repository.

Source tree: rule `P001`, the [public snapshot audit](../public-snapshot-audit.md), checks regular staged or committed
files for defined credentials, contact data, cloud coordinates, secret-file names, build paths, and selected image/PDF
metadata. The source audit does not establish ownership, permission, or completeness of release artifacts.
Per-deployment rows now live outside the repository. `deployments/` tracks only a synthetic example, and the [exclusion
manifest](../../.public-snapshot-exclude) has no entries. On the current review basis, P001 reports no image-metadata
findings for the brand PNGs at B1, B12, B17, B21, and B26. Their provenance remains unverified.

Snapshot: [`navigator ops public-snapshot`](../public-snapshot.md) builds a deterministic, parentless root commit from a
merged `main` commit. Record the candidate's commit SHA and tree hash with the release, not in this document.

### Unresolved clearance blockers

Publication waits on every item below. Each needs evidence or a verified replacement from its named owner. Record
private instruments and clearance references outside this repository.

| Area | Blocker | Owner |
| --- | --- | --- |
| Rights | Title to first-party source and chain into Neon Law IP LLC | Owner/counsel |
| Rights | Historical contribution grants reconciled to authors | Owner/counsel |
| Rights | Imported text in templates and catalog metadata | Owner |
| Rights | Government form material under each issuing agency's terms | Counsel |
| Rights | Confirm Apache-2.0 scope for the separately licensed PostgreSQL image context | Owner/counsel |
| Rights | SendGrid generator template provenance | Owner/counsel |
| Rights | Trademark permissions for each brand mark and QR artwork | Owner/counsel |
| Notice | Reconcile Cargo notices with the packages and native components in each shipped artifact | Release/counsel |
| Notice | NuGet license texts for the Word adapter graph | Release/counsel |
| Notice | Swagger UI 5.17.14 upstream license and NOTICE for CSS, bundle, preset, and favicons | Release |
| Notice | Pirata One: the tracked record has no full OFL text | Release |
| Notice | Tinos web WOFF2 files have no colocated OFL record | Release |
| Notice | Notices delivered inside the extension package and each image | Release |
| Asset | Ferris illustration and Plausible script provenance | Owner |
| Asset | Brand logos, marks, and illustrations B1 through B29 | Owner/counsel |
| Asset | Workshop screenshot B32 | Owner/counsel |
| Asset | Embedded assets in the synthetic seed PDF B33 | Owner |
| Asset | GORP Serif: proprietary; no binary tracked, operator licence required | Operator/counsel |
| Artifact | Each publication output compared with the snapshot tree and notices | Release |

The current `navigator ops notices --check` reports 1,369 registry packages and 595 distinct licence texts, and confirms
that `THIRD-PARTY-NOTICES.txt` matches the generator. This checks the generated source inventory; it does not show which
packages ship in an artifact or clear their licenses, native components, or notices. Publication outputs to compare are
the snapshot root commit, the CLI and LSP release archives, the editor extension package, and the `neon`, `trigger`,
`workflows-service`, and PostgreSQL images. A deployed operator asset needs its own check.

The [CI workflow](../../.github/workflows/ci.yml) pins Gitleaks and runs the PII scan against a digest-pinned Presidio
analyzer. These checks do not decide rights, verify signed agreements, prove every asset's source, or ensure every
generated artifact has been inspected. No private instruments or client material were reviewed.

Before a publication candidate is approved, rerun the checks under [Validation boundaries](#validation-boundaries) on
its exact commit and artifacts. Reconcile this inventory with every changed, generated, embedded, and packaged file, and
hold any affected distribution until its evidence is complete. Publishing stays a maintainer action after that evidence
is complete. Keep private instruments and operational coordinates out of this document.

## Evidence and status key

| Key | Repository evidence |
| --- | --- |
| R | [LICENSE](../../LICENSE), [NOTICE](../../NOTICE) |
| C | [CONTRIBUTING.md](../../CONTRIBUTING.md), [licensing guide](../licensing.md) |
| W | [Workspace manifest](../../Cargo.toml), member manifests listed below |
| L | [Cargo.lock](../../Cargo.lock), exact name and version in each row |
| N | [THIRD-PARTY-NOTICES.txt](../../THIRD-PARTY-NOTICES.txt), exact package heading or gap entry |
| D | [deny.toml](../../deny.toml), [notice generator](../../cli/src/notices.rs) |
| V | [VENDOR.toml](../../server/public/VENDOR.toml), pinned Swagger UI paths, versions, and hashes |
| I | [Image LICENSE](../../images/postgres/LICENSE), [image NOTICE](../../images/postgres/NOTICE) |

Unverified means the repository does not establish a complete redistribution path. Declared means a license record is
present but its applicability to the exact bytes and distribution still requires verification. Missing means the cited
inventory has no exact package entry. None of these statuses establishes rights to relicense third-party material.

Dual declared records the first-party `MIT OR Apache-2.0` declaration, at the recipient's option. It matches the current
declaration; title and grants remain unverified.

Notice codes: **R** retain the current root license, attribution, and notices; **M** retain the complete MIT copyright,
permission, and disclaimer; **F** retain the font's copyright and complete OFL 1.1 text, reserved names, and conditions;
**A** inspect upstream Apache license and NOTICE, preserve attribution and applicable notices, and mark modifications;
**D** preserve existing dependency notices and verify the exact upstream license obligations for the shipped artifact;
**U** obtain the applicable owner's terms and notices before redistribution. Codes describe evidence checks, not counsel
conclusions. An inbound license allowlist is not proof that notice delivery or outbound relicensing is complete.

## Ownership, grants, and package metadata

The rows in this section and the following asset matrices describe the historical evidence baseline named above. They
preserve unresolved findings that still require current, file-specific review; they do not imply those findings are
cleared.

| Item | Evidence | Redistribution status | Notice | Unresolved owner or counsel review |
| --- | --- | --- | --- | --- |
| First-party Rust, tooling, docs | R; W; C | Dual declared | R | IP owner: confirm title and authorized release |
| Contribution grants | E1 | Dual policy; grants unverified | R | Owner/counsel: reconcile authors to grants |
| Internal author work | E2 | Baseline assignment asserted | R | Counsel: verify chain into Neon Law IP LLC |
| External author work | E3 | Historical grants unverified | R | Owner: verify historical grant coverage |
| Templates and catalog metadata | E4 | Dual declared | R | Owner: verify authorship and imported text |
| Government form references | E5 | No grant claimed by Navigator | U | Counsel: verify each issuing agency's terms |
| PostgreSQL image build code | E6 | Apache-2.0 declared separately | A | Owner: reconcile first-party scope with R |
| PostgreSQL, AGE, pgvector, Debian | E7 | Upstream terms declared separately | D | Release/counsel |
| DBOS test application | E8 | MIT declared for test only | M | Release: verify actual copied or shipped bytes |
| VS Code extension | E9 | Dual declared | R | Release: include notices in extension package |
| Word adapter | E10 | No separate license field | R; D | Owner/release: confirm binary and NuGet scope |

The root LICENSE offered MIT or Apache-2.0 at the inventory baseline. NOTICE was informational; CONTRIBUTORS.md licensed
accepted contributions under both options without a separate CLA or copyright assignment. Existing signed agreements
remain separate. The historical assignment assertions recorded at the inventory baseline did not supply signed
instruments or a contribution-by-contribution chain of title. Git attribution is not an assignment instrument. Private
instrument review belongs with the owner and counsel outside this public document.

At the inventory baseline, every member manifest below declared `license.workspace = true`; W set `MIT OR Apache-2.0`
and `publish = false`. This records metadata, not legal title. The separate image declaration needed explicit scope
reconciliation before replacement.

| Item | Evidence | Redistribution status | Notice | Unresolved owner or counsel review |
| --- | --- | --- | --- | --- |
| archives | E11 | Dual declared | R | Owner: grants; release: notices |
| billing | [billing/Cargo.toml](../../billing/Cargo.toml) | Dual declared | R | Owner: grants; release: notices |
| billing-workflows | E12 | Dual declared | R | Owner: grants; release: notices |
| cli | [cli/Cargo.toml](../../cli/Cargo.toml) | Dual declared | R | Owner: grants; release: notices |
| cloud | [cloud/Cargo.toml](../../cloud/Cargo.toml) | Dual declared | R | Owner: grants; release: notices |
| features | E13 | Dual declared | R | Owner: grants; release: notices |
| forms | [forms/Cargo.toml](../../forms/Cargo.toml) | Dual declared | R | Owner: grants; release: notices |
| import | [import/Cargo.toml](../../import/Cargo.toml) | Dual declared | R | Owner: grants; release: notices |
| live-inquiry | E14 | Dual declared | R | Owner: grants; release: notices |
| lsp | [lsp/Cargo.toml](../../lsp/Cargo.toml) | Dual declared | R | Owner: grants; release: notices |
| mcp | [mcp/Cargo.toml](../../mcp/Cargo.toml) | Dual declared | R | Owner: grants; release: notices |
| neon | [neon/Cargo.toml](../../neon/Cargo.toml) | Dual declared | R | Owner: grants; release: notices |
| pdf | [pdf/Cargo.toml](../../pdf/Cargo.toml) | Dual declared | R | Owner: grants; release: notices |
| portal | [portal/Cargo.toml](../../portal/Cargo.toml) | Dual declared | R | Owner: grants; release: notices |
| repos | [repos/Cargo.toml](../../repos/Cargo.toml) | Dual declared | R | Owner: grants; release: notices |
| rules | [rules/Cargo.toml](../../rules/Cargo.toml) | Dual declared | R | Owner: grants; release: notices |
| server | [server/Cargo.toml](../../server/Cargo.toml) | Dual declared | R | Owner: grants; release: notices |
| store | [store/Cargo.toml](../../store/Cargo.toml) | Dual declared | R | Owner: grants; release: notices |
| telemetry | E15 | Dual declared | R | Owner: grants; release: notices |
| views | [views/Cargo.toml](../../views/Cargo.toml) | Dual declared | R | Owner: grants; release: notices |
| webapp | [webapp/Cargo.toml](../../webapp/Cargo.toml) | Dual declared | R | Owner: grants; release: notices |
| word | [word/Cargo.toml](../../word/Cargo.toml) | Dual declared | R | Owner: grants; release: notices |
| workflows | E16 | Dual declared | R | Owner: grants; release: notices |
| workflows-service | E17 | Dual declared | R | Owner: grants; release: notices |

## Vendored code and other assets

| Item | Evidence | Redistribution status | Notice | Unresolved owner or counsel review |
| --- | --- | --- | --- | --- |
| SendGrid schema | E18 | MIT declared; pinned commit/hash | M | Release: verify upstream bytes and delivery |
| SendGrid generator template | E19 | Separate provenance unverified | R; M | Owner/counsel |
| Swagger UI CSS | E20 | Pinned 5.17.14; grant unverified | A; D | Release: obtain exact upstream license/NOTICE |
| Swagger UI bundle | E21 | Pinned 5.17.14; grant unverified | A; D | Release: inspect bundled dependencies |
| Swagger standalone preset | E22 | Pinned 5.17.14; grant unverified | A; D | Release: inspect bundled dependencies |
| Swagger favicons 16 and 32 | E23 | Pinned 5.17.14; grant unverified | U | Release/counsel |
| Swagger index and init | E24 | First-party asserted | R | Owner: verify authorship |
| Plausible script | E25 | Provenance/grant unverified | U | Release/counsel: identify origin or replace |
| Ferris illustration | server/public/ferris.svg | Provenance/grant unverified | U | Owner: identify source or replace |
| Brand marks and QR artwork | E26 | Copyright and marks distinct | R; U | Owner/counsel |
| Shared logos and illustrations | E27 | Ownership unverified per file | R; U | Owner: confirm source or replace |
| PDF brand logo | E28 | Ownership assertion only | R; U | Owner: confirm source and permitted use |
| Workshop screenshot | E29 | Screenshot rights unverified | U | Owner/counsel: verify source or replace |
| Synthetic seed PDF | E30 | Synthetic content; grant unverified | R | Owner: verify authorship and embedded assets |
| Other served CSS and JS | E31 | First-party scope asserted only | R; D | Owner: inspect copied snippets and imports |

[SendGrid's MIT instrument](../../vendor/sendgrid/LICENSE) names Twilio Inc. Its
[README](../../vendor/sendgrid/README.md) pins upstream commit and schema hash. Retain that instrument independently of
any first-party replacement. V pins bytes and URLs but contains no license field: a hash demonstrates identity, not a
grant. Asset category rows cover the indicated tracked files; their unresolved status includes every file in that scope.
The root's trademark statements are assertions to review separately from copyright permissions.

## Fonts

F records the OFL conditions appearing in the repository: preserve copyright and license, keep font software under OFL,
check reserved font names for modifications, and check standalone-sale and endorsement conditions. Do not replace these
records with a first-party Apache license. Exact binary provenance, modification history, and delivered notices remain
release-maintainer checks for every row, even where the family has a declared license.

| Item | Evidence | Redistribution status | Notice | Unresolved owner or counsel review |
| --- | --- | --- | --- | --- |
| barlow-condensed web | [F1][F1] | OFL 1.1 declared | F | Release: verify bytes and full notice |
| dm-sans web | [F2][F2] | OFL 1.1 declared | F | Release: verify bytes and full notice |
| eb-garamond web | [F3][F3] | OFL 1.1 declared | F | Release: verify bytes and full notice |
| libre-franklin web | [F4][F4] | OFL 1.1 declared | F | Release: verify bytes and full notice |
| mukta web | [F5][F5] | OFL 1.1 declared | F | Release: verify bytes and full notice |
| pirata-one web | [F6][F6] | OFL declaration only | F | Release: verify bytes and full notice |
| plus-jakarta-sans web | [F7][F7] | OFL 1.1 declared | F | Release: verify bytes and full notice |
| public-sans web | [F8][F8] | OFL 1.1 declared | F | Release: verify bytes and full notice |
| source-sans-3 web | [F9][F9] | OFL 1.1 declared | F | Release: verify bytes and full notice |
| source-serif-4 web | [F10][F10] | OFL 1.1 declared | F | Release: verify bytes and full notice |
| NotoSerif PDF | [F11][F11] | OFL 1.1 declared | F | Release: verify bytes and full notice |
| Tinos PDF | [F12][F12] | OFL 1.1 declared | F | Release: verify bytes and full notice |
| Tinos web Regular/Bold | E32 | Grant-to-bytes unverified | F | Release: link WOFF2 to source and OFL |
| GORP Serif web | E33 | Proprietary; no binary tracked | U | Operator/counsel: license or replace |

[F1]: ../../server/public/fonts/barlow-condensed/OFL.txt
[F2]: ../../server/public/fonts/dm-sans/OFL.txt
[F3]: ../../server/public/fonts/eb-garamond/OFL.txt
[F4]: ../../server/public/fonts/libre-franklin/OFL.txt
[F5]: ../../server/public/fonts/mukta/OFL.txt
[F6]: ../../server/public/fonts/pirata-one/OFL.txt
[F7]: ../../server/public/fonts/plus-jakarta-sans/OFL.txt
[F8]: ../../server/public/fonts/public-sans/OFL.txt
[F9]: ../../server/public/fonts/source-sans-3/OFL.txt
[F10]: ../../server/public/fonts/source-serif-4/OFL.txt
[F11]: ../../pdf/assets/fonts/NotoSerif/OFL.txt
[F12]: ../../pdf/assets/fonts/Tinos/OFL.txt

The [GORP record](../../server/public/fonts/gorp-serif/LICENSE.txt) identifies TrashType LLC and separately licensed
operator-supplied WOFF2 assets. The repository is not evidence of an operator's purchased rights. A replacement must
have verified web-serving, embedding, and redistribution terms. PDF Noto Serif has two variable TTFs; PDF Tinos has four
static TTFs. The tracked web Tinos directory contains two WOFF2 files without a colocated OFL record; the PDF Tinos
record is evidence to compare, not proof that it covers those particular web binaries. Pirata One's record has a
declaration and external link only, so release review must supply the full applicable text. Other web OFL directories
record families but have no tracked font binaries; deployed asset inventories require a separate operator check.

## Third-party dependency notice coverage

The following matrix enumerates source-bearing `Cargo.lock` packages at the historical baseline. Evidence L gives exact
version and source. N heading means an exact package/version line appears in the retained notices; N gap means the
generator records manifest-only license metadata. Missing means no such line was found. This is a textual coverage
check, not a license compatibility audit or an assertion that all lockfile packages ship. All rows require
release/counsel review of features, target, linking, native components, and the actual source/binary artifact. D
supplies the notice procedure. Current generated counts appear in the readiness snapshot above. Refresh this matrix
before using it as a package-level release inventory.

Keep the existing N text intact. Before publication, regenerate into a scratch output and compare coverage against the
release graph, including transitive and native components. MIT, BSD, ISC, Apache, MPL, Unicode, and other texts in N
retain their own obligations. Dual-license choices and exceptions require explicit review; MPL source/file obligations
and native or embedded subcomponents cannot be discharged by changing the workspace's license field.

| Item | Evidence | Redistribution status | Notice | Unresolved owner or counsel review |
| --- | --- | --- | --- | --- |
| addr 0.15.6 | L; N heading | Unverified | D | Release/counsel |
| adler2 2.0.1 | L; N heading | Unverified | D | Release/counsel |
| adobe-cmap-parser 0.4.1 | L; N gap | Unverified | D | Release/counsel |
| aead 0.5.2 | L; N heading | Unverified | D | Release/counsel |
| aead 0.6.1 | L; N heading | Unverified | D | Release/counsel |
| aes 0.8.4 | L; N heading | Unverified | D | Release/counsel |
| aes 0.9.1 | L; N heading | Unverified | D | Release/counsel |
| aes-gcm 0.10.3 | L; N heading | Unverified | D | Release/counsel |
| aes-gcm 0.11.1 | L; N heading | Unverified | D | Release/counsel |
| affinitypool 0.7.0 | L; N heading | Unverified | D | Release/counsel |
| ahash 0.7.8 | L; N heading | Unverified | D | Release/counsel |
| ahash 0.8.12 | L; N heading | Unverified | D | Release/counsel |
| aho-corasick 1.1.4 | L; N heading | Unverified | D | Release/counsel |
| aligned 0.4.3 | L; N heading | Unverified | D | Release/counsel |
| aligned-vec 0.6.4 | L; N heading | Unverified | D | Release/counsel |
| alloc-no-stdlib 2.0.4 | L; N heading | Unverified | D | Release/counsel |
| alloc-stdlib 0.2.4 | L; N gap | Unverified | D | Release/counsel |
| allocator-api2 0.2.21 | L; N heading | Unverified | D | Release/counsel |
| ammonia 4.1.4 | L; N heading | Unverified | D | Release/counsel |
| android_system_properties 0.1.5 | L; N heading | Unverified | D | Release/counsel |
| annotate-snippets 0.12.16 | L; N heading | Unverified | D | Release/counsel |
| anstream 1.0.0 | L; N heading | Unverified | D | Release/counsel |
| anstyle 1.0.14 | L; N heading | Unverified | D | Release/counsel |
| anstyle-parse 1.0.0 | L; N heading | Unverified | D | Release/counsel |
| anstyle-query 1.1.5 | L; N heading | Unverified | D | Release/counsel |
| anstyle-wincon 3.0.11 | L; N heading | Unverified | D | Release/counsel |
| anyhow 1.0.104 | L; N heading | Unverified | D | Release/counsel |
| apache-avro 0.21.0 | L; N heading | Unverified | D | Release/counsel |
| approx 0.5.1 | L; N heading | Unverified | D | Release/counsel |
| ar_archive_writer 0.5.2 | L; N heading | Unverified | D | Release/counsel |
| arbitrary 1.4.2 | L; N heading | Unverified | D | Release/counsel |
| arc-swap 1.9.2 | L; N heading | Unverified | D | Release/counsel |
| arg_enum_proc_macro 0.3.4 | L; N heading | Unverified | D | Release/counsel |
| argon2 0.5.3 | L; N heading | Unverified | D | Release/counsel |
| array-init 2.1.0 | L; N heading | Unverified | D | Release/counsel |
| arraydeque 0.5.1 | L; N heading | Unverified | D | Release/counsel |
| arrayref 0.3.9 | L; N heading | Unverified | D | Release/counsel |
| arrayvec 0.7.8 | L; N heading | Unverified | D | Release/counsel |
| arrow 60.0.0 | L; N heading | Unverified | D | Release/counsel |
| arrow-arith 58.4.0 | L; N heading | Unverified | D | Release/counsel |
| arrow-arith 60.0.0 | L; N heading | Unverified | D | Release/counsel |
| arrow-array 58.4.0 | L; N heading | Unverified | D | Release/counsel |
| arrow-array 60.0.0 | L; N heading | Unverified | D | Release/counsel |
| arrow-buffer 58.4.0 | L; N heading | Unverified | D | Release/counsel |
| arrow-buffer 60.0.0 | L; N heading | Unverified | D | Release/counsel |
| arrow-cast 58.4.0 | L; N heading | Unverified | D | Release/counsel |
| arrow-cast 60.0.0 | L; N heading | Unverified | D | Release/counsel |
| arrow-cmp 60.0.0 | L; N heading | Unverified | D | Release/counsel |
| arrow-data 58.4.0 | L; N heading | Unverified | D | Release/counsel |
| arrow-data 60.0.0 | L; N heading | Unverified | D | Release/counsel |
| arrow-ipc 58.4.0 | L; N heading | Unverified | D | Release/counsel |
| arrow-ipc 60.0.0 | L; N heading | Unverified | D | Release/counsel |
| arrow-ord 58.4.0 | L; N heading | Unverified | D | Release/counsel |
| arrow-ord 60.0.0 | L; N heading | Unverified | D | Release/counsel |
| arrow-row 60.0.0 | L; N heading | Unverified | D | Release/counsel |
| arrow-schema 58.4.0 | L; N heading | Unverified | D | Release/counsel |
| arrow-schema 60.0.0 | L; N heading | Unverified | D | Release/counsel |
| arrow-select 58.4.0 | L; N heading | Unverified | D | Release/counsel |
| arrow-select 60.0.0 | L; N heading | Unverified | D | Release/counsel |
| arrow-string 58.4.0 | L; N heading | Unverified | D | Release/counsel |
| arrow-string 60.0.0 | L; N heading | Unverified | D | Release/counsel |
| as-any 0.3.2 | L; N heading | Unverified | D | Release/counsel |
| as-slice 0.1.5 | L; N heading | Unverified | D | Release/counsel |
| as-slice 0.2.1 | L; N heading | Unverified | D | Release/counsel |
| askama_escape 0.13.0 | L; N heading | Unverified | D | Release/counsel |
| assert-json-diff 2.0.2 | L; N heading | Unverified | D | Release/counsel |
| assert_cmd 2.2.2 | L; N heading | Unverified | D | Release/counsel |
| async-channel 2.5.0 | L; N heading | Unverified | D | Release/counsel |
| async-lock 3.4.2 | L; N heading | Unverified | D | Release/counsel |
| async-stream 0.3.6 | L; N heading | Unverified | D | Release/counsel |
| async-stream-impl 0.3.6 | L; N heading | Unverified | D | Release/counsel |
| async-task 4.7.1 | L; N heading | Unverified | D | Release/counsel |
| async-trait 0.1.92 | L; N heading | Unverified | D | Release/counsel |
| async-tungstenite 0.31.0 | L; N heading | Unverified | D | Release/counsel |
| atoi 2.0.0 | L; N heading | Unverified | D | Release/counsel |
| atoi 3.1.0 | L; N heading | Unverified | D | Release/counsel |
| atomic-polyfill 1.0.3 | L; N heading | Unverified | D | Release/counsel |
| atomic-waker 1.1.2 | L; N heading | Unverified | D | Release/counsel |
| autocfg 1.5.1 | L; N heading | Unverified | D | Release/counsel |
| av-scenechange 0.14.1 | L; N heading | Unverified | D | Release/counsel |
| av1-grain 0.2.5 | L; N heading | Unverified | D | Release/counsel |
| avif-serialize 0.8.9 | L; N heading | Unverified | D | Release/counsel |
| aws-credential-types 1.3.0 | L; N heading | Unverified | D | Release/counsel |
| aws-lc-rs 1.18.1 | L; N heading | Unverified | D | Release/counsel |
| aws-lc-sys 0.45.0 | L; N heading | Unverified | D | Release/counsel |
| aws-runtime 1.9.0 | L; N heading | Unverified | D | Release/counsel |
| aws-sdk-s3 1.139.0 | L; N heading | Unverified | D | Release/counsel |
| aws-sigv4 1.5.1 | L; N heading | Unverified | D | Release/counsel |
| aws-smithy-async 1.3.0 | L; N heading | Unverified | D | Release/counsel |
| aws-smithy-checksums 0.65.0 | L; N heading | Unverified | D | Release/counsel |
| aws-smithy-eventstream 0.61.1 | L; N heading | Unverified | D | Release/counsel |
| aws-smithy-http 0.64.0 | L; N heading | Unverified | D | Release/counsel |
| aws-smithy-http-client 1.2.0 | L; N heading | Unverified | D | Release/counsel |
| aws-smithy-json 0.63.0 | L; N heading | Unverified | D | Release/counsel |
| aws-smithy-observability 0.3.0 | L; N heading | Unverified | D | Release/counsel |
| aws-smithy-runtime 1.12.0 | L; N heading | Unverified | D | Release/counsel |
| aws-smithy-runtime-api 1.13.0 | L; N heading | Unverified | D | Release/counsel |
| aws-smithy-runtime-api-macros 1.1.0 | L; N heading | Unverified | D | Release/counsel |
| aws-smithy-schema 0.2.0 | L; N heading | Unverified | D | Release/counsel |
| aws-smithy-types 1.6.1 | L; N heading | Unverified | D | Release/counsel |
| aws-smithy-xml 0.62.0 | L; N heading | Unverified | D | Release/counsel |
| aws-types 1.5.0 | L; N heading | Unverified | D | Release/counsel |
| axum 0.8.9 | L; N heading | Unverified | D | Release/counsel |
| axum-core 0.5.6 | L; N heading | Unverified | D | Release/counsel |
| axum-extra 0.10.3 | L; N heading | Unverified | D | Release/counsel |
| axum-macros 0.5.1 | L; N heading | Unverified | D | Release/counsel |
| az 1.3.0 | L; N heading | Unverified | D | Release/counsel |
| backon 1.6.0 | L; N heading | Unverified | D | Release/counsel |
| base16 0.2.1 | L; N heading | Unverified | D | Release/counsel |
| base16ct 0.2.0 | L; N heading | Unverified | D | Release/counsel |
| base16ct 1.0.0 | L; N heading | Unverified | D | Release/counsel |
| base64 0.12.3 | L; N heading | Unverified | D | Release/counsel |
| base64 0.21.7 | L; N heading | Unverified | D | Release/counsel |
| base64 0.22.1 | L; N heading | Unverified | D | Release/counsel |
| base64 0.23.1 | L; N heading | Unverified | D | Release/counsel |
| base64-simd 0.8.0 | L; N gap | Unverified | D | Release/counsel |
| base64ct 1.8.3 | L; N heading | Unverified | D | Release/counsel |
| bcrypt 0.18.0 | L; N heading | Unverified | D | Release/counsel |
| biblatex 0.12.0 | L; N heading | Unverified | D | Release/counsel |
| bigdecimal 0.4.10 | L; N heading | Unverified | D | Release/counsel |
| bimap 0.6.3 | L; N heading | Unverified | D | Release/counsel |
| bincode 1.3.3 | L; N heading | Unverified | D | Release/counsel |
| bincode 2.0.1 | L; N heading | Unverified | D | Release/counsel |
| bincode_derive 2.0.1 | L; N heading | Unverified | D | Release/counsel |
| bit-set 0.8.0 | L; N heading | Unverified | D | Release/counsel |
| bit-vec 0.8.0 | L; N heading | Unverified | D | Release/counsel |
| bitflags 1.3.2 | L; N heading | Unverified | D | Release/counsel |
| bitflags 2.13.0 | L; N heading | Unverified | D | Release/counsel |
| bitstream-io 4.10.0 | L; N heading | Unverified | D | Release/counsel |
| bitvec 1.1.1 | L; N heading | Unverified | D | Release/counsel |
| blake2 0.10.6 | L; N heading | Unverified | D | Release/counsel |
| blake3 1.8.5 | L; N heading | Unverified | D | Release/counsel |
| block-buffer 0.10.4 | L; N heading | Unverified | D | Release/counsel |
| block-buffer 0.12.1 | L; N heading | Unverified | D | Release/counsel |
| block-buffer 0.9.0 | L; N heading | Unverified | D | Release/counsel |
| block-padding 0.3.3 | L; N heading | Unverified | D | Release/counsel |
| block-padding 0.4.2 | L; N heading | Unverified | D | Release/counsel |
| blowfish 0.9.1 | L; N heading | Unverified | D | Release/counsel |
| bnum 0.12.1 | L; N heading | Unverified | D | Release/counsel |
| bon 3.9.3 | L; N heading | Unverified | D | Release/counsel |
| bon-macros 3.9.3 | L; N heading | Unverified | D | Release/counsel |
| borsh 0.10.4 | L; N gap | Unverified | D | Release/counsel |
| borsh 1.8.0 | L; N heading | Unverified | D | Release/counsel |
| borsh-derive 0.10.4 | L; N gap | Unverified | D | Release/counsel |
| borsh-derive 1.8.0 | L; N heading | Unverified | D | Release/counsel |
| borsh-derive-internal 0.10.4 | L; N gap | Unverified | D | Release/counsel |
| borsh-schema-derive-internal 0.10.4 | L; N gap | Unverified | D | Release/counsel |
| boxcar 0.2.14 | L; N heading | Unverified | D | Release/counsel |
| brotli 8.0.4 | L; N heading | Unverified | D | Release/counsel |
| brotli-decompressor 5.0.3 | L; N heading | Unverified | D | Release/counsel |
| bs58 0.5.1 | L; N heading | Unverified | D | Release/counsel |
| bstr 1.12.3 | L; N heading | Unverified | D | Release/counsel |
| built 0.8.1 | L; N heading | Unverified | D | Release/counsel |
| bumpalo 3.20.3 | L; N heading | Unverified | D | Release/counsel |
| bv 0.11.1 | L; N heading | Unverified | D | Release/counsel |
| by_address 1.2.1 | L; N heading | Unverified | D | Release/counsel |
| bytecheck 0.6.12 | L; N heading | Unverified | D | Release/counsel |
| bytecheck_derive 0.6.12 | L; N heading | Unverified | D | Release/counsel |
| bytecount 0.6.9 | L; N heading | Unverified | D | Release/counsel |
| bytemuck 1.25.1 | L; N heading | Unverified | D | Release/counsel |
| bytemuck_derive 1.11.0 | L; N heading | Unverified | D | Release/counsel |
| byteorder 1.5.0 | L; N heading | Unverified | D | Release/counsel |
| byteorder-lite 0.1.0 | L; N heading | Unverified | D | Release/counsel |
| bytes 1.12.1 | L; N heading | Unverified | D | Release/counsel |
| bytes-utils 0.1.4 | L; N heading | Unverified | D | Release/counsel |
| castaway 0.2.4 | L; N heading | Unverified | D | Release/counsel |
| cbc 0.1.2 | L; N heading | Unverified | D | Release/counsel |
| cbc 0.2.1 | L; N heading | Unverified | D | Release/counsel |
| cc 1.2.67 | L; N heading | Unverified | D | Release/counsel |
| cesu8 1.1.0 | L; N gap | Unverified | D | Release/counsel |
| cfb 0.7.3 | L; N heading | Unverified | D | Release/counsel |
| cff-parser 0.2.0 | L; N heading | Unverified | D | Release/counsel |
| cfg-if 1.0.4 | L; N heading | Unverified | D | Release/counsel |
| cfg_aliases 0.2.1 | L; N heading | Unverified | D | Release/counsel |
| chacha20 0.10.1 | L; N heading | Unverified | D | Release/counsel |
| charset 0.1.5 | L; N heading | Unverified | D | Release/counsel |
| chinese-number 0.7.8 | L; N heading | Unverified | D | Release/counsel |
| chinese-variant 1.1.6 | L; N heading | Unverified | D | Release/counsel |
| chrono 0.4.45 | L; N heading | Unverified | D | Release/counsel |
| ciborium 0.2.2 | L; N heading | Unverified | D | Release/counsel |
| ciborium-io 0.2.2 | L; N heading | Unverified | D | Release/counsel |
| ciborium-ll 0.2.2 | L; N heading | Unverified | D | Release/counsel |
| cipher 0.4.4 | L; N heading | Unverified | D | Release/counsel |
| cipher 0.5.2 | L; N heading | Unverified | D | Release/counsel |
| citationberg 0.7.0 | L; N heading | Unverified | D | Release/counsel |
| clap 4.6.7 | L; N heading | Unverified | D | Release/counsel |
| clap_builder 4.6.7 | L; N heading | Unverified | D | Release/counsel |
| clap_derive 4.6.7 | L; N heading | Unverified | D | Release/counsel |
| clap_lex 1.1.0 | L; N heading | Unverified | D | Release/counsel |
| cmake 0.1.58 | L; N heading | Unverified | D | Release/counsel |
| cmov 0.5.4 | L; N heading | Unverified | D | Release/counsel |
| cobs 0.3.0 | L; N heading | Unverified | D | Release/counsel |
| codex 0.3.0 | L; N heading | Unverified | D | Release/counsel |
| color 0.3.3 | L; N heading | Unverified | D | Release/counsel |
| color_quant 1.1.0 | L; N heading | Unverified | D | Release/counsel |
| colorchoice 1.0.5 | L; N heading | Unverified | D | Release/counsel |
| combine 4.6.7 | L; N heading | Unverified | D | Release/counsel |
| comemo 0.5.1 | L; N heading | Unverified | D | Release/counsel |
| comemo-macros 0.5.1 | L; N gap | Unverified | D | Release/counsel |
| comfy-table 8.0.1 | L; N heading | Unverified | D | Release/counsel |
| concurrent-queue 2.5.0 | L; N heading | Unverified | D | Release/counsel |
| console 0.16.4 | L; N heading | Unverified | D | Release/counsel |
| console_error_panic_hook 0.1.7 | L; N heading | Unverified | D | Release/counsel |
| console_log 0.2.2 | L; N heading | Unverified | D | Release/counsel |
| const-oid 0.10.2 | L; N heading | Unverified | D | Release/counsel |
| const-oid 0.9.6 | L; N heading | Unverified | D | Release/counsel |
| const-random 0.1.18 | L; N heading | Unverified | D | Release/counsel |
| const-random-macro 0.1.16 | L; N heading | Unverified | D | Release/counsel |
| const-serialize 0.7.2 | L; N gap | Unverified | D | Release/counsel |
| const-serialize 0.8.0-alpha.0 | L; N gap | Unverified | D | Release/counsel |
| const-serialize-macro 0.7.2 | L; N gap | Unverified | D | Release/counsel |
| const-serialize-macro 0.8.0-alpha.0 | L; N gap | Unverified | D | Release/counsel |
| const-str 0.7.1 | L; N heading | Unverified | D | Release/counsel |
| const_format 0.2.36 | L; N heading | Unverified | D | Release/counsel |
| const_format_proc_macros 0.2.34 | L; N heading | Unverified | D | Release/counsel |
| constant_time_eq 0.4.2 | L; N heading | Unverified | D | Release/counsel |
| content_disposition 0.4.0 | L; N gap | Unverified | D | Release/counsel |
| convert_case 0.10.0 | L; N heading | Unverified | D | Release/counsel |
| convert_case 0.4.0 | L; N gap | Unverified | D | Release/counsel |
| convert_case 0.8.0 | L; N heading | Unverified | D | Release/counsel |
| cookie 0.16.2 | L; N heading | Unverified | D | Release/counsel |
| cookie 0.18.1 | L; N heading | Unverified | D | Release/counsel |
| cookie_store 0.22.1 | L; N heading | Unverified | D | Release/counsel |
| core-foundation 0.10.1 | L; N heading | Unverified | D | Release/counsel |
| core-foundation 0.9.4 | L; N heading | Unverified | D | Release/counsel |
| core-foundation-sys 0.8.7 | L; N heading | Unverified | D | Release/counsel |
| core_detect 1.0.0 | L; N heading | Unverified | D | Release/counsel |
| core_maths 0.1.1 | L; N heading | Unverified | D | Release/counsel |
| cpubits 0.1.1 | L; N heading | Unverified | D | Release/counsel |
| cpufeatures 0.2.17 | L; N heading | Unverified | D | Release/counsel |
| cpufeatures 0.3.0 | L; N heading | Unverified | D | Release/counsel |
| crc-fast 1.10.0 | L; N heading | Unverified | D | Release/counsel |
| crc32fast 1.5.0 | L; N heading | Unverified | D | Release/counsel |
| critical-section 1.2.0 | L; N heading | Unverified | D | Release/counsel |
| crossbeam-channel 0.5.16 | L; N heading | Unverified | D | Release/counsel |
| crossbeam-deque 0.8.7 | L; N heading | Unverified | D | Release/counsel |
| crossbeam-epoch 0.9.20 | L; N heading | Unverified | D | Release/counsel |
| crossbeam-queue 0.3.13 | L; N heading | Unverified | D | Release/counsel |
| crossbeam-skiplist 0.1.3 | L; N heading | Unverified | D | Release/counsel |
| crossbeam-utils 0.8.22 | L; N heading | Unverified | D | Release/counsel |
| crossterm 0.29.0 | L; N heading | Unverified | D | Release/counsel |
| crossterm_winapi 0.9.1 | L; N heading | Unverified | D | Release/counsel |
| crunchy 0.2.4 | L; N heading | Unverified | D | Release/counsel |
| crypto-bigint 0.5.5 | L; N heading | Unverified | D | Release/counsel |
| crypto-bigint 0.7.5 | L; N heading | Unverified | D | Release/counsel |
| crypto-common 0.1.6 | L; N heading | Unverified | D | Release/counsel |
| crypto-common 0.2.2 | L; N heading | Unverified | D | Release/counsel |
| cssparser 0.37.0 | L; N heading | Unverified | D | Release/counsel |
| cssparser-macros 0.7.0 | L; N heading | Unverified | D | Release/counsel |
| csv 1.4.0 | L; N heading | Unverified | D | Release/counsel |
| csv-core 0.1.13 | L; N heading | Unverified | D | Release/counsel |
| ctr 0.10.1 | L; N heading | Unverified | D | Release/counsel |
| ctr 0.9.2 | L; N heading | Unverified | D | Release/counsel |
| ctutils 0.4.2 | L; N heading | Unverified | D | Release/counsel |
| cucumber 0.23.0 | L; N heading | Unverified | D | Release/counsel |
| cucumber-codegen 0.23.0 | L; N gap | Unverified | D | Release/counsel |
| cucumber-expressions 0.5.0 | L; N heading | Unverified | D | Release/counsel |
| curve25519-dalek 4.1.3 | L; N heading | Unverified | D | Release/counsel |
| curve25519-dalek 5.0.0 | L; N heading | Unverified | D | Release/counsel |
| curve25519-dalek-derive 0.1.1 | L; N heading | Unverified | D | Release/counsel |
| darling 0.20.11 | L; N heading | Unverified | D | Release/counsel |
| darling 0.21.3 | L; N heading | Unverified | D | Release/counsel |
| darling 0.23.0 | L; N heading | Unverified | D | Release/counsel |
| darling_core 0.20.11 | L; N heading | Unverified | D | Release/counsel |
| darling_core 0.21.3 | L; N heading | Unverified | D | Release/counsel |
| darling_core 0.23.0 | L; N heading | Unverified | D | Release/counsel |
| darling_macro 0.20.11 | L; N heading | Unverified | D | Release/counsel |
| darling_macro 0.21.3 | L; N heading | Unverified | D | Release/counsel |
| darling_macro 0.23.0 | L; N heading | Unverified | D | Release/counsel |
| dashmap 6.2.1 | L; N heading | Unverified | D | Release/counsel |
| data-encoding 2.11.0 | L; N heading | Unverified | D | Release/counsel |
| data-url 0.3.2 | L; N heading | Unverified | D | Release/counsel |
| deadpool 0.12.3 | L; N heading | Unverified | D | Release/counsel |
| deadpool-runtime 0.1.4 | L; N heading | Unverified | D | Release/counsel |
| defmt 1.1.1 | L; N heading | Unverified | D | Release/counsel |
| defmt-macros 1.1.1 | L; N heading | Unverified | D | Release/counsel |
| defmt-parser 1.0.0 | L; N gap | Unverified | D | Release/counsel |
| der 0.7.10 | L; N heading | Unverified | D | Release/counsel |
| der 0.8.1 | L; N heading | Unverified | D | Release/counsel |
| deranged 0.5.8 | L; N heading | Unverified | D | Release/counsel |
| derive_builder 0.20.2 | L; N heading | Unverified | D | Release/counsel |
| derive_builder_core 0.20.2 | L; N heading | Unverified | D | Release/counsel |
| derive_builder_macro 0.20.2 | L; N heading | Unverified | D | Release/counsel |
| derive_more 2.1.1 | L; N heading | Unverified | D | Release/counsel |
| derive_more-impl 2.1.1 | L; N heading | Unverified | D | Release/counsel |
| deunicode 1.6.2 | L; N heading | Unverified | D | Release/counsel |
| difflib 0.4.0 | L; N gap | Unverified | D | Release/counsel |
| digest 0.10.7 | L; N heading | Unverified | D | Release/counsel |
| digest 0.11.3 | L; N heading | Unverified | D | Release/counsel |
| digest 0.9.0 | L; N heading | Unverified | D | Release/counsel |
| dioxus 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-asset-resolver 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-cli-config 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-config-macro 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-config-macros 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-core 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-core-macro 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-core-types 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-devtools 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-devtools-types 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-document 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-fullstack 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-fullstack-core 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-fullstack-macro 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-history 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-hooks 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-html 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-html-internal-macro 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-interpreter-js 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-liveview 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-logger 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-router 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-router-macro 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-rsx 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-server 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-signals 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-ssr 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-stores 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-stores-macro 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dioxus-web 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| dirs 6.0.0 | L; N heading | Unverified | D | Release/counsel |
| dirs-sys 0.5.0 | L; N heading | Unverified | D | Release/counsel |
| diskann 0.54.0 | L; N gap | Unverified | D | Release/counsel |
| diskann-utils 0.54.0 | L; N gap | Unverified | D | Release/counsel |
| diskann-vector 0.54.0 | L; N gap | Unverified | D | Release/counsel |
| diskann-wide 0.54.0 | L; N gap | Unverified | D | Release/counsel |
| displaydoc 0.2.6 | L; N heading | Unverified | D | Release/counsel |
| dissimilar 1.0.11 | L; N heading | Unverified | D | Release/counsel |
| dmp 0.2.3 | L; N heading | Unverified | D | Release/counsel |
| document-features 0.2.12 | L; N heading | Unverified | D | Release/counsel |
| dotenvy 0.15.7 | L; N heading | Unverified | D | Release/counsel |
| dtoa 1.0.11 | L; N heading | Unverified | D | Release/counsel |
| dtoa-short 0.3.5 | L; N heading | Unverified | D | Release/counsel |
| dunce 1.0.5 | L; N heading | Unverified | D | Release/counsel |
| dyn-clone 1.0.20 | L; N heading | Unverified | D | Release/counsel |
| earcutr 0.4.3 | L; N heading | Unverified | D | Release/counsel |
| ecb 0.1.2 | L; N heading | Unverified | D | Release/counsel |
| ecb 0.2.0 | L; N heading | Unverified | D | Release/counsel |
| ecdsa 0.16.9 | L; N heading | Unverified | D | Release/counsel |
| ecdsa 0.17.0 | L; N heading | Unverified | D | Release/counsel |
| ecow 0.2.6 | L; N heading | Unverified | D | Release/counsel |
| ed25519 2.2.3 | L; N heading | Unverified | D | Release/counsel |
| ed25519 3.0.0 | L; N heading | Unverified | D | Release/counsel |
| ed25519-dalek 2.2.0 | L; N heading | Unverified | D | Release/counsel |
| ed25519-dalek 3.0.0 | L; N heading | Unverified | D | Release/counsel |
| ego-tree 0.11.0 | L; N heading | Unverified | D | Release/counsel |
| either 1.16.0 | L; N heading | Unverified | D | Release/counsel |
| elliptic-curve 0.13.8 | L; N heading | Unverified | D | Release/counsel |
| elliptic-curve 0.14.1 | L; N heading | Unverified | D | Release/counsel |
| encode_unicode 1.0.0 | L; N heading | Unverified | D | Release/counsel |
| encoding_rs 0.8.42 | L; N heading | Unverified | D | Release/counsel |
| encoding_rs_io 0.1.7 | L; N heading | Unverified | D | Release/counsel |
| enum-ordinalize 4.4.1 | L; N heading | Unverified | D | Release/counsel |
| enum-ordinalize-derive 4.4.1 | L; N heading | Unverified | D | Release/counsel |
| enumset 1.1.14 | L; N heading | Unverified | D | Release/counsel |
| enumset_derive 0.15.0 | L; N heading | Unverified | D | Release/counsel |
| equator 0.4.2 | L; N heading | Unverified | D | Release/counsel |
| equator-macro 0.4.2 | L; N heading | Unverified | D | Release/counsel |
| equivalent 1.0.2 | L; N heading | Unverified | D | Release/counsel |
| erased-serde 0.4.10 | L; N heading | Unverified | D | Release/counsel |
| errno 0.3.14 | L; N heading | Unverified | D | Release/counsel |
| euclid 0.20.14 | L; N heading | Unverified | D | Release/counsel |
| euclid 0.22.14 | L; N heading | Unverified | D | Release/counsel |
| event-listener 5.4.1 | L; N heading | Unverified | D | Release/counsel |
| event-listener-strategy 0.5.4 | L; N heading | Unverified | D | Release/counsel |
| expect-test 1.5.1 | L; N heading | Unverified | D | Release/counsel |
| ext-sort 0.1.5 | L; N heading | Unverified | D | Release/counsel |
| extended 0.1.0 | L; N heading | Unverified | D | Release/counsel |
| fancy-regex 0.16.2 | L; N heading | Unverified | D | Release/counsel |
| fantoccini 0.22.1 | L; N heading | Unverified | D | Release/counsel |
| fast-srgb8 1.0.0 | L; N heading | Unverified | D | Release/counsel |
| fastnum 0.7.5 | L; N heading | Unverified | D | Release/counsel |
| fastrand 2.4.1 | L; N heading | Unverified | D | Release/counsel |
| fdeflate 0.3.7 | L; N heading | Unverified | D | Release/counsel |
| fearless_simd 0.4.1 | L; N heading | Unverified | D | Release/counsel |
| feature-probe 0.1.1 | L; N heading | Unverified | D | Release/counsel |
| ff 0.13.1 | L; N heading | Unverified | D | Release/counsel |
| ff 0.14.0 | L; N heading | Unverified | D | Release/counsel |
| fiat-crypto 0.2.9 | L; N heading | Unverified | D | Release/counsel |
| fiat-crypto 0.3.0 | L; N heading | Unverified | D | Release/counsel |
| filetime 0.2.29 | L; N heading | Unverified | D | Release/counsel |
| find-msvc-tools 0.1.9 | L; N heading | Unverified | D | Release/counsel |
| five8 0.2.1 | L; N gap | Unverified | D | Release/counsel |
| five8_const 0.1.4 | L; N gap | Unverified | D | Release/counsel |
| five8_core 0.1.2 | L; N gap | Unverified | D | Release/counsel |
| flatbuffers 25.12.19 | L; N gap | Unverified | D | Release/counsel |
| flate2 1.1.10 | L; N heading | Unverified | D | Release/counsel |
| float-cmp 0.10.0 | L; N heading | Unverified | D | Release/counsel |
| float-cmp 0.9.0 | L; N heading | Unverified | D | Release/counsel |
| float_next_after 1.0.0 | L; N heading | Unverified | D | Release/counsel |
| fluent-uri 0.1.4 | L; N heading | Unverified | D | Release/counsel |
| fnv 1.0.7 | L; N heading | Unverified | D | Release/counsel |
| foldhash 0.2.0 | L; N heading | Unverified | D | Release/counsel |
| font-types 0.11.3 | L; N heading | Unverified | D | Release/counsel |
| fontconfig-parser 0.5.8 | L; N heading | Unverified | D | Release/counsel |
| fontdb 0.23.0 | L; N heading | Unverified | D | Release/counsel |
| foreign-types 0.3.2 | L; N heading | Unverified | D | Release/counsel |
| foreign-types-shared 0.1.1 | L; N heading | Unverified | D | Release/counsel |
| form_urlencoded 1.2.2 | L; N heading | Unverified | D | Release/counsel |
| fs_extra 1.3.0 | L; N heading | Unverified | D | Release/counsel |
| fst 0.4.7 | L; N heading | Unverified | D | Release/counsel |
| funty 2.0.0 | L; N heading | Unverified | D | Release/counsel |
| futures 0.3.34 | L; N heading | Unverified | D | Release/counsel |
| futures-channel 0.3.34 | L; N heading | Unverified | D | Release/counsel |
| futures-core 0.3.34 | L; N heading | Unverified | D | Release/counsel |
| futures-executor 0.3.34 | L; N heading | Unverified | D | Release/counsel |
| futures-io 0.3.34 | L; N heading | Unverified | D | Release/counsel |
| futures-macro 0.3.34 | L; N heading | Unverified | D | Release/counsel |
| futures-sink 0.3.34 | L; N heading | Unverified | D | Release/counsel |
| futures-task 0.3.34 | L; N heading | Unverified | D | Release/counsel |
| futures-util 0.3.34 | L; N heading | Unverified | D | Release/counsel |
| fuzzy-matcher 0.3.7 | L; N heading | Unverified | D | Release/counsel |
| generational-box 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| generic-array 0.12.4 | L; N heading | Unverified | D | Release/counsel |
| generic-array 0.13.3 | L; N heading | Unverified | D | Release/counsel |
| generic-array 0.14.9 | L; N heading | Unverified | D | Release/counsel |
| geo 0.32.0 | L; N gap | Unverified | D | Release/counsel |
| geo-types 0.7.19 | L; N gap | Unverified | D | Release/counsel |
| geographiclib-rs 0.2.7 | L; N heading | Unverified | D | Release/counsel |
| getopts 0.2.24 | L; N heading | Unverified | D | Release/counsel |
| getrandom 0.1.16 | L; N heading | Unverified | D | Release/counsel |
| getrandom 0.2.17 | L; N heading | Unverified | D | Release/counsel |
| getrandom 0.3.4 | L; N heading | Unverified | D | Release/counsel |
| getrandom 0.4.3 | L; N heading | Unverified | D | Release/counsel |
| ghash 0.5.1 | L; N heading | Unverified | D | Release/counsel |
| ghash 0.6.0 | L; N heading | Unverified | D | Release/counsel |
| gherkin 0.16.0 | L; N heading | Unverified | D | Release/counsel |
| gif 0.14.2 | L; N heading | Unverified | D | Release/counsel |
| glidesort 0.1.2 | L; N gap | Unverified | D | Release/counsel |
| glifo 0.1.1 | L; N heading | Unverified | D | Release/counsel |
| glob 0.3.3 | L; N heading | Unverified | D | Release/counsel |
| globset 0.4.18 | L; N heading | Unverified | D | Release/counsel |
| globwalk 0.9.1 | L; N heading | Unverified | D | Release/counsel |
| gloo-net 0.6.0 | L; N gap | Unverified | D | Release/counsel |
| gloo-timers 0.3.0 | L; N gap | Unverified | D | Release/counsel |
| gloo-utils 0.2.0 | L; N gap | Unverified | D | Release/counsel |
| google-cloud-auth 0.17.2 | L; N heading | Unverified | D | Release/counsel |
| google-cloud-metadata 0.5.1 | L; N heading | Unverified | D | Release/counsel |
| google-cloud-storage 0.24.0 | L; N heading | Unverified | D | Release/counsel |
| google-cloud-token 0.1.2 | L; N gap | Unverified | D | Release/counsel |
| granit-parser 0.0.7 | L; N heading | Unverified | D | Release/counsel |
| group 0.13.0 | L; N heading | Unverified | D | Release/counsel |
| group 0.14.0 | L; N heading | Unverified | D | Release/counsel |
| guillotiere 0.7.0 | L; N heading | Unverified | D | Release/counsel |
| h2 0.3.27 | L; N heading | Unverified | D | Release/counsel |
| h2 0.4.15 | L; N heading | Unverified | D | Release/counsel |
| half 2.7.1 | L; N heading | Unverified | D | Release/counsel |
| hash32 0.1.1 | L; N heading | Unverified | D | Release/counsel |
| hash32 0.2.1 | L; N heading | Unverified | D | Release/counsel |
| hash32 0.3.1 | L; N heading | Unverified | D | Release/counsel |
| hashbrown 0.12.3 | L; N heading | Unverified | D | Release/counsel |
| hashbrown 0.14.5 | L; N heading | Unverified | D | Release/counsel |
| hashbrown 0.16.1 | L; N heading | Unverified | D | Release/counsel |
| hashbrown 0.17.1 | L; N heading | Unverified | D | Release/counsel |
| hashify 0.2.9 | L; N gap | Unverified | D | Release/counsel |
| hayagriva 0.10.1 | L; N heading | Unverified | D | Release/counsel |
| hayro 0.7.1 | L; N heading | Unverified | D | Release/counsel |
| hayro-ccitt 0.3.0 | L; N heading | Unverified | D | Release/counsel |
| hayro-cmap 0.1.0 | L; N heading | Unverified | D | Release/counsel |
| hayro-interpret 0.7.0 | L; N heading | Unverified | D | Release/counsel |
| hayro-jbig2 0.3.0 | L; N heading | Unverified | D | Release/counsel |
| hayro-jpeg2000 0.3.5 | L; N heading | Unverified | D | Release/counsel |
| hayro-postscript 0.1.0 | L; N heading | Unverified | D | Release/counsel |
| hayro-svg 0.7.0 | L; N heading | Unverified | D | Release/counsel |
| hayro-syntax 0.7.2 | L; N heading | Unverified | D | Release/counsel |
| hayro-write 0.7.0 | L; N heading | Unverified | D | Release/counsel |
| headers 0.4.1 | L; N heading | Unverified | D | Release/counsel |
| headers-core 0.3.0 | L; N heading | Unverified | D | Release/counsel |
| heapless 0.6.1 | L; N heading | Unverified | D | Release/counsel |
| heapless 0.7.17 | L; N heading | Unverified | D | Release/counsel |
| heapless 0.8.0 | L; N heading | Unverified | D | Release/counsel |
| heck 0.4.1 | L; N heading | Unverified | D | Release/counsel |
| heck 0.5.0 | L; N heading | Unverified | D | Release/counsel |
| hermit-abi 0.5.2 | L; N heading | Unverified | D | Release/counsel |
| hex 0.4.3 | L; N heading | Unverified | D | Release/counsel |
| hkdf 0.12.4 | L; N heading | Unverified | D | Release/counsel |
| hmac 0.12.1 | L; N heading | Unverified | D | Release/counsel |
| hmac 0.13.0 | L; N heading | Unverified | D | Release/counsel |
| home 0.5.12 | L; N heading | Unverified | D | Release/counsel |
| html5ever 0.39.0 | L; N heading | Unverified | D | Release/counsel |
| http 0.2.12 | L; N heading | Unverified | D | Release/counsel |
| http 1.4.2 | L; N heading | Unverified | D | Release/counsel |
| http-body 0.4.6 | L; N heading | Unverified | D | Release/counsel |
| http-body 1.1.0 | L; N heading | Unverified | D | Release/counsel |
| http-body-util 0.1.5 | L; N heading | Unverified | D | Release/counsel |
| http-range-header 0.4.2 | L; N heading | Unverified | D | Release/counsel |
| httparse 1.10.1 | L; N heading | Unverified | D | Release/counsel |
| httpdate 1.0.3 | L; N heading | Unverified | D | Release/counsel |
| humantime 2.4.0 | L; N heading | Unverified | D | Release/counsel |
| hybrid-array 0.4.13 | L; N heading | Unverified | D | Release/counsel |
| hyper 0.14.32 | L; N heading | Unverified | D | Release/counsel |
| hyper 1.10.1 | L; N heading | Unverified | D | Release/counsel |
| hyper-rustls 0.24.2 | L; N heading | Unverified | D | Release/counsel |
| hyper-rustls 0.27.9 | L; N heading | Unverified | D | Release/counsel |
| hyper-timeout 0.5.2 | L; N heading | Unverified | D | Release/counsel |
| hyper-tls 0.6.0 | L; N heading | Unverified | D | Release/counsel |
| hyper-util 0.1.20 | L; N heading | Unverified | D | Release/counsel |
| hypher 0.1.7 | L; N heading | Unverified | D | Release/counsel |
| i_float 1.15.0 | L; N heading | Unverified | D | Release/counsel |
| i_key_sort 0.6.0 | L; N heading | Unverified | D | Release/counsel |
| i_overlay 4.0.7 | L; N gap | Unverified | D | Release/counsel |
| i_shape 1.14.0 | L; N heading | Unverified | D | Release/counsel |
| i_tree 0.16.0 | L; N heading | Unverified | D | Release/counsel |
| iana-time-zone 0.1.65 | L; N heading | Unverified | D | Release/counsel |
| iana-time-zone-haiku 0.1.2 | L; N heading | Unverified | D | Release/counsel |
| iceberg 0.10.1 | L; N heading | Unverified | D | Release/counsel |
| icu_collator 2.2.1 | L; N heading | Unverified | D | Release/counsel |
| icu_collator_data 2.2.0 | L; N heading | Unverified | D | Release/counsel |
| icu_collections 1.5.0 | L; N heading | Unverified | D | Release/counsel |
| icu_collections 2.2.0 | L; N heading | Unverified | D | Release/counsel |
| icu_locale 2.2.0 | L; N heading | Unverified | D | Release/counsel |
| icu_locale_core 2.2.0 | L; N heading | Unverified | D | Release/counsel |
| icu_locale_data 2.2.0 | L; N heading | Unverified | D | Release/counsel |
| icu_locid 1.5.0 | L; N heading | Unverified | D | Release/counsel |
| icu_normalizer 2.2.0 | L; N heading | Unverified | D | Release/counsel |
| icu_normalizer_data 2.2.0 | L; N heading | Unverified | D | Release/counsel |
| icu_properties 2.2.0 | L; N heading | Unverified | D | Release/counsel |
| icu_properties_data 2.2.0 | L; N heading | Unverified | D | Release/counsel |
| icu_provider 1.5.0 | L; N heading | Unverified | D | Release/counsel |
| icu_provider 2.2.0 | L; N heading | Unverified | D | Release/counsel |
| icu_provider_blob 2.2.0 | L; N heading | Unverified | D | Release/counsel |
| icu_provider_macros 1.5.0 | L; N heading | Unverified | D | Release/counsel |
| icu_segmenter 1.5.0 | L; N heading | Unverified | D | Release/counsel |
| icu_segmenter 2.2.0 | L; N heading | Unverified | D | Release/counsel |
| icu_segmenter_data 1.5.1 | L; N heading | Unverified | D | Release/counsel |
| icu_segmenter_data 2.2.0 | L; N heading | Unverified | D | Release/counsel |
| ident_case 1.0.1 | L; N heading | Unverified | D | Release/counsel |
| idna 1.1.0 | L; N heading | Unverified | D | Release/counsel |
| idna_adapter 1.2.2 | L; N heading | Unverified | D | Release/counsel |
| ignore 0.4.28 | L; N heading | Unverified | D | Release/counsel |
| image 0.25.10 | L; N heading | Unverified | D | Release/counsel |
| image-webp 0.2.4 | L; N heading | Unverified | D | Release/counsel |
| imagesize 0.14.0 | L; N heading | Unverified | D | Release/counsel |
| imgref 1.12.2 | L; N heading | Unverified | D | Release/counsel |
| include_dir 0.7.4 | L; N gap | Unverified | D | Release/counsel |
| include_dir_macros 0.7.4 | L; N gap | Unverified | D | Release/counsel |
| indexmap 1.9.3 | L; N heading | Unverified | D | Release/counsel |
| indexmap 2.14.0 | L; N heading | Unverified | D | Release/counsel |
| infer 0.19.0 | L; N heading | Unverified | D | Release/counsel |
| inflections 1.1.1 | L; N heading | Unverified | D | Release/counsel |
| inout 0.1.4 | L; N heading | Unverified | D | Release/counsel |
| inout 0.2.2 | L; N heading | Unverified | D | Release/counsel |
| integer-encoding 3.0.4 | L; N heading | Unverified | D | Release/counsel |
| interpolate_name 0.2.4 | L; N heading | Unverified | D | Release/counsel |
| inventory 0.3.24 | L; N heading | Unverified | D | Release/counsel |
| ipnet 2.12.2 | L; N heading | Unverified | D | Release/counsel |
| is-terminal 0.4.17 | L; N heading | Unverified | D | Release/counsel |
| is_ci 1.2.0 | L; N heading | Unverified | D | Release/counsel |
| is_terminal_polyfill 1.70.2 | L; N heading | Unverified | D | Release/counsel |
| itertools 0.11.0 | L; N heading | Unverified | D | Release/counsel |
| itertools 0.13.0 | L; N heading | Unverified | D | Release/counsel |
| itertools 0.14.0 | L; N heading | Unverified | D | Release/counsel |
| itoa 1.0.18 | L; N heading | Unverified | D | Release/counsel |
| jiff 0.2.32 | L; N heading | Unverified | D | Release/counsel |
| jiff-static 0.2.32 | L; N heading | Unverified | D | Release/counsel |
| jni 0.21.1 | L; N heading | Unverified | D | Release/counsel |
| jni 0.22.4 | L; N gap | Unverified | D | Release/counsel |
| jni-macros 0.22.4 | L; N gap | Unverified | D | Release/counsel |
| jni-sys 0.3.1 | L; N heading | Unverified | D | Release/counsel |
| jni-sys 0.4.1 | L; N heading | Unverified | D | Release/counsel |
| jni-sys-macros 0.4.1 | L; N gap | Unverified | D | Release/counsel |
| jobserver 0.1.35 | L; N heading | Unverified | D | Release/counsel |
| js-sys 0.3.103 | L; N heading | Unverified | D | Release/counsel |
| jsonpath-rust 1.0.7 | L; N heading | Unverified | D | Release/counsel |
| jsonptr 0.7.1 | L; N heading | Unverified | D | Release/counsel |
| jsonwebtoken 10.4.0 | L; N heading | Unverified | D | Release/counsel |
| jsonwebtoken 11.1.0 | L; N heading | Unverified | D | Release/counsel |
| jsonwebtoken 9.3.1 | L; N heading | Unverified | D | Release/counsel |
| k8s-openapi 0.28.0 | L; N heading | Unverified | D | Release/counsel |
| kamadak-exif 0.6.1 | L; N heading | Unverified | D | Release/counsel |
| keccak 0.1.6 | L; N heading | Unverified | D | Release/counsel |
| keyboard-types 0.7.0 | L; N heading | Unverified | D | Release/counsel |
| konst 0.2.20 | L; N heading | Unverified | D | Release/counsel |
| konst_macro_rules 0.2.19 | L; N heading | Unverified | D | Release/counsel |
| krilla 0.8.2 | L; N gap | Unverified | D | Release/counsel |
| krilla-svg 0.8.1 | L; N gap | Unverified | D | Release/counsel |
| kube 4.2.0 | L; N gap | Unverified | D | Release/counsel |
| kube-client 4.2.0 | L; N gap | Unverified | D | Release/counsel |
| kube-core 4.2.0 | L; N gap | Unverified | D | Release/counsel |
| kurbo 0.13.1 | L; N heading | Unverified | D | Release/counsel |
| lazy-js-bundle 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| lazy_static 1.5.0 | L; N heading | Unverified | D | Release/counsel |
| lexical-core 1.0.6 | L; N heading | Unverified | D | Release/counsel |
| lexical-parse-float 1.0.6 | L; N heading | Unverified | D | Release/counsel |
| lexical-parse-integer 1.0.6 | L; N heading | Unverified | D | Release/counsel |
| lexical-util 1.0.7 | L; N heading | Unverified | D | Release/counsel |
| lexical-write-float 1.0.6 | L; N heading | Unverified | D | Release/counsel |
| lexical-write-integer 1.0.6 | L; N heading | Unverified | D | Release/counsel |
| lexicmp 0.2.0 | L; N heading | Unverified | D | Release/counsel |
| libc 0.2.186 | L; N heading | Unverified | D | Release/counsel |
| libfuzzer-sys 0.4.13 | L; N heading | Unverified | D | Release/counsel |
| libloading 0.8.9 | L; N heading | Unverified | D | Release/counsel |
| libm 0.2.16 | L; N heading | Unverified | D | Release/counsel |
| libredox 0.1.18 | L; N heading | Unverified | D | Release/counsel |
| libsecp256k1 0.6.0 | L; N heading | Unverified | D | Release/counsel |
| libsecp256k1-core 0.2.2 | L; N gap | Unverified | D | Release/counsel |
| libsecp256k1-gen-ecmult 0.2.1 | L; N gap | Unverified | D | Release/counsel |
| libsecp256k1-gen-genmult 0.2.1 | L; N gap | Unverified | D | Release/counsel |
| libwebp-sys 0.9.6 | L; N gap | Unverified | D | Release/counsel |
| linebender_resource_handle 0.1.1 | L; N heading | Unverified | D | Release/counsel |
| linked-hash-map 0.5.6 | L; N heading | Unverified | D | Release/counsel |
| linux-raw-sys 0.12.1 | L; N heading | Unverified | D | Release/counsel |
| lipsum 0.9.1 | L; N heading | Unverified | D | Release/counsel |
| litemap 0.7.5 | L; N heading | Unverified | D | Release/counsel |
| litemap 0.8.2 | L; N heading | Unverified | D | Release/counsel |
| litrs 1.0.0 | L; N heading | Unverified | D | Release/counsel |
| lock_api 0.4.14 | L; N heading | Unverified | D | Release/counsel |
| log 0.4.33 | L; N heading | Unverified | D | Release/counsel |
| longest-increasing-subsequence 0.1.0 | L; N heading | Unverified | D | Release/counsel |
| loop9 0.1.5 | L; N heading | Unverified | D | Release/counsel |
| lopdf 0.42.0 | L; N heading | Unverified | D | Release/counsel |
| lopdf 0.45.0 | L; N heading | Unverified | D | Release/counsel |
| lru 0.16.4 | L; N heading | Unverified | D | Release/counsel |
| lru-slab 0.1.2 | L; N heading | Unverified | D | Release/counsel |
| lsp-types 0.97.0 | L; N heading | Unverified | D | Release/counsel |
| lz4 1.28.1 | L; N heading | Unverified | D | Release/counsel |
| lz4-sys 1.11.1+lz4-1.10.0 | L; N heading | Unverified | D | Release/counsel |
| lz4_flex 0.13.1 | L; N heading | Unverified | D | Release/counsel |
| macro-string 0.1.4 | L; N heading | Unverified | D | Release/counsel |
| mail-parser 0.11.9 | L; N gap | Unverified | D | Release/counsel |
| manganis 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| manganis-core 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| manganis-macro 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| maplit 1.0.2 | L; N heading | Unverified | D | Release/counsel |
| markup5ever 0.39.0 | L; N heading | Unverified | D | Release/counsel |
| matchers 0.2.0 | L; N heading | Unverified | D | Release/counsel |
| matchit 0.8.4 | L; N heading | Unverified | D | Release/counsel |
| matrixmultiply 0.3.11 | L; N heading | Unverified | D | Release/counsel |
| maybe-rayon 0.1.1 | L; N heading | Unverified | D | Release/counsel |
| md-5 0.10.6 | L; N heading | Unverified | D | Release/counsel |
| md-5 0.11.0 | L; N heading | Unverified | D | Release/counsel |
| memchr 2.8.3 | L; N heading | Unverified | D | Release/counsel |
| memfd 0.6.5 | L; N heading | Unverified | D | Release/counsel |
| memmap2 0.9.11 | L; N heading | Unverified | D | Release/counsel |
| memoffset 0.9.1 | L; N heading | Unverified | D | Release/counsel |
| mime 0.3.17 | L; N heading | Unverified | D | Release/counsel |
| mime_guess 2.0.5 | L; N heading | Unverified | D | Release/counsel |
| miniz_oxide 0.8.9 | L; N heading | Unverified | D | Release/counsel |
| miniz_oxide 0.9.1 | L; N heading | Unverified | D | Release/counsel |
| mio 1.2.2 | L; N heading | Unverified | D | Release/counsel |
| moka 0.12.15 | L; N heading | Unverified | D | Release/counsel |
| moxcms 0.8.1 | L; N heading | Unverified | D | Release/counsel |
| msvc_spectre_libs 0.1.3 | L; N heading | Unverified | D | Release/counsel |
| multer 3.1.0 | L; N heading | Unverified | D | Release/counsel |
| multiversion_no_op 1.0.0 | L; N heading | Unverified | D | Release/counsel |
| murmur3 0.5.2 | L; N heading | Unverified | D | Release/counsel |
| mutate_once 0.1.2 | L; N heading | Unverified | D | Release/counsel |
| native-tls 0.2.18 | L; N heading | Unverified | D | Release/counsel |
| ndarray 0.17.2 | L; N heading | Unverified | D | Release/counsel |
| ndarray-stats 0.7.0 | L; N heading | Unverified | D | Release/counsel |
| ndk 0.9.0 | L; N gap | Unverified | D | Release/counsel |
| ndk-context 0.1.1 | L; N gap | Unverified | D | Release/counsel |
| ndk-sys 0.6.0+11769913 | L; N gap | Unverified | D | Release/counsel |
| new_debug_unreachable 1.0.6 | L; N heading | Unverified | D | Release/counsel |
| no_std_io2 0.9.4 | L; N heading | Unverified | D | Release/counsel |
| nohash-hasher 0.2.0 | L; N heading | Unverified | D | Release/counsel |
| noisy_float 0.2.1 | L; N heading | Unverified | D | Release/counsel |
| nom 8.0.0 | L; N heading | Unverified | D | Release/counsel |
| nom_locate 5.0.0 | L; N heading | Unverified | D | Release/counsel |
| noop_proc_macro 0.3.0 | L; N heading | Unverified | D | Release/counsel |
| normalize-line-endings 0.3.0 | L; N heading | Unverified | D | Release/counsel |
| ntapi 0.4.3 | L; N heading | Unverified | D | Release/counsel |
| nu-ansi-term 0.50.3 | L; N heading | Unverified | D | Release/counsel |
| num-bigint 0.4.8 | L; N heading | Unverified | D | Release/counsel |
| num-bigint 0.5.1 | L; N heading | Unverified | D | Release/counsel |
| num-bigint-dig 0.8.6 | L; N heading | Unverified | D | Release/counsel |
| num-complex 0.4.6 | L; N heading | Unverified | D | Release/counsel |
| num-conv 0.2.2 | L; N heading | Unverified | D | Release/counsel |
| num-derive 0.4.2 | L; N heading | Unverified | D | Release/counsel |
| num-integer 0.1.46 | L; N heading | Unverified | D | Release/counsel |
| num-iter 0.1.46 | L; N heading | Unverified | D | Release/counsel |
| num-rational 0.4.2 | L; N heading | Unverified | D | Release/counsel |
| num-traits 0.2.19 | L; N heading | Unverified | D | Release/counsel |
| num_cpus 1.17.0 | L; N heading | Unverified | D | Release/counsel |
| num_enum 0.7.6 | L; N heading | Unverified | D | Release/counsel |
| num_enum_derive 0.7.6 | L; N heading | Unverified | D | Release/counsel |
| objc2 0.6.4 | L; N gap | Unverified | D | Release/counsel |
| objc2-core-foundation 0.3.2 | L; N gap | Unverified | D | Release/counsel |
| objc2-encode 4.1.0 | L; N gap | Unverified | D | Release/counsel |
| objc2-io-kit 0.3.2 | L; N gap | Unverified | D | Release/counsel |
| object 0.37.3 | L; N heading | Unverified | D | Release/counsel |
| object_store 0.13.2 | L; N heading | Unverified | D | Release/counsel |
| once_cell 1.21.4 | L; N heading | Unverified | D | Release/counsel |
| once_cell_polyfill 1.70.2 | L; N heading | Unverified | D | Release/counsel |
| opaque-debug 0.3.1 | L; N heading | Unverified | D | Release/counsel |
| openssl 0.10.81 | L; N heading | Unverified | D | Release/counsel |
| openssl-macros 0.1.1 | L; N heading | Unverified | D | Release/counsel |
| openssl-probe 0.2.1 | L; N heading | Unverified | D | Release/counsel |
| openssl-sys 0.9.117 | L; N heading | Unverified | D | Release/counsel |
| opentelemetry 0.33.0 | L; N gap | Unverified | D | Release/counsel |
| opentelemetry-appender-tracing 0.33.0 | L; N gap | Unverified | D | Release/counsel |
| opentelemetry-otlp 0.33.0 | L; N gap | Unverified | D | Release/counsel |
| opentelemetry-proto 0.33.0 | L; N gap | Unverified | D | Release/counsel |
| opentelemetry_sdk 0.33.0 | L; N gap | Unverified | D | Release/counsel |
| option-ext 0.2.0 | L; N heading | Unverified | D | Release/counsel |
| ordered-float 2.10.1 | L; N heading | Unverified | D | Release/counsel |
| ordered-float 4.6.0 | L; N heading | Unverified | D | Release/counsel |
| outref 0.5.2 | L; N heading | Unverified | D | Release/counsel |
| owo-colors 4.4.0 | L; N heading | Unverified | D | Release/counsel |
| p256 0.13.2 | L; N heading | Unverified | D | Release/counsel |
| p256 0.14.0 | L; N heading | Unverified | D | Release/counsel |
| p384 0.13.1 | L; N heading | Unverified | D | Release/counsel |
| palette 0.7.6 | L; N heading | Unverified | D | Release/counsel |
| palette_derive 0.7.6 | L; N heading | Unverified | D | Release/counsel |
| papaya 0.2.4 | L; N heading | Unverified | D | Release/counsel |
| parking 2.2.1 | L; N heading | Unverified | D | Release/counsel |
| parking_lot 0.12.5 | L; N heading | Unverified | D | Release/counsel |
| parking_lot_core 0.9.12 | L; N heading | Unverified | D | Release/counsel |
| parquet 58.4.0 | L; N heading | Unverified | D | Release/counsel |
| parquet 60.0.0 | L; N heading | Unverified | D | Release/counsel |
| password-hash 0.5.0 | L; N heading | Unverified | D | Release/counsel |
| paste 1.0.15 | L; N heading | Unverified | D | Release/counsel |
| pastey 0.1.1 | L; N heading | Unverified | D | Release/counsel |
| pastey 0.2.3 | L; N heading | Unverified | D | Release/counsel |
| path-clean 1.0.1 | L; N heading | Unverified | D | Release/counsel |
| pbkdf2 0.12.2 | L; N heading | Unverified | D | Release/counsel |
| pdf-extract 0.12.1 | L; N gap | Unverified | D | Release/counsel |
| pdf-writer 0.15.0 | L; N heading | Unverified | D | Release/counsel |
| pdqselect 0.1.0 | L; N gap | Unverified | D | Release/counsel |
| peg 0.6.3 | L; N heading | Unverified | D | Release/counsel |
| peg-macros 0.6.3 | L; N gap | Unverified | D | Release/counsel |
| peg-runtime 0.6.3 | L; N gap | Unverified | D | Release/counsel |
| pem 3.0.6 | L; N heading | Unverified | D | Release/counsel |
| pem-rfc7468 0.7.0 | L; N heading | Unverified | D | Release/counsel |
| pem-rfc7468 1.0.0 | L; N heading | Unverified | D | Release/counsel |
| peniko 0.6.1 | L; N heading | Unverified | D | Release/counsel |
| percent-encoding 2.3.2 | L; N heading | Unverified | D | Release/counsel |
| pest 2.8.8 | L; N heading | Unverified | D | Release/counsel |
| pest_derive 2.8.8 | L; N heading | Unverified | D | Release/counsel |
| pest_generator 2.8.8 | L; N heading | Unverified | D | Release/counsel |
| pest_meta 2.8.8 | L; N heading | Unverified | D | Release/counsel |
| phf 0.13.1 | L; N heading | Unverified | D | Release/counsel |
| phf_codegen 0.13.1 | L; N heading | Unverified | D | Release/counsel |
| phf_generator 0.13.1 | L; N heading | Unverified | D | Release/counsel |
| phf_macros 0.13.1 | L; N heading | Unverified | D | Release/counsel |
| phf_shared 0.13.1 | L; N heading | Unverified | D | Release/counsel |
| pic-scale 0.7.10 | L; N heading | Unverified | D | Release/counsel |
| pico-args 0.5.0 | L; N heading | Unverified | D | Release/counsel |
| pin-project 1.1.13 | L; N heading | Unverified | D | Release/counsel |
| pin-project-internal 1.1.13 | L; N heading | Unverified | D | Release/counsel |
| pin-project-lite 0.2.17 | L; N heading | Unverified | D | Release/counsel |
| pin-utils 0.1.0 | L; N heading | Unverified | D | Release/counsel |
| pkcs1 0.7.5 | L; N heading | Unverified | D | Release/counsel |
| pkcs8 0.10.2 | L; N heading | Unverified | D | Release/counsel |
| pkcs8 0.11.0 | L; N heading | Unverified | D | Release/counsel |
| pkg-config 0.3.33 | L; N heading | Unverified | D | Release/counsel |
| plist 1.10.0 | L; N heading | Unverified | D | Release/counsel |
| png 0.18.1 | L; N heading | Unverified | D | Release/counsel |
| polycool 0.4.0 | L; N heading | Unverified | D | Release/counsel |
| polyval 0.6.2 | L; N heading | Unverified | D | Release/counsel |
| polyval 0.7.3 | L; N heading | Unverified | D | Release/counsel |
| pom 1.1.0 | L; N heading | Unverified | D | Release/counsel |
| portable-atomic 1.13.1 | L; N heading | Unverified | D | Release/counsel |
| portable-atomic-util 0.2.7 | L; N heading | Unverified | D | Release/counsel |
| postcard 1.1.3 | L; N heading | Unverified | D | Release/counsel |
| postscript 0.14.1 | L; N heading | Unverified | D | Release/counsel |
| potential_utf 0.1.5 | L; N heading | Unverified | D | Release/counsel |
| powerfmt 0.2.0 | L; N heading | Unverified | D | Release/counsel |
| ppv-lite86 0.2.21 | L; N heading | Unverified | D | Release/counsel |
| precomputed-hash 0.1.1 | L; N heading | Unverified | D | Release/counsel |
| predicates 3.1.4 | L; N heading | Unverified | D | Release/counsel |
| predicates-core 1.0.10 | L; N heading | Unverified | D | Release/counsel |
| predicates-tree 1.0.13 | L; N heading | Unverified | D | Release/counsel |
| prettyplease 0.2.37 | L; N heading | Unverified | D | Release/counsel |
| primal-check 0.3.4 | L; N heading | Unverified | D | Release/counsel |
| primefield 0.14.0 | L; N heading | Unverified | D | Release/counsel |
| primeorder 0.13.6 | L; N heading | Unverified | D | Release/counsel |
| primeorder 0.14.0 | L; N heading | Unverified | D | Release/counsel |
| proc-macro-crate 0.1.5 | L; N heading | Unverified | D | Release/counsel |
| proc-macro-crate 3.5.0 | L; N heading | Unverified | D | Release/counsel |
| proc-macro-hack 0.5.20+deprecated | L; N heading | Unverified | D | Release/counsel |
| proc-macro2 1.0.106 | L; N heading | Unverified | D | Release/counsel |
| proc-macro2-diagnostics 0.10.1 | L; N heading | Unverified | D | Release/counsel |
| profiling 1.0.18 | L; N gap | Unverified | D | Release/counsel |
| profiling-procmacros 1.0.18 | L; N gap | Unverified | D | Release/counsel |
| prost 0.14.4 | L; N heading | Unverified | D | Release/counsel |
| prost-derive 0.14.4 | L; N heading | Unverified | D | Release/counsel |
| prost-types 0.14.4 | L; N heading | Unverified | D | Release/counsel |
| psl-types 2.0.11 | L; N heading | Unverified | D | Release/counsel |
| psm 0.1.31 | L; N heading | Unverified | D | Release/counsel |
| ptr_meta 0.1.4 | L; N heading | Unverified | D | Release/counsel |
| ptr_meta_derive 0.1.4 | L; N heading | Unverified | D | Release/counsel |
| publicsuffix 2.3.0 | L; N heading | Unverified | D | Release/counsel |
| pulldown-cmark 0.13.4 | L; N heading | Unverified | D | Release/counsel |
| pulldown-cmark-escape 0.11.0 | L; N heading | Unverified | D | Release/counsel |
| pxfm 0.1.30 | L; N heading | Unverified | D | Release/counsel |
| quad-rand 0.2.3 | L; N gap | Unverified | D | Release/counsel |
| quick-error 2.0.1 | L; N heading | Unverified | D | Release/counsel |
| quick-xml 0.38.4 | L; N heading | Unverified | D | Release/counsel |
| quick-xml 0.41.0 | L; N heading | Unverified | D | Release/counsel |
| quick_cache 0.6.24 | L; N heading | Unverified | D | Release/counsel |
| quinn 0.11.11 | L; N heading | Unverified | D | Release/counsel |
| quinn-proto 0.11.16 | L; N heading | Unverified | D | Release/counsel |
| quinn-udp 0.5.15 | L; N heading | Unverified | D | Release/counsel |
| quote 1.0.46 | L; N heading | Unverified | D | Release/counsel |
| r-efi 5.3.0 | L; N gap | Unverified | D | Release/counsel |
| r-efi 6.0.0 | L; N gap | Unverified | D | Release/counsel |
| radium 0.7.0 | L; N heading | Unverified | D | Release/counsel |
| rand 0.10.3 | L; N heading | Unverified | D | Release/counsel |
| rand 0.7.3 | L; N heading | Unverified | D | Release/counsel |
| rand 0.8.7 | L; N heading | Unverified | D | Release/counsel |
| rand 0.9.5 | L; N heading | Unverified | D | Release/counsel |
| rand_chacha 0.2.2 | L; N heading | Unverified | D | Release/counsel |
| rand_chacha 0.3.1 | L; N heading | Unverified | D | Release/counsel |
| rand_chacha 0.9.0 | L; N heading | Unverified | D | Release/counsel |
| rand_core 0.10.1 | L; N heading | Unverified | D | Release/counsel |
| rand_core 0.5.1 | L; N heading | Unverified | D | Release/counsel |
| rand_core 0.6.4 | L; N heading | Unverified | D | Release/counsel |
| rand_core 0.9.5 | L; N heading | Unverified | D | Release/counsel |
| rand_distr 0.5.1 | L; N heading | Unverified | D | Release/counsel |
| rand_hc 0.2.0 | L; N heading | Unverified | D | Release/counsel |
| rand_pcg 0.10.2 | L; N heading | Unverified | D | Release/counsel |
| rangemap 1.7.1 | L; N heading | Unverified | D | Release/counsel |
| rav1e 0.8.1 | L; N heading | Unverified | D | Release/counsel |
| ravif 0.13.0 | L; N heading | Unverified | D | Release/counsel |
| raw-window-handle 0.6.2 | L; N heading | Unverified | D | Release/counsel |
| rawpointer 0.2.1 | L; N heading | Unverified | D | Release/counsel |
| rayon 1.12.0 | L; N heading | Unverified | D | Release/counsel |
| rayon-core 1.13.0 | L; N heading | Unverified | D | Release/counsel |
| read-fonts 0.39.2 | L; N heading | Unverified | D | Release/counsel |
| reblessive 0.4.3 | L; N heading | Unverified | D | Release/counsel |
| redox_syscall 0.5.18 | L; N heading | Unverified | D | Release/counsel |
| redox_users 0.5.2 | L; N heading | Unverified | D | Release/counsel |
| ref-cast 1.0.25 | L; N heading | Unverified | D | Release/counsel |
| ref-cast-impl 1.0.25 | L; N heading | Unverified | D | Release/counsel |
| regex 1.13.1 | L; N heading | Unverified | D | Release/counsel |
| regex-automata 0.4.16 | L; N heading | Unverified | D | Release/counsel |
| regex-lite 0.1.9 | L; N heading | Unverified | D | Release/counsel |
| regex-syntax 0.8.11 | L; N heading | Unverified | D | Release/counsel |
| regorus 0.12.0 | L; N heading | Unverified | D | Release/counsel |
| regress 0.10.5 | L; N heading | Unverified | D | Release/counsel |
| regress 0.11.1 | L; N heading | Unverified | D | Release/counsel |
| rend 0.4.2 | L; N heading | Unverified | D | Release/counsel |
| reqwest 0.12.28 | L; N heading | Unverified | D | Release/counsel |
| reqwest 0.13.5 | L; N heading | Unverified | D | Release/counsel |
| reqwest-middleware 0.4.2 | L; N heading | Unverified | D | Release/counsel |
| restate-sdk 0.12.1 | L; N heading | Unverified | D | Release/counsel |
| restate-sdk-macros 0.12.0 | L; N gap | Unverified | D | Release/counsel |
| restate-sdk-shared-core 7.0.3 | L; N heading | Unverified | D | Release/counsel |
| resvg 0.47.0 | L; N heading | Unverified | D | Release/counsel |
| revision 0.30.0 | L; N heading | Unverified | D | Release/counsel |
| revision-derive 0.30.0 | L; N heading | Unverified | D | Release/counsel |
| rfc6979 0.4.0 | L; N heading | Unverified | D | Release/counsel |
| rfc6979 0.6.0 | L; N heading | Unverified | D | Release/counsel |
| rgb 0.8.53 | L; N heading | Unverified | D | Release/counsel |
| ring 0.17.14 | L; N heading | Unverified | D | Release/counsel |
| rkyv 0.7.46 | L; N heading | Unverified | D | Release/counsel |
| rkyv_derive 0.7.46 | L; N heading | Unverified | D | Release/counsel |
| rmp 0.8.15 | L; N heading | Unverified | D | Release/counsel |
| rmp-serde 1.3.1 | L; N heading | Unverified | D | Release/counsel |
| roaring 0.11.4 | L; N heading | Unverified | D | Release/counsel |
| robust 1.2.0 | L; N heading | Unverified | D | Release/counsel |
| roman-numerals-rs 3.1.0 | L; N gap | Unverified | D | Release/counsel |
| roxmltree 0.20.0 | L; N heading | Unverified | D | Release/counsel |
| roxmltree 0.21.1 | L; N heading | Unverified | D | Release/counsel |
| rsa 0.9.10 | L; N heading | Unverified | D | Release/counsel |
| rstar 0.10.0 | L; N gap | Unverified | D | Release/counsel |
| rstar 0.11.0 | L; N gap | Unverified | D | Release/counsel |
| rstar 0.12.2 | L; N gap | Unverified | D | Release/counsel |
| rstar 0.8.4 | L; N gap | Unverified | D | Release/counsel |
| rstar 0.9.3 | L; N gap | Unverified | D | Release/counsel |
| rust-stemmers 1.2.0 | L; N heading | Unverified | D | Release/counsel |
| rust_decimal 1.42.1 | L; N heading | Unverified | D | Release/counsel |
| rustc-hash 1.1.0 | L; N heading | Unverified | D | Release/counsel |
| rustc-hash 2.1.3 | L; N heading | Unverified | D | Release/counsel |
| rustc_version 0.4.1 | L; N heading | Unverified | D | Release/counsel |
| rustfft 6.4.1 | L; N heading | Unverified | D | Release/counsel |
| rustix 1.1.4 | L; N heading | Unverified | D | Release/counsel |
| rustls 0.21.12 | L; N heading | Unverified | D | Release/counsel |
| rustls 0.23.45 | L; N heading | Unverified | D | Release/counsel |
| rustls-native-certs 0.8.4 | L; N heading | Unverified | D | Release/counsel |
| rustls-pki-types 1.15.0 | L; N heading | Unverified | D | Release/counsel |
| rustls-platform-verifier 0.7.0 | L; N heading | Unverified | D | Release/counsel |
| rustls-platform-verifier-android 0.1.1 | L; N gap | Unverified | D | Release/counsel |
| rustls-webpki 0.101.7 | L; N heading | Unverified | D | Release/counsel |
| rustls-webpki 0.103.15 | L; N heading | Unverified | D | Release/counsel |
| rustversion 1.0.23 | L; N heading | Unverified | D | Release/counsel |
| rustybuzz 0.20.1 | L; N heading | Unverified | D | Release/counsel |
| ryu 1.0.23 | L; N heading | Unverified | D | Release/counsel |
| salsa20 0.10.2 | L; N heading | Unverified | D | Release/counsel |
| same-file 1.0.6 | L; N heading | Unverified | D | Release/counsel |
| schannel 0.1.29 | L; N heading | Unverified | D | Release/counsel |
| schemars 0.8.22 | L; N heading | Unverified | D | Release/counsel |
| schemars 0.9.0 | L; N heading | Unverified | D | Release/counsel |
| schemars 1.2.1 | L; N heading | Unverified | D | Release/counsel |
| schemars_derive 0.8.22 | L; N heading | Unverified | D | Release/counsel |
| scopeguard 1.2.0 | L; N heading | Unverified | D | Release/counsel |
| scraper 0.27.0 | L; N heading | Unverified | D | Release/counsel |
| scrypt 0.11.0 | L; N heading | Unverified | D | Release/counsel |
| sct 0.7.1 | L; N heading | Unverified | D | Release/counsel |
| seahash 4.1.0 | L; N gap | Unverified | D | Release/counsel |
| sealed 0.6.0 | L; N heading | Unverified | D | Release/counsel |
| sec1 0.7.3 | L; N heading | Unverified | D | Release/counsel |
| sec1 0.8.1 | L; N heading | Unverified | D | Release/counsel |
| secrecy 0.10.3 | L; N heading | Unverified | D | Release/counsel |
| security-framework 3.7.0 | L; N heading | Unverified | D | Release/counsel |
| security-framework-sys 2.17.0 | L; N heading | Unverified | D | Release/counsel |
| seize 0.5.1 | L; N heading | Unverified | D | Release/counsel |
| selectors 0.38.0 | L; N gap | Unverified | D | Release/counsel |
| semver 1.0.28 | L; N heading | Unverified | D | Release/counsel |
| send_wrapper 0.6.0 | L; N heading | Unverified | D | Release/counsel |
| seq-macro 0.3.6 | L; N heading | Unverified | D | Release/counsel |
| serde 1.0.229 | L; N heading | Unverified | D | Release/counsel |
| serde-big-array 0.5.1 | L; N heading | Unverified | D | Release/counsel |
| serde-saphyr 0.0.29 | L; N heading | Unverified | D | Release/counsel |
| serde-value 0.7.0 | L; N heading | Unverified | D | Release/counsel |
| serde-wasm-bindgen 0.6.5 | L; N heading | Unverified | D | Release/counsel |
| serde_bytes 0.11.19 | L; N heading | Unverified | D | Release/counsel |
| serde_core 1.0.229 | L; N heading | Unverified | D | Release/counsel |
| serde_derive 1.0.229 | L; N heading | Unverified | D | Release/counsel |
| serde_derive_internals 0.29.1 | L; N heading | Unverified | D | Release/counsel |
| serde_json 1.0.151 | L; N heading | Unverified | D | Release/counsel |
| serde_path_to_error 0.1.20 | L; N heading | Unverified | D | Release/counsel |
| serde_qs 0.15.0 | L; N heading | Unverified | D | Release/counsel |
| serde_repr 0.1.20 | L; N heading | Unverified | D | Release/counsel |
| serde_spanned 0.6.9 | L; N heading | Unverified | D | Release/counsel |
| serde_spanned 1.1.1 | L; N heading | Unverified | D | Release/counsel |
| serde_tokenstream 0.2.3 | L; N heading | Unverified | D | Release/counsel |
| serde_urlencoded 0.7.1 | L; N heading | Unverified | D | Release/counsel |
| serde_with 3.21.0 | L; N heading | Unverified | D | Release/counsel |
| serde_with_macros 3.21.0 | L; N heading | Unverified | D | Release/counsel |
| serde_yaml 0.9.34+deprecated | L; N heading | Unverified | D | Release/counsel |
| serdect 0.4.3 | L; N heading | Unverified | D | Release/counsel |
| servo_arc 0.4.3 | L; N heading | Unverified | D | Release/counsel |
| sha1 0.10.7 | L; N heading | Unverified | D | Release/counsel |
| sha1 0.11.0 | L; N heading | Unverified | D | Release/counsel |
| sha1_smol 1.0.1 | L; N heading | Unverified | D | Release/counsel |
| sha2 0.10.9 | L; N heading | Unverified | D | Release/counsel |
| sha2 0.11.0 | L; N heading | Unverified | D | Release/counsel |
| sha2 0.9.9 | L; N heading | Unverified | D | Release/counsel |
| sha3 0.10.9 | L; N heading | Unverified | D | Release/counsel |
| sharded-slab 0.1.7 | L; N heading | Unverified | D | Release/counsel |
| shlex 2.0.1 | L; N heading | Unverified | D | Release/counsel |
| sif-itree 0.4.1 | L; N heading | Unverified | D | Release/counsel |
| signal-hook-registry 1.4.8 | L; N heading | Unverified | D | Release/counsel |
| signature 2.2.0 | L; N heading | Unverified | D | Release/counsel |
| signature 3.0.0 | L; N heading | Unverified | D | Release/counsel |
| simd-adler32 0.3.9 | L; N heading | Unverified | D | Release/counsel |
| simd_cesu8 1.2.0 | L; N heading | Unverified | D | Release/counsel |
| simd_helpers 0.1.0 | L; N gap | Unverified | D | Release/counsel |
| simdutf8 0.1.5 | L; N heading | Unverified | D | Release/counsel |
| simple_asn1 0.6.4 | L; N heading | Unverified | D | Release/counsel |
| simplecss 0.2.2 | L; N heading | Unverified | D | Release/counsel |
| siphasher 1.0.3 | L; N heading | Unverified | D | Release/counsel |
| skrifa 0.42.1 | L; N heading | Unverified | D | Release/counsel |
| slab 0.4.12 | L; N heading | Unverified | D | Release/counsel |
| sledgehammer_bindgen 0.6.0 | L; N gap | Unverified | D | Release/counsel |
| sledgehammer_bindgen_macro 0.6.5 | L; N gap | Unverified | D | Release/counsel |
| sledgehammer_utils 0.3.1 | L; N gap | Unverified | D | Release/counsel |
| slotmap 1.1.1 | L; N heading | Unverified | D | Release/counsel |
| smallvec 1.15.2 | L; N heading | Unverified | D | Release/counsel |
| smart-default 0.7.1 | L; N heading | Unverified | D | Release/counsel |
| smawk 0.3.3 | L; N heading | Unverified | D | Release/counsel |
| snap 1.1.1 | L; N heading | Unverified | D | Release/counsel |
| socket2 0.5.10 | L; N heading | Unverified | D | Release/counsel |
| socket2 0.6.5 | L; N heading | Unverified | D | Release/counsel |
| solana-account 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-account-info 2.3.0 | L; N gap | Unverified | D | Release/counsel |
| solana-address-lookup-table-interface 2.2.2 | L; N gap | Unverified | D | Release/counsel |
| solana-atomic-u64 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-attestation-service-client 1.0.9 | L; N gap | Unverified | D | Release/counsel |
| solana-big-mod-exp 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-bincode 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-blake3-hasher 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-borsh 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-clock 2.2.3 | L; N gap | Unverified | D | Release/counsel |
| solana-cpi 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-decode-error 2.3.0 | L; N gap | Unverified | D | Release/counsel |
| solana-define-syscall 2.3.0 | L; N gap | Unverified | D | Release/counsel |
| solana-epoch-rewards 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-epoch-schedule 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-example-mocks 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-feature-gate-interface 2.2.2 | L; N gap | Unverified | D | Release/counsel |
| solana-fee-calculator 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-hash 2.3.0 | L; N gap | Unverified | D | Release/counsel |
| solana-instruction 2.3.3 | L; N gap | Unverified | D | Release/counsel |
| solana-instructions-sysvar 2.2.2 | L; N gap | Unverified | D | Release/counsel |
| solana-keccak-hasher 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-last-restart-slot 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-loader-v2-interface 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-loader-v3-interface 5.0.0 | L; N gap | Unverified | D | Release/counsel |
| solana-loader-v4-interface 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-message 2.4.0 | L; N gap | Unverified | D | Release/counsel |
| solana-msg 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-native-token 2.3.0 | L; N gap | Unverified | D | Release/counsel |
| solana-nonce 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-program 2.3.0 | L; N gap | Unverified | D | Release/counsel |
| solana-program-entrypoint 2.3.0 | L; N gap | Unverified | D | Release/counsel |
| solana-program-error 2.2.2 | L; N gap | Unverified | D | Release/counsel |
| solana-program-memory 2.3.1 | L; N gap | Unverified | D | Release/counsel |
| solana-program-option 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-program-pack 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-pubkey 2.4.0 | L; N gap | Unverified | D | Release/counsel |
| solana-rent 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-sanitize 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-sdk-ids 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-sdk-macro 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-secp256k1-recover 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-serde-varint 2.2.2 | L; N gap | Unverified | D | Release/counsel |
| solana-serialize-utils 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-sha256-hasher 2.3.0 | L; N gap | Unverified | D | Release/counsel |
| solana-short-vec 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-slot-hashes 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-slot-history 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-stable-layout 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-stake-interface 1.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-system-interface 1.0.0 | L; N gap | Unverified | D | Release/counsel |
| solana-sysvar 2.3.0 | L; N gap | Unverified | D | Release/counsel |
| solana-sysvar-id 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-transaction-error 2.2.1 | L; N gap | Unverified | D | Release/counsel |
| solana-vote-interface 2.2.6 | L; N gap | Unverified | D | Release/counsel |
| spade 2.15.1 | L; N heading | Unverified | D | Release/counsel |
| spin 0.10.1 | L; N heading | Unverified | D | Release/counsel |
| spin 0.12.2 | L; N heading | Unverified | D | Release/counsel |
| spin 0.9.9 | L; N heading | Unverified | D | Release/counsel |
| spki 0.7.3 | L; N heading | Unverified | D | Release/counsel |
| spki 0.8.0 | L; N heading | Unverified | D | Release/counsel |
| stable_deref_trait 1.2.1 | L; N heading | Unverified | D | Release/counsel |
| stacker 0.1.24 | L; N heading | Unverified | D | Release/counsel |
| storekey 0.11.0 | L; N heading | Unverified | D | Release/counsel |
| storekey-derive 0.11.0 | L; N gap | Unverified | D | Release/counsel |
| strength_reduce 0.2.4 | L; N heading | Unverified | D | Release/counsel |
| strict-num 0.1.1 | L; N heading | Unverified | D | Release/counsel |
| string_cache 0.9.0 | L; N heading | Unverified | D | Release/counsel |
| string_cache_codegen 0.6.1 | L; N heading | Unverified | D | Release/counsel |
| stringprep 0.1.5 | L; N heading | Unverified | D | Release/counsel |
| strsim 0.11.1 | L; N heading | Unverified | D | Release/counsel |
| strum 0.27.2 | L; N heading | Unverified | D | Release/counsel |
| strum 0.28.0 | L; N heading | Unverified | D | Release/counsel |
| strum_macros 0.27.2 | L; N heading | Unverified | D | Release/counsel |
| strum_macros 0.28.0 | L; N heading | Unverified | D | Release/counsel |
| subsecond 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| subsecond-types 0.7.10 | L; N gap | Unverified | D | Release/counsel |
| subsetter 0.2.6 | L; N heading | Unverified | D | Release/counsel |
| subtle 2.6.1 | L; N heading | Unverified | D | Release/counsel |
| supports-color 2.1.0 | L; N heading | Unverified | D | Release/counsel |
| supports-color 3.0.2 | L; N heading | Unverified | D | Release/counsel |
| surrealdb 3.2.4 | L; N heading | Unverified | D | Release/counsel |
| surrealdb-collections 3.2.4 | L; N heading | Unverified | D | Release/counsel |
| surrealdb-core 3.2.4 | L; N heading | Unverified | D | Release/counsel |
| surrealdb-protocol 0.10.2 | L; N heading | Unverified | D | Release/counsel |
| surrealdb-strand 3.2.4 | L; N heading | Unverified | D | Release/counsel |
| surrealdb-types 3.2.4 | L; N heading | Unverified | D | Release/counsel |
| surrealdb-types-derive 3.2.4 | L; N heading | Unverified | D | Release/counsel |
| surrealmx 0.22.0 | L; N heading | Unverified | D | Release/counsel |
| svgtypes 0.16.1 | L; N heading | Unverified | D | Release/counsel |
| symphonia 0.6.1 | L; N heading | Unverified | D | Release/counsel |
| symphonia-bundle-flac 0.6.1 | L; N heading | Unverified | D | Release/counsel |
| symphonia-bundle-mp3 0.6.1 | L; N heading | Unverified | D | Release/counsel |
| symphonia-codec-aac 0.6.1 | L; N heading | Unverified | D | Release/counsel |
| symphonia-codec-adpcm 0.6.1 | L; N heading | Unverified | D | Release/counsel |
| symphonia-codec-alac 0.6.1 | L; N heading | Unverified | D | Release/counsel |
| symphonia-codec-pcm 0.6.1 | L; N heading | Unverified | D | Release/counsel |
| symphonia-codec-vorbis 0.6.1 | L; N heading | Unverified | D | Release/counsel |
| symphonia-common 0.6.1 | L; N heading | Unverified | D | Release/counsel |
| symphonia-core 0.6.1 | L; N heading | Unverified | D | Release/counsel |
| symphonia-format-caf 0.6.1 | L; N heading | Unverified | D | Release/counsel |
| symphonia-format-isomp4 0.6.1 | L; N heading | Unverified | D | Release/counsel |
| symphonia-format-mkv 0.6.1 | L; N heading | Unverified | D | Release/counsel |
| symphonia-format-ogg 0.6.1 | L; N heading | Unverified | D | Release/counsel |
| symphonia-format-riff 0.6.1 | L; N heading | Unverified | D | Release/counsel |
| symphonia-metadata 0.6.1 | L; N heading | Unverified | D | Release/counsel |
| syn 1.0.109 | L; N heading | Unverified | D | Release/counsel |
| syn 2.0.118 | L; N heading | Unverified | D | Release/counsel |
| syn 3.0.5 | L; N heading | Unverified | D | Release/counsel |
| sync_wrapper 1.0.2 | L; N heading | Unverified | D | Release/counsel |
| synstructure 0.13.2 | L; N heading | Unverified | D | Release/counsel |
| syntect 5.3.0 | L; N heading | Unverified | D | Release/counsel |
| synthez 0.4.0 | L; N heading | Unverified | D | Release/counsel |
| synthez-codegen 0.4.0 | L; N gap | Unverified | D | Release/counsel |
| synthez-core 0.4.0 | L; N gap | Unverified | D | Release/counsel |
| sysinfo 0.37.2 | L; N heading | Unverified | D | Release/counsel |
| system-configuration 0.7.0 | L; N heading | Unverified | D | Release/counsel |
| system-configuration-sys 0.6.0 | L; N heading | Unverified | D | Release/counsel |
| tagptr 0.2.0 | L; N heading | Unverified | D | Release/counsel |
| tap 1.0.1 | L; N heading | Unverified | D | Release/counsel |
| tar 0.4.46 | L; N heading | Unverified | D | Release/counsel |
| tempfile 3.27.0 | L; N heading | Unverified | D | Release/counsel |
| tendril 0.5.1 | L; N heading | Unverified | D | Release/counsel |
| terminal_size 0.4.4 | L; N heading | Unverified | D | Release/counsel |
| termtree 0.5.1 | L; N heading | Unverified | D | Release/counsel |
| textwrap 0.16.2 | L; N heading | Unverified | D | Release/counsel |
| thin-vec 0.2.18 | L; N heading | Unverified | D | Release/counsel |
| thiserror 1.0.69 | L; N heading | Unverified | D | Release/counsel |
| thiserror 2.0.21 | L; N heading | Unverified | D | Release/counsel |
| thiserror-impl 1.0.69 | L; N heading | Unverified | D | Release/counsel |
| thiserror-impl 2.0.21 | L; N heading | Unverified | D | Release/counsel |
| thread_local 1.1.10 | L; N heading | Unverified | D | Release/counsel |
| thrift 0.17.0 | L; N gap | Unverified | D | Release/counsel |
| time 0.3.53 | L; N heading | Unverified | D | Release/counsel |
| time-core 0.1.9 | L; N heading | Unverified | D | Release/counsel |
| time-macros 0.2.31 | L; N heading | Unverified | D | Release/counsel |
| tiny-keccak 2.0.2 | L; N heading | Unverified | D | Release/counsel |
| tiny-skia 0.12.0 | L; N heading | Unverified | D | Release/counsel |
| tiny-skia-path 0.12.0 | L; N heading | Unverified | D | Release/counsel |
| tinystr 0.7.6 | L; N heading | Unverified | D | Release/counsel |
| tinystr 0.8.3 | L; N heading | Unverified | D | Release/counsel |
| tinyvec 1.12.0 | L; N heading | Unverified | D | Release/counsel |
| tinyvec_macros 0.1.1 | L; N heading | Unverified | D | Release/counsel |
| tokio 1.53.1 | L; N heading | Unverified | D | Release/counsel |
| tokio-macros 2.7.0 | L; N heading | Unverified | D | Release/counsel |
| tokio-native-tls 0.3.1 | L; N heading | Unverified | D | Release/counsel |
| tokio-rustls 0.24.1 | L; N heading | Unverified | D | Release/counsel |
| tokio-rustls 0.26.4 | L; N heading | Unverified | D | Release/counsel |
| tokio-stream 0.1.18 | L; N heading | Unverified | D | Release/counsel |
| tokio-tungstenite 0.28.0 | L; N heading | Unverified | D | Release/counsel |
| tokio-tungstenite 0.29.0 | L; N heading | Unverified | D | Release/counsel |
| tokio-tungstenite-wasm 0.8.2 | L; N heading | Unverified | D | Release/counsel |
| tokio-util 0.7.18 | L; N heading | Unverified | D | Release/counsel |
| toml 0.5.11 | L; N heading | Unverified | D | Release/counsel |
| toml 0.8.23 | L; N heading | Unverified | D | Release/counsel |
| toml 1.1.3+spec-1.1.0 | L; N heading | Unverified | D | Release/counsel |
| toml_datetime 0.6.11 | L; N heading | Unverified | D | Release/counsel |
| toml_datetime 1.1.1+spec-1.1.0 | L; N heading | Unverified | D | Release/counsel |
| toml_edit 0.22.27 | L; N heading | Unverified | D | Release/counsel |
| toml_edit 0.25.13+spec-1.1.0 | L; N heading | Unverified | D | Release/counsel |
| toml_parser 1.1.2+spec-1.1.0 | L; N heading | Unverified | D | Release/counsel |
| toml_write 0.1.2 | L; N heading | Unverified | D | Release/counsel |
| toml_writer 1.1.2+spec-1.1.0 | L; N heading | Unverified | D | Release/counsel |
| tonic 0.14.6 | L; N heading | Unverified | D | Release/counsel |
| tonic-prost 0.14.6 | L; N gap | Unverified | D | Release/counsel |
| tonic-types 0.14.6 | L; N heading | Unverified | D | Release/counsel |
| tower 0.5.3 | L; N heading | Unverified | D | Release/counsel |
| tower-cookies 0.11.0 | L; N heading | Unverified | D | Release/counsel |
| tower-http 0.6.11 | L; N heading | Unverified | D | Release/counsel |
| tower-http 0.7.1 | L; N heading | Unverified | D | Release/counsel |
| tower-layer 0.3.3 | L; N heading | Unverified | D | Release/counsel |
| tower-service 0.3.3 | L; N heading | Unverified | D | Release/counsel |
| tracing 0.1.44 | L; N heading | Unverified | D | Release/counsel |
| tracing-attributes 0.1.31 | L; N heading | Unverified | D | Release/counsel |
| tracing-core 0.1.36 | L; N heading | Unverified | D | Release/counsel |
| tracing-futures 0.2.5 | L; N heading | Unverified | D | Release/counsel |
| tracing-log 0.2.0 | L; N heading | Unverified | D | Release/counsel |
| tracing-opentelemetry 0.34.0 | L; N heading | Unverified | D | Release/counsel |
| tracing-serde 0.2.0 | L; N heading | Unverified | D | Release/counsel |
| tracing-subscriber 0.3.23 | L; N heading | Unverified | D | Release/counsel |
| tracing-wasm 0.2.1 | L; N heading | Unverified | D | Release/counsel |
| transpose 0.2.3 | L; N heading | Unverified | D | Release/counsel |
| trice 0.4.0 | L; N heading | Unverified | D | Release/counsel |
| try-lock 0.2.5 | L; N heading | Unverified | D | Release/counsel |
| ttf-parser 0.25.1 | L; N heading | Unverified | D | Release/counsel |
| tungstenite 0.27.0 | L; N heading | Unverified | D | Release/counsel |
| tungstenite 0.28.0 | L; N heading | Unverified | D | Release/counsel |
| tungstenite 0.29.0 | L; N heading | Unverified | D | Release/counsel |
| two-face 0.4.5 | L; N heading | Unverified | D | Release/counsel |
| twox-hash 2.1.2 | L; N heading | Unverified | D | Release/counsel |
| type1-encoding-parser 0.1.1 | L; N gap | Unverified | D | Release/counsel |
| typed-arena 2.0.2 | L; N heading | Unverified | D | Release/counsel |
| typed-builder 0.20.1 | L; N heading | Unverified | D | Release/counsel |
| typed-builder 0.23.2 | L; N heading | Unverified | D | Release/counsel |
| typed-builder-macro 0.20.1 | L; N heading | Unverified | D | Release/counsel |
| typed-builder-macro 0.23.2 | L; N heading | Unverified | D | Release/counsel |
| typed-path 0.12.3 | L; N heading | Unverified | D | Release/counsel |
| typeid 1.0.3 | L; N heading | Unverified | D | Release/counsel |
| typenum 1.20.1 | L; N heading | Unverified | D | Release/counsel |
| typetag 0.2.22 | L; N heading | Unverified | D | Release/counsel |
| typetag-impl 0.2.22 | L; N heading | Unverified | D | Release/counsel |
| typify 0.6.2 | L; N gap | Unverified | D | Release/counsel |
| typify-impl 0.6.2 | L; N gap | Unverified | D | Release/counsel |
| typify-macro 0.6.2 | L; N gap | Unverified | D | Release/counsel |
| typst 0.15.1 | L; N gap | Unverified | D | Release/counsel |
| typst-as-lib 0.16.0 | L; N heading | Unverified | D | Release/counsel |
| typst-assets 0.15.1 | L; N heading | Unverified | D | Release/counsel |
| typst-eval 0.15.1 | L; N gap | Unverified | D | Release/counsel |
| typst-html 0.15.1 | L; N gap | Unverified | D | Release/counsel |
| typst-kit 0.15.1 | L; N gap | Unverified | D | Release/counsel |
| typst-layout 0.15.1 | L; N gap | Unverified | D | Release/counsel |
| typst-library 0.15.1 | L; N gap | Unverified | D | Release/counsel |
| typst-macros 0.15.1 | L; N gap | Unverified | D | Release/counsel |
| typst-pdf 0.15.1 | L; N gap | Unverified | D | Release/counsel |
| typst-realize 0.15.1 | L; N gap | Unverified | D | Release/counsel |
| typst-svg 0.15.1 | L; N gap | Unverified | D | Release/counsel |
| typst-syntax 0.15.1 | L; N gap | Unverified | D | Release/counsel |
| typst-timing 0.15.1 | L; N gap | Unverified | D | Release/counsel |
| typst-utils 0.15.1 | L; N gap | Unverified | D | Release/counsel |
| ucd-trie 0.1.7 | L; N heading | Unverified | D | Release/counsel |
| ulid 1.2.1 | L; N heading | Unverified | D | Release/counsel |
| unic-langid 0.9.6 | L; N heading | Unverified | D | Release/counsel |
| unic-langid-impl 0.9.6 | L; N heading | Unverified | D | Release/counsel |
| unic-langid-macros 0.9.6 | L; N heading | Unverified | D | Release/counsel |
| unic-langid-macros-impl 0.9.6 | L; N heading | Unverified | D | Release/counsel |
| unicase 2.9.0 | L; N heading | Unverified | D | Release/counsel |
| unicode-bidi 0.3.18 | L; N heading | Unverified | D | Release/counsel |
| unicode-bidi-mirroring 0.4.0 | L; N heading | Unverified | D | Release/counsel |
| unicode-ccc 0.4.0 | L; N heading | Unverified | D | Release/counsel |
| unicode-ident 1.0.24 | L; N heading | Unverified | D | Release/counsel |
| unicode-linebreak 0.1.5 | L; N heading | Unverified | D | Release/counsel |
| unicode-math-class 0.1.0 | L; N heading | Unverified | D | Release/counsel |
| unicode-normalization 0.1.25 | L; N heading | Unverified | D | Release/counsel |
| unicode-properties 0.1.4 | L; N heading | Unverified | D | Release/counsel |
| unicode-script 0.5.8 | L; N heading | Unverified | D | Release/counsel |
| unicode-segmentation 1.13.3 | L; N heading | Unverified | D | Release/counsel |
| unicode-vo 0.1.0 | L; N heading | Unverified | D | Release/counsel |
| unicode-width 0.2.2 | L; N heading | Unverified | D | Release/counsel |
| unicode-xid 0.2.6 | L; N heading | Unverified | D | Release/counsel |
| universal-hash 0.5.1 | L; N heading | Unverified | D | Release/counsel |
| universal-hash 0.6.1 | L; N heading | Unverified | D | Release/counsel |
| unsafe-libyaml 0.2.11 | L; N heading | Unverified | D | Release/counsel |
| unscanny 0.1.0 | L; N heading | Unverified | D | Release/counsel |
| untrusted 0.7.1 | L; N heading | Unverified | D | Release/counsel |
| untrusted 0.9.0 | L; N heading | Unverified | D | Release/counsel |
| unty 0.0.4 | L; N heading | Unverified | D | Release/counsel |
| url 2.5.8 | L; N heading | Unverified | D | Release/counsel |
| urlencoding 2.1.3 | L; N heading | Unverified | D | Release/counsel |
| usvg 0.47.0 | L; N heading | Unverified | D | Release/counsel |
| utf-8 0.7.6 | L; N heading | Unverified | D | Release/counsel |
| utf16_iter 1.0.5 | L; N heading | Unverified | D | Release/counsel |
| utf8_iter 1.0.4 | L; N heading | Unverified | D | Release/counsel |
| utf8parse 0.2.2 | L; N heading | Unverified | D | Release/counsel |
| uuid 1.26.1 | L; N heading | Unverified | D | Release/counsel |
| v_frame 0.3.9 | L; N heading | Unverified | D | Release/counsel |
| valuable 0.1.1 | L; N gap | Unverified | D | Release/counsel |
| vart 0.9.3 | L; N heading | Unverified | D | Release/counsel |
| vcpkg 0.2.15 | L; N heading | Unverified | D | Release/counsel |
| vello_common 0.0.8 | L; N heading | Unverified | D | Release/counsel |
| vello_common 0.0.9 | L; N heading | Unverified | D | Release/counsel |
| vello_cpu 0.0.8 | L; N heading | Unverified | D | Release/counsel |
| version_check 0.9.5 | L; N heading | Unverified | D | Release/counsel |
| verus_builtin 0.0.0-2026-08-09-0044 | L; N gap | Unverified | D | Release/counsel |
| verus_builtin_macros 0.0.0-2026-08-23-0033 | L; N gap | Unverified | D | Release/counsel |
| verus_prettyplease 0.0.0-2026-08-09-0044 | L; N heading | Unverified | D | Release/counsel |
| verus_state_machines_macros 0.0.0-2026-08-02-0125 | L; N gap | Unverified | D | Release/counsel |
| verus_syn 0.0.0-2026-08-02-0125 | L; N heading | Unverified | D | Release/counsel |
| virtue 0.0.18 | L; N heading | Unverified | D | Release/counsel |
| vsimd 0.8.0 | L; N gap | Unverified | D | Release/counsel |
| vstd 0.0.0-2026-08-23-0033 | L; N gap | Unverified | D | Release/counsel |
| wait-timeout 0.2.1 | L; N heading | Unverified | D | Release/counsel |
| walkdir 2.5.0 | L; N heading | Unverified | D | Release/counsel |
| want 0.3.1 | L; N heading | Unverified | D | Release/counsel |
| warnings 0.2.1 | L; N gap | Unverified | D | Release/counsel |
| warnings-macro 0.2.0 | L; N gap | Unverified | D | Release/counsel |
| wasi 0.11.1+wasi-snapshot-preview1 | L; N heading | Unverified | D | Release/counsel |
| wasi 0.9.0+wasi-snapshot-preview1 | L; N heading | Unverified | D | Release/counsel |
| wasip2 1.0.4+wasi-0.2.12 | L; N heading | Unverified | D | Release/counsel |
| wasm-bindgen 0.2.126 | L; N heading | Unverified | D | Release/counsel |
| wasm-bindgen-futures 0.4.76 | L; N heading | Unverified | D | Release/counsel |
| wasm-bindgen-macro 0.2.126 | L; N heading | Unverified | D | Release/counsel |
| wasm-bindgen-macro-support 0.2.126 | L; N heading | Unverified | D | Release/counsel |
| wasm-bindgen-shared 0.2.126 | L; N heading | Unverified | D | Release/counsel |
| wasm-streams 0.4.2 | L; N heading | Unverified | D | Release/counsel |
| wasm-streams 0.5.0 | L; N heading | Unverified | D | Release/counsel |
| wasmi 1.1.0 | L; N gap | Unverified | D | Release/counsel |
| wasmi_collections 1.1.0 | L; N gap | Unverified | D | Release/counsel |
| wasmi_core 1.1.0 | L; N gap | Unverified | D | Release/counsel |
| wasmi_ir 1.1.0 | L; N gap | Unverified | D | Release/counsel |
| wasmparser 0.239.0 | L; N gap | Unverified | D | Release/counsel |
| wasmtimer 0.4.3 | L; N heading | Unverified | D | Release/counsel |
| web-sys 0.3.103 | L; N heading | Unverified | D | Release/counsel |
| web-time 1.1.0 | L; N heading | Unverified | D | Release/counsel |
| web_atoms 0.2.5 | L; N heading | Unverified | D | Release/counsel |
| webdriver 0.53.0 | L; N gap | Unverified | D | Release/counsel |
| webp 0.3.1 | L; N heading | Unverified | D | Release/counsel |
| webpki-root-certs 1.0.8 | L; N heading | Unverified | D | Release/counsel |
| webpki-roots 0.26.11 | L; N heading | Unverified | D | Release/counsel |
| webpki-roots 1.0.8 | L; N heading | Unverified | D | Release/counsel |
| weezl 0.1.12 | L; N heading | Unverified | D | Release/counsel |
| weezl 0.2.1 | L; N heading | Unverified | D | Release/counsel |
| winapi 0.3.9 | L; N heading | Unverified | D | Release/counsel |
| winapi-i686-pc-windows-gnu 0.4.0 | L; N gap | Unverified | D | Release/counsel |
| winapi-util 0.1.11 | L; N heading | Unverified | D | Release/counsel |
| winapi-x86_64-pc-windows-gnu 0.4.0 | L; N gap | Unverified | D | Release/counsel |
| windows 0.61.3 | L; N heading | Unverified | D | Release/counsel |
| windows-collections 0.2.0 | L; N heading | Unverified | D | Release/counsel |
| windows-core 0.61.2 | L; N heading | Unverified | D | Release/counsel |
| windows-future 0.2.1 | L; N heading | Unverified | D | Release/counsel |
| windows-implement 0.60.2 | L; N heading | Unverified | D | Release/counsel |
| windows-interface 0.59.3 | L; N heading | Unverified | D | Release/counsel |
| windows-link 0.1.3 | L; N heading | Unverified | D | Release/counsel |
| windows-link 0.2.1 | L; N heading | Unverified | D | Release/counsel |
| windows-numerics 0.2.0 | L; N heading | Unverified | D | Release/counsel |
| windows-registry 0.6.1 | L; N heading | Unverified | D | Release/counsel |
| windows-result 0.3.4 | L; N heading | Unverified | D | Release/counsel |
| windows-result 0.4.1 | L; N heading | Unverified | D | Release/counsel |
| windows-strings 0.4.2 | L; N heading | Unverified | D | Release/counsel |
| windows-strings 0.5.1 | L; N heading | Unverified | D | Release/counsel |
| windows-sys 0.45.0 | L; N heading | Unverified | D | Release/counsel |
| windows-sys 0.52.0 | L; N heading | Unverified | D | Release/counsel |
| windows-sys 0.61.2 | L; N heading | Unverified | D | Release/counsel |
| windows-targets 0.42.2 | L; N heading | Unverified | D | Release/counsel |
| windows-targets 0.52.6 | L; N heading | Unverified | D | Release/counsel |
| windows-threading 0.1.0 | L; N heading | Unverified | D | Release/counsel |
| windows_aarch64_gnullvm 0.42.2 | L; N heading | Unverified | D | Release/counsel |
| windows_aarch64_gnullvm 0.52.6 | L; N heading | Unverified | D | Release/counsel |
| windows_aarch64_msvc 0.42.2 | L; N heading | Unverified | D | Release/counsel |
| windows_aarch64_msvc 0.52.6 | L; N heading | Unverified | D | Release/counsel |
| windows_i686_gnu 0.42.2 | L; N heading | Unverified | D | Release/counsel |
| windows_i686_gnu 0.52.6 | L; N heading | Unverified | D | Release/counsel |
| windows_i686_gnullvm 0.52.6 | L; N heading | Unverified | D | Release/counsel |
| windows_i686_msvc 0.42.2 | L; N heading | Unverified | D | Release/counsel |
| windows_i686_msvc 0.52.6 | L; N heading | Unverified | D | Release/counsel |
| windows_x86_64_gnu 0.42.2 | L; N heading | Unverified | D | Release/counsel |
| windows_x86_64_gnu 0.52.6 | L; N heading | Unverified | D | Release/counsel |
| windows_x86_64_gnullvm 0.42.2 | L; N heading | Unverified | D | Release/counsel |
| windows_x86_64_gnullvm 0.52.6 | L; N heading | Unverified | D | Release/counsel |
| windows_x86_64_msvc 0.42.2 | L; N heading | Unverified | D | Release/counsel |
| windows_x86_64_msvc 0.52.6 | L; N heading | Unverified | D | Release/counsel |
| winnow 0.7.15 | L; N heading | Unverified | D | Release/counsel |
| winnow 1.0.4 | L; N heading | Unverified | D | Release/counsel |
| wiremock 0.6.5 | L; N heading | Unverified | D | Release/counsel |
| wit-bindgen 0.57.1 | L; N heading | Unverified | D | Release/counsel |
| wnaf 0.14.0 | L; N heading | Unverified | D | Release/counsel |
| write-fonts 0.48.1 | L; N heading | Unverified | D | Release/counsel |
| write16 1.0.0 | L; N heading | Unverified | D | Release/counsel |
| writeable 0.5.5 | L; N heading | Unverified | D | Release/counsel |
| writeable 0.6.3 | L; N heading | Unverified | D | Release/counsel |
| wyz 0.5.1 | L; N heading | Unverified | D | Release/counsel |
| xattr 1.6.1 | L; N heading | Unverified | D | Release/counsel |
| xmlparser 0.13.6 | L; N heading | Unverified | D | Release/counsel |
| xmlwriter 0.1.0 | L; N heading | Unverified | D | Release/counsel |
| xmp-writer 0.3.3 | L; N heading | Unverified | D | Release/counsel |
| xxhash-rust 0.8.18 | L; N heading | Unverified | D | Release/counsel |
| y4m 0.8.0 | L; N heading | Unverified | D | Release/counsel |
| yaml-rust 0.4.5 | L; N heading | Unverified | D | Release/counsel |
| yoke 0.7.5 | L; N heading | Unverified | D | Release/counsel |
| yoke 0.8.3 | L; N heading | Unverified | D | Release/counsel |
| yoke-derive 0.7.5 | L; N heading | Unverified | D | Release/counsel |
| yoke-derive 0.8.2 | L; N heading | Unverified | D | Release/counsel |
| zerocopy 0.8.54 | L; N heading | Unverified | D | Release/counsel |
| zerocopy-derive 0.8.54 | L; N heading | Unverified | D | Release/counsel |
| zerofrom 0.1.8 | L; N heading | Unverified | D | Release/counsel |
| zerofrom-derive 0.1.7 | L; N heading | Unverified | D | Release/counsel |
| zeroize 1.9.0 | L; N heading | Unverified | D | Release/counsel |
| zeroize_derive 1.5.0 | L; N heading | Unverified | D | Release/counsel |
| zerotrie 0.2.4 | L; N heading | Unverified | D | Release/counsel |
| zerovec 0.10.4 | L; N heading | Unverified | D | Release/counsel |
| zerovec 0.11.6 | L; N heading | Unverified | D | Release/counsel |
| zerovec-derive 0.10.3 | L; N heading | Unverified | D | Release/counsel |
| zerovec-derive 0.11.3 | L; N heading | Unverified | D | Release/counsel |
| zip 8.6.0 | L; N heading | Unverified | D | Release/counsel |
| zlib-rs 0.6.6 | L; N heading | Unverified | D | Release/counsel |
| zmij 1.0.23 | L; N heading | Unverified | D | Release/counsel |
| zopfli 0.8.3 | L; N heading | Unverified | D | Release/counsel |
| zstd 0.13.3 | L; N heading | Unverified | D | Release/counsel |
| zstd-safe 7.2.4 | L; N heading | Unverified | D | Release/counsel |
| zstd-sys 2.0.16+zstd.1.5.7 | L; N heading | Unverified | D | Release/counsel |
| zune-core 0.5.1 | L; N heading | Unverified | D | Release/counsel |
| zune-jpeg 0.5.15 | L; N heading | Unverified | D | Release/counsel |

Baseline coverage: 1,337 source-bearing packages; 1,122 exact headings, 215 manifest-only entries, and 0 absent exact
entries. These historical counts do not establish notice sufficiency or current artifact coverage.

### NuGet inventory

Cargo notices do not inventory the Word adapter's NuGet graph. These lockfile entries establish versions and dependency
types but do not contain full license instruments. Release/counsel must obtain exact package license texts, copyright,
native/runtime obligations, and determine which packages ship; test dependencies still need source/package review.
Evidence Q1 is [WordAdapter.packages.lock.json](../../word/adapter/WordAdapter.packages.lock.json); Q2 is
[WordAdapter.Tests.packages.lock.json](../../word/adapter/WordAdapter.Tests.packages.lock.json).

| Item | Evidence | Redistribution status | Notice | Unresolved owner or counsel review |
| --- | --- | --- | --- | --- |
| DocumentFormat.OpenXml 3.5.1 | Q1 | Unverified | D | Release/counsel |
| Microsoft.NET.ILLink.Tasks 8.0.31 | Q1 | Unverified | D | Release/counsel |
| DocumentFormat.OpenXml.Framework 3.5.1 | Q1 | Unverified | D | Release/counsel |
| System.IO.Packaging 8.0.1 | Q1 | Unverified | D | Release/counsel |
| Microsoft.NET.Test.Sdk 17.13.0 | Q2 | Unverified | D | Release/counsel |
| xunit 2.9.3 | Q2 | Unverified | D | Release/counsel |
| xunit.runner.visualstudio 3.0.2 | Q2 | Unverified | D | Release/counsel |
| DocumentFormat.OpenXml 3.5.1 | Q2 | Unverified | D | Release/counsel |
| DocumentFormat.OpenXml.Framework 3.5.1 | Q2 | Unverified | D | Release/counsel |
| Microsoft.CodeCoverage 17.13.0 | Q2 | Unverified | D | Release/counsel |
| Microsoft.TestPlatform.ObjectModel 17.13.0 | Q2 | Unverified | D | Release/counsel |
| Microsoft.TestPlatform.TestHost 17.13.0 | Q2 | Unverified | D | Release/counsel |
| Newtonsoft.Json 13.0.1 | Q2 | Unverified | D | Release/counsel |
| System.IO.Packaging 8.0.1 | Q2 | Unverified | D | Release/counsel |
| System.Reflection.Metadata 1.6.0 | Q2 | Unverified | D | Release/counsel |
| xunit.abstractions 2.0.3 | Q2 | Unverified | D | Release/counsel |
| xunit.analyzers 1.18.0 | Q2 | Unverified | D | Release/counsel |
| xunit.assert 2.9.3 | Q2 | Unverified | D | Release/counsel |
| xunit.core 2.9.3 | Q2 | Unverified | D | Release/counsel |
| xunit.extensibility.core 2.9.3 | Q2 | Unverified | D | Release/counsel |
| xunit.extensibility.execution 2.9.3 | Q2 | Unverified | D | Release/counsel |
| navigator-word-adapter project | Q2; R | First-party scope asserted | R | Owner/release/counsel |

## Individual non-font asset inventory

Each B reference names the exact tracked file. Status and notice requirements inherit the corresponding asset category
above. Every unverified row needs owner provenance review or a verified replacement before a new publication. Swagger
assets retain their upstream notice review separately; the synthetic PDF needs embedded-asset review as well.

| Item | Evidence | Redistribution status | Notice | Unresolved owner or counsel review |
| --- | --- | --- | --- | --- |
| logo-neon-law.png | B1 | Unverified | R; U | Owner/release/counsel |
| logo.svg | B2 | Unverified | R; U | Owner/release/counsel |
| mark.svg | B3 | Unverified | R; U | Owner/release/counsel |
| cyber-android-qr.svg | B4 | Unverified | R; U | Owner/release/counsel |
| cyber-consultation-qr.svg | B5 | Unverified | R; U | Owner/release/counsel |
| cyber-injury-law.svg | B6 | Unverified | R; U | Owner/release/counsel |
| cyber-iphone-qr.svg | B7 | Unverified | R; U | Owner/release/counsel |
| cyber-phone-qr.svg | B8 | Unverified | R; U | Owner/release/counsel |
| cyber-testimonials-qr.svg | B9 | Unverified | R; U | Owner/release/counsel |
| logo.svg | B10 | Unverified | R; U | Owner/release/counsel |
| mark.svg | B11 | Unverified | R; U | Owner/release/counsel |
| logo.png | B12 | Unverified | R; U | Owner/release/counsel |
| logo.svg | B13 | Unverified | R; U | Owner/release/counsel |
| logo.svg | B14 | Unverified | R; U | Owner/release/counsel |
| door.svg | B15 | Unverified | R; U | Owner/release/counsel |
| mark.svg | B16 | Unverified | R; U | Owner/release/counsel |
| logo.png | B17 | Unverified | R; U | Owner/release/counsel |
| logo.svg | B18 | Unverified | R; U | Owner/release/counsel |
| logo.svg | B19 | Unverified | R; U | Owner/release/counsel |
| logo.svg | B20 | Unverified | R; U | Owner/release/counsel |
| vesta.png | B21 | Unverified | R; U | Owner/release/counsel |
| vesta.svg | B22 | Unverified | R; U | Owner/release/counsel |
| favicon.svg | B23 | Unverified | R; U | Owner/release/counsel |
| ferris.svg | B24 | Unverified | U | Owner/release/counsel |
| logo-foundation.png | B25 | Unverified | R; U | Owner/release/counsel |
| logo.png | B26 | Unverified | R; U | Owner/release/counsel |
| logo.svg | B27 | Unverified | R; U | Owner/release/counsel |
| navigator-wheel.svg | B28 | Unverified | R; U | Owner/release/counsel |
| notation-pen.svg | B29 | Unverified | R; U | Owner/release/counsel |
| favicon-16x16.png | B30 | Unverified | A; D | Owner/release/counsel |
| favicon-32x32.png | B31 | Unverified | A; D | Owner/release/counsel |
| google-oauth-browser-client-form.jpg | B32 | Unverified | U | Owner/release/counsel |
| synthetic-initial-case-assessment.pdf | B33 | Unverified | R; U | Owner/release/counsel |
| logo.png | B34 | Unverified | R; U; F | Owner/release/counsel |
| logo.svg | B35 | Unverified | R; U; F | Owner/release/counsel |

B1: [logo-neon-law.png](../../pdf/assets/brand/logo-neon-law.png)

B2: [logo.svg](../../server/public/brand/abhaya/logo.svg)

B3: [mark.svg](../../server/public/brand/aegis/mark.svg)

B4: [cyber-android-qr.svg](../../server/public/brand/cyber-android-qr.svg)

B5: [cyber-consultation-qr.svg](../../server/public/brand/cyber-consultation-qr.svg)

B6: [cyber-injury-law.svg](../../server/public/brand/cyber-injury-law.svg)

B7: [cyber-iphone-qr.svg](../../server/public/brand/cyber-iphone-qr.svg)

B8: [cyber-phone-qr.svg](../../server/public/brand/cyber-phone-qr.svg)

B9: [cyber-testimonials-qr.svg](../../server/public/brand/cyber-testimonials-qr.svg)

B10: [logo.svg](../../server/public/brand/daybridge/logo.svg)

B11: [mark.svg](../../server/public/brand/death-and-divorce/mark.svg)

B12: [logo.png](../../server/public/brand/delete-your-data/logo.png)

B13: [logo.svg](../../server/public/brand/delete-your-data/logo.svg)

B14: [logo.svg](../../server/public/brand/delete-your-debt/logo.svg)

B15: [door.svg](../../server/public/brand/groundwork/door.svg)

B16: [mark.svg](../../server/public/brand/groundwork/mark.svg)

B17: [logo.png](../../server/public/brand/lawyer-shook/logo.png)

B18: [logo.svg](../../server/public/brand/lawyer-shook/logo.svg)

B19: [logo.svg](../../server/public/brand/misericordia/logo.svg)

B20: [logo.svg](../../server/public/brand/summons/logo.svg)

B21: [vesta.png](../../server/public/brand/vesta.png)

B22: [vesta.svg](../../server/public/brand/vesta.svg)

B23: [favicon.svg](../../server/public/favicon.svg)

B24: [ferris.svg](../../server/public/ferris.svg)

B25: [logo-foundation.png](../../server/public/logo-foundation.png)

B26: [logo.png](../../server/public/logo.png)

B27: [logo.svg](../../server/public/logo.svg)

B28: [navigator-wheel.svg](../../server/public/navigator-wheel.svg)

B29: [notation-pen.svg](../../server/public/notation-pen.svg)

B30: [favicon-16x16.png](../../server/public/swagger-ui/favicon-16x16.png)

B31: [favicon-32x32.png](../../server/public/swagger-ui/favicon-32x32.png)

B32: [workshop screenshot](../../server/public/workshops/navigator/google-oauth-browser-client-form.jpg)

B33: [synthetic-initial-case-assessment.pdf](../../cli/testdata/synthetic-initial-case-assessment.pdf)

B34: [logo.png](../../server/public/brand/guieb-shook/logo.png) — rendered from B35.

B35: [logo.svg](../../server/public/brand/guieb-shook/logo.svg) — glyph outlines drawn from the tracked Tinos Regular
WOFF2 (E32), so its font notice follows Tinos web Regular/Bold.

## Detailed matrix evidence

E1: Current CONTRIBUTORS.md contribution terms; baseline C and NOTICE contribution assertions

E2: Baseline C employment/contractor paragraph; current CONTRIBUTORS.md preserves separate signed agreements

E3: Baseline C outside-contributor paragraph; current CONTRIBUTING.md keeps outside intake closed

E4: R; C; templates/

E5: NOTICE government forms; forms catalog

E6: I; images/postgres/Containerfile

E7: I; images/postgres/Containerfile; review: Release/counsel: collect image notices

E8: images/postgres/NOTICE

E9: lsp/vscode-ext/package.json

E10: word/adapter/WordAdapter.csproj

E11: [archives/Cargo.toml](../../archives/Cargo.toml)

E12: [billing-workflows/Cargo.toml](../../billing-workflows/Cargo.toml)

E13: [features/Cargo.toml](../../features/Cargo.toml)

E14: [live-inquiry/Cargo.toml](../../live-inquiry/Cargo.toml)

E15: [telemetry/Cargo.toml](../../telemetry/Cargo.toml)

E16: [workflows/Cargo.toml](../../workflows/Cargo.toml)

E17: [workflows-service/Cargo.toml](../../workflows-service/Cargo.toml)

E18: vendor/sendgrid/README.md; LICENSE

E19: vendor/sendgrid/sendgrid_openapi.rs.template; review: Owner: determine source and applicable grant

E20: V; swagger-ui/swagger-ui.css

E21: V; swagger-ui/swagger-ui-bundle.js

E22: V; swagger-ui/swagger-ui-standalone-preset.js

E23: V; swagger-ui/favicon-*.png; review: Release: verify image grant and attribution

E24: V comments; swagger-ui/index.html; init.js

E25: server/public/js/plausible.js

E26: server/public/brand/; NOTICE trademarks; review: Owner/counsel: verify each brand and artwork

E27: `server/public/logo*`; `server/public/*.svg`

E28: pdf/assets/brand/logo-neon-law.png

E29: server/public/workshops/navigator/*.jpg

E30: cli/testdata/synthetic-initial-case-assessment.pdf

E31: server/public/css/; server/public/js/

E32: server/public/fonts/tinos/*.woff2; F12

E33: server/public/fonts/gorp-serif/LICENSE.txt

## Replacement path and unresolved decisions

1. Owner and counsel reconcile first-party authorship, assignments, imported material, and the separate image grant.
   Record clearance references without publishing signed agreements, personal information, or private instruments.
2. Release maintainers verify each vendored item and font against its exact bytes and prepare artifact-specific notices.
   Obtain missing Swagger, Pirata One, NuGet, and native-component records; reconcile the Cargo notice graph.
3. Owner selects replacements for any material whose grant cannot be established. Verify the replacement's provenance,
   license, notice delivery, and embedding/serving requirements before changing code or asset references.
4. Recheck first-party license and package metadata at the exact publication commit. Preserve third-party instruments
   and distinguish their scope. Owner/counsel resolves version-specific published grants and trademark permissions.
5. Refresh this source inventory at the publication candidate and inspect the snapshot, release archives, extension,
   images, and deployed assets against it. Release only artifacts whose evidence and notices have been reconciled.

## Validation boundaries

The historical matrix was compared with R, every workspace member manifest, extension metadata, SendGrid's LICENSE,
every tracked font license record, V, I, Cargo.lock, retained N entries, and both NuGet lockfiles. On the current review
basis, `navigator ops notices --check` confirms generated notice text matches the current lockfile inventory. The
license-of-record test protects current declarations; it does not verify signed grants, font purchases, external agency
terms, or legal clearance. The PII, secret, and public-snapshot scans cover tracked source only under their configured
detectors. The content gate and whitespace check validate this document's repository form. Run the following checks on
the publication candidate:

```bash
cargo test -p cli --test license_of_record
cargo test -p cli --test public_distribution --test release_cli_archive --test release_lsp_archive
cargo test -p cli --bin navigator public_snapshot
cargo run -p cli --quiet -- ops notices --check
cargo run -p cli --quiet -- validate
cargo run -p cli --quiet -- project gate
git diff --check
```

These commands test metadata, packaging, notices, and repository rules; they do not determine rights. Use the CI scan
steps in `ci.yml` for PII and secret checks because they provide the required pinned tools and dependencies. This audit
remains unresolved for publication until the responsible owner, release maintainer, and counsel complete the evidence
checks above. Recording this inventory grants no new rights.
