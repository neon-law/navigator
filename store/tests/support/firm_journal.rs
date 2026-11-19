use store::firm_journal::*;
use uuid::Uuid;

pub fn entry(firm: Uuid, key: &str) -> JournalEntry {
    JournalEntry {
        id: Uuid::now_v7(),
        firm_id: firm,
        key: key.into(),
        actor_id: Uuid::now_v7(),
        occurred_at: "2026-10-11T00:00:00Z".into(),
        memo: "Synthetic receipt".into(),
        postings: vec![
            Posting {
                account: "bank".into(),
                ownership: store::firm_journal::BeneficialOwnership::Firm,
                restrictions: vec![],
                unit: Unit::fiat("USD", 2),
                quantity: "10000".into(),
                project_id: None,
                category: Category::Cash,
            },
            Posting {
                account: "client-liability".into(),
                ownership: store::firm_journal::BeneficialOwnership::Client,
                restrictions: vec![],
                unit: Unit::fiat("USD", 2),
                quantity: "-10000".into(),
                project_id: None,
                category: Category::Trust,
            },
        ],
        sources: vec![],
        receipts: vec![],
        links: vec![],
        reversal_of: None,
    }
}
