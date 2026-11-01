# Unit tests for the Neon Law Navigator authorization policy.
#
# These pin every decision the firm-wide access model promises — the
# allow rules AND the deny cases — directly against the production Rego
# source. `cli/tests/regorus_policy.rs` compiles both files with Regorus, so
# there is exactly one copy of the policy and nothing to drift.
#
# Run locally: cargo test -p cli --test regorus_policy

package navigator.authz_test

import rego.v1

import data.navigator.authz

# ---------- sessions ----------
# `session` is null when unauthenticated; otherwise it carries the
# singular `role` field the policy evaluates (post `roles[] → role` collapse).

owner_session := {"sub": "x", "email": "o@neonlaw.com", "exp": 9999999999, "role": "owner", "csrf_token": ""}

admin_session := {"sub": "x", "email": "a@neonlaw.com", "exp": 9999999999, "role": "admin", "csrf_token": ""}

lawyer_session := {"sub": "x", "email": "s@neonlaw.com", "exp": 9999999999, "role": "lawyer", "csrf_token": ""}

clerk_session := {"sub": "x", "email": "clerk@neonlaw.com", "exp": 9999999999, "role": "clerk", "csrf_token": ""}

client_session := {"sub": "x", "email": "c@example.com", "exp": 9999999999, "role": "client", "csrf_token": ""}

# ---------- Owner/Admin bypass ----------

# The bypass rule grants admin every path no other rule allows — pin it
# with a path that has no explicit allow rule of its own.
test_admin_bypass_reaches_unrouted_path if {
	authz.allow with input as {"path": ["not-a-mounted-surface"], "method": "GET", "session": admin_session}
}

test_owner_bypass_reaches_unrouted_path if {
	authz.allow with input as {"path": ["not-a-mounted-surface"], "method": "GET", "session": owner_session}
}

# ---------- /start/{service_id} ----------

test_authenticated_roles_reach_the_start_door if {
	authz.allow with input as {"path": ["start", "llc-file"], "method": "GET", "session": client_session}
	authz.allow with input as {"path": ["start", "llc-file"], "method": "POST", "session": lawyer_session}
}

test_clerk_cannot_reach_the_start_door if {
	not authz.allow with input as {"path": ["start", "llc-file"], "method": "GET", "session": clerk_session}
}

test_anonymous_cannot_reach_the_start_door if {
	not authz.allow with input as {"path": ["start", "llc-file"], "method": "GET", "session": null}
}

test_start_door_rule_is_exact if {
	not authz.allow with input as {"path": ["start", "llc-file", "extra"], "method": "GET", "session": client_session}
}

# ---------- /purchases ----------

test_client_reads_and_buys_on_the_purchase_page if {
	authz.allow with input as {"path": ["purchases"], "method": "GET", "session": client_session}
	authz.allow with input as {"path": ["purchases", "consultation"], "method": "POST", "session": client_session}
}

test_only_clients_reach_the_purchase_page if {
	not authz.allow with input as {"path": ["purchases"], "method": "GET", "session": lawyer_session}
	not authz.allow with input as {"path": ["purchases"], "method": "GET", "session": clerk_session}
	not authz.allow with input as {"path": ["purchases"], "method": "GET", "session": null}
}

test_purchase_rules_are_exact if {
	not authz.allow with input as {"path": ["purchases", "consultation"], "method": "GET", "session": client_session}
	not authz.allow with input as {"path": ["purchases"], "method": "POST", "session": client_session}
	not authz.allow with input as {"path": ["purchases", "consultation", "extra"], "method": "POST", "session": client_session}
}

# ---------- /app/nautilus ----------

# The Legal Sandbox carries the same client-only role contract as the
# purchase page it bills through: the page reads, the message route posts.
test_client_reads_the_sandbox_page_and_posts_a_prompt if {
	authz.allow with input as {"path": ["app", "nautilus"], "method": "GET", "session": client_session}
	authz.allow with input as {"path": ["app", "nautilus", "messages"], "method": "POST", "session": client_session}
}

test_the_sandbox_page_denies_everyone_else if {
	not authz.allow with input as {"path": ["app", "nautilus"], "method": "GET", "session": lawyer_session}
	not authz.allow with input as {"path": ["app", "nautilus"], "method": "GET", "session": clerk_session}
	not authz.allow with input as {"path": ["app", "nautilus"], "method": "GET", "session": null}
	not authz.allow with input as {"path": ["app", "nautilus", "messages"], "method": "POST", "session": lawyer_session}
	not authz.allow with input as {"path": ["app", "nautilus", "messages"], "method": "POST", "session": null}
}

test_sandbox_rules_are_exact if {
	not authz.allow with input as {"path": ["app", "nautilus"], "method": "POST", "session": client_session}
	not authz.allow with input as {"path": ["app", "nautilus", "messages"], "method": "GET", "session": client_session}
	not authz.allow with input as {"path": ["app", "nautilus", "messages", "extra"], "method": "POST", "session": client_session}
}

# ---------- /app/projects/* (any authenticated caller; ENG-81) ----------
# The policy admits every tier onto the one matter surface. Row scoping is the
# handler's job, so these tests pin admission only — the denial that matters
# (an Owner with no participation row) is asserted in `store::access` and in
# `server/tests/routes.rs`, where the ledger is readable.

test_client_reaches_projects if {
	authz.allow with input as {"path": ["app", "projects"], "method": "GET", "session": client_session}
}

test_lawyer_reaches_projects if {
	authz.allow with input as {"path": ["app", "projects"], "method": "GET", "session": lawyer_session}
}

test_clerk_reaches_projects if {
	authz.allow with input as {"path": ["app", "projects"], "method": "GET", "session": clerk_session}
}

test_anonymous_denied_on_projects if {
	not authz.allow with input as {"path": ["app", "projects"], "method": "GET", "session": null}
}

test_client_can_approve_plan if {
	authz.allow with input as {"path": ["app", "projects", "p1", "approve-plan"], "method": "POST", "session": client_session}
}

# A Project's Portal at `/app/projects/{code}/portal`. The rule matches
# on the first two path elements, so a four-element path relies on that prefix
# rather than on a rule of its own — pinned here because the portal mount is the
# first surface at that depth, and a policy that silently denied it would make
# the route unreachable in production while passing every passthrough-policy
# test in the workspace.
#
# Admission only, and deliberately so: the portal is *participation*-scoped, and
# the ledger this policy cannot read is what the handler consults. A client who
# is not on this matter is admitted here and answered 404 there.
test_client_reaches_a_project_portal if {
	authz.allow with input as {"path": ["app", "projects", "p1", "portal"], "method": "GET", "session": client_session}
}

test_lawyer_reaches_a_project_portal if {
	authz.allow with input as {"path": ["app", "projects", "p1", "portal"], "method": "GET", "session": lawyer_session}
}

test_anonymous_denied_on_a_project_portal if {
	not authz.allow with input as {"path": ["app", "projects", "p1", "portal"], "method": "GET", "session": null}
}

test_client_can_submit_contract_review if {
	authz.allow with input as {"path": ["app", "projects", "p1", "contract-review"], "method": "POST", "session": client_session}
}

# The old prefixes are gone, not redirected.
test_old_portal_projects_path_is_not_allowed if {
	not authz.allow with input as {"path": ["portal", "projects"], "method": "GET", "session": client_session}
}

test_old_lawyer_prefix_is_not_allowed if {
	not authz.allow with input as {"path": ["lawyer"], "method": "GET", "session": lawyer_session}
	not authz.allow with input as {"path": ["lawyer", "notations"], "method": "GET", "session": lawyer_session}
	not authz.allow with input as {"path": ["lawyer", "projects"], "method": "GET", "session": client_session}
}

# ---------- /app/forms/* (blank public forms) ----------

test_client_can_browse_forms if {
	authz.allow with input as {"path": ["app", "forms"], "method": "GET", "session": client_session}
}

test_anonymous_denied_on_forms if {
	not authz.allow with input as {"path": ["app", "forms"], "method": "GET", "session": null}
}

# ---------- /app/notations/* (client-lens notation documents) ----------

test_client_reaches_app_notation_document if {
	authz.allow with input as {"path": ["app", "notations", "n1", "documents", "d1"], "method": "GET", "session": client_session}
}

test_anonymous_denied_on_app_notations if {
	not authz.allow with input as {"path": ["app", "notations", "n1", "documents", "d1"], "method": "GET", "session": null}
}

# The `/app/notations/**` rule constrains neither the depth nor the third
# segment, so it admits `sign` as readily as `documents`. That is deliberate,
# and this case is what makes it deliberate rather than incidental: the
# handler gate is the entire authorization boundary for the signing route,
# so a future narrowing of the rule must break here rather than silently
# make the route unreachable in production.
test_client_reaches_the_app_notation_signing_route if {
	authz.allow with input as {"path": ["app", "notations", "n1", "sign"], "method": "GET", "session": client_session}
}

# ---------- /app/lawyer/* (lawyer tier only) ----------

test_lawyer_reaches_lawyer_notations if {
	authz.allow with input as {"path": ["app", "lawyer", "notations"], "method": "GET", "session": lawyer_session}
}

test_client_reaches_a_project_notation_outline if {
	authz.allow with input as {"path": ["app", "projects", "p1", "n1", "outline"], "method": "GET", "session": client_session}
}

test_admin_reaches_lawyer_notations if {
	authz.allow with input as {"path": ["app", "lawyer", "notations"], "method": "GET", "session": admin_session}
}

test_owner_reaches_lawyer_notations if {
	authz.allow with input as {"path": ["app", "lawyer", "notations"], "method": "GET", "session": owner_session}
}

test_client_denied_on_lawyer_notations if {
	not authz.allow with input as {"path": ["app", "lawyer", "notations"], "method": "GET", "session": client_session}
}

test_clerk_denied_on_lawyer_notations if {
	not authz.allow with input as {"path": ["app", "lawyer", "notations"], "method": "GET", "session": clerk_session}
}

test_anonymous_denied_on_app_lawyer if {
	not authz.allow with input as {"path": ["app", "lawyer"], "method": "GET", "session": null}
}

# The brand-font download rides the /app/team prefix rules above rather than a
# rule of its own, so it is asserted per tier here: all four firm tiers reach
# it, and the two unauthorized audiences do not.
test_lawyer_reaches_font_download if {
	authz.allow with input as {"path": ["app", "team", "fonts", "gorp-serif.zip"], "method": "GET", "session": lawyer_session}
}

test_admin_reaches_font_download if {
	authz.allow with input as {"path": ["app", "team", "fonts", "gorp-serif.zip"], "method": "GET", "session": admin_session}
}

test_owner_reaches_font_download if {
	authz.allow with input as {"path": ["app", "team", "fonts", "gorp-serif.zip"], "method": "GET", "session": owner_session}
}

test_clerk_reaches_font_download if {
	authz.allow with input as {"path": ["app", "team", "fonts", "gorp-serif.zip"], "method": "GET", "session": clerk_session}
}

test_client_denied_on_font_download if {
	not authz.allow with input as {"path": ["app", "team", "fonts", "gorp-serif.zip"], "method": "GET", "session": client_session}
}

test_anonymous_denied_on_font_download if {
	not authz.allow with input as {"path": ["app", "team", "fonts", "gorp-serif.zip"], "method": "GET", "session": null}
}

# Clerk is denied every /app/lawyer path, including the workbench listings.
test_clerk_denied_on_every_lawyer_path if {
	not authz.allow with input as {"path": ["app", "lawyer", "notations"], "method": "GET", "session": clerk_session}
	not authz.allow with input as {"path": ["app", "lawyer", "disclosures"], "method": "GET", "session": clerk_session}
}

# ---------- /app/lawyer and /app/admin (the two dashboards) ----------

test_lawyer_reaches_the_firm_dashboard if {
	authz.allow with input as {"path": ["app", "lawyer"], "method": "GET", "session": lawyer_session}
}

test_clerk_denied_on_the_firm_dashboard if {
	not authz.allow with input as {"path": ["app", "lawyer"], "method": "GET", "session": clerk_session}
}

test_client_denied_on_the_firm_dashboard if {
	not authz.allow with input as {"path": ["app", "lawyer"], "method": "GET", "session": client_session}
}

# The admin dashboard rides the Owner/Admin route bypass and has no rule of its
# own, so Lawyer must not reach it. This is the assertion that catches
# someone "fixing" the missing rule by copying the /app/lawyer one.
test_lawyer_denied_on_the_admin_dashboard if {
	not authz.allow with input as {"path": ["app", "admin"], "method": "GET", "session": lawyer_session}
}

test_lawyer_reaches_admin_reference_listings if {
	authz.allow with input as {"path": ["app", "admin", "entities"], "method": "GET", "session": lawyer_session}
	authz.allow with input as {"path": ["app", "admin", "entity-types"], "method": "GET", "session": lawyer_session}
	authz.allow with input as {"path": ["app", "admin", "playbooks", "new"], "method": "POST", "session": lawyer_session}
	authz.allow with input as {"path": ["app", "admin", "people.csv"], "method": "GET", "session": lawyer_session}
	authz.allow with input as {"path": ["app", "admin", "schedules", "archives", "run"], "method": "POST", "session": lawyer_session}
}

test_clauses_are_lawyer_tier_only if {
	authz.allow with input as {"path": ["app", "admin", "clauses"], "method": "GET", "session": lawyer_session}
	authz.allow with input as {"path": ["app", "admin", "clauses", "governing_law"], "method": "GET", "session": lawyer_session}
	authz.allow with input as {"path": ["app", "admin", "clauses"], "method": "GET", "session": admin_session}
	not authz.allow with input as {"path": ["app", "admin", "clauses"], "method": "GET", "session": client_session}
	not authz.allow with input as {"path": ["app", "admin", "clauses", "governing_law"], "method": "GET", "session": clerk_session}
	not authz.allow with input as {"path": ["app", "admin", "clauses"], "method": "GET", "session": null}
}

test_lawyer_denied_on_admin_people_and_analytics if {
	not authz.allow with input as {"path": ["app", "admin", "people"], "method": "GET", "session": lawyer_session}
	not authz.allow with input as {"path": ["app", "admin", "people", "new"], "method": "GET", "session": lawyer_session}
	not authz.allow with input as {"path": ["app", "admin", "analytics"], "method": "GET", "session": lawyer_session}
	not authz.allow with input as {"path": ["app", "admin", "leads"], "method": "GET", "session": lawyer_session}
	not authz.allow with input as {"path": ["app", "admin", "leads", "x"], "method": "GET", "session": lawyer_session}
	not authz.allow with input as {"path": ["app", "admin", "leads", "x", "status"], "method": "POST", "session": lawyer_session}
}

test_the_admin_tiers_reach_the_lead_queue if {
	authz.allow with input as {"path": ["app", "admin", "leads"], "method": "GET", "session": admin_session}
	authz.allow with input as {"path": ["app", "admin", "leads"], "method": "GET", "session": owner_session}
	authz.allow with input as {"path": ["app", "admin", "leads", "x"], "method": "GET", "session": admin_session}
	authz.allow with input as {"path": ["app", "admin", "leads", "x", "convert"], "method": "POST", "session": owner_session}
}

test_clerk_denied_on_the_lead_queue if {
	not authz.allow with input as {"path": ["app", "admin", "leads"], "method": "GET", "session": clerk_session}
}

test_client_denied_on_the_lead_queue if {
	not authz.allow with input as {"path": ["app", "admin", "leads"], "method": "GET", "session": client_session}
}

