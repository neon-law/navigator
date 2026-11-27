#![allow(clippy::doc_markdown)]
//! End-to-end tests for the **second** browser sign-in provider, Microsoft
//! Entra ID, running alongside the primary OIDC slot rather than replacing it.
//!
//! These drive the real `server::neon_router()` through
//! `tower::ServiceExt::oneshot`, with `wiremock` standing in for both IdPs and
//! a locally-held RSA key signing the id_tokens. No socket on the app side, no
//! browser, and no network call to Microsoft.
//!
//! Three things are worth testing here and nothing else is:
//!
//! 1. **The templated issuer.** Multi-tenant Entra publishes the literal
//!    `https://login.microsoftonline.com/{tenantid}/v2.0` as its `issuer`, and
//!    the token carries the signing tenant's GUID in `tid`. Pinning the
//!    template verbatim — what `Validation::set_issuer` does for every other
//!    provider — rejects every real token, so the per-tenant path has to be
//!    exercised for real.
//! 2. **The tenant allowlist.** It is the control that makes multi-tenant
//!    sign-in safe, so a token from an unlisted tenant must be refused even
//!    when its signature, audience, nonce and issuer are all internally
//!    consistent.
//! 3. **Provider disambiguation.** Both providers share one redirect URI, so
//!    the callback picks the provider out of the signed pre-auth cookie. A
//!    Microsoft login must not be redeemable at the primary provider's token
//!    endpoint, and vice versa.

#[path = "common/seed_person.rs"]
mod seed_person;

// ---------------------------------------------------------------------------
// `OAUTH_MICROSOFT_ALLOW_ANY_TENANT`: the foreign-tenant path, end to end.
//
// These drive the real callback (signature, audience, nonce, per-tenant issuer,
// then person resolution) so the seam between `IdTokenVerifier::verify` and
// `complete_sign_in` is protected: the resolver unit tests build their claims by
// hand and cannot notice if that seam stops setting the foreign-tenant mark.
// ---------------------------------------------------------------------------
