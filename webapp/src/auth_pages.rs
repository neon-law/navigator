//! Server-rendered public authentication pages.
//!
//! These handlers sit outside a Dioxus router, so they render through the
//! standalone document seam used by the error pages while retaining native,
//! pre-hydration form submissions.

use axum::response::Html;
use dioxus::prelude::*;

use crate::components::{Field, PublicShell, SiteHeader, SiteNavLink};
use crate::public_chrome::{firm_public_chrome, PublicChrome, PublicFooter};

/// One sign-in button on the chooser: where it goes and what it says.
///
/// The caller owns both, because which providers exist is a deployment fact
/// (`portal::oauth::AuthState::configured_providers`) and this crate renders
/// pages rather than deciding policy.
#[derive(Clone, PartialEq, Eq)]
pub struct SignInProvider {
    /// Absolute path including `?return_to=`, e.g.
    /// `/auth/login/microsoft?return_to=/app/projects`.
    pub href: String,
    /// The button text, e.g. [`MICROSOFT_SIGN_IN`].
    pub label: String,
}

#[derive(Clone, PartialEq, Eq)]
pub enum LoginNotice {
    Danger(String),
    Success(String),
}

impl LoginNotice {
    fn class(&self) -> &'static str {
        match self {
            Self::Danger(_) => "nav-alert nav-alert--danger",
            Self::Success(_) => "nav-alert nav-alert--success",
        }
    }

    fn message(&self) -> &str {
        match self {
            Self::Danger(message) | Self::Success(message) => message,
        }
    }
}

/// The Project-portal framing on the sign-in page: who the visitor is
/// signing in to, before any provider is named.
///
/// A client reaches `/auth/login` from a portal link they were sent, signed
/// out, and would otherwise meet the identity provider's own screen with
/// nothing naming the firm or the matter. This names both, says access is by
/// invitation, and offers a route to a person beside the provider button.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PortalWelcome {
    /// The Project the link pointed at, when its code resolved to a live
    /// row. `None` renders the firm-only copy.
    pub project_name: Option<String>,
}

/// The "reach a person" link beside the provider button on a portal
/// sign-in. Named so the covering test asserts the string the page renders.
pub const CONTACT_US: &str = "Contact us";

#[derive(Clone, PartialEq, Eq)]
enum Page {
    Signup {
        csrf: String,
        error: Option<SignupError>,
    },
    Login {
        return_to: String,
        csrf: String,
        /// Every configured identity provider, in render order. Empty renders
        /// no buttons at all, which is what a password-only deployment wants.
        providers: Vec<SignInProvider>,
        /// Whether the email/password form renders. Off for a deployment whose
        /// only doors are identity providers, so the page does not offer a
        /// credential nothing will accept.
        password_enabled: bool,
        error: Option<String>,
        notice: Option<LoginNotice>,
        /// Present when the visitor is bound for a Project portal: the page
        /// then frames the sign-in as that portal's front door.
        portal: Option<PortalWelcome>,
    },
    ResetRequest {
        csrf: String,
        error: Option<String>,
    },
    ResetNew {
        token: String,
        csrf: String,
        error: Option<String>,
    },
    ResetSent,
    InvalidLink,
    ConfirmEmail {
        email: String,
        csrf: String,
    },
}

pub fn login(
    return_to: &str,
    csrf: &str,
    providers: &[SignInProvider],
    password_enabled: bool,
    error: Option<&str>,
    notice: Option<LoginNotice>,
) -> Html<String> {
    login_page(
        return_to,
        csrf,
        providers,
        password_enabled,
        error,
        notice,
        None,
    )
}

/// The sign-in page framed as a Project portal's front door: the same doors
/// as [`login`], under copy that names the firm and the Project and offers
/// [`CONTACT_US`] beside them.
pub fn portal_login(
    return_to: &str,
    csrf: &str,
    providers: &[SignInProvider],
    password_enabled: bool,
    error: Option<&str>,
    notice: Option<LoginNotice>,
    portal: PortalWelcome,
) -> Html<String> {
    login_page(
        return_to,
        csrf,
        providers,
        password_enabled,
        error,
        notice,
        Some(portal),
    )
}

