package navigator.authz

import rego.v1

default allow := false
default route_allow := false

allow if {
    route_allow
    not ci_matter_content_denied
}

ci_matter_content_denied if {
    input.session.source == "ci"
    input.session.scope.project_id
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] in {"docket", "deadlines", "documents", "matter"}
    count(input.path) == 5
}

ci_matter_content_denied if {
    input.session.source == "ci"
    input.session.scope.project_id
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "section-counts"
    not own_section_counts
}

own_section_counts if {
    count(input.path) == 5
    input.path[3] == input.session.scope.project_id
    input.method == "GET"
}

route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "section-counts"
    count(input.path) == 5
    input.method == "GET"
    is_lawyer(input.session)
}

route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "costs"
    input.path[6] == "events"
    count(input.path) == 7
    input.method == "GET"
    is_lawyer(input.session)
}

# Relationship-graph views: a matter's neighborhood, a person's working
# neighborhood, and the aggregated firm-to-firm ties. Lawyer tier at route
# admission; the fine-grained matter scoping, self-or-directory authorization,
# and FirmCapability::ViewDirectory checks live in the handlers
# (`portal/src/api.rs` project_graph/lawyer_graph/firm_graph), which answer the
# non-disclosing 404 for anything out of scope — route admission decides the
# tier, never the tenant question.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "graph"
    input.path[3] in {"project", "lawyer", "firm"}
    count(input.path) == 5
    input.method == "GET"
    is_lawyer(input.session)
}

# Owner and Admin are supersets of Lawyer: either privileged tier
# passes every lawyer-work gate. Clerk is deliberately absent; adding
# a non-lawyer role must not inherit legal authority by accident.
lawyer_tier := {"owner", "admin", "lawyer"}
admin_tier := {"owner", "admin"}

is_authenticated(session) if {
    session != null
}

is_lawyer(session) if {
    session != null
    lawyer_tier[session.role]
}

is_clerk(session) if {
    session != null
    session.role == "clerk"
}

is_admin(session) if {
    session != null
    admin_tier[session.role]
}

# Owner/Admin bypass: an authenticated Owner or Admin reaches every route the
# other rules below don't otherwise allow, except `/app/owner` (the
# deployment-wide firm inventory) — Owner only. Brand CRUD lives under
# `/app/admin/brands` and rides this bypass like `/app/admin/people`. Per
# docs/access-model.md this bypass is silent — no per-read audit row.
owner_only_path if {
    input.path[0] == "app"
    input.path[1] == "owner"
    count(input.path) == 2
}

owner_only_path if {
    input.path[0] == "app"
    input.path[1] == "owner"
    input.path[2] == "inbox"
}

owner_only_path if {
    input.path[0] == "app"
    input.path[1] == "owner"
    input.path[2] == "keys"
}

route_allow if {
    is_admin(input.session)
    not owner_only_path
}

route_allow if {
    owner_only_path
    input.session.role == "owner"
}

# Matter-side testimonial entry is explicit even though Owner/Admin inherit the
# general admin route bypass above. Keeping the route in the policy makes the
# role-level contract visible beside its focused test matrix; participation is
# still enforced by the handler because Rego has no matter ledger input.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "admin"
    input.path[2] == "projects"
    count(input.path) == 5
    input.path[4] == "testimonial"
    input.method == "POST"
    is_admin(input.session)
}

# Publishing or unpublishing the matter's testimonial is the same admin door
# with a verb segment. Participation and the lawyer-tier store check stay in
# the handler; Rego carries only the role contract.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "admin"
    input.path[2] == "projects"
    count(input.path) == 6
    input.path[4] == "testimonial"
    input.path[5] in {"publish", "unpublish"}
    input.method == "POST"
    is_admin(input.session)
}

# Matter-scoped avatar upload and clear are explicit admin doors. The handler
# checks the actor's and target person's participation on the matter.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "admin"
    input.path[2] == "projects"
    count(input.path) == 7
    input.path[4] == "people"
    input.path[6] == "avatar"
    input.method == "POST"
    is_admin(input.session)
}

route_allow if {
    input.path[0] == "app"
    input.path[1] == "admin"
    input.path[2] == "projects"
    count(input.path) == 8
    input.path[4] == "people"
    input.path[6] == "avatar"
    input.path[7] == "clear"
    input.method == "POST"
    is_admin(input.session)
}

# The service start door admits clients and lawyer-tier sessions. The handler
# sends lawyer-tier POSTs to the existing lawyer start form; Clerk is not a
# lawyer tier and must not reach this door.
route_allow if {
    input.path[0] == "start"
    count(input.path) == 2
    input.session.role == "client"
}

route_allow if {
    input.path[0] == "start"
    count(input.path) == 2
    is_lawyer(input.session)
}

# The purchase page belongs to clients: they read it and buy one Offering at
# a time. A firm account has no Purchases of its own to make here.
route_allow if {
    input.path[0] == "purchases"
    count(input.path) == 1
    input.method == "GET"
    input.session.role == "client"
}

route_allow if {
    input.path[0] == "purchases"
    count(input.path) == 2
    input.method == "POST"
    input.session.role == "client"
}

# The Legal Sandbox is the subscriber's own workspace: a client reads the
# page and posts a prompt to the session. Whether the caller's subscription
# actually opens the composer — versus the preview — is the Nautilus gate's
# decision, made against the Purchase ledger; Rego carries only the role
# contract, like the purchase page above.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "nautilus"
    count(input.path) == 2
    input.method == "GET"
    input.session.role == "client"
}

route_allow if {
    input.path[0] == "app"
    input.path[1] == "nautilus"
    count(input.path) == 3
    input.path[2] == "messages"
    input.method == "POST"
    input.session.role == "client"
}

# /app/projects/* is the one matter surface (ENG-81). Every tier enters the
# same path, Clerk included now that the dedicated `/clerk` namespace is
# retired, so the policy cannot make the firm/client/supervised split here —
# and must not try. It admits any authenticated caller; which of the five
# renderings a caller gets, and whether they may see this matter at all, is
# decided in the handler by `store::access::matter_viewer`, the only layer that
# can read the participation ledger.
#
# The Clerk boundary that the separate `/clerk` mount used to carry
# structurally is now carried by that resolver: a Clerk resolves to
# `MatterViewer::Clerk` only when they hold a firm-side row *and* the matter's
# flagged lawyer DRI currently holds the lawyer tier, and that variant renders
# the name/status/supervisor page — never documents or legal work.
#
# This is deliberately weaker than the `/app/lawyer/*` rule these paths used to sit
# behind. The lawyer-only writes underneath — matter open/edit/delete, the
# participation forms, document upload, transcript intake — each re-check the
# lawyer tier in their own handler, which is what carries that guard now. A
# route added here without its own tier check is admitting every signed-in
# client.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "owner"
    input.path[2] == "inbox"
    is_authenticated(input.session)
    input.session.role == "owner"
}