test_clerk_denied_on_admin_reference_listings if {
	not authz.allow with input as {"path": ["app", "admin", "entities"], "method": "GET", "session": clerk_session}
}

test_client_denied_on_admin_reference_listings if {
	not authz.allow with input as {"path": ["app", "admin", "entities"], "method": "GET", "session": client_session}
}

test_clerk_denied_on_the_admin_dashboard if {
	not authz.allow with input as {"path": ["app", "admin"], "method": "GET", "session": clerk_session}
}

# The matter directory beneath the desk (ENG-221) inherits admission from the
# same bypass and likewise has no rule of its own. It is a firm-wide list of
# every matter, so the tier that must never reach it is the one a rule copied
# from /app/lawyer would admit: Lawyer.
test_the_admin_tiers_reach_the_matter_directory if {
	authz.allow with input as {"path": ["app", "admin", "projects"], "method": "GET", "session": admin_session}
	authz.allow with input as {"path": ["app", "admin", "projects"], "method": "GET", "session": owner_session}
}

test_lawyer_denied_on_the_matter_directory if {
	not authz.allow with input as {"path": ["app", "admin", "projects"], "method": "GET", "session": lawyer_session}
}

test_clerk_denied_on_the_matter_directory if {
	not authz.allow with input as {"path": ["app", "admin", "projects"], "method": "GET", "session": clerk_session}
}

test_client_denied_on_the_matter_directory if {
	not authz.allow with input as {"path": ["app", "admin", "projects"], "method": "GET", "session": client_session}
}

# ---------- /clerk/* is retired ----------
# The supervised surface is now a rendering of `/app/projects`, not a namespace.
# A Clerk reaches the matter path (asserted above); which of the five renderings
# they get is `store::access::matter_viewer`'s decision, not the policy's, so it
# is pinned in Rust rather than here.

test_the_retired_clerk_namespace_is_not_allowed if {
	not authz.allow with input as {"path": ["clerk"], "method": "GET", "session": clerk_session}
	not authz.allow with input as {"path": ["clerk", "projects", "p1"], "method": "GET", "session": clerk_session}
}

test_anonymous_denied_on_clerk if {
	not authz.allow with input as {"path": ["clerk"], "method": "GET", "session": null}
}

# ---------- the retired nonprofit reading surfaces ----------
# The mission letter, Notations, and the transparency disclosures were the one
# grant in this policy that admitted `client` to a page outside their own
# matters. Those pages are retired, so the grant is gone and every tier — the
# authenticated ones included — is denied. Kept as a guard: reintroducing a
# blanket authenticated-reads rule would widen `client` again.

test_no_tier_reads_the_retired_foundation_pages if {
	not authz.allow with input as {"path": ["mission"], "method": "GET", "session": client_session}
	not authz.allow with input as {"path": ["notations"], "method": "GET", "session": client_session}
	not authz.allow with input as {"path": ["transparency"], "method": "GET", "session": client_session}
	not authz.allow with input as {"path": ["transparency", "bylaws"], "method": "GET", "session": client_session}
	not authz.allow with input as {"path": ["foundation"], "method": "GET", "session": client_session}
	not authz.allow with input as {"path": ["foundation", "mission"], "method": "GET", "session": client_session}
}

test_anonymous_denied_on_the_retired_foundation_pages if {
	not authz.allow with input as {"path": ["mission"], "method": "GET", "session": null}
	not authz.allow with input as {"path": ["notations"], "method": "GET", "session": null}
	not authz.allow with input as {"path": ["transparency"], "method": "GET", "session": null}
	not authz.allow with input as {"path": ["foundation"], "method": "GET", "session": null}
}

# Workshop reads are public and bypass this policy. Keep the client case here
# as a guard against accidentally reintroducing a policy grant for the public
# catalog or its class material.
test_client_denied_on_the_workshops_catalog_and_its_classes if {
	not authz.allow with input as {"path": ["workshops"], "method": "GET", "session": client_session}
	not authz.allow with input as {"path": ["workshops", "use-the-navigator"], "method": "GET", "session": client_session}
}

# Lawyer and Clerk likewise do not need a policy grant for public material;
# Owner and Admin remain covered by their general bypass, which is unrelated
# to the public route's authorization.
test_firm_side_roles_do_not_need_a_workshop_policy_grant if {
	not authz.allow with input as {"path": ["workshops"], "method": "GET", "session": lawyer_session}
	not authz.allow with input as {"path": ["workshops"], "method": "GET", "session": clerk_session}
	not authz.allow with input as {"path": ["workshops", "use-the-navigator"], "method": "GET", "session": lawyer_session}
	not authz.allow with input as {"path": ["workshops", "use-the-navigator"], "method": "GET", "session": clerk_session}
}

# Anonymous access is also handled by the public route rather than this policy.
test_anonymous_denied_on_the_workshops_catalog if {
	not authz.allow with input as {"path": ["workshops"], "method": "GET", "session": null}
}

# The reading grant must not leak onto the application. It names its pages
# exactly, so a path that merely starts with one of them is not covered.
test_the_reading_grant_does_not_reach_the_application if {
	not authz.allow with input as {"path": ["lawyer"], "method": "GET", "session": client_session}
	not authz.allow with input as {"path": ["clerk"], "method": "GET", "session": client_session}
	not authz.allow with input as {"path": ["mcp"], "method": "POST", "session": client_session}
}

# ---------- /workshops/* certificate claim ----------
# Workshop reads are public and bypass the policy. Only the completion
# certificate POST enters this rule family.

workshop_certificate := ["workshops", "use-the-navigator", "certificate"]

test_firm_roles_may_claim_a_workshop_certificate if {
	authz.allow with input as {"path": workshop_certificate, "method": "POST", "session": owner_session}
	authz.allow with input as {"path": workshop_certificate, "method": "POST", "session": admin_session}
	authz.allow with input as {"path": workshop_certificate, "method": "POST", "session": lawyer_session}
	authz.allow with input as {"path": workshop_certificate, "method": "POST", "session": clerk_session}
}

test_client_and_anonymous_may_not_claim_a_workshop_certificate if {
	not authz.allow with input as {"path": workshop_certificate, "method": "POST", "session": client_session}
	not authz.allow with input as {"path": workshop_certificate, "method": "POST", "session": null}
}

test_workshop_certificate_grant_is_exact if {
	not authz.allow with input as {"path": ["workshops"], "method": "GET", "session": lawyer_session}
	not authz.allow with input as {"path": ["workshops", "use-the-navigator"], "method": "GET", "session": lawyer_session}
	not authz.allow with input as {"path": workshop_certificate, "method": "GET", "session": lawyer_session}
	not authz.allow with input as {"path": ["presentations", "rust-in-peace", "certificate"], "method": "POST", "session": lawyer_session}
}

# The firm surface lives at /app/lawyer; the old /portal/admin prefix carries
# no lawyer allow rule. A lawyer GET there must NOT be allowed (the routes
# are gone, but if one ever returned the policy must not silently
# re-open it). Admin still passes via the bypass — that is the bypass
# rule's documented semantics, not a /portal/admin grant.
test_lawyer_denied_old_portal_admin if {
	not authz.allow with input as {"path": ["portal", "admin", "people"], "method": "GET", "session": lawyer_session}
}

# ---------- /mcp (lawyer tier only) ----------

test_lawyer_reaches_mcp if {
	authz.allow with input as {"path": ["mcp"], "method": "POST", "session": lawyer_session}
}

test_client_denied_on_mcp if {
	not authz.allow with input as {"path": ["mcp"], "method": "POST", "session": client_session}
}

test_clerk_denied_on_mcp if {
	not authz.allow with input as {"path": ["mcp"], "method": "POST", "session": clerk_session}
}

# ---------- /app/api/mcp/rpc (lawyer tier only) ----------

test_lawyer_reaches_agent_rpc if {
	authz.allow with input as {"path": ["app", "api", "mcp", "rpc"], "method": "POST", "session": lawyer_session}
}

test_client_denied_on_agent_rpc if {
	not authz.allow with input as {"path": ["app", "api", "mcp", "rpc"], "method": "POST", "session": client_session}
}

test_clerk_denied_on_agent_rpc if {
	not authz.allow with input as {"path": ["app", "api", "mcp", "rpc"], "method": "POST", "session": clerk_session}
}

# ---------- /app/api/* read paths ----------
#
# A signed-in caller is not automatically a permitted reader. These two used to
# assert the opposite — a `client` and a `clerk` both reached
# `GET /app/api/people` under a blanket any-authenticated grant. Each read is
# now named with its own tier, and the per-resource decisions live in the
# dedicated section further down; what stays here is the shape those tests
# replaced, so the reversal is visible where the old grant was asserted.

test_lawyer_get_api_allowed if {
	authz.allow with input as {"path": ["app", "api", "people"], "method": "GET", "session": lawyer_session}
}

test_client_get_api_denied if {
	not authz.allow with input as {"path": ["app", "api", "people"], "method": "GET", "session": client_session}
}

test_anonymous_get_api_denied if {
	not authz.allow with input as {"path": ["app", "api", "people"], "method": "GET", "session": null}
}

test_authenticated_post_api_denied if {
	not authz.allow with input as {"path": ["app", "api", "people"], "method": "POST", "session": client_session}
}

# ---------- /app/api/people* command writes (lawyer tier only) ----------

test_lawyer_can_create_person if {
	authz.allow with input as {"path": ["app", "api", "people"], "method": "POST", "session": lawyer_session}
}

test_admin_can_create_person if {
	authz.allow with input as {"path": ["app", "api", "people"], "method": "POST", "session": admin_session}
}

test_owner_can_create_person if {
	authz.allow with input as {"path": ["app", "api", "people"], "method": "POST", "session": owner_session}
}

test_lawyer_can_update_person if {
	authz.allow with input as {"path": ["app", "api", "people", "p1"], "method": "PATCH", "session": lawyer_session}
}

test_lawyer_can_delete_person if {
	authz.allow with input as {"path": ["app", "api", "people", "p1"], "method": "DELETE", "session": lawyer_session}
}

test_lawyer_can_send_welcome if {
	authz.allow with input as {"path": ["app", "api", "people", "p1", "welcome"], "method": "POST", "session": lawyer_session}
}

test_client_denied_person_write if {
	not authz.allow with input as {"path": ["app", "api", "people", "p1"], "method": "PATCH", "session": client_session}
}

test_clerk_denied_person_write if {
	not authz.allow with input as {"path": ["app", "api", "people", "p1"], "method": "PATCH", "session": clerk_session}
}

test_anonymous_denied_person_write if {
	not authz.allow with input as {"path": ["app", "api", "people"], "method": "POST", "session": null}
}

# ---------- /app/api/entities command writes (lawyer tier only) ----------

test_lawyer_can_create_entity if {
	authz.allow with input as {"path": ["app", "api", "entities"], "method": "POST", "session": lawyer_session}
}

test_admin_can_create_entity if {
	authz.allow with input as {"path": ["app", "api", "entities"], "method": "POST", "session": admin_session}
}

test_client_denied_entity_write if {
	not authz.allow with input as {"path": ["app", "api", "entities"], "method": "POST", "session": client_session}
}

test_clerk_denied_entity_write if {
	not authz.allow with input as {"path": ["app", "api", "entities"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_entity_write if {
	not authz.allow with input as {"path": ["app", "api", "entities"], "method": "POST", "session": null}
}

test_lawyer_can_update_entity if {
	authz.allow with input as {"path": ["app", "api", "entities", "e1"], "method": "PATCH", "session": lawyer_session}
}

test_admin_can_update_entity if {
	authz.allow with input as {"path": ["app", "api", "entities", "e1"], "method": "PATCH", "session": admin_session}
}

test_client_denied_entity_update if {
	not authz.allow with input as {"path": ["app", "api", "entities", "e1"], "method": "PATCH", "session": client_session}
}

test_clerk_denied_entity_update if {
	not authz.allow with input as {"path": ["app", "api", "entities", "e1"], "method": "PATCH", "session": clerk_session}
}

test_anonymous_denied_entity_update if {
	not authz.allow with input as {"path": ["app", "api", "entities", "e1"], "method": "PATCH", "session": null}
}

test_lawyer_can_delete_entity if {
	authz.allow with input as {"path": ["app", "api", "entities", "e1"], "method": "DELETE", "session": lawyer_session}
}

test_admin_can_delete_entity if {
	authz.allow with input as {"path": ["app", "api", "entities", "e1"], "method": "DELETE", "session": admin_session}
}

test_client_denied_entity_delete if {
	not authz.allow with input as {"path": ["app", "api", "entities", "e1"], "method": "DELETE", "session": client_session}
}

test_clerk_denied_entity_delete if {
	not authz.allow with input as {"path": ["app", "api", "entities", "e1"], "method": "DELETE", "session": clerk_session}
}

test_anonymous_denied_entity_delete if {
	not authz.allow with input as {"path": ["app", "api", "entities", "e1"], "method": "DELETE", "session": null}
}

# ---------- /app/api/projects/{id}/contract-review upload (any AUTHENTICATED tier — client-writable) ----------

# ---------- /app/api/projects/{id}/testimonial (client-lens write) ----------

# Rego admits the client tier to this exact route; the handler and store then
# resolve the client DRI's participation on the named Project and collapse a
# different Project to 404.
test_client_can_save_a_testimonial if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "testimonial"], "method": "POST", "session": client_session}
}

test_lawyer_denied_saving_a_client_testimonial if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "testimonial"], "method": "POST", "session": lawyer_session}
}

test_clerk_denied_saving_a_client_testimonial if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "testimonial"], "method": "POST", "session": clerk_session}
}

# Owner and Admin reach this door through the route-admission bypass. The
# handler and store still require the client DRI and refuse those tiers.
test_owner_and_admin_reach_client_testimonial_save if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "testimonial"], "method": "POST", "session": owner_session}
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "testimonial"], "method": "POST", "session": admin_session}
}

test_anonymous_denied_saving_a_client_testimonial if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "testimonial"], "method": "POST", "session": null}
}

# The native form sits under `/app/projects`, so every authenticated tier is
# admitted here. The handler's client-DRI check is the write gate.
test_authenticated_reaches_project_testimonial_form if {
	authz.allow with input as {"path": ["app", "projects", "sample-litigation", "testimonial"], "method": "POST", "session": client_session}
	authz.allow with input as {"path": ["app", "projects", "sample-litigation", "testimonial"], "method": "POST", "session": lawyer_session}
	authz.allow with input as {"path": ["app", "projects", "sample-litigation", "testimonial"], "method": "POST", "session": admin_session}
}

test_anonymous_denied_project_testimonial_form if {
	not authz.allow with input as {"path": ["app", "projects", "sample-litigation", "testimonial"], "method": "POST", "session": null}
}

# ---------- POST /app/admin/projects/{code}/testimonial (admin matter write) ----------
# Rego carries the system-tier part of this door. Matter participation is a
# handler check because the policy input has no participation ledger.

test_client_denied_admin_project_testimonial if {
	not authz.allow with input as {"path": ["app", "admin", "projects", "matter", "testimonial"], "method": "POST", "session": client_session}
}

test_lawyer_denied_admin_project_testimonial if {
	not authz.allow with input as {"path": ["app", "admin", "projects", "matter", "testimonial"], "method": "POST", "session": lawyer_session}
}

test_admin_reaches_admin_project_testimonial if {
	authz.allow with input as {"path": ["app", "admin", "projects", "matter", "testimonial"], "method": "POST", "session": admin_session}
}

