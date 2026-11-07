//! The self-service `/app/profile` page — every authenticated tier, Client
//! included, updates their own avatar here through the native `POST
//! /app/avatar` multipart form. `avatar-upload.js` replays that submit as
//! `fetch` so the page stays put and the `/app/me/avatar` preview updates
//! in place; without JavaScript the form still posts and the handler
//! redirects back here. The upload is a sibling of `/app/profile`, not a
//! nested child: a nested `/app/profile/avatar` action resolves in the
//! browser as `/app/avatar` (the last path segment is replaced). No tier
//! gate at all: unlike the admin-only `/app/admin/people/{id}/avatar`, the
//! target is always the caller's own row, resolved server-side from the
//! signed session, never a person id supplied by the page. Firm-tier avatars
//! become publicly available through the deployment's public-assets origin;
//! client avatars stay on the private documents lane.
//!
//! Email renders read-only: the account's mailbox is also its sign-in
//! identity, so a self-service edit here would drift from the OIDC identity
//! the session was minted against. The disabled field carries a note to
//! contact the firm instead.
//!
//! A Firm-tier viewer also manages their own SSH public keys here (ENG-1219):
//! the card lists each key by label and fingerprint, posts a new key to
//! `POST /app/profile/ssh-keys`, and removes one through
//! `POST /app/profile/ssh-keys/{id}/delete`. Navigator reconciles the keys
//! onto the viewer's Forgejo account after each change. A client has no Firm
//! membership and sees no card.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{Field, FormCard, Heading};
use crate::people::ViewerRole;

/// The profile page path.
pub const PROFILE_PATH: &str = "/app/profile";
/// The native multipart upload the page's avatar card posts to. Sibling of
/// [`PROFILE_PATH`] so a relative resolution from that page still lands here.
pub const PROFILE_AVATAR_PATH: &str = "/app/avatar";

/// The SSH key card's native add form posts here.
pub const PROFILE_SSH_KEYS_PATH: &str = "/app/profile/ssh-keys";

/// The signature card's native form posts here. Sibling of [`PROFILE_PATH`]
/// so a relative resolution from that page still lands here.
pub const PROFILE_MAIL_SIGNATURE_PATH: &str = "/app/profile/mail-signature";

/// The `?ssh_key=` / `?signature=` outcome the profile page's forms redirect
/// back with — fixed codes, never the submitted text.
#[derive(Deserialize, Serialize, Clone, Default, PartialEq, Eq)]
pub struct ProfileQuery {
    #[serde(default)]
    pub ssh_key: Option<String>,
    #[serde(default)]
    pub ssh_key_error: Option<String>,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(default)]
    pub signature_error: Option<String>,
}

/// The signature card: the attorney's stored block plus the outcome flash.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct SignatureCard {
    /// The stored signature, prefilled into the form. Empty when none is set
    /// yet — the attorney starts from a blank block, not an error.
    pub body: String,
    /// `false` when the signature store could not be read; the card says so
    /// rather than offering a form that would silently overwrite.
    pub available: bool,
    pub notice: Option<String>,
    pub error: Option<String>,
}

/// The sentence for a `?signature=` outcome code.
#[cfg_attr(not(feature = "server"), allow(dead_code))]
fn signature_notice(code: &str) -> Option<&'static str> {
    match code {
        "saved" => Some("Signature saved."),
        _ => None,
    }
}

/// The sentence for a `?signature_error=` code.
#[cfg_attr(not(feature = "server"), allow(dead_code))]
fn signature_error_sentence(code: &str) -> &'static str {
    match code {
        "invalid" => {
            "The signature could not be saved. It must not be blank, must be at most \
             2000 characters, and must not contain a `--` delimiter line — \
             Navigator adds the delimiter itself."
        }
        "not_found" => {
            "Your signature was not saved: your account has no linked Navigator profile."
        }
        "unavailable" => "Signatures are unavailable right now. Try again later.",
        _ => "The signature could not be saved.",
    }
}

