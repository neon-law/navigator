/// Every reason the schema admits, in the schema's order. The `ASSERT` on
/// `project_reference.reason` is the engine's copy of this list.
pub const REASONS: [&str; 5] = [
    REASON_COUNTERPARTY_OFFICER,
    REASON_REGISTERED_AGENT,
    REASON_SIGNATORY,
    REASON_ADVERSE_PARTY,
    REASON_COUNTERPARTY_ENTITY,
];

/// Whether `reason` is one the schema admits.
#[must_use]
pub fn is_reason(reason: &str) -> bool {
    REASONS.contains(&reason)
}

/// Reason: an officer, member, or manager of the counterparty.
pub const REASON_COUNTERPARTY_OFFICER: &str = "counterparty_officer";

/// Reason: the counterparty's registered agent for service.
pub const REASON_REGISTERED_AGENT: &str = "registered_agent";

/// Reason: the individual who signed the instrument.
pub const REASON_SIGNATORY: &str = "signatory";

/// Reason: the party the client is adverse to.
pub const REASON_ADVERSE_PARTY: &str = "adverse_party";

/// Reason: the counterparty organization itself.
pub const REASON_COUNTERPARTY_ENTITY: &str = "counterparty_entity";
