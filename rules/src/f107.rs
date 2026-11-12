//! `N107` — notation template signature placeholders must resolve.
//!
//! A *signature placeholder* is a `{{ … }}` token whose trimmed inner
//! text names a signer and field — e.g. `{{client.signature}}`. Dotted
//! data paths such as `{{person__client.name}}` and aggregate loop
//! variables such as `{{m.name}}` belong to `N115`, so this rule skips
//! questionnaire states and lexical `#for` variables.
//!
//! The placeholder splits on the **first** dot into `<signer>.<field>`:
//!
//! - `signer` must be a role from the template's declared [`SignerSet`]:
//!   the optional frontmatter `signers:` list, or `[client, firm]` when
//!   that key is absent. Roles, never a person's name — each signer
//!   resolves to a real Person at notation time (`client` to the
//!   respondent, `firm` to the attorney of record, any other role to
//!   the matching `person__<role>` questionnaire state).
//! - `field` must be one of [`F107SignaturePlaceholders::FIELDS`]
//!   (`signature`, `initials`, `date`).
//!
//! An explicit `signers:` list is bidirectional with the body: every
//! listed role must appear as a `signature` or `initials` placeholder,
//! and every placeholder role must be on the list. A declared role with
//! no placeholder is an error.
//!
//! The signing declaration is **bidirectional** — a well-formed
//! signature is declared in two places that must agree:
//!
//! - **Forward:** a template that draws *any* valid signature block must
//!   declare a signing State in its workflow — a state keyed
//!   `sent_for_signature` or prefixed `sent_for_signature__`. A
//!   signature line that never becomes an attributable signature is a
//!   candor problem.
//! - **Reverse:** a template whose workflow declares a
//!   `sent_for_signature[__*]` State must carry at least one body
//!   signature anchor for the field to land on. A signing step with
//!   nowhere to sign is the same candor gap in mirror image — and it is
//!   exactly how a template reaches the e-signature provider with a
//!   signing state but no placed tab (the live retainer bug).
//!
//! Both directions are deliberate: the provider's signers and tabs are
//! derived from this two-part declaration, never from ad-hoc UI
//! placement, so the template can never reach the provider with one half
//! of the pair missing.
//!
//! Files without frontmatter are skipped: N107 is a notation-template
//! rule, not a check on arbitrary prose that happens to contain a
//! `{{x.y}}` token.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_yaml::Value;

use crate::{frontmatter, is_snake_case, Rule, SourceFile, Violation};

/// Default `signers:` when the key is absent — every existing template.
pub const DEFAULT_SIGNERS: &[&str] = &["client", "firm"];

/// How the envelope reaches one declared signer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignerDelivery {
    /// Captive embedded signing — the default posture. Navigator serves
    /// the embedded ceremony only to the notation's bound client, so a
    /// declared role other than `client` must not keep this default
    /// (the parser refuses it).
    Embedded,
    /// The provider emails the signer their link — non-captive.
    Emailed,
}

impl SignerDelivery {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "embedded" => Some(Self::Embedded),
            "emailed" => Some(Self::Emailed),
            _ => None,
        }
    }

    /// The word the frontmatter spells, for diagnostics.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Embedded => "embedded",
            Self::Emailed => "emailed",
        }
    }
}

/// One declared signer: the role, where it sits in the envelope's routing
/// order, how the envelope reaches it, and — for roles the send path cannot
/// resolve on its own — the questionnaire answer keys that carry its
/// identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignerRole {
    /// Role name (`client`, `firm`, `confirming_assignor`, …).
    pub role: String,
    /// 1-based envelope routing order. Declaration order when the template
    /// does not spell routing orders out; a declared order must form a
    /// permutation of `1..=n` across the whole `signers:` list.
    pub routing_order: u32,
    /// How the envelope reaches this signer.
    pub delivery: SignerDelivery,
    /// Questionnaire answer key overriding the default identity lookup for
    /// the signer's name (`person__<role>.name` when undeclared).
    pub name: Option<String>,
    /// Questionnaire answer key overriding the default identity lookup for
    /// the signer's email (`person__<role>.email` when undeclared).
    pub email: Option<String>,
}

/// The signer roles a template permits, in routing order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignerSet {
    /// The declared signers, in routing order.
    pub signers: Vec<SignerRole>,
    /// True when frontmatter carried an explicit `signers:` list.
    pub explicit: bool,
}

impl SignerSet {
    fn implicit_default() -> Self {
        Self {
            signers: vec![
                SignerRole {
                    role: "client".to_string(),
                    routing_order: 1,
                    delivery: SignerDelivery::Embedded,
                    name: None,
                    email: None,
                },
                SignerRole {
                    role: "firm".to_string(),
                    routing_order: 2,
                    delivery: SignerDelivery::Emailed,
                    name: None,
                    email: None,
                },
            ],
            explicit: false,
        }
    }

    /// The routing-ordered roles, as bare names.
    pub fn roles(&self) -> impl Iterator<Item = &str> {
        self.signers.iter().map(|s| s.role.as_str())
    }