route_allow if {
    input.path[0] == "app"
    input.path[1] == "inbox"
    is_authenticated(input.session)
    input.session.role in {"owner", "admin", "lawyer", "clerk"}
}

route_allow if {
    input.path[0] == "app"
    input.path[1] == "projects"
    is_authenticated(input.session)
}

# Testimonial publication is a lawyer-tier operation. The handler still
# checks the acting person's firm-side participation on the testimonial's
# project; this rule only admits the system tier to the API door.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "testimonials"
    count(input.path) == 5
    input.method == "POST"
    is_lawyer(input.session)
}

# /app/lawyer is the lawyer workbench: the firm dashboard and every
# remaining lawyer-tier listing and walk that used to sit under a
# separate `/lawyer` prefix. Owner and Admin reach it through the
# route bypass above; this prefix rule is what admits Lawyer. Clerk
# and client are denied here, which keeps the CRUD directory and
# legal-work surfaces away from a supervised non-lawyer.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "lawyer"
    is_lawyer(input.session)
}

# Reconcile one committed pointer's visibility, or set a slug on a slugless
# asset. The handler repeats the lawyer-tier matter-scope check and collapses
# an out-of-scope asset to 404. Six segments distinguishes this exact asset
# command from collection upload and all other Project subpaths. Restoring a
# missing storage object is POST .../documents/{asset_id}/storage (seven
# segments) and is not this rule: only the Owner/Admin bypass admits it, and
# the handler requires admin.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "documents"
    count(input.path) == 6
    input.method == "PATCH"
    is_lawyer(input.session)
}

# Change a document chain from one accepted kind to another: POST
# /app/api/projects/{id}/documents/{asset_id}/reclassify. Lawyer tier, the
# same gate as the PATCH asset command above; the handler repeats the
# matter-scope check and collapses an out-of-scope asset to 404. Its own
# literal seventh segment keeps it apart from the admin-only `storage` door.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "documents"
    input.path[6] == "reclassify"
    count(input.path) == 7
    input.method == "POST"
    is_lawyer(input.session)
}

# Soft-delete, restore, or place a Legal Hold on a document chain: POST
# /app/api/projects/{id}/documents/{asset_id}/{delete,restore,hold} (ENG-862).
# Lawyer tier, the same gate as reclassify above; the handler repeats the
# matter-scope check and collapses an out-of-scope asset to 404. The three
# literal seventh segments keep them apart from the admin-only `storage` door.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "documents"
    input.path[6] in {"delete", "restore", "hold"}
    count(input.path) == 7
    input.method == "POST"
    is_lawyer(input.session)
}

# Record and close a matter Deadline (LAW-126): POST
# Project cost writes are lawyer-side matter actions. Reads use the ordinary
# five-segment Project read rule and the handler applies Document visibility.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "costs"
    count(input.path) == 5
    input.method == "POST"
    is_lawyer(input.session)
}

route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "costs"
    input.path[6] in {"confirm", "invoice", "resolve"}
    count(input.path) == 7
    input.method == "POST"
    is_lawyer(input.session)
}

# /app/api/projects/{id}/deadlines, PATCH .../deadlines/{deadline_id}, and POST
# .../deadlines/{deadline_id}/{complete,cancel,reopen}. Lawyer tier, the same gate the
# `LawyerSession` extractor applies; the handler repeats the matter-scope check
# and collapses an out-of-scope Project to 404. Without these the writes behind
# `navigator project deadline` reached only Owner and Admin through the bypass.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "deadlines"
    count(input.path) == 5
    input.method == "POST"
    is_lawyer(input.session)
}

route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "deadlines"
    count(input.path) == 6
    input.method == "PATCH"
    is_lawyer(input.session)
}

route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "deadlines"
    input.path[6] in {"complete", "cancel", "reopen"}
    count(input.path) == 7
    input.method == "POST"
    is_lawyer(input.session)
}

# Record a Docket Entry (LAW-133): POST /app/api/projects/{id}/docket, and
# POST .../docket/{entry_id}/{link-document,supersede}. Lawyer tier, the same
# gate the `LawyerSession` extractor applies; the handler repeats the
# matter-scope check and collapses an out-of-scope Project or entry to 404.
# The read (GET .../docket) rides the five-segment matter read rule above.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "docket"
    count(input.path) == 5
    input.method == "POST"
    is_lawyer(input.session)
}

route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "docket"
    input.path[6] in {"link-document", "supersede"}
    count(input.path) == 7
    input.method == "POST"
    is_lawyer(input.session)
}

# Record a Service (LAW-124): POST /app/api/projects/{id}/service, and read
# one back: GET .../service/{service_id}. Lawyer tier, the same gate the
# `LawyerSession` extractor applies; the handler repeats the matter-scope
# check and collapses an out-of-scope Project or record to 404. The list
# (GET .../service) rides the five-segment matter read rule above.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "service"
    count(input.path) == 5
    input.method == "POST"
    is_lawyer(input.session)
}

route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "service"
    count(input.path) == 6
    input.method == "GET"
    is_lawyer(input.session)
}

# Store a Project's Matter Pages: PUT /app/api/projects/{id}/pages, the door
# `navigator project sync` sends the repository's `pages:` block to. Lawyer
# tier, the same gate the `LawyerSession` extractor applies; the handler
# repeats the matter-scope check and collapses an out-of-scope Project to 404.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "pages"
    count(input.path) == 5
    input.method == "PUT"
    is_lawyer(input.session)
}

# Store a Project's Matter Caption: PUT /app/api/projects/{id}/matter, the
# door `navigator project sync` sends the repository's `matter:` block to.
# Lawyer tier, the same gate the `LawyerSession` extractor applies; the
# handler repeats the matter-scope check and collapses an out-of-scope
# Project to 404.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "matter"
    count(input.path) == 5
    input.method == "PUT"
    is_lawyer(input.session)
}

# Read the rows behind the estate and cap_table Modules: GET
# /app/api/projects/{id}/{fiduciary-roles,estate-assets,cap-table-holdings}.
# Any authenticated session is admitted, because a client reads their own
# matter's rows as well as the firm. Admission only: the policy has no
# participation ledger, so the handler resolves the caller's lens on the named
# Project, answers a non-participant (another client's matter) or a Clerk 404,
# and gives a client an empty list while the owning Module is disabled.
# Read-only — every other verb on these paths is denied; seed import writes.
module_record_resources := {"fiduciary-roles", "estate-assets", "cap-table-holdings"}

route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    module_record_resources[input.path[4]]
    count(input.path) == 5
    input.method == "GET"
    is_authenticated(input.session)
}

# Log a Communication (LAW-140): POST /app/api/projects/{id}/communications.
# Lawyer tier, the same gate the `LawyerSession` extractor applies; the handler
# repeats the matter-scope check and collapses an out-of-scope Project to 404.
# The firm read (GET .../communications) rides the five-segment matter read
# rule above; the handler keeps it at Lawyer tier.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "communications"
    count(input.path) == 5
    input.method == "POST"
    is_lawyer(input.session)
}