/// One of the viewer's registered SSH public keys, as the card lists it.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct SshKeyRow {
    pub id: String,
    pub label: String,
    pub algorithm: String,
    pub fingerprint: String,
    pub created_on: String,
    /// `None` until the key has reached the viewer's Forgejo account.
    pub synced_on: Option<String>,
}

/// The SSH key card: the viewer's keys plus the outcome flash.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct SshKeysCard {
    pub keys: Vec<SshKeyRow>,
    /// `false` when the key store could not be read; the card says so
    /// rather than showing an empty list.
    pub available: bool,
    pub notice: Option<String>,
    pub error: Option<String>,
}

/// The sentence for a `?ssh_key=` outcome code.
#[cfg_attr(not(feature = "server"), allow(dead_code))]
fn ssh_key_notice(code: &str) -> Option<&'static str> {
    match code {
        "added" => Some("SSH key added."),
        "removed" => Some("SSH key removed."),
        _ => None,
    }
}

/// The sentence for a `?ssh_key_error=` code.
#[cfg_attr(not(feature = "server"), allow(dead_code))]
fn ssh_key_error(code: &str) -> &'static str {
    match code {
        "invalid" => {
            "That is not an accepted SSH public key. Paste one ed25519 or ECDSA public key \
             (the .pub file), never the private key."
        }
        "duplicate" => "That SSH key is already registered.",
        "not_member" => "Only a Firm member can register an SSH key.",
        "not_found" => "That SSH key was not found.",
        "unavailable" => "SSH keys are unavailable right now. Try again later.",
        _ => "The SSH key could not be saved.",
    }
}

/// The rendered self-service profile page.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct ProfileView {
    /// The resolved brand's tokens stylesheet href, so the page wears its own
    /// palette rather than the firm's on a non-default host.
    #[serde(default)]
    pub tokens_href: String,
    /// The deploy's firm name, for the document title.
    #[serde(default)]
    pub firm_name: String,
    pub role: ViewerRole,
    /// `None` when the session carries no linked person — unreachable outside
    /// a test harness edge case, since every route this page mounts on
    /// requires an authenticated session with a linked `persons` row.
    pub fields: Option<ProfileFields>,
    /// The deploy's brand mark for the navbar. `None` when the mounted brand
    /// configures none.
    #[serde(default)]
    pub logo: Option<crate::components::AppLogo>,
}

/// The caller's own prefilled fields.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct ProfileFields {
    pub name: String,
    pub email: String,
    pub csrf_token: String,
    /// Public availability applies only to firm-tier avatars. This derives
    /// from the current `person.role`, not a potentially stale session role.
    pub avatar_is_public: bool,
    /// The SSH key card, for a Firm-tier viewer only.
    #[serde(default)]
    pub ssh_keys: Option<SshKeysCard>,
    /// The signature card, for a Firm-tier viewer only — the attorney's own
    /// mail signature (LAW-280).
    #[serde(default)]
    pub signature: Option<SignatureCard>,
}

/// Read the viewer's SSH keys for the card. A store that cannot be reached
/// yields an unavailable card, never a failed page.
#[cfg(feature = "server")]
async fn load_ssh_keys(person_id: uuid::Uuid, query: ProfileQuery) -> SshKeysCard {
    let notice = query
        .ssh_key
        .as_deref()
        .and_then(ssh_key_notice)
        .map(str::to_string);
    let error = query
        .ssh_key_error
        .as_deref()
        .map(|code| ssh_key_error(code).to_string());
    let listed = match store::postgres::PostgresDb::from_env().await {
        Ok(pg) => store::ssh_public_keys::list_for_person(&pg, person_id)
            .await
            .ok(),
        Err(_) => None,
    };
    let available = listed.is_some();
    let keys = listed
        .unwrap_or_default()
        .into_iter()
        .map(|key| SshKeyRow {
            id: key.id.to_string(),
            label: key.label,
            algorithm: key.algorithm,
            fingerprint: key.fingerprint,
            created_on: key.created_at.format("%Y-%m-%d").to_string(),
            synced_on: key
                .last_synced_at
                .map(|at| at.format("%Y-%m-%d").to_string()),
        })
        .collect();
    SshKeysCard {
        keys,
        available,
        notice,
        error,
    }
}

