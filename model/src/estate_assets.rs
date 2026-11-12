//! The pure half of `store::estate_assets`.

/// How an asset passes at death. The `ASSERT` on `estate_asset.passes_by` is
/// the engine's copy of this list.
pub const PASSES_BY: [&str; 5] = ["trust", "beneficiary_designation", "will", "joint", "other"];

/// Whether an asset is funded into the plan. The `ASSERT` on
/// `estate_asset.funding_state` is the engine's copy of this list.
pub const FUNDING_STATES: [&str; 4] = ["funded", "in_progress", "not_funded", "consent_needed"];
