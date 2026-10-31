//! Compatibility gate for the accepted Regorus policy-engine replacement.
//!
//! The portal-owned policy and its checked-in Rego rules share one behavioral
//! oracle under the deployed Regorus engine.

use std::path::{Path, PathBuf};

use regorus::{Engine, Value};
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli crate has a parent (the workspace root)")
        .to_path_buf()
}

fn test_rule_names(tests: &str) -> Vec<String> {
    tests
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            line.strip_prefix("test_")
                .and_then(|rest| rest.split_whitespace().next())
                .map(|name| format!("test_{name}"))
        })
        .collect()
}

fn assert_send_sync<T: Send + Sync>() {}

/// A Project reference grants nothing, so the policy has nothing to read
/// from it. This fails the moment a rule or its input names the relation.
/// The token is split so the guard does not match itself.
#[test]
fn the_policy_never_reads_project_references() {
    let relation = concat!("project_", "reference");
    let policy = std::fs::read_to_string(repo_root().join("portal/policy/navigator.rego"))
        .expect("read portal-owned policy");
    let rules: String = policy
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !rules.contains(relation),
        "navigator.rego must not read the {relation} relation: a reference grants no lens"
    );
    let input_builder = std::fs::read_to_string(repo_root().join("portal/src/policy.rs"))
        .expect("read the policy input builder");
    let builder = input_builder
        .split("#[cfg(test)]")
        .next()
        .unwrap_or_default();
    assert!(
        !builder.contains(relation),
        "the Rego input document must not carry {relation} rows"
    );
}