fn login_page(
    return_to: &str,
    csrf: &str,
    providers: &[SignInProvider],
    password_enabled: bool,
    error: Option<&str>,
    notice: Option<LoginNotice>,
    portal: Option<PortalWelcome>,
) -> Html<String> {
    render(
        if portal.is_some() {
            "Portal"
        } else {
            "Sign in"
        },
        Page::Login {
            return_to: return_to.into(),
            csrf: csrf.into(),
            providers: providers.to_vec(),
            password_enabled,
            error: error.map(str::to_string),
            notice,
            portal,
        },
    )
}

/// Safe public account-creation outcomes, with wording in the shared catalog.
#[derive(Clone, PartialEq, Eq)]
pub enum SignupError {
    Invalid,
    Rejected,
    SignInRequired,
}

impl SignupError {
    fn message(&self) -> String {
        crate::public_chrome::marketing_copy(match self {
            Self::Invalid => "signup.invalid",
            Self::Rejected => "signup.rejected",
            Self::SignInRequired => "signup.signin_required",
        })
    }
}

/// Account creation uses the same signed-CSRF and native form conventions as sign-in.
pub fn signup(csrf: &str, error: Option<SignupError>) -> Html<String> {
    render(
        &crate::public_chrome::marketing_copy("signup.title"),
        Page::Signup {
            csrf: csrf.into(),
            error,
        },
    )
}

pub fn password_reset_request(csrf: &str, error: Option<&str>) -> Html<String> {
    render(
        "Reset your password",
        Page::ResetRequest {
            csrf: csrf.into(),
            error: error.map(str::to_string),
        },
    )
}

pub fn password_reset_new(token: &str, csrf: &str, error: Option<&str>) -> Html<String> {
    render(
        "Choose a new password",
        Page::ResetNew {
            token: token.into(),
            csrf: csrf.into(),
            error: error.map(str::to_string),
        },
    )
}

pub fn password_reset_sent() -> Html<String> {
    render("Check your inbox", Page::ResetSent)
}
pub fn invalid_link() -> Html<String> {
    render("This link is no longer valid", Page::InvalidLink)
}
pub fn confirm_email(email: &str, csrf: &str) -> Html<String> {
    render(
        "Confirm your email",
        Page::ConfirmEmail {
            email: email.into(),
            csrf: csrf.into(),
        },
    )
}

fn render(title: &str, page: Page) -> Html<String> {
    let chrome = firm_public_chrome(vec![]);
    let brand_name = chrome.brand_name.clone();
    let firm_name = chrome.firm_name.clone();
    let mut dom = VirtualDom::new_with_props(
        AuthPage,
        AuthPageProps {
            page,
            chrome,
            firm_name,
        },
    );
    dom.rebuild_in_place();
    Html(crate::error_pages::standalone_document(
        title,
        &brand_name,
        &dioxus_ssr::render(&dom),
    ))
}

/// The OIDC button label on the sign-in chooser.
///
/// Named so the covering test asserts the same string the page renders
/// rather than a second copy of it that can drift.
pub const GOOGLE_SIGN_IN: &str = "Sign in with Google";

/// The Microsoft button label on the sign-in chooser.
///
/// Microsoft's branding guidance for apps requires the words "Sign in with
/// Microsoft" and forbids showing end users the "Azure" or "Active Directory"
/// brands, so the wording is fixed here rather than derived from config.
pub const MICROSOFT_SIGN_IN: &str = "Sign in with Microsoft";

/// Apple's button label for Sign in with Apple.
pub const APPLE_SIGN_IN: &str = "Sign in with Apple";

/// The account email field the sign-in and reset-request forms share.
fn email_field() -> Field {
    Field::email("Email", "email", "")
        .required()
        .autocomplete("email")
        .placeholder("you@example.com")
}