# Record an E-Filing Transaction status (LAW-128): POST
# /app/api/projects/{id}/efilings. Lawyer tier, the same gate the
# `LawyerSession` extractor applies; the handler repeats the matter-scope check
# and collapses an out-of-scope Project to 404.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "efilings"
    count(input.path) == 5
    input.method == "POST"
    is_lawyer(input.session)
}

# Legal-files-only handoffs: POST /app/api/projects/{id}/handoffs (lawyer tier)
# and GET .../handoffs/{handoff} and .../handoffs/{handoff}/bundle (any
# authenticated session; the handler admits only a lawyer on the matter or the
# handoff's own recipient and answers anyone else 404).
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "handoffs"
    count(input.path) == 5
    input.method == "POST"
    is_lawyer(input.session)
}

route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "handoffs"
    count(input.path) == 6
    input.method == "GET"
    is_authenticated(input.session)
}

route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "handoffs"
    input.path[6] == "bundle"
    count(input.path) == 7
    input.method == "GET"
    is_authenticated(input.session)
}

# A recipient acknowledges a handoff: POST
# /app/api/projects/{id}/handoffs/{handoff}/acknowledge. Any authenticated
# session; the handler admits only the handoff's recipient.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "handoffs"
    input.path[6] == "acknowledge"
    count(input.path) == 7
    input.method == "POST"
    is_authenticated(input.session)
}

# Headless source imports: POST /app/api/projects/{id}/source-imports and
# GET .../source-imports/{import}. Lawyer tier; the handler re-checks matter
# participation and collapses out-of-scope to 404.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "source-imports"
    count(input.path) == 5
    input.method == "POST"
    is_lawyer(input.session)
}

route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "source-imports"
    count(input.path) == 6
    input.method == "GET"
    is_lawyer(input.session)
}

# Project Recovery Archives: GET and POST /app/api/projects/{id}/recovery-archives
# and POST .../recovery-archives/{generation}/verify. Admin tier only; the
# handler then requires the Firm's recovery-archive capability on the Project's
# Firm and answers anything out of scope 404.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "recovery-archives"
    count(input.path) == 5
    input.method in {"GET", "POST"}
    is_admin(input.session)
}

route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "recovery-archives"
    input.path[6] == "verify"
    count(input.path) == 7
    input.method == "POST"
    is_admin(input.session)
}

# The retention purge: POST /app/api/documents/purge. Admin tier only, since it
# runs the governed expunge, whose authorizer must be an admin.
route_allow if {
    input.path == ["app", "api", "documents", "purge"]
    input.method == "POST"
    is_admin(input.session)
}

# File one inbound message's attachments into a matter: POST
# /app/api/projects/{id}/mail/file (#517). Firm-side matter write, lawyer/admin
# only; the handler re-checks matter participation and collapses out-of-scope
# to 404. Own literal `mail`/`file` segments distinguish this from the
# documents collection POST above. Six segments and POST.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "mail"
    input.path[5] == "file"
    count(input.path) == 6
    input.method == "POST"
    is_lawyer(input.session)
}

# Read one document's revision chain (the slug travels as `?slug=`, never a path
# segment — it may itself contain `/`): GET
# /app/api/projects/{id}/documents/revisions. Any authenticated caller; the
# handler applies the caller's lens (a client never learns a lawyer-only
# revision exists) and collapses out-of-scope to 404. Own literal segment
# distinguishes it from the PATCH asset-item rule above, which shares the
# `documents` segment but not the six-segment `revisions` literal.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "documents"
    input.path[5] == "revisions"
    count(input.path) == 6
    input.method == "GET"
    is_authenticated(input.session)
}

# Storage integrity for every asset row the caller's lens can see: GET
# /app/api/projects/{id}/documents/integrity. Lawyer tier only. The handler
# collapses a matter the caller cannot see to 404. The CI document scope
# reaches this route and no document bytes.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "documents"
    input.path[5] == "integrity"
    count(input.path) == 6
    input.method == "GET"
    is_lawyer(input.session)
}

# Brand presentation writes (ENG-586), named explicitly rather than resting
# on the Owner/Admin route bypass — the same convention every other
# `/app/api` write follows. `store::brands`' own `authorize` is what actually
# decides Owner-system-wide vs. that Firm's Admin DRI; this admits the tier,
# not the scope.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "brands"
    count(input.path) == 4
    input.method == "PATCH"
    is_admin(input.session)
}

# The native create-brand form and the logo/font upload doors it links to
# (ENG-586) already pass through the general Owner/Admin bypass under
# `/app/admin`. Naming them here keeps every brand-write path in one place
# rather than splitting "explicit" writes from ones that merely survive the
# bypass.
route_allow if {
    input.path == ["app", "admin", "brands", "new"]
    input.method == "POST"
    is_admin(input.session)
}

route_allow if {
    input.path[0] == "app"
    input.path[1] == "admin"
    input.path[2] == "brands"
    input.path[4] == "logo"
    count(input.path) == 5
    input.method == "POST"
    is_admin(input.session)
}

route_allow if {
    input.path[0] == "app"
    input.path[1] == "admin"
    input.path[2] == "brands"
    input.path[4] == "font"
    count(input.path) == 5
    input.method == "POST"
    is_admin(input.session)
}

# /app/team is the firm team's home. It admits every firm tier and denies the
# client tier, which uses the Project home instead.
#
# Lawyer and Clerk are admitted by the two rules below, Owner and Admin
# through the route bypass at the top of this policy. `client` is the one
# authenticated tier denied.
#
# Clerk gets an explicit rule rather than being folded into `lawyer_tier`, for
# the reason stated at the top of this file: a non-lawyer role must never
# inherit legal authority as a side effect of being admitted somewhere.
# Downloading the CLI is not legal authority.
#
# These two rules are a prefix, so they also carry the page's own assets —
# `/app/team/fonts/gorp-serif.zip`, the licensed GORP Serif desktop family the
# home offers as a card. That download is a firm brand asset, not lawyer work,
# and its audience is exactly this page's: all four firm tiers, no client. It
# used to sit under `/app/lawyer`, where admitting a Clerk needed an exact-path
# exception to "Clerk never enters /app/lawyer"; here it needs no rule of its own.
#
route_allow if {
    input.path[0] == "app"
    input.path[1] == "team"
    is_lawyer(input.session)
}

route_allow if {
    input.path[0] == "app"
    input.path[1] == "team"
    is_clerk(input.session)
}

# The authenticated viewer's own avatar is safe for every tier and never
# accepts a person id from the caller. The handler resolves the id from the
# signed session, so this route cannot become a directory read by accident.
route_allow if {
    input.path == ["app", "me", "avatar"]
    is_authenticated(input.session)
}