test_owner_reaches_admin_project_testimonial if {
	authz.allow with input as {"path": ["app", "admin", "projects", "matter", "testimonial"], "method": "POST", "session": owner_session}
}

test_anonymous_denied_admin_project_testimonial if {
	not authz.allow with input as {"path": ["app", "admin", "projects", "matter", "testimonial"], "method": "POST", "session": null}
}

# ---------- POST /app/admin/projects/{code}/testimonial/{publish|unpublish} ----------
# The same role contract as the entry door; participation is a handler check.

test_client_denied_admin_project_testimonial_publication if {
	not authz.allow with input as {"path": ["app", "admin", "projects", "matter", "testimonial", "publish"], "method": "POST", "session": client_session}
	not authz.allow with input as {"path": ["app", "admin", "projects", "matter", "testimonial", "unpublish"], "method": "POST", "session": client_session}
}

test_lawyer_denied_admin_project_testimonial_publication if {
	not authz.allow with input as {"path": ["app", "admin", "projects", "matter", "testimonial", "publish"], "method": "POST", "session": lawyer_session}
	not authz.allow with input as {"path": ["app", "admin", "projects", "matter", "testimonial", "unpublish"], "method": "POST", "session": lawyer_session}
}

test_clerk_denied_admin_project_testimonial_publication if {
	not authz.allow with input as {"path": ["app", "admin", "projects", "matter", "testimonial", "publish"], "method": "POST", "session": clerk_session}
	not authz.allow with input as {"path": ["app", "admin", "projects", "matter", "testimonial", "unpublish"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_admin_project_testimonial_publication if {
	not authz.allow with input as {"path": ["app", "admin", "projects", "matter", "testimonial", "publish"], "method": "POST", "session": null}
	not authz.allow with input as {"path": ["app", "admin", "projects", "matter", "testimonial", "unpublish"], "method": "POST", "session": null}
}

test_admin_reaches_admin_project_testimonial_publication if {
	authz.allow with input as {"path": ["app", "admin", "projects", "matter", "testimonial", "publish"], "method": "POST", "session": admin_session}
	authz.allow with input as {"path": ["app", "admin", "projects", "matter", "testimonial", "unpublish"], "method": "POST", "session": admin_session}
}

test_owner_reaches_admin_project_testimonial_publication if {
	authz.allow with input as {"path": ["app", "admin", "projects", "matter", "testimonial", "publish"], "method": "POST", "session": owner_session}
	authz.allow with input as {"path": ["app", "admin", "projects", "matter", "testimonial", "unpublish"], "method": "POST", "session": owner_session}
}

# ---------- POST /app/admin/projects/{code}/people/{person}/avatar ----------
# Role admission is Rego's part of this door. The handler checks both the
# acting admin's and target person's matter participation.

test_client_denied_admin_project_person_avatar_upload if {
	not authz.allow with input as {"path": ["app", "admin", "projects", "matter", "people", "person", "avatar"], "method": "POST", "session": client_session}
}

test_lawyer_denied_admin_project_person_avatar_upload if {
	not authz.allow with input as {"path": ["app", "admin", "projects", "matter", "people", "person", "avatar"], "method": "POST", "session": lawyer_session}
}

test_admin_reaches_admin_project_person_avatar_upload if {
	authz.allow with input as {"path": ["app", "admin", "projects", "matter", "people", "person", "avatar"], "method": "POST", "session": admin_session}
}

test_owner_reaches_admin_project_person_avatar_upload if {
	authz.allow with input as {"path": ["app", "admin", "projects", "matter", "people", "person", "avatar"], "method": "POST", "session": owner_session}
}

test_anonymous_denied_admin_project_person_avatar_upload if {
	not authz.allow with input as {"path": ["app", "admin", "projects", "matter", "people", "person", "avatar"], "method": "POST", "session": null}
}

test_admin_reaches_admin_project_person_avatar_clear if {
	authz.allow with input as {"path": ["app", "admin", "projects", "matter", "people", "person", "avatar", "clear"], "method": "POST", "session": admin_session}
}

test_client_denied_admin_project_person_avatar_clear if {
	not authz.allow with input as {"path": ["app", "admin", "projects", "matter", "people", "person", "avatar", "clear"], "method": "POST", "session": client_session}
}

# ---------- /app/api/testimonials/{id}/publish|unpublish (lawyer-tier door) ----------

test_lawyer_tier_can_publish_or_unpublish_a_testimonial if {
	authz.allow with input as {"path": ["app", "api", "testimonials", "t1", "publish"], "method": "POST", "session": lawyer_session}
	authz.allow with input as {"path": ["app", "api", "testimonials", "t1", "unpublish"], "method": "POST", "session": lawyer_session}
	authz.allow with input as {"path": ["app", "api", "testimonials", "t1", "publish"], "method": "POST", "session": admin_session}
	authz.allow with input as {"path": ["app", "api", "testimonials", "t1", "publish"], "method": "POST", "session": owner_session}
}

test_client_denied_publishing_a_testimonial if {
	not authz.allow with input as {"path": ["app", "api", "testimonials", "t1", "publish"], "method": "POST", "session": client_session}
	not authz.allow with input as {"path": ["app", "api", "testimonials", "t1", "unpublish"], "method": "POST", "session": client_session}
}

test_clerk_denied_publishing_a_testimonial if {
	not authz.allow with input as {"path": ["app", "api", "testimonials", "t1", "publish"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_publishing_a_testimonial if {
	not authz.allow with input as {"path": ["app", "api", "testimonials", "t1", "publish"], "method": "POST", "session": null}
}

test_client_can_upload_contract_review if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "contract-review"], "method": "POST", "session": client_session}
}

test_lawyer_can_upload_contract_review if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "contract-review"], "method": "POST", "session": lawyer_session}
}

test_admin_can_upload_contract_review if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "contract-review"], "method": "POST", "session": admin_session}
}

test_anonymous_denied_contract_review if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "contract-review"], "method": "POST", "session": null}
}

# ---------- /app/api/projects/{id}/participants add-participant command (lawyer tier only) ----------

test_lawyer_can_add_participant if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "participants"], "method": "POST", "session": lawyer_session}
}

test_admin_can_add_participant if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "participants"], "method": "POST", "session": admin_session}
}

test_owner_can_add_participant if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "participants"], "method": "POST", "session": owner_session}
}

test_client_denied_add_participant if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "participants"], "method": "POST", "session": client_session}
}

test_clerk_denied_add_participant if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "participants"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_add_participant if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "participants"], "method": "POST", "session": null}
}

# ---------- POST /app/api/projects/{id}/relationships/adverse record-adversity command (lawyer tier only) ----------

test_lawyer_can_record_adversity if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "relationships", "adverse"], "method": "POST", "session": lawyer_session}
}

test_admin_can_record_adversity if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "relationships", "adverse"], "method": "POST", "session": admin_session}
}

test_owner_can_record_adversity if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "relationships", "adverse"], "method": "POST", "session": owner_session}
}

test_client_denied_record_adversity if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "relationships", "adverse"], "method": "POST", "session": client_session}
}

test_clerk_denied_record_adversity if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "relationships", "adverse"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_record_adversity if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "relationships", "adverse"], "method": "POST", "session": null}
}

# The adverse door admits only its own verb and shape: a GET on the path, and
# a lawyer-tier POST one segment deeper, are both denied.
test_adversity_rule_is_exact if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "relationships", "adverse"], "method": "GET", "session": lawyer_session}
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "relationships", "adverse", "x"], "method": "POST", "session": lawyer_session}
}

# ---------- PATCH/DELETE /app/api/projects/{id}/participants/{role_id} edit + remove (lawyer tier only) ----------

test_lawyer_can_edit_participant if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "participants", "r1"], "method": "PATCH", "session": lawyer_session}
}

test_admin_can_remove_participant if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "participants", "r1"], "method": "DELETE", "session": admin_session}
}

test_owner_can_edit_participant if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "participants", "r1"], "method": "PATCH", "session": owner_session}
}

test_client_denied_edit_participant if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "participants", "r1"], "method": "PATCH", "session": client_session}
}

test_clerk_denied_remove_participant if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "participants", "r1"], "method": "DELETE", "session": clerk_session}
}

test_anonymous_denied_remove_participant if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "participants", "r1"], "method": "DELETE", "session": null}
}

# ---------- PUT/DELETE /app/api/projects/{id}/participants/{role_id}/dri designate + clear DRI (lawyer tier only) ----------

test_lawyer_can_designate_dri if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "participants", "r1", "dri"], "method": "PUT", "session": lawyer_session}
}

test_admin_can_clear_dri if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "participants", "r1", "dri"], "method": "DELETE", "session": admin_session}
}

test_owner_can_designate_dri if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "participants", "r1", "dri"], "method": "PUT", "session": owner_session}
}

test_client_denied_designate_dri if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "participants", "r1", "dri"], "method": "PUT", "session": client_session}
}

test_clerk_denied_clear_dri if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "participants", "r1", "dri"], "method": "DELETE", "session": clerk_session}
}

test_anonymous_denied_designate_dri if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "participants", "r1", "dri"], "method": "PUT", "session": null}
}

# ---------- POST /app/api/projects/{id}/close open-closing-notation command (lawyer tier only) ----------

test_lawyer_can_close_matter if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "close"], "method": "POST", "session": lawyer_session}
}

test_admin_can_close_matter if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "close"], "method": "POST", "session": admin_session}
}

test_client_denied_close_matter if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "close"], "method": "POST", "session": client_session}
}

test_clerk_denied_close_matter if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "close"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_close_matter if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "close"], "method": "POST", "session": null}
}

# ---------- POST /app/api/projects/{id}/lifecycle direct transition command (lawyer tier only) ----------

test_lawyer_can_transition_matter if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "lifecycle"], "method": "POST", "session": lawyer_session}
}

test_admin_can_transition_matter if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "lifecycle"], "method": "POST", "session": admin_session}
}

test_client_denied_transition_matter if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "lifecycle"], "method": "POST", "session": client_session}
}

test_clerk_denied_transition_matter if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "lifecycle"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_transition_matter if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "lifecycle"], "method": "POST", "session": null}
}

# ---------- POST /app/api/projects/{id}/approve-plan (any AUTHENTICATED tier — client-writable) ----------

# The client approving their own estate plan reaches this door; the handler then
# enforces client-lens matter access.
test_client_can_approve_plan if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "approve-plan"], "method": "POST", "session": client_session}
}

test_lawyer_allowed_at_opa_approve_plan if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "approve-plan"], "method": "POST", "session": lawyer_session}
}

test_admin_allowed_at_opa_approve_plan if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "approve-plan"], "method": "POST", "session": admin_session}
}

test_clerk_allowed_at_opa_approve_plan if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "approve-plan"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_approve_plan if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "approve-plan"], "method": "POST", "session": null}
}

# ---------- POST /app/api/projects/{id}/conversation/messages (any AUTHENTICATED tier — client-writable) ----------

test_client_can_post_conversation_message if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "conversation", "messages"], "method": "POST", "session": client_session}
}

test_lawyer_can_post_conversation_message if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "conversation", "messages"], "method": "POST", "session": lawyer_session}
}

test_admin_can_post_conversation_message if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "conversation", "messages"], "method": "POST", "session": admin_session}
}

test_clerk_allowed_at_opa_post_conversation_message if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "conversation", "messages"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_post_conversation_message if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "conversation", "messages"], "method": "POST", "session": null}
}

# ---------- POST /app/api/expunge-requests/{id}/authorize (ADMIN tier only — runs the expunge) ----------

test_admin_can_authorize_expunge if {
	authz.allow with input as {"path": ["app", "api", "expunge-requests", "e1", "authorize"], "method": "POST", "session": admin_session}
}

# The one project-adjacent write the lawyer tier alone cannot fire.
test_lawyer_denied_authorize_expunge if {
	not authz.allow with input as {"path": ["app", "api", "expunge-requests", "e1", "authorize"], "method": "POST", "session": lawyer_session}
}

test_client_denied_authorize_expunge if {
	not authz.allow with input as {"path": ["app", "api", "expunge-requests", "e1", "authorize"], "method": "POST", "session": client_session}
}