/// Read the viewer's mail signature for the card. A store that cannot be
/// reached yields an unavailable card, never a failed page — and never a
/// form that would overwrite a block the card could not read.
#[cfg(feature = "server")]
async fn load_signature(
    surreal: &store::surreal::SurrealDb,
    person_id: uuid::Uuid,
    query: ProfileQuery,
) -> SignatureCard {
    let notice = query
        .signature
        .as_deref()
        .and_then(signature_notice)
        .map(str::to_string);
    let error = query
        .signature_error
        .as_deref()
        .map(|code| signature_error_sentence(code).to_string());
    let stored = match store::postgres::PostgresDb::from_env().await {
        Ok(pg) => store::mail_signatures::for_person(surreal, &pg, person_id)
            .await
            .ok()
            .flatten(),
        Err(_) => None,
    };
    match stored {
        Some(signature) => SignatureCard {
            body: signature.body,
            available: true,
            notice,
            error,
        },
        None => SignatureCard {
            body: String::new(),
            available: false,
            notice,
            error,
        },
    }
}

#[cfg(feature = "server")]
async fn load_profile(role: ViewerRole) -> Result<ProfileView, ServerFnError> {
    let csrf_token = dioxus_fullstack_core::FullstackContext::extract::<
        axum::Extension<crate::csrf::CsrfToken>,
        _,
    >()
    .await
    .map(|axum::Extension(token)| token.0)
    .unwrap_or_default();

    let crate::portal_project_list::PersonId(person_id) =
        dioxus_fullstack_core::FullstackContext::extract::<
            axum::Extension<crate::portal_project_list::PersonId>,
            _,
        >()
        .await
        .map(|axum::Extension(id)| id)
        .unwrap_or_default();
    let person_id = person_id.and_then(|raw| raw.parse::<uuid::Uuid>().ok());

    let tokens_href = crate::app_chrome::app_tokens_href_from_context().await;
    let firm_name = crate::app_chrome::firm_name_from_context().await;
    let logo = crate::app_chrome::app_logo_from_context().await;

    let Some(person_id) = person_id else {
        return Ok(ProfileView {
            tokens_href,
            firm_name,
            role,
            fields: None,
            logo,
        });
    };

    let surreal = consume_context::<store::surreal::SurrealDb>();
    let person = store::persons::find_by_id(&surreal, person_id)
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    let Some(person) = person else {
        return Ok(ProfileView {
            tokens_href,
            firm_name,
            role,
            fields: None,
            logo,
        });
    };

    let firm_tier = !matches!(person.role, store::persons::Role::Client);
    let (ssh_keys, signature) = if firm_tier {
        let query = dioxus_fullstack_core::FullstackContext::extract::<
            axum::extract::Query<ProfileQuery>,
            _,
        >()
        .await
        .map(|axum::extract::Query(query)| query)
        .unwrap_or_default();
        (
            Some(load_ssh_keys(person_id, query.clone()).await),
            Some(load_signature(&surreal, person_id, query).await),
        )
    } else {
        (None, None)
    };

    Ok(ProfileView {
        tokens_href,
        firm_name,
        role,
        fields: Some(ProfileFields {
            name: person.name,
            email: person.email,
            csrf_token,
            avatar_is_public: firm_tier,
            ssh_keys,
            signature,
        }),
        logo,
    })
}

/// Load the profile page: every authenticated tier reaches this — there is no
/// role check here, unlike every other `get_*` loader in this crate.
#[server]
pub async fn get_profile() -> Result<ProfileView, ServerFnError> {
    let role = dioxus_fullstack_core::FullstackContext::extract::<axum::Extension<ViewerRole>, _>()
        .await
        .map(|axum::Extension(role)| role)
        .unwrap_or_default();
    load_profile(role).await
}