# `/app/profile` is the self-service profile page: every authenticated tier
# sees (but cannot edit) their email here. The handler resolves the person
# from the signed session, never from the URL, so this is a flat
# authenticated rule rather than a tier-scoped one. The nested
# `/app/profile/avatar` POST is admitted as the same prefix.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "profile"
    is_authenticated(input.session)
}

# A native form on `/app/profile` may also post to the sibling `/app/avatar`
# when the browser replaces the last path segment. Same session-resolved
# person id as `/app/me/avatar` above.
route_allow if {
    input.path == ["app", "avatar"]
    is_authenticated(input.session)
}

# Another person's avatar is participation-scoped, not tier-scoped: every firm
# tier sees any avatar, while a client sees only their own and a fellow
# client's when the two share a Project (`docs/access-model.md`). Rego has no
# participation data to decide that here, so this rule only admits the
# authenticated tier at the route; `store::access::avatar_visible_to` in the
# handler carries the narrower, participation-aware rule.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "people"
    count(input.path) == 4
    input.path[3] == "avatar"
    is_authenticated(input.session)
}

# One person's relationship-graph page rides the avatar rule's shape: lawyer
# tier at the route, because the `GET /app/api/graph/lawyer/{id}` door the
# page's island fetches admits the person themself and the firm directory
# (`FirmCapability::ViewDirectory`) — that check lives there, not here. Clerk
# and client are denied, the same boundary the lawyer workbench carries.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "people"
    count(input.path) == 4
    input.path[3] == "graph"
    input.method == "GET"
    is_lawyer(input.session)
}

# /app/admin/brands is the house-of-brands home: every registered brand's
# typeface. Owner and Admin reach it through the route bypass at the top of
# this policy; Lawyer and Clerk stay denied. The handler then refuses anyone
# who is not Owner (system-wide brands) or that Firm's Admin DRI.

# /app/admin is Owner/Admin only at the hub, the matter directory
# (`/app/admin/projects`), Person CRUD (`/app/admin/people`), the lead
# queue (`/app/admin/leads`), and visitor analytics. Those need no rule
# of their own: the route bypass at the top of this policy is exactly
# that set. Spelled out here as a deny-by-omission note rather than a
# rule, because a prefix `is_lawyer` grant for `/app/admin` would
# silently widen the hub, the matter directory, and the lead queue to
# Lawyer.
#
# Firm-administration listings that a Lawyer already reached under `/app/lawyer`
# now live as named resources under `/app/admin`. The grant is the resource
# segment, not the prefix, so `/app/admin/people`, `/app/admin/leads`, and
# `/app/admin/projects` stay Owner/Admin. Letters and the email log are in the
# set so admission matches the old `/app/lawyer` prefix; their handlers still
# require the admin tier.
admin_lawyer_resources := {
	"letters",
	"clauses",
	"email-log",
	"addresses",
	"entities",
	"entity-types",
	"git-repositories",
	"jurisdictions",
	"mailrooms",
	"playbooks",
	"questions",
	"templates",
	"drafts",
	"schedules",
	"people.csv",
}

route_allow if {
	input.path[0] == "app"
	input.path[1] == "admin"
	count(input.path) >= 3
	admin_lawyer_resources[input.path[2]]
	is_lawyer(input.session)
}

# /app/notations/:id/documents/:doc_id exposes reviewed notation
# PDFs through the client lens. The handler resolves the notation to
# its Project and applies the client-lens project ACL before issuing
# any signed URL.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "notations"
    is_authenticated(input.session)
}

# /app/forms/* — blank government forms (public records,
# vendored from each authority's own site). Any authenticated
# person may browse the index and download a blank; the *filled*
# packets are client documents and live on project routes, never
# here.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "forms"
    is_authenticated(input.session)
}

# Workshop reads are public and never reach this policy. Claiming a completion
# certificate remains a firm-side action for Lawyer and Clerk; Owner and Admin
# reach it through the bypass above.
route_allow if {
    input.path[0] == "workshops"
    count(input.path) == 3
    input.path[2] == "certificate"
    input.method == "POST"
    is_lawyer(input.session)
}

route_allow if {
    input.path[0] == "workshops"
    count(input.path) == 3
    input.path[2] == "certificate"
    input.method == "POST"
    is_clerk(input.session)
}

# MCP (LibreChat-driven tool calls) requires a Lawyer or admin —
# the tools mutate the same CRM tables.
route_allow if {
    input.path[0] == "mcp"
    is_lawyer(input.session)
}

# A2A JSON-RPC dispatches to the same MCP tool registry — same
# lawyer-tier requirement. The agent card at /app/api/mcp.json is
# decided at the router level, not here: it composes behind the session
# boundary alone, because reading the card is not a tool call. So this
# rule governs only the RPC endpoint, the path that actually dispatches.
route_allow if {
    input.path == ["app", "api", "mcp", "rpc"]
    is_lawyer(input.session)
}

# The API documentation surfaces — the Swagger UI shell at /app/api (and its
# short public alias /api) and the OpenAPI document beside it — carry no rule
# here at all. `portal::bootstrap` mounts them with no session boundary and no
# `require_policy` layer, so this policy never sees a request for them: the
# reference is public, and a reader needs no session just to see what the API
# looks like. What the reference *describes* is a different set of paths
# entirely — `/app/api/people` and the rest of the data reads below — and
# those keep their own per-resource rules unchanged. A Clerk (or an anonymous
# reader) sees the reference but not the directory it describes.
#
# ---------------------------------------------------------------------------
# /app/api read paths — one rule per resource, no blanket grant.
# ---------------------------------------------------------------------------
#
# There used to be a single rule here admitting any authenticated caller to
# every `GET /app/api/*` path. It was the only grant on this surface that named
# no resource, and it had two consequences:
#
# 1. A `client` read the firm's whole people and entities directory. The
#    handlers carry no tier check of their own on the read path, so this rule
#    was the only thing standing there, and it said yes.
# 2. A newly added GET route was authorized the moment it was routed. Adding a
#    read endpoint could not fail closed, because the grant did not depend on
#    anyone having considered it.
#
# Each read is now named with the tier that should have it. A new GET route
# under /app/api gets no decision until a rule is written for it, which is the
# behaviour a default-deny policy is supposed to have.

# The firm's CRM directory: people and entities, collection and item alike.
# Lawyer and admin only. This is the firm's own book of clients,
# counterparties, and contacts — a client sees their own matter through the
# project surfaces, never the directory those surfaces are drawn from.
#
# The prefix match covers `/app/api/people` and `/app/api/people/{id}`
# together, because "who may list the directory" and "who may read one row of
# it" are the same question here.
api_directory_resources := {"people", "entities"}

route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    api_directory_resources[input.path[2]]
    input.method == "GET"
    is_lawyer(input.session)
}

# Reference data: the jurisdictions list and the entity-type vocabulary. Both
# are firm-side authoring inputs — they populate the lawyer matter and entity
# forms — and neither is reachable from a client surface, so they take the same
# lawyer gate as the directory rather than a wider one. They carry no client
# content, so this is about keeping the surface coherent rather than about
# secrecy: a read a client cannot use is a read a client should not have.
api_reference_resources := {"jurisdictions", "entity-types"}