test_clerk_denied_authorize_expunge if {
	not authz.allow with input as {"path": ["app", "api", "expunge-requests", "e1", "authorize"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_authorize_expunge if {
	not authz.allow with input as {"path": ["app", "api", "expunge-requests", "e1", "authorize"], "method": "POST", "session": null}
}

# ---------- GET /app/api/project-repositories (ADMIN tier only — reads every matter's row) ----------

test_admin_can_read_project_repositories if {
	authz.allow with input as {"path": ["app", "api", "project-repositories"], "method": "GET", "session": admin_session}
}

# The tier is the whole control. Every other matter read on this surface is
# participation-scoped, so a lawyer reaching an all-rows report would be the
# silent directory bypass `visible_projects_as_lawyer` refuses to grant.
test_lawyer_denied_project_repositories if {
	not authz.allow with input as {"path": ["app", "api", "project-repositories"], "method": "GET", "session": lawyer_session}
}

test_clerk_denied_project_repositories if {
	not authz.allow with input as {"path": ["app", "api", "project-repositories"], "method": "GET", "session": clerk_session}
}

test_client_denied_project_repositories if {
	not authz.allow with input as {"path": ["app", "api", "project-repositories"], "method": "GET", "session": client_session}
}

test_anonymous_denied_project_repositories if {
	not authz.allow with input as {"path": ["app", "api", "project-repositories"], "method": "GET", "session": null}
}

# The reason this door carries its own noun. Nested under `projects` the rule
# admitting any authenticated caller up to five segments would have reached it,
# so a client would have been policy-allowed on an admin-only report. Asserted
# so a future move back under that prefix fails here rather than in review.
test_a_client_reaches_a_projects_subpath_but_not_this_one if {
	authz.allow with input as {"path": ["app", "api", "projects", "reconcile"], "method": "GET", "session": client_session}
	not authz.allow with input as {"path": ["app", "api", "project-repositories"], "method": "GET", "session": client_session}
}

# ---------- GET /app/api/project-lifecycle (ADMIN tier only) ----------

test_admin_can_read_project_lifecycle if {
	authz.allow with input as {"path": ["app", "api", "project-lifecycle"], "method": "GET", "session": admin_session}
}

test_owner_can_read_project_lifecycle if {
	authz.allow with input as {"path": ["app", "api", "project-lifecycle"], "method": "GET", "session": owner_session}
}

test_lawyer_denied_project_lifecycle if {
	not authz.allow with input as {"path": ["app", "api", "project-lifecycle"], "method": "GET", "session": lawyer_session}
}

test_clerk_denied_project_lifecycle if {
	not authz.allow with input as {"path": ["app", "api", "project-lifecycle"], "method": "GET", "session": clerk_session}
}

test_client_denied_project_lifecycle if {
	not authz.allow with input as {"path": ["app", "api", "project-lifecycle"], "method": "GET", "session": client_session}
}

test_anonymous_denied_project_lifecycle if {
	not authz.allow with input as {"path": ["app", "api", "project-lifecycle"], "method": "GET", "session": null}
}

test_a_client_reaches_a_projects_subpath_but_not_project_lifecycle if {
	authz.allow with input as {"path": ["app", "api", "projects", "lifecycle"], "method": "GET", "session": client_session}
	not authz.allow with input as {"path": ["app", "api", "project-lifecycle"], "method": "GET", "session": client_session}
}

# ---------- POST /app/api/authorities (LAWYER tier — creates global citation-apparatus rows) ----------

test_lawyer_can_create_an_authority if {
	authz.allow with input as {"path": ["app", "api", "authorities"], "method": "POST", "session": lawyer_session}
}

test_admin_can_create_an_authority if {
	authz.allow with input as {"path": ["app", "api", "authorities"], "method": "POST", "session": admin_session}
}

test_clerk_denied_create_authority if {
	not authz.allow with input as {"path": ["app", "api", "authorities"], "method": "POST", "session": clerk_session}
}

test_client_denied_create_authority if {
	not authz.allow with input as {"path": ["app", "api", "authorities"], "method": "POST", "session": client_session}
}

test_anonymous_denied_create_authority if {
	not authz.allow with input as {"path": ["app", "api", "authorities"], "method": "POST", "session": null}
}

# The rule is exactly the collection path and POST — not a GET, and not a
# nested item path.
test_authorities_rule_is_exactly_the_collection_path_and_post if {
	not authz.allow with input as {"path": ["app", "api", "authorities"], "method": "GET", "session": lawyer_session}
	not authz.allow with input as {"path": ["app", "api", "authorities", "extra"], "method": "POST", "session": lawyer_session}
}

# A filed Authority's metadata uses the archive door's lawyer tier.
test_filed_authority_metadata_is_lawyer_only if {
    authz.allow with input as {"path": ["app", "api", "authorities", "a1"], "method": "GET", "session": lawyer_session}
    authz.allow with input as {"path": ["app", "api", "authorities", "a1"], "method": "GET", "session": admin_session}
    not authz.allow with input as {"path": ["app", "api", "authorities", "a1"], "method": "GET", "session": clerk_session}
    not authz.allow with input as {"path": ["app", "api", "authorities", "a1"], "method": "GET", "session": client_session}
    not authz.allow with input as {"path": ["app", "api", "authorities", "a1"], "method": "GET", "session": null}
    not authz.allow with input as {"path": ["app", "api", "authorities", "a1"], "method": "POST", "session": lawyer_session}
}

# ---------- GET /app/api/authorities/{id}/archive (LAWYER tier — reads a global archive) ----------

test_lawyer_and_admin_can_read_an_authority_archive if {
	authz.allow with input as {"path": ["app", "api", "authorities", "a1", "archive"], "method": "GET", "session": lawyer_session}
	authz.allow with input as {"path": ["app", "api", "authorities", "a1", "archive"], "method": "GET", "session": admin_session}
}

test_clerk_client_and_anonymous_denied_authority_archive if {
	not authz.allow with input as {"path": ["app", "api", "authorities", "a1", "archive"], "method": "GET", "session": clerk_session}
	not authz.allow with input as {"path": ["app", "api", "authorities", "a1", "archive"], "method": "GET", "session": client_session}
	not authz.allow with input as {"path": ["app", "api", "authorities", "a1", "archive"], "method": "GET", "session": null}
}

test_authority_archive_rule_is_exactly_five_segments_and_get if {
	not authz.allow with input as {"path": ["app", "api", "authorities", "a1", "archive"], "method": "POST", "session": lawyer_session}
	not authz.allow with input as {"path": ["app", "api", "authorities", "a1", "other"], "method": "GET", "session": lawyer_session}
	not authz.allow with input as {"path": ["app", "api", "authorities", "a1", "b", "archive"], "method": "GET", "session": lawyer_session}
}

# ---------- POST /app/api/integrations/{provider}/{verb} (ADMIN tier only — provisions Firm-private resources) ----------

test_admin_can_ensure_a_firm_private_notion_page if {
	authz.allow with input as {"path": ["app", "api", "integrations", "notion", "ensure"], "method": "POST", "session": admin_session}
}

test_admin_can_ensure_a_project_calendar if {
	authz.allow with input as {"path": ["app", "api", "integrations", "calendar", "ensure"], "method": "POST", "session": admin_session}
}

test_a_client_cannot_ensure_a_project_calendar if {
	not authz.allow with input as {"path": ["app", "api", "integrations", "calendar", "ensure"], "method": "POST", "session": client_session}
}

test_owner_can_ensure_a_firm_private_slack_channel if {
	authz.allow with input as {"path": ["app", "api", "integrations", "slack", "ensure"], "method": "POST", "session": owner_session}
}

test_lawyer_denied_integrations if {
	not authz.allow with input as {"path": ["app", "api", "integrations", "notion", "ensure"], "method": "POST", "session": lawyer_session}
	not authz.allow with input as {"path": ["app", "api", "integrations", "slack", "notify"], "method": "POST", "session": lawyer_session}
}

test_clerk_denied_integrations if {
	not authz.allow with input as {"path": ["app", "api", "integrations", "notion", "reconcile"], "method": "POST", "session": clerk_session}
}

test_client_denied_integrations if {
	not authz.allow with input as {"path": ["app", "api", "integrations", "slack", "notify"], "method": "POST", "session": client_session}
}

test_anonymous_denied_integrations if {
	not authz.allow with input as {"path": ["app", "api", "integrations", "notion", "ensure"], "method": "POST", "session": null}
}

# The rule is five segments and POST. A GET, a shorter path, or a deeper one
# is not admitted by it — so a read surface cannot appear on this noun by
# accident, and neither can a nested sub-resource.
test_integrations_rule_is_exactly_five_segments_and_post if {
	not authz.allow with input as {"path": ["app", "api", "integrations", "notion", "ensure"], "method": "GET", "session": lawyer_session}
	not authz.allow with input as {"path": ["app", "api", "integrations", "notion"], "method": "POST", "session": lawyer_session}
	not authz.allow with input as {"path": ["app", "api", "integrations", "notion", "ensure", "extra"], "method": "POST", "session": lawyer_session}
}

# Same noun-isolation as project-surfaces: a path nested under `projects` is
# policy-reachable by a client, and a provisioning door must not be.
test_a_client_reaches_a_projects_subpath_but_not_integrations if {
	authz.allow with input as {"path": ["app", "api", "projects", "integrations"], "method": "GET", "session": client_session}
	not authz.allow with input as {"path": ["app", "api", "integrations", "notion", "ensure"], "method": "POST", "session": client_session}
}

# ---------- POST /app/api/project-surfaces/{id} (ADMIN tier only — provisions one matter's handles) ----------

test_admin_can_reconcile_project_surfaces if {
	authz.allow with input as {"path": ["app", "api", "project-surfaces", "p1"], "method": "POST", "session": admin_session}
}

test_lawyer_denied_project_surfaces if {
	not authz.allow with input as {"path": ["app", "api", "project-surfaces", "p1"], "method": "POST", "session": lawyer_session}
}

test_clerk_denied_project_surfaces if {
	not authz.allow with input as {"path": ["app", "api", "project-surfaces", "p1"], "method": "POST", "session": clerk_session}
}

test_client_denied_project_surfaces if {
	not authz.allow with input as {"path": ["app", "api", "project-surfaces", "p1"], "method": "POST", "session": client_session}
}

test_anonymous_denied_project_surfaces if {
	not authz.allow with input as {"path": ["app", "api", "project-surfaces", "p1"], "method": "POST", "session": null}
}

test_lawyer_can_read_project_surfaces if {
	authz.allow with input as {"path": ["app", "api", "project-surfaces", "p1"], "method": "GET", "session": lawyer_session}
}

test_client_cannot_read_project_surfaces if {
	not authz.allow with input as {"path": ["app", "api", "project-surfaces", "p1"], "method": "GET", "session": client_session}
}

# ---------- POST /app/api/project-publishers/{id} is retired with Portal publishing ----------

test_project_publisher_route_is_retired if {
	not authz.allow with input as {"path": ["app", "api", "project-publishers", "p1"], "method": "POST", "session": lawyer_session}
}

# Same noun-isolation as project-repositories: a GET nested under `projects`
# is policy-reachable by a client; this POST must not be.
test_a_client_reaches_a_projects_subpath_but_not_project_surfaces if {
	authz.allow with input as {"path": ["app", "api", "projects", "reconcile"], "method": "GET", "session": client_session}
	not authz.allow with input as {"path": ["app", "api", "project-surfaces", "p1"], "method": "POST", "session": client_session}
}

# ---------- POST /app/api/expunge-requests/{id}/deny (lawyer tier only) ----------

test_lawyer_can_deny_expunge if {
	authz.allow with input as {"path": ["app", "api", "expunge-requests", "e1", "deny"], "method": "POST", "session": lawyer_session}
}

test_admin_can_deny_expunge if {
	authz.allow with input as {"path": ["app", "api", "expunge-requests", "e1", "deny"], "method": "POST", "session": admin_session}
}

test_client_denied_deny_expunge if {
	not authz.allow with input as {"path": ["app", "api", "expunge-requests", "e1", "deny"], "method": "POST", "session": client_session}
}

test_clerk_denied_deny_expunge if {
	not authz.allow with input as {"path": ["app", "api", "expunge-requests", "e1", "deny"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_deny_expunge if {
	not authz.allow with input as {"path": ["app", "api", "expunge-requests", "e1", "deny"], "method": "POST", "session": null}
}

# ---------- POST /app/api/playbooks create playbook (lawyer tier only) ----------

test_lawyer_can_create_playbook if {
	authz.allow with input as {"path": ["app", "api", "playbooks"], "method": "POST", "session": lawyer_session}
}

test_admin_can_create_playbook if {
	authz.allow with input as {"path": ["app", "api", "playbooks"], "method": "POST", "session": admin_session}
}

test_client_denied_create_playbook if {
	not authz.allow with input as {"path": ["app", "api", "playbooks"], "method": "POST", "session": client_session}
}

test_clerk_denied_create_playbook if {
	not authz.allow with input as {"path": ["app", "api", "playbooks"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_create_playbook if {
	not authz.allow with input as {"path": ["app", "api", "playbooks"], "method": "POST", "session": null}
}

# ---------- PUT /app/api/playbooks/{id} replace positions (lawyer tier only) ----------

test_lawyer_can_update_playbook if {
	authz.allow with input as {"path": ["app", "api", "playbooks", "p1"], "method": "PUT", "session": lawyer_session}
}

test_admin_can_update_playbook if {
	authz.allow with input as {"path": ["app", "api", "playbooks", "p1"], "method": "PUT", "session": admin_session}
}

test_client_denied_update_playbook if {
	not authz.allow with input as {"path": ["app", "api", "playbooks", "p1"], "method": "PUT", "session": client_session}
}

test_clerk_denied_update_playbook if {
	not authz.allow with input as {"path": ["app", "api", "playbooks", "p1"], "method": "PUT", "session": clerk_session}
}

test_anonymous_denied_update_playbook if {
	not authz.allow with input as {"path": ["app", "api", "playbooks", "p1"], "method": "PUT", "session": null}
}

# ---------- POST /app/api/contract-reviews/{id}/findings/{idx} save finding (lawyer tier only) ----------

test_lawyer_can_save_review_finding if {
	authz.allow with input as {"path": ["app", "api", "contract-reviews", "r1", "findings", "0"], "method": "POST", "session": lawyer_session}
}

test_admin_can_save_review_finding if {
	authz.allow with input as {"path": ["app", "api", "contract-reviews", "r1", "findings", "0"], "method": "POST", "session": admin_session}
}

test_client_denied_save_review_finding if {
	not authz.allow with input as {"path": ["app", "api", "contract-reviews", "r1", "findings", "0"], "method": "POST", "session": client_session}
}

test_clerk_denied_save_review_finding if {
	not authz.allow with input as {"path": ["app", "api", "contract-reviews", "r1", "findings", "0"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_save_review_finding if {
	not authz.allow with input as {"path": ["app", "api", "contract-reviews", "r1", "findings", "0"], "method": "POST", "session": null}
}

# ---------- POST /app/api/contract-reviews/{id}/summary edit summary (lawyer tier only) ----------

test_lawyer_can_save_review_summary if {
	authz.allow with input as {"path": ["app", "api", "contract-reviews", "r1", "summary"], "method": "POST", "session": lawyer_session}
}

test_admin_can_save_review_summary if {
	authz.allow with input as {"path": ["app", "api", "contract-reviews", "r1", "summary"], "method": "POST", "session": admin_session}
}

test_client_denied_save_review_summary if {
	not authz.allow with input as {"path": ["app", "api", "contract-reviews", "r1", "summary"], "method": "POST", "session": client_session}
}

test_clerk_denied_save_review_summary if {
	not authz.allow with input as {"path": ["app", "api", "contract-reviews", "r1", "summary"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_save_review_summary if {
	not authz.allow with input as {"path": ["app", "api", "contract-reviews", "r1", "summary"], "method": "POST", "session": null}
}

# ---------- POST /app/api/contract-reviews/{id}/approve approve review (lawyer tier only) ----------

test_lawyer_can_approve_review if {
	authz.allow with input as {"path": ["app", "api", "contract-reviews", "r1", "approve"], "method": "POST", "session": lawyer_session}
}

test_admin_can_approve_review if {
	authz.allow with input as {"path": ["app", "api", "contract-reviews", "r1", "approve"], "method": "POST", "session": admin_session}
}

test_client_denied_approve_review if {
	not authz.allow with input as {"path": ["app", "api", "contract-reviews", "r1", "approve"], "method": "POST", "session": client_session}
}

test_clerk_denied_approve_review if {
	not authz.allow with input as {"path": ["app", "api", "contract-reviews", "r1", "approve"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_approve_review if {
	not authz.allow with input as {"path": ["app", "api", "contract-reviews", "r1", "approve"], "method": "POST", "session": null}
}

# ---------- POST /app/api/contract-reviews/{id}/reject reject review (lawyer tier only) ----------

test_lawyer_can_reject_review if {
	authz.allow with input as {"path": ["app", "api", "contract-reviews", "r1", "reject"], "method": "POST", "session": lawyer_session}
}

test_admin_can_reject_review if {
	authz.allow with input as {"path": ["app", "api", "contract-reviews", "r1", "reject"], "method": "POST", "session": admin_session}
}

test_client_denied_reject_review if {
	not authz.allow with input as {"path": ["app", "api", "contract-reviews", "r1", "reject"], "method": "POST", "session": client_session}
}

test_clerk_denied_reject_review if {
	not authz.allow with input as {"path": ["app", "api", "contract-reviews", "r1", "reject"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_reject_review if {
	not authz.allow with input as {"path": ["app", "api", "contract-reviews", "r1", "reject"], "method": "POST", "session": null}
}

# ---------- POST /app/api/projects/{id}/documents file a document (lawyer tier only) ----------

test_lawyer_can_upload_document if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents"], "method": "POST", "session": lawyer_session}
}

test_admin_can_upload_document if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents"], "method": "POST", "session": admin_session}
}

test_client_denied_upload_document if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents"], "method": "POST", "session": client_session}
}

test_clerk_denied_upload_document if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_upload_document if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents"], "method": "POST", "session": null}
}

# ---------- PATCH /app/api/projects/{id}/documents/{asset_id} visibility (lawyer tier only) ----------

test_lawyer_can_reconcile_document_visibility if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "a1"], "method": "PATCH", "session": lawyer_session}
}

test_admin_can_reconcile_document_visibility if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "a1"], "method": "PATCH", "session": admin_session}
}

test_client_denied_reconcile_document_visibility if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "a1"], "method": "PATCH", "session": client_session}
}

test_clerk_denied_reconcile_document_visibility if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "a1"], "method": "PATCH", "session": clerk_session}
}

test_anonymous_denied_reconcile_document_visibility if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "a1"], "method": "PATCH", "session": null}
}

# ---------- POST /app/api/projects/{id}/documents/{asset_id}/storage (admin bypass only) ----------

test_admin_can_repair_document_storage if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "a1", "storage"], "method": "POST", "session": admin_session}
}