#[test]
fn regorus_matches_every_checked_in_policy_decision() {
    assert_send_sync::<regorus::CompiledPolicy>();

    let root = repo_root();
    let policy = std::fs::read_to_string(root.join("portal/policy/navigator.rego"))
        .expect("read portal-owned policy");
    let tests = include_str!("../../portal/policy/navigator_test.rego");
    let test_names = test_rule_names(tests);

    // 178 + 9 for the former `/app/docs` hub (since removed, below).
    //
    // + 4 for `/app/team`, the firm team home: each firm tier is admitted, and
    // the client and anonymous requests are denied.
    // + 3 for a Project's Portal at `/app/projects/{code}/portal`: a
    // client and a firm-side caller admitted, and an anonymous request denied.
    // Admission only — the portal is *participation*-scoped, and the ledger this
    // policy cannot read is what the handler consults, so a client who is not on
    // the matter is admitted here and answered 404 there. These three exist
    // because the portal is the first surface four path elements deep, and the
    // rule that admits it matches on the first two: a policy that silently
    // denied that depth would make the route unreachable in production while
    // every passthrough-policy test in the workspace still passed.
    // + 4 for the Owner/Admin matter directory at `/app/admin/projects`
    // (ENG-221): both admin tiers admitted, and Lawyer, Clerk, and
    // Client each denied. The path carries no rule of its own — it rides the
    // Owner/Admin route bypass, exactly as `/app/admin` does — so these
    // assertions are the only thing holding the deny-by-omission in place. The
    // Lawyer denial is the load-bearing one: a rule copied from `/app/lawyer`
    // would hand every lawyer the firm's whole matter list.
    //
    // The base is 178 rather than 183 because ENG-144 removed the reprice
    // command: `POST /app/api/projects/{id}/price-events` and its five decisions
    // (lawyer and admin admitted; client, clerk, and anonymous denied) went
    // with the price journal the route appended to.
    //
    // + 33 for consolidating the API under the private `/app/api` prefix, in
    // three groups.
    //
    // **Retired paths (3).** The two former top-level doc paths (`/api-docs`,
    // `/openapi.json`) must not keep an exemption now that nothing mounts
    // there, and the A2A agent card left the anonymous allowlist and must not
    // be re-opened by a rule here.
    //
    // **The documentation gate (13).** ENG-83's audience, decided by this
    // policy rather than by routing: the four operating tiers admitted at each
    // of the two documentation paths, plus `client` and anonymous denied at
    // each, plus the pair asserting a Clerk reads the reference plane but not
    // the directory it describes.
    //
    // **Per-resource reads (17).** The blanket any-authenticated
    // `GET /app/api/*` grant is gone, so each read is now named with its tier:
    // the CRM directory and the reference vocabularies at lawyer, raw Template
    // markdown deliberately wide, and — the property the blanket grant made
    // impossible — an unnamed read denied, so adding a GET endpoint fails
    // closed. Two decisions here *replace* rather than add: the former
    // `test_authenticated_get_api_allowed` and `test_clerk_get_api_allowed`
    // asserted a client and a clerk reaching the people directory, which is
    // exactly the behaviour removed.
    //
    // − 7 for making workshop reads public while keeping the certificate POST
    // policy-gated: the ten old workshop/presentation spillover cases became
    // three certificate-claim matrix cases.
    //
    // − 5 for retiring the `/portal` landing: the client, clerk, owner, admin,
    // and anonymous decisions on the exact `["portal"]` path went with the
    // route. The two functional `/portal/*` surfaces moved rather than
    // retired — `/portal/forms/*` → `/app/forms/*` and
    // `/portal/notations/.../documents` → `/app/notations/.../documents` — so
    // those decisions kept their count, only their path changed. (Base is now
    // 233.)
    //
    // + 125 for the API-parity doors, each proved by its five-case authorization
    // matrix (client-writable and admin-only doors vary which tier is admitted):
    //   + 5   DRI designate/clear (PUT/DELETE .../participants/{role_id}/dri)
    //   + 5   close matter (POST .../projects/{id}/close)
    //   + 10  notation request-changes + reask
    //   + 5   approve-plan (client-writable)
    //   + 5   conversation message (client-writable)
    //   + 10  expunge authorize (admin-only) + deny (lawyer)
    //   + 10  playbook create + update
    //   + 20  contract-review workbench (findings/summary/approve/reject)
    //   + 15  document upload + notation transcript coverage + estate transcript
    //   + 40  the #866 GET read clusters (five matter reads authenticated +
    //         handler-scoped; three firm-tool reads lawyer-tier)
    //   + 20  the remaining #866 GET reads: matter documents + conversation
    //         (authenticated, client-visible-filtered in the handler) and the
    //         expunge queue + a notation's review documents (lawyer-tier)
    // 233 + 134 = 367.
    //
    // + 6 for the admin-only reconciliation read (GET
    //   /app/api/project-repositories): the five-case tier matrix, plus one
    //   case pinning *why* it carries its own noun — the `projects` GET rule
    //   admits any authenticated caller up to five segments, so the same report
    //   nested there would be policy-reachable by a client.
    // 367 + 6 = 373.
    //
    // + 6 for the admin-only surfaces reconcile (POST
    //   /app/api/project-surfaces/{id}): the five-case tier matrix, plus one
    //   noun-isolation case matching project-repositories.
    // 373 + 6 = 379.
    //
    // + 4 for firm-administration listings under `/app/admin`: Lawyer reaches
    //   the named reference resources, and is denied Person CRUD, analytics,
    //   Clerk, and client on those same paths.
    // 379 + 4 = 383.
    //
    // − 2 for unmounting the Harvard-outline stage at `/app/lawyer/outline`.
    // 383 remains the last listing-and-prefix inventory before the `/app`
    // outline catalog.
    //
    // + 5 for `/app/outline` (Lawyer admitted, Clerk/client/anonymous denied)
    // and the matter-bound outline path under `/app/projects` (client admitted
    // by the ENG-81 prefix; the handler hides it from a Clerk).
    // 383 + 5 = 388.
    //
    // + 3 for Owner inheritance across participant add, participant edit, and
    //   DRI designation.
    // 388 + 3 = 391.
    //
    // − 11 for making the API documentation public. `/app/api`, its `/api`
    //   alias, and `/app/api/openapi.json` now mount with no session boundary
    //   and no `require_policy` layer at all, so the 13 decisions that used to
    //   pin their Clerk-and-above audience (four tiers reaching each of the
    //   two paths, `client` and anonymous denied at each, plus the
    //   Clerk-reads-the-reference-not-the-directory pair) no longer describe
    //   anything this policy decides. Two replace them, mirroring the
    //   `/app/api/mcp.json` card below: an anonymous read must not be allowed
    //   by this policy either, which is the only half a Rego test can prove —
    //   `portal/tests/router_contract.rs` covers the routing half.
    // 391 − 13 + 2 = 380.
    //
    // + 5 for the direct matter-lifecycle transition (POST
    //   /app/api/projects/{id}/lifecycle): the five-case tier matrix, same
    //   shape as `close matter` above.
    // 380 + 5 = 385.
    //
    // + 7 for the admin-only lifecycle projection (GET
    //   /app/api/project-lifecycle): Owner/Admin admitted, the other three
    //   tiers and anonymous denied, plus one noun-isolation case.
    // 385 + 7 = 392.
    //
    // + 6 for `/app/admin/brands`: Owner and Admin admitted; Lawyer, Clerk,
    //   client, and anonymous denied (the house-of-brands home rides the
    //   Owner/Admin bypass like `/app/admin/people`).
    // 392 + 6 = 398.
    //
    // + 6 for `/app/owner`: Owner admitted; Admin, Lawyer, Clerk, client, and
    //   anonymous denied.
    // 398 + 6 = 404.
    //
    // + 5 for pointer visibility reconciliation: Lawyer and Admin admitted;
    // Client, Clerk, and anonymous denied.
    // 404 + 5 = 409.
    //
    // + 5 for reading a document's revision chain (GET
    //   /app/api/projects/{id}/documents/revisions), for `navigator site
    //   document log`/`get` (#485): every authenticated tier admitted (the handler
    //   applies the caller's lens), anonymous denied.
    // 409 + 5 = 414.
    //
    // + 5 for filing an inbound message's attachments (POST
    //   /app/api/projects/{id}/mail/file), for `navigator site mail file`
    //   (#517): Lawyer and Admin admitted; Client, Clerk, and anonymous
    //   denied.
    // 414 + 5 = 419.
    //
    // + 8 for brand presentation edit: Owner and Admin reach
    //   `/app/admin/brands/{key}/edit` and `PATCH /app/api/brands/{key}`; Lawyer
    //   and Clerk are denied on both.
    // 419 + 8 = 427.
    //
    // + 1 for the client lens on the signing route,
    //   `/app/notations/{id}/sign` (ENG-557): the `/app/notations/**` rule
    //   constrains neither depth nor the third segment, so it already admits
    //   this path. The case pins that as intentional — the handler's
    //   participation + signer-identity gate is the entire authorization
    //   boundary there, so a narrowing of the rule must break this assertion
    //   rather than silently make the route unreachable in production.
    // 427 + 1 = 428.
    //
    // + 9 for `/app/profile`, the self-service profile page: every
    //   authenticated tier admitted at the page, at nested
    //   `/app/profile/avatar`, and at sibling `/app/avatar` (six page cases:
    //   five tiers plus anonymous denied; three upload cases: a client
    //   admitted on each POST path, anonymous denied on `/app/avatar` — the
    //   person id comes from the session, never the URL).
    // 428 + 9 = 437.
    //
    // + 6 for `/app/people/{id}/avatar`: every authenticated tier admitted at
    //   the route, anonymous denied. Rego has no participation data to narrow
    //   further; `store::access::avatar_visible_to` in the handler carries the
    //   actual, participation-scoped rule (docs/access-model.md).
    // 437 + 6 = 443.
    //
    // + 8 for the Firm integration doors, POST
    //   /app/api/integrations/{notion,slack}/{verb} (ENG-491): Admin and Owner
    //   admitted; Lawyer, Clerk, Client, and anonymous denied; one case pins
    //   the rule at exactly five segments and POST, and one pins the
    //   noun-isolation that keeps a provisioning door off the client-reachable
    //   `projects` prefix.
    // 443 + 8 = 451.
    //
    // + 11 for ENG-586's create-brand and logo/font upload doors
    //   (`/app/admin/brands/new`, `/app/admin/brands/{key}/logo`,
    //   `/app/admin/brands/{key}/font`): Owner and Admin admitted on each;
    //   Lawyer denied on each; Clerk and an anonymous caller each denied once
    //   more, on the create-brand door. Every one rides the Owner/Admin
    //   bypass, exactly as `/app/admin/projects` does — these assertions hold
    //   that deny-by-omission in place. ENG-585's firm create
    //   (`/app/owner/firms/new`, `owner_only_path`) and edit
    //   (`/app/admin/firms/{id}/edit`, the ordinary bypass) needed no test of
    //   their own here: both ride rules this file already asserts by shape,
    //   not by resource name.
    // 451 + 11 = 462.
    //
    // − 4 for deprecating the bundled Harvard-outline catalog at `/app/outline`.
    //   It read no store and served no matter — narrating a notation now lives
    //   only bound to a real matter, at `/app/projects/{code}/{id}/outline`,
    //   which rides the existing matter-surface rule and needs no rule of its
    //   own. The four `/app/outline` admission cases (Lawyer admitted; Clerk,
    //   client, and anonymous denied) no longer describe anything this policy
    //   decides.
    // 462 − 4 = 458.
    //
    // + 6 for the citation apparatus' write door, POST /app/api/authorities
    //   (ENG-712): Lawyer and Admin admitted; Clerk, Client, and anonymous
    //   denied; one case pins the rule at exactly the collection path and
    //   POST.
    // 458 + 6 = 464.
    //
    // + 3 for the lead queue (`/app/admin/leads` and `/app/admin/leads/{id}`):
    //   Owner and Admin admitted; Clerk and client denied. Lawyer denial is
    //   pinned on the existing people/analytics deny test rather than a fourth
    //   named case. Deny-by-omission, same as `/app/admin/projects`.
    // 464 + 3 = 467.
    //
    // + 4 for the authenticated `/start/{service_id}` door: the exact path
    // shape, its client and firm-tier policy cases, and the Clerk denial.
    // 467 + 4 = 471.
    //
    // + 3 for client testimonial submission: Client admitted, Lawyer and
    // Clerk denied. The handler resolves the named Project participation.
    // 471 + 3 = 474.
    //
    // + 8 for the remaining testimonial matrix: Owner/Admin route-admission
    // bypass on the API save (handler/store still refuse), anonymous save
    // denial, authenticated vs anonymous native-form admission under
    // `/app/projects`, lawyer-tier publish/unpublish, and Client, Clerk, and
    // anonymous publish denials.
    // 474 + 8 = 482.
    //
    // − 9 for retiring the `/app/docs` hub: it is now an anonymous redirect to
    // the public `/glossary`, mounted outside the policy layer, so its rules
    // and their nine decisions went with it.
    // 482 − 9 = 473.
    //
    // + 5 for document storage integrity (GET
    //   /app/api/projects/{id}/documents/integrity): Lawyer and Admin admitted;
    //   Client, Clerk, and anonymous denied.
    // 473 + 5 = 478.
    //
    // + 4 for restoring a missing document object (POST
    //   /app/api/projects/{id}/documents/{asset_id}/storage): Admin admitted
    //   through the route bypass; Lawyer, Client, and anonymous denied.
    // 478 + 4 = 482.
    //
    // + 5 for the admin matter testimonial role matrix: Owner/Admin admitted;
    // Lawyer, Client, and anonymous denied. Matter participation remains a
    // handler concern because it is not part of policy input.
    // 482 + 5 = 487.
    //
    // + 7 for matter-scoped admin avatar upload and clear: Owner/Admin
    // admitted, Client/Lawyer/anonymous denied for upload, and Admin admitted
    // while Client is denied for clear.
    // 487 + 7 = 494.
    // + 2 for the Project-surface read route: Owner/Admin may read the
    // aggregate handles; Lawyer/Clerk/Client/anonymous are denied.
    // 494 + 2 = 496.
    //
    // + 8 for the finance aggregate: the run door (POST
    // /app/api/projects/{id}/finance/run) admits Lawyer and Admin and denies
    // Client, Clerk, and anonymous; a GET on the run door is denied; the read
    // (GET /app/api/projects/{id}/finance) admits any authenticated session
    // and denies anonymous.
    // 496 + 8 = 504.
    //
    // + 5 for changing a document chain between accepted kinds (POST
    //   /app/api/projects/{id}/documents/{asset_id}/reclassify): Lawyer and
    //   Admin admitted; Clerk, Client, and anonymous denied.
    // 504 + 5 = 509.
    //
    // + 3 for reading an Authority's archive (GET
    //   /app/api/authorities/{id}/archive): Lawyer and Admin admitted; Clerk,
    //   Client, and anonymous denied; one case pins five segments and GET.
    // 509 + 3 = 512.
    //
    // + 14 for soft-deleting, restoring, and holding a document chain (POST
    //   /app/api/projects/{id}/documents/{asset_id}/{delete,restore,hold}):
    //   Lawyer admitted for each, Admin and the Clerk case pinned on delete,
    //   Client and anonymous denied for each; and for the retention purge
    //   (POST /app/api/documents/purge): Admin admitted, Lawyer and
    //   anonymous denied.
    // 512 + 14 = 526.
    //
    // + 3 for the Draft twins of the Template routes: raw source
    //   (GET /app/api/drafts/*path), the validator (POST
    //   /app/api/drafts/validate), and the admin listing (/app/admin/drafts)
    //   are admitted and refused tier for tier exactly as their `templates`
    //   spellings are.
    // 526 + 3 = 529.
    //
    // + 3 for requesting durable Project publisher setup: Lawyer admitted,
    //   Client denied, and route/method guards are exact.
    // 529 + 3 = 532.
    //
    // + 6 for the admin matter testimonial publish/unpublish door: Client,
    //   Lawyer and Clerk denied, anonymous denied, Admin and Owner admitted,
    //   each pinning both verbs.
    // 532 + 6 = 538.
    //
    // + 17 for the Deadline writes (LAW-126): POST /app/api/projects/{id}/deadlines,
    //   PATCH .../deadlines/{deadline_id}, and POST .../{complete,cancel} each
    //   admit Lawyer and deny Clerk, Client, and anonymous; an unknown verb
    //   segment is denied to Lawyer.
    // 538 + 17 = 555.
    //
    // + 5 for recording an E-Filing Transaction status (LAW-128): POST
    //   /app/api/projects/{id}/efilings admits Lawyer and denies Clerk, Client,
    //   and anonymous; a deeper POST path is denied to Lawyer.
    // 555 + 5 = 560.
    //
    // + 2 for the Owner inbox GET: Owner admitted and Admin denied.
    // 560 + 2 = 562.
    // + 1 for preset Owner credential routes across roles and methods.
    // 562 + 1 = 563.
    //
    // + 7 for recording adversity from a matter: POST
    //   /app/api/projects/{id}/relationships/adverse admits Lawyer, Admin, and
    //   Owner and denies Client, Clerk, and anonymous; a GET on the path and a
    //   deeper POST are denied to Lawyer.
    // 563 + 7 = 570.
    //
    // + 13 for the Docket Entry writes (LAW-133): POST
    //   /app/api/projects/{id}/docket and POST
    //   .../docket/{entry_id}/{link-document,supersede} each admit Lawyer and
    //   deny Clerk, Client, and anonymous; an unknown verb segment is denied
    //   to Lawyer.
    // 570 + 13 = 583.
    //
    // 583 + 8 = 591.
    //
    // + 8 for Service records (LAW-124): POST /app/api/projects/{id}/service
    //   admits Lawyer and denies Clerk, Client, and anonymous; GET
    //   .../service/{service_id} admits Lawyer and denies Client and
    //   anonymous; a deeper POST path is denied to Lawyer.
    // 583 + 8 = 591.
    //
    // 591 + 4 = 595.
    //
    // + 4 for logging a Communication (LAW-140): POST
    //   /app/api/projects/{id}/communications admits Lawyer and denies
    //   Clerk, Client, and anonymous.
    // 591 + 4 = 595.
    //
    // + 5 for the main-push notice (ENG-957): POST
    //   /app/api/projects/{id}/events/main-push admits Lawyer and Admin and
    //   denies Client, Clerk, and anonymous.
    // 595 + 5 = 600.
    // + 1 for the plural lawyer dashboard redirect and its role boundary.
    //
    // + 5 for storing Matter Pages: PUT /app/api/projects/{id}/pages admits
    //   Lawyer and denies Clerk, Client, and anonymous; a deeper PUT is denied
    //   to Lawyer.
    // 601 + 5 = 606.
    //
    // + 7 for the estate and cap-table reads: GET
    //   /app/api/projects/{id}/{fiduciary-roles,estate-assets,cap-table-holdings}
    //   admits Lawyer and Client and denies anonymous; a POST on the path is
    //   denied to Lawyer and Client, and a deeper GET is denied to Client.
    // + 5 for storing a Matter Caption: PUT /app/api/projects/{id}/matter
    //   admits Lawyer and denies Clerk, Client, and anonymous; a deeper PUT is
    //   denied to Lawyer.
    // 613 + 5 + one scoped CI section-count matrix = 619.
    // + 1 for the lawyer-tier Clauses pages (`/app/admin/clauses`) = 620.
    // - 2 for the retired Project-publisher route, whose three tests collapse
    //   into one proving it is no longer allowed = 618.
    // + 3 for the client purchase page (`/purchases`): a client reads it and
    //   buys one Offering; firm tiers and anonymous are denied; the rule is
    //   exact = 621.
    // + 2 for the Project calendar door: an Admin may ensure it and a Client
    //   may not = 623.
    // + 1 for filed Authority metadata: lawyer admission, lower-tier denial,
    //   and exact method/path checks share one matrix = 624.
    // + 3 for the relationship-graph views (`/app/api/graph/{view}/{id}`):
    //   lawyer admission for all three views, non-lawyer denial, and the
    //   read-only method/path-shape boundary = 627.
    assert_eq!(
        test_names.len(),
        627,
        "the policy decision inventory changed; review every new or removed rule"
    );

    let mut engine = Engine::new();
    engine
        .add_policy("navigator.rego".to_owned(), policy)
        .expect("Regorus parses the deployed policy");
    engine
        .add_policy("navigator_test.rego".to_owned(), tests.to_owned())
        .expect("Regorus parses the policy tests");

    let mut failures = Vec::new();
    for name in test_names {
        engine.set_input(Value::Undefined);
        let result = engine
            .eval_rule(format!("data.navigator.authz_test.{name}"))
            .unwrap_or_else(|error| panic!("Regorus could not evaluate {name}: {error}"));
        if result != Value::from(true) {
            failures.push(format!("{name} returned {result:?}"));
        }
    }

    assert!(
        failures.is_empty(),
        "Regorus policy parity failed:\n{}",
        failures.join("\n")
    );
}