/// The `/app/profile` page.
#[component]
pub fn Profile() -> Element {
    let resource = use_server_future(get_profile)?;
    render_profile(&resource)
}

/// The avatar preview and self-service upload form, a sibling `FormCard` to
/// the read-only contact card below it — the two need different `enctype`s,
/// and `FormCard` only supports one per `<form>`.
fn avatar_upload_card(csrf_token: &str, avatar_is_public: bool) -> Element {
    let help = if avatar_is_public {
        "PNG or JPEG, up to 5 MB and 1024 × 1024 pixels. Replaces any existing avatar."
    } else {
        "PNG, JPEG, or WebP, up to 5 MB. Replaces any existing avatar."
    };
    rsx! {
        section { id: "profile-avatar", class: "person-avatar",
            h2 { "Avatar" }
            img {
                class: "person-avatar__preview",
                src: "/app/me/avatar",
                alt: "Your profile photo",
                width: "96",
                height: "96",
            }
            // In-place upload: `avatar-upload.js` finds this form via
            // `#profile-avatar` and `#profile-avatar-file`, posts the same
            // multipart body as a `fetch`, and cache-busts `/app/me/avatar`.
            document::Script { src: "/public/js/avatar-upload.js", defer: true }
            if avatar_is_public {
                p { class: "form-help", "Your avatar will be publicly available." }
            }
            FormCard {
                title: "Upload avatar".to_string(),
                action: PROFILE_AVATAR_PATH.to_string(),
                submit_label: "Upload".to_string(),
                heading: Heading::H2,
                multipart: true,
                csrf_token: Some(csrf_token.to_string()),
                fields: vec![
                    Field::file("Avatar", "file")
                        .id("profile-avatar-file")
                        .required()
                        .help(help),
                ],
            }
        }
    }
}

/// The read-only contact card: name and email, for context. Email is
/// disabled — greyed out — with a note to contact the firm instead of
/// offering a Save that would drift from the sign-in identity.
fn contact_card(fields: &ProfileFields) -> Element {
    rsx! {
        section { id: "profile-details",
            h2 { "Details" }
            FormCard {
                title: "Contact".to_string(),
                action: PROFILE_PATH.to_string(),
                submit_label: String::new(),
                heading: Heading::H2,
                read_only: true,
                fields: vec![
                    Field::text("Name", "name", fields.name.clone()).disabled(),
                    Field::email("Email", "email", fields.email.clone())
                        .disabled()
                        .help("Contact us to update your email address."),
                ],
            }
        }
    }
}