test_lawyer_denied_repair_document_storage if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "a1", "storage"], "method": "POST", "session": lawyer_session}
}

test_client_denied_repair_document_storage if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "a1", "storage"], "method": "POST", "session": client_session}
}

test_anonymous_denied_repair_document_storage if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "a1", "storage"], "method": "POST", "session": null}
}

# ---------- POST /app/api/projects/{id}/documents/{asset_id}/reclassify (lawyer tier) ----------

test_lawyer_can_reclassify_a_document if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "a1", "reclassify"], "method": "POST", "session": lawyer_session}
}

test_admin_can_reclassify_a_document if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "a1", "reclassify"], "method": "POST", "session": admin_session}
}

test_clerk_denied_reclassify_a_document if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "a1", "reclassify"], "method": "POST", "session": clerk_session}
}

test_client_denied_reclassify_a_document if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "a1", "reclassify"], "method": "POST", "session": client_session}
}

test_anonymous_denied_reclassify_a_document if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "a1", "reclassify"], "method": "POST", "session": null}
}

# ---------- Deadline writes under /app/api/projects/{id}/deadlines (lawyer tier, LAW-126) ----------

test_lawyer_can_create_a_deadline if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "deadlines"], "method": "POST", "session": lawyer_session}
}

test_clerk_denied_create_a_deadline if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "deadlines"], "method": "POST", "session": clerk_session}
}

test_client_denied_create_a_deadline if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "deadlines"], "method": "POST", "session": client_session}
}

test_anonymous_denied_create_a_deadline if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "deadlines"], "method": "POST", "session": null}
}

test_lawyer_can_update_a_deadline if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "deadlines", "d1"], "method": "PATCH", "session": lawyer_session}
}

test_clerk_denied_update_a_deadline if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "deadlines", "d1"], "method": "PATCH", "session": clerk_session}
}

test_client_denied_update_a_deadline if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "deadlines", "d1"], "method": "PATCH", "session": client_session}
}

test_anonymous_denied_update_a_deadline if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "deadlines", "d1"], "method": "PATCH", "session": null}
}

test_lawyer_can_complete_a_deadline if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "deadlines", "d1", "complete"], "method": "POST", "session": lawyer_session}
}

test_clerk_denied_complete_a_deadline if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "deadlines", "d1", "complete"], "method": "POST", "session": clerk_session}
}

test_client_denied_complete_a_deadline if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "deadlines", "d1", "complete"], "method": "POST", "session": client_session}
}

test_anonymous_denied_complete_a_deadline if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "deadlines", "d1", "complete"], "method": "POST", "session": null}
}

test_lawyer_can_cancel_or_reopen_a_deadline if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "deadlines", "d1", "cancel"], "method": "POST", "session": lawyer_session}
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "deadlines", "d1", "reopen"], "method": "POST", "session": lawyer_session}
}

test_clerk_denied_cancel_or_reopen_a_deadline if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "deadlines", "d1", "cancel"], "method": "POST", "session": clerk_session}
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "deadlines", "d1", "reopen"], "method": "POST", "session": clerk_session}
}

test_client_denied_cancel_or_reopen_a_deadline if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "deadlines", "d1", "cancel"], "method": "POST", "session": client_session}
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "deadlines", "d1", "reopen"], "method": "POST", "session": client_session}
}

test_anonymous_denied_cancel_or_reopen_a_deadline if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "deadlines", "d1", "cancel"], "method": "POST", "session": null}
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "deadlines", "d1", "reopen"], "method": "POST", "session": null}
}

test_lawyer_denied_an_unknown_deadline_verb if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "deadlines", "d1", "delete"], "method": "POST", "session": lawyer_session}
}

# ---------- Docket Entry writes under /app/api/projects/{id}/docket (lawyer tier, LAW-133) ----------

test_lawyer_can_record_a_docket_entry if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "docket"], "method": "POST", "session": lawyer_session}
}

test_clerk_denied_record_a_docket_entry if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "docket"], "method": "POST", "session": clerk_session}
}

test_client_denied_record_a_docket_entry if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "docket"], "method": "POST", "session": client_session}
}

test_anonymous_denied_record_a_docket_entry if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "docket"], "method": "POST", "session": null}
}

test_lawyer_can_link_a_docket_document if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "docket", "e1", "link-document"], "method": "POST", "session": lawyer_session}
}

test_clerk_denied_link_a_docket_document if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "docket", "e1", "link-document"], "method": "POST", "session": clerk_session}
}

test_client_denied_link_a_docket_document if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "docket", "e1", "link-document"], "method": "POST", "session": client_session}
}

test_anonymous_denied_link_a_docket_document if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "docket", "e1", "link-document"], "method": "POST", "session": null}
}

test_lawyer_can_supersede_a_docket_entry if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "docket", "e1", "supersede"], "method": "POST", "session": lawyer_session}
}

test_clerk_denied_supersede_a_docket_entry if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "docket", "e1", "supersede"], "method": "POST", "session": clerk_session}
}

test_client_denied_supersede_a_docket_entry if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "docket", "e1", "supersede"], "method": "POST", "session": client_session}
}

test_anonymous_denied_supersede_a_docket_entry if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "docket", "e1", "supersede"], "method": "POST", "session": null}
}

test_lawyer_denied_an_unknown_docket_verb if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "docket", "e1", "delete"], "method": "POST", "session": lawyer_session}
}

# ---------- Service records under /app/api/projects/{id}/service (lawyer tier, LAW-124) ----------

test_lawyer_can_record_a_service if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "service"], "method": "POST", "session": lawyer_session}
}

test_clerk_denied_record_a_service if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "service"], "method": "POST", "session": clerk_session}
}

test_client_denied_record_a_service if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "service"], "method": "POST", "session": client_session}
}

test_anonymous_denied_record_a_service if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "service"], "method": "POST", "session": null}
}

test_lawyer_can_read_one_service if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "service", "s1"], "method": "GET", "session": lawyer_session}
}

test_client_denied_read_one_service if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "service", "s1"], "method": "GET", "session": client_session}
}

test_anonymous_denied_read_one_service if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "service", "s1"], "method": "GET", "session": null}
}

test_lawyer_denied_a_deeper_service_post if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "service", "s1", "delete"], "method": "POST", "session": lawyer_session}
}

# ---------- Matter Pages under /app/api/projects/{id}/pages (lawyer tier) ----------

test_lawyer_can_store_matter_pages if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "pages"], "method": "PUT", "session": lawyer_session}
}

test_clerk_denied_store_matter_pages if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "pages"], "method": "PUT", "session": clerk_session}
}

test_client_denied_store_matter_pages if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "pages"], "method": "PUT", "session": client_session}
}

test_anonymous_denied_store_matter_pages if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "pages"], "method": "PUT", "session": null}
}

test_lawyer_denied_a_deeper_pages_put if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "pages", "timeline"], "method": "PUT", "session": lawyer_session}
}

# ---------- Matter Caption under /app/api/projects/{id}/matter (lawyer tier) ----------

test_lawyer_can_store_a_matter_caption if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "matter"], "method": "PUT", "session": lawyer_session}
}

test_clerk_denied_store_a_matter_caption if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "matter"], "method": "PUT", "session": clerk_session}
}

test_client_denied_store_a_matter_caption if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "matter"], "method": "PUT", "session": client_session}
}

test_anonymous_denied_store_a_matter_caption if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "matter"], "method": "PUT", "session": null}
}

test_lawyer_denied_a_deeper_matter_put if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "matter", "parties"], "method": "PUT", "session": lawyer_session}
}

# ---------- Estate and cap-table reads under /app/api/projects/{id}/... (any participant) ----------
# Admission only. The policy cannot read the participation ledger, so a client
# is admitted here for any Project id and the handler answers a matter the
# client does not participate in with 404 (portal/tests/litigation_records/
# module_records_api.rs pins that half).

test_lawyer_can_read_fiduciary_roles if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "fiduciary-roles"], "method": "GET", "session": lawyer_session}
}

test_client_admitted_to_read_estate_assets if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "estate-assets"], "method": "GET", "session": client_session}
}

test_client_admitted_to_read_cap_table_holdings if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "cap-table-holdings"], "method": "GET", "session": client_session}
}

test_anonymous_denied_read_fiduciary_roles if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "fiduciary-roles"], "method": "GET", "session": null}
}

test_lawyer_denied_write_estate_assets if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "estate-assets"], "method": "POST", "session": lawyer_session}
}

test_client_denied_write_cap_table_holdings if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "cap-table-holdings"], "method": "POST", "session": client_session}
}

test_client_denied_a_deeper_fiduciary_roles_read if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "fiduciary-roles", "r1", "notation"], "method": "GET", "session": client_session}
}

# ---------- Communication writes under /app/api/projects/{id}/communications (lawyer tier, LAW-140) ----------

test_lawyer_can_log_a_communication if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "communications"], "method": "POST", "session": lawyer_session}
}

test_clerk_denied_log_a_communication if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "communications"], "method": "POST", "session": clerk_session}
}

test_client_denied_log_a_communication if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "communications"], "method": "POST", "session": client_session}
}

test_anonymous_denied_log_a_communication if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "communications"], "method": "POST", "session": null}
}

# ---------- POST /app/api/projects/{id}/documents/{asset_id}/{delete,restore,hold} (lawyer tier) ----------

test_lawyer_can_soft_delete_a_document if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "a1", "delete"], "method": "POST", "session": lawyer_session}
}

test_admin_can_soft_delete_a_document if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "a1", "delete"], "method": "POST", "session": admin_session}
}

test_clerk_denied_soft_delete_a_document if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "a1", "delete"], "method": "POST", "session": clerk_session}
}

test_client_denied_soft_delete_a_document if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "a1", "delete"], "method": "POST", "session": client_session}
}

test_anonymous_denied_soft_delete_a_document if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "a1", "delete"], "method": "POST", "session": null}
}

test_lawyer_can_restore_a_document if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "a1", "restore"], "method": "POST", "session": lawyer_session}
}

test_client_denied_restore_a_document if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "a1", "restore"], "method": "POST", "session": client_session}
}

test_anonymous_denied_restore_a_document if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "a1", "restore"], "method": "POST", "session": null}
}

test_lawyer_can_hold_a_document if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "a1", "hold"], "method": "POST", "session": lawyer_session}
}

test_client_denied_hold_a_document if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "a1", "hold"], "method": "POST", "session": client_session}
}

test_anonymous_denied_hold_a_document if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "a1", "hold"], "method": "POST", "session": null}
}

# ---------- POST /app/api/documents/purge (admin tier only) ----------

test_admin_can_run_the_retention_purge if {
	authz.allow with input as {"path": ["app", "api", "documents", "purge"], "method": "POST", "session": admin_session}
}

test_lawyer_denied_the_retention_purge if {
	not authz.allow with input as {"path": ["app", "api", "documents", "purge"], "method": "POST", "session": lawyer_session}
}

test_anonymous_denied_the_retention_purge if {
	not authz.allow with input as {"path": ["app", "api", "documents", "purge"], "method": "POST", "session": null}
}

# ---------- POST /app/api/projects/{id}/mail/file (lawyer tier only) ----------

test_lawyer_can_file_mail_attachments if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "mail", "file"], "method": "POST", "session": lawyer_session}
}

test_admin_can_file_mail_attachments if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "mail", "file"], "method": "POST", "session": admin_session}
}

test_client_denied_file_mail_attachments if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "mail", "file"], "method": "POST", "session": client_session}
}

test_clerk_denied_file_mail_attachments if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "mail", "file"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_file_mail_attachments if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "mail", "file"], "method": "POST", "session": null}
}

# ---------- GET /app/api/projects/{id}/documents/revisions?slug= (any authenticated tier) ----------

test_lawyer_can_read_document_revisions if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "revisions"], "method": "GET", "session": lawyer_session}
}

test_admin_can_read_document_revisions if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "revisions"], "method": "GET", "session": admin_session}
}

test_client_can_read_document_revisions if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "revisions"], "method": "GET", "session": client_session}
}

test_clerk_can_read_document_revisions if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "revisions"], "method": "GET", "session": clerk_session}
}

test_anonymous_denied_read_document_revisions if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "revisions"], "method": "GET", "session": null}
}

# ---------- POST /app/api/projects/{id}/finance/run (lawyer tier) and GET .../finance ----------

test_lawyer_can_run_finance if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "finance", "run"], "method": "POST", "session": lawyer_session}
}

test_admin_can_run_finance if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "finance", "run"], "method": "POST", "session": admin_session}
}

test_client_denied_run_finance if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "finance", "run"], "method": "POST", "session": client_session}
}

test_clerk_denied_run_finance if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "finance", "run"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_run_finance if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "finance", "run"], "method": "POST", "session": null}
}

test_run_door_denies_a_get if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "finance", "run"], "method": "GET", "session": client_session}
}

test_authenticated_can_read_finance if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "finance"], "method": "GET", "session": client_session}
}

test_anonymous_denied_read_finance if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "finance"], "method": "GET", "session": null}
}

# ---------- GET /app/api/projects/{id}/documents/integrity (lawyer tier) ----------

test_lawyer_can_read_document_integrity if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "integrity"], "method": "GET", "session": lawyer_session}
}

test_admin_can_read_document_integrity if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "integrity"], "method": "GET", "session": admin_session}
}

test_client_denied_read_document_integrity if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "integrity"], "method": "GET", "session": client_session}
}

test_clerk_denied_read_document_integrity if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "integrity"], "method": "GET", "session": clerk_session}
}

test_anonymous_denied_read_document_integrity if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents", "integrity"], "method": "GET", "session": null}
}

# ---------- POST /app/api/notations/{id}/transcript coverage pass (lawyer tier only) ----------

test_lawyer_can_run_transcript_coverage if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1", "transcript"], "method": "POST", "session": lawyer_session}
}

test_admin_can_run_transcript_coverage if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1", "transcript"], "method": "POST", "session": admin_session}
}

test_client_denied_run_transcript_coverage if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "transcript"], "method": "POST", "session": client_session}
}

test_clerk_denied_run_transcript_coverage if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "transcript"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_run_transcript_coverage if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "transcript"], "method": "POST", "session": null}
}

# ---------- POST /app/api/projects/{id}/notations/{nid}/transcript estate intake (lawyer tier only) ----------

test_lawyer_can_file_estate_transcript if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "notations", "n1", "transcript"], "method": "POST", "session": lawyer_session}
}

test_admin_can_file_estate_transcript if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "notations", "n1", "transcript"], "method": "POST", "session": admin_session}
}

test_client_denied_file_estate_transcript if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "notations", "n1", "transcript"], "method": "POST", "session": client_session}
}

test_clerk_denied_file_estate_transcript if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "notations", "n1", "transcript"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_file_estate_transcript if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "notations", "n1", "transcript"], "method": "POST", "session": null}
}

# ---------- GET matter read clusters (#866 — any AUTHENTICATED tier; handler self-scopes) ----------

test_client_can_list_projects if {
	authz.allow with input as {"path": ["app", "api", "projects"], "method": "GET", "session": client_session}
}

test_lawyer_can_list_projects if {
	authz.allow with input as {"path": ["app", "api", "projects"], "method": "GET", "session": lawyer_session}
}

test_admin_can_list_projects if {
	authz.allow with input as {"path": ["app", "api", "projects"], "method": "GET", "session": admin_session}
}