    /// Comma-separated roles for diagnostics.
    #[must_use]
    pub fn joined(&self) -> String {
        self.roles().collect::<Vec<_>>().join(", ")
    }

    /// True when `role` is in this set.
    #[must_use]
    pub fn contains(&self, role: &str) -> bool {
        self.signers.iter().any(|s| s.role == role)
    }

    /// A role's full declaration, when the set carries one.
    #[must_use]
    pub fn declared(&self, role: &str) -> Option<&SignerRole> {
        self.signers.iter().find(|s| s.role == role)
    }
}

/// Parse the template's signer set from its full Markdown source.
///
/// No frontmatter, no `signers:` key, or a null `signers:` all yield the
/// implicit default `[client, firm]`. A present list mixes bare role names
/// with declarations (`role`, optional `routing_order`, `delivery`, and
/// `name`/`email` answer-key bindings); routing orders, when any entry
/// declares one, must form a permutation of `1..=n` across the list.
/// Embedded signing is servable only for the notation's bound client, so
/// every other declared role must spell `delivery: emailed`.
pub fn signer_set(contents: &str) -> Result<SignerSet, String> {
    let Some(fm) = frontmatter::extract(contents) else {
        return Ok(SignerSet::implicit_default());
    };
    let yaml: Value = serde_yaml::from_str(fm).map_err(|e| e.to_string())?;
    parse_signer_set(&yaml)
}

fn parse_signer_set(yaml: &Value) -> Result<SignerSet, String> {
    let Some(signers) = yaml.get("signers") else {
        return Ok(SignerSet::implicit_default());
    };
    if signers.is_null() {
        return Ok(SignerSet::implicit_default());
    }
    let Some(seq) = signers.as_sequence() else {
        return Err(
            "signers: must be a list of role names or `{role, delivery, …}` declarations"
                .to_string(),
        );
    };
    let questionnaire_states = declared_questionnaire_states(yaml);
    let mut signers: Vec<SignerRole> = Vec::with_capacity(seq.len());
    for (i, item) in seq.iter().enumerate() {
        signers.push(parse_signer_role(i, item, &questionnaire_states)?);
    }
    if let Some(dup) = first_duplicate_role(&signers) {
        return Err(format!("signer role `{dup}` is listed twice"));
    }
    order_by_routing(&mut signers)?;
    Ok(SignerSet {
        signers,
        explicit: true,
    })
}