/// The SSH key card: the viewer's keys with a remove button each, then the
/// add form. Only the public key is ever asked for.
fn ssh_keys_card(card: &SshKeysCard, csrf_token: &str) -> Element {
    rsx! {
        section { id: "profile-ssh-keys",
            h2 { "SSH keys" }
            p { class: "nav-muted",
                "Navigator adds these keys to your account on the Firm's source forge, \
                 and removes them when you remove them here or leave the Firm."
            }
            if let Some(error) = card.error.as_ref() {
                p { class: "nav-form-error", role: "alert", "{error}" }
            }
            if let Some(notice) = card.notice.as_ref() {
                p { role: "status", "{notice}" }
            }
            if !card.available {
                p { "SSH keys are unavailable right now." }
            } else if card.keys.is_empty() {
                p { "You have no SSH keys yet." }
            } else {
                div { class: "nav-table-wrap",
                    table { class: "nav-table",
                        thead {
                            tr {
                                th { "Label" }
                                th { "Type" }
                                th { "Fingerprint" }
                                th { "Added" }
                                th { "On the forge" }
                                th { "" }
                            }
                        }
                        tbody {
                            for key in card.keys.iter().cloned() {
                                tr { class: "ssh-key-row", key: "{key.id}",
                                    td { "{key.label}" }
                                    td { "{key.algorithm}" }
                                    td { code { "{key.fingerprint}" } }
                                    td { "{key.created_on}" }
                                    td {
                                        match key.synced_on.as_ref() {
                                            Some(on) => rsx! { "Synced {on}" },
                                            None => rsx! { "Not yet" },
                                        }
                                    }
                                    td {
                                        form {
                                            class: "nav-form",
                                            method: "post",
                                            action: "{PROFILE_SSH_KEYS_PATH}/{key.id}/delete",
                                            "aria-label": "Remove {key.label}",
                                            input { r#type: "hidden", name: "_csrf", value: "{csrf_token}" }
                                            button { class: "nav-btn nav-btn--secondary", r#type: "submit", "Remove" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if card.available {
                FormCard {
                    title: "Add an SSH key".to_string(),
                    action: PROFILE_SSH_KEYS_PATH.to_string(),
                    submit_label: "Add key".to_string(),
                    heading: Heading::H2,
                    csrf_token: Some(csrf_token.to_string()),
                    fields: vec![
                        Field::textarea("Public key", "public_key", "", 3)
                            .id("profile-ssh-key")
                            .required()
                            .help("Paste one ed25519 or ECDSA public key, the contents of your .pub file. Never paste a private key."),
                        Field::text("Label", "label", "")
                            .maxlength(100)
                            .help("Optional. Defaults to the key's comment."),
                    ],
                }
            }
        }
    }
}

/// The signature card: the attorney's own mail signature, prefilled from the
/// stored row, saving through the self-service route. A store the page could
/// not read keeps the form off the card, so a save cannot overwrite a block
/// the card never saw.
fn signature_card(card: &SignatureCard, csrf_token: &str) -> Element {
    rsx! {
        section { id: "profile-signature",
            h2 { "Email signature" }
            p { class: "nav-muted",
                "The block that signs your Gmail drafts. Navigator adds the `-- ` delimiter \
                 line above it when drafting, so never include the delimiter here."
            }
            if let Some(error) = card.error.as_ref() {
                p { class: "nav-form-error", role: "alert", "{error}" }
            }
            if let Some(notice) = card.notice.as_ref() {
                p { role: "status", "{notice}" }
            }
            if !card.available {
                p { "Your signature is unavailable right now. Try again later." }
            } else {
                FormCard {
                    title: "Email signature".to_string(),
                    action: PROFILE_MAIL_SIGNATURE_PATH.to_string(),
                    submit_label: "Save signature".to_string(),
                    heading: Heading::H2,
                    csrf_token: Some(csrf_token.to_string()),
                    fields: vec![
                        Field::textarea("Signature", "body", card.body.clone(), 6)
                            .id("profile-signature-body")
                            .help("At most 2000 characters."),
                    ],
                }
            }
        }
    }
}

/// Render the resolved profile page.
fn render_profile(resource: &Resource<Result<ProfileView, ServerFnError>>) -> Element {
    let view = match &*resource.read() {
        Some(Ok(view)) => view.clone(),
        Some(Err(_)) => {
            return rsx! {
                main { id: "profile", p { "Failed to load your profile." } }
            }
        }
        None => {
            return rsx! {
                main { id: "profile", p { "Loading…" } }
            }
        }
    };

    let page_title = format!("{} | Profile", view.firm_name);

    rsx! {
        document::Title { "{page_title}" }
        document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
        document::Stylesheet { href: "{view.tokens_href}" }
        crate::components::AppNavbar {
            destinations: crate::app_chrome::app_destinations(view.role),
            logo: view.logo.clone(),
        }
        main { id: "profile", class: "nav-theme",
            header { class: "page-header",
                h1 { "Your profile" }
            }
            match &view.fields {
                Some(fields) => rsx! {
                    {avatar_upload_card(&fields.csrf_token, fields.avatar_is_public)}
                    {contact_card(fields)}
                    if let Some(card) = fields.signature.as_ref() {
                        {signature_card(card, &fields.csrf_token)}
                    }
                    if let Some(card) = fields.ssh_keys.as_ref() {
                        {ssh_keys_card(card, &fields.csrf_token)}
                    }
                },
                None => rsx! {
                    p { "Your session has no linked profile to show." }
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssr(app: fn() -> Element) -> String {
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    fn sample_fields() -> ProfileFields {
        ProfileFields {
            name: "Libra Scales".to_string(),
            email: "libra@example.com".to_string(),
            csrf_token: "csrf-token".to_string(),
            avatar_is_public: false,
            ssh_keys: None,
            signature: None,
        }
    }

    fn signature_app() -> Element {
        let card = SignatureCard {
            body: "Example Attorney\nConfidential: intended recipient only.".to_string(),
            available: true,
            notice: None,
            error: Some(signature_error_sentence("invalid").to_string()),
        };
        signature_card(&card, "csrf-token")
    }

    fn fresh_signature_app() -> Element {
        let card = SignatureCard {
            body: String::new(),
            available: true,
            notice: Some(signature_notice("saved").unwrap().to_string()),
            error: None,
        };
        signature_card(&card, "csrf-token")
    }

    fn unavailable_signature_app() -> Element {
        let card = SignatureCard {
            body: String::new(),
            available: false,
            notice: None,
            error: None,
        };
        signature_card(&card, "csrf-token")
    }

    fn ssh_keys_app() -> Element {
        let card = SshKeysCard {
            keys: vec![SshKeyRow {
                id: "00000000-0000-0000-0000-000000000001".to_string(),
                label: "laptop".to_string(),
                algorithm: "ssh-ed25519".to_string(),
                fingerprint: "SHA256:synthetic".to_string(),
                created_on: "2026-11-01".to_string(),
                synced_on: None,
            }],
            available: true,
            notice: None,
            error: Some(ssh_key_error("invalid").to_string()),
        };
        ssh_keys_card(&card, "csrf-token")
    }

    #[test]
    fn the_ssh_key_card_lists_keys_with_remove_forms_and_an_add_form() {
        let out = ssr(ssh_keys_app);
        assert!(out.contains("SHA256:synthetic"), "{out}");
        assert!(
            out.contains(
                r#"action="/app/profile/ssh-keys/00000000-0000-0000-0000-000000000001/delete""#
            ),
            "each key carries its own remove form: {out}"
        );
        assert!(
            out.contains(r#"action="/app/profile/ssh-keys""#),
            "the add form posts to the profile's SSH key route: {out}"
        );
        assert!(out.contains(r#"name="public_key""#), "{out}");
        assert!(out.contains("Never paste a private key."), "{out}");
        assert!(out.contains("Not yet"), "an unsynced key says so: {out}");
        assert!(
            out.contains(r#"role="alert""#),
            "the error flash renders: {out}"
        );
    }

    #[test]
    fn ssh_key_flash_codes_map_to_sentences_and_ignore_unknown_notices() {
        assert_eq!(ssh_key_notice("added"), Some("SSH key added."));
        assert_eq!(ssh_key_notice("<script>"), None);
        assert_eq!(
            ssh_key_error("<script>"),
            "The SSH key could not be saved.",
            "an unknown code never echoes request text"
        );
    }

    fn avatar_app() -> Element {
        avatar_upload_card("csrf-token", true)
    }

    fn private_avatar_app() -> Element {
        avatar_upload_card("csrf-token", false)
    }

    fn contact_app() -> Element {
        contact_card(&sample_fields())
    }

    #[test]
    fn the_avatar_card_is_a_sibling_multipart_form_posting_to_the_self_service_route() {
        let out = ssr(avatar_app);
        assert!(
            out.contains(r#"enctype="multipart/form-data""#),
            "the avatar form must be multipart: {out}"
        );
        assert!(
            out.contains(r#"action="/app/avatar""#),
            "posts to the sibling /app/avatar route, not a nested child of /app/profile: {out}"
        );
        assert!(
            out.contains(r#"src="/app/me/avatar""#),
            "the preview reads the caller's own avatar route: {out}"
        );
        assert!(
            out.contains(r#"id="profile-avatar-file""#),
            "the file input carries a stable id the in-place script can find: {out}"
        );
        assert!(
            out.contains("Your avatar will be publicly available."),
            "the upload card must disclose public availability: {out}"
        );
        assert!(
            out.contains("1024 × 1024 pixels"),
            "the upload card must state the dimension limits: {out}"
        );
        let csrf_pos = out.find(r#"name="_csrf""#);
        let file_pos = out.find(r#"type="file""#);
        assert!(
            csrf_pos.is_some() && file_pos.is_some() && csrf_pos < file_pos,
            "CSRF must be the first field, before the file input: {out}"
        );
    }

    #[test]
    fn a_client_avatar_card_keeps_the_private_lane_copy() {
        let out = ssr(private_avatar_app);
        assert!(
            !out.contains("publicly available"),
            "client avatars must not claim public availability: {out}"
        );
        assert!(out.contains("PNG, JPEG, or WebP, up to 5 MB"), "{out}");
    }

    /// The contact card renders every field disabled and offers no Save — an
    /// email edit here would drift from the OIDC identity the session was
    /// minted against, so the page never invites one.
    #[test]
    fn the_contact_card_is_read_only_with_no_submit_button() {
        let out = ssr(contact_app);
        assert!(
            out.contains(r#"id="name""#) && out.contains("disabled"),
            "{out}"
        );
        assert!(!out.contains(r#"type="submit""#), "{out}");
    }

    #[test]
    fn the_email_field_carries_a_contact_us_note() {
        let out = ssr(contact_app);
        assert!(
            out.contains("Contact us to update your email address."),
            "{out}"
        );
    }

    #[test]
    fn the_name_and_email_fields_are_prefilled_from_the_caller() {
        let out = ssr(contact_app);
        assert!(out.contains(r#"value="Libra Scales""#), "{out}");
        assert!(out.contains(r#"value="libra@example.com""#), "{out}");
    }

    /// The signature card is a native form posting the body to the
    /// self-service route: CSRF first, then the prefilled textarea. The
    /// subject is never on the form — the session is the only subject.
    #[test]
    fn the_signature_card_posts_the_body_to_the_self_service_route() {
        let out = ssr(signature_app);
        assert!(
            out.contains(r#"action="/app/profile/mail-signature""#),
            "{out}"
        );
        assert!(out.contains(r#"name="body""#), "{out}");
        assert!(
            !out.contains("person_id") && !out.contains("person"),
            "no subject field on the form: {out}"
        );
        assert!(
            out.contains("Confidential: intended recipient only."),
            "the stored block prefills the textarea: {out}"
        );
        let csrf_pos = out.find(r#"name="_csrf""#);
        let body_pos = out.find(r#"name="body""#);
        assert!(
            csrf_pos.is_some() && body_pos.is_some() && csrf_pos < body_pos,
            "CSRF must be the first field, before the body textarea: {out}"
        );
        assert!(
            out.contains("never include the delimiter here"),
            "the card warns off the delimiter line: {out}"
        );
    }

    #[test]
    fn the_fresh_signature_card_starts_from_a_blank_block_with_the_saved_flash() {
        let out = ssr(fresh_signature_app);
        assert!(
            out.contains(r#"role="status""#) && out.contains("Signature saved."),
            "{out}"
        );
        assert!(!out.contains("nav-form-error"), "{out}");
    }

    #[test]
    fn the_unavailable_signature_card_keeps_the_form_off_the_page() {
        let out = ssr(unavailable_signature_app);
        assert!(out.contains("unavailable right now"), "{out}");
        assert!(!out.contains(r#"name="body""#), "{out}");
    }

    #[test]
    fn signature_flash_codes_map_to_fixed_sentences_and_ignore_unknown_codes() {
        assert_eq!(signature_notice("saved"), Some("Signature saved."));
        assert_eq!(signature_notice("<script>"), None);
        assert!(signature_error_sentence("invalid").contains("2000"));
        assert!(signature_error_sentence("invalid").contains("delimiter"));
        assert_eq!(
            signature_error_sentence("<script>"),
            "The signature could not be saved.",
            "an unknown code never echoes request text"
        );
    }
}