test_clerk_allowed_at_opa_list_projects if {
	authz.allow with input as {"path": ["app", "api", "projects"], "method": "GET", "session": clerk_session}
}

test_anonymous_denied_list_projects if {
	not authz.allow with input as {"path": ["app", "api", "projects"], "method": "GET", "session": null}
}

test_client_can_get_project if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1"], "method": "GET", "session": client_session}
}

test_lawyer_can_get_project if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1"], "method": "GET", "session": lawyer_session}
}

test_admin_can_get_project if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1"], "method": "GET", "session": admin_session}
}

test_clerk_allowed_at_opa_get_project if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1"], "method": "GET", "session": clerk_session}
}

test_anonymous_denied_get_project if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1"], "method": "GET", "session": null}
}

test_client_can_list_participants if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "participants"], "method": "GET", "session": client_session}
}

test_lawyer_can_list_participants if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "participants"], "method": "GET", "session": lawyer_session}
}

test_admin_can_list_participants if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "participants"], "method": "GET", "session": admin_session}
}

test_clerk_allowed_at_opa_list_participants if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "participants"], "method": "GET", "session": clerk_session}
}

test_anonymous_denied_list_participants if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "participants"], "method": "GET", "session": null}
}

test_client_can_list_project_notations if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "notations"], "method": "GET", "session": client_session}
}

test_lawyer_can_list_project_notations if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "notations"], "method": "GET", "session": lawyer_session}
}

test_admin_can_list_project_notations if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "notations"], "method": "GET", "session": admin_session}
}

test_clerk_allowed_at_opa_list_project_notations if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "notations"], "method": "GET", "session": clerk_session}
}

test_anonymous_denied_list_project_notations if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "notations"], "method": "GET", "session": null}
}

test_client_can_get_notation if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1"], "method": "GET", "session": client_session}
}

test_lawyer_can_get_notation if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1"], "method": "GET", "session": lawyer_session}
}

test_admin_can_get_notation if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1"], "method": "GET", "session": admin_session}
}

test_clerk_allowed_at_opa_get_notation if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1"], "method": "GET", "session": clerk_session}
}

test_anonymous_denied_get_notation if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1"], "method": "GET", "session": null}
}

# ---------- GET firm-tool reads (#866 — lawyer tier only) ----------

test_lawyer_can_list_playbooks if {
	authz.allow with input as {"path": ["app", "api", "playbooks"], "method": "GET", "session": lawyer_session}
}

test_admin_can_list_playbooks if {
	authz.allow with input as {"path": ["app", "api", "playbooks"], "method": "GET", "session": admin_session}
}

test_client_denied_list_playbooks if {
	not authz.allow with input as {"path": ["app", "api", "playbooks"], "method": "GET", "session": client_session}
}

test_clerk_denied_list_playbooks if {
	not authz.allow with input as {"path": ["app", "api", "playbooks"], "method": "GET", "session": clerk_session}
}

test_anonymous_denied_list_playbooks if {
	not authz.allow with input as {"path": ["app", "api", "playbooks"], "method": "GET", "session": null}
}

test_lawyer_can_get_playbook if {
	authz.allow with input as {"path": ["app", "api", "playbooks", "p1"], "method": "GET", "session": lawyer_session}
}

test_admin_can_get_playbook if {
	authz.allow with input as {"path": ["app", "api", "playbooks", "p1"], "method": "GET", "session": admin_session}
}

test_client_denied_get_playbook if {
	not authz.allow with input as {"path": ["app", "api", "playbooks", "p1"], "method": "GET", "session": client_session}
}

test_clerk_denied_get_playbook if {
	not authz.allow with input as {"path": ["app", "api", "playbooks", "p1"], "method": "GET", "session": clerk_session}
}

test_anonymous_denied_get_playbook if {
	not authz.allow with input as {"path": ["app", "api", "playbooks", "p1"], "method": "GET", "session": null}
}

test_lawyer_can_get_contract_review if {
	authz.allow with input as {"path": ["app", "api", "contract-reviews", "r1"], "method": "GET", "session": lawyer_session}
}

test_admin_can_get_contract_review if {
	authz.allow with input as {"path": ["app", "api", "contract-reviews", "r1"], "method": "GET", "session": admin_session}
}

test_client_denied_get_contract_review if {
	not authz.allow with input as {"path": ["app", "api", "contract-reviews", "r1"], "method": "GET", "session": client_session}
}

test_clerk_denied_get_contract_review if {
	not authz.allow with input as {"path": ["app", "api", "contract-reviews", "r1"], "method": "GET", "session": clerk_session}
}

test_anonymous_denied_get_contract_review if {
	not authz.allow with input as {"path": ["app", "api", "contract-reviews", "r1"], "method": "GET", "session": null}
}

# ---------- GET matter documents + conversation (#866 — authenticated + handler-scoped) ----------

test_client_can_list_documents if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents"], "method": "GET", "session": client_session}
}

test_lawyer_can_list_documents if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents"], "method": "GET", "session": lawyer_session}
}

test_admin_can_list_documents if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents"], "method": "GET", "session": admin_session}
}

test_clerk_allowed_at_opa_list_documents if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents"], "method": "GET", "session": clerk_session}
}

test_anonymous_denied_list_documents if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "documents"], "method": "GET", "session": null}
}

test_client_can_get_conversation if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "conversation"], "method": "GET", "session": client_session}
}

test_lawyer_can_get_conversation if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "conversation"], "method": "GET", "session": lawyer_session}
}

test_admin_can_get_conversation if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "conversation"], "method": "GET", "session": admin_session}
}

test_clerk_allowed_at_opa_get_conversation if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "conversation"], "method": "GET", "session": clerk_session}
}

test_anonymous_denied_get_conversation if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "conversation"], "method": "GET", "session": null}
}

# ---------- GET expunge queue + review documents (#866 — lawyer tier, firm work product) ----------

test_lawyer_can_list_expunge_requests if {
	authz.allow with input as {"path": ["app", "api", "expunge-requests"], "method": "GET", "session": lawyer_session}
}

test_admin_can_list_expunge_requests if {
	authz.allow with input as {"path": ["app", "api", "expunge-requests"], "method": "GET", "session": admin_session}
}

test_client_denied_list_expunge_requests if {
	not authz.allow with input as {"path": ["app", "api", "expunge-requests"], "method": "GET", "session": client_session}
}

test_clerk_denied_list_expunge_requests if {
	not authz.allow with input as {"path": ["app", "api", "expunge-requests"], "method": "GET", "session": clerk_session}
}

test_anonymous_denied_list_expunge_requests if {
	not authz.allow with input as {"path": ["app", "api", "expunge-requests"], "method": "GET", "session": null}
}

test_lawyer_can_list_review_documents if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1", "review-documents"], "method": "GET", "session": lawyer_session}
}

test_admin_can_list_review_documents if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1", "review-documents"], "method": "GET", "session": admin_session}
}

test_client_denied_list_review_documents if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "review-documents"], "method": "GET", "session": client_session}
}

test_clerk_denied_list_review_documents if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "review-documents"], "method": "GET", "session": clerk_session}
}

test_anonymous_denied_list_review_documents if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "review-documents"], "method": "GET", "session": null}
}

# ---------- POST /app/api/projects/{id}/notations open-notation command (lawyer tier only) ----------

test_lawyer_can_create_notation if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "notations"], "method": "POST", "session": lawyer_session}
}

test_admin_can_create_notation if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "notations"], "method": "POST", "session": admin_session}
}

test_client_denied_create_notation if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "notations"], "method": "POST", "session": client_session}
}

test_clerk_denied_create_notation if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "notations"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_create_notation if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "notations"], "method": "POST", "session": null}
}

# ---------- POST /app/api/notations/{id}/answers answer-questionnaire-step command (lawyer tier only) ----------

test_lawyer_can_answer_notation_step if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1", "answers"], "method": "POST", "session": lawyer_session}
}

test_admin_can_answer_notation_step if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1", "answers"], "method": "POST", "session": admin_session}
}

test_client_denied_answer_notation_step if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "answers"], "method": "POST", "session": client_session}
}

test_clerk_denied_answer_notation_step if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "answers"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_answer_notation_step if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "answers"], "method": "POST", "session": null}
}

# ---------- POST /app/api/notations/{id}/request-changes send-back command (lawyer tier only) ----------

test_lawyer_can_request_changes if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1", "request-changes"], "method": "POST", "session": lawyer_session}
}

test_admin_can_request_changes if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1", "request-changes"], "method": "POST", "session": admin_session}
}

test_client_denied_request_changes if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "request-changes"], "method": "POST", "session": client_session}
}

test_clerk_denied_request_changes if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "request-changes"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_request_changes if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "request-changes"], "method": "POST", "session": null}
}

# ---------- POST /app/api/notations/{id}/reask resubmit command (lawyer tier only) ----------

test_lawyer_can_resubmit_reask if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1", "reask"], "method": "POST", "session": lawyer_session}
}

test_admin_can_resubmit_reask if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1", "reask"], "method": "POST", "session": admin_session}
}

test_client_denied_resubmit_reask if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "reask"], "method": "POST", "session": client_session}
}

test_clerk_denied_resubmit_reask if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "reask"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_resubmit_reask if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "reask"], "method": "POST", "session": null}
}

# ---------- POST /app/api/notations/{id}/intake send-intake-link command (lawyer tier only) ----------

test_lawyer_can_send_notation_intake if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1", "intake"], "method": "POST", "session": lawyer_session}
}

test_admin_can_send_notation_intake if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1", "intake"], "method": "POST", "session": admin_session}
}

test_client_denied_send_notation_intake if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "intake"], "method": "POST", "session": client_session}
}

test_clerk_denied_send_notation_intake if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "intake"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_send_notation_intake if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "intake"], "method": "POST", "session": null}
}

# ---------- POST /app/api/notations/{id}/approval approve-notation command (lawyer tier only) ----------

test_lawyer_can_approve_notation if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1", "approval"], "method": "POST", "session": lawyer_session}
}

test_admin_can_approve_notation if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1", "approval"], "method": "POST", "session": admin_session}
}

test_client_denied_approve_notation if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "approval"], "method": "POST", "session": client_session}
}

test_clerk_denied_approve_notation if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "approval"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_approve_notation if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "approval"], "method": "POST", "session": null}
}

# ---------- POST /app/api/notations/{id}/signature send-for-signature command (lawyer tier only) ----------

test_lawyer_can_send_notation_signature if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1", "signature"], "method": "POST", "session": lawyer_session}
}

test_admin_can_send_notation_signature if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1", "signature"], "method": "POST", "session": admin_session}
}

test_client_denied_send_notation_signature if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "signature"], "method": "POST", "session": client_session}
}

test_clerk_denied_send_notation_signature if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "signature"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_send_notation_signature if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "signature"], "method": "POST", "session": null}
}

# ---------- POST /app/api/notations/{id}/release-drafts release-drafts command (lawyer tier only) ----------

test_lawyer_can_release_drafts if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1", "release-drafts"], "method": "POST", "session": lawyer_session}
}

test_admin_can_release_drafts if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1", "release-drafts"], "method": "POST", "session": admin_session}
}

test_client_denied_release_drafts if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "release-drafts"], "method": "POST", "session": client_session}
}

test_clerk_denied_release_drafts if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "release-drafts"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_release_drafts if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "release-drafts"], "method": "POST", "session": null}
}

# ---------- POST /app/api/notations/{id}/clauses append-clause command (lawyer tier only) ----------

test_lawyer_can_add_clause if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1", "clauses"], "method": "POST", "session": lawyer_session}
}

test_admin_can_add_clause if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1", "clauses"], "method": "POST", "session": admin_session}
}

test_client_denied_add_clause if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "clauses"], "method": "POST", "session": client_session}
}

test_clerk_denied_add_clause if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "clauses"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_add_clause if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "clauses"], "method": "POST", "session": null}
}

# ---------- PATCH/DELETE /app/api/notations/{id}/clauses/{cid} + POST .../move (lawyer tier only) ----------

test_lawyer_can_edit_clause if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1", "clauses", "c1"], "method": "PATCH", "session": lawyer_session}
}

test_admin_can_delete_clause if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1", "clauses", "c1"], "method": "DELETE", "session": admin_session}
}

test_lawyer_can_move_clause if {
	authz.allow with input as {"path": ["app", "api", "notations", "n1", "clauses", "c1", "move"], "method": "POST", "session": lawyer_session}
}

test_client_denied_edit_clause if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "clauses", "c1"], "method": "PATCH", "session": client_session}
}

test_client_denied_move_clause if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "clauses", "c1", "move"], "method": "POST", "session": client_session}
}

test_anonymous_denied_delete_clause if {
	not authz.allow with input as {"path": ["app", "api", "notations", "n1", "clauses", "c1"], "method": "DELETE", "session": null}
}

# ---------- POST /app/api/review-documents/{id}/comments (any AUTHENTICATED tier — client-writable) ----------

# The new case: a CLIENT may reach this door (the handler then enforces
# client-lens matter scope). Every other /api write is lawyer-only.
test_client_can_add_review_comment if {
	authz.allow with input as {"path": ["app", "api", "review-documents", "d1", "comments"], "method": "POST", "session": client_session}
}

test_lawyer_can_add_review_comment if {
	authz.allow with input as {"path": ["app", "api", "review-documents", "d1", "comments"], "method": "POST", "session": lawyer_session}
}

test_admin_can_add_review_comment if {
	authz.allow with input as {"path": ["app", "api", "review-documents", "d1", "comments"], "method": "POST", "session": admin_session}
}

# A Clerk clears the policy layer (any authenticated), but the handler fails it
# closed (Clerk has no review-comment capability yet).
test_clerk_allowed_at_opa_add_review_comment if {
	authz.allow with input as {"path": ["app", "api", "review-documents", "d1", "comments"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_add_review_comment if {
	not authz.allow with input as {"path": ["app", "api", "review-documents", "d1", "comments"], "method": "POST", "session": null}
}

# ---------- POST /app/api/documents/{id}/deletion-requests (any AUTHENTICATED tier — client-writable) ----------

test_client_can_request_document_deletion if {
	authz.allow with input as {"path": ["app", "api", "documents", "d1", "deletion-requests"], "method": "POST", "session": client_session}
}

test_lawyer_can_request_document_deletion if {
	authz.allow with input as {"path": ["app", "api", "documents", "d1", "deletion-requests"], "method": "POST", "session": lawyer_session}
}

test_admin_can_request_document_deletion if {
	authz.allow with input as {"path": ["app", "api", "documents", "d1", "deletion-requests"], "method": "POST", "session": admin_session}
}

test_anonymous_denied_request_document_deletion if {
	not authz.allow with input as {"path": ["app", "api", "documents", "d1", "deletion-requests"], "method": "POST", "session": null}
}

# ---------- PATCH /app/api/projects/{id} descriptive update (lawyer tier only) ----------

test_lawyer_can_update_project if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1"], "method": "PATCH", "session": lawyer_session}
}

test_admin_can_update_project if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1"], "method": "PATCH", "session": admin_session}
}

test_client_denied_project_update if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1"], "method": "PATCH", "session": client_session}
}

test_clerk_denied_project_update if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1"], "method": "PATCH", "session": clerk_session}
}

test_anonymous_denied_project_update if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1"], "method": "PATCH", "session": null}
}

test_lawyer_can_delete_project if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1"], "method": "DELETE", "session": lawyer_session}
}

test_admin_can_delete_project if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1"], "method": "DELETE", "session": admin_session}
}

test_client_denied_project_delete if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1"], "method": "DELETE", "session": client_session}
}