#[derive(Props, Clone, PartialEq)]
struct AuthPageProps {
    page: Page,
    chrome: PublicChrome,
    /// The practice the portal copy names, from the firm brand seam so a
    /// rebranded fork never introduces itself as Neon Law.
    firm_name: String,
}

#[component]
fn AuthPage(props: AuthPageProps) -> Element {
    let header = header(&props.chrome);
    let footer = footer(&props.chrome);
    rsx! { PublicShell { header, footer,
        section { class: "auth-page",
            div { class: "nav-card auth-card",
                match props.page {
                    Page::Login { return_to, csrf, providers, password_enabled, error, notice, portal } => rsx! {
                        if let Some(portal) = portal.as_ref() {
                            h1 { "Portal" }
                            // The firm and the matter, before any provider is
                            // named: the visitor should know whose door this is.
                            if let Some(project_name) = portal.project_name.as_deref() {
                                p { class: "auth-portal-lede",
                                    "This is the " strong { "{props.firm_name}" } " Portal for "
                                    strong { "{project_name}" } "."
                                }
                            } else {
                                p { class: "auth-portal-lede",
                                    "This is the " strong { "{props.firm_name}" } " Portal."
                                }
                            }
                            p { "Access is by invitation. Sign in with the email address your {props.firm_name} contact invited." }
                        } else {
                            h1 { "Sign in" }
                        }
                        if let Some(notice) = notice { p { class: "{notice.class()}", "{notice.message()}" } }
                        if let Some(error) = error { p { class: "nav-alert nav-alert--danger", "{error}" } }
                        if password_enabled {
                            form { class: "auth-form", method: "post", action: "/auth/password", "aria-label": "Sign in with email",
                                input { r#type: "hidden", name: "return_to", value: "{return_to}" }
                                input { r#type: "hidden", name: "csrf_token", value: "{csrf}" }
                                {email_field().render()}
                                {Field::password("Password", "password")
                                    .required()
                                    .autocomplete("current-password")
                                    .label_link("Forgot your password?", "/auth/password/reset")
                                    .render()}
                                button { r#type: "submit", class: "nav-btn nav-btn--primary auth-form__submit", "Sign in" }
                            }
                            if !providers.is_empty() {
                                div { class: "auth-divider", role: "separator", span { "or" } }
                            }
                        }
                        if !providers.is_empty() {
                            div { class: "auth-providers",
                                for provider in providers.iter() {
                                    a { class: "nav-btn nav-btn--secondary nav-btn--oauth", href: "{provider.href}",
                                        {provider_icon(&provider.label)}
                                        span { "{provider.label}" }
                                    }
                                }
                            }
                        }
                        if portal.is_some() {
                            p { "Not expecting this? " a { class: "nav-btn nav-btn--secondary", href: "/contact", "{CONTACT_US}" } }
                        }
                    },
                    Page::Signup { csrf, error } => rsx! {
                        h1 { {crate::public_chrome::marketing_copy("signup.title")} }
                        p { {crate::public_chrome::marketing_copy("signup.body")} }
                        if let Some(error) = error { p { class: "nav-alert nav-alert--danger", "{error.message()}" } }
                        form { class: "auth-form", method: "post", action: "/auth/signup",
                            input { r#type: "hidden", name: "csrf_token", value: "{csrf}" }
                            {email_field().render()}
                            {Field::password(crate::public_chrome::marketing_copy("signup.password_label"), "password").required().autocomplete("new-password").render()}
                            button { r#type: "submit", class: "nav-btn nav-btn--primary", {crate::public_chrome::marketing_copy("signup.action")} }
                        }
                        p { {crate::public_chrome::marketing_copy("signup.notice")} }
                        p { a { href: "/auth/login", {crate::public_chrome::marketing_copy("signup.existing")} } }
                    },
                    Page::ResetRequest { csrf, error } => rsx! {
                        h1 { "Reset your password" }
                        p { "Enter the email address for your account and we'll send you a link to choose a new password. The link expires in 30 minutes." }
                        if let Some(error) = error { p { class: "nav-alert nav-alert--danger", "{error}" } }
                        form { class: "auth-form", method: "post", action: "/auth/password/reset", "aria-label": "Request a reset link",
                            input { r#type: "hidden", name: "csrf_token", value: "{csrf}" }
                            {email_field().render()}
                            button { r#type: "submit", class: "nav-btn nav-btn--primary auth-form__submit", "Email me a reset link" }
                        }
                        p { a { href: "/auth/login", "Back to sign in" } }
                    },
                    Page::ResetNew { token, csrf, error } => rsx! {
                        h1 { "Choose a new password" }
                        p { "Choose a new password. Use at least 8 characters." }
                        if let Some(error) = error { p { class: "nav-alert nav-alert--danger", "{error}" } }
                        form { class: "auth-form", method: "post", action: "/auth/password/reset/new", "aria-label": "Choose a new password",
                            input { r#type: "hidden", name: "token", value: "{token}" }
                            input { r#type: "hidden", name: "csrf_token", value: "{csrf}" }
                            {Field::password("New password", "password").required().autocomplete("new-password").render()}
                            {Field::password("Confirm new password", "confirm").required().autocomplete("new-password").render()}
                            button { r#type: "submit", class: "nav-btn nav-btn--primary auth-form__submit", "Set new password" }
                        }
                    },
                    Page::ResetSent => rsx! { h1 { "Check your inbox" } p { "If an account exists for that email, we've sent a link to reset its password." } p { a { href: "/auth/password/reset", "Request another reset link" } } },
                    Page::InvalidLink => rsx! { h1 { "This link is no longer valid" } p { "This link has expired or has already been used. Reset links can be used once and are good for 30 minutes." } p { a { href: "/auth/password/reset", "Request a new reset link" } } },
                    Page::ConfirmEmail { email, csrf } => rsx! {
                        h1 { "Confirm your email" }
                        p { "Almost there — please confirm your email address before signing in." }
                        p { "We've sent a confirmation link to your inbox. Click it and you'll be able to sign in. The link expires in 30 minutes." }
                        form { method: "post", action: "/auth/email/confirm/resend",
                            input { r#type: "hidden", name: "email", value: "{email}" }
                            input { r#type: "hidden", name: "csrf_token", value: "{csrf}" }
                            button { r#type: "submit", class: "nav-btn nav-btn--primary", "Resend confirmation email" }
                        }
                    },
                }
            }
        }
    } }
}

/// The brand mark for a sign-in button, chosen from the fixed button label
/// rather than the provider slug. Each supported provider carries its own
/// brand mark instead of the single-colour `currentColor` set in
/// [`crate::components::Icon`].
fn provider_icon(label: &str) -> Element {
    if label == GOOGLE_SIGN_IN {
        google_mark()
    } else if label == MICROSOFT_SIGN_IN {
        microsoft_mark()
    } else if label == APPLE_SIGN_IN {
        apple_mark()
    } else {
        rsx! {}
    }
}

/// Google's official four-colour "G" mark.
fn google_mark() -> Element {
    rsx! {
        svg {
            class: "nav-oauth-icon",
            xmlns: "http://www.w3.org/2000/svg",
            "viewBox": "0 0 18 18",
            width: "18",
            height: "18",
            "aria-hidden": "true",
            path { fill: "#4285F4", d: "M17.64 9.2c0-.637-.057-1.251-.164-1.84H9v3.481h4.844a4.14 4.14 0 0 1-1.796 2.716v2.259h2.908c1.702-1.567 2.684-3.874 2.684-6.615z" }
            path { fill: "#34A853", d: "M9 18c2.43 0 4.467-.806 5.956-2.184l-2.908-2.259c-.806.54-1.837.86-3.048.86-2.344 0-4.328-1.584-5.036-3.711H.957v2.332A8.997 8.997 0 0 0 9 18z" }
            path { fill: "#FBBC05", d: "M3.964 10.706A5.41 5.41 0 0 1 3.682 9c0-.593.102-1.17.282-1.706V4.962H.957A8.996 8.996 0 0 0 0 9c0 1.452.348 2.827.957 4.038l3.007-2.332z" }
            path { fill: "#EA4335", d: "M9 3.58c1.321 0 2.508.454 3.44 1.345l2.582-2.581C13.463.891 11.426 0 9 0A8.997 8.997 0 0 0 .957 4.962L3.964 7.294C4.672 5.167 6.656 3.58 9 3.58z" }
        }
    }
}

/// Microsoft's official four-square mark.
fn microsoft_mark() -> Element {
    rsx! {
        svg {
            class: "nav-oauth-icon",
            xmlns: "http://www.w3.org/2000/svg",
            "viewBox": "0 0 21 21",
            width: "18",
            height: "18",
            "aria-hidden": "true",
            rect { x: "1", y: "1", width: "9", height: "9", fill: "#f25022" }
            rect { x: "1", y: "11", width: "9", height: "9", fill: "#00a4ef" }
            rect { x: "11", y: "1", width: "9", height: "9", fill: "#7fba00" }
            rect { x: "11", y: "11", width: "9", height: "9", fill: "#ffb900" }
        }
    }
}

/// Apple's monochrome mark.
fn apple_mark() -> Element {
    rsx! {
        svg {
            class: "nav-oauth-icon",
            xmlns: "http://www.w3.org/2000/svg",
            "viewBox": "0 0 24 24",
            width: "18",
            height: "18",
            "aria-hidden": "true",
            path { fill: "currentColor", d: "M17.05 12.54c-.02-2.08 1.7-3.08 1.78-3.13-.97-1.42-2.48-1.61-3.01-1.63-1.27-.13-2.5.76-3.15.76-.66 0-1.67-.75-2.75-.73-1.41.02-2.71.82-3.44 2.08-1.48 2.56-.38 6.33 1.05 8.4.71 1.01 1.54 2.14 2.63 2.1 1.06-.04 1.46-.68 2.74-.68 1.28 0 1.64.68 2.75.65 1.14-.02 1.86-1.02 2.55-2.04.8-1.17 1.13-2.3 1.15-2.36-.03-.01-2.27-.87-2.3-3.42zM14.97 6.43c.58-.7.97-1.68.86-2.66-.84.03-1.85.56-2.45 1.26-.54.62-1.01 1.62-.88 2.57.94.07 1.9-.48 2.47-1.17z" }
        }
    }
}

fn header(chrome: &PublicChrome) -> Element {
    rsx! { SiteHeader { brand_name: chrome.brand_name.clone(), home_href: chrome.home_href.clone(), logo_href: chrome.logo_href.clone(), destinations: chrome.destinations.iter().map(|link| SiteNavLink::new(link.label.clone(), link.href.clone())).collect(), utility: chrome.utility.iter().map(|link| SiteNavLink::new(link.label.clone(), link.href.clone())).collect() } }
}
fn footer(chrome: &PublicChrome) -> Element {
    rsx! { PublicFooter { chrome: chrome.clone() } }
}

#[cfg(test)]
mod tests {
    use super::{
        confirm_email, invalid_link, login, password_reset_new, password_reset_request,
        portal_login, LoginNotice, PortalWelcome, SignInProvider, APPLE_SIGN_IN, CONTACT_US,
        GOOGLE_SIGN_IN, MICROSOFT_SIGN_IN,
    };

    fn provider(slug: &str, label: &str) -> SignInProvider {
        SignInProvider {
            href: format!("/auth/login/{slug}?return_to=/app/projects"),
            label: label.to_string(),
        }
    }

    /// A portal link's sign-in names the firm and the matter before the
    /// provider, says access is by invitation, and offers a route to a person
    /// beside the provider button — the page a signed-out client meets
    /// instead of the identity provider's own screen.
    #[test]
    fn portal_login_names_the_firm_and_the_project_and_offers_both_doors() {
        let html = portal_login(
            "/app/projects/sample-estate/portal/",
            "CSRF",
            &[SignInProvider {
                href: "/auth/login/oidc?return_to=/app/projects/sample-estate/portal/".into(),
                label: GOOGLE_SIGN_IN.into(),
            }],
            false,
            None,
            None,
            PortalWelcome {
                project_name: Some("Estate of Cornelius Montgomery".into()),
            },
        )
        .0;
        let firm = crate::public_chrome::firm_public_chrome(vec![]).firm_name;
        for needle in [
            "| Portal</title>",
            "<h1>Portal</h1>",
            &format!("<strong>{firm}</strong> Portal for <strong>Estate of Cornelius Montgomery</strong>"),
            "Access is by invitation.",
            "Not expecting this?",
            "href=\"/contact\"",
            CONTACT_US,
            "/auth/login/oidc?return_to=/app/projects/sample-estate/portal/",
            GOOGLE_SIGN_IN,
        ] {
            assert!(html.contains(needle), "missing {needle}: {html}");
        }
        assert!(
            !html.contains("action=\"/auth/password\""),
            "no password form on a provider-only deployment: {html}"
        );
    }

    /// Without a resolvable Project the page still frames itself as the
    /// firm's portal rather than falling back to the bare chooser.
    #[test]
    fn portal_login_without_a_project_keeps_the_firm_copy() {
        let html = portal_login(
            "/app/projects/unknown/portal/",
            "CSRF",
            &[provider("oidc", GOOGLE_SIGN_IN)],
            false,
            None,
            None,
            PortalWelcome { project_name: None },
        )
        .0;
        let firm = crate::public_chrome::firm_public_chrome(vec![]).firm_name;
        assert!(
            html.contains(&format!("<strong>{firm}</strong> Portal.")),
            "firm-only lede: {html}"
        );
        assert!(!html.contains(" Portal for "), "no matter named: {html}");
        assert!(html.contains(CONTACT_US), "{html}");
    }

    /// The ordinary chooser is unchanged by the portal framing: no portal
    /// copy, no Contact us door.
    #[test]
    fn login_without_a_portal_destination_renders_no_portal_copy() {
        let html = login(
            "/app/projects",
            "CSRF",
            &[provider("oidc", GOOGLE_SIGN_IN)],
            false,
            None,
            None,
        )
        .0;
        assert!(html.contains("<h1>Sign in</h1>"), "{html}");
        assert!(!html.contains("Portal"), "{html}");
        assert!(!html.contains(CONTACT_US), "{html}");
    }

    #[test]
    fn login_keeps_the_native_password_and_oidc_contracts() {
        let html = login(
            "/app/projects",
            "CSRF",
            &[provider("oidc", GOOGLE_SIGN_IN)],
            true,
            Some("Try again"),
            Some(LoginNotice::Danger("Sign in required".into())),
        )
        .0;
        for needle in [
            "action=\"/auth/password\"",
            "name=\"return_to\" value=\"/app/projects\"",
            "name=\"csrf_token\" value=\"CSRF\"",
            "name=\"email\"",
            "name=\"password\"",
            // Both credentials render through the shared `Field`.
            r#"<label class="nav-label" for="email">"#,
            r#"class="nav-input" type="email""#,
            r#"autocomplete="current-password""#,
            r#"<a class="nav-field__label-link" href="/auth/password/reset">Forgot your password?</a>"#,
            "/auth/login/oidc?return_to=/app/projects",
            "Sign in required",
            "Try again",
        ] {
            assert!(html.contains(needle), "missing {needle}: {html}");
        }
        assert!(
            html.contains(GOOGLE_SIGN_IN),
            "missing {GOOGLE_SIGN_IN}: {html}"
        );
    }

    /// Two providers render two buttons, each to its own
    /// `/auth/login/{provider}` route, in the order given. The order is
    /// asserted because a signed-out person navigates the page by button
    /// position, and switching a provider on must not shuffle it.
    #[test]
    fn login_renders_one_button_per_configured_provider_in_order() {
        let html = login(
            "/app/projects",
            "CSRF",
            &[
                provider("oidc", GOOGLE_SIGN_IN),
                provider("microsoft", MICROSOFT_SIGN_IN),
                provider("apple", APPLE_SIGN_IN),
            ],
            false,
            None,
            None,
        )
        .0;
        let google = html
            .find(GOOGLE_SIGN_IN)
            .unwrap_or_else(|| panic!("missing {GOOGLE_SIGN_IN}: {html}"));
        let microsoft = html
            .find(MICROSOFT_SIGN_IN)
            .unwrap_or_else(|| panic!("missing {MICROSOFT_SIGN_IN}: {html}"));
        let apple = html
            .find(APPLE_SIGN_IN)
            .unwrap_or_else(|| panic!("missing {APPLE_SIGN_IN}: {html}"));
        assert!(google < microsoft, "primary provider must render first");
        assert!(microsoft < apple, "Apple must render after Microsoft");
        assert!(html.contains("/auth/login/oidc?return_to=/app/projects"));
        assert!(html.contains("/auth/login/microsoft?return_to=/app/projects"));
        assert!(html.contains("/auth/login/apple?return_to=/app/projects"));
    }

    /// Each provider button carries its own brand mark, not the other's or a
    /// generic glyph — a signed-out person recognizes the button by colour
    /// before reading the label.
    #[test]
    fn login_renders_each_providers_brand_mark() {
        let html = login(
            "/app/projects",
            "CSRF",
            &[
                provider("oidc", GOOGLE_SIGN_IN),
                provider("microsoft", MICROSOFT_SIGN_IN),
                provider("apple", APPLE_SIGN_IN),
            ],
            false,
            None,
            None,
        )
        .0;
        assert!(
            html.contains("nav-oauth-icon") && html.contains("fill=\"#4285F4\""),
            "missing Google mark: {html}"
        );
        assert!(
            html.contains("fill=\"#f25022\""),
            "missing Microsoft mark: {html}"
        );
        assert!(
            html.contains("Sign in with Apple") && html.contains("currentColor"),
            "missing Apple mark: {html}"
        );
    }

    /// With no password door there is no credential form and no reset link —
    /// offering either would invite a password nothing will accept.
    #[test]
    fn login_without_a_password_door_renders_no_credential_form() {
        let html = login(
            "/app/projects",
            "CSRF",
            &[provider("oidc", GOOGLE_SIGN_IN)],
            false,
            None,
            None,
        )
        .0;
        assert!(!html.contains("action=\"/auth/password\""), "{html}");
        assert!(!html.contains("/auth/password/reset"), "{html}");
        assert!(html.contains(GOOGLE_SIGN_IN), "{html}");
    }

    #[test]
    fn recovery_forms_preserve_their_tokens_and_actions() {
        let request = password_reset_request("REQUEST", None).0;
        assert!(request.contains("action=\"/auth/password/reset\""));
        assert!(request.contains("name=\"csrf_token\" value=\"REQUEST\""));
        let new_password = password_reset_new("RESET", "NEW", Some("Too short")).0;
        assert!(new_password.contains("action=\"/auth/password/reset/new\""));
        assert!(new_password.contains("name=\"token\" value=\"RESET\""));
        assert!(new_password.contains("name=\"csrf_token\" value=\"NEW\""));
        assert!(new_password.contains("Too short"));
    }

    #[test]
    fn confirmation_and_invalid_link_remain_complete_documents() {
        let confirmation = confirm_email("libra@example.com", "CONFIRM").0;
        assert!(confirmation.starts_with("<!DOCTYPE html>"));
        assert!(confirmation.contains("action=\"/auth/email/confirm/resend\""));
        assert!(confirmation.contains("name=\"email\" value=\"libra@example.com\""));
        let invalid = invalid_link().0;
        assert!(invalid.contains("Request a new reset link"));
    }
}