/// The questionnaire state names a template declares, for binding checks.
fn declared_questionnaire_states(yaml: &Value) -> std::collections::BTreeSet<String> {
    yaml.get("questionnaire")
        .and_then(|q| q.as_mapping())
        .map(|m| {
            m.keys()
                .filter_map(|k| k.as_str())
                .filter(|state| *state != "BEGIN" && *state != "END")
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_signer_role(
    index: usize,
    item: &Value,
    questionnaire_states: &std::collections::BTreeSet<String>,
) -> Result<SignerRole, String> {
    if let Some(role) = item.as_str() {
        let role = require_role_name(role)?;
        return match role.as_str() {
            "client" => Ok(SignerRole {
                role,
                routing_order: positional_order(index),
                delivery: SignerDelivery::Embedded,
                name: None,
                email: None,
            }),
            _ => Err(format!(
                "declared signer `{role}` defaults to captive embedded signing, which \
                 Navigator serves only to the notation's bound client; spell its \
                 delivery with a declaration entry carrying `delivery: emailed`"
            )),
        };
    }
    let Some(entry) = item.as_mapping() else {
        return Err(format!(
            "signers[{index}] must be a role name or a declaration"
        ));
    };
    let Some(role_str) = entry.get(Value::from("role")).and_then(Value::as_str) else {
        return Err(format!(
            "signers[{index}] declaration needs a string `role:`"
        ));
    };
    let role = require_role_name(role_str)?;

    let delivery = match entry.get(Value::from("delivery")) {
        None | Some(Value::Null) if role == "client" => SignerDelivery::Embedded,
        None | Some(Value::Null) => {
            return Err(format!(
                "declared signer `{role}` defaults to captive embedded signing, which \
                 Navigator serves only to the notation's bound client; add \
                 `delivery: emailed`"
            ));
        }
        Some(value) => {
            let Some(text) = value.as_str() else {
                return Err(format!("signers[{index}].delivery must be a string"));
            };
            let parsed = SignerDelivery::parse(text).ok_or_else(|| {
                format!("signers[{index}].delivery is `{text}`; expected `emailed` or `embedded`")
            })?;
            if role == "client" && parsed == SignerDelivery::Emailed {
                return Err(
                    "the client signer cannot declare `delivery: emailed`: the client's \
                     delivery is the notation's, chosen when the matter opens (default \
                     embedded)"
                        .to_string(),
                );
            }
            parsed
        }
    };

    let name = answer_binding(
        index,
        "name",
        entry.get(Value::from("name")),
        questionnaire_states,
    )?;
    let email = answer_binding(
        index,
        "email",
        entry.get(Value::from("email")),
        questionnaire_states,
    )?;
    let routing_order = match entry.get(Value::from("routing_order")) {
        None | Some(Value::Null) => None,
        Some(value) => {
            let order = value
                .as_u64()
                .and_then(|order| u32::try_from(order).ok())
                .filter(|order| *order > 0)
                .ok_or_else(|| {
                    format!("signers[{index}].routing_order must be a positive integer")
                })?;
            Some(order)
        }
    };
    Ok(SignerRole {
        role,
        routing_order: routing_order.unwrap_or_else(|| positional_order(index)),
        delivery,
        name,
        email,
    })
}

fn require_role_name(role: &str) -> Result<String, String> {
    if !is_signer_role_name(role) {
        return Err(format!(
            "`{role}` is not a lowercase snake_case signer role"
        ));
    }
    Ok(role.to_string())
}

/// Validate an optional `name:`/`email:` identity binding: a non-empty
/// questionnaire answer key whose state part names a declared state.
fn answer_binding(
    index: usize,
    field: &str,
    value: Option<&Value>,
    questionnaire_states: &std::collections::BTreeSet<String>,
) -> Result<Option<String>, String> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let Some(key) = value.as_str() else {
        return Err(format!(
            "signers[{index}].{field} must be a questionnaire answer key"
        ));
    };
    if key.is_empty() {
        return Err(format!("signers[{index}].{field} must not be empty"));
    }
    let state = key.split('.').next().unwrap_or(key);
    if !questionnaire_states.contains(state) {
        return Err(format!(
            "signers[{index}].{field} binds `{key}`, which is not a declared \
             questionnaire state"
        ));
    }
    Ok(Some(key.to_string()))
}

fn first_duplicate_role(signers: &[SignerRole]) -> Option<String> {
    for (i, signer) in signers.iter().enumerate() {
        if signers[..i].iter().any(|s| s.role == signer.role) {
            return Some(signer.role.clone());
        }
    }
    None
}

/// Routing orders, when any entry spells one, must form a permutation of
/// `1..=n`; the set is then stored in routing order.
fn order_by_routing(signers: &mut [SignerRole]) -> Result<(), String> {
    signers.sort_by_key(|s| s.routing_order);
    for (position, signer) in signers.iter().enumerate() {
        let expected = positional_order(position);
        if signer.routing_order != expected {
            return Err(format!(
                "declared signer routing orders must form a permutation of 1..={}; \
                 `{}` sits at position {expected} but declares {}",
                signers.len(),
                signer.role,
                signer.routing_order
            ));
        }
    }
    Ok(())
}

fn is_signer_role_name(name: &str) -> bool {
    is_snake_case(name) && !name.contains("__")
}

/// A list position as a 1-based routing order, saturating at `u32::MAX`.
fn positional_order(index: usize) -> u32 {
    u32::try_from(index).map_or(u32::MAX, |i| i.saturating_add(1))
}

const FOR_OPEN: &str = "{{#for ";

pub struct F107SignaturePlaceholders;

impl F107SignaturePlaceholders {
    pub const CODE: &'static str = "N107";

    /// Default signer roles when a template omits `signers:`.
    pub const SIGNERS: &'static [&'static str] = DEFAULT_SIGNERS;

    /// Recognized signature field types, each mapping to a distinct
    /// downstream e-signature tab (signHere / initialHere / dateSigned).
    pub const FIELDS: &'static [&'static str] = &["signature", "initials", "date"];

    /// The workflow State prefix that marks a signing step.
    const SIGNING_STATE: &'static str = "sent_for_signature";
}

/// One `{{ signer.field }}` token found in a source file. `offset` is
/// the byte offset of the opening `{{`; `end` is the byte offset just
/// past the closing `}}` (so `contents[offset..end]` is the whole
/// token); `signer`/`field` are the halves of the trimmed inner text
/// split on the first `.`.
///
/// Exposed so the renderer and the signature-manifest builder consume
/// the *same* grammar this rule validates — one parser, one source of
/// truth for what a signature placeholder is. `offset`/`end` let the
/// renderer splice the token out and replace it without re-scanning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignaturePlaceholder {
    pub offset: usize,
    pub end: usize,
    pub signer: String,
    pub field: String,
}