route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    api_reference_resources[input.path[2]]
    input.method == "GET"
    is_lawyer(input.session)
}

# Draft is Template (docs/glossary/draft.md), so each Template route has a
# `/drafts` twin that answers by the same rule: one grant, two spellings.
template_api_resources := {"templates", "drafts"}

# Raw Template markdown: GET /app/api/templates/*path serves the source of a
# notation template. Deliberately the permissive rule — any authenticated
# person — because it is what the notation gallery at /notations and the
# Template gallery at /templates already link, and both admit every
# authenticated tier including `client`.
#
# This rule changes nothing about who reads it; before this the route simply
# had no policy layer at all and rested on the session boundary. Stating the
# grant is the point: the next reader can see that "any authenticated person"
# was decided rather than defaulted, and tightening it is now one edit here
# plus the two galleries that link it.
#
# Scoped to GET. POST /app/api/templates/validate is the lawyer-tier authoring
# command below and must not be widened by this.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    template_api_resources[input.path[2]]
    input.method == "GET"
    is_authenticated(input.session)
}

# Matter read clusters (#866). The portal's matter pages read these, and both a
# client (their own matter) and the firm reach them, so they are admitted at the
# session boundary and the handler self-scopes: `visible_projects` returns only
# the caller's matters, and the by-id reads collapse an out-of-scope resource to
# 404. Scoped to GET so the write verbs on these paths keep their own tier rules.
#
# GET /app/api/projects (list), /app/api/projects/{id} (detail),
# /app/api/projects/{id}/{participants,notations} (matter sub-reads).
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.method == "GET"
    count(input.path) <= 5
    is_authenticated(input.session)
}

# A client may save only their own testimonial under a Project. The policy
# admits the client tier to this exact route; the handler and store resolve the
# client's participation on the named Project and collapse a mismatch to 404.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "testimonial"
    count(input.path) == 5
    input.method == "POST"
    input.session.role == "client"
}

# GET /app/api/notations/{id} — one notation, scoped by its matter in the handler.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "notations"
    count(input.path) == 4
    input.method == "GET"
    is_authenticated(input.session)
}

# Firm-tool reads: the contract-review playbooks and one inbound-contract review.
# Not reachable from a client surface, so they take the lawyer tier like the
# directory reads. GET /app/api/playbooks, /app/api/playbooks/{id},
# /app/api/contract-reviews/{id}.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "playbooks"
    input.method == "GET"
    is_lawyer(input.session)
}

route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "contract-reviews"
    count(input.path) == 4
    input.method == "GET"
    is_lawyer(input.session)
}

# The pending client-deletion queue and a notation's review drafts are firm work
# product, so they take the lawyer tier. GET /app/api/expunge-requests and
# /app/api/notations/{id}/review-documents. (A matter's documents and
# conversation are read under the `projects` GET rule above, client-visible-
# filtered in the handler.)
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "expunge-requests"
    count(input.path) == 3
    input.method == "GET"
    is_lawyer(input.session)
}

route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "notations"
    input.path[4] == "review-documents"
    count(input.path) == 5
    input.method == "GET"
    is_lawyer(input.session)
}

# Stateless Template markdown validator: POST /app/api/templates/validate
# lints draft markdown and returns violations. It touches no database,
# but linting a Template is a lawyer authoring activity — so it takes
# the same lawyer-tier gate as every other /app/api/* command, not the
# any-authenticated GET grant. The canonical path is
# /app/api/templates/validate; there is no /app/api/notations/validate alias.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    template_api_resources[input.path[2]]
    input.path[3] == "validate"
    count(input.path) == 4
    input.method == "POST"
    is_lawyer(input.session)
}

# People command resource: create/update/delete and the welcome-send
# command move through POST/PATCH/DELETE /app/api/people*. These are the
# REST command boundary for both the browser lawyer forms (cookie +
# CSRF) and machine clients (bearer). Owner/Admin/Lawyer only — the
# handler re-checks the tier, but the policy gate is the first line so a
# client or anonymous write never reaches it. Read paths stay open to
# any authenticated caller via the GET rule above.
api_write_methods := {"POST", "PATCH", "DELETE"}

route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "people"
    api_write_methods[input.method]
    is_lawyer(input.session)
}

# Entity command resource: POST /app/api/entities is the REST command
# boundary the lawyer create form, the inline "Add entity" modal on the
# project form, and machine clients all travel. Same lawyer-tier gate as
# the People commands — a client or anonymous write never reaches the
# handler's own check. Read paths stay open to any authenticated caller
# via the GET rule above.
#
# The entities resource is now complete on the command boundary: create,
# update, and delete all have handlers, so this grant covers the full
# `api_write_methods` set. Each verb was added here in the same change
# that added its handler, never ahead of it.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "entities"
    api_write_methods[input.method]
    is_lawyer(input.session)
}

# Seed reconciliation: POST /app/api/seed accepts the same YAML envelope as
# the checked-in seed files. The handler maps its model name through the typed
# seed registry, so this rule grants a command rather than arbitrary database
# access. It is lawyer-tier for the same reason as directory writes.
route_allow if {
    input.path == ["app", "api", "seed"]
    input.method == "POST"
    is_lawyer(input.session)
}

# Create (or find) the global Authority for a citation, archiving its
# artifact: POST /app/api/authorities. An Authority carries no `project_id`
# (#890's global reference data), so this sits on its own noun rather than
# under `projects` — the same reason `integrations` and `project-surfaces`
# do. Lawyer-tier only, the same gate as every other `/app/api` authoring
# door. Scoped to the collection path (three segments) and POST.
route_allow if {
    input.path == ["app", "api", "authorities"]
    input.method == "POST"
    is_lawyer(input.session)
}

# Resolve filed global Authority metadata with the archive's lawyer-tier gate.
route_allow if {
    count(input.path) == 4
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "authorities"
    input.method == "GET"
    is_lawyer(input.session)
}

# Read an Authority's archived artifact: GET /app/api/authorities/{id}/archive
# (LAW-93). The read half of the write above, lawyer-tier for the same reason;
# exactly five segments and GET.
route_allow if {
    count(input.path) == 5
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "authorities"
    input.path[4] == "archive"
    input.method == "GET"
    is_lawyer(input.session)
}

# Upload a contract for playbook review: POST /app/api/projects/{id}/contract-review
# ingests an inbound third-party contract (multipart) and runs the deviation
# analysis. Client-writable like the review surface — a matter's client may
# submit their own contract, or the firm may — so any authenticated caller is
# admitted; the handler then enforces matter scope through either lens (lawyer
# or client), collapsing a non-participant to 404. Scoped to the
# contract-review sub-path and POST.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "contract-review"
    input.method == "POST"
    is_authenticated(input.session)
}

