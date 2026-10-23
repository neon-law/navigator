# Synthetic portfolio acceptance walkthrough

## Fixture and target

The walkthrough applies synthetic portfolio fixture version 3 to an isolated Navigator KIND environment with the
explicit target `staging`. It applies the fixture twice and requires the second apply to create no rows or objects. The
acceptance adapter adds the bundled local Rauthy client and lawyer to selected synthetic matters, and adds one
internal-only synthetic document to the base matter. These adapter records are idempotent and remain in the disposable
worktree store.

The scenario definitions live in [`src/portfolio_acceptance.rs`](src/portfolio_acceptance.rs). The API runner, project
list/detail and invoice feature runners, the isolated trust-position runner, and the browser runner use the same IDs,
actors, routes, expected visible and absent facts, and screenshot names.

## Scenario record

| Scenario | Screenshot checkpoint | Result |
| --- | --- | --- |
| `firm.lifecycle-list` | `firm-lifecycle-list.png` | API and browser pass |
| `firm.closed-lifecycle-list` | `firm-closed-lifecycle-list.png` | API and browser pass |
| `client.participation-list` | `client-participation-list.png` | API and browser pass |
| `client.invoices-currencies` | `client-invoice-currencies.png` | API and browser pass |
| `client.trust-allocation` | `client-trust-allocation.png` | API and browser pass |
| `client.documents-portal` | `client-documents-portal.png` | API and browser pass |
| `client.offboarding-document` | `client-offboarding-document.png` | API and browser pass |
| `client.cross-project-denial` | `client-cross-project-denial.png` | API and browser pass |

The browser runner writes one screenshot for every scenario and a machine- readable report with the fixture version,
target, actor, route, HTTP result, visible and absent facts, screenshot checkpoint, and fixture apply counts. API
assertions use the same records and write their own report. Both reports mark all nine scenarios as passing. The API run
creates 91 records on its first apply and zero on its second; the second apply reports all 91 unchanged. The KIND run
confirms the same 91 existing records on both applies. That run predates ENG-1074, which retired the
`client.portal-access` scenario and the synthetic Portal bundle record with Portal serving.

## Run evidence

| Proof | Command | Result |
| --- | --- | --- |
| API acceptance | `cargo test -p features --test portfolio_acceptance` | Pass: 9/9 scenarios |
| Portal feature runners | List, detail, invoice card, and isolated trust-position | Pass: 12/13/8/1 scenarios |
| Existing trust feature | `cargo test -p features --test portal_trust_position` | Pass: 7/7 scenarios |
| Browser | `navigator dev browser-e2e` with pinned Chrome and required harness | Pass: 34/34 |
| Accessibility | Browser gate's required-harness axe suite | Pass: 12/12 |
| Repository gate | Repository CLI | Pass: 0 errors across 293 files, 179 YAML, 4 seeds, 34 locales |

The shell-installed `navigator validate` reports N108/N124 template metadata errors. The repository-built CLI passes
`cargo run -p cli --quiet -- validate` with zero errors and warnings.

Reports and PNG checkpoints are ephemeral under `/tmp/navigator-screenshots/eng-821/`; they are not committed. The
browser gate must run with the repository's required-harness setting so an unavailable browser cannot be reported as a
pass.

The report files are `api-report.json` and `browser-report.json`. They both identify fixture version 3, target
`staging`, actor email, route, status, visible and absent facts, checkpoint filename, and pass result. The browser
report also marks the fixture data synthetic. The API and browser runners share the same scenario table in
`src/portfolio_acceptance.rs`.