/// Scan `contents` for signature placeholders: every `{{ … }}` token
/// whose trimmed inner text contains a `.`. Data placeholders without
/// a dot are ignored. The split is on the *first* dot, so `{{a.b.c}}`
/// yields `signer = "a"`, `field = "b.c"` (which the rule then flags
/// as an unknown field).
#[must_use]
pub fn signature_placeholders(contents: &str) -> Vec<SignaturePlaceholder> {
    let bytes = contents.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'{' && bytes[i + 1] == b'{' {
            if let Some(rel) = contents[i + 2..].find("}}") {
                let end = i + 2 + rel + 2;
                let inner = contents[i + 2..i + 2 + rel].trim();
                if !inner.starts_with('#') && !inner.starts_with('/') {
                    if let Some((signer, field)) = inner.split_once('.') {
                        out.push(SignaturePlaceholder {
                            offset: i,
                            end,
                            signer: signer.trim().to_string(),
                            field: field.trim().to_string(),
                        });
                    }
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Lexical variables introduced by `{{#for <var> in <state>}}` blocks.
fn loop_variables(contents: &str) -> std::collections::BTreeSet<String> {
    let mut vars = std::collections::BTreeSet::new();
    let mut rest = contents;
    while let Some(start) = rest.find(FOR_OPEN) {
        let after_open = &rest[start + FOR_OPEN.len()..];
        let Some(header_len) = after_open.find("}}") else {
            break;
        };
        let header = after_open[..header_len].trim();
        if let Some((var, _state)) = header.split_once(" in ") {
            vars.insert(var.trim().to_string());
        }
        rest = &after_open[header_len + 2..];
    }
    vars
}

#[derive(Debug, Deserialize)]
struct FrontmatterShape {
    #[serde(default)]
    workflow: Option<BTreeMap<String, BTreeMap<String, String>>>,
    #[serde(default)]
    questionnaire: Option<BTreeMap<String, BTreeMap<String, String>>>,
}

/// 1-based line number containing the byte at `offset`.
fn line_at(contents: &str, offset: usize) -> usize {
    contents[..offset.min(contents.len())]
        .bytes()
        .filter(|b| *b == b'\n')
        .count()
        + 1
}

impl Rule for F107SignaturePlaceholders {
    fn code(&self) -> &'static str {
        Self::CODE
    }

    fn lint(&self, file: &SourceFile) -> Vec<Violation> {
        let Some(fm) = frontmatter::extract(&file.contents) else {
            return Vec::new();
        };

        let mut violations = Vec::new();
        let set = match signer_set(&file.contents) {
            Ok(set) => set,
            Err(message) => {
                violations.push(n107(file, 1, 0..0, message));
                SignerSet::implicit_default()
            }
        };

        let parsed = crate::questionnaire::deserialize_frontmatter::<FrontmatterShape>(fm).ok();
        let questionnaire_states: std::collections::BTreeSet<&str> = parsed
            .as_ref()
            .and_then(|p| p.questionnaire.as_ref())
            .into_iter()
            .flat_map(|q| q.keys().map(String::as_str))
            .collect();
        let loops = loop_variables(&file.contents);
        let scan = scan_placeholders(file, &set, &questionnaire_states, &loops);
        violations.extend(scan.violations);
        if set.explicit {
            require_declared_placeholders(file, &set, &scan.signed_roles, &mut violations);
        }
        cross_check_signing_state(
            file,
            parsed,
            scan.saw_valid,
            scan.saw_relevant,
            &mut violations,
        );
        violations
    }
}

struct PlaceholderScan {
    violations: Vec<Violation>,
    saw_valid: bool,
    saw_relevant: bool,
    signed_roles: std::collections::BTreeSet<String>,
}

fn n107(
    file: &SourceFile,
    line: usize,
    range: std::ops::Range<usize>,
    message: impl Into<String>,
) -> Violation {
    Violation {
        code: F107SignaturePlaceholders::CODE,
        path: file.path.clone(),
        line,
        range,
        message: message.into(),
    }
}

fn scan_placeholders(
    file: &SourceFile,
    set: &SignerSet,
    questionnaire_states: &std::collections::BTreeSet<&str>,
    loops: &std::collections::BTreeSet<String>,
) -> PlaceholderScan {
    let mut scan = PlaceholderScan {
        violations: Vec::new(),
        saw_valid: false,
        saw_relevant: false,
        signed_roles: std::collections::BTreeSet::new(),
    };
    for ph in signature_placeholders(&file.contents) {
        if questionnaire_states.contains(ph.signer.as_str()) || loops.contains(ph.signer.as_str()) {
            continue;
        }
        // The Firm's legal name is a render binding, filled from the Project's
        // Brand. It shares the `firm.` prefix with the signature anchors and
        // is not itself a signature field.
        if ph.signer == "firm" && ph.field == "legal_name" {
            continue;
        }
        scan.saw_relevant = true;
        let signer_ok = set.contains(&ph.signer);
        let field_ok = F107SignaturePlaceholders::FIELDS.contains(&ph.field.as_str());
        let line = line_at(&file.contents, ph.offset);
        let range = ph.offset..ph.offset;
        if !signer_ok {
            scan.violations.push(n107(
                file,
                line,
                range.clone(),
                format!(
                    "unknown signer role `{}` in signature placeholder (expected one of: {})",
                    ph.signer,
                    set.joined()
                ),
            ));
        }
        if !field_ok {
            scan.violations.push(n107(
                file,
                line,
                range,
                format!(
                    "unknown signature field `{}` in signature placeholder (expected one of: {})",
                    ph.field,
                    F107SignaturePlaceholders::FIELDS.join(", ")
                ),
            ));
        }
        if signer_ok && field_ok {
            scan.saw_valid = true;
            if ph.field == "signature" || ph.field == "initials" {
                scan.signed_roles.insert(ph.signer.clone());
            }
        }
    }
    scan
}

fn workflow_has_signing_state(workflow: &BTreeMap<String, BTreeMap<String, String>>) -> bool {
    workflow.keys().any(|state| {
        state == F107SignaturePlaceholders::SIGNING_STATE
            || state.starts_with(&format!("{}__", F107SignaturePlaceholders::SIGNING_STATE))
    })
}

/// True when the template's workflow declares a `sent_for_signature` (or
/// `sent_for_signature__*`) state. Shared with `spec_coherence`, whose
/// reverse anchor check requires a body anchor for every such template.
#[must_use]
pub fn declares_signing_state(contents: &str) -> bool {
    let Some(fm) = frontmatter::extract(contents) else {
        return false;
    };
    let Ok(parsed) = crate::questionnaire::deserialize_frontmatter::<FrontmatterShape>(fm) else {
        return false;
    };
    parsed
        .workflow
        .is_some_and(|workflow| workflow_has_signing_state(&workflow))
}

fn require_declared_placeholders(
    file: &SourceFile,
    set: &SignerSet,
    signed_roles: &std::collections::BTreeSet<String>,
    violations: &mut Vec<Violation>,
) {
    for signer in &set.signers {
        let role = &signer.role;
        if !signed_roles.contains(role.as_str()) {
            violations.push(n107(
                file,
                1,
                0..0,
                format!(
                    "declared signer role `{role}` has no placeholder in the body \
                     (expected `{{{{{role}.signature}}}}` or `{{{{{role}.initials}}}}`)"
                ),
            ));
        }
    }
}

fn cross_check_signing_state(
    file: &SourceFile,
    parsed: Option<FrontmatterShape>,
    saw_valid: bool,
    saw_relevant: bool,
    violations: &mut Vec<Violation>,
) {
    let has_signing_state = parsed
        .and_then(|p| p.workflow)
        .is_some_and(|wf| workflow_has_signing_state(&wf));
    if saw_valid && !has_signing_state {
        violations.push(n107(
            file,
            1,
            0..0,
            format!(
                "template draws a signature block but its workflow has no \
                 `{}` (or `{}__*`) state to collect the signature",
                F107SignaturePlaceholders::SIGNING_STATE,
                F107SignaturePlaceholders::SIGNING_STATE
            ),
        ));
    }
    if has_signing_state && !saw_relevant {
        violations.push(n107(
            file,
            1,
            0..0,
            format!(
                "workflow declares a `{}` state but the body carries no signature \
                 anchor (expected at least one `{{{{<signer>.<field>}}}}`, e.g. \
                 `{{{{client.signature}}}}`) for the tab to land on",
                F107SignaturePlaceholders::SIGNING_STATE
            ),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::{signature_placeholders, signer_set, F107SignaturePlaceholders, SignerDelivery};
    use crate::{Rule, SourceFile};
    use std::path::PathBuf;

    #[test]
    fn shipped_offboarding_letter_validates_without_a_signers_key() {
        let body = include_str!("../../templates/notations/neon_law/offboarding__letter.md");
        assert!(
            F107SignaturePlaceholders.lint(&file(body)).is_empty(),
            "the bundled closing letter must stay valid with the default signer set: {:?}",
            F107SignaturePlaceholders.lint(&file(body)),
        );
    }

    /// The implicit set keeps the shipped posture: the client signs
    /// embedded (subject to the notation's delivery), the firm is emailed.
    #[test]
    fn implicit_set_keeps_client_embedded_and_firm_emailed() {
        let body = "---\ntitle: T\n---\nbody";
        let set = signer_set(body).expect("the default set parses");
        assert!(!set.explicit);
        assert_eq!(
            set.declared("client").unwrap().delivery,
            SignerDelivery::Embedded
        );
        assert_eq!(
            set.declared("firm").unwrap().delivery,
            SignerDelivery::Emailed
        );
        assert_eq!(
            (
                set.declared("client").unwrap().routing_order,
                set.declared("firm").unwrap().routing_order
            ),
            (1, 2)
        );
    }

    /// Declared routing orders reorder the set: the envelope routes the
    /// employee before the client regardless of list order.
    #[test]
    fn declared_routing_order_sorts_the_set() {
        let body = "---\ntitle: Offer\nquestionnaire:\n  BEGIN:\n    _: person__client\n  \
                    person__client:\n    _: END\n  END: {}\nsigners:\n  - role: client\n    \
                    routing_order: 2\n  - role: employee\n    delivery: emailed\n    \
                    routing_order: 1\n---\nbody";
        let set = signer_set(body).expect("the declarations parse");
        let roles: Vec<&str> = set.roles().collect();
        assert_eq!(roles, ["employee", "client"]);
        assert_eq!(set.declared("employee").unwrap().routing_order, 1);
        assert_eq!(set.declared("client").unwrap().routing_order, 2);
    }

    #[test]
    fn routing_orders_must_form_a_permutation() {
        let body = "---\ntitle: T\nsigners:\n  - role: client\n    \
                    routing_order: 1\n  - role: firm\n    delivery: emailed\n    \
                    routing_order: 1\n---\nbody";
        let error = signer_set(body).expect_err("duplicate routing orders refuse");
        assert!(error.contains("permutation of 1..=2"), "{error}");
    }

    #[test]
    fn a_bare_third_party_role_is_refused_with_the_delivery_teaching() {
        let body = "---\ntitle: T\nsigners:\n  - client\n  - firm\n---\nbody";
        let error = signer_set(body).expect_err("a bare non-client role refuses");
        assert!(error.contains("delivery: emailed"), "{error}");
    }

    #[test]
    fn client_cannot_declare_emailed_delivery() {
        let body = "---\ntitle: T\nsigners:\n  - role: client\n    delivery: \
                    emailed\n---\nbody";
        let error = signer_set(body).expect_err("the client's delivery is the notation's");
        assert!(error.contains("chosen when the matter opens"), "{error}");
    }

    #[test]
    fn unknown_delivery_value_is_refused() {
        let body = "---\ntitle: T\nsigners:\n  - role: client\n  - role: firm\n    \
                    delivery: courier\n---\nbody";
        let error = signer_set(body).expect_err("courier is not a delivery");
        assert!(
            error.contains("expected `emailed` or `embedded`"),
            "{error}"
        );
    }

    #[test]
    fn identity_bindings_must_name_declared_states() {
        let good = "---\ntitle: T\nquestionnaire:\n  BEGIN:\n    _: END\n  END: {}\n  \
                    custom_text__employee_email: {}\nsigners:\n  - role: employee\n    \
                    delivery: emailed\n    email: custom_text__employee_email\n---\nbody";
        let set = signer_set(good).expect("the binding names a declared state");
        assert_eq!(
            set.declared("employee").unwrap().email.as_deref(),
            Some("custom_text__employee_email")
        );
        let bad = "---\ntitle: T\nsigners:\n  - role: employee\n    delivery: \
                    emailed\n    email: custom_text__nowhere\n---\nbody";
        let error = signer_set(bad).expect_err("undeclared binding state refuses");
        assert!(
            error.contains("not a declared questionnaire state"),
            "{error}"
        );
    }

    #[test]
    fn a_third_party_declaration_without_delivery_is_refused() {
        let body = "---\ntitle: T\nsigners:\n  - client\n  - role: firm\n---\nbody";
        let error = signer_set(body).expect_err("firm without delivery refuses");
        assert!(error.contains("add `delivery: emailed`"), "{error}");
    }

    fn file(body: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from("retainer.md"),
            contents: body.to_string(),
        }
    }

    /// A frontmatter block with a valid signing workflow, plus whatever
    /// body the test appends.
    fn with_signing_workflow(body: &str) -> String {
        format!(
            "---\ntitle: Retainer\nworkflow:\n  BEGIN:\n    created: sent_for_signature__pending\n  \
             sent_for_signature__pending:\n    signature_received: END\n  END: {{}}\n---\n{body}"
        )
    }

    #[test]
    fn parser_ignores_data_placeholders_without_a_dot() {
        let found = signature_placeholders("Hello {{client_name}} and {{project_name}}.");
        assert!(found.is_empty(), "data placeholders have no dot: {found:?}");
    }

    #[test]
    fn parser_extracts_signer_and_field_split_on_first_dot() {
        let found = signature_placeholders("{{client.signature}} {{firm.date}}");
        assert_eq!(found.len(), 2);
        assert_eq!(
            (found[0].signer.as_str(), found[0].field.as_str()),
            ("client", "signature")
        );
        assert_eq!(
            (found[1].signer.as_str(), found[1].field.as_str()),
            ("firm", "date")
        );
    }

    #[test]
    fn parser_offset_and_end_span_the_whole_token() {
        let body = "x {{client.signature}} y";
        let found = signature_placeholders(body);
        assert_eq!(found.len(), 1);
        assert_eq!(&body[found[0].offset..found[0].end], "{{client.signature}}");
    }

    #[test]
    fn parser_trims_inner_whitespace() {
        let found = signature_placeholders("{{  client.signature  }}");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].signer, "client");
        assert_eq!(found[0].field, "signature");
    }

    #[test]
    fn loop_variable_paths_are_not_signature_placeholders() {
        let body = "---\nquestionnaire:\n  BEGIN:\n    _: people__members\n  people__members:\n    _: END\n  END: {}\nworkflow:\n  BEGIN:\n    intake_submitted: lawyer_review\n  lawyer_review:\n    approved: END\n  END: {}\n---\n{{#for m in people__members}}{{m.name}} from {{m.city}}{{/for}}";
        assert!(
            F107SignaturePlaceholders.lint(&file(body)).is_empty(),
            "loop row fields are N115 data grammar, not N107 signature grammar"
        );
    }

    #[test]
    fn passes_valid_client_and_firm_blocks_with_signing_state() {
        let body = with_signing_workflow(
            "Signature: {{client.signature}} {{client.date}}\nCountersigned: {{firm.signature}}\n",
        );
        assert!(
            F107SignaturePlaceholders.lint(&file(&body)).is_empty(),
            "well-formed signature blocks with a signing state should pass",
        );
    }

    #[test]
    fn passes_initials_block() {
        let body = with_signing_workflow("Initials: {{client.initials}}\n");
        assert!(F107SignaturePlaceholders.lint(&file(&body)).is_empty());
    }

    #[test]
    fn no_frontmatter_means_no_violation() {
        // Arbitrary prose with a dotted token is NOT a template.
        let v = F107SignaturePlaceholders.lint(&file("see {{config.value}} in the docs"));
        assert!(v.is_empty());
    }

    #[test]
    fn flags_unknown_signer_role() {
        let body = with_signing_workflow("{{spouse.signature}}\n");
        let v = F107SignaturePlaceholders.lint(&file(&body));
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].code, "N107");
        assert!(v[0].message.contains("unknown signer role `spouse`"));
    }

    #[test]
    fn a_firm_legal_name_binding_is_not_a_signature_field() {
        let body = "---\nkind: letter\ntitle: Letter\n---\n\n{{firm.legal_name}}\n";
        assert!(F107SignaturePlaceholders.lint(&file(body)).is_empty());
    }

    #[test]
    fn flags_unknown_field_type() {
        let body = with_signing_workflow("{{client.fingerprint}}\n");
        let v = F107SignaturePlaceholders.lint(&file(&body));
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0]
            .message
            .contains("unknown signature field `fingerprint`"));
    }

    #[test]
    fn rejects_person_name_signer() {
        // Role, never a name: `{{nick.signature}}` is not a valid block.
        let body = with_signing_workflow("{{nick.signature}}\n");
        let v = F107SignaturePlaceholders.lint(&file(&body));
        assert_eq!(v.len(), 1);
        assert!(v[0].message.contains("unknown signer role `nick`"));
    }

    #[test]
    fn flags_signature_block_without_signing_workflow_state() {
        // Valid grammar, but the workflow has no place to collect it.
        let body = "---\ntitle: Retainer\nworkflow:\n  BEGIN:\n    created: lawyer_review\n  \
                    lawyer_review:\n    approved: END\n  END: {}\n---\n{{client.signature}}\n";
        let v = F107SignaturePlaceholders.lint(&file(body));
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].message.contains("no `sent_for_signature`"));
        assert_eq!(v[0].line, 1);
    }

    #[test]
    fn flags_signing_state_without_a_body_anchor() {
        // Reverse direction: the workflow declares a signing state but
        // the body has no signature anchor for the tab to land on — the
        // mirror of the live retainer bug (signing step, no placed tab).
        let body = "---\ntitle: Retainer\nworkflow:\n  BEGIN:\n    \
                    created: sent_for_signature__pending\n  \
                    sent_for_signature__pending:\n    signature_received: END\n  \
                    END: {}\n---\nDear {{client_name}}, please sign offline.\n";
        let v = F107SignaturePlaceholders.lint(&file(body));
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].code, "N107");
        assert!(
            v[0].message.contains("no signature anchor"),
            "message was: {}",
            v[0].message
        );
        assert_eq!(v[0].line, 1);
    }

    #[test]
    fn signing_state_with_a_malformed_anchor_does_not_double_flag_reverse() {
        // A present-but-malformed token draws its own unknown-signer
        // violation; the reverse "no anchor" check must stay silent so
        // the author gets one clear message, not a contradictory pair.
        let body = "---\ntitle: R\nworkflow:\n  BEGIN:\n    \
                    created: sent_for_signature\n  sent_for_signature:\n    \
                    signature_received: END\n  END: {}\n---\n{{spouse.signature}}\n";
        let v = F107SignaturePlaceholders.lint(&file(body));
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].message.contains("unknown signer role"));
        assert!(
            !v.iter().any(|x| x.message.contains("no signature anchor")),
            "reverse check must not fire when a (malformed) anchor is present",
        );
    }

    #[test]
    fn signing_state_with_only_loop_tokens_still_flags_missing_anchor() {
        // A signing workflow whose body uses a `{{#for}}` loop but forgets
        // its real signature block. The loop row tokens (`{{m.name}}`) are
        // N115 data grammar, not signature anchors, so the reverse check
        // must still fire — else `expand_signatures` places zero tabs and
        // the template reaches the provider with a signing step and nowhere
        // to sign (the very bug this guard exists to catch).
        let body = "---\ntitle: Retainer\nquestionnaire:\n  BEGIN:\n    _: people__members\n  \
                    people__members:\n    _: END\n  END: {}\nworkflow:\n  BEGIN:\n    \
                    created: sent_for_signature__pending\n  \
                    sent_for_signature__pending:\n    signature_received: END\n  \
                    END: {}\n---\nMembers:\n{{#for m in people__members}}- {{m.name}}\n{{/for}}\n";
        let v = F107SignaturePlaceholders.lint(&file(body));
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].code, "N107");
        assert!(
            v[0].message.contains("no signature anchor"),
            "message was: {}",
            v[0].message
        );
        assert_eq!(v[0].line, 1);
    }

    #[test]
    fn bare_sent_for_signature_state_satisfies_cross_check() {
        let body = "---\ntitle: R\nworkflow:\n  BEGIN:\n    created: sent_for_signature\n  \
                    sent_for_signature:\n    signature_received: END\n  END: {}\n---\n\
                    {{client.signature}}\n";
        assert!(F107SignaturePlaceholders.lint(&file(body)).is_empty());
    }

    #[test]
    fn reports_violation_at_the_token_line() {
        let body = with_signing_workflow("line one\nline two has {{client.oops}}\n");
        let v = F107SignaturePlaceholders.lint(&file(&body));
        assert_eq!(v.len(), 1);
        // The frontmatter is 6 lines; body line "line one" follows, then
        // the offending token is on the next line. Assert it is NOT 1.
        assert!(
            v[0].line > 1,
            "violation should point at the token, not line 1"
        );
    }

    #[test]
    fn no_signature_blocks_means_workflow_cross_check_is_silent() {
        // A template with only data placeholders never triggers the
        // signing-state requirement.
        let body = "---\ntitle: T\nworkflow:\n  BEGIN:\n    created: END\n  END: {}\n---\n\
                    Dear {{client_name}}, welcome.\n";
        assert!(F107SignaturePlaceholders.lint(&file(body)).is_empty());
    }

    /// A three-party assignment: client, firm, and a confirming assignor.
    /// Questionnaire states back the respondent-side roles; `firm` is the
    /// configured countersignature. Fixtures use the sample transactional
    /// matter, never a live Project code.
    fn three_party_assignment(signers_yaml: &str, extra_body: &str) -> String {
        format!(
            "---\ntitle: Assignment\n{signers_yaml}questionnaire:\n  BEGIN:\n    \
             _: person__client\n  person__client:\n    _: person__confirming_assignor\n  \
             person__confirming_assignor:\n    _: END\n  END: {{}}\nworkflow:\n  BEGIN:\n    \
             created: sent_for_signature__pending\n  sent_for_signature__pending:\n    \
             signature_received: END\n  END: {{}}\n---\n\
             {{{{client.signature}}}}\n{{{{firm.signature}}}}\n{{{{confirming_assignor.signature}}}}\n\
             {extra_body}"
        )
    }

    #[test]
    fn declared_three_party_signer_set_validates() {
        let body = three_party_assignment(
            "signers:\n  - client\n  - role: firm\n    delivery: emailed\n  - role: \
             confirming_assignor\n    delivery: emailed\n",
            "",
        );
        assert!(
            F107SignaturePlaceholders.lint(&file(&body)).is_empty(),
            "a template that declares its three signer roles must lint clean: {:?}",
            F107SignaturePlaceholders.lint(&file(&body)),
        );
    }

    #[test]
    fn absent_signers_rejects_a_third_role_and_names_the_permitted_set() {
        let body = three_party_assignment("", "");
        let v = F107SignaturePlaceholders.lint(&file(&body));
        assert!(
            v.iter().any(|x| {
                x.code == "N107"
                    && x.message
                        .contains("unknown signer role `confirming_assignor`")
                    && x.message.contains("client, firm")
            }),
            "absent signers: must permit only client, firm; got {v:?}"
        );
    }

    #[test]
    fn declared_role_without_a_placeholder_is_an_error() {
        let body = "---\ntitle: Assignment\nsigners:\n  - client\n  - role: firm\n    delivery: \
                    emailed\n  - role: confirming_assignor\n    delivery: emailed\n\
                    questionnaire:\n  BEGIN:\n    _: person__client\n  person__client:\n    \
                    _: person__confirming_assignor\n  person__confirming_assignor:\n    _: END\n  \
                    END: {}\nworkflow:\n  BEGIN:\n    created: sent_for_signature__pending\n  \
                    sent_for_signature__pending:\n    signature_received: END\n  END: {}\n---\n\
                    {{client.signature}}\n{{firm.signature}}\n";
        let v = F107SignaturePlaceholders.lint(&file(body));
        assert!(
            v.iter().any(|x| {
                x.code == "N107"
                    && x.message.contains("confirming_assignor")
                    && x.message.contains("no placeholder")
            }),
            "a declared role with no body placeholder must fail N107; got {v:?}"
        );
    }

    #[test]
    fn shipped_onboarding_letter_validates_without_a_signers_key() {
        let body = include_str!("../../templates/notations/neon_law/onboarding__letter.md");
        assert!(
            F107SignaturePlaceholders.lint(&file(body)).is_empty(),
            "the bundled onboarding letter must stay valid with the default signer set: {:?}",
            F107SignaturePlaceholders.lint(&file(body)),
        );
    }
}