# Add-participant command: POST /app/api/projects/{id}/participants adds a person
# to a matter's participation ledger (co-counsel, paralegal, a second client
# contact, or a Clerk being granted portal visibility). Firm-side
# matter-management action, so lawyer/admin only; a client or anonymous write
# never reaches the handler's own LawyerSession check. The handler additionally
# re-checks that the acting lawyer participates in the matter (admin bypasses),
# same as `/close` below, and collapses an out-of-scope matter to 404. Scoped to
# the participants sub-path and POST, the one verb this slice ships — edit/remove
# earn their own grants when they land.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "participants"
    input.method == "POST"
    is_lawyer(input.session)
}

# Open a matter's closing-letter notation: POST /app/api/projects/{id}/close is
# the REST mirror of the lawyer close control. Firm-side matter action,
# lawyer/admin only at the tier; the handler additionally re-checks that the
# acting lawyer participates in the matter (admin bypasses) and collapses an
# out-of-scope matter to 404. Scoped to the close sub-path (five segments) and
# POST, the one verb it ships.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "close"
    count(input.path) == 5
    input.method == "POST"
    is_lawyer(input.session)
}

# Start one FinanceAggregate workflow run: POST /app/api/projects/{id}/finance/run.
# Firm-side matter action, lawyer/admin only at the tier; the handler re-checks
# that the acting lawyer participates in the matter (admin bypasses) and
# collapses an out-of-scope matter to 404. Six segments and POST. The read side,
# GET /app/api/projects/{id}/finance, is five segments and falls under the
# projects GET rule above.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "finance"
    input.path[5] == "run"
    count(input.path) == 6
    input.method == "POST"
    is_lawyer(input.session)
}

# A Project repository's CI reports a push to `main`: POST
# /app/api/projects/{id}/events/main-push. Lawyer tier at the policy — the CI
# session `project-gate.yml` mints is attributed to the Project's lawyer DRI —
# and the handler then requires that session's document scope to name this
# exact Project, refusing every other session with 403. Six segments and POST.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "events"
    input.path[5] == "main-push"
    count(input.path) == 6
    input.method == "POST"
    is_lawyer(input.session)
}

# Move a matter directly through its lifecycle (close/reopen/archive): POST
# /app/api/projects/{id}/lifecycle reaches `store::projects::transition_project`
# directly — no closing-letter notation opens here, unlike `/close` above.
# Lawyer/admin only at the tier, same as the bare PATCH/DELETE matter path
# below: a lifecycle move is firm administration, not scoped to the acting
# lawyer's own matters. Scoped to the lifecycle sub-path (five segments) and
# POST, the one verb it ships.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "lifecycle"
    count(input.path) == 5
    input.method == "POST"
    is_lawyer(input.session)
}

# Approve a released estate plan: POST /app/api/projects/{id}/approve-plan is
# the client approving their own plan, so — unlike every other project write —
# it is client-writable. Any authenticated caller is admitted here; the handler
# enforces client-lens matter access (a caller who is not this matter's client
# sees 404). Scoped to the approve-plan sub-path (five segments) and POST.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "approve-plan"
    count(input.path) == 5
    input.method == "POST"
    is_authenticated(input.session)
}

# Post a matter conversation message: POST
# /app/api/projects/{id}/conversation/messages. Both the client and the firm
# post here, so it is client-writable. Any authenticated caller is admitted; the
# handler enforces either-lens matter access (a non-participant sees 404) and the
# tier decides the message side. Scoped to the conversation/messages sub-path
# (six segments) and POST.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "conversation"
    input.path[5] == "messages"
    count(input.path) == 6
    input.method == "POST"
    is_authenticated(input.session)
}

# Ensure or reconcile a Project's Firm-private provider resources:
# POST /app/api/integrations/{notion,slack,calendar}/{verb}. Admin-only, and on its own
# noun rather than under `projects` for the same reason `project-surfaces` is —
# the projects GET rule admits any authenticated caller up to five segments, so
# nesting a provisioning path there would make it policy-reachable by a client.
# Five segments and POST; the handler owns which verbs exist.
route_allow if {
	input.path[0] == "app"
	input.path[1] == "api"
	input.path[2] == "integrations"
	count(input.path) == 5
	input.method == "POST"
	is_admin(input.session)
}

# Reconcile every matter against the repository it records: GET
# /app/api/project-repositories. Admin-only, and it carries its own noun rather
# than sitting under `projects` on purpose — the projects GET rule above admits
# any authenticated caller up to five segments, so nesting this there would make
# it policy-reachable by a client. Three segments and GET.
route_allow if {
	input.path[0] == "app"
	input.path[1] == "api"
	input.path[2] == "project-repositories"
	count(input.path) == 3
	input.method == "GET"
	is_admin(input.session)
}

# Read every matter's lifecycle fields: GET /app/api/project-lifecycle.
# Admin-only, and it carries its own noun because the projects GET rule admits
# any authenticated caller up to five segments. Three segments and GET.
route_allow if {
	input.path[0] == "app"
	input.path[1] == "api"
	input.path[2] == "project-lifecycle"
	count(input.path) == 3
	input.method == "GET"
	is_admin(input.session)
}

# Create or adopt a Project's Drive ingest folder and source repository:
# POST /app/api/project-surfaces/{id}. Admin-only, and it carries its own
# noun rather than sitting under `projects` on purpose — the projects GET
# rule above admits any authenticated caller up to five segments, so nesting
# this there would make it policy-reachable by a client. Four segments and
# POST.
route_allow if {
	input.path[0] == "app"
	input.path[1] == "api"
	input.path[2] == "project-surfaces"
	count(input.path) == 4
	input.method == "POST"
	is_admin(input.session)
}

# Read the nonsecret manifest coordinates for a single matter. The handler
# enforces participation and document-token Project scope; clients never see
# private Slack/Notion coordinates through this door.
route_allow if {
	input.path[0] == "app"
	input.path[1] == "api"
	input.path[2] == "project-surfaces"
	count(input.path) == 4
	input.method == "GET"
	is_lawyer(input.session)
}

# Authorize a client document-deletion request: POST
# /app/api/expunge-requests/{id}/authorize runs the governed expunge, so it is
# admin-only — the one write on this surface that the lawyer tier alone cannot
# fire. Scoped to the authorize sub-path (five segments) and POST.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "expunge-requests"
    input.path[4] == "authorize"
    count(input.path) == 5
    input.method == "POST"
    is_admin(input.session)
}

# Deny a client document-deletion request: POST
# /app/api/expunge-requests/{id}/deny resolves it without deleting anything, so
# the lawyer tier may fire it. Scoped to the deny sub-path (five segments) and
# POST.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "expunge-requests"
    input.path[4] == "deny"
    count(input.path) == 5
    input.method == "POST"
    is_lawyer(input.session)
}

