//! One Stripe Customer per billed Person in each of the Firm's Stripe
//! accounts (ENG-1342, LAW-571): saved the moment Stripe returns it, so the
//! next draft on the same account reuses it instead of creating another.
use uuid::Uuid;

use crate::postgres::PostgresDb;

/// The saved Customer for `person` on `account`.
///
/// # Errors
/// A database failure.
pub async fn find(
    pg: &PostgresDb,
    firm_id: Uuid,
    account: &str,
    person: Uuid,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT customer_id FROM stripe_customer WHERE firm_id=$1 AND stripe_account_id=$2 AND person_id=$3",
    )
    .bind(firm_id)
    .bind(account)
    .bind(person)
    .fetch_optional(&pg.0)
    .await
}

/// Save the Customer Stripe returned. A Customer Stripe replaced (deleted
/// or recreated) is updated in place; the row stays one per account and
/// Person.
///
/// # Errors
/// A database failure.
pub async fn save(
    pg: &PostgresDb,
    firm_id: Uuid,
    account: &str,
    person: Uuid,
    customer: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO stripe_customer (firm_id,stripe_account_id,person_id,customer_id)
         VALUES ($1,$2,$3,$4)
         ON CONFLICT (stripe_account_id,person_id)
         DO UPDATE SET customer_id=excluded.customer_id, updated_at=now()
         WHERE stripe_customer.firm_id=excluded.firm_id",
    )
    .bind(firm_id)
    .bind(account)
    .bind(person)
    .bind(customer)
    .execute(&pg.0)
    .await?;
    Ok(())
}