test_clerk_denied_project_delete if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1"], "method": "DELETE", "session": clerk_session}
}

test_anonymous_denied_project_delete if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1"], "method": "DELETE", "session": null}
}

# ---------- POST /app/api/projects matter open (lawyer tier only) ----------

test_lawyer_can_open_matter if {
	authz.allow with input as {"path": ["app", "api", "projects"], "method": "POST", "session": lawyer_session}
}

test_admin_can_open_matter if {
	authz.allow with input as {"path": ["app", "api", "projects"], "method": "POST", "session": admin_session}
}

test_client_denied_matter_open if {
	not authz.allow with input as {"path": ["app", "api", "projects"], "method": "POST", "session": client_session}
}

test_clerk_denied_matter_open if {
	not authz.allow with input as {"path": ["app", "api", "projects"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_matter_open if {
	not authz.allow with input as {"path": ["app", "api", "projects"], "method": "POST", "session": null}
}

# A write to an /api resource that hasn't moved onto the command boundary
# yet stays denied — the per-resource rules are scoped to their resource,
# not a blanket /api write grant.
test_lawyer_denied_unmigrated_api_write if {
	not authz.allow with input as {"path": ["app", "api", "jurisdictions"], "method": "POST", "session": lawyer_session}
}

# ---------- stateless Template markdown validator (POST /app/api/templates/validate) ----------
# Linting Template markdown is a lawyer authoring activity — the same
# lawyer-tier gate as every other /app/api/* write/command. The canonical
# route is /app/api/templates/validate; the old /app/api/notations/validate
# alias is gone and must stay denied for everyone (see below).

test_lawyer_can_validate_template if {
	authz.allow with input as {"path": ["app", "api", "templates", "validate"], "method": "POST", "session": lawyer_session}
}

test_admin_can_validate_template if {
	authz.allow with input as {"path": ["app", "api", "templates", "validate"], "method": "POST", "session": admin_session}
}

test_client_denied_template_validate if {
	not authz.allow with input as {"path": ["app", "api", "templates", "validate"], "method": "POST", "session": client_session}
}

test_anonymous_denied_template_validate if {
	not authz.allow with input as {"path": ["app", "api", "templates", "validate"], "method": "POST", "session": null}
}

# The undocumented /app/api/notations/validate alias was removed. It carries
# no allow rule anymore, so even a lawyer caller falls through to
# default-deny — proving the alias is gone, not merely undocumented.
test_removed_notations_validate_alias_denied_for_lawyer if {
	not authz.allow with input as {"path": ["app", "api", "notations", "validate"], "method": "POST", "session": lawyer_session}
}

test_removed_notations_validate_alias_denied_for_anonymous if {
	not authz.allow with input as {"path": ["app", "api", "notations", "validate"], "method": "POST", "session": null}
}

# ---------- API documentation surfaces (decided by routing, not this policy) ----------

# The Swagger shell at /app/api (and its shorter public alias /api) and the
# OpenAPI document beside it are public: `portal::bootstrap` mounts all three
# with no session boundary and no `require_policy` layer at all, so this
# policy never evaluates a request for them — see the note above
# `/app/api/mcp.json`'s own such rule, and `portal/tests/router_contract.rs`
# for the router-level half this Rego test cannot see. Mirrored here like the
# A2A agent card below: an anonymous read must not be allowed by this policy
# either, which is the only half a policy test can prove.
test_policy_does_not_decide_api_docs if {
	not authz.allow with input as {"path": ["app", "api"], "method": "GET", "session": null}
}

test_policy_does_not_decide_openapi_json if {
	not authz.allow with input as {"path": ["app", "api", "openapi.json"], "method": "GET", "session": null}
}

# The retired top-level paths. Nothing mounts at /api-docs or /openapi.json now
# that the documentation lives under /app/api, and the policy must not carry an
# exemption that would silently re-open them if a route ever returned there.
test_anonymous_denied_retired_api_docs if {
	not authz.allow with input as {"path": ["api-docs"], "method": "GET", "session": null}
}

test_anonymous_denied_retired_openapi if {
	not authz.allow with input as {"path": ["openapi.json"], "method": "GET", "session": null}
}

# The A2A agent card. It is gated by the session boundary rather than by a rule
# here, so an anonymous read must not be allowed by this policy either.
test_policy_does_not_decide_agent_card if {
	not authz.allow with input as {"path": ["app", "api", "mcp.json"], "method": "GET", "session": null}
}

# ---------------------------------------------------------------------------
# /app/api read paths — one decision per resource, no blanket grant
# ---------------------------------------------------------------------------

# The CRM directory. Collection and item are asserted separately: the rule
# matches on a prefix, so a regression that narrowed it to the collection would
# leave `/app/api/people/{id}` silently unreachable.
test_lawyer_reads_people_collection if {
	authz.allow with input as {"path": ["app", "api", "people"], "method": "GET", "session": lawyer_session}
}

test_lawyer_reads_one_person if {
	authz.allow with input as {"path": ["app", "api", "people", "p1"], "method": "GET", "session": lawyer_session}
}

test_admin_reads_people_collection if {
	authz.allow with input as {"path": ["app", "api", "people"], "method": "GET", "session": admin_session}
}

test_owner_reads_people_collection if {
	authz.allow with input as {"path": ["app", "api", "people"], "method": "GET", "session": owner_session}
}

test_lawyer_reads_entities if {
	authz.allow with input as {"path": ["app", "api", "entities"], "method": "GET", "session": lawyer_session}
}

test_lawyer_reads_one_entity if {
	authz.allow with input as {"path": ["app", "api", "entities", "e1"], "method": "GET", "session": lawyer_session}
}

# The reason this whole section exists. Before the per-resource rules, a single
# any-authenticated GET grant admitted a `client` to the firm's entire people
# and entities directory, and the read handlers carry no tier check of their
# own — so this rule was the only thing standing there.
test_client_denied_on_people_directory if {
	not authz.allow with input as {"path": ["app", "api", "people"], "method": "GET", "session": client_session}
}

test_client_denied_on_one_person if {
	not authz.allow with input as {"path": ["app", "api", "people", "p1"], "method": "GET", "session": client_session}
}

test_client_denied_on_entities if {
	not authz.allow with input as {"path": ["app", "api", "entities"], "method": "GET", "session": client_session}
}

test_clerk_denied_on_people_directory if {
	not authz.allow with input as {"path": ["app", "api", "people"], "method": "GET", "session": clerk_session}
}

test_anonymous_denied_on_people_directory if {
	not authz.allow with input as {"path": ["app", "api", "people"], "method": "GET", "session": null}
}

# Reference data takes the same lawyer gate as the directory.
test_lawyer_reads_jurisdictions if {
	authz.allow with input as {"path": ["app", "api", "jurisdictions"], "method": "GET", "session": lawyer_session}
}

test_lawyer_reads_entity_types if {
	authz.allow with input as {"path": ["app", "api", "entity-types"], "method": "GET", "session": lawyer_session}
}

test_client_denied_on_jurisdictions if {
	not authz.allow with input as {"path": ["app", "api", "jurisdictions"], "method": "GET", "session": client_session}
}

test_client_denied_on_entity_types if {
	not authz.allow with input as {"path": ["app", "api", "entity-types"], "method": "GET", "session": client_session}
}

# Raw Template markdown stays open to every authenticated tier, because the
# notation and Template galleries link it and both admit `client`. This is the
# one read on the surface whose grant is deliberately wide, so a `client` allow
# here is the assertion rather than an oversight.
test_client_reads_raw_template_markdown if {
	authz.allow with input as {
		"path": ["app", "api", "templates", "neon-law", "shared", "retainer"],
		"method": "GET",
		"session": client_session,
	}
}

test_lawyer_reads_raw_template_markdown if {
	authz.allow with input as {
		"path": ["app", "api", "templates", "neon-law", "shared", "retainer"],
		"method": "GET",
		"session": lawyer_session,
	}
}

test_anonymous_denied_on_raw_template_markdown if {
	not authz.allow with input as {
		"path": ["app", "api", "templates", "neon-law", "shared", "retainer"],
		"method": "GET",
		"session": null,
	}
}

# The wide Template read must not widen the authoring command beside it: a
# client may read a template's source and still not lint a draft.
test_client_denied_on_template_validate_despite_the_read_grant if {
	not authz.allow with input as {"path": ["app", "api", "templates", "validate"], "method": "POST", "session": client_session}
}

# Draft is Template: each Template route has a `/drafts` twin that the same
# rules admit and refuse, tier for tier. One grant, two spellings — never a
# second policy.
test_draft_twin_of_raw_template_markdown_matches_the_canonical_grant if {
	every session in [client_session, lawyer_session, admin_session] {
		authz.allow with input as {"path": ["app", "api", "drafts", "neon-law", "shared", "retainer"], "method": "GET", "session": session}
	}
	not authz.allow with input as {"path": ["app", "api", "drafts", "neon-law", "shared", "retainer"], "method": "GET", "session": null}
	not authz.allow with input as {"path": ["app", "api", "drafts", "neon-law", "shared", "retainer"], "method": "POST", "session": lawyer_session}
}

test_draft_twin_of_the_validator_is_lawyer_tier_like_the_canonical_one if {
	authz.allow with input as {"path": ["app", "api", "drafts", "validate"], "method": "POST", "session": lawyer_session}
	authz.allow with input as {"path": ["app", "api", "drafts", "validate"], "method": "POST", "session": admin_session}
	not authz.allow with input as {"path": ["app", "api", "drafts", "validate"], "method": "POST", "session": client_session}
	not authz.allow with input as {"path": ["app", "api", "drafts", "validate"], "method": "POST", "session": null}
	# Only the exact validate path is a command; a deeper one is not.
	not authz.allow with input as {"path": ["app", "api", "drafts", "validate", "extra"], "method": "POST", "session": lawyer_session}
}

test_admin_drafts_listing_is_the_lawyer_tier_like_admin_templates if {
	every route in ["templates", "drafts"] {
		authz.allow with input as {"path": ["app", "admin", route], "method": "GET", "session": lawyer_session}
		authz.allow with input as {"path": ["app", "admin", route], "method": "GET", "session": admin_session}
		not authz.allow with input as {"path": ["app", "admin", route], "method": "GET", "session": client_session}
		not authz.allow with input as {"path": ["app", "admin", route], "method": "GET", "session": null}
	}
}

# A GET route nobody has written a rule for gets no decision. This is what the
# blanket grant used to prevent from being true, and it is the property that
# makes adding a read endpoint fail closed.
test_an_unnamed_api_read_is_denied if {
	not authz.allow with input as {"path": ["app", "api", "invoices"], "method": "GET", "session": lawyer_session}
}

# ---------- /app/team ----------
# The firm team home: every firm tier is admitted, with `client` the one
# authenticated tier denied.

test_lawyer_reaches_app_portal if {
	authz.allow with input as {"path": ["app", "team"], "method": "GET", "session": lawyer_session}
}

test_clerk_reaches_app_portal if {
	authz.allow with input as {"path": ["app", "team"], "method": "GET", "session": clerk_session}
}

test_owner_reaches_app_portal if {
	authz.allow with input as {"path": ["app", "team"], "method": "GET", "session": owner_session}
}

test_admin_reaches_app_portal if {
	authz.allow with input as {"path": ["app", "team"], "method": "GET", "session": admin_session}
}

# The denials. A client is authenticated and still refused: the firm team home
# is not part of the client-facing matter surface.
test_client_denied_app_portal if {
	not authz.allow with input as {"path": ["app", "team"], "method": "GET", "session": client_session}
}

test_anonymous_denied_app_portal if {
	not authz.allow with input as {"path": ["app", "team"], "method": "GET", "session": null}
}

# ---------- /app/profile ----------
# The self-service profile page. Every authenticated tier reaches it — the
# handler resolves the person from the session, never the URL — and only an
# anonymous caller is denied.

test_owner_reaches_app_profile if {
	authz.allow with input as {"path": ["app", "profile"], "method": "GET", "session": owner_session}
}

test_admin_reaches_app_profile if {
	authz.allow with input as {"path": ["app", "profile"], "method": "GET", "session": admin_session}
}

test_lawyer_reaches_app_profile if {
	authz.allow with input as {"path": ["app", "profile"], "method": "GET", "session": lawyer_session}
}

test_clerk_reaches_app_profile if {
	authz.allow with input as {"path": ["app", "profile"], "method": "GET", "session": clerk_session}
}

test_client_reaches_app_profile if {
	authz.allow with input as {"path": ["app", "profile"], "method": "GET", "session": client_session}
}

test_anonymous_denied_app_profile if {
	not authz.allow with input as {"path": ["app", "profile"], "method": "GET", "session": null}
}

# ---------- /app/api/ssh-keys ----------
# A Firm user's own SSH public keys (ENG-1219). Every Firm tier reaches the
# doors; a client and an anonymous caller are denied, and nothing deeper than
# `/app/api/ssh-keys/{id}` is admitted.

test_lawyer_reaches_ssh_keys if {
	authz.allow with input as {"path": ["app", "api", "ssh-keys"], "method": "POST", "session": lawyer_session}
}

test_clerk_reaches_ssh_keys if {
	authz.allow with input as {"path": ["app", "api", "ssh-keys"], "method": "GET", "session": clerk_session}
}

test_admin_revokes_ssh_key if {
	authz.allow with input as {"path": ["app", "api", "ssh-keys", "k1"], "method": "DELETE", "session": admin_session}
}

test_lawyer_revokes_ssh_key if {
	authz.allow with input as {"path": ["app", "api", "ssh-keys", "k1"], "method": "DELETE", "session": lawyer_session}
}

test_client_denied_ssh_keys if {
	not authz.allow with input as {"path": ["app", "api", "ssh-keys"], "method": "POST", "session": client_session}
}

test_anonymous_denied_ssh_keys if {
	not authz.allow with input as {"path": ["app", "api", "ssh-keys"], "method": "GET", "session": null}
}

test_lawyer_denied_deeper_ssh_key_path if {
	not authz.allow with input as {"path": ["app", "api", "ssh-keys", "k1", "extra"], "method": "GET", "session": lawyer_session}
}

# ---------- /app/avatar ----------
# The self-service avatar upload. Sibling of `/app/profile` so the native
# form action and the browser's relative resolution agree. Every
# authenticated tier is admitted; the handler resolves the person from the
# session. Only an anonymous caller is denied.

test_client_reaches_app_avatar_upload if {
	authz.allow with input as {"path": ["app", "avatar"], "method": "POST", "session": client_session}
}

test_client_reaches_app_profile_avatar_upload if {
	authz.allow with input as {"path": ["app", "profile", "avatar"], "method": "POST", "session": client_session}
}

test_anonymous_denied_app_avatar_upload if {
	not authz.allow with input as {"path": ["app", "avatar"], "method": "POST", "session": null}
}

# ---------- /app/people/{id}/avatar ----------
# Another person's avatar. Rego admits any authenticated tier at the route —
# it has no participation data to narrow further — and the handler's
# `store::access::avatar_visible_to` carries the actual, participation-scoped
# rule (docs/access-model.md). Only an anonymous caller is denied here.

test_owner_reaches_app_people_avatar if {
	authz.allow with input as {"path": ["app", "people", "p1", "avatar"], "method": "GET", "session": owner_session}
}

test_admin_reaches_app_people_avatar if {
	authz.allow with input as {"path": ["app", "people", "p1", "avatar"], "method": "GET", "session": admin_session}
}

test_lawyer_reaches_app_people_avatar if {
	authz.allow with input as {"path": ["app", "people", "p1", "avatar"], "method": "GET", "session": lawyer_session}
}

test_clerk_reaches_app_people_avatar if {
	authz.allow with input as {"path": ["app", "people", "p1", "avatar"], "method": "GET", "session": clerk_session}
}

test_client_reaches_app_people_avatar if {
	authz.allow with input as {"path": ["app", "people", "p1", "avatar"], "method": "GET", "session": client_session}
}

test_anonymous_denied_app_people_avatar if {
	not authz.allow with input as {"path": ["app", "people", "p1", "avatar"], "method": "GET", "session": null}
}

# ---------- /app/admin/brands ----------
# The house-of-brands home. Owner and Admin ride the `/app/admin` route
# bypass; Lawyer and Clerk stay denied — the same shape as `/app/admin/people`.

test_owner_reaches_app_brands if {
	authz.allow with input as {"path": ["app", "admin", "brands"], "method": "GET", "session": owner_session}
}

test_admin_reaches_app_brands if {
	authz.allow with input as {"path": ["app", "admin", "brands"], "method": "GET", "session": admin_session}
}

test_lawyer_denied_app_brands if {
	not authz.allow with input as {"path": ["app", "admin", "brands"], "method": "GET", "session": lawyer_session}
}

test_clerk_denied_app_brands if {
	not authz.allow with input as {"path": ["app", "admin", "brands"], "method": "GET", "session": clerk_session}
}

test_client_denied_app_brands if {
	not authz.allow with input as {"path": ["app", "admin", "brands"], "method": "GET", "session": client_session}
}

test_anonymous_denied_app_brands if {
	not authz.allow with input as {"path": ["app", "admin", "brands"], "method": "GET", "session": null}
}

test_owner_reaches_app_brands_edit if {
	authz.allow with input as {"path": ["app", "admin", "brands", "neon", "edit"], "method": "GET", "session": owner_session}
}

test_admin_reaches_app_brands_edit if {
	authz.allow with input as {"path": ["app", "admin", "brands", "neon", "edit"], "method": "GET", "session": admin_session}
}

test_lawyer_denied_app_brands_edit if {
	not authz.allow with input as {"path": ["app", "admin", "brands", "neon", "edit"], "method": "GET", "session": lawyer_session}
}

test_clerk_denied_app_brands_edit if {
	not authz.allow with input as {"path": ["app", "admin", "brands", "neon", "edit"], "method": "GET", "session": clerk_session}
}

test_owner_patches_app_api_brands if {
	authz.allow with input as {"path": ["app", "api", "brands", "neon"], "method": "PATCH", "session": owner_session}
}

test_admin_patches_app_api_brands if {
	authz.allow with input as {"path": ["app", "api", "brands", "neon"], "method": "PATCH", "session": admin_session}
}

test_lawyer_denied_app_api_brands_patch if {
	not authz.allow with input as {"path": ["app", "api", "brands", "neon"], "method": "PATCH", "session": lawyer_session}
}

test_clerk_denied_app_api_brands_patch if {
	not authz.allow with input as {"path": ["app", "api", "brands", "neon"], "method": "PATCH", "session": clerk_session}
}

# ENG-586: create-brand and logo/font upload doors. Named explicitly even
# though the general Owner/Admin bypass already covers every one of these
# (none is `owner_only_path`) — see the policy's own comment.
test_owner_posts_app_brands_new if {
	authz.allow with input as {"path": ["app", "admin", "brands", "new"], "method": "POST", "session": owner_session}
}

test_admin_posts_app_brands_new if {
	authz.allow with input as {"path": ["app", "admin", "brands", "new"], "method": "POST", "session": admin_session}
}

test_lawyer_denied_app_brands_new_post if {
	not authz.allow with input as {"path": ["app", "admin", "brands", "new"], "method": "POST", "session": lawyer_session}
}

test_clerk_denied_app_brands_new_post if {
	not authz.allow with input as {"path": ["app", "admin", "brands", "new"], "method": "POST", "session": clerk_session}
}

test_owner_posts_app_brands_logo if {
	authz.allow with input as {"path": ["app", "admin", "brands", "neon", "logo"], "method": "POST", "session": owner_session}
}

test_admin_posts_app_brands_logo if {
	authz.allow with input as {"path": ["app", "admin", "brands", "neon", "logo"], "method": "POST", "session": admin_session}
}

test_lawyer_denied_app_brands_logo_post if {
	not authz.allow with input as {"path": ["app", "admin", "brands", "neon", "logo"], "method": "POST", "session": lawyer_session}
}

test_owner_posts_app_brands_font if {
	authz.allow with input as {"path": ["app", "admin", "brands", "neon", "font"], "method": "POST", "session": owner_session}
}

test_admin_posts_app_brands_font if {
	authz.allow with input as {"path": ["app", "admin", "brands", "neon", "font"], "method": "POST", "session": admin_session}
}

test_lawyer_denied_app_brands_font_post if {
	not authz.allow with input as {"path": ["app", "admin", "brands", "neon", "font"], "method": "POST", "session": lawyer_session}
}

test_anonymous_denied_app_brands_new_post if {
	not authz.allow with input as {"path": ["app", "admin", "brands", "new"], "method": "POST", "session": null}
}

# ---------- /app/owner ----------
# Deployment-wide firm inventory. Owner only: the Owner/Admin route bypass
# does not apply here, so an Admin is denied the same way a Lawyer is.

test_owner_reaches_app_owner if {
	authz.allow with input as {"path": ["app", "owner"], "method": "GET", "session": owner_session}
}

test_admin_denied_app_owner if {
	not authz.allow with input as {"path": ["app", "owner"], "method": "GET", "session": admin_session}
}

test_lawyer_denied_app_owner if {
	not authz.allow with input as {"path": ["app", "owner"], "method": "GET", "session": lawyer_session}
}

test_clerk_denied_app_owner if {
	not authz.allow with input as {"path": ["app", "owner"], "method": "GET", "session": clerk_session}
}

test_client_denied_app_owner if {
	not authz.allow with input as {"path": ["app", "owner"], "method": "GET", "session": client_session}
}

test_anonymous_denied_app_owner if {
	not authz.allow with input as {"path": ["app", "owner"], "method": "GET", "session": null}
}

test_owner_can_open_owner_inbox if {
	authz.allow with input as {"path": ["app", "owner", "inbox"], "method": "GET", "session": owner_session}
}

test_admin_denied_owner_inbox if {
	not authz.allow with input as {"path": ["app", "owner", "inbox"], "method": "GET", "session": admin_session}
}

# ---------- E-Filing Transaction writes under /app/api/projects/{id}/efilings (lawyer tier, LAW-128) ----------

test_lawyer_can_record_an_efiling if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "efilings"], "method": "POST", "session": lawyer_session}
}