# Create a contract-review playbook: POST /app/api/playbooks. A firm tool for
# authoring a Company's negotiating positions; lawyer/admin only. Scoped to the
# playbooks collection (three segments) and POST.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "playbooks"
    count(input.path) == 3
    input.method == "POST"
    is_lawyer(input.session)
}

# Replace a playbook's positions: PUT /app/api/playbooks/{id}. Same firm tool;
# lawyer/admin only. Scoped to the playbook item (four segments) and PUT.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "playbooks"
    count(input.path) == 4
    input.method == "PUT"
    is_lawyer(input.session)
}

# The contract-review workbench: save a finding, edit the summary, approve, or
# reject an inbound-contract review. Firm-side matter action, lawyer/admin only
# at the tier; each handler additionally re-checks the acting lawyer participates
# in the review's matter (admin bypasses) and collapses out-of-scope to 404.
# Save a finding: POST /app/api/contract-reviews/{id}/findings/{idx} (six segs).
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "contract-reviews"
    input.path[4] == "findings"
    count(input.path) == 6
    input.method == "POST"
    is_lawyer(input.session)
}

# Edit the summary, approve, or reject: POST
# /app/api/contract-reviews/{id}/{summary,approve,reject} (five segments).
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "contract-reviews"
    input.path[4] in {"summary", "approve", "reject"}
    count(input.path) == 5
    input.method == "POST"
    is_lawyer(input.session)
}

# File a document into a matter: POST /app/api/projects/{id}/documents. Firm-side
# matter write, lawyer/admin only; the handler re-checks matter participation and
# collapses out-of-scope to 404. Scoped to the documents sub-path (five segments)
# and POST.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "documents"
    count(input.path) == 5
    input.method == "POST"
    is_lawyer(input.session)
}

# Run a transcript against a notation's questionnaire: POST
# /app/api/notations/{id}/transcript. Firm-side matter action, lawyer/admin only;
# the handler re-checks matter participation. Scoped to the transcript sub-path
# under notations (five segments) and POST.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "notations"
    input.path[4] == "transcript"
    count(input.path) == 5
    input.method == "POST"
    is_lawyer(input.session)
}

# File an estate matter's sitting transcript: POST
# /app/api/projects/{id}/notations/{nid}/transcript. Firm-side matter action,
# lawyer/admin only; the handler re-checks participation and that the notation
# belongs to the matter. Scoped to the transcript sub-path under a project's
# notation (seven segments) and POST.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "notations"
    input.path[6] == "transcript"
    count(input.path) == 7
    input.method == "POST"
    is_lawyer(input.session)
}

# Edit + remove a participation row: PATCH/DELETE
# /app/api/projects/{id}/participants/{role_id} rewrite or drop one participant
# (the command refuses to strand the matter's lawyer DRI) — this is also how a
# participating lawyer revokes a Clerk's portal visibility. Same firm-side
# matter-management action as the add above; lawyer/admin only at the tier, and
# the handler re-checks matter participation the same way. Scoped to the
# participant-item path (five segments) and the two mutating verbs.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "participants"
    count(input.path) == 6
    input.method in {"PATCH", "DELETE"}
    is_lawyer(input.session)
}

# Designate or clear a participant's DRI marker: PUT/DELETE
# /app/api/projects/{id}/participants/{role_id}/dri. Same firm-side matter
# self-governance as the edit/remove above; lawyer/admin only at the tier. This
# grant is necessary but not sufficient — the command additionally gates the
# change on the caller already holding the marker for that side, so a lawyer who
# is not a current DRI is admitted here and refused by the command. Scoped to the
# dri sub-path (seven segments) and the two mutating verbs.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "participants"
    input.path[6] == "dri"
    count(input.path) == 7
    input.method in {"PUT", "DELETE"}
    is_lawyer(input.session)
}

# Record adversity from a matter: POST /app/api/projects/{id}/relationships/adverse
# asserts that the Project's client is adverse to a party the Project has
# referenced, landing a manual, full-confidence `adverse_to` edge in the
# Conflict-Check Graph. Firm-side matter action, lawyer/admin only at the
# tier; the handler additionally re-checks that the acting lawyer
# participates in the matter (admin bypasses) and collapses an out-of-scope
# matter to 404. The policy reads no project reference: a reference grants
# nothing, so there is nothing here for it to decide. Scoped to the
# relationships/adverse sub-path (six segments) and POST.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "relationships"
    input.path[5] == "adverse"
    count(input.path) == 6
    input.method == "POST"
    is_lawyer(input.session)
}

# Open a notation on a matter: POST /app/api/projects/{id}/notations opens a
# notation from a template authored in the matter's own repo (or the bundled
# firm catalog). Firm-side matter action, lawyer/admin only at the
# tier; this lawyer grant is necessary but not sufficient, because the handler
# additionally re-checks that the acting lawyer participates in the matter
# (admin bypasses) and collapses an out-of-scope matter to 404. Scoped to the
# notations sub-path and POST, the one verb this slice ships.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    input.path[4] == "notations"
    input.method == "POST"
    is_lawyer(input.session)
}

# Answer a notation's questionnaire step: POST /app/api/notations/{id}/answers
# records the answer to the step the questionnaire is currently asking,
# attributed to the acting lawyer. Firm-side matter action, lawyer/admin
# only at the tier; the handler additionally re-checks that the acting lawyer
# participates in the notation's matter (admin bypasses) and collapses an
# out-of-scope notation to 404. The client-facing self-serve intake (the
# magic-link walk) is a separate surface, not this REST path. Scoped to the
# answers sub-path and POST.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "notations"
    input.path[4] == "answers"
    input.method == "POST"
    is_lawyer(input.session)
}

# Send a notation back for changes: POST
# /app/api/notations/{id}/request-changes flags answers and fires the
# changes_requested transition. Firm-side matter action, lawyer/admin only at
# the tier; the handler additionally re-checks matter participation (admin
# bypasses) and collapses an out-of-scope notation to 404. Scoped to the
# request-changes sub-path and POST.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "notations"
    input.path[4] == "request-changes"
    input.method == "POST"
    is_lawyer(input.session)
}

# Resubmit a re-collected notation: POST /app/api/notations/{id}/reask writes the
# re-collected answers and fires intake_resubmitted. Firm-side matter action,
# lawyer/admin only at the tier; the handler additionally re-checks matter
# participation (admin bypasses) and collapses an out-of-scope notation to 404.
# Scoped to the reask sub-path and POST.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "notations"
    input.path[4] == "reask"
    input.method == "POST"
    is_lawyer(input.session)
}

# Send a notation's client their self-serve intake link: POST
# /app/api/notations/{id}/intake emails the matter's client the magic link that
# backs their intake walk. Firm-side matter action, lawyer/admin only
# at the tier; the handler additionally re-checks that the acting lawyer
# participates in the notation's matter (admin bypasses) and collapses an
# out-of-scope notation to 404. Scoped to the intake sub-path and POST.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "notations"
    input.path[4] == "intake"
    input.method == "POST"
    is_lawyer(input.session)
}

