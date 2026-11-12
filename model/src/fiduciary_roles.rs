//! The pure half of `store::fiduciary_roles`.

/// Every role the schema admits, in the order a roster lists them. The
/// `ASSERT` on `fiduciary_role.role` is the engine's copy of this list.
pub const ROLES: [&str; 7] = [
    "trustee",
    "successor_trustee",
    "executor",
    "alternate_executor",
    "agent_poa",
    "health_care_agent",
    "guardian",
];