test_clerk_denied_record_an_efiling if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "efilings"], "method": "POST", "session": clerk_session}
}

test_client_denied_record_an_efiling if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "efilings"], "method": "POST", "session": client_session}
}

test_anonymous_denied_record_an_efiling if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "efilings"], "method": "POST", "session": null}
}

test_lawyer_denied_an_efiling_subpath_post if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "efilings", "tx1"], "method": "POST", "session": lawyer_session}
}


test_keys_owner_only if {
    authz.allow with input as {"path": ["app", "owner", "keys"], "method": "GET", "session": owner_session}
    not authz.allow with input as {"path": ["app", "owner", "keys"], "method": "GET", "session": admin_session}
    not authz.allow with input as {"path": ["app", "owner", "keys", "export"], "method": "GET", "session": admin_session}
    not authz.allow with input as {"path": ["app", "owner", "keys", "test"], "method": "POST", "session": lawyer_session}
}

# ---------- POST /app/api/projects/{id}/events/main-push (lawyer tier; the handler re-checks the CI scope) ----------

test_lawyer_can_report_a_main_push if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "events", "main-push"], "method": "POST", "session": lawyer_session}
}

test_admin_can_report_a_main_push if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "events", "main-push"], "method": "POST", "session": admin_session}
}

test_client_denied_report_a_main_push if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "events", "main-push"], "method": "POST", "session": client_session}
}

test_clerk_denied_report_a_main_push if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "events", "main-push"], "method": "POST", "session": clerk_session}
}

test_anonymous_denied_report_a_main_push if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "events", "main-push"], "method": "POST", "session": null}
}

# The plural entry redirects to the lawyer workbench under the same role boundary.
test_plural_lawyer_dashboard_has_the_same_role_boundary if {
    authz.allow with input as {"path": ["app", "lawyers"], "method": "GET", "session": lawyer_session}
    not authz.allow with input as {"path": ["app", "lawyers"], "method": "GET", "session": clerk_session}
    not authz.allow with input as {"path": ["app", "lawyers"], "method": "GET", "session": client_session}
    not authz.allow with input as {"path": ["app", "lawyers"], "method": "GET", "session": {}}
    not authz.allow with input as {"path": ["app", "lawyers"], "method": "POST", "session": lawyer_session}
}

# The tier bypass cannot grant CI content or foreign aggregate access.
test_ci_section_counts_only_own_get if {
    every role in ["lawyer", "admin", "owner"] {
        session := {"role": role, "source": "ci", "scope": {"project_id": "own", "project_code": "sample"}}
        authz.allow with input as {"path": ["app", "api", "projects", "own", "section-counts"], "method": "GET", "session": session}
        not authz.allow with input as {"path": ["app", "api", "projects", "foreign", "section-counts"], "method": "GET", "session": session}
        not authz.allow with input as {"path": ["app", "api", "projects", "own", "section-counts"], "method": "POST", "session": session}
        not authz.allow with input as {"path": ["app", "api", "projects", "own", "matter"], "method": "PUT", "session": session}
        every door in ["documents", "deadlines", "docket"] {
            not authz.allow with input as {"path": ["app", "api", "projects", "own", door], "method": "GET", "session": session}
        }
    }
}

# Relationship-graph views: lawyer tier at route admission — the handler owns
# the tenant question (matter scoping, self-or-directory, ViewDirectory), so
# route admission must not answer it. GET only, one view segment, no depth of
# path beyond the seed id.
test_lawyer_can_read_graph_views if {
    every view in ["project", "lawyer", "firm"] {
        authz.allow with input as {"path": ["app", "api", "graph", view, "seed-1"], "method": "GET", "session": lawyer_session}
    }
}

test_non_lawyer_denied_graph_views if {
    every session in [client_session, clerk_session, null] {
        every view in ["project", "lawyer", "firm"] {
            not authz.allow with input as {"path": ["app", "api", "graph", view, "seed-1"], "method": "GET", "session": session}
        }
    }
}

test_graph_views_are_read_only_and_shape_bound if {
    not authz.allow with input as {"path": ["app", "api", "graph", "project", "seed-1"], "method": "POST", "session": lawyer_session}
    not authz.allow with input as {"path": ["app", "api", "graph", "project"], "method": "GET", "session": lawyer_session}
    not authz.allow with input as {"path": ["app", "api", "graph", "project", "seed-1", "extra"], "method": "GET", "session": lawyer_session}
    not authz.allow with input as {"path": ["app", "api", "graph", "matter", "seed-1"], "method": "GET", "session": lawyer_session}
}

# ---------- Relationship-graph pages ----------
# The pages the graph islands live on. The lawyer-graph page carries its own
# lawyer-tier rule (the avatar rule's shape); the project and firm pages ride
# the matter-surface and workbench prefix rules — pinned here so a narrowing
# of either prefix breaks this test rather than leaving a page unreachable in
# production while every other check still passes.

test_lawyer_tier_reaches_lawyer_graph_page if {
    every session in [owner_session, admin_session, lawyer_session] {
        authz.allow with input as {"path": ["app", "people", "person-1", "graph"], "method": "GET", "session": session}
    }
}

test_lower_tiers_denied_lawyer_graph_page if {
    every session in [clerk_session, client_session, null] {
        not authz.allow with input as {"path": ["app", "people", "person-1", "graph"], "method": "GET", "session": session}
    }
}

test_lawyer_graph_page_is_read_only if {
    not authz.allow with input as {"path": ["app", "people", "person-1", "graph"], "method": "POST", "session": lawyer_session}
}

# Any authenticated caller is admitted at the route — the project graph page's
# loader applies the participation ledger (`store::access::can_see_project`),
# exactly as the matter surface itself does.
test_matter_graph_page_rides_the_matter_surface_rule if {
    authz.allow with input as {"path": ["app", "projects", "matter-1", "graph"], "method": "GET", "session": client_session}
    authz.allow with input as {"path": ["app", "projects", "matter-1", "graph"], "method": "GET", "session": lawyer_session}
}

test_firm_graph_page_rides_the_workbench_prefix if {
    authz.allow with input as {"path": ["app", "lawyer", "firms", "firm-1", "graph"], "method": "GET", "session": lawyer_session}
    not authz.allow with input as {"path": ["app", "lawyer", "firms", "firm-1", "graph"], "method": "GET", "session": client_session}
}


# ---------- Matter tasks under /app/api/projects/{id}/tasks (lawyer tier) ----------

# The Tasks tab's doors: the collection (GET list, POST create) and the two
# task commands. Lawyer tier at route admission; the handler resolves
# `matter_viewer` and answers 404 for a caller with no firm-side participation
# row, so route admission must not answer the tenant question either.

test_lawyer_can_list_and_create_tasks if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "tasks"], "method": "GET", "session": lawyer_session}
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "tasks"], "method": "POST", "session": lawyer_session}
}

test_admin_can_reach_the_task_collection_door if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "tasks"], "method": "POST", "session": admin_session}
}

# Clerk and client are denied the create door at the boundary. The list read
# (GET) rides the matter-read-cluster rule, which admits any authenticated
# session to five-segment /app/api/projects sub-reads and leaves the tier
# question to the handler — the costs routes ship the same way: the handler's
# `LawyerSession` extractor answers a clerk or client 403 before the matter
# question is ever asked.
test_clerk_denied_the_task_collection_door if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "tasks"], "method": "POST", "session": clerk_session}
}

test_client_denied_the_task_collection_door if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "tasks"], "method": "POST", "session": client_session}
}

test_anonymous_denied_the_task_collection_door if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "tasks"], "method": "GET", "session": null}
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "tasks"], "method": "POST", "session": null}
}

test_lawyer_can_assign_and_transition_a_task if {
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "tasks", "t1", "assign"], "method": "POST", "session": lawyer_session}
	authz.allow with input as {"path": ["app", "api", "projects", "p1", "tasks", "t1", "transition"], "method": "POST", "session": lawyer_session}
}

test_clerk_denied_task_commands if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "tasks", "t1", "assign"], "method": "POST", "session": clerk_session}
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "tasks", "t1", "transition"], "method": "POST", "session": clerk_session}
}

test_client_denied_task_commands if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "tasks", "t1", "assign"], "method": "POST", "session": client_session}
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "tasks", "t1", "transition"], "method": "POST", "session": client_session}
}

test_anonymous_denied_task_commands if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "tasks", "t1", "assign"], "method": "POST", "session": null}
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "tasks", "t1", "transition"], "method": "POST", "session": null}
}

test_task_doors_are_shape_bound if {
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "tasks"], "method": "DELETE", "session": lawyer_session}
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "tasks", "t1"], "method": "GET", "session": lawyer_session}
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "tasks", "t1", "delete"], "method": "POST", "session": lawyer_session}
	not authz.allow with input as {"path": ["app", "api", "projects", "p1", "tasks", "t1", "assign", "extra"], "method": "POST", "session": lawyer_session}
}
