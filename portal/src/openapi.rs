//! OpenAPI 3.1 description of the JSON `/app/api/*` surface.
//!
//! Hand-curated rather than generated so the schema stays
//! deliberately small and free of toolchain ceremony — each entity
//! we expose ends up as one short `components.schemas` entry. When
//! the API grows enough to justify it, a future commit can swap
//! this for a `utoipa`-derived doc without changing the wire path.
//!
//! Scope: this document covers the JSON endpoints under `/app/api/*`.
//! Every operation requires OIDC authentication — either a
//! browser session cookie (`navigator_session`) issued by the OAuth
//! flow at `/auth/login`, or an upstream-validated JWT / Google OAuth
//! bearer token. The MCP endpoint at `/mcp` is JSON-RPC over HTTP
//! (not REST) and is intentionally NOT documented here — Swagger UI's
//! "Try it out" affordance would mislead callers and risks leaking
//! bearer tokens pasted into a static UI. MCP clients should consult
//! the MCP specification directly.
//!
//! Drift between this document and `api::routes()` is asserted by
//! `web/tests/openapi_drift.rs` at `(method, path)` granularity.
//!
//! # API versioning: `/app/api` is the pre-1.0 contract
//!
//! The surface is mounted unversioned at `/app/api`, not `/app/api/v1`. That is
//! a deliberate, current decision: while the app is pre-1.0 the `/app/api`
//! paths are the working contract and may still change shape as
//! resources move onto the command boundary (issue #355). It is not an
//! oversight and no `/app/api/v1` alias exists.
//!
//! Once the contract is depended on externally, a **breaking** change to
//! any documented request/response (a removed field, a renamed path, a
//! narrowed type) requires an explicit versioning decision — introduce
//! `/app/api/v1` and serve both for a deprecation window — rather than
//! silently changing the wire format under the existing `/app/api` path.
//! Additive, backward-compatible changes (a new optional field, a new
//! endpoint) stay on `/app/api`. This note is the record of that decision so
//! a future contributor does not either bump a version reflexively or,
//! worse, mutate the wire format in place.

use serde_json::{json, Value};

use views::brand::FIRM_BRAND;

/// Documentation placeholder used only when neither an explicit
/// override nor a request host is available. Matches the placeholder
/// substitution flow in `examples/deploy/k8s/gke/`.
const PLACEHOLDER_BASE_URL: &str = "https://www.your-domain.example";

/// Resolve the public-facing base URL for the OpenAPI `servers` and
/// `contact` blocks. Precedence, mirroring how the A2A agent card
/// resolves its authority in [`crate::a2a`]:
///
/// 1. The mounted brand bundle's `base_url`.
/// 2. `NAV_BASE_URL` — a non-brand operational override.
/// 3. The `authority` from the incoming request's `Host` header — so a
///    deploy surfaces its own host (`www.neonlaw.com` in prod) with
///    zero config and no hard-coded domain in source.
/// 4. The documentation placeholder.
#[must_use]
pub fn base_url_for(authority: Option<&str>) -> String {
    if !views::brand::base_url().is_empty() {
        return views::brand::base_url().to_string();
    }
    if let Ok(explicit) = std::env::var("NAV_BASE_URL") {
        if !explicit.is_empty() {
            return explicit;
        }
    }
    match authority.filter(|a| !a.is_empty()) {
        Some(authority) => {
            // Loopback hosts are dev-only and never TLS-terminated.
            let scheme = if authority.starts_with("localhost")
                || authority.starts_with("127.0.0.1")
                || authority.starts_with("0.0.0.0")
            {
                "http"
            } else {
                "https"
            };
            format!("{scheme}://{authority}")
        }
        None => PLACEHOLDER_BASE_URL.to_string(),
    }
}

/// The OpenAPI document with the base URL resolved from env / request
/// host. Convenience wrapper over [`document_with_base`] used by the
/// drift test and unit tests, where no request host is available.
#[must_use]
pub fn document() -> Value {
    document_with_base(&base_url_for(None))
}

/// The OpenAPI document, with every operation its `/draft` twin documents
/// beside it.
#[must_use]
pub fn document_with_base(base: &str) -> Value {
    let mut document = canonical_document(base);
    add_draft_aliases(&mut document);
    add_journal_operations(&mut document);
    add_blog_operations(&mut document);
    document["paths"]["/app/api/projects/{id}/section-counts"] = serde_json::json!({
      "get": {
        "operationId": "getProjectSectionCounts", "tags": ["Projects"],
        "summary": "Count Project sections without reading their content",
        "description": "Returns only Docket Entry, open Deadline and live Document counts. A document-scoped CI session can read only its own Project. Human lawyer-tier sessions require matter participation.",
        "parameters": [{"name":"id","in":"path","required":true,"schema":{"type":"string","format":"uuid"}}],
        "responses": {
          "200": {"description":"Content-free counts", "content":{"application/json":{"schema":{
            "type":"object","additionalProperties":false,"required":["docket","deadlines","documents"],
            "properties":{"docket":{"type":"integer","minimum":0},"deadlines":{"type":"integer","minimum":0},"documents":{"type":"integer","minimum":0}}
          }}}},
          "401":{"description":"Unauthenticated"},"403":{"description":"CI scope or tier denied"},"404":{"description":"Project unavailable or outside matter scope"}
        }
      }
    });
    document
}

fn record_post_schema() -> Value {
    json!({
        "type":"object","additionalProperties":false,
        "required":["brand","slug","title","description","body_markdown","publish_at"],
        "properties":{
            "brand":{"type":"string"},"slug":{"type":"string"},
            "title":{"type":"string","maxLength":120},
            "description":{"type":"string","maxLength":200},
            "body_markdown":{"type":"string"},
            "publish_at":{"type":"string","format":"date-time","description":"Actual publication time, no later than now."},
            "video_asset":{"type":"string","description":"Existing MP4 below img/<brand>/; measured before recording."}
        }
    })
}

fn add_blog_operations(document: &mut Value) {
    let publish = json!({
        "type":"object", "additionalProperties":false,
        "required":["brand","slug","title","description","body_markdown","providers","account_ids"],
        "properties":{
            "brand":{"type":"string"},"slug":{"type":"string"},
            "title":{"type":"string","maxLength":120},"description":{"type":"string","maxLength":200},
            "body_markdown":{"type":"string"},
            "providers":{"type":"array","minItems":1,"uniqueItems":true,"items":{"type":"string","enum":["facebook","instagram","threads","youtube","linkedin","x","tiktok"]}},
            "account_ids":{"type":"object","additionalProperties":{"type":"string"},"description":"Approved provider account ID for every selected provider; mismatches are refused before writing."},
            "publish_at":{"type":"string","format":"date-time"},
            "video_asset":{"type":"string","description":"Uploaded MP4 under img/<brand>/; server measures orientation."},
            "tiktok_ai_generated":{"type":"boolean","description":"Required explicit disclosure for TikTok."}
        }
    });
    for (path, method, id, summary, parameters, body, success) in [
        (
            "/app/api/blog",
            "post",
            "publishBrandPost",
            "Queue a Brand Post through the durable social publisher",
            vec![],
            Some(publish),
            "202",
        ),
        (
            "/app/api/blog/record",
            "post",
            "recordBrandPost",
            "Record a published website Post without provider delivery",
            vec![],
            Some(record_post_schema()),
            "201",
        ),
        (
            "/app/api/blog/{brand}/{slug}",
            "get",
            "getBrandPostStatus",
            "Read a Post and social publication receipts",
            vec!["brand", "slug"],
            None,
            "200",
        ),
        (
            "/app/api/blog/{brand}/handles/{provider}",
            "put",
            "setBrandSocialHandle",
            "Record a Brand social handle",
            vec!["brand", "provider"],
            Some(
                json!({"type":"object","additionalProperties":false,"required":["handle"],"properties":{"handle":{"type":"string"}}}),
            ),
            "200",
        ),
        (
            "/app/api/blog/{brand}/{slug}/links/{provider}",
            "put",
            "linkBrandSocialPost",
            "Record an existing public provider post without sending",
            vec!["brand", "slug", "provider"],
            Some(
                json!({"type":"object","additionalProperties":false,"required":["post_id","permalink"],"properties":{"post_id":{"type":"string"},"permalink":{"type":"string","format":"uri"}}}),
            ),
            "200",
        ),
    ] {
        let mut operation = json!({
            "operationId":id,"tags":["Blog"],"summary":summary,
            "description":"Requires Owner or the Brand Firm's Admin DRI. Publication reuses stored provider receipts; queued does not mean published.",
            "parameters":parameters.iter().map(|name| json!({"name":name,"in":"path","required":true,"schema":{"type":"string"}})).collect::<Vec<_>>(),
            "responses":{success:{"description":"Successful command or status read"},"401":{"description":"Unauthenticated"},"403":{"description":"Administrator tier required"},"404":{"description":"Brand or Post outside actor scope"},"409":{"description":"Post already exists or is public"},"422":{"description":"Invalid content, media, or social connection"},"503":{"description":"Publisher unavailable"}}
        });
        if let Some(schema) = body {
            operation["requestBody"] =
                json!({"required":true,"content":{"application/json":{"schema":schema}}});
        }
        document["paths"][path][method] = operation;
    }
    document["paths"]["/app/api/blog/schedule"]["post"] = json!({
        "operationId":"scheduleBrandPostForm", "tags":["Blog"],
        "summary":"Approve and schedule a Post from the native website form",
        "description":"Uses the same Brand authority, destination checks and durable publisher as the JSON command. Cookie callers must include _csrf. Invalid submissions retain authored copy in an editable HTML response.",
        "requestBody":{"required":true,"content":{"application/x-www-form-urlencoded":{"schema":{
            "type":"object", "required":["brand","slug","title","description","body_markdown","publish_at","video_asset","tiktok_ai_generated"],
            "properties":{
                "brand":{"type":"string"}, "slug":{"type":"string"}, "title":{"type":"string"},
                "description":{"type":"string"}, "body_markdown":{"type":"string"},
                "publish_at":{"type":"string","description":"Future RFC3339 time including UTC offset, or empty for now."},
                "video_asset":{"type":"string"}, "tiktok_ai_generated":{"type":"string","enum":["","true","false"]},
                "_csrf":{"type":"string"},
                "facebook":{"type":"string"}, "instagram":{"type":"string"}, "threads":{"type":"string"},
                "youtube":{"type":"string"}, "tiktok":{"type":"string"}, "linkedin":{"type":"string"}, "x":{"type":"string"}
            }
        }}}},
        "responses":{"303":{"description":"Queued; redirect to publishing calendar"},"401":{"description":"Unauthenticated"},"403":{"description":"Administrator tier or CSRF denied"},"404":{"description":"Brand outside actor scope"},"422":{"description":"Review retained form and validation error"}}
    });
}

/// The canonical `/templates` path of each documented operation that has a
/// `/drafts` twin, and the twin's path. The twin is served by the very same
/// handler and policy rule as the canonical operation (Draft is Template),
/// so it is documented, but it carries no MCP tool of its own: the canonical
/// operation already names the tool, and one tool does not get two names.
const DRAFT_ALIASES: [(&str, &str, &str, &str); 1] = [(
    "/app/api/templates/validate",
    "/app/api/drafts/validate",
    "validateTemplate",
    "validateDraft",
)];

/// Document each [`DRAFT_ALIASES`] twin: a copy of the canonical path item
/// with its own `operationId`, a summary that names the alias, and no
/// `x-mcp-tool`.
fn add_draft_aliases(document: &mut Value) {
    for (canonical, twin, canonical_id, twin_id) in DRAFT_ALIASES {
        let Some(item) = document["paths"].get(canonical).cloned() else {
            continue;
        };
        let mut item = item;
        if let Some(operations) = item.as_object_mut() {
            for operation in operations.values_mut() {
                let Some(operation) = operation.as_object_mut() else {
                    continue;
                };
                operation.remove("x-mcp-tool");
                let summary = operation
                    .get("summary")
                    .and_then(Value::as_str)
                    .map(|summary| {
                        format!(
                            "{} (Draft is Template: same as `{canonical}`)",
                            summary.replace("Template", "Draft")
                        )
                    });
                if let Some(summary) = summary {
                    operation.insert("summary".into(), Value::String(summary));
                }
                operation.insert("operationId".into(), Value::String(twin_id.to_string()));
            }
        }
        if let Some(operations) = document["paths"][canonical].as_object_mut() {
            for operation in operations.values_mut() {
                if let Some(operation) = operation.as_object_mut() {
                    operation.insert(
                        "operationId".into(),
                        Value::String(canonical_id.to_string()),
                    );
                }
            }
        }
        document["paths"][twin] = item;
    }
}