# Approve a notation parked at lawyer_review: POST /app/api/notations/{id}/approval
# re-assembles the reviewed document and fires the `approved` transition so
# the worker renders the PDF. A binding attorney action (the reviewed paper
# is what goes out for signature next), so lawyer/admin only at the
# tier; the handler additionally re-checks that the acting lawyer participates
# in the notation's matter (admin bypasses) and collapses an out-of-scope
# notation to 404. Scoped to the approval sub-path and POST.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "notations"
    input.path[4] == "approval"
    input.method == "POST"
    is_lawyer(input.session)
}

# Dispatch a notation for signature: POST /app/api/notations/{id}/signature fires
# `pdf_persisted` and sends exactly one signature envelope. The binding send
# — the reviewed engagement goes out for the client's signature — so lawyer
# lawyer/admin only at the tier; the handler additionally re-checks that the
# acting lawyer participates in the notation's matter (admin bypasses) and
# collapses an out-of-scope notation to 404. Scoped to the signature
# sub-path and POST.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "notations"
    input.path[4] == "signature"
    input.method == "POST"
    is_lawyer(input.session)
}

# Release an estate notation's drafts to client review: POST
# /app/api/notations/{id}/release-drafts advances lawyer_review -> client_review
# and flips every generated draft instrument to pending_review (making it
# visible to the client). The attorney gate before a client-facing legal
# document leaves `draft`, so lawyer/admin only at the tier; the
# handler additionally re-checks that the acting lawyer participates in the
# notation's matter (admin bypasses) and collapses an out-of-scope notation
# to 404. Scoped to the release-drafts sub-path and POST.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "notations"
    input.path[4] == "release-drafts"
    input.method == "POST"
    is_lawyer(input.session)
}

# Append a custom clause to a notation's document: POST
# /app/api/notations/{id}/clauses adds firm-authored prose spliced into the
# assembled body at render time. Firm-side document authoring, so lawyer
# lawyer/admin only at the tier; the handler additionally re-checks that the
# acting lawyer participates in the notation's matter (admin bypasses) and
# collapses an out-of-scope notation to 404. Scoped to the clauses sub-path
# (the collection, four segments) and POST.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "notations"
    input.path[4] == "clauses"
    count(input.path) == 5
    input.method == "POST"
    is_lawyer(input.session)
}

# Edit + remove a clause: PATCH/DELETE /app/api/notations/{id}/clauses/{clause_id}
# rewrite or drop one clause. Same firm-side document authoring as the append
# above; lawyer/admin only, and the handler re-checks matter scope and
# that the clause belongs to the notation. Scoped to the clause-item path
# (five segments) and the two mutating verbs.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "notations"
    input.path[4] == "clauses"
    count(input.path) == 6
    input.method in {"PATCH", "DELETE"}
    is_lawyer(input.session)
}

# Reorder a clause: POST /app/api/notations/{id}/clauses/{clause_id}/move swaps it
# with its neighbour. Same firm-side authoring action; lawyer/admin
# only. Scoped to the clause move sub-path (six segments) and POST.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "notations"
    input.path[4] == "clauses"
    count(input.path) == 7
    input.path[6] == "move"
    input.method == "POST"
    is_lawyer(input.session)
}

# Add a comment to a review document: POST /app/api/review-documents/{id}/comments.
# The FIRST client-writable /api door — the read-only review surface lets a
# matter's CLIENT annotate the document, so this allows any authenticated
# caller (not just lawyer), mirroring the /app review surface. The handler
# enforces client-lens matter scope (a firm-side-only lawyer or a
# non-participant sees 404) and derives the comment's direction from the
# caller's role. Scoped to the comments sub-path and POST.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "review-documents"
    input.path[4] == "comments"
    input.method == "POST"
    is_authenticated(input.session)
}

# Request a document's deletion: POST /app/api/documents/{id}/deletion-requests.
# A matter participant (typically the client) asks for a document to be
# deleted; this only records a pending request — a lawyer/admin must
# authorize the actual expunge. Client-writable like the review surface, so
# any authenticated caller is admitted; the handler enforces client-lens
# matter scope (a firm-side-only lawyer or a non-participant sees 404).
# Scoped to the deletion-requests sub-path and POST.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "documents"
    input.path[4] == "deletion-requests"
    input.method == "POST"
    is_authenticated(input.session)
}

# Project update + delete on the bare matter path:
# PATCH /app/api/projects/{id} edits a matter's descriptive fields (no
# lifecycle/status, no conflict check, no repo provisioning, no price move);
# DELETE /app/api/projects/{id} removes a matter (blocked by the database when
# dependents still reference it). Lawyer/admin only. Scoped to these
# two verbs on the bare matter path — the matter-open (POST) verb earns its
# own grant in the change that adds its handler.
project_bare_methods := {"PATCH", "DELETE"}

route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    count(input.path) == 4
    project_bare_methods[input.method]
    is_lawyer(input.session)
}

# Matter open: POST /app/api/projects opens a new matter — the conflict check,
# the opening attorney's required conflict attestation, and the DRI
# designations. At this firm `lawyer` is an attorney,
# so this lawyer/admin gate is the "an attorney is opening (and attesting)"
# check; a client or anonymous caller never reaches the handler. Scoped to
# POST on the collection path (`/app/api/projects`, length 2), distinct from the
# per-matter verbs above.
route_allow if {
    input.path[0] == "app"
    input.path[1] == "api"
    input.path[2] == "projects"
    count(input.path) == 3
    input.method == "POST"
    is_lawyer(input.session)
}

# NOTE: the API documentation surfaces (/app/api, its /api alias, and
# /app/api/openapi.json) are NOT decided here. `portal::bootstrap` mounts them
# with no session boundary and no `require_policy` layer at all — see
# `portal::api::doc_routes` — so this policy never evaluates a request for
# them. They are public: a reader needs no session just to see what the API
# looks like, and the document itself carries no client data, only the shape
# of the operations below.
#
# This looks like the same public exemption the #204 incident retired, and
# the difference is what makes it safe again. #204's danger was an
# allow-rule-shaped grant a stale ConfigMap could default-deny out from under
# a path that needed to stay open — the bundle and the `web` image redeploy on
# separate schedules, so a lagging bundle silently narrowed access. A routing
# decision has no such failure mode: there is no policy evaluation on these
# paths to go stale, so a lagging bundle cannot narrow OR widen what they
# serve. The lesson survives; it just never applied to a path this policy
# never sees.
#
# The API those docs describe is a different question, and this file still
# answers it: `/app/api/people` and every other operation below still
# composes behind `portal::auth::require_session`, and this policy still
# decides which authenticated tier proceeds. Reading the reference and
# calling the API it describes are two different gates.

# The plural workbench URL redirects to the singular lawyer surface.
route_allow if {
    input.path == ["app", "lawyers"]
    input.method == "GET"
    is_lawyer(input.session)
}