#[allow(clippy::too_many_lines)]
fn canonical_document(base: &str) -> Value {
    let contact_url = base.to_string();
    json!({
      "openapi": "3.1.0",
      "info": {
        "title": "Neon Law Navigator API",
        "version": "0.1.0",
        "description":
          "JSON listings and commands for the Neon Law Navigator domain tables, plus a stateless \
           markdown notation validator. The surface itself lives under `/app/api`, and every \
           endpoint requires OIDC authentication — either a browser session cookie issued by \
           the OAuth flow at `/auth/login`, or a JWT bearer token. This document and the \
           Swagger UI that renders it (at `/app/api` and its shorter alias `/api`) are public: \
           reading the reference needs no session, only calling an operation it describes does. \
           The MCP endpoint at `/mcp` (JSON-RPC, Google OAuth bearer) is documented separately.",
        "contact": { "name": FIRM_BRAND.site_name, "url": contact_url }
      },
      "servers": [
        { "url": base, "description": "Production" }
      ],
      "security": [
        { "bearerAuth": [] },
        { "sessionCookie": [] }
      ],
      "paths": {
        "/app/api/people": {
          "get": {
            "summary": "List all people",
            "x-mcp-tool": "show_person",
            "responses": {
              "200": { "description": "Person list", "content": { "application/json": {
                "schema": { "type": "array", "items": { "$ref": "#/components/schemas/Person" } }
              } } }
            }
          },
          "post": {
            "summary": "Create a person",
            "x-mcp-tool": "create_person",
            "description":
              "Creates one Person row. The `role` field defaults conservatively to `client`; \
               supported values are `owner`, `admin`, `lawyer`, `clerk`, and `client`, and non-empty invalid \
               values are rejected. Blank structured legal-name fields are stored as null. \
               Authorization: the caller's `persons.role` must be `lawyer` or `admin`; \
               anonymous, `client`, and non-lawyer `clerk` callers are rejected.",
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/CreatePersonRequest" }
              } }
            },
            "responses": {
              "201": { "description": "Created person", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/Person" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "400": { "description": "Validation failed", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "409": { "description": "Email already exists", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/people/{id}": {
          "get": {
            "summary": "Get one person by id",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "Person", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/Person" }
              } } },
              "404": { "description": "Not found" }
            }
          },
          "patch": {
            "summary": "Update a person",
            "description":
              "Updates one Person row. A blank/absent `role` preserves the row's current role; \
               a submitted `role` is honored only for `admin` callers (a `lawyer` caller's role \
               change is ignored) and the bootstrap Owner is pinned to `owner`. An Admin cannot \
               edit or assign Owner. Structured \
               legal-name fields preserve an omitted value and store a present-but-blank one as \
               null. Authorization: the caller's `persons.role` must be `lawyer` or `admin`; \
               anonymous, `client`, and non-lawyer `clerk` callers are rejected.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/UpdatePersonRequest" }
              } }
            },
            "responses": {
              "200": { "description": "Updated person", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/Person" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "400": { "description": "Validation failed", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No person with that id", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "409": { "description": "Email already exists", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          },
          "delete": {
            "summary": "Delete a person",
            "description":
              "Deletes one Person row. The configured bootstrap Owner (see \
               `NAVIGATOR_BOOTSTRAP_OWNER_EMAIL`) is undeletable and returns 409. \
               Authorization: the caller's `persons.role` must be `lawyer` or `admin`; \
               anonymous and `client` callers are rejected.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "Deleted person", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/Person" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No person with that id", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "409": { "description": "The bootstrap Owner cannot be deleted", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/people/merge": {
          "post": {
            "summary": "Merge a duplicate Person into the Person to keep",
            "description":
              "Moves every relationship the dropped Person holds (Conflict-Check Graph edges, \
               Project participation, Relationship Log rows, and the rest) to the kept Person, \
               keeps both email addresses on the kept Person as aliases, and deletes the \
               duplicate last. Both Persons are named by an email they answer to. Only a client \
               Person can be dropped. Authorization: the caller's `persons.role` must be \
               `lawyer` or `admin`; scoped sessions are refused.",
            "requestBody": { "required": true, "content": { "application/json": {
              "schema": { "type": "object", "required": ["keep", "drop"], "properties": {
                "keep": { "type": "string", "description": "Email of the Person to keep" },
                "drop": { "type": "string", "description": "Email of the duplicate to remove" }
              } }
            } } },
            "responses": {
              "200": { "description": "Merged", "content": { "application/json": {
                "schema": { "type": "object", "properties": {
                  "kept_id": { "type": "string", "format": "uuid" },
                  "dropped_id": { "type": "string", "format": "uuid" },
                  "emails": { "type": "array", "items": { "type": "string" } }
                } }
              } } },
              "400": { "description": "Keep and drop are the same Person", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin, or the session is scoped", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No Person answers to one of the emails", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "409": { "description": "The Person to drop is not a client", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/people/{id}/welcome": {
          "post": {
            "summary": "Send this person the welcome email",
            "x-mcp-tool": "send_welcome_email",
            "description":
              "Renders and dispatches the welcome email to the Person, journaling one \
               `sent_emails` row per attempt. Authorization: the caller's `persons.role` must be \
               `lawyer` or `admin`; anonymous and `client` callers are rejected.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "Welcome email dispatched", "content": { "application/json": {
                "schema": { "type": "object", "properties": { "status": { "type": "string" } } }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No person with that id", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "502": { "description": "The email could not be dispatched", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/brands/{key}": {
          "patch": {
            "summary": "Update a brand's typeface and palette",
            "description":
              "Sets the brand row's typeface and palette. `typeface` is `\"uploaded\"` or one \
               of the closed catalog ids in `views::brand`; free CSS is refused. The path \
               parameter is the brand key string, not a UUID. Every brand is Firm-scoped: \
               Owner may edit any Firm's brand, and that Firm's own Admin DRI may edit its \
               own. Lawyer and Clerk are denied.",
            "parameters": [
              { "name": "key", "in": "path", "required": true,
                "schema": { "type": "string" } }
            ],
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/UpdateBrandPresentationRequest" }
              } }
            },
            "responses": {
              "200": { "description": "Updated brand", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/Brand" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Caller may not edit this brand", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "400": { "description": "Typeface or palette is not on the closed list", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No brand with that key", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/entities": {
          "get": {
            "summary": "List all entities",
            "x-mcp-tool": "list_entities",
            "responses": {
              "200": { "description": "Entity list", "content": { "application/json": {
                "schema": { "type": "array", "items": { "$ref": "#/components/schemas/Entity" } }
              } } }
            }
          },
          "post": {
            "summary": "Create an entity",
            "description":
              "Creates one Entity row from a name, an entity type, and a jurisdiction. \
               The name is required and trimmed; the type and jurisdiction must reference \
               existing rows. Entity names are deliberately not unique — namesakes are real — \
               with one exception: the firm's own anchor Entity (`NAVIGATOR_BOOTSTRAP_COMPANY`, \
               falling back to the shipped firm) cannot be duplicated and returns 409. \
               Authorization: the caller's `persons.role` must be `lawyer` or `admin`; \
               anonymous, `client`, and non-lawyer `clerk` callers are rejected.",
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/CreateEntityRequest" }
              } }
            },
            "responses": {
              "201": { "description": "Created entity", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/Entity" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "400": { "description": "Validation failed: a blank name, or an entity type or jurisdiction that references no existing row", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "409": { "description": "The firm anchor entity already exists", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/firms": {
          "get": {
            "summary": "List Firms the caller may administer",
            "description":
              "Owner sees every Firm. Admin sees only Firms where their membership admits `manage_membership`. \
               This directory lets the CLI resolve an operator-supplied Firm name without accepting a raw id on its command line.",
            "responses": {
              "200": { "description": "Visible Firm ids and names", "content": { "application/json": {
                "schema": { "type": "array", "items": { "type": "object", "required": ["id", "name"], "properties": {
                  "id": { "type": "string", "format": "uuid" },
                  "name": { "type": "string" }
                } } }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Owner or Admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/seed": {
          "post": {
            "summary": "Reconcile a seed document",
            "description": "Applies one YAML document in Navigator's standard `lookup_fields` / `records` seed format. The model is a supported singular glossary term. Existing lookup matches are unchanged by default; `overwrite` replaces the fields represented in the seed record. `dry_run` performs the same lookups and reference checks, returns a per-record plan, and writes nothing. Authorization: Lawyer tier only. A session minted with a project scope (a CI-obtained token) is further restricted to its own endpoint, models, and project — a pull-request CI session is additionally dry-run-only and rejects `overwrite`; any request outside that scope is refused as `403 scope_violation`, distinguishable from an ordinary role failure. An unscoped session may ask for the same rules with `as_ci` and `project` on a dry run, so an operator's plan says whether CI could perform the write.",
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/SeedRequest" }
              } }
            },
            "responses": {
              "200": { "description": "Reconciliation summary", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/SeedReport" }
              } } },
              "401": { "description": "No authenticated session" },
              "403": { "description": "Authenticated caller is not Lawyer/admin, or a project-scoped session wrote outside its endpoint, model, or project scope (`error: scope_violation`)" },
              "422": { "description": "Unsupported model or invalid seed document" }
            }
          }
        },
        "/app/api/authorities": {
          "post": {
            "summary": "Create (or find) the global Authority for a citation, archiving its artifact",
            "description": "Global legal reference data (#890): a case, statute, regulation, administrative proceeding, or secondary source, shared across every matter rather than scoped to one. `archive_base64` is ingested as a bare content asset (content-addressed, deduped by SHA-256) and the resulting asset id is recorded as `archived_asset_id`. Find-or-create on `citation`: repeating an already-recorded citation returns the existing row untouched, including its original archive — the second call's bytes are still ingested as an asset (so the request is never rejected for carrying them) but never replace the first row's archive or metadata. Authorization: Lawyer tier only, the same gate as every other `/app/api` authoring door.",
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": {
                  "type": "object",
                  "required": ["class", "citation", "title", "archive_base64"],
                  "properties": {
                    "class": { "type": "string", "enum": ["case_law", "statute", "regulation", "administrative", "secondary"] },
                    "citation": { "type": "string" },
                    "title": { "type": "string" },
                    "short_cite": { "type": "string" },
                    "publisher": { "type": "string" },
                    "issued_on": { "type": "string" },
                    "canonical_url": { "type": "string" },
                    "checked_on": { "type": "string" },
                    "archive_base64": { "type": "string", "format": "byte", "description": "The archived artifact's bytes, base64-encoded." },
                    "content_type": { "type": "string", "description": "Defaults to application/octet-stream when absent or blank." }
                  }
                }
              } }
            },
            "responses": {
              "200": { "description": "The recorded (or already-existing) Authority", "content": { "application/json": {
                "schema": {
                  "type": "object",
                  "properties": {
                    "id": { "type": "string", "format": "uuid" },
                    "class": { "type": "string" },
                    "citation": { "type": "string" },
                    "short_cite": { "type": "string", "nullable": true },
                    "title": { "type": "string" },
                    "publisher": { "type": "string", "nullable": true },
                    "issued_on": { "type": "string", "nullable": true },
                    "canonical_url": { "type": "string", "nullable": true },
                    "checked_on": { "type": "string", "nullable": true },
                    "archived_asset_id": { "type": "string", "format": "uuid", "nullable": true },
                    "inserted_at": { "type": "string", "format": "date-time" },
                    "updated_at": { "type": "string", "format": "date-time" }
                  }
                }
              } } },
              "400": { "description": "class is outside the closed vocabulary (`invalid_class`), or archive_base64 is missing, not valid base64, or decodes to zero bytes (`archive_unreadable`)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          },
          "patch": {
            "summary": "Correct a field on an existing Authority (LAW-61)",
            "description": "Exactly one of `id` or `citation` locates the Authority to update. Every other field is optional and left unchanged when absent. `citation` and `class` are immutable — they are the Authority's identity (see `POST /app/api/authorities`); changing either means a new Authority, not an update of this one. When `archive_base64` is given it is ingested as a new content asset and replaces `archived_asset_id`; the previous archive is left in place. Authorization: Lawyer tier only, the same gate as create.",
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": {
                  "type": "object",
                  "properties": {
                    "id": { "type": "string", "format": "uuid" },
                    "citation": { "type": "string", "description": "Looks up the existing Authority. Give this or `id`, not both." },
                    "title": { "type": "string" },
                    "short_cite": { "type": "string" },
                    "publisher": { "type": "string" },
                    "issued_on": { "type": "string" },
                    "canonical_url": { "type": "string" },
                    "checked_on": { "type": "string" },
                    "archive_base64": { "type": "string", "format": "byte", "description": "A new artifact version's bytes, base64-encoded." },
                    "content_type": { "type": "string", "description": "Defaults to application/octet-stream when absent or blank." }
                  }
                }
              } }
            },
            "responses": {
              "200": { "description": "The updated Authority", "content": { "application/json": {
                "schema": {
                  "type": "object",
                  "properties": {
                    "id": { "type": "string", "format": "uuid" },
                    "class": { "type": "string" },
                    "citation": { "type": "string" },
                    "short_cite": { "type": "string", "nullable": true },
                    "title": { "type": "string" },
                    "publisher": { "type": "string", "nullable": true },
                    "issued_on": { "type": "string", "nullable": true },
                    "canonical_url": { "type": "string", "nullable": true },
                    "checked_on": { "type": "string", "nullable": true },
                    "archived_asset_id": { "type": "string", "format": "uuid", "nullable": true },
                    "inserted_at": { "type": "string", "format": "date-time" },
                    "updated_at": { "type": "string", "format": "date-time" }
                  }
                }
              } } },
              "400": { "description": "Neither id nor citation was given (`identifier_required`), both were (`ambiguous_identifier`), or archive_base64 is not valid base64 or decodes to zero bytes (`archive_unreadable`)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No authority matches the given id or citation (`not_found`)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/authorities/{id}": {
          "get": {
            "operationId": "readFiledAuthority",
            "summary": "Read a filed Authority's canonical citation",
            "description": "Resolves the Authority id and canonical citation only while its archived artifact remains available in storage. This is global reference data with no Project-scoped usage or disposition. Authorization: Lawyer tier only (Owner, Admin, or Lawyer); Clerk and Client are refused. Inherits the API's bearer-token or session-cookie authentication.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The filed Authority's id and canonical citation", "content": { "application/json": {
                "schema": {
                  "type": "object",
                  "required": ["id", "citation"],
                  "properties": {
                    "id": { "type": "string", "format": "uuid" },
                    "citation": { "type": "string" }
                  }
                }
              } } },
              "400": { "description": "The path id is not a valid UUID" },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is below Lawyer tier", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "The Authority, archive reference, asset record, or stored bytes are missing", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The Authority or its archived artifact could not be read", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/authorities/{id}/archive": {
          "get": {
            "summary": "Read an Authority's archived artifact (LAW-93)",
            "description": "The bytes `POST /app/api/authorities` archived for the Authority, served as an attachment with the content type recorded at upload. `navigator project sync` hydrates `documents/cases/**` and `documents/rules/**` from this door, verifying each against the pointer's committed SHA-256. Authorization: Lawyer tier only, the same gate as the write.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The archived bytes", "content": { "application/octet-stream": { "schema": { "type": "string", "format": "binary" } } } },
              "401": { "description": "No authenticated session" },
              "403": { "description": "Authenticated caller is not Lawyer/admin" },
              "404": { "description": "No Authority with that id, or it carries no archive" }
            }
          }
        },
        "/app/api/assets": {
          "post": {
            "summary": "Publish one public-safe asset to the deployment's public assets bucket",
            "description": "The authenticated sibling of the ADC-backed `navigator ops assets upload` batch command (ENG-909): a single brand mark, hero image, or web font goes out with nothing but a `navigator site login` bearer, no bucket name or GCP credential. `key` is the exact bucket key, reachable immediately afterwards at `GET /assets/{key}`, and must fall below `brand/`, `img/`, or `fonts/` — the three bucket-served lanes `docs/assets.md` documents. No `project_id` — like `/app/api/authorities`, a public asset is deployment-wide, not matter-scoped. The handler writes only to the public assets bucket; it never touches the private documents bucket. `content_type` must match what `key`'s extension requires, and `sha256` must match the decoded bytes — both checked before a single byte reaches storage, catching transit corruption a post-write read-back alone could not. Writing the same key with the same bytes and content type again is a no-op (`unchanged: true`, `200`); a new or changed key writes through `cloud::StorageService` and answers `201`. Either way the handler then reads the object back from the same storage the public route serves, so the returned `sha256` and `bytes` certify what a browser will actually receive. Authorization: Owner/Admin only, the same gate `PATCH /app/api/brands/{key}` uses — a public asset is site-branding configuration, not matter-scoped lawyer work.",
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": {
                  "type": "object",
                  "required": ["key", "content_base64", "content_type", "sha256"],
                  "properties": {
                    "key": { "type": "string", "description": "The bucket key, e.g. `img/vesta-home/logo.svg` or `brand/rabbit.svg`. Must fall below brand/, img/, or fonts/, with no leading `/` and no `.` or `..` path segment." },
                    "content_base64": { "type": "string", "format": "byte", "description": "The asset's bytes, base64-encoded." },
                    "content_type": { "type": "string", "description": "Must equal the type key's extension requires (an image type, video/mp4, a web-font format, or text/plain for a fonts/*/OFL.txt license notice)." },
                    "sha256": { "type": "string", "description": "Lowercase hex SHA-256 of the decoded bytes; checked against the server's own digest." }
                  }
                }
              } }
            },
            "responses": {
              "200": { "description": "The key already held these exact bytes and content type; the write was skipped (`unchanged: true`)", "content": { "application/json": {
                "schema": {
                  "type": "object",
                  "properties": {
                    "key": { "type": "string" },
                    "bytes": { "type": "integer" },
                    "content_type": { "type": "string" },
                    "sha256": { "type": "string" },
                    "unchanged": { "type": "boolean" }
                  }
                }
              } } },
              "201": { "description": "The asset was written (or overwritten) and read back", "content": { "application/json": {
                "schema": {
                  "type": "object",
                  "properties": {
                    "key": { "type": "string" },
                    "bytes": { "type": "integer" },
                    "content_type": { "type": "string" },
                    "sha256": { "type": "string" },
                    "unchanged": { "type": "boolean" }
                  }
                }
              } } },
              "400": { "description": "key is outside brand/img/fonts, absolute, or traverses a segment (`invalid_key`), key's extension is not published by this door (`unsupported_content_type`), content_type does not match what the extension requires (`content_type_mismatch`), content_base64 is missing, not valid base64, or decodes to zero bytes (`content_unreadable`), the asset exceeds the size limit (`asset_too_large`), a font/woff2 payload is missing its signature (`invalid_font`), a fonts/*/OFL.txt payload is not valid UTF-8 or exceeds 64 KiB (`invalid_license`), or sha256 does not match the decoded bytes (`sha256_mismatch`)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Owner/Admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/entities/{id}": {
          "get": {
            "summary": "Get one entity by id",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "Entity", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/Entity" }
              } } },
              "404": { "description": "Not found" }
            }
          },
          "patch": {
            "summary": "Update an entity",
            "description":
              "Partial update for one Entity row: every field is optional, and an absent one \
               leaves its column unchanged — a caller correcting only `jurisdiction_id` sends only \
               that field. A present `name` must not be blank, and `entity_type_id`/`jurisdiction_id` \
               must reference existing rows. The firm's own anchor Entity \
               (`NAVIGATOR_BOOTSTRAP_COMPANY`, falling back to the shipped firm) has \
               an immutable name — its type and jurisdiction remain editable — and renaming any \
               other Entity *into* the anchor's name is refused; both return 409. The name is \
               compared byte for byte, so a case or whitespace variant of the anchor's name counts \
               as a rename. Authorization: the caller's `persons.role` must be `lawyer` or \
               `admin`; anonymous, `client`, and non-lawyer `clerk` callers are rejected.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/UpdateEntityRequest" }
              } }
            },
            "responses": {
              "200": { "description": "Updated entity", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/Entity" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "400": { "description": "Validation failed — a blank name, or an entity type or jurisdiction naming no existing row", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No entity with that id", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "409": { "description": "The firm anchor's name is immutable, or the rename would duplicate it", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          },
          "delete": {
            "summary": "Delete an entity",
            "description":
              "Removes one Entity row and returns it. The firm's own anchor Entity \
               (`NAVIGATOR_BOOTSTRAP_COMPANY`, falling back to the shipped firm) is undeletable and \
               returns 409 — `store::seed` re-creates that row by exact name on every boot, so \
               removing it would never stick. An Entity other rows still reference (a matter, say) \
               is also refused with 409, carrying the database's own detail naming the referencing \
               table. Authorization: the caller's `persons.role` must be `lawyer` or `admin`; \
               anonymous, `client`, and non-lawyer `clerk` callers are rejected.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The deleted entity", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/Entity" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No entity with that id", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "409": { "description": "The firm anchor is protected, or other rows still reference this entity", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/jurisdictions": {
          "get": {
            "summary": "List all jurisdictions",
            "x-mcp-tool": "list_jurisdictions",
            "responses": {
              "200": { "description": "Jurisdiction list", "content": { "application/json": {
                "schema": { "type": "array",
                            "items": { "$ref": "#/components/schemas/Jurisdiction" } }
              } } }
            }
          }
        },
        "/app/api/entity-types": {
          "get": {
            "summary": "List all entity types",
            "responses": {
              "200": { "description": "EntityType list", "content": { "application/json": {
                "schema": { "type": "array",
                            "items": { "$ref": "#/components/schemas/EntityType" } }
              } } }
            }
          }
        },
        "/app/api/integrations/notion/ensure": {
          "post": {
            "summary": "Create or adopt each Project's Firm-private Notion page (admin)",
            "description":
              "Find-then-create against the Firm's own Notion credential, resolved from the Project's `firm_id` — never a deployment-wide token. A failed lookup does not fall through to a create, so a provider outage cannot produce a duplicate page. The page address is recorded on the Project row before the outcome is reported. Outcomes: `created`, `adopted`, `runtime_not_configured`, `credential_missing`, `credential_unusable`, `provider_unavailable`, `address_not_recorded`. The response carries outcomes only — the page id and URL are Firm-private coordinates and do not cross this door. Authorization: admin-tier, matching `POST /app/api/project-surfaces/{id}`, plus `use_integrations` on the Project's owning Firm — being shown a Project is not permission to spend its Firm's credentials. `all` sweeps only the visible Projects that also clear that check.",
            "requestBody": { "required": true, "content": { "application/json": {
              "schema": { "$ref": "#/components/schemas/ProjectSelector" }
            } } },
            "responses": {
              "200": { "description": "One outcome per selected Project", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/IntegrationReport" } } } },
              "400": { "description": "The selector or the event kind is not accepted", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not admin-tier", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No Project with that code is visible to the caller, or the caller may not use its Firm's integrations — the two answer identically on purpose", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/integrations/calendar/ensure": {
          "post": {
            "summary": "Create or adopt each Project's shared Google Calendar (admin)",
            "description":
              "Creates the Project's calendar on the deployment's Workspace, named for the Project code, or adopts an unrecorded one with that name, and records its id on the Project before sharing it as a writer with every firm-side participant — never a client. A recorded calendar is checked by id and never replaced. A closed or archived Project's calendar is made read-only instead and its booking link cleared. Outcomes: `created`, `adopted`, `present`, `read_only`, `recorded_resource_missing`, `duplicate`, `runtime_not_configured`, `credential_unusable`, `provider_unavailable`, `address_not_recorded`. The response carries outcomes only. Authorization: admin-tier, plus `use_integrations` on the Project's owning Firm.",
            "requestBody": { "required": true, "content": { "application/json": {
              "schema": { "$ref": "#/components/schemas/ProjectSelector" }
            } } },
            "responses": {
              "200": { "description": "One outcome per selected Project", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/IntegrationReport" } } } },
              "400": { "description": "The selector is not accepted", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not admin-tier", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No Project with that code is visible to the caller, or the caller may not use its Firm's integrations — the two answer identically on purpose", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/integrations/notion/reconcile": {
          "post": {
            "summary": "Reconcile each Project against the selected Notion database (admin)",
            "description":
              "Compares the Project's recorded page address against every page in the environment-selected `NAVIGATOR_NOTION_DATABASE_ID` carrying its code, and reports the repair decision. A missing, moved, duplicated, conflicting, or unshared page is an operator-visible outcome rather than a silent second page: outcomes are `unchanged`, `repaired`, `missing`, `duplicate`, `conflict`, `unavailable`, `not_recorded`, plus the resolution outcomes `ensure` reports. Only a `Repair` writes, and it preserves manual Notion fields. Authorization: admin-tier, plus `use_integrations` on the Project's owning Firm.",
            "requestBody": { "required": true, "content": { "application/json": {
              "schema": { "$ref": "#/components/schemas/ProjectSelector" }
            } } },
            "responses": {
              "200": { "description": "One outcome per selected Project", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/IntegrationReport" } } } },
              "400": { "description": "The selector or the event kind is not accepted", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not admin-tier", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No Project with that code is visible to the caller, or the caller may not use its Firm's integrations — the two answer identically on purpose", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/integrations/slack/ensure": {
          "post": {
            "summary": "Create or adopt one Project's Firm-private Slack channel (admin)",
            "description":
              "Find-then-create a private channel named for the Project code, using the Firm's own bot token resolved from the Project's `firm_id`. The invite list is empty by construction: the adapter accepts only provider-issued member ids, Navigator stores none, and it will not turn a participation row or an email address into an invite — so the Firm's own Slack membership governs who joins. The channel id is recorded on the Project row. Outcomes: `created`, `adopted`, and the resolution outcomes. Authorization: admin-tier, plus `use_integrations` on the Project's owning Firm.",
            "requestBody": { "required": true, "content": { "application/json": {
              "schema": { "$ref": "#/components/schemas/SlackIntegrationRequest" }
            } } },
            "responses": {
              "200": { "description": "One outcome per selected Project", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/IntegrationReport" } } } },
              "400": { "description": "The selector or the event kind is not accepted", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not admin-tier", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No Project with that code is visible to the caller, or the caller may not use its Firm's integrations — the two answer identically on purpose", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/integrations/slack/notify": {
          "post": {
            "summary": "Post one mechanism-only notice to a Project's Firm-private channel (admin)",
            "description":
              "`event` must be one of `project_opened`, `project_closed`, `project_reconciled`, or `integration_unavailable`; anything else is a 400 before any provider call. The vocabulary is closed precisely so this door cannot carry arbitrary text — a client name, a document title, a provider URL — into a channel; the posted message is derived from the event kind alone. Notify never provisions: a Project with no channel reports `no_channel` so an operator runs `ensure` deliberately. Outcomes: `notified`, `no_channel`, and the resolution outcomes. Authorization: admin-tier, plus `use_integrations` on the Project's owning Firm.",
            "requestBody": { "required": true, "content": { "application/json": {
              "schema": { "$ref": "#/components/schemas/SlackIntegrationRequest" }
            } } },
            "responses": {
              "200": { "description": "One outcome per selected Project", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/IntegrationReport" } } } },
              "400": { "description": "The selector or the event kind is not accepted", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not admin-tier", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No Project with that code is visible to the caller, or the caller may not use its Firm's integrations — the two answer identically on purpose", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/project-repositories": {
          "get": {
            "summary": "Reconcile every matter against the repository it records (admin)",
            "description":
              "Reports where a Project row and its `repository_url` disagree, across every matter \
               in the deployment. A Project code is its repository name, so a row whose recorded \
               URL names a different repository is drift provable from the row alone — no checkout \
               and no configuration required. Findings carry a `severity` of `warn` or `fail`; the \
               report's `reconciled` is false when any finding is a `fail`. Where the deployment \
               has a configured forge pair, a row recorded outside it is reported as a `warn` and \
               `compared_against_deployment_forge` is true; where it has none that comparison is \
               skipped and the flag is false. `project_codes` lists every live Project code, sorted, \
               including the ones with no finding — the repository-side reconciliation (`navigator \
               projects drift`) reads it to ask whether a checkout's declared code names a live row \
               at all, which the finding set alone cannot answer. Authorization: admin-tier only \
               (`owner`/`admin`) — \
               unlike every other matter read here, this deliberately reads all rows rather than \
               the caller's participation-scoped lens, so `lawyer`, `clerk`, and `client` are \
               rejected.",
            "responses": {
              "200": { "description": "The reconciliation report", "content": { "application/json": { "schema": { "type": "object" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not admin-tier", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The matters could not be read", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/project-lifecycle": {
          "get": {
            "summary": "Read every matter's lifecycle fields (admin)",
              "description":
              "Returns one minimal row per matter with its `code`, `status`, `closed_at`, `closure_reason`, and \
               derived `source_state`. This is a deployment-wide oversight read, not a \
               participation-scoped matter read, and returns no matter content — no repository \
               URL, no Drive folder id. Authorization: admin-tier only (`owner`/`admin`).",
            "responses": {
              "200": { "description": "The lifecycle rows", "content": { "application/json": {
                "schema": { "type": "array", "items": { "type": "object", "required": ["code", "status", "brand", "firm_id", "closed_at", "closure_reason", "source_state"], "properties": {
                  "code": { "type": "string" },
                  "status": { "type": "string", "enum": ["open", "closed", "archived"] },
                  "brand": { "type": "string", "description": "The house brand the Project belongs to." },
                  "firm_id": { "type": ["string", "null"], "format": "uuid", "description": "The owning Firm, or null when the Project has not been assigned." },
                  "closed_at": { "type": ["string", "null"], "format": "date-time" },
                  "closure_reason": { "type": ["string", "null"], "enum": ["pitch_declined", "pitch_lapsed", "pitch_withdrawn", "pitch_superseded", "engagement_completed", "client_terminated", "firm_withdrew", null] },
                  "source_state": { "type": "string",
                    "enum": ["not_enabled", "pending", "unknown", "attached", "initialized", "failed"],
                    "description": "Derived from repository_url/forge_provisioned_at/git_initialized_at — never a stored column. See docs/glossary/project.md." }
                } } }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not admin-tier", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The matters could not be read", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/project-surfaces/{id}": {
          "get": {
            "summary": "Read nonsecret Project coordinates for manifest fill and gate",
            "description":
              "Read-only. Returns the Project code, repository URL, DRI email sets, and stored private Slack/Notion URLs. Owner/Admin may read a Project; a CI document session may read only its bound Project. Other sessions are refused so Firm-private coordinates do not become a directory surface.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The Project's nonsecret coordinates", "content": { "application/json": { "schema": { "type": "object" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No matter with that id or the session is not scoped to it", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "500": { "description": "The Project coordinates could not be read", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          },
          "post": {
            "summary": "Create or adopt a Project's Drive ingest folder and source repository (admin)",
            "description":
              "Creates or adopts the three handles a Project opens with: the documents-bucket \
               prefix `projects/<code>/documents` (a key convention; nothing is written), the Drive ingest \
               folder named for the code, and one private source repository named for the code. \
               Each step is idempotent — an existing folder or repository is adopted, a recorded \
               `repository_url` is left alone, and an entirely absent provider configuration \
               skips that surface with a reason in the response. Partial or invalid provider \
               configuration fails the request with its initialization error. \
               Matter-open runs this pass best-effort; this door is the \
               admin retry for a failed or legacy Project. Project participation is never copied \
               onto the forge. Authorization: admin-tier only (`owner`/`admin`).",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The recorded handles", "content": { "application/json": { "schema": { "type": "object" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not admin-tier", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No matter with that id", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "Drive, forge, or the store failed", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/projects": {
          "get": {
            "summary": "List the caller's matters",
            "x-mcp-tool": "list_projects",
            "description":
              "Every matter the caller may see, already scoped — the directory lens for \
               Owner/Admin, participation for lawyer/clerk, the client's own matters for a client. \
               Authorization: any authenticated session.",
            "responses": {
              "200": { "description": "The caller's visible matters", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "object" } } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "500": { "description": "The matters could not be read", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          },
          "post": {
            "summary": "Open a matter",
            "x-mcp-tool": "create_project",
            "description":
              "Opens a new Project: it runs the conflict check, requires the opening attorney's \
               conflict attestation, and designates both DRIs. The Project's Drive ingest folder \
               and source repository are then created or adopted best-effort — a Drive or forge \
               fault leaves the Project open, and `POST /app/api/project-surfaces/{id}` retries. \
               The client of record must be a pre-existing \
               `client`-role person (never a firm attorney), and the entity must \
               already exist. `attestation` must be `true` — a Project open with no attestation is \
               refused (`attestation_required`). The attorney attests they have checked for \
               conflicts, and that either none prevent the open or the Project is not legal \
               advice. A **blocking** conflict (adverse to a current client) is a hard \
               `409 conflict_blocked` that no attestation overrides. The \
               attester is the authenticated session's person, recorded on the attestation audit \
               row and designated the accountable lawyer DRI — never taken from the request body. \
               Authorization: the caller's `persons.role` must be `lawyer` or `admin`; at this firm \
               `lawyer` is an attorney, so this is the 'an attorney is opening and attesting' gate. \
               Anonymous, `client`, and non-lawyer `clerk` callers are rejected. Optionally opens \
               the matter already closed: `status: \"closed\"` with a required `closed_at` records \
               an engagement that ended before anyone opened its row, in one call, through the \
               same transition validation `POST /app/api/projects/{id}/lifecycle` runs.",
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/OpenMatterRequest" }
              } }
            },
            "responses": {
              "201": { "description": "The opened matter", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/Project" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "400": { "description": "Validation failed, a bad client/entity reference, a non-client client of record, a non-lawyer attester, or a missing attestation", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "409": { "description": "A blocking conflict (adverse to a current client) or a duplicate project code", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/projects/{id}": {
          "get": {
            "summary": "Get one matter",
            "description":
              "One matter the caller may see. Authorization: any authenticated session; a matter \
               the caller does not participate in returns a non-disclosing 404 (Owner/Admin see \
               all).",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The matter", "content": { "application/json": { "schema": { "type": "object" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such matter, or out of scope", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          },
          "patch": {
            "summary": "Update a matter's descriptive fields or Firm/brand pair",
            "description":
              "Updates descriptive fields of an existing matter, or atomically changes its Firm and \
               brand as a matching pair. A Firm/brand update requires Owner or an Admin who can \
               administer the target Firm; ordinary descriptive updates require Lawyer-tier access. \
               This is the \
               descriptive update only: it runs no conflict check and provisions no repo (those \
               belong to matter-open and `POST /app/api/project-surfaces/{id}`), and it does not \
               change the matter's \
               lifecycle `status`/`closed_at` — moving through open/closed/archived is a transition \
               with firm retention semantics, handled by `POST /app/api/projects/{id}/lifecycle`, \
               not this edit. `status` and `closed_at` are rejected outright (`400`) rather than \
               silently dropped if a caller posts them — a caller must be told a field it named was \
               not applied. **Every field is optional and this is always a patch**: send only the \
               fields you want to change. An absent field — or an explicit `null` — leaves its \
               column exactly as it was, and an empty string clears it. Nothing is blanked out \
               because a body did not mention it. `name` is the one exception to clearing: an \
               empty `name` is rejected rather than applied, while omitting it leaves the name \
               alone. `firm_id` and `brand` must be supplied together and the Firm must wear that \
               brand. Anonymous, `client`, and non-lawyer `clerk` callers are rejected.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/UpdateProjectRequest" }
              } }
            },
            "responses": {
              "200": { "description": "The updated matter", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/Project" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "400": { "description": "Validation failed — a blank name or an entity that does not exist", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No matter with that id", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          },
          "delete": {
            "summary": "Delete a matter",
            "description":
              "Removes one matter and returns it. A matter opened the normal way carries dependent \
               rows (DRI participations, later notations and documents), \
               whose foreign keys block the delete — returned as 409 with the database's own detail \
               naming the referencing table, a conflict the caller resolves by detaching or closing \
               those records first. Only a matter with no dependents deletes cleanly. \
               Authorization: the caller's `persons.role` must be `lawyer` or `admin`; \
               anonymous, `client`, and non-lawyer `clerk` callers are rejected.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The deleted matter", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/Project" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No matter with that id", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "409": { "description": "Other records still reference this matter", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/projects/{id}/status": {
          "get": {
            "summary": "Read live matter lifecycle and onboarding evidence",
            "description": "Lawyer-tier, firm-side participation required for every tier. Returns the workbench lifecycle label, stored project_status, opened_at (Project creation), closed_at, source_state, onboarding presence and document visibility, and missing onboarding work. Presence includes generated onboarding instruments and never verifies execution.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "Live lifecycle evidence", "content": { "application/json": {
                "schema": { "type": "object" }
              } } },
              "401": { "description": "No authenticated session" },
              "403": { "description": "Lawyer tier required" },
              "404": { "description": "No matter in firm-side participation scope" }
            }
          }
        },
        "/app/api/projects/{id}/testimonial": {
          "post": {
            "summary": "Submit or edit the authenticated client's testimonial",
            "description":
              "Saves the current caller's testimonial for this matter. The caller's person id is \
               derived from the authenticated session; the body cannot select a person or matter. \
               Only the matter's client DRI may write. `request_public: true` records client consent, \
               but does not publish the testimonial. Every edit clears prior publication approval.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": {
                  "type": "object",
                  "required": ["quote", "request_public"],
                  "additionalProperties": false,
                  "properties": {
                    "quote": { "type": "string" },
                    "attribution": { "type": ["string", "null"] },
                    "request_public": { "type": "boolean" }
                  }
                }
              } }
            },
            "responses": {
              "200": { "description": "Saved testimonial", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/Testimonial" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No client-DRI testimonial in scope", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/testimonials/{id}/publish": {
          "post": {
            "summary": "Publish a consented testimonial",
            "description": "Lawyer/admin approval sets `published_at`; the testimonial must already carry client consent and the approver must participate on the project's firm side.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "Published testimonial", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/Testimonial" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No testimonial in scope", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "409": { "description": "Client consent is required", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/testimonials/{id}/unpublish": {
          "post": {
            "summary": "Unpublish a testimonial",
            "description": "Lawyer/admin approval removes `published_at` and therefore website visibility while preserving the client's consent record.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "Unpublished testimonial", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/Testimonial" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No testimonial in scope", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/projects/{id}/close": {
          "post": {
            "summary": "Open a matter's closing-letter notation",
            "description":
              "Opens the firm-signed `offboarding__letter` notation on an existing matter, addressed to \
               the matter's client — the REST mirror of the lawyer close control, converging on the \
               same command. `201 Created` carrying the new notation's id; the walk that follows \
               (and the eventual flip to `closed`) is driven separately. Authorization: the \
               caller's `persons.role` must be `lawyer` or `admin`, and the caller must \
               participate in the matter — an out-of-scope matter returns 404, never disclosing its \
               existence (`admin` bypasses the scope check).",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "201": { "description": "The closing notation was opened", "content": { "application/json": {
                "schema": {
                  "type": "object",
                  "required": ["notation_id"],
                  "properties": {
                    "notation_id": { "type": "string", "format": "uuid" }
                  }
                }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No such matter, or the caller does not participate in it", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "409": { "description": "The matter has no client to address the closing letter to", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The closing notation could not be opened", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/projects/{id}/lifecycle": {
          "post": {
            "summary": "Move a matter through its lifecycle",
            "x-mcp-tool": "close_project",
            "description":
              "Moves a matter directly through `open` → `closed` → `archived`, the REST door onto \
               `store::projects::transition_project`. Distinct from `POST /app/api/projects/{id}/close`: \
               that door opens the firm-signed closing-letter notation and leaves the status flip to \
               the walk that follows, while this one flips `status`/`closed_at` immediately and opens \
               no notation. A close or archive also makes the Project's shared calendar read-only and \
               clears its booking link, best-effort. `closed_at` is derived from `transition` and optional \
               `effective_at`, never posted directly. An effective time must be RFC 3339, not be in the future, and not \
               precede matter-open. Without one, an existing close stamp is preserved. Re-applying \
               the same transition and effective time is idempotent; supplying a different valid \
               effective time corrects the retention start. `archived` is terminal — every transition off it, other than a repeated \
               archive, is refused as `400`. Archiving is final closeout: it is refused as `409` with every blocker named \
               until the matter is closed, a committed recovery generation was cut after the close with no document \
               changed since, and every required handoff is acknowledged. Authorization: the caller's `persons.role` must be \
               `lawyer` or `admin`; anonymous, `client`, and non-lawyer `clerk` callers are rejected \
               — the same tier gate as the bare `PATCH`/`DELETE` matter path, not scoped to the \
               caller's own matters.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/TransitionProjectRequest" }
              } }
            },
            "responses": {
              "200": { "description": "The matter, in its new (or unchanged) lifecycle state", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/Project" }
              } } },
              "400": { "description": "Malformed or unknown fields, an invalid effective time, an unrecognized `transition`, or a transition refused because the matter is archived", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No matter with that id", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "409": { "description": "Archiving is not ready; the body names every blocker", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/projects/{id}/conversation": {
          "get": {
            "summary": "Read a matter's conversation",
            "description":
              "The matter conversation thread. A client sees the client-visible thread (internal \
               notes filtered out); the firm sees the full thread. Authorization: any authenticated \
               session; out-of-scope matter → 404.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The conversation", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "object" } } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such matter, or out of scope", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/conversation/messages": {
          "post": {
            "summary": "Post a message to a matter's conversation",
            "description":
              "Posts one message to a matter's conversation — the REST mirror of the portal message \
               control, converging on the same command. `204 No Content` on success. This is a \
               client-writable door: any authenticated caller reaches the policy layer, and the \
               command enforces either-lens matter access (a non-participant is a non-disclosing \
               404). The caller's tier decides the side: a client's message is inbound, a lawyer's \
               is outbound, or an internal note when `internal` is set (a client's `internal` flag \
               is ignored).",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": {
                  "type": "object",
                  "required": ["body"],
                  "properties": {
                    "body": { "type": "string", "description": "The message body" },
                    "internal": {
                      "type": "boolean",
                      "description": "Lawyer lens only: post as an internal note rather than a client-visible message"
                    }
                  }
                }
              } }
            },
            "responses": {
              "204": { "description": "The message was posted" },
              "400": { "description": "The message body is empty", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No such matter, or the caller does not participate in it", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The message could not be posted", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/projects/{id}/handoffs": {
          "post": {
            "summary": "Prepare a legal-files-only handoff",
            "description": "Records an immutable manifest of the current revision of every live legal document the recipient's own matter lens admits, after reading and verifying every byte from Navigator storage (the active copy, or the verified archived copy). Source and history, Notation internals, deleted chains, and recovery metadata never ship. A missing or damaged file fails the handoff. `required` marks a handoff whose acknowledgment closing the Project waits for. Authorization: lawyer tier and Project participation; otherwise 404.",
            "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }],
            "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["recipient_person_id"], "properties": { "recipient_person_id": { "type": "string", "format": "uuid" }, "required": { "type": "boolean" } } } } } },
            "responses": { "201": { "description": "The handoff and its manifest", "content": { "application/json": { "schema": { "type": "object" } } } }, "401": { "description": "Authenticated session required" }, "403": { "description": "Lawyer tier required" }, "404": { "description": "No such Project or the caller does not participate" }, "409": { "description": "A file is missing or damaged, or the Project has no Firm", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }, "413": { "description": "The handoff exceeds its file or byte ceiling" }, "422": { "description": "The recipient does not participate in the Project", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }, "503": { "description": "PostgreSQL is unavailable", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } } }
          }
        },
        "/app/api/projects/{id}/handoffs/{handoff_id}": {
          "get": {
            "summary": "Read a handoff's manifest",
            "description": "The immutable manifest: each file's entry name, revision id, size, and SHA-256. Authorization: a lawyer on the matter or the handoff's recipient; otherwise 404.",
            "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }, { "name": "handoff_id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }],
            "responses": { "200": { "description": "The handoff and its manifest", "content": { "application/json": { "schema": { "type": "object" } } } }, "401": { "description": "Authenticated session required" }, "404": { "description": "No such handoff or not the caller's" }, "503": { "description": "PostgreSQL is unavailable", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } } }
          }
        },
        "/app/api/projects/{id}/handoffs/{handoff_id}/acknowledge": {
          "post": {
            "summary": "Acknowledge a handoff",
            "description": "The recipient confirms receipt of exactly the manifest whose SHA-256 they send. A retry with the same digest returns the original acknowledgment; a different digest is refused. Archiving the Project waits for every required handoff's acknowledgment. Authorization: the handoff's recipient; otherwise 404.",
            "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }, { "name": "handoff_id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }],
            "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["manifest_sha256"], "properties": { "manifest_sha256": { "type": "string", "pattern": "^[0-9a-f]{64}$" } } } } } },
            "responses": { "200": { "description": "The acknowledgment", "content": { "application/json": { "schema": { "type": "object" } } } }, "401": { "description": "Authenticated session required" }, "404": { "description": "No such handoff or not the caller's" }, "409": { "description": "The digest is not this handoff's manifest", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }, "503": { "description": "PostgreSQL is unavailable", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } } }
          }
        },
        "/app/api/projects/{id}/handoffs/{handoff_id}/bundle": {
          "get": {
            "summary": "Download a handoff bundle",
            "description": "A ZIP of MANIFEST.json and every manifest file, rebuilt and re-verified against the manifest on each download; the manifest digest is in X-Manifest-SHA256. Authorization: a lawyer on the matter or the handoff's recipient; otherwise 404.",
            "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }, { "name": "handoff_id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }],
            "responses": { "200": { "description": "The bundle", "content": { "application/zip": { "schema": { "type": "string", "format": "binary" } } } }, "401": { "description": "Authenticated session required" }, "404": { "description": "No such handoff or not the caller's" }, "409": { "description": "A file is now missing or damaged", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }, "503": { "description": "PostgreSQL is unavailable", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } } }
          }
        },
        "/app/api/projects/{id}/source-imports": {
          "post": {
            "summary": "Import a headless Project source archive",
            "description": "Validates a raw application/zip headless source archive with the shared bounded rules (canonical ASCII paths, no links or special entries, size and inflation ceilings, headless roots only, UTF-8 text only, no credentials) and records a durable import bound to the tree digest. The SourceImport schedule creates or adopts the Firm's Forgejo repository and writes the tree as its first commit; an existing repository with other source is never replaced. An invalid archive changes nothing. Requires NAVIGATOR_FORGE_BACKEND=forgejo. Authorization: lawyer tier and Project participation; otherwise 404.",
            "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }, { "name": "operation_key", "in": "query", "required": true, "schema": { "type": "string", "minLength": 1, "maxLength": 200 } }],
            "requestBody": { "required": true, "content": { "application/zip": { "schema": { "type": "string", "format": "binary" } } } },
            "responses": { "202": { "description": "The pending or existing import", "content": { "application/json": { "schema": { "type": "object" } } } }, "401": { "description": "Authenticated session required" }, "403": { "description": "Lawyer tier required" }, "404": { "description": "No such Project or the caller does not participate" }, "409": { "description": "Forgejo not selected, another import pending, source already initialized, or key conflict", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }, "413": { "description": "Archive exceeds the upload ceiling" }, "422": { "description": "The archive breaks a headless source rule", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }, "503": { "description": "PostgreSQL is unavailable", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } } }
          }
        },
        "/app/api/projects/{id}/source-imports/{import_id}": {
          "get": {
            "summary": "Read a source import's state",
            "description": "Pending, committed (with its repository URL and commit), or refused, with the recorded reason. Authorization: lawyer tier and Project participation; otherwise 404.",
            "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }, { "name": "import_id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }],
            "responses": { "200": { "description": "The import", "content": { "application/json": { "schema": { "type": "object" } } } }, "401": { "description": "Authenticated session required" }, "403": { "description": "Lawyer tier required" }, "404": { "description": "No such import or out of scope" }, "503": { "description": "PostgreSQL is unavailable", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } } }
          }
        },
        "/app/api/projects/{id}/recovery-archives": {
          "get": {
            "summary": "List a Project's recovery archive generations",
            "description": "Every Project Recovery Archive generation of the Project, newest first, with its state, cutoff, manifest digest, attempts, and last recorded error. Authorization: admin tier and the Firm's recovery-archive capability on the Project's Firm; otherwise 404.",
            "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }],
            "responses": { "200": { "description": "Generations", "content": { "application/json": { "schema": { "type": "object" } } } }, "401": { "description": "Authenticated session required" }, "403": { "description": "Admin tier required" }, "404": { "description": "No such Project or outside the caller's Firm" }, "503": { "description": "The PostgreSQL archive store is unavailable", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } } }
          },
          "post": {
            "summary": "Request a Project recovery archive generation",
            "description": "Records a durable request to archive the Project's complete recoverable record into the private archived-projects lane and returns the pending generation. The ProjectRecovery schedule copies, verifies, and commits it. The same operation_key returns the same generation. A Project with a repository must supply its verified Git bundle receipt. Authorization: admin tier and the Firm's recovery-archive capability on the Project's Firm; otherwise 404.",
            "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }],
            "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["operation_key"], "properties": { "operation_key": { "type": "string", "minLength": 1, "maxLength": 200 }, "source_backup": { "type": "object", "required": ["format", "object", "generation", "size_bytes", "sha256", "head", "refs"], "properties": { "format": { "type": "integer" }, "object": { "type": "string" }, "generation": { "type": "integer" }, "size_bytes": { "type": "integer" }, "sha256": { "type": "string", "pattern": "^[0-9a-f]{64}$" }, "head": { "type": "string" }, "refs": { "type": "string" } } } } } } } },
            "responses": { "202": { "description": "The pending or existing generation", "content": { "application/json": { "schema": { "type": "object" } } } }, "401": { "description": "Authenticated session required" }, "403": { "description": "Admin tier required" }, "404": { "description": "No such Project or outside the caller's Firm" }, "409": { "description": "The operation key names a different request", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }, "422": { "description": "Malformed key, or a missing or malformed source backup receipt", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }, "503": { "description": "PostgreSQL or the archived-projects lane is unavailable", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } } }
          }
        },
        "/app/api/projects/{id}/recovery-archives/{generation_id}/verify": {
          "post": {
            "summary": "Re-verify a committed recovery archive generation",
            "description": "Re-reads every archived object of a committed generation and compares its size and SHA-256 with the manifest. Writes nothing. Authorization: admin tier and the Firm's recovery-archive capability on the Project's Firm; otherwise 404.",
            "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }, { "name": "generation_id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }],
            "responses": { "200": { "description": "Verified item count and failing paths", "content": { "application/json": { "schema": { "type": "object" } } } }, "401": { "description": "Authenticated session required" }, "403": { "description": "Admin tier required" }, "404": { "description": "No such generation or outside the caller's Firm" }, "409": { "description": "The generation is not committed or its manifest does not match", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }, "503": { "description": "PostgreSQL or the archived-projects lane is unavailable", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } } }
          }
        },
        "/app/api/projects/{id}/costs": {
          "get": {
            "summary": "List receipt-backed Project costs",
            "description": "Returns costs scoped to a participating Project. Client reads include only costs whose live receipt Document is currently client-visible; totals separate estimated from confirmed amounts by currency.",
            "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }],
            "responses": { "200": { "description": "Visible costs and status-separated totals", "content": { "application/json": { "schema": { "type": "object" } } } }, "404": { "description": "No such Project or the caller does not participate", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } } }
          },
          "post": {
            "summary": "Record a receipt-backed Project cost",
            "description": "Creates one Project cost and an immutable creation audit event. The receipt must be a live Document on the same Project; internal receipts are allowed for lawyers, but clients see the cost only while the receipt is client-visible. Replaying the same idempotency_key and content returns the original row; changed content conflicts. Authorization: lawyer tier and Project participation.",
            "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }],
            "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["occurred_on", "payee", "description", "amount_cents", "currency", "amount_status", "receipt_document_id", "idempotency_key"], "properties": { "occurred_on": { "type": "string", "format": "date" }, "payee": { "type": "string" }, "description": { "type": "string" }, "amount_cents": { "type": "integer", "minimum": 1 }, "currency": { "type": "string", "pattern": "^[A-Z]{3}$" }, "amount_status": { "type": "string", "enum": ["estimated", "confirmed"] }, "receipt_document_id": { "type": "string", "format": "uuid" }, "idempotency_key": { "type": "string" } } } } } },
            "responses": { "401": { "description": "Authenticated session required" }, "403": { "description": "Lawyer tier required" }, "404": { "description": "No such Project or the caller does not participate" }, "201": { "description": "Stored cost", "content": { "application/json": { "schema": { "type": "object" } } } }, "409": { "description": "Idempotency conflict or receipt already recorded", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }, "422": { "description": "Invalid cost or receipt", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } } }
          }
        },
        "/app/api/projects/{id}/costs/{cost_id}/confirm": {
          "post": { "summary": "Confirm an estimated Project cost", "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }, { "name": "cost_id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }], "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["amount_cents"], "properties": { "amount_cents": { "type": "integer", "minimum": 1 } } } } } }, "responses": { "401": { "description": "Authenticated session required" }, "403": { "description": "Lawyer tier required" }, "404": { "description": "No such cost or the caller does not participate" }, "200": { "description": "Confirmed cost; audit event appended", "content": { "application/json": { "schema": { "type": "object" } } } } } }
        },
        "/app/api/projects/{id}/costs/{cost_id}/invoice": {
          "post": { "summary": "Link a later Xero invoice", "description": "Links the cost to an existing Xero invoice for the same Project issued on or after the cost date. This operation does not modify the invoice.", "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }, { "name": "cost_id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }], "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["invoice_id"], "properties": { "invoice_id": { "type": "string" } } } } } }, "responses": { "401": { "description": "Authenticated session required" }, "403": { "description": "Lawyer tier required" }, "404": { "description": "No such cost or the caller does not participate" }, "200": { "description": "Cost linked; audit event appended", "content": { "application/json": { "schema": { "type": "object" } } } } } }
        },
        "/app/api/projects/{id}/costs/{cost_id}/resolve": {
          "post": { "summary": "Resolve an unbilled Project cost", "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }, { "name": "cost_id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }], "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["reason"], "properties": { "reason": { "type": "string" } } } } } }, "responses": { "401": { "description": "Authenticated session required" }, "403": { "description": "Lawyer tier required" }, "404": { "description": "No such cost or the caller does not participate" }, "200": { "description": "Cost resolved; reason is recorded in its audit event", "content": { "application/json": { "schema": { "type": "object" } } } } } }
        },
        "/app/api/projects/{id}/costs/{cost_id}/events": {
          "get": { "summary": "Read a Project cost's audit history", "description": "Lawyer-only immutable before/after history.", "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }, { "name": "cost_id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }], "responses": { "200": { "description": "Audit events, oldest first", "content": { "application/json": { "schema": { "type": "object" } } } }, "404": { "description": "No such cost or out of scope", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } } } }
        },
        "/app/api/projects/{id}/deadlines": {
          "get": {
            "summary": "List a Project's deadlines",
            "description": "Deadlines stored on a Project the caller participates in. Each row is read back from the store. Authorization: lawyer tier and a participation row; otherwise 404.",
            "x-mcp-tool": "list_project_deadlines",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The stored deadlines", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "object" } } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such Project, or the caller does not participate", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          },
          "post": {
            "summary": "Create a deadline",
            "description": "Store a deadline and return the written row. An optional title (at most 200 characters) names what is due. due_at is an RFC 3339 timestamp with an offset. A statute or court_rule requires an authority citation. The internal lead time is never shorter than the client lead time. Dates are stored as given; court days are not computed. Replaying a cancelled row is refused without changing it; POST to the row's /reopen endpoint first. Replaying a complete row updates its fields and preserves complete status.",
            "x-mcp-tool": "create_deadline",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "201": { "description": "The stored deadline", "content": { "application/json": { "schema": { "type": "object" } } } },
              "400": { "description": "The deadline is not valid", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such Project, or the caller does not participate", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/deadlines/{deadline_id}": {
          "patch": {
            "summary": "Update a deadline",
            "description": "Change the stored fields and return the written row. An empty title clears it. Status changes only through complete, cancel, and reopen.",
            "x-mcp-tool": "update_deadline",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } },
              { "name": "deadline_id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The stored deadline", "content": { "application/json": { "schema": { "type": "object" } } } },
              "400": { "description": "The deadline is not valid", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such deadline, or the caller does not participate", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/deadlines/{deadline_id}/complete": {
          "post": {
            "summary": "Complete a deadline",
            "description": "Set status to complete and return the stored row. This is not a delete.",
            "x-mcp-tool": "complete_deadline",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } },
              { "name": "deadline_id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The stored deadline", "content": { "application/json": { "schema": { "type": "object" } } } },
              "400": { "description": "The status cannot move", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such deadline, or the caller does not participate", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/deadlines/{deadline_id}/cancel": {
          "post": {
            "summary": "Cancel a deadline",
            "description": "Set status to cancelled and return the stored row. This is not a delete.",
            "x-mcp-tool": "cancel_deadline",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } },
              { "name": "deadline_id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The stored deadline", "content": { "application/json": { "schema": { "type": "object" } } } },
              "400": { "description": "The status cannot move", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such deadline, or the caller does not participate", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/deadlines/{deadline_id}/reopen": {
          "post": {
            "summary": "Reopen a cancelled deadline",
            "x-mcp-tool": "reopen_deadline",
            "description": "Restore a cancelled deadline to open and return the same stored row. An open row is unchanged; a complete row is refused.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } },
              { "name": "deadline_id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The stored deadline", "content": { "application/json": { "schema": { "type": "object" } } } },
              "400": { "description": "The status cannot move", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such deadline, or the caller does not participate", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/docket": {
          "get": {
            "summary": "Read a Project's docket",
            "description": "Every Docket Entry on every case of a Project the caller participates in, case by case in insertion order, with the cases themselves. Authorization: lawyer tier and a participation row; otherwise 404.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The cases and their docket entries", "content": { "application/json": { "schema": { "type": "object" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such Project, or the caller does not participate", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          },
          "post": {
            "summary": "Record a docket entry",
            "description": "Record one Docket Entry and return the written row. The entry joins the case named by docket_number; a Project with no case opens one from that number on the first write. A hearing or trial requires scheduled_on; every other kind refuses it. An entry number already on the case returns the existing row. The filed document is named by document_id or by its slug.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The entry number was already recorded; the existing row", "content": { "application/json": { "schema": { "type": "object" } } } },
              "201": { "description": "The stored docket entry", "content": { "application/json": { "schema": { "type": "object" } } } },
              "400": { "description": "The entry is not valid", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such Project, or the caller does not participate", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/docket/{entry_id}/link-document": {
          "post": {
            "summary": "Link a filed document to a docket entry",
            "description": "Name the document on file for an entry, ending its source-pending state, and return the written row. The document is named by document_id or by its slug and must belong to the same Project.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } },
              { "name": "entry_id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The stored docket entry", "content": { "application/json": { "schema": { "type": "object" } } } },
              "400": { "description": "The document is not valid", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such entry, or the caller does not participate", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/docket/{entry_id}/supersede": {
          "post": {
            "summary": "Continue a hearing or trial",
            "description": "Record a continuance: a new entry of the same kind carrying entry_number and the new scheduled_on, with a supersedes link to the prior entry, so the earlier date stays on the record. Returns the new row. Only a hearing or trial can be superseded.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } },
              { "name": "entry_id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "201": { "description": "The new docket entry", "content": { "application/json": { "schema": { "type": "object" } } } },
              "400": { "description": "The continuance is not valid", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such entry, or the caller does not participate", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/service": {
          "get": {
            "summary": "List a Project's service records",
            "description": "Every Service on a Project the caller participates in, earliest served first. Each row names its served document by docket entry where one links it, else by slug. Authorization: lawyer tier and a participation row; otherwise 404.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The stored service records", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "object" } } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such Project, or the caller does not participate", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          },
          "post": {
            "summary": "Record service of a document",
            "description": "Store one Service — one served document × one recipient × one method × one date — and return the written row. The document is named by document_id or its slug and must be this Project's. served_on is YYYY-MM-DD and is stored as given; no response deadline is computed. The recipient may link a Person or an Entity, not both.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "201": { "description": "The stored service record", "content": { "application/json": { "schema": { "type": "object" } } } },
              "400": { "description": "The record is not valid", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such Project, or the caller does not participate", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/service/{service_id}": {
          "get": {
            "summary": "Read one service record",
            "description": "One Service on a Project the caller participates in. Authorization: lawyer tier and a participation row; otherwise 404.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } },
              { "name": "service_id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The stored service record", "content": { "application/json": { "schema": { "type": "object" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such record, or the caller does not participate", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/pages": {
          "put": {
            "summary": "Store a Project's Matter Pages",
            "description": "Replace a Project's stored Matter Pages with the repository's navigator.yaml `pages:` block, in order. navigator project sync calls this so the matter page never reads git at request time. The set is checked by the gate's own rules (Y021 and D001-D005) before anything is written; a refused set stores nothing. Authorization: lawyer tier and a participation row; otherwise 404.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": {
              "required": true,
              "content": { "application/json": { "schema": { "type": "object", "required": ["pages"], "properties": { "pages": { "type": "array", "items": { "type": "object" } } } } } }
            },
            "responses": {
              "200": { "description": "The stored pages and when they were synced", "content": { "application/json": { "schema": { "type": "object" } } } },
              "400": { "description": "The gate refuses the set", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such Project, or the caller does not participate", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/fiduciary-roles": {
          "get": {
            "summary": "List a Project's Fiduciary Roles",
            "description": "Every Fiduciary Role the Project's estate plan names, in roster order: role (trustee, successor_trustee, executor, alternate_executor, agent_poa, health_care_agent, guardian), then successor order. Each row links one Person or one Entity and, optionally, the instrument Notation. Rows are written by seed import. Authorization: a firm-side participant reads every row; a client-side participant reads them while the estate Module is enabled and an empty list otherwise; anyone else, a Clerk included, gets 404.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The stored Fiduciary Roles", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "object" } } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such Project, or the caller does not participate", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/estate-assets": {
          "get": {
            "summary": "List a Project's estate asset schedule",
            "description": "Every Estate Asset on the Project, by category then description: how each passes (trust, beneficiary_designation, will, joint, other) and its funding state (funded, in_progress, not_funded, consent_needed). Rows are written by seed import. Authorization: a firm-side participant reads every row; a client-side participant reads them while the estate Module is enabled and an empty list otherwise; anyone else, a Clerk included, gets 404.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The stored Estate Assets", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "object" } } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such Project, or the caller does not participate", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/cap-table-holdings": {
          "get": {
            "summary": "List a Project's Cap Table Holdings",
            "description": "Every Cap Table Holding on the Project, by share class and largest holding first. Each row links one Person or one Entity and carries a whole number of shares. Rows are written by seed import. Authorization: a firm-side participant reads every row; a client-side participant reads them while the cap_table Module is enabled and an empty list otherwise; anyone else, a Clerk included, gets 404.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The stored Cap Table Holdings", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "object" } } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such Project, or the caller does not participate", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/matter": {
          "put": {
            "summary": "Store a Project's Matter Caption",
            "description": "Replace a Project's stored Matter Caption with the repository's navigator.yaml `matter:` block (court, case_number, judicial_officer, department, filed_on, trial_on, parties, counsel). navigator project sync calls this so the matter page's caption and party roster never read git at request time. `null` or an empty block removes the stored caption. Authorization: lawyer tier and a participation row; otherwise 404.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": {
              "required": true,
              "content": { "application/json": { "schema": { "type": "object", "required": ["matter"], "properties": { "matter": { "type": "object", "nullable": true } } } } }
            },
            "responses": {
              "200": { "description": "The stored block, or null when none is stored, and when it was synced", "content": { "application/json": { "schema": { "type": "object" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such Project, or the caller does not participate", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/communications": {
          "get": {
            "summary": "List a Project's communications",
            "description": "The firm view of a Project's conversation log, oldest first, internal notes and logged calls included. Authorization: lawyer tier and a participation row; otherwise 404. A client reads the conversation through GET .../conversation, which excludes internal notes.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The stored communications", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "object" } } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such Project, or the caller does not participate", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          },
          "post": {
            "summary": "Log a communication",
            "description": "Log a call (channel call) as an internal-note Communication with its direction, counterparty and role, start time, duration, recording consent, meet-and-confer issue, and agreements, each unconfirmed until an email confirms it; or log an email (channel email_inbound or email_outbound) that confirms a call's agreements by naming the call in confirms. Email requests may include optional source_ref (1–512 bytes), an opaque client-supplied idempotency key, not proof of provider identity. A duplicate key returns the existing row only within the same Project; reuse from another Project is rejected with 400. A call with opposing counsel is always internal. Returns the stored row.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "201": { "description": "The stored communication", "content": { "application/json": { "schema": { "type": "object" } } } },
              "400": { "description": "The communication is not valid", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such Project or call, or the caller does not participate", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/efilings": {
          "get": {
            "summary": "List a Project's e-filing transactions",
            "description": "E-Filing Transactions recorded on a Project the caller participates in, most recently changed first. Authorization: lawyer tier and a participation row; otherwise 404.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The stored transactions", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "object" } } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such Project, or the caller does not participate", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          },
          "post": {
            "summary": "Record an e-filing transaction status",
            "description": "Record one EFSP status report, keyed by transaction_id, and return the stored row. status is submitted, accepted, rejected, or unknown. filed_on, accepted_on, and conformed_document_id are refused unless the status is accepted, and a report without a decision never replaces a recorded accepted or rejected status.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The stored transaction", "content": { "application/json": { "schema": { "type": "object" } } } },
              "400": { "description": "The report is not valid", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such Project, or the caller does not participate", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/participants": {
          "get": {
            "summary": "List a matter's participation ledger",
            "description":
              "The participation rows on a matter the caller may see. Authorization: any \
               authenticated session; out-of-scope matter → 404.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The participation ledger", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "object" } } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such matter, or out of scope", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          },
          "post": {
            "summary": "Add a person to a matter's participation ledger",
            "x-mcp-tool": "link_person_project",
            "description":
              "Adds a person to a matter as a participant. The matter and the person must \
               pre-exist, and a person appears on a matter at most once. The body names only the \
               person: their matter-side participation is derived from `persons.role`, so a \
               `client` lands on the client side and a firm tier lands firm-side. This is the same \
               command the lawyer participation form funnels through. Designating the accountable \
               DRI is a separate concern. Authorization: the caller's `persons.role` must be \
               `lawyer` or `admin`; anonymous, `client`, and non-lawyer `clerk` callers are \
               rejected.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/AddParticipantRequest" }
              } }
            },
            "responses": {
              "201": { "description": "The created participation row", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/Participation" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No matter or no person with that id", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "409": { "description": "That person is already assigned to this matter", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The participation row could not be persisted (database error)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/projects/{id}/relationships/adverse": {
          "post": {
            "summary": "Record that a matter's client is adverse to a referenced party",
            "description":
              "Lands a `manual` `adverse_to` edge at full confidence in the Conflict-Check \
               Graph, from the matter's client (its Entity by default, or its client DRI with \
               `subject: client_dri`) to a party the matter already references through \
               `project_reference` (the `project_reference` seed model, or `navigator project \
               reference add`). The edge is sourced to the Relationship Log row this write \
               appends on the Project, so its provenance names the matter that lived the \
               adversity, and `check_new_matter` raises it on the next open that reaches either \
               end. Name the party by id or by the natural key the seeds use. Authorization: \
               the caller's `persons.role` must be `lawyer` or `admin`, and a lawyer must \
               participate in the matter (Owner/Admin bypass); an out-of-scope matter is `404`.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/RecordAdversityRequest" }
              } }
            },
            "responses": {
              "201": { "description": "The recorded relationship edge", "content": { "application/json": {
                "schema": { "type": "object" }
              } } },
              "400": { "description": "The body names no party, or more than one", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No such matter, out of scope, or no such party", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "422": { "description": "The matter does not reference that party, or has no client DRI to start the edge from", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The edge could not be written (database error)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/projects/{id}/participants/{role_id}": {
          "patch": {
            "summary": "Edit a matter participation row",
            "description":
              "Re-points a participation row at a different person; the participation is re-derived \
               from that person's `persons.role`. The row must belong to the matter, the person \
               must exist, no other row may already place them on the matter, and the edit must \
               not strand the matter's lawyer DRI (the accountability marker rides this row, so \
               moving it to another person or to a client-tier one is refused — reassign the DRI \
               first). Same command the lawyer form uses. Authorization: the caller's \
               `persons.role` must be `lawyer` or `admin`; anonymous, `client`, and \
               non-lawyer `clerk` callers are rejected.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } },
              { "name": "role_id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/UpdateParticipantRequest" }
              } }
            },
            "responses": {
              "200": { "description": "The updated participation row", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/Participation" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No such participation row on that matter, or no such person", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "409": { "description": "The person is already on the matter, or the edit would strand the lawyer DRI", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The participation row could not be updated (database error)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          },
          "delete": {
            "summary": "Remove a matter participant",
            "description":
              "Removes a participation row from a matter. The row must belong to the matter and must \
               not be the matter's lawyer DRI — removing it would strand the accountable lawyer, so \
               reassign the DRI first. `204 No Content` on success. Authorization: the caller's \
               `persons.role` must be `lawyer` or `admin`; anonymous, `client`, and non-lawyer \
               `clerk` callers are rejected.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } },
              { "name": "role_id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "204": { "description": "The participant was removed" },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No such participation row on that matter", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "409": { "description": "That row is the matter's lawyer DRI and cannot be removed", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The participation row could not be removed (database error)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/projects/{id}/participants/{role_id}/dri": {
          "put": {
            "summary": "Designate a matter participant as a DRI",
            "description":
              "Marks the participation row named by `role_id` as one of the matter's directly \
               responsible individuals. The side — lawyer DRI or client DRI — follows from that \
               person's tier and is not a field the caller names, so this door takes no body. \
               `204 No Content` on success. Authorization: the caller's `persons.role` must be \
               `lawyer` or `admin`; the shared command additionally gates the change on the caller \
               already holding the marker for that side (a matter's lawyer DRIs govern their own \
               set), so a lawyer who is not a current holder is refused `403`.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } },
              { "name": "role_id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "204": { "description": "The participant now carries the DRI marker" },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin, or does not hold the marker for that side", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No such participation row on that matter", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "422": { "description": "That person cannot hold the DRI marker on that side", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The designation could not be written (database error)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          },
          "delete": {
            "summary": "Clear a matter participant's DRI marker",
            "description":
              "Removes the DRI marker from the participation row named by `role_id`. `204 No \
               Content` on success; clearing a marker that is not set is an accepted no-op. \
               Authorization: the caller's `persons.role` must be `lawyer` or `admin` and, like \
               designation, the caller must already hold the marker for that side (`403` \
               otherwise). The command refuses to remove the matter's last lawyer DRI — that would \
               strand its accountable lawyer — answering `409`.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } },
              { "name": "role_id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "204": { "description": "The participant no longer carries the DRI marker" },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin, or does not hold the marker for that side", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No such participation row on that matter", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "422": { "description": "That row is the matter's last lawyer DRI; a matter always keeps one", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The marker could not be cleared (database error)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/projects/{id}/notations": {
          "get": {
            "summary": "List a matter's notations",
            "description":
              "The notations opened on a matter the caller may see. Authorization: any \
               authenticated session; out-of-scope matter → 404.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The matter's notations", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "object" } } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such matter, or out of scope", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          },
          "post": {
            "summary": "Open a notation on a matter",
            "x-mcp-tool": "create_notation",
            "description":
              "Opens a notation on an existing matter from a template. Send exactly one of \
               `template_code` or `draft_code`; both keys are rejected even when the values match. \
               The template is read from the \
               matter's own git repo, auto-saved as an immutable version, and the notation opens \
               pinned to it; a code absent from the repo falls back to the bundled firm catalog. \
               The client is resolved by email (a `client`-role Person is created on first sight \
               and attached to the matter). The first notation on a matter must be the engagement \
               that opens it (a retainer or an onboarding). This is the same command the lawyer \
               notation form funnels through. Authorization: the caller's `persons.role` must be \
               `lawyer` or `admin` (anonymous, `client`, and non-lawyer `clerk` callers are \
               rejected), and the caller must additionally participate in the matter — an \
               out-of-scope matter returns 404, never disclosing its existence (`admin` bypasses \
               the scope check).",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/CreateNotationRequest" }
              } }
            },
            "responses": {
              "201": { "description": "The opened notation's id", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/CreateNotationResponse" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "400": { "description": "Validation failed — the body is not exactly one of template_code or draft_code, or that code or client_email is blank", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No such matter (or it is outside the caller's scope), or no such template", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "409": { "description": "The client email already belongs to another person", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "422": { "description": "The template is not the required engagement, or it fails a blocking authoring rule", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The notation could not be opened (repo unconfigured or database error)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/projects/{id}/notation-inventory": {
          "get": {
            "summary": "List a matter's private notation inventory",
            "description":
              "Lists each notation's template code, workflow state, and respondent for a participating lawyer. Authorization: lawyer tier (lawyer, admin, or owner), and the caller must hold a firm-side participation row on the matter. Participation is required of every tier: owner and admin bypass project-scoping at route admission but not on matter content, so an unassigned owner or admin gets 404 like any other non-participant. A clerk is not lawyer tier and gets 403.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The private notation inventory", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "object" } } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "403": { "description": "Authenticated caller is not lawyer tier (a clerk or a client)", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such matter, or the caller holds no firm-side participation row on it", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/finance": {
          "get": {
            "summary": "Read a Project's latest finance aggregate",
            "description":
              "The newest FinanceAggregate workflow result for the Project: `source` is `workflow`, every amount is whole cents, and `totals` plus `by_month`, `by_state`, `by_vertical` and `by_attorney` follow the fee waterfall (overhead off collected, attorney share of net, Lead and Rain Maker split of the firm share). Authorization: any authenticated session that can see the Project. A Project the caller cannot see, or one no run has stored a result for, returns a non-disclosing 404.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The latest finance aggregate", "content": { "application/json": { "schema": { "type": "object" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such Project, out of scope, or no run has stored a result", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/finance/run": {
          "post": {
            "summary": "Start a finance aggregate run",
            "description":
              "Starts one FinanceAggregate workflow run for the Project. The run reads `finance/<project code>/input.json` from object storage and stores the result for the finance read above. Authorization: lawyer tier, and the caller must participate in the matter (admin bypasses); an out-of-scope Project returns 404. `202 Accepted` carries the new run id.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "202": { "description": "The run was queued", "content": { "application/json": { "schema": {
                "type": "object",
                "required": ["run_id"],
                "properties": { "run_id": { "type": "string" } }
              } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "403": { "description": "Authenticated caller is not lawyer tier", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such Project, or out of scope", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "500": { "description": "The run could not be queued", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "503": { "description": "No Restate ingress is configured", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/events/main-push": {
          "post": {
            "summary": "Report a push to a Project repository's main",
            "description":
              "A Project repository's CI reports one push to `main`. Authorization: the document-scoped CI session `project-gate.yml` mints for exactly this Project; any other session, including a CI session for another Project, returns 403. The event is handed to the per-Project `project-slack` workflow object, which resolves the Project's private channel from its own row and posts one notice per `(short_sha, run_attempt)`; a repeat of the same pair posts nothing new. The body carries mechanism fields only — no commit subject is accepted.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": { "required": true, "content": { "application/json": { "schema": {
              "type": "object",
              "required": ["short_sha", "run_url", "author_login", "gate", "run_attempt"],
              "additionalProperties": false,
              "properties": {
                "short_sha": { "type": "string", "description": "7 to 40 lowercase hex characters" },
                "run_url": { "type": "string", "format": "uri", "description": "The GitHub Actions run, https" },
                "author_login": { "type": "string", "description": "The pusher's GitHub login" },
                "gate": { "type": "string", "enum": ["pass", "fail"] },
                "publish": { "type": "string", "enum": ["pass", "fail", "skipped"], "description": "Absent when the gate reports before the publish half has run" },
                "run_attempt": { "type": "integer", "minimum": 1 }
              }
            } } } },
            "responses": {
              "202": { "description": "The notice was queued", "content": { "application/json": { "schema": {
                "type": "object",
                "required": ["queued"],
                "properties": { "queued": { "type": "boolean" } }
              } } } },
              "400": { "description": "A field is malformed", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "403": { "description": "Not the document-scoped CI session for this Project", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such Project", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "500": { "description": "The notice could not be queued", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "503": { "description": "No Restate ingress is configured", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/notations/{id}": {
          "get": {
            "summary": "Get one notation",
            "description":
              "One notation, scoped by its matter. Authorization: any authenticated session; a \
               notation whose matter the caller does not participate in returns a non-disclosing \
               404 (Owner/Admin see all).",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The notation", "content": { "application/json": { "schema": { "type": "object" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such notation, or out of scope", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/notations/{id}/review-documents": {
          "get": {
            "summary": "List a notation's review drafts",
            "description":
              "The review drafts rendered on a notation (firm work product). Authorization: lawyer \
               or admin, and the caller must participate in the notation's matter (out-of-scope → \
               404).",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The review drafts", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "object" } } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such notation, or out of scope", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/notations/{id}/answers": {
          "get": {
            "summary": "Read a notation's filed answers",
            "description":
              "Returns the filed answer values and source provenance, which are firm work product. Authorization: lawyer tier (lawyer, admin, or owner), and the caller must hold a firm-side participation row on the notation's matter. Participation is required of every tier: owner and admin bypass project-scoping at route admission but not on matter content, so an unassigned owner or admin gets 404, as does an unknown notation. A clerk is not lawyer tier and gets 403.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The notation's filed answers", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "object" } } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "403": { "description": "Authenticated caller is not lawyer tier (a clerk or a client)", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such notation, or the caller holds no firm-side participation row on its matter", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          },
          "post": {
            "summary": "Answer a notation's current questionnaire step",
            "x-mcp-tool": "answer_notation",
            "description":
              "Records an answer to the step the notation's questionnaire is currently asking, \
               attributed to the acting lawyer (the notation's bound Person stays the respondent). \
               The questionnaire advances one step at a time: `question_code` must match the step \
               being asked — an out-of-order answer is rejected with `409 question_mismatch` \
               carrying the expected code. Returns where the questionnaire is next: another \
               question to collect, or `complete`. This is the machine/lawyer surface; the \
               client-facing self-serve intake (the magic-link walk) is a separate flow, not this \
               REST path. Authorization: the caller's `persons.role` must be `lawyer` or \
               `admin` (anonymous, `client`, and non-lawyer `clerk` callers are rejected), and the \
               caller must additionally participate in the notation's matter — an out-of-scope or \
               unknown notation returns 404, never disclosing it (`admin` bypasses the scope check).",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/AnswerStepRequest" }
              } }
            },
            "responses": {
              "200": { "description": "The questionnaire's next step (a question, or complete)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/NotationStep" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No such notation, or it is outside the caller's scope", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "409": { "description": "The answer names a different step than the one being asked, or the questionnaire is already complete", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "422": { "description": "The question is not answerable through this door (not client-facing, or not flagged for re-collection)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The answer could not be recorded (runtime or database error)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/notations/{id}/request-changes": {
          "post": {
            "summary": "Send a notation back to its client for changes",
            "description":
              "Sends a notation parked at `lawyer_review` back to its client for changes: records the \
               flagged answer codes (and an optional note) and fires the `changes_requested` \
               transition — the REST mirror of the lawyer request-changes control, converging on the \
               same command. `204 No Content` on success. Authorization: the caller's `persons.role` \
               must be `lawyer` or `admin`, and the caller must participate in the notation's matter \
               — an out-of-scope or unknown notation returns 404 (`admin` bypasses the scope check).",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": {
                  "type": "object",
                  "required": ["flagged"],
                  "properties": {
                    "flagged": {
                      "type": "array",
                      "items": { "type": "string" },
                      "description": "The question codes to send back for re-collection"
                    },
                    "note": { "type": "string", "description": "An optional note to the client" }
                  }
                }
              } }
            },
            "responses": {
              "204": { "description": "The notation was sent back for changes" },
              "400": { "description": "No answer was flagged", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No such notation, or the caller does not participate in its matter", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "409": { "description": "The notation is not awaiting review", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The change request could not be recorded", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/notations/{id}/reask": {
          "post": {
            "summary": "Resubmit a re-collected notation for review",
            "description":
              "Resubmits a notation parked at re-collection: every flagged question must carry a \
               non-empty re-collected value in `answers` (keyed by bare question code), each is \
               written and the `intake_resubmitted` transition fires — the REST mirror of the lawyer \
               reask control, converging on the same command. `204 No Content` on success. \
               Authorization: the caller's `persons.role` must be `lawyer` or `admin`, and the \
               caller must participate in the notation's matter — an out-of-scope or unknown \
               notation returns 404 (`admin` bypasses the scope check).",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": {
                  "type": "object",
                  "required": ["answers"],
                  "properties": {
                    "answers": {
                      "type": "object",
                      "additionalProperties": { "type": "string" },
                      "description": "Re-collected values keyed by bare question code"
                    }
                  }
                }
              } }
            },
            "responses": {
              "204": { "description": "The notation was resubmitted for review" },
              "400": { "description": "A flagged answer was left blank", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No such notation, or the caller does not participate in its matter", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "409": { "description": "The notation is not awaiting re-collection", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The re-collection could not be resubmitted", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/notations/{id}/intake": {
          "post": {
            "summary": "Email a notation's client their self-serve intake link",
            "description":
              "Sends the notation's bound client the secure magic link that backs their self-serve \
               intake walk, ensuring the client participates in the matter first. Email delivery is \
               best-effort — a send failure is logged, not surfaced, since the link is idempotent \
               and can be re-sent — so `200` reports the link was dispatched to the returned \
               recipient. Same command the lawyer form drives. Authorization: the caller's \
               `persons.role` must be `lawyer` or `admin` (anonymous, `client`, and non-lawyer \
               `clerk` callers are rejected), and the caller must additionally participate in the \
               notation's matter — an out-of-scope or unknown notation returns 404, never \
               disclosing it (`admin` bypasses the scope check).",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The intake link was dispatched", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/SendIntakeResponse" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No such notation, or it is outside the caller's scope", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "409": { "description": "The notation has no client to send an intake link to", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The intake could not be dispatched (database error)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/notations/{id}/approval": {
          "post": {
            "summary": "Approve a notation parked at lawyer_review",
            "description":
              "The attorney approves a notation parked at `lawyer_review`: re-assembles the reviewed \
               document (template + custom clauses, or a filled government AcroForm) and fires the \
               `approved` transition so the worker renders and persists its PDF, parking at the \
               generate-PDF step. The deliberate signature dispatch is a separate door. Idempotent: \
               if the PDF is already rendered (a prior approve, or a clean machine-only intake that \
               walked straight through), approving again is a no-op that reports the current state. \
               Returns the notation's workflow state after the action. Same command the lawyer \
               review screen drives. Authorization: the caller's `persons.role` must be lawyer \
               `lawyer` or `admin` (anonymous, `client`, and non-lawyer `clerk` callers are \
               rejected), and the caller must additionally participate in the notation's matter — \
               an out-of-scope or unknown notation returns 404 (`admin` bypasses the scope check).",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The notation's workflow state after approval", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/NotationLifecycleResponse" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No such notation, or it is outside the caller's scope", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "422": { "description": "A government form could not be prepared (missing blank, pin mismatch, or mis-mapped field)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The approval could not be recorded (runtime, storage, or database error)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/notations/{id}/signature": {
          "post": {
            "summary": "Dispatch a notation's document for signature",
            "description":
              "Sends the notation's rendered document out for signature: fires `pdf_persisted` and \
               dispatches exactly one envelope through the signature provider (the client signs \
               first, the firm countersigns). Requires the document PDF to have been rendered by a \
               prior approval — if the worker has not persisted it yet, returns `409 \
               document_not_ready` (retry) rather than sending a missing document. Idempotent: a \
               notation that already has an envelope out reuses its request id and sends nothing. \
               Returns the workflow state and the provider's signature request id (which the \
               inbound completion webhook correlates back to this notation). Same command the \
               lawyer form drives. Authorization: the caller's `persons.role` must be `lawyer` \
               or `admin` (anonymous, `client`, and non-lawyer `clerk` callers are rejected), and \
               the caller must additionally participate in the notation's matter — an out-of-scope \
               or unknown notation returns 404 (`admin` bypasses the scope check).",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The envelope was dispatched (or already out)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/NotationSignatureResponse" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No such notation, or it is outside the caller's scope", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "409": { "description": "The document PDF has not been rendered yet — approve first, then retry", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The envelope could not be dispatched (provider, storage, or database error)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/review-documents/{id}/comments": {
          "post": {
            "summary": "Add an anchored comment to a review document",
            "description":
              "Adds one anchored comment to a review document (a ProseMirror range plus the text it \
               covered) and folds it into the matter's privileged conversation log. This is the \
               client-writable review surface, so — unlike every other write here — any \
               authenticated caller is admitted, and the door then enforces **client-lens** matter \
               scope: the caller must participate in the document's matter through the client side \
               (the same gate the read-only review surface uses), so a firm-side-only lawyer or a \
               non-participant receives 404, and a still-draft document is never disclosed. The \
               comment's `direction` in the conversation log is derived from the caller's role (a \
               client's comment is inbound, a lawyer/admin comment outbound); a Clerk has no \
               review-comment capability and is refused.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/AddReviewCommentRequest" }
              } }
            },
            "responses": {
              "201": { "description": "The created comment and its conversation-log spine row", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/AddReviewCommentResponse" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "400": { "description": "Validation failed — blank body or invalid anchor range", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No such (client-visible, non-draft) review document, or the caller can't author here", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The comment could not be recorded (database error)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/documents/{id}/deletion-requests": {
          "post": {
            "summary": "Request a document's deletion",
            "description":
              "A matter participant (typically the client) asks for a document to be deleted. This \
               only records a `pending` request — a lawyer/admin must later authorize the actual \
               expunge; nothing is deleted here. Client-writable like the review surface: any \
               authenticated caller is admitted, then the door enforces **client-lens** matter \
               scope (the caller must participate in the document's matter through the client \
               side), so a firm-side-only lawyer or a non-participant receives 404. Idempotent: a \
               second ask while one is already pending returns the existing request (`200`); a \
               fresh request is `201`.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "201": { "description": "A fresh pending deletion request was recorded", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/DeletionRequestResponse" }
              } } },
              "200": { "description": "A pending deletion request already existed (idempotent)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/DeletionRequestResponse" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated, but the session has no linked person to attribute the request to", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No such document, or it is outside the caller's scope", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The request could not be recorded (database error)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/notations/{id}/clauses": {
          "post": {
            "summary": "Append a custom clause to a notation's document",
            "description":
              "Appends one firm-authored clause to a notation's document; the clause is spliced into \
               the assembled body at render time (after the template, in append order). Same store \
               command the lawyer clause form drives. Authorization: the caller's `persons.role` \
               must be `lawyer` or `admin` (anonymous, `client`, and non-lawyer `clerk` \
               callers are rejected), and the caller must additionally participate in the \
               notation's matter — an out-of-scope or unknown notation returns 404 (`admin` \
               bypasses the scope check).",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/AddClauseRequest" }
              } }
            },
            "responses": {
              "201": { "description": "The appended clause", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/AddClauseResponse" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "400": { "description": "Validation failed — the clause body is blank", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No such notation, or it is outside the caller's scope", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The clause could not be appended (database error)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/notations/{id}/clauses/{clause_id}": {
          "patch": {
            "summary": "Edit a notation clause",
            "description":
              "Replaces one clause's body. Lawyer-tier only and matter-scoped; the clause must belong \
               to the notation in the path (else 404), and a blank body is rejected (400). Same \
               store command the lawyer clause form drives.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } },
              { "name": "clause_id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/AddClauseRequest" }
              } }
            },
            "responses": {
              "200": { "description": "The edited clause", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/AddClauseResponse" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "400": { "description": "Validation failed — the clause body is blank", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No such clause on that notation, or the notation is out of scope", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The clause could not be edited (database error)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          },
          "delete": {
            "summary": "Remove a notation clause",
            "description":
              "Deletes one clause from a notation's document. Lawyer-tier only and matter-scoped; the \
               clause must belong to the notation in the path. `204 No Content` on success.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } },
              { "name": "clause_id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "204": { "description": "The clause was removed" },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No such clause on that notation, or the notation is out of scope", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The clause could not be removed (database error)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/notations/{id}/clauses/{clause_id}/move": {
          "post": {
            "summary": "Reorder a notation clause",
            "description":
              "Swaps a clause with its neighbour in render order (`direction: up|down`; a move at the \
               ends is an idempotent no-op). Lawyer-tier only and matter-scoped; the clause must \
               belong to the notation in the path. `204 No Content`.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } },
              { "name": "clause_id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/MoveClauseRequest" }
              } }
            },
            "responses": {
              "204": { "description": "The clause order was updated (or unchanged at an end)" },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No such clause on that notation, or the notation is out of scope", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The clause could not be reordered (database error)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/projects/{id}/contract-review": {
          "post": {
            "summary": "Upload a contract for playbook review",
            "description":
              "Uploads an inbound third-party contract for deviation review as `multipart/form-data`: \
               either a `file` part (the contract) or a `text` part (the pasted contract). Opens a \
               contract-review notation, files the contract, runs the deviation analysis \
               against the client company's playbook, and lands the matter at `lawyer_review`. Same \
               command the lawyer/portal upload form drives. Client-writable: a matter's client may \
               submit their own contract, or the firm may — so any authenticated caller is admitted, \
               and the door enforces matter scope through either lens (lawyer or client); a \
               non-participant receives 404. If the company has no playbook on file yet, returns \
               422.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": {
              "required": true,
              "content": { "multipart/form-data": {
                "schema": {
                  "type": "object",
                  "properties": {
                    "file": { "type": "string", "format": "binary", "description": "The contract file (preferred over `contract_text`)." },
                    "contract_text": { "type": "string", "description": "The pasted contract text, used when no file is given." }
                  }
                }
              } }
            },
            "responses": {
              "201": { "description": "The created contract review", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ContractReviewResponse" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "The session has no linked person to attribute the contract to", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "400": { "description": "Malformed multipart, or neither a file nor non-empty text was provided", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No such project, or the caller does not participate in it", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "422": { "description": "The client company has no contract-review playbook on file", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The review could not be run (runtime, reviewer, or database error)", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/contract-reviews/{id}": {
          "get": {
            "summary": "Get one inbound-contract review",
            "description":
              "One inbound-contract review, scoped to its matter. Authorization: lawyer or admin, \
               and the caller must participate in the review's matter (out-of-scope → 404).",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The contract review", "content": { "application/json": { "schema": { "type": "object" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such review, or out of scope", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/contract-reviews/{id}/findings/{idx}": {
          "post": {
            "summary": "Save an attorney's decision on a review finding",
            "description":
              "Saves the attorney's edits and accept/reject decision on one finding of an inbound \
               contract review, recording the decision to the immutable audit trail — the REST \
               mirror of the review surface, converging on the same command. `204 No Content` on \
               success. Authorization: lawyer or admin, and the caller must participate in the \
               review's matter (out-of-scope → 404). A closed review is `409`; an out-of-range \
               finding index is `404`.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } },
              { "name": "idx", "in": "path", "required": true, "schema": { "type": "integer", "minimum": 0 } }
            ],
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": {
                  "type": "object",
                  "required": ["accept"],
                  "properties": {
                    "accept": { "type": "boolean", "description": "Accept (true) or reject (false) the finding for delivery" },
                    "severity": { "type": "string", "enum": ["low", "medium", "high"] },
                    "suggested_redline": { "type": "string" },
                    "attorney_note": { "type": "string" }
                  }
                }
              } }
            },
            "responses": {
              "204": { "description": "The decision was recorded" },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such review/finding, or out of scope", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "409": { "description": "The review is closed", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "500": { "description": "The decision could not be saved", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/contract-reviews/{id}/summary": {
          "post": {
            "summary": "Edit a contract review's risk summary",
            "description":
              "Edits the risk summary of an inbound contract review — the REST mirror of the review \
               surface, converging on the same command. `204 No Content` on success. Authorization: \
               lawyer or admin, and the caller must participate in the review's matter (out-of-scope \
               → 404).",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": {
                  "type": "object",
                  "required": ["risk_summary"],
                  "properties": { "risk_summary": { "type": "string" } }
                }
              } }
            },
            "responses": {
              "204": { "description": "The summary was saved" },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such review, or out of scope", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "500": { "description": "The summary could not be saved", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/contract-reviews/{id}/approve": {
          "post": {
            "summary": "Approve a contract review and deliver the memo",
            "description":
              "Assembles the review memo from the signed-off findings and risk summary, files it \
               into the matter, and drives the workflow to completion — the REST mirror of the \
               review surface, converging on the same command. `204 No Content` on success. \
               Authorization: lawyer or admin, and the caller must participate in the matter. Not at \
               the review gate is `409`; approving before every finding has an accept/reject \
               decision is `422`.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "204": { "description": "The memo was delivered and the review approved" },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such review, or out of scope", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "409": { "description": "The review is not at the approval gate", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "422": { "description": "Not every finding has been acted on", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "500": { "description": "The memo could not be delivered", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/contract-reviews/{id}/reject": {
          "post": {
            "summary": "Reject a contract review without a memo",
            "description":
              "Rejects an inbound contract review without producing a memo — the REST mirror of the \
               review surface, converging on the same command. `204 No Content` on success. \
               Authorization: lawyer or admin, and the caller must participate in the matter. Not at \
               the review gate is `409`.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "204": { "description": "The review was rejected" },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such review, or out of scope", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "409": { "description": "The review is not at the reject gate", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "500": { "description": "The review could not be rejected", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/documents": {
          "get": {
            "summary": "List a matter's documents",
            "description":
              "The documents filed on a matter. A client sees only client-visible documents \
               (internal work product is filtered out); the firm sees them all. A soft-deleted \
               document is left out unless a lawyer-lens caller sends `include_deleted=true`, which \
               adds `deleted_at`, `purge_after`, `deleted_by_person_id`, and `deleted_commit_sha` to \
               those rows; a client never sees one. Authorization: any \
               authenticated session; out-of-scope matter → 404.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } },
              { "name": "include_deleted", "in": "query", "required": false, "schema": { "type": "boolean" }, "description": "Include soft-deleted documents. Ignored for a client lens." }
            ],
            "responses": {
              "200": { "description": "The matter's documents", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "object" } } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such matter, or out of scope", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          },
          "post": {
            "summary": "File a document into a matter",
            "description":
              "Files a document into a matter — the REST mirror of the lawyer upload control, \
               converging on the same command. The browser uploads via multipart; this door takes \
               the bytes base64-encoded. `201 Created` with a source-safe revision pointer, or \
               `200 OK` when an identical retry changes nothing. Authorization: \
               lawyer or admin, and the caller must participate in the matter (out-of-scope → 404). \
               A blank filename, a missing or blank `kind`, undecodable base64, or a `kind` \
               outside the accepted set is `400`. `kind` is required and must be one of the \
               documented enum values. Uploads over 25 MiB are refused with `400 \
               document_too_large`. A supplied `slug` must retain the filename extension or the \
               request is refused with `400 invalid_slug`. `visibility` defaults to internal work product; pass \
               `\"client\"` to make it client-visible.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": {
                  "type": "object",
                  "required": ["filename", "content_base64", "kind"],
                  "properties": {
                    "filename": { "type": "string" },
                    "content_base64": { "type": "string", "description": "Base64-encoded file bytes" },
                    "content_type": { "type": "string" },
                    "slug": { "type": "string", "description": "Stable document identity; defaults to filename" },
                    "kind": {
                      "type": "string",
                      "description": "Required asset-lane document classification (`rules::kind::Kind` values valid for `Lane::Asset`). A missing or blank value is refused with `400 kind_required`; a value outside this set is refused with `400 invalid_kind`. Both messages name the accepted values; nothing is silently coerced to `unclassified`.",
                      "enum": ["letter", "filing", "will", "trust", "directive", "agreement", "pleading", "onboarding", "offboarding", "memo", "resume", "trial_prep", "transcript", "inbound_contract", "certificate_of_naturalization", "exhibit", "order", "closed_repository", "invoice", "unclassified"]
                    },
                    "visibility": { "type": "string", "enum": ["client", "internal"] },
                    "description": { "type": "string" },
                    "signing": { "$ref": "#/components/schemas/DocumentSigning" },
                    "notarization": { "type": "array", "items": { "$ref": "#/components/schemas/DocumentNotarization" } }
                  }
                }
              } }
            },
            "responses": {
              "200": { "description": "An identical retry returned the existing revision pointer", "content": { "application/json": {
                "schema": { "type": "object" }
              } } },
              "201": { "description": "The document revision was filed and its pointer returned", "content": { "application/json": {
                "schema": { "type": "object", "required": ["kind", "visibility", "current_version"], "properties": {
                  "kind": { "type": "string" },
                  "visibility": { "type": "string", "enum": ["client", "internal"] },
                  "current_version": { "type": "object", "required": ["version", "asset_id", "created_at", "sha256", "size_bytes"] },
                  "previous_version": { "type": "string", "format": "uuid" },
                  "signing": { "$ref": "#/components/schemas/DocumentSigning" },
                  "notarization": { "type": "array", "items": { "$ref": "#/components/schemas/DocumentNotarization" } }
                } }
              } } },
              "400": { "description": "Blank filename, missing or blank `kind` (error `kind_required`), undecodable base64, an unaccepted `kind` (error `invalid_kind`), or a document over 25 MiB (error `document_too_large`, with maximum and received byte counts). Both kind errors name the accepted values.", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such matter, or out of scope", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "500": { "description": "The document could not be filed — a storage or database fault, never a rejected field", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/documents/{asset_id}": {
          "patch": {
            "summary": "Reconcile visibility, set a slug on a slugless row, replace a legacy kind, or move an unrepresentable slug",
            "description": "Send `visibility` to reconcile the visibility a committed Project pointer declares, or send `slug` (and optionally `kind`) to set those fields on a row whose slug is null. A row that already has a slug, or a slug that already names a revision chain on the matter, is `409`. `sha256`, `storage_key`, and byte size stay unchanged. Send `kind` alone to replace a kind the asset lane rejects (a legacy free-text classification) on the row and every revision of its slug chain; a row whose kind is already accepted, or a chain where any revision carries an accepted kind, is `409 kind_accepted`. Send `slug` with `reslug: true` to move the row's document identity to another slug valid for its filename, across the row's whole chain (revision order, `sha256`, storage keys, and kinds are unchanged); the response carries `previous_slug`. A target equal to the current slug (`slug_unchanged`) or one that already names a chain (`slug_taken`) is `409`. `dry_run` validates and reports the write without changing the row. Authorization: lawyer or admin, and both the matter and asset are scoped (out-of-scope → 404). The API audit records the actor and operation.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } },
              { "name": "asset_id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": { "required": true, "content": { "application/json": { "schema": {
              "type": "object", "properties": {
                "visibility": { "type": "string", "enum": ["client", "internal"] },
                "slug": { "type": "string", "description": "Document identity. Must carry the stored filename's extension. Accepted while the row's slug is null, or (with `reslug: true`) as the target of moving the row's identity." },
                "kind": { "type": "string", "description": "Asset-lane kind. Applied together with `slug` on a slugless row, or alone to replace a legacy kind the asset lane rejects.", "enum": ["letter", "filing", "will", "trust", "directive", "agreement", "pleading", "onboarding", "offboarding", "memo", "resume", "trial_prep", "transcript", "inbound_contract", "certificate_of_naturalization", "exhibit", "order", "closed_repository", "invoice", "unclassified"] },
                "reslug": { "type": "boolean", "description": "With `slug`, move the row's identity to the given `slug` instead of assigning a slugless row. Refused when the target equals the current slug or already names a chain." },
                "dry_run": { "type": "boolean", "description": "When setting `slug`, `kind`, or moving a slug, validate and report without writing." }
              }
            } } } },
            "responses": {
              "200": { "description": "Visibility reconciled, the slug or kind was set, or the slug was moved (or would be, when `dry_run`)" },
              "400": { "description": "Invalid visibility, slug, or kind, `reslug` without `slug`, `reslug` combined with `visibility`/`kind`, or both `visibility` and `slug` were sent", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such matter or asset, or out of scope", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "409": { "description": "The row already has a slug (`slug_present`), the slug already names a chain (`slug_taken`), the kind is already accepted (`kind_accepted`), or the target equals the current slug (`slug_unchanged`)", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "500": { "description": "The document could not be updated", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/documents/{asset_id}/storage": {
          "post": {
            "summary": "Restore a missing storage object from a same-hash sibling",
            "description": "When the asset's storage object is missing and another asset in the same matter has the same sha256 and a readable object, copy those bytes onto the content-addressed key or re-point the row at that key, then verify the sha256. No sibling leaves the row unchanged and returns `409 no_sibling`. `dry_run` reports the key without writing. Authorization: admin, and the matter is scoped (out-of-scope → 404). The API audit records the actor and operation.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } },
              { "name": "asset_id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": { "required": true, "content": { "application/json": { "schema": {
              "type": "object", "properties": { "dry_run": { "type": "boolean" } }
            } } } },
            "responses": {
              "200": { "description": "The object is present, or the repair was applied (or would be, when `dry_run`)", "content": { "application/json": { "schema": {
                "type": "object", "required": ["storage_key", "copied", "dry_run"], "properties": {
                  "storage_key": { "type": "string" },
                  "copied": { "type": "boolean" },
                  "dry_run": { "type": "boolean" }
                }
              } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "403": { "description": "Authenticated caller is not admin", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such matter or asset, or out of scope", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "409": { "description": "No sibling holds the bytes (`no_sibling`), the present object does not match (`object_corrupt`), or the copy failed verification (`digest_mismatch`)", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "500": { "description": "The object could not be repaired", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/documents/{asset_id}/reclassify": {
          "post": {
            "summary": "Change a document chain from one accepted kind to another",
            "description": "The correction for a document filed under the wrong valid kind. Writes `kind` on the row and every revision of its slug chain, because a revision cannot change its chain's kind. `sha256`, `storage_key`, byte size, and `slug` stay unchanged. A row whose kind is not accepted (a legacy free-text classification) is `409 kind_not_accepted`; replace it through `PATCH /app/api/projects/{id}/documents/{asset_id}` with `kind`. A target equal to the current kind is `409 kind_unchanged`. `dry_run` validates and reports the write without changing the row. Authorization: lawyer or admin, and both the matter and asset are scoped (out-of-scope → 404). The API audit records the actor and operation.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } },
              { "name": "asset_id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": { "required": true, "content": { "application/json": { "schema": {
              "type": "object", "required": ["kind"], "properties": {
                "kind": { "type": "string", "description": "The accepted asset-lane kind the chain moves to.", "enum": ["letter", "filing", "will", "trust", "directive", "agreement", "pleading", "onboarding", "offboarding", "memo", "resume", "trial_prep", "transcript", "inbound_contract", "certificate_of_naturalization", "exhibit", "order", "closed_repository", "invoice", "unclassified"] },
                "dry_run": { "type": "boolean", "description": "Validate and report without writing." }
              }
            } } } },
            "responses": {
              "200": { "description": "The chain's kind was changed (or would be, when `dry_run`)", "content": { "application/json": { "schema": {
                "type": "object", "required": ["asset_id", "kind", "dry_run"], "properties": {
                  "asset_id": { "type": "string", "format": "uuid" },
                  "slug": { "type": ["string", "null"] },
                  "kind": { "type": "string" },
                  "dry_run": { "type": "boolean" }
                }
              } } } },
              "400": { "description": "`kind` is not an accepted document kind (`invalid_kind`)", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such matter or asset, or out of scope", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "409": { "description": "The row's kind is not accepted (`kind_not_accepted`), or the target is the current kind (`kind_unchanged`)", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "500": { "description": "The document could not be updated", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/documents/{asset_id}/delete": {
          "post": {
            "summary": "Soft-delete a document for 180 days",
            "description": "Stamps every revision of the document's slug chain with `deleted_at`, `purge_after` (`deleted_at` plus 180 days), and the deleting person. A pointer-removal `commit_sha` is recorded for repository sync; `manual: true` retires a mistaken chain without a repository commit and leaves `deleted_commit_sha` empty. The document drops out of the matter page and default lists; a lawyer can still read it (`GET .../documents?include_deleted=true`) and restore it, and its revision chain is kept. Deleting a chain that is already deleted changes nothing and reports the original stamps. A `closed_repository` archive is never deleted (`409 closed_repository_protected`). Authorization: lawyer or admin, and both the matter and asset are scoped (out-of-scope → 404). The API audit records the actor and request.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } },
              { "name": "asset_id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": { "required": true, "content": { "application/json": { "schema": {
              "type": "object", "anyOf": [{ "required": ["commit_sha"] }, { "required": ["manual"] }], "properties": {
                "commit_sha": { "type": "string", "description": "The 7 to 64 hexadecimal digits of the commit that removed the document's pointer." },
                "manual": { "type": "boolean", "description": "Set true to retire a mistaken chain that has no pointer-removal commit. Do not combine with commit_sha." }
              }
            } } } },
            "responses": {
              "200": { "description": "The chain's soft-deletion stamps", "content": { "application/json": { "schema": {
                "type": "object", "required": ["revisions", "deleted_at", "purge_after", "changed"], "properties": {
                  "slug": { "type": ["string", "null"] },
                  "revisions": { "type": "integer" },
                  "deleted_at": { "type": "string", "format": "date-time" },
                  "purge_after": { "type": "string", "format": "date-time" },
                  "changed": { "type": "boolean" }
                }
              } } } },
              "400": { "description": "The delete source is invalid (`invalid_delete_source`)", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such matter or asset, or out of scope", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "409": { "description": "The document is a closed_repository archive (`closed_repository_protected`)", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/documents/{asset_id}/restore": {
          "post": {
            "summary": "Restore a soft-deleted document",
            "description": "Clears the soft-deletion stamps on every revision of the chain, returning the document and its full revision history to the default lists. A chain the retention purge already expunged is `409 purged`. Authorization: lawyer or admin, and both the matter and asset are scoped (out-of-scope → 404).",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } },
              { "name": "asset_id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The chain was restored (or was not deleted: `changed` is false)", "content": { "application/json": { "schema": {
                "type": "object", "required": ["revisions", "changed"], "properties": {
                  "slug": { "type": ["string", "null"] },
                  "revisions": { "type": "integer" },
                  "changed": { "type": "boolean" }
                }
              } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such matter or asset, or out of scope", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "409": { "description": "The retention purge already expunged the document (`purged`)", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/documents/{asset_id}/hold": {
          "post": {
            "summary": "Place or release a Legal Hold on a document",
            "description": "Sets or clears `legal_hold_at` on every revision of the document's chain. A held chain is never purged, however long it has been soft-deleted. Authorization: lawyer or admin, and both the matter and asset are scoped (out-of-scope → 404).",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } },
              { "name": "asset_id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": { "required": true, "content": { "application/json": { "schema": {
              "type": "object", "required": ["held"], "properties": {
                "held": { "type": "boolean", "description": "`true` places the hold; `false` releases it." }
              }
            } } } },
            "responses": {
              "200": { "description": "The hold state and how many revisions carry it", "content": { "application/json": { "schema": {
                "type": "object", "required": ["held", "revisions"], "properties": {
                  "held": { "type": "boolean" },
                  "revisions": { "type": "integer" }
                }
              } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such matter or asset, or out of scope", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/documents/purge": {
          "post": {
            "summary": "Purge soft-deleted documents past their retention window",
            "description": "Runs the governed expunge (category `retention`) on every soft-deleted chain whose `purge_after` has passed, then marks its rows purged. A chain under a Legal Hold, and a chain on a Project that is closed but has no `closed_repository` archive yet, is skipped and reported. Idempotent: a purged chain is never touched again. Authorization: admin only; the calling admin is the expunge's authorizer.",
            "responses": {
              "200": { "description": "What the run did", "content": { "application/json": { "schema": {
                "type": "object", "required": ["purged", "held", "awaiting_archive", "failed"], "properties": {
                  "purged": { "type": "array", "items": { "type": "object" } },
                  "held": { "type": "integer" },
                  "awaiting_archive": { "type": "integer" },
                  "failed": { "type": "array", "items": { "type": "string" } }
                }
              } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "403": { "description": "Authenticated caller is not admin", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/mail/file": {
          "post": {
            "summary": "File an inbound message's attachments into the matter",
            "description":
              "Files one inbound message's attachments into the matter, entirely server-side — the raw MIME is read \
               from object storage and parsed here, so the bytes never reach the caller. Each attachment's slug is \
               derived from the message id and its position, so re-filing the same message is a no-op. \
               Authorization: lawyer or admin, and the caller must participate in the matter (out-of-scope → 404). \
               A missing or blank `kind` is `400 kind_required`; a message with no archived raw MIME is \
               `400 no_raw_message`. A message with no attachments returns `200` with empty lists rather than an \
               error. `dry_run` lists what would be filed and writes nothing.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": {
                  "type": "object",
                  "required": ["message_id", "kind"],
                  "properties": {
                    "message_id": { "type": "string", "format": "uuid", "description": "The email_conversation_message row to file attachments from" },
                    "kind": {
                      "type": "string",
                      "description": "Required asset-lane document classification, applied to every attachment in this message.",
                      "enum": ["letter", "filing", "will", "trust", "directive", "agreement", "onboarding", "offboarding", "memo", "resume", "transcript", "inbound_contract", "certificate_of_naturalization", "exhibit", "order", "unclassified"]
                    },
                    "visibility": { "type": "string", "enum": ["client", "internal"] },
                    "dry_run": { "type": "boolean", "default": false }
                  }
                }
              } }
            },
            "responses": {
              "200": { "description": "Dry run, or a message with no attachments", "content": { "application/json": { "schema": {
                "type": "object", "required": ["message_id", "dry_run", "attachments", "filed"], "properties": {
                  "message_id": { "type": "string", "format": "uuid" },
                  "dry_run": { "type": "boolean" },
                  "attachments": { "type": "array", "items": { "type": "object", "required": ["filename", "content_type", "size_bytes"], "properties": {
                    "filename": { "type": "string" },
                    "content_type": { "type": "string" },
                    "size_bytes": { "type": "integer" }
                  } } },
                  "filed": { "type": "array", "items": { "type": "object" } }
                }
              } } } },
              "201": { "description": "Every attachment filed, with one pointer each", "content": { "application/json": { "schema": {
                "type": "object", "required": ["message_id", "dry_run", "attachments", "filed"], "properties": {
                  "message_id": { "type": "string", "format": "uuid" },
                  "dry_run": { "type": "boolean" },
                  "attachments": { "type": "array", "items": { "type": "object" } },
                  "filed": { "type": "array", "items": { "type": "object", "required": ["kind", "visibility", "current_version"] } }
                }
              } } } },
              "400": { "description": "Missing or blank `kind` (error `kind_required`), or the message has no archived raw MIME (error `no_raw_message`)", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such matter or message, or out of scope", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "500": { "description": "An attachment could not be filed", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/documents/revisions": {
          "get": {
            "summary": "A document's revision chain",
            "description":
              "The revision chain of one slugged document, newest first — what `navigator site document \
               log` and `navigator site document get --version` read. The slug travels as `?slug=`, never \
               a path segment, because a slug may itself contain `/`. Authorization: any authenticated \
               session, applied as the caller's lens — a client sees only published, client-visible \
               revisions, renumbered over that visible subset so a lawyer-only revision between two \
               visible ones never shows as a gap. Out-of-scope matter, or a slug with no visible \
               revision, is `404`.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } },
              { "name": "slug", "in": "query", "required": true, "schema": { "type": "string" } }
            ],
            "responses": {
              "200": { "description": "The revision chain under the caller's lens", "content": { "application/json": { "schema": {
                "type": "object", "required": ["kind", "revisions"], "properties": {
                  "kind": { "type": "string" },
                  "revisions": { "type": "array", "items": { "type": "object", "required": ["version", "asset_id", "created_at", "sha256", "size_bytes", "filename", "visibility", "operative"], "properties": {
                    "version": { "type": "integer" },
                    "asset_id": { "type": "string", "format": "uuid" },
                    "created_at": { "type": "string" },
                    "sha256": { "type": "string" },
                    "size_bytes": { "type": "integer" },
                    "filename": { "type": "string" },
                    "visibility": { "type": "string" },
                    "operative": { "type": "boolean" },
                    "signing": { "$ref": "#/components/schemas/DocumentSigning" },
                    "notarization": { "type": "array", "items": { "$ref": "#/components/schemas/DocumentNotarization" } }
                  } } }
                }
              } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such matter, out of scope, or no visible revision under this slug", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/projects/{id}/documents/integrity": {
          "get": {
            "summary": "Storage integrity for a Project's documents",
            "description":
              "Every asset row the caller's lens can see, with whether the object at its storage key \
               exists and that object's byte size. `recorded_size` is the size on the row. \
               `deep=true` reads each object and reports whether its sha256 matches the row. \
               Rows are ordered by slug then id; `offset` and `limit` (at most 100) return one \
               page, `total` counts every visible row, and `next_offset` names the next page and \
               is absent on the last. Without `limit` every row is returned in one response. \
               `integrations` is the server-side external-id check and is empty while a pointer \
               carries no external id. Authorization: lawyer tier, including the CI document scope. \
               Out of scope is `404`.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } },
              { "name": "deep", "in": "query", "required": false, "schema": { "type": "boolean" } },
              { "name": "offset", "in": "query", "required": false, "schema": { "type": "integer", "minimum": 0 } },
              { "name": "limit", "in": "query", "required": false, "schema": { "type": "integer", "minimum": 1, "maximum": 100 } }
            ],
            "responses": {
              "200": { "description": "One row per asset the lens can see, one page at a time when `limit` is set", "content": { "application/json": { "schema": {
                "type": "object", "required": ["assets", "total", "integrations"], "properties": {
                  "total": { "type": "integer" },
                  "next_offset": { "type": "integer" },
                  "assets": { "type": "array", "items": { "type": "object", "required": ["asset_id", "exists", "recorded_size"], "properties": {
                    "asset_id": { "type": "string", "format": "uuid" },
                    "slug": { "type": "string", "nullable": true },
                    "exists": { "type": "boolean" },
                    "size_bytes": { "type": "integer" },
                    "recorded_size": { "type": "integer" },
                    "sha256_matches": { "type": "boolean" },
                    "docket_name_matches": { "type": "boolean", "description": "Whether exactly one linked Docket Entry gives this document its current DKT filename; no docket content is returned." }
                  } } },
                  "integrations": { "type": "array", "items": { "type": "object", "required": ["asset_id", "integration", "outcome", "detail"], "properties": {
                    "asset_id": { "type": "string", "format": "uuid" },
                    "integration": { "type": "string" },
                    "outcome": { "type": "string" },
                    "detail": { "type": "string" }
                  } } }
                }
              } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "403": { "description": "Not lawyer tier, or a CI session scoped to another route", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such matter, or out of the caller's lens", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/notations/{id}/transcript": {
          "post": {
            "summary": "Run a transcript against a notation's questionnaire",
            "description":
              "Runs a batch transcript against the notation's bound questionnaire, recording every \
               likely-answered inquiry as a proposed default — the REST mirror of the lawyer/CLI \
               transcript control, converging on the same command. `200 OK` with `{template_code, \
               covered, uncovered}`. Authorization: lawyer or admin, and the caller must participate \
               in the matter (out-of-scope → 404). An empty transcript is `400`; a template with no \
               questionnaire is `422`.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": { "type": "object", "required": ["transcript"], "properties": { "transcript": { "type": "string" } } }
              } }
            },
            "responses": {
              "200": { "description": "Coverage computed", "content": { "application/json": {
                "schema": {
                  "type": "object",
                  "properties": {
                    "template_code": { "type": "string" },
                    "covered": { "type": "array", "items": { "type": "object" } },
                    "uncovered": { "type": "array", "items": { "type": "string" } }
                  }
                }
              } } },
              "400": { "description": "Empty transcript", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such notation, or out of scope", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "422": { "description": "The template has no questionnaire", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "500": { "description": "The coverage pass failed", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/playbooks": {
          "get": {
            "summary": "List contract-review playbooks",
            "description": "Every firm contract-review playbook. Authorization: lawyer or admin.",
            "responses": {
              "200": { "description": "The playbooks", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "object" } } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "500": { "description": "The playbooks could not be read", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          },
          "post": {
            "summary": "Create a contract-review playbook",
            "description":
              "Creates a client Company's negotiating-position playbook — the yardstick an inbound \
               contract review measures against. The REST mirror of the lawyer playbook form, \
               converging on the same command; unlike the form's pipe-delimited textarea, this door \
               takes structured positions. `201 Created` with the new id. Authorization: lawyer or \
               admin. A blank name or empty position set is `400`; a duplicate name on that Company \
               is `409`.",
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": {
                  "type": "object",
                  "required": ["entity_id", "name", "positions"],
                  "properties": {
                    "entity_id": { "type": "string", "format": "uuid" },
                    "name": { "type": "string" },
                    "positions": {
                      "type": "array",
                      "items": {
                        "type": "object",
                        "properties": {
                          "topic": { "type": "string" },
                          "preferred": { "type": "string" },
                          "fallback": { "type": "string" },
                          "walkaway": { "type": "string" },
                          "severity": { "type": "string", "enum": ["low", "medium", "high"] }
                        }
                      }
                    }
                  }
                }
              } }
            },
            "responses": {
              "201": { "description": "The playbook was created", "content": { "application/json": {
                "schema": {
                  "type": "object",
                  "required": ["playbook_id"],
                  "properties": { "playbook_id": { "type": "string", "format": "uuid" } }
                }
              } } },
              "400": { "description": "Blank name or empty position set", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "409": { "description": "That Company already has a playbook with that name", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The playbook could not be created", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/playbooks/{id}": {
          "get": {
            "summary": "Get one playbook",
            "description": "One firm contract-review playbook. Authorization: lawyer or admin.",
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "200": { "description": "The playbook", "content": { "application/json": { "schema": { "type": "object" } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "404": { "description": "No such playbook", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          },
          "put": {
            "summary": "Replace a playbook's positions",
            "description":
              "Replaces a playbook's whole position set — the REST mirror of the lawyer edit form, \
               converging on the same command. `204 No Content` on success. Authorization: lawyer or \
               admin. An unknown playbook is `404`; an empty position set is `400`.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": {
                  "type": "object",
                  "required": ["positions"],
                  "properties": {
                    "positions": {
                      "type": "array",
                      "items": {
                        "type": "object",
                        "properties": {
                          "topic": { "type": "string" },
                          "preferred": { "type": "string" },
                          "fallback": { "type": "string" },
                          "walkaway": { "type": "string" },
                          "severity": { "type": "string", "enum": ["low", "medium", "high"] }
                        }
                      }
                    }
                  }
                }
              } }
            },
            "responses": {
              "204": { "description": "The positions were replaced" },
              "400": { "description": "Empty position set", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No such playbook", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The positions could not be saved", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/expunge-requests": {
          "get": {
            "summary": "List the pending document-deletion queue",
            "description": "The pending client document-deletion requests awaiting firm review. Authorization: lawyer or admin.",
            "responses": {
              "200": { "description": "The pending requests", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "object" } } } } },
              "401": { "description": "No authenticated session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } },
              "500": { "description": "The queue could not be read", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ApiError" } } } }
            }
          }
        },
        "/app/api/expunge-requests/{id}/authorize": {
          "post": {
            "summary": "Authorize a client document-deletion request (admin)",
            "description":
              "An admin authorizes a pending client expunge request: runs the governed expunge on \
               the requested document and marks the request authorized, linked to its audit record \
               — the REST mirror of the lawyer queue's authorize control, converging on the same \
               command. `204 No Content` on success. Authorization: admin-tier only \
               (`owner`/`admin`); `lawyer`, `clerk`, and `client` are rejected. A request already \
               resolved is `409`; an unknown request or a vanished document is `404`.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "204": { "description": "The document was expunged and the request authorized" },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not admin-tier", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No such request, or its document is gone", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "409": { "description": "The request has already been resolved", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The expunge could not be authorized", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/expunge-requests/{id}/deny": {
          "post": {
            "summary": "Deny a client document-deletion request",
            "description":
              "A lawyer or admin denies a pending client expunge request without deleting anything \
               — the REST mirror of the lawyer queue's deny control, converging on the same \
               command. `204 No Content` on success. Authorization: the caller's `persons.role` \
               must be `lawyer` or `admin`; `client` and non-lawyer `clerk` are rejected. An \
               unknown or already-resolved request is `404`.",
            "parameters": [
              { "name": "id", "in": "path", "required": true,
                "schema": { "type": "string", "format": "uuid" } }
            ],
            "responses": {
              "204": { "description": "The request was denied" },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "404": { "description": "No such pending request", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "500": { "description": "The request could not be denied", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        },
        "/app/api/templates/validate": {
          "post": {
            "summary": "Lint a Template markdown file without saving it",
            "x-mcp-tool": "validate_notation",
            "description":
              "Runs the Neon Law Navigator rule engine over the supplied markdown and returns the \
               violations. Stateless: no row is inserted and no Template is registered; nothing \
               is looked up or created, so this is the right call to lint a draft before it \
               exists anywhere. Mirrors `cli validate` rule-set selection: the default uses \
               `navigator_default_rules` (M-family markdown + N-family notation + S101 \
               line length); set `markdown_only: true` to drop the N-family and enable \
               `S102` line packing. Authorization: linting a Template is a lawyer authoring \
               activity, so the caller's `persons.role` must be `lawyer` or `admin`; anonymous \
               and `client` callers are rejected.",
            "requestBody": {
              "required": true,
              "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ValidateRequest" }
              } }
            },
            "responses": {
              "200": { "description": "Lint report", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ValidateResponse" }
              } } },
              "401": { "description": "No authenticated session", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } },
              "403": { "description": "Authenticated caller is not Lawyer/admin", "content": { "application/json": {
                "schema": { "$ref": "#/components/schemas/ApiError" }
              } } }
            }
          }
        }
      },
      "components": {
        "securitySchemes": {
          "bearerAuth": {
            "type": "http",
            "scheme": "bearer",
            "bearerFormat": "JWT",
            "description":
              "OIDC bearer token. In production the token is validated against the \
               configured IdP (Google Identity); in KIND the workspace's Rauthy \
               instance signs RS256 JWTs. A browser-initiated alternative — the \
               `navigator_session` cookie set by `/auth/login` — is documented as the \
               `sessionCookie` scheme. Authorization is then delegated to the Open \
               Embedded Rego policy: the policy compiled into Navigator lets any \
               authenticated session call the `/app/api/*` GET listings, and restricts the \
               write commands (people create/update/delete and welcome, entity create/update/delete, \
               project open/update/delete) and \
               the stateless \
               `/app/api/templates/validate` linter to `lawyer` and `admin`."
          },
          "sessionCookie": {
            "type": "apiKey",
            "in": "cookie",
            "name": "navigator_session",
            "description":
              "Opaque session cookie set by the OAuth Authorization Code + PKCE flow \
               at `/auth/login`. The same cookie gates the `/app/*` surface."
          }
        },
        "schemas": {
          "Person": {
            "type": "object",
            "required": ["id", "name", "email", "role",
                         "inserted_at", "updated_at"],
            "properties": {
              "id":                 { "type": "string", "format": "uuid" },
              "name":               { "type": "string" },
              "given_name":         { "type": ["string", "null"] },
              "family_name":        { "type": ["string", "null"] },
              "middle_name":        { "type": ["string", "null"] },
              "email":              { "type": "string", "format": "email" },
              "oidc_subject":       { "type": ["string", "null"],
                                      "description": "OIDC `sub` claim once the row is linked." },
              "role":               { "$ref": "#/components/schemas/PersonRole" },
              "title":              { "type": ["string", "null"] },
              "phone":              { "type": ["string", "null"] },
              "xero_contact_id":    { "type": ["string", "null"] },
              "profile_image_url":  { "type": ["string", "null"] },
              "inserted_at":        { "type": "string" },
              "updated_at":         { "type": "string" }
            }
          },
          "PersonRole": {
            "type": "string",
            "enum": ["owner", "admin", "lawyer", "clerk", "client"],
              "description": "System-wide authorization tier stored in `persons.role`; Lawyer is a person licensed to practice law and authorized for Navigator legal work, while Owner/Admin are organization-level operators and Clerk is a supervised non-lawyer."
          },
          "Testimonial": {
            "type": "object",
            "required": ["id", "quote", "attribution", "consented_at", "published_at"],
            "properties": {
              "id": { "type": "string", "format": "uuid" },
              "quote": { "type": "string" },
              "attribution": { "type": ["string", "null"] },
              "consented_at": { "type": ["string", "null"] },
              "published_at": { "type": ["string", "null"] }
            }
          },
          "SeedRequest": {
            "type": "object",
            "required": ["model", "yaml"],
            "properties": {
              "model": { "type": "string", "description": "Supported singular glossary term: `person`, `entity`, `person_project_role`, `person_entity_role`, `address`, `relationship_log`, or `project_reference`." },
              "yaml": { "type": "string", "description": "Seed YAML with `lookup_fields` and `records`." },
              "overwrite": { "type": "boolean", "default": false },
              "dry_run": { "type": "boolean", "default": false, "description": "Return the reconciliation plan without writing." },
              "as_ci": { "type": "boolean", "default": false, "description": "Apply the scoped-session rules a CI-minted session carries, so the plan answers whether CI could perform the same write. Requires `dry_run` without `overwrite`, and `project`; anything else is refused as `403 scope_violation`." },
              "project": { "type": "string", "description": "With `as_ci`: the Project code CI would be scoped to." }
            }
          },
          "SeedReport": {
            "type": "object",
            "required": ["model", "created", "updated", "unchanged", "records"],
            "properties": {
              "model": { "type": "string" },
              "created": { "type": "integer", "minimum": 0 },
              "updated": { "type": "integer", "minimum": 0 },
              "unchanged": { "type": "integer", "minimum": 0 },
              "records": {
                "type": "array",
                "items": {
                  "type": "object",
                  "required": ["key", "action"],
                  "properties": {
                    "key": { "type": "string" },
                    "action": { "type": "string", "enum": ["new", "unchanged", "changed"] },
                    "changed_fields": { "type": "array", "items": { "type": "string" } },
                    "warnings": { "type": "array", "items": { "type": "string" }, "description": "Plan-time cautions on a dry run, such as a new person whose name already belongs to another person." }
                  }
                }
              }
            }
          },
          "CreatePersonRequest": {
            "type": "object",
            "required": ["name", "email"],
            "properties": {
              "name":        { "type": "string" },
              "email":       { "type": "string", "format": "email" },
              "role":        { "allOf": [ { "$ref": "#/components/schemas/PersonRole" } ],
                               "description": "Defaults to `client` when omitted or blank; non-empty invalid values are rejected." },
              "given_name":  { "type": ["string", "null"] },
              "family_name": { "type": ["string", "null"] },
              "middle_name": { "type": ["string", "null"] },
              "notion_user_id": { "type": ["string", "null"],
                               "description": "Stable Notion workspace user ID. Stored as an external identity; never a token, credential, or permission." }
            },
            "example": {
              "name": "Libra Example",
              "email": "libra@example.com",
              "role": "client",
              "given_name": "Libra",
              "family_name": "Example"
            }
          },
          "UpdatePersonRequest": {
            "type": "object",
            "properties": {
              "name":        { "type": "string",
                               "description": "Omit to leave unchanged; present must not be blank." },
              "email":       { "type": "string", "format": "email",
                               "description": "Omit to leave unchanged; present must not be blank." },
              "role":        { "allOf": [ { "$ref": "#/components/schemas/PersonRole" } ],
                               "description": "Blank/absent preserves the current role; honored only for Owner/Admin callers up to their own authority, and the bootstrap Owner is always `owner`." },
              "given_name":  { "type": ["string", "null"],
                               "description": "Omit to leave unchanged; send null or a blank string to clear." },
              "family_name": { "type": ["string", "null"],
                               "description": "Omit to leave unchanged; send null or a blank string to clear." },
              "middle_name": { "type": ["string", "null"],
                               "description": "Omit to leave unchanged; send null or a blank string to clear." },
              "notion_user_id": { "type": ["string", "null"],
                               "description": "Omit to leave unchanged; send null or a blank string to clear the stable Notion external identity." }
            },
            "example": {
              "name": "Libra Example",
              "email": "libra@example.com",
              "role": "lawyer"
            }
          },
          "DocumentSigning": {
            "type": "object",
            "description": "How an executed document was signed. Optional on every document; a method outside the enum, a malformed date, or an unknown key is refused before any byte is stored.",
            "required": ["method", "signed_on"],
            "additionalProperties": false,
            "properties": {
              "method": { "type": "string", "enum": ["wet_ink", "docusign"] },
              "signed_on": { "type": "string", "format": "date" }
            }
          },
          "DocumentNotarization": {
            "type": "object",
            "description": "One notarial acknowledgment on an executed document; a packet may carry several. Never the notary's name or commission number: an unknown key is refused, as is a state that is not a two-letter uppercase code, a blank county, or a commission that expires before the acknowledgment date (`400 invalid_notarization`). Refused before any byte is stored.",
            "required": ["method", "acknowledged_on", "state", "county", "commission_expires"],
            "additionalProperties": false,
            "properties": {
              "method": { "type": "string", "enum": ["in_person", "remote_online"] },
              "acknowledged_on": { "type": "string", "format": "date" },
              "state": { "type": "string", "pattern": "^[A-Z]{2}$" },
              "county": { "type": "string" },
              "commission_expires": { "type": "string", "format": "date" }
            }
          },
          "ApiError": {
            "type": "object",
            "required": ["error"],
            "properties": {
              "error":   { "type": "string" },
              "message": { "type": "string" }
            }
          },
          "Entity": {
            "type": "object",
            "required": ["id", "name", "entity_type_id", "jurisdiction_id",
                         "inserted_at", "updated_at"],
            "properties": {
              "id":              { "type": "string", "format": "uuid" },
              "name":            { "type": "string" },
              "entity_type_id":  { "type": "string", "format": "uuid" },
              "jurisdiction_id": { "type": "string", "format": "uuid" },
              "inserted_at":     { "type": "string" },
              "updated_at":      { "type": "string" }
            }
          },
          "Project": {
            "type": "object",
            "required": ["id", "code", "name", "status", "brand", "firm_id", "entity_id",
                         "inserted_at", "updated_at"],
            "properties": {
              "id":                 { "type": "string", "format": "uuid" },
              "code":               { "type": "string",
                                      "description": "Stable slug, e.g. `acme-llc-formation`." },
              "name":               { "type": "string" },
              "status":             { "type": "string",
                                      "description": "`open`, `closed`, or `archived`." },
              "brand":              { "type": "string",
                                      "description": "The house brand this Project belongs to; changed only with its owning Firm." },
              "firm_id":            { "type": ["string", "null"], "format": "uuid",
                                      "description": "The owning Firm, or null until the Project is assigned." },
              "entity_id":          { "type": "string", "format": "uuid" },
              "description":        { "type": ["string", "null"] },
              "drive_folder_id":    { "type": ["string", "null"],
                                      "description": "The Project's Drive ingest folder id, or `null` before it is provisioned." },
              "repository_url":     { "type": ["string", "null"],
                                      "description": "The Project's one source repository, as a whole URL on any forge, or `null` when none is recorded." },
              "git_initialized_at": { "type": ["string", "null"] },
              "forge_provisioned_at": { "type": ["string", "null"],
                                      "description": "Stamped only once `repository_url` was created or adopted; `null` means not done or unknown." },
              "closed_at":          { "type": ["string", "null"] },
              "calendar_id":        { "type": ["string", "null"],
                                      "description": "The Project's shared Google Calendar id, or `null` before it is created." },
              "booking_url":        { "type": ["string", "null"],
                                      "description": "Where the client books time with the firm, or `null`. Cleared when the Project closes." },
              "onboarding_jurisdiction": { "type": ["string", "null"],
                                      "description": "The jurisdiction an onboarding Notation's Clauses bind to (the state whose law and arbitration seat an engagement letter names), or `null` before it is chosen." },
              "inserted_at":        { "type": "string" },
              "updated_at":         { "type": "string" }
            }
          },
          "ProjectSelector": {
            "type": "object",
            "description": "Exactly one of the two selects the work. Both together, or neither, is a 400 — `all` beside a code would leave it ambiguous whether the code narrowed the sweep or was ignored.",
            "properties": {
              "project_code": { "type": ["string", "null"],
                                "description": "One Project code. Omit when `all` is true." },
              "all": { "type": "boolean", "default": false,
                       "description": "Act on every Project visible to the caller — the same participation-scoped lens `GET /app/api/projects` uses, so an admin sweep can never reach a matter the read surface would hide." }
            }
          },
          "SlackIntegrationRequest": {
            "type": "object",
            "required": ["project_code"],
            "properties": {
              "project_code": { "type": "string" },
              "event": { "type": ["string", "null"],
                         "enum": ["project_opened", "project_closed", "project_reconciled", "integration_unavailable", null],
                         "description": "Required by `notify`, absent for `ensure`. A closed vocabulary: the notice text is derived from the kind, so no caller-supplied prose reaches a channel." }
            }
          },
          "IntegrationReport": {
            "type": "object",
            "required": ["results"],
            "properties": {
              "results": {
                "type": "array",
                "items": {
                  "type": "object",
                  "required": ["project_code", "outcome"],
                  "properties": {
                    "project_code": { "type": "string" },
                    "outcome": { "type": "string",
                                 "description": "A closed slug naming what happened. Never a provider URL, page id, or channel id — those stay on the firm side." },
                    "detail": { "type": "string",
                                "description": "Present only for an outcome that names a mechanism the operator must act on, such as how many pages carry one code." }
                  }
                }
              }
            }
          },
          "OpenMatterRequest": {
            "type": "object",
            "required": ["name", "code", "client_id", "entity_id", "attestation"],
            "properties": {
              "name":         { "type": "string", "description": "The matter's display name." },
              "code":         { "type": "string",
                                "description": "The matter's code, stored exactly as given — lowercase letters, digits, and single hyphens, starting and ending with a letter or digit, at most 80 characters. Required. A code is chosen once at matter-open and never changes; a code already in use by another matter is refused with 409." },
              "client_id":    { "type": "string", "format": "uuid",
                                "description": "The client of record — a pre-existing `client`-role person, never a firm attorney." },
              "entity_id":    { "type": "string", "format": "uuid",
                                "description": "The pre-existing entity the matter opens against." },
              "jurisdiction_id": { "type": ["string", "null"], "format": "uuid",
                                    "description": "The matter's governing jurisdiction, when settled at open." },
              "description":  { "type": ["string", "null"], "description": "The matter's scope narrative." },
              "attestation":  { "type": "boolean",
                                "description": "The opening attorney's conflict attestation. Must be true; a missing attestation is refused. Affirms the attorney has checked for conflicts, and that either none prevent opening this Project or this Project is not legal advice. The attester is the authenticated session's person — never taken from this body." },
              "status":       { "type": "string", "enum": ["closed"],
                                "description": "Open the matter already closed — an engagement that ended before anyone opened its row. Omit for the ordinary open. Requires closed_at; refused if closed_at is present without this." },
              "closed_at":    { "type": ["string", "null"], "format": "date-time",
                                "description": "Required exactly when status is \"closed\". Validated the same way POST /app/api/projects/{id}/lifecycle validates an effective time: may not precede the matter's own open or fall in the future." }
            },
            "example": {
              "name": "Acme LLC — Formation",
              "code": "acme-llc-formation",
              "client_id": "0199a1f0-0000-7000-8000-000000000001",
              "entity_id": "0199a1f0-0000-7000-8000-000000000002",
              "description": "Delaware LLC formation.",
              "attestation": true
            }
          },
          "UpdateProjectRequest": {
            "type": "object",
            "description": "A patch. Every field is optional: send only what you want to change. Absent — or an explicit \
                            JSON `null` — leaves a column exactly as it was; an empty string clears it. No column is ever \
                            blanked out because the body did not mention it, so a caller that knows about three fields \
                            cannot erase the other five. Any field not listed here — `status` and `closed_at` included — \
                            is rejected with `400` rather than silently ignored; move a matter's lifecycle through \
                            `POST /app/api/projects/{id}/lifecycle` instead.",
            "properties": {
              "name":        { "type": "string",
                               "description": "Omit to leave unchanged. The one field with no clear operation: a blank \
                                               name is rejected rather than applied, because a matter with no name is not \
                                               a state a patch may produce." },
              "entity_id":   { "type": "string", "format": "uuid",
                               "description": "Omit to leave the matter's entity unchanged; a value moves it. \
                                               `projects.entity_id` is NOT NULL, so there is no clear operation — a JSON `null` \
                                               is accepted but treated as omission (leaves the entity unchanged), never as a clear." },
              "firm_id":     { "type": "string", "format": "uuid",
                               "description": "Set only together with `brand`. Owner may target any Firm; Admin must hold `manage_membership` on the target Firm. The CLI resolves the Firm name from `GET /app/api/firms`." },
              "brand":       { "type": "string",
                               "description": "Set only together with `firm_id`, and only to a brand that Firm wears." },
              "description": { "type": "string",
                               "description": "Omit to leave unchanged; a blank string clears it. A JSON `null` \
                                               is accepted but treated as omission; send \"\" to clear." },
              "repository_url": { "type": "string",
                               "description": "The matter's source repository, as a whole URL on any forge. Omit to leave \
                                               unchanged; a blank string clears it. A value must be an http(s) URL naming a \
                                               host and a path, with no embedded credential." },
              "internal_slack_channel_url": { "type": "string",
                               "description": "The lawyer-only Slack channel. Omit to leave unchanged; a blank string clears it." },
              "external_slack_channel_url": { "type": "string",
                               "description": "The Slack channel shared with the client. Omit to leave unchanged; a blank string clears it." },
              "private_notion_page_url": { "type": "string",
                               "description": "The firm-only Notion page. Omit to leave unchanged; a blank string clears it." },
              "shared_notion_page_url": { "type": "string",
                               "description": "The Notion page shared with the client. Omit to leave unchanged; a blank string clears it." },
              "calendar_id": { "type": "string",
                               "description": "The Project's shared Google Calendar id. Omit to leave unchanged; a blank string clears it." },
              "drive_folder_id": { "type": "string",
                               "description": "The Project's Drive ingest folder id, in the Firm's Projects shared drive. Omit to leave unchanged; a blank string clears it. Refused when another matter already records the folder." },
              "booking_url": { "type": "string",
                               "description": "The client's booking page, an http(s) URL. Omit to leave unchanged; a blank string clears it." },
              "onboarding_jurisdiction": { "type": "string",
                               "description": "The jurisdiction an onboarding Notation's Clauses bind to (the state whose law and \
                                               arbitration seat an engagement letter names). Omit to leave unchanged; a blank string clears it." }
            },
            "additionalProperties": false,
            "example": {
              "repository_url": "https://forge.example/an-organization/acme"
            }
          },
          "TransitionProjectRequest": {
            "type": "object",
            "required": ["transition"],
            "additionalProperties": false,
            "description": "A lifecycle move and its optional effective time.",
            "properties": {
              "transition": {
                "type": "string",
                "enum": ["close", "reopen", "archive"],
                "description": "Which lifecycle move to make. `close` and `archive` stamp `closed_at` \
                                if it is not already stamped (preserving an existing stamp rather than \
                                restarting the retention window); `reopen` clears it. `archive` is \
                                terminal — every transition off it besides a repeated `archive` is \
                                refused."
              },
              "effective_at": {
                "type": ["string", "null"],
                "format": "date-time",
                "description": "When a close or archive actually took effect. Optional; rejected for \
                                reopen, in the future, or before matter-open. A supplied valid value \
                                replaces an existing close stamp so the retention start can be corrected."
              },
              "reason": {
                "type": ["string", "null"],
                "enum": ["pitch_declined", "pitch_lapsed", "pitch_withdrawn", "pitch_superseded", "engagement_completed", "client_terminated", "firm_withdrew", null],
                "description": "Required for a first close. Pitch reasons apply only before onboarding; active-matter reasons apply only after onboarding. A pitch also requires an offboarding document."
              }
            },
            "example": {
              "transition": "close",
              "effective_at": "2026-01-15T00:00:00Z"
            }
          },
          "RecordAdversityRequest": {
            "type": "object",
            "additionalProperties": false,
            "description": "Exactly one of `person_id`, `entity_id`, `person_email`, or `entity_name` names the referenced party.",
            "properties": {
              "person_id":    { "type": "string", "format": "uuid" },
              "entity_id":    { "type": "string", "format": "uuid" },
              "person_email": { "type": "string", "format": "email" },
              "entity_name":  { "type": "string" },
              "subject":      { "type": "string", "enum": ["entity", "client_dri"], "default": "entity",
                                "description": "Which side of the client the edge starts from: the matter's Entity, or its client DRI." }
            },
            "example": {
              "entity_name": "Widget Works LLC",
              "subject": "entity"
            }
          },
          "AddParticipantRequest": {
            "type": "object",
            "additionalProperties": false,
            "description": "DRI fields are refused with 400; use PUT or DELETE on the participant's /dri endpoint for an authorized, audited marker change.",
            "required": ["person_id"],
            "properties": {
              "person_id":     { "type": "string", "format": "uuid",
                                 "description": "An existing person to add to the matter. Their participation is derived from `persons.role` and is not an input." }
            },
            "example": {
              "person_id": "0199a1f0-0000-7000-8000-000000000003"
            }
          },
          "UpdateParticipantRequest": {
            "type": "object",
            "additionalProperties": false,
            "description": "DRI fields are refused with 400; use PUT or DELETE on the participant's /dri endpoint for an authorized, audited marker change.",
            "required": ["person_id"],
            "properties": {
              "person_id":     { "type": "string", "format": "uuid",
                                 "description": "The person the row should name (may differ from the current one). The participation is re-derived from that person's `persons.role`." }
            },
            "example": {
              "person_id": "0199a1f0-0000-7000-8000-000000000003"
            }
          },
          "Participation": {
            "type": "object",
            "required": ["id", "person_id", "project_id", "participation",
                         "inserted_at", "updated_at"],
            "properties": {
              "id":            { "type": "string", "format": "uuid" },
              "person_id":     { "type": "string", "format": "uuid" },
              "project_id":    { "type": "string", "format": "uuid" },
              "participation": { "type": "string" },
              "inserted_at":   { "type": "string" },
              "updated_at":    { "type": "string" }
            }
          },
          "CreateNotationRequest": {
            "type": "object",
            "description": "Exactly one of template_code or draft_code, plus client_email. Both code keys are invalid even when the values match. The chosen code is trimmed and opened as the canonical template.",
            "required": ["client_email"],
            "properties": {
              "template_code": { "type": "string",
                                 "description": "Canonical template code. Read from the matter's git repo first, then the bundled firm catalog. Provide this or draft_code, not both. Trimmed; must not be blank." },
              "draft_code":    { "type": "string",
                                 "description": "The same template code under the lawyer's name for a Template (a Draft). Provide this or template_code, not both. Trimmed; must not be blank." },
              "client_email":  { "type": "string", "format": "email",
                                 "description": "The client the notation is bound to. Resolved by email; a `client`-role Person is created on first sight and attached to the matter. Trimmed; must not be blank." }
            },
            "oneOf": [
              { "required": ["template_code"] },
              { "required": ["draft_code"] }
            ],
            "example": {
              "template_code": "retainer",
              "client_email": "client@example.com"
            }
          },
          "CreateNotationResponse": {
            "type": "object",
            "required": ["notation_id"],
            "properties": {
              "notation_id": { "type": "string", "format": "uuid",
                               "description": "The opened notation. Drive its questionnaire next." }
            }
          },
          "AnswerStepRequest": {
            "type": "object",
            "required": ["question_code", "value"],
            "properties": {
              "question_code": { "type": "string",
                                 "description": "The step being answered. Must match the code the questionnaire is currently asking (trimmed); an out-of-order code is rejected with 409." },
              "value":         { "type": "string",
                                 "description": "The answer value, verbatim (not trimmed — a value may carry meaningful whitespace)." },
              "reference_id":  { "type": "string", "format": "uuid",
                                 "description": "For a record/reference question, the id of the picked row; `value` stays its display name. Omit for a free-typed answer." }
            },
            "example": {
              "question_code": "client_full_name",
              "value": "Aquarius Lumen"
            }
          },
          "NotationStep": {
            "oneOf": [
              {
                "type": "object",
                "required": ["status", "question"],
                "properties": {
                  "status":   { "type": "string", "enum": ["needs_answer"] },
                  "question": { "$ref": "#/components/schemas/NotationQuestion" }
                }
              },
              {
                "type": "object",
                "required": ["status"],
                "properties": {
                  "status": { "type": "string", "enum": ["complete"],
                              "description": "The questionnaire reached END; nothing more to answer." }
                }
              }
            ],
            "description": "Where the questionnaire is after recording the answer — discriminated by `status`."
          },
          "NotationQuestion": {
            "type": "object",
            "required": ["id", "code", "prompt", "answer_type", "choices"],
            "properties": {
              "id":          { "type": "string", "format": "uuid" },
              "code":        { "type": "string",
                               "description": "POST this back as `question_code` to answer this step." },
              "prompt":      { "type": "string" },
              "answer_type": { "type": "string",
                               "description": "How to collect the value (e.g. text, choice, record)." },
              "choices":     { "type": "array", "items": { "$ref": "#/components/schemas/QuestionChoice" },
                               "description": "The allowed answers for a choice question; empty otherwise." },
              "help_text":   { "type": "string",
                               "description": "Attorney-authored guidance for this question; omitted when it declares none." }
            }
          },
          "QuestionChoice": {
            "type": "object",
            "required": ["value", "label"],
            "properties": {
              "value": { "type": "string", "description": "Send this as `value` when picking the choice." },
              "label": { "type": "string", "description": "Human-readable label for the choice." }
            }
          },
          "SendIntakeResponse": {
            "type": "object",
            "required": ["notation_id", "recipient"],
            "properties": {
              "notation_id": { "type": "string", "format": "uuid" },
              "recipient":   { "type": "string", "format": "email",
                               "description": "The client address the intake link was dispatched to." }
            }
          },
          "NotationLifecycleResponse": {
            "type": "object",
            "required": ["notation_id", "state"],
            "properties": {
              "notation_id": { "type": "string", "format": "uuid" },
              "state":       { "type": "string",
                               "description": "The notation's workflow state after the transition (e.g. `generate_pdf__retainer_pdf`)." }
            }
          },
          "NotationSignatureResponse": {
            "type": "object",
            "required": ["notation_id", "state", "signature_request_id"],
            "properties": {
              "notation_id":          { "type": "string", "format": "uuid" },
              "state":                { "type": "string",
                                        "description": "The notation's workflow state after dispatch (e.g. `sent_for_signature__pending`)." },
              "signature_request_id": { "type": "string",
                                        "description": "The provider's request id; the inbound completion webhook correlates it back to this notation." }
            }
          },
          "AddReviewCommentRequest": {
            "type": "object",
            "required": ["anchor_start", "anchor_end", "quoted_text", "body"],
            "properties": {
              "anchor_start": { "type": "integer",
                                "description": "ProseMirror start position of the covered range." },
              "anchor_end":   { "type": "integer",
                                "description": "ProseMirror end position; must be greater than anchor_start." },
              "quoted_text":  { "type": "string",
                                "description": "The document text the range covered (trimmed)." },
              "body":         { "type": "string",
                                "description": "The comment text. Required and non-blank (trimmed)." }
            },
            "example": {
              "anchor_start": 120,
              "anchor_end": 148,
              "quoted_text": "the indemnification clause",
              "body": "Can we cap this at fees paid?"
            }
          },
          "AddReviewCommentResponse": {
            "type": "object",
            "required": ["comment_id", "communication_id"],
            "properties": {
              "comment_id":       { "type": "string", "format": "uuid" },
              "communication_id": { "type": "string", "format": "uuid",
                                    "description": "The comment's row in the matter's unified conversation log." }
            }
          },
          "DeletionRequestResponse": {
            "type": "object",
            "required": ["request_id", "already_pending"],
            "properties": {
              "request_id":      { "type": "string", "format": "uuid",
                                   "description": "The pending expunge request." },
              "already_pending": { "type": "boolean",
                                   "description": "True when a pending request already existed (the ask was a no-op)." }
            }
          },
          "AddClauseRequest": {
            "type": "object",
            "required": ["body"],
            "properties": {
              "body": { "type": "string",
                        "description": "The clause markdown. Required and non-blank (trimmed)." }
            },
            "example": { "body": "The parties agree to binding arbitration in Clark County, Nevada." }
          },
          "AddClauseResponse": {
            "type": "object",
            "required": ["clause_id"],
            "properties": {
              "clause_id": { "type": "string", "format": "uuid" }
            }
          },
          "MoveClauseRequest": {
            "type": "object",
            "required": ["direction"],
            "properties": {
              "direction": { "type": "string", "enum": ["up", "down"],
                             "description": "`up` moves the clause earlier in render order; anything else moves it later." }
            },
            "example": { "direction": "up" }
          },
          "ContractReviewResponse": {
            "type": "object",
            "required": ["review_id"],
            "properties": {
              "review_id": { "type": "string", "format": "uuid",
                             "description": "The created contract review (deviation analysis attached; matter parked at lawyer_review)." }
            }
          },
          "UpdateBrandPresentationRequest": {
            "type": "object",
            "required": ["typeface", "primary_color"],
            "additionalProperties": false,
            "properties": {
              "typeface": { "type": "string",
                            "description": "A closed catalog id, or \"uploaded\" to use font_family with a font uploaded via the native multipart door.",
                            "enum": ["gorp-serif", "plus-jakarta-sans", "tinos", "system-serif", "system-sans", "uploaded"] },
              "primary_color": { "type": "string",
                            "description": "A #rrggbb hex, refused unless its on-primary text (white or black, whichever contrasts more) clears WCAG AA 4.5:1 and it clears 3:1 against the light page surface." },
              "font_family": { "type": "string",
                            "description": "The CSS font-family name for an uploaded font. Blank clears it." }
            },
            "example": { "typeface": "uploaded", "primary_color": "#007c91", "font_family": "Custom Sans" }
          },
          "Brand": {
            "type": "object",
            "required": ["id", "name", "key"],
            "properties": {
              "id": { "type": "string", "format": "uuid" },
              "name": { "type": "string" },
              "key": { "type": "string" },
              "typeface": { "type": "string" },
              "primary_color": { "type": "string" },
              "accent_color": { "type": "string" },
              "logo_object_key": { "type": "string" },
              "logo_content_type": { "type": "string" },
              "font_family": { "type": "string" },
              "font_object_key": { "type": "string" },
              "font_licence": { "type": "string" }
            }
          },
          "CreateEntityRequest": {
            "type": "object",
            "required": ["name", "entity_type_id", "jurisdiction_id"],
            "properties": {
              "name":            { "type": "string",
                                   "description": "Trimmed on the way in; must not be blank." },
              "entity_type_id":  { "type": "string", "format": "uuid",
                                   "description": "An existing `/app/api/entity-types` row." },
              "jurisdiction_id": { "type": "string", "format": "uuid",
                                   "description": "An existing `/app/api/jurisdictions` row." }
            },
            "example": {
              "name": "Example Holdings LLC",
              "entity_type_id": "0199a1f0-0000-7000-8000-000000000001",
              "jurisdiction_id": "0199a1f0-0000-7000-8000-000000000002"
            }
          },
          "UpdateEntityRequest": {
            "type": "object",
            "properties": {
              "name":            { "type": "string",
                                   "description": "Omit to leave unchanged; present must not be blank. Immutable for the firm anchor row." },
              "entity_type_id":  { "type": "string", "format": "uuid",
                                   "description": "Omit to leave unchanged. An existing `/app/api/entity-types` row." },
              "jurisdiction_id": { "type": "string", "format": "uuid",
                                   "description": "Omit to leave unchanged. An existing `/app/api/jurisdictions` row." }
            },
            "example": {
              "name": "Example Holdings LLC",
              "entity_type_id": "0199a1f0-0000-7000-8000-000000000001",
              "jurisdiction_id": "0199a1f0-0000-7000-8000-000000000002"
            }
          },
          "Jurisdiction": {
            "type": "object",
            "required": ["id", "name", "code", "jurisdiction_type", "inserted_at", "updated_at"],
            "properties": {
              "id":          { "type": "string", "format": "uuid" },
              "name":        { "type": "string" },
              "code":        { "type": "string",
                               "description": "Short code, e.g. `NV`, `CA`, `US`." },
              "jurisdiction_type": { "type": "string",
                                     "enum": ["state", "country"],
                                     "description": "`state` (US state or DC) or `country` (federal sovereign)." },
              "inserted_at": { "type": "string" },
              "updated_at":  { "type": "string" }
            }
          },
          "EntityType": {
            "type": "object",
            "required": ["id", "name", "inserted_at", "updated_at"],
            "properties": {
              "id":          { "type": "string", "format": "uuid" },
              "name":        { "type": "string" },
              "inserted_at": { "type": "string" },
              "updated_at":  { "type": "string" }
            }
          },
          "ValidateRequest": {
            "type": "object",
            "required": ["contents"],
            "properties": {
              "contents":      { "type": "string",
                                 "description": "Raw markdown body, including any YAML frontmatter." },
              "path":          { "type": "string",
                                 "description": "Pretend filename so rules that key off the path \
                                                 (e.g. N103 snake_case) have something to read. \
                                                 Defaults to `template.md`." },
              "markdown_only": { "type": "boolean",
                                 "description": "Lint with `navigator_markdown_only_rules` instead \
                                                 of the default Neon Law Navigator notation set." }
            },
            "example": {
              "contents":
                "---\nkind: trust\ntitle: Trust\nrespondent_type: entity\nconfidential: false\n\
                 questionnaire:\n  BEGIN:\n    _: END\n  END: {}\n\
                 workflow:\n  BEGIN:\n    next: lawyer_review\n  \
                 lawyer_review:\n    next: onchain\n  \
                 onchain:\n    next: END\n  END: {}\n---\n\nBody.\n",
              "path": "trust.md"
            }
          },
          "ValidateResponse": {
            "type": "object",
            "required": ["path", "clean", "violations"],
            "properties": {
              "path":       { "type": "string" },
              "clean":      { "type": "boolean" },
              "violations": { "type": "array",
                              "items": { "$ref": "#/components/schemas/ValidationViolation" } }
            },
            "example": { "path": "trust.md", "clean": true,
                         "violations": [ { "code": "N112", "line": 15,
                           "message": "workflow step `onchain` is allowed but its automation is not built yet (from state `onchain`)" } ] }
          },
          "ValidationViolation": {
            "type": "object",
            "required": ["code", "line", "message"],
            "properties": {
              "code":    { "type": "string", "description": "Rule code, e.g. `S101`, `N101`." },
              "line":    { "type": "integer", "format": "int32" },
              "message": { "type": "string" }
            }
          }
        }
      }
    })
}

/// Every `/app/api/*` path key declared in [`document`]. Public so the
/// drift test in `web/tests/openapi_drift.rs` can compare it against
/// the routes registered in [`crate::api::routes`].
#[must_use]
pub fn documented_paths() -> Vec<String> {
    let doc = document();
    doc["paths"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// Every operation declared in [`document`] as an uppercase
/// `(HTTP method, path)` pair — one entry per method key under each
/// path. This is the operation-level view of the document that
/// `web/tests/openapi_drift.rs` compares against
/// [`crate::api::documented_api_operations`], so the drift guard is
/// path+method, not path-only: an undocumented method on an
/// already-documented path is now caught.
#[must_use]
pub fn documented_operations() -> Vec<(String, String)> {
    let doc = document();
    let mut ops = Vec::new();
    if let Some(paths) = doc["paths"].as_object() {
        for (path, methods) in paths {
            if let Some(methods) = methods.as_object() {
                for verb in methods.keys() {
                    ops.push((verb.to_uppercase(), path.clone()));
                }
            }
        }
    }
    ops
}

/// MCP tools that legitimately carry no `x-mcp-tool` annotation, each
/// with the reason it has no `/app/api` twin. An entry here is a
/// decision; the absence of an entry is what
/// `every_tool_names_an_api_operation` refuses.
///
/// Keep this list short. The orphan guard is the ratchet that keeps a new
/// tool going through the same command layer the API route uses instead
/// of reaching into `store::` on its own — an exemption opts a tool out
/// of that pressure, so it needs a reason a reader can disagree with.
pub const TOOLS_WITHOUT_AN_API_OPERATION: &[(&str, &str)] = &[
    (
        "project_skill_list",
        "No route today. Project Skills are compiled reference data; the CLI and MCP share rules::project_skill_catalog.",
    ),
    (
        "project_skill_show",
        "No route today. A playbook is read from the same compiled catalog as navigator project skill show.",
    ),
    (
        "glossary_list",
        "No route today. The glossary is public reference data compiled into \
         the binary; the `/glossary` page and `navigator glossary list` read \
         the same embedded terms, so there is no command for an API \
         operation to share.",
    ),
    (
        "glossary_show",
        "No route today. One term's definition is the same embedded entry \
         the `/glossary` page anchors and `navigator glossary show` prints, \
         so there is no command for an API operation to share.",
    ),
    (
        "list_tools",
        "Protocol, not capability: it enumerates the catalog itself. The \
         OpenAPI document is the API surface's own equivalent, so an \
         operation for it would be circular.",
    ),
    (
        "bulk_import",
        "No route today. Bulk contact loading is agent-only; the API door \
         exposes single-record `POST /app/api/people` and nothing that \
         takes a batch.",
    ),
    (
        "spawn_legal_council",
        "No route today. The council is an authoring aid that renders a \
         review inline and writes nothing, so there is no command for an \
         API operation to share.",
    ),
    (
        "list_deadlines",
        "No route today. Deadline reads are an MCP-only aggregate over the \
         existing Project-scoped deadline query, so there is no API operation \
         for the tool to share.",
    ),
    (
        "project_status",
        "No route today. It is an MCP-only aggregate over three existing \
         Project-scoped queries (deadlines, notation events, participation), \
         so there is no single API operation for the tool to share.",
    ),
    (
        "delete_closed_repository",
        "No route today. Deleting a forge repository is a supervised, \
         confirmation-gated destructive act with no HTTP-side command to \
         share — there is no `DELETE /app/api/projects/{id}/repository` — \
         and adding one would give a second, un-gated door onto the same \
         irreversible operation.",
    ),
];

/// Every `x-mcp-tool` value in [`document`], paired with the operation
/// carrying it, as `(tool, METHOD, path)`.
#[must_use]
pub fn annotated_mcp_tools() -> Vec<(String, String, String)> {
    let doc = document();
    let mut out = Vec::new();
    if let Some(paths) = doc["paths"].as_object() {
        for (path, methods) in paths {
            if let Some(methods) = methods.as_object() {
                for (verb, op) in methods {
                    if let Some(tool) = op.get("x-mcp-tool").and_then(|v| v.as_str()) {
                        out.push((tool.to_string(), verb.to_uppercase(), path.clone()));
                    }
                }
            }
        }
    }
    out
}

/// Firm journal commands share the Firm capability resolver and require authenticated admin-tier access.
fn add_journal_operations(document: &mut Value) {
    for (suffix, method, id, summary) in [
        (
            "/audits",
            "get",
            "firmJournalAudits",
            "Read bounded reconciliation audit history and checkpoints",
        ),
        (
            "/preview",
            "post",
            "previewFirmJournal",
            "Validate a balanced posting and evidence without persistence",
        ),
        (
            "/migration-triage",
            "get",
            "firmJournalMigrationTriage",
            "Read legacy journal timestamp gaps requiring evidence",
        ),
        (
            "/statement",
            "get",
            "firmJournalStatement",
            "Read separate account and asset balances for a Firm or Project",
        ),
        (
            "/signals",
            "get",
            "readJournalSignals",
            "Read verified reconciliation wake-up signals after a cursor",
        ),
        (
            "/solana",
            "post",
            "readSolanaEvidence",
            "Read one finalized Solana instruction or network fee into private evidence",
        ),
        (
            "/xero",
            "post",
            "readXeroEvidence",
            "Read one explicit Xero accounting object into private evidence",
        ),
        (
            "",
            "get",
            "listFirmJournal",
            "Read the Firm-owned immutable journal",
        ),
        (
            "",
            "post",
            "postFirmJournal",
            "Post a balanced exact-quantity Journal Entry",
        ),
        (
            "/reconcile",
            "post",
            "reconcileFirmJournal",
            "Reconcile explicit linked sources with bound repair approvals",
        ),
        (
            "/mercury",
            "post",
            "readMercuryEvidence",
            "Read one Mercury transaction and file private evidence",
        ),
    ] {
        let path = format!("/app/api/firms/{{id}}/journal{suffix}");
        document["paths"][&path][method] = json!({
            "operationId": id, "summary": summary, "tags": ["Firms"],
            "parameters": [{"name":"id","in":"path","required":true,"schema":{"type":"string","format":"uuid"}}],
            "responses": {"401":{"description":"Authenticated session required"},"403":{"description":"Firm journal capability required"},"200":{"description":"Read or reconciliation result"},"201":{"description":"Immutable entry identity"},"404":{"description":"Firm unavailable to this actor"},"409":{"description":"Idempotency key content conflicts"},"422":{"description":"Journal invariant refused"},"502":{"description":"Bank evidence unavailable"}}
        });
        if method == "post" {
            document["paths"][&path][method]["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":{"type":"object"}}}});
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        base_url_for, document, document_with_base, documented_operations, documented_paths,
    };

    #[test]
    fn the_finance_operations_are_documented() {
        let ops = documented_operations();
        for (method, path) in [
            ("GET", "/app/api/projects/{id}/finance"),
            ("POST", "/app/api/projects/{id}/finance/run"),
        ] {
            assert!(ops.contains(&(method.to_string(), path.to_string())));
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)] // one assertion per documented operation
    fn documented_operations_expands_paths_into_method_pairs() {
        let ops = documented_operations();
        // Every documented path/method pair is present, uppercased.
        assert!(ops.contains(&("GET".to_string(), "/app/api/people".to_string())));
        assert!(ops.contains(&("POST".to_string(), "/app/api/people".to_string())));
        assert!(ops.contains(&("PATCH".to_string(), "/app/api/people/{id}".to_string())));
        assert!(ops.contains(&("DELETE".to_string(), "/app/api/people/{id}".to_string())));
        assert!(ops.contains(&("POST".to_string(), "/app/api/entities".to_string())));
        assert!(ops.contains(&("PATCH".to_string(), "/app/api/brands/{key}".to_string())));
        assert!(ops.contains(&("PATCH".to_string(), "/app/api/entities/{id}".to_string())));
        assert!(ops.contains(&("DELETE".to_string(), "/app/api/entities/{id}".to_string())));
        assert!(ops.contains(&("PATCH".to_string(), "/app/api/projects/{id}".to_string())));
        assert!(ops.contains(&(
            "POST".to_string(),
            "/app/api/project-surfaces/{id}".to_string()
        )));
        assert!(ops.contains(&(
            "POST".to_string(),
            "/app/api/projects/{id}/participants".to_string()
        )));
        assert!(ops.contains(&(
            "POST".to_string(),
            "/app/api/projects/{id}/relationships/adverse".to_string()
        )));
        assert!(ops.contains(&(
            "PATCH".to_string(),
            "/app/api/projects/{id}/participants/{role_id}".to_string()
        )));
        assert!(ops.contains(&(
            "DELETE".to_string(),
            "/app/api/projects/{id}/participants/{role_id}".to_string()
        )));
        assert!(ops.contains(&(
            "GET".to_string(),
            "/app/api/projects/{id}/notation-inventory".to_string()
        )));
        assert!(ops.contains(&(
            "POST".to_string(),
            "/app/api/projects/{id}/notations".to_string()
        )));
        assert!(ops.contains(&(
            "GET".to_string(),
            "/app/api/notations/{id}/answers".to_string()
        )));
        assert!(ops.contains(&(
            "POST".to_string(),
            "/app/api/notations/{id}/answers".to_string()
        )));
        assert!(ops.contains(&(
            "POST".to_string(),
            "/app/api/notations/{id}/intake".to_string()
        )));
        assert!(ops.contains(&(
            "POST".to_string(),
            "/app/api/notations/{id}/approval".to_string()
        )));
        assert!(ops.contains(&(
            "POST".to_string(),
            "/app/api/notations/{id}/signature".to_string()
        )));
        assert!(ops.contains(&(
            "POST".to_string(),
            "/app/api/review-documents/{id}/comments".to_string()
        )));
        assert!(ops.contains(&(
            "POST".to_string(),
            "/app/api/documents/{id}/deletion-requests".to_string()
        )));
        assert!(ops.contains(&(
            "POST".to_string(),
            "/app/api/notations/{id}/clauses".to_string()
        )));
        assert!(ops.contains(&(
            "PATCH".to_string(),
            "/app/api/notations/{id}/clauses/{clause_id}".to_string()
        )));
        assert!(ops.contains(&(
            "DELETE".to_string(),
            "/app/api/notations/{id}/clauses/{clause_id}".to_string()
        )));
        assert!(ops.contains(&(
            "POST".to_string(),
            "/app/api/notations/{id}/clauses/{clause_id}/move".to_string()
        )));
        assert!(ops.contains(&(
            "POST".to_string(),
            "/app/api/projects/{id}/contract-review".to_string()
        )));
        assert!(ops.contains(&(
            "POST".to_string(),
            "/app/api/templates/validate".to_string()
        )));
        assert!(ops.contains(&("POST".to_string(), "/app/api/drafts/validate".to_string())));
        // A GET-only path expands to exactly one operation.
        let jurisdiction_ops: Vec<_> = ops
            .iter()
            .filter(|(_, p)| p == "/app/api/jurisdictions")
            .collect();
        assert_eq!(
            jurisdiction_ops,
            vec![&("GET".to_string(), "/app/api/jurisdictions".to_string())]
        );
        // Methods are always uppercase (the drift guard compares to
        // `api::documented_api_operations`, which is uppercase).
        assert!(ops
            .iter()
            .all(|(m, _)| m.chars().all(|c| c.is_ascii_uppercase())));
    }

    #[test]
    fn base_url_derives_https_from_request_host() {
        // No NAV_BASE_URL set in the test process: a real request host
        // drives the scheme + servers URL, so prod surfaces its own
        // domain without any hard-coded value in source.
        assert_eq!(
            base_url_for(Some("www.neonlaw.com")),
            "https://www.neonlaw.com"
        );
        assert_eq!(
            base_url_for(Some("localhost:8080")),
            "http://localhost:8080"
        );
    }

    #[test]
    fn base_url_falls_back_to_placeholder_without_host() {
        assert_eq!(base_url_for(None), super::PLACEHOLDER_BASE_URL);
    }

    #[test]
    fn document_with_base_threads_host_into_servers_and_contact() {
        let d = document_with_base("https://www.neonlaw.com");
        assert_eq!(d["servers"][0]["url"], "https://www.neonlaw.com");
        assert_eq!(d["info"]["contact"]["url"], "https://www.neonlaw.com");
    }

    #[test]
    fn document_has_openapi_version_and_paths() {
        let d = document();
        assert_eq!(d["openapi"], "3.1.0");
        assert!(d["paths"]["/app/api/people"].is_object());
        assert!(d["paths"]["/app/api/people/{id}"].is_object());
        assert!(d["paths"]["/app/api/entities"].is_object());
        assert!(d["paths"]["/app/api/jurisdictions"].is_object());
        assert!(d["paths"]["/app/api/entity-types"].is_object());
        assert!(d["paths"]["/app/api/templates/validate"]["post"].is_object());
    }

    #[test]
    fn document_declares_each_schema() {
        let d = document();
        let schemas = &d["components"]["schemas"];
        for name in [
            "Person",
            "PersonRole",
            "CreatePersonRequest",
            "ApiError",
            "Entity",
            "CreateEntityRequest",
            "UpdateEntityRequest",
            "Project",
            "OpenMatterRequest",
            "UpdateProjectRequest",
            "TransitionProjectRequest",
            "RecordAdversityRequest",
            "AddParticipantRequest",
            "UpdateParticipantRequest",
            "Participation",
            "CreateNotationRequest",
            "CreateNotationResponse",
            "AnswerStepRequest",
            "NotationStep",
            "NotationQuestion",
            "QuestionChoice",
            "SendIntakeResponse",
            "NotationLifecycleResponse",
            "NotationSignatureResponse",
            "AddReviewCommentRequest",
            "AddReviewCommentResponse",
            "DeletionRequestResponse",
            "AddClauseRequest",
            "AddClauseResponse",
            "MoveClauseRequest",
            "ContractReviewResponse",
            "Jurisdiction",
            "EntityType",
            "ValidateRequest",
            "ValidateResponse",
            "ValidationViolation",
        ] {
            assert!(schemas[name].is_object(), "missing schema {name}");
        }
    }

    #[test]
    fn create_notation_request_requires_exactly_one_code_spelling() {
        let schema = document()["components"]["schemas"]["CreateNotationRequest"].clone();
        assert_eq!(schema["required"][0], "client_email");
        assert_eq!(schema["required"].as_array().unwrap().len(), 1);
        assert!(schema["properties"]["template_code"].is_object());
        assert!(schema["properties"]["draft_code"].is_object());
        assert!(schema["properties"]["client_email"].is_object());
        let branches = schema["oneOf"].as_array().expect("exactly-one oneOf");
        assert_eq!(branches.len(), 2);
        let answer = document()["components"]["schemas"]["AnswerStepRequest"]["properties"].clone();
        assert!(answer.get("template_code").is_none());
        assert!(answer.get("draft_code").is_none());
        let validate = document()["components"]["schemas"]["ValidateRequest"]["properties"].clone();
        assert!(validate.get("template_code").is_none());
        assert!(validate.get("draft_code").is_none());
    }

    #[test]
    fn id_schemas_are_uuid_strings_not_int32() {
        let d = document();
        for entity in ["Person", "Entity", "Jurisdiction", "EntityType"] {
            let id = &d["components"]["schemas"][entity]["properties"]["id"];
            assert_eq!(id["type"], "string", "{entity}.id should be string");
            assert_eq!(id["format"], "uuid", "{entity}.id should be uuid");
        }
        for path in ["/app/api/people/{id}", "/app/api/entities/{id}"] {
            let params = &d["paths"][path]["get"]["parameters"];
            let id_schema = &params[0]["schema"];
            assert_eq!(id_schema["type"], "string", "{path} id should be string");
            assert_eq!(id_schema["format"], "uuid", "{path} id should be uuid");
        }
    }

    #[test]
    fn top_level_security_requires_auth() {
        let d = document();
        let sec = d["security"].as_array().expect("top-level security array");
        assert!(
            !sec.is_empty(),
            "`/app/api/*` requires OIDC; the OpenAPI doc must declare it at the top level"
        );
        let has_bearer = sec.iter().any(|req| {
            req.as_object()
                .is_some_and(|m| m.contains_key("bearerAuth"))
        });
        let has_cookie = sec.iter().any(|req| {
            req.as_object()
                .is_some_and(|m| m.contains_key("sessionCookie"))
        });
        assert!(
            has_bearer,
            "bearerAuth must be one of the documented schemes"
        );
        assert!(
            has_cookie,
            "sessionCookie must be one of the documented schemes"
        );
    }

    #[test]
    fn no_operation_overrides_security_to_empty() {
        let d = document();
        let paths = d["paths"].as_object().expect("paths object");
        for (path, methods) in paths {
            for (verb, op) in methods.as_object().expect("methods object") {
                let sec = &op["security"];
                assert!(
                    sec.is_null(),
                    "{verb} {path} must inherit the top-level `security` requirement \
                     (no per-op override); got {sec}"
                );
            }
        }
    }

    #[test]
    fn mutating_api_operations_document_authz_failures() {
        let d = document();
        let paths = d["paths"].as_object().expect("paths object");
        for (path, methods) in paths {
            for verb in ["post", "patch", "put", "delete"] {
                let Some(op) = methods.get(verb) else {
                    continue;
                };
                let responses = op["responses"]
                    .as_object()
                    .unwrap_or_else(|| panic!("{verb} {path} must declare OpenAPI responses"));
                assert!(
                    responses.contains_key("401"),
                    "{verb} {path} must document the unauthenticated response; API writes are never anonymous"
                );
                // Every write must document how it rejects an authenticated but
                // non-authorized caller. A lawyer-tier-gated door does that with
                // 403 (wrong tier). A client-writable door (any authenticated
                // caller is admitted, then a per-matter scope check runs) does
                // it with 404 — an out-of-scope caller is told the resource does
                // not exist rather than that they lack access. Either satisfies
                // the invariant; a write documenting neither is the bug.
                assert!(
                    responses.contains_key("403") || responses.contains_key("404"),
                    "{verb} {path} must document its authenticated-but-not-authorized rejection (403 for a tier gate, or 404 for a non-disclosing scope gate)"
                );
            }
        }
    }

    #[test]
    fn bearer_and_cookie_schemes_are_declared() {
        let d = document();
        let bearer = &d["components"]["securitySchemes"]["bearerAuth"];
        assert_eq!(bearer["type"], "http");
        assert_eq!(bearer["scheme"], "bearer");
        let cookie = &d["components"]["securitySchemes"]["sessionCookie"];
        assert_eq!(cookie["type"], "apiKey");
        assert_eq!(cookie["in"], "cookie");
        assert_eq!(cookie["name"], "navigator_session");
    }

    #[test]
    fn documented_paths_matches_paths_object() {
        let d = document();
        let mut from_obj: Vec<String> = d["paths"].as_object().unwrap().keys().cloned().collect();
        from_obj.sort();
        let mut from_helper = documented_paths();
        from_helper.sort();
        assert_eq!(from_obj, from_helper);
    }

    #[test]
    fn validate_request_example_is_itself_clean() {
        // The example shipped in the OpenAPI doc is what Swagger's "Try
        // it out" pre-fills. It must lint clean under the default rule
        // set, or the first request a caller sends comes back dirty —
        // a confusing first impression of an endpoint whose whole job
        // is linting. This guards the example against rule drift.
        let d = document();
        let ex = &d["components"]["schemas"]["ValidateRequest"]["example"];
        let contents = ex["contents"].as_str().expect("example.contents string");
        let path = ex["path"].as_str().expect("example.path string");
        let file = rules::SourceFile {
            path: std::path::PathBuf::from(path),
            contents: contents.to_string(),
        };
        let error_codes: Vec<&str> = rules::navigator_default_rules()
            .iter()
            .flat_map(|r| r.lint(&file))
            .filter(|v| rules::severity_for_code(v.code) == rules::Severity::Error)
            .map(|v| v.code)
            .collect();
        // The example must carry no *blocking* errors. Its `onchain` step
        // earns the yellow N112 advisory, which is expected and
        // non-blocking; its mandatory `lawyer_review` gate earns none.
        assert!(
            error_codes.is_empty(),
            "OpenAPI ValidateRequest example must lint free of errors; got {error_codes:?}"
        );
    }

    #[test]
    fn mcp_is_intentionally_absent() {
        let d = document();
        assert!(
            d["paths"]["/mcp"].is_null(),
            "/mcp is JSON-RPC and out of scope for this OpenAPI doc"
        );
    }

    /// Direction 1: every `x-mcp-tool` names a real tool. Catches a typo
    /// or a reference left behind by a renamed tool.
    #[test]
    fn every_x_mcp_tool_names_a_real_tool() {
        let catalog: Vec<String> = mcp::tools::list_tools()
            .iter()
            .filter_map(|t| t["name"].as_str().map(String::from))
            .collect();
        let annotated = super::annotated_mcp_tools();
        assert!(!annotated.is_empty(), "the annotations went missing");
        for (tool, verb, path) in &annotated {
            assert!(
                catalog.contains(tool),
                "`{verb} {path}` names `{tool}`, which is not in mcp::tools::list_tools(); \
                 got catalog {catalog:?}"
            );
        }
    }

    /// Direction 2, and the point of the pair: every tool in the catalog
    /// is named by at least one operation, or is exempt with a reason.
    ///
    /// This is the ratchet. A new tool cannot merge without either naming
    /// its API operation — which is the pressure that keeps it going
    /// through the same command layer rather than reaching into `store::`
    /// on its own — or being written down here as a deliberate exception.
    #[test]
    fn every_tool_names_an_api_operation() {
        let annotated: Vec<String> = super::annotated_mcp_tools()
            .into_iter()
            .map(|(tool, _, _)| tool)
            .collect();
        let exempt: Vec<&str> = super::TOOLS_WITHOUT_AN_API_OPERATION
            .iter()
            .map(|(name, _)| *name)
            .collect();

        for descriptor in mcp::tools::list_tools() {
            let name = descriptor["name"].as_str().unwrap();
            assert!(
                annotated.iter().any(|t| t == name) || exempt.contains(&name),
                "`{name}` is in the MCP catalog but no OpenAPI operation carries \
                 `x-mcp-tool: {name}`. Annotate the operation it shares a command with, \
                 or add it to TOOLS_WITHOUT_AN_API_OPERATION with the reason it has none."
            );
        }
    }

    /// An exemption must be a decision, not an omission: it has to name a
    /// real tool and carry a written reason.
    #[test]
    fn every_exemption_names_a_real_tool_and_gives_a_reason() {
        let catalog: Vec<String> = mcp::tools::list_tools()
            .iter()
            .filter_map(|t| t["name"].as_str().map(String::from))
            .collect();
        let annotated: Vec<String> = super::annotated_mcp_tools()
            .into_iter()
            .map(|(tool, _, _)| tool)
            .collect();

        for (name, reason) in super::TOOLS_WITHOUT_AN_API_OPERATION {
            assert!(
                catalog.contains(&(*name).to_string()),
                "`{name}` is exempted but is not a tool in the catalog"
            );
            assert!(
                reason.len() > 30,
                "`{name}`'s exemption reason is too thin to argue with: `{reason}`"
            );
            assert!(
                !annotated.iter().any(|t| t == name),
                "`{name}` is both exempted and annotated — drop the exemption"
            );
        }
    }

    /// The annotation is on the operation, so a tool inherits that route's
    /// documented authorization failures. Spot-check the shape holds for a
    /// mutating twin rather than trusting the placement.
    #[test]
    fn an_annotated_mutating_operation_still_documents_its_authz_failures() {
        let doc = document();
        let op = &doc["paths"]["/app/api/people/{id}/welcome"]["post"];
        assert_eq!(op["x-mcp-tool"], "send_welcome_email");
        assert!(
            op["responses"]["403"].is_object() || op["responses"]["401"].is_object(),
            "expected a documented authz failure on the annotated operation: {op}"
        );
    }

    /// Draft is Template: the validator is documented under both names, with
    /// the same request and response shapes and security, its own operation
    /// id, and no MCP tool of its own (the canonical operation names it).
    #[test]
    fn the_draft_validator_twin_is_documented_beside_the_canonical_operation() {
        let doc = document();
        let canonical = &doc["paths"]["/app/api/templates/validate"]["post"];
        let twin = &doc["paths"]["/app/api/drafts/validate"]["post"];
        assert!(twin.is_object(), "the twin is documented");
        assert_eq!(canonical["operationId"], "validateTemplate");
        assert_eq!(twin["operationId"], "validateDraft");
        assert_eq!(canonical["requestBody"], twin["requestBody"]);
        assert_eq!(canonical["responses"], twin["responses"]);
        assert_eq!(canonical["x-mcp-tool"], "validate_notation");
        assert!(twin.get("x-mcp-tool").is_none(), "one tool, one name");
        assert!(twin["summary"].as_str().unwrap().contains("Draft"));
    }

    /// Operation ids are the one identifier an OpenAPI client generates
    /// code from, so no two operations may share one.
    #[test]
    fn operation_ids_are_unique() {
        let doc = document();
        let mut seen = std::collections::BTreeSet::new();
        for (path, item) in doc["paths"].as_object().unwrap() {
            for (method, operation) in item.as_object().unwrap() {
                if let Some(id) = operation
                    .get("operationId")
                    .and_then(serde_json::Value::as_str)
                {
                    assert!(seen.insert(id.to_string()), "{method} {path} repeats {id}");
                }
            }
        }
        assert!(seen.contains("validateDraft") && seen.contains("validateTemplate"));
    }
}
