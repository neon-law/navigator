//! Explicit-ID, read-only Xero Accounting API evidence. Grounded in Xero's
//! official OpenAPI: InvoiceID, PaymentID, BankTransactionID and BankTransferID
//! are distinct identities; Amount and BankAmount are distinct currencies.
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::value::RawValue;
use uuid::Uuid;
#[derive(Debug, thiserror::Error)]
pub enum EvidenceError {
    #[error("Xero evidence request failed")]
    Transport,
    #[error("Xero evidence unavailable ({0})")]
    Http(reqwest::StatusCode),
    #[error("Xero evidence has invalid identity, precision, or shape")]
    Invalid,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Account {
    #[serde(rename = "AccountID")]
    pub account_id: Uuid,
    pub currency_code: String,
    pub status: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Invoice {
    pub date: Option<String>,
    #[serde(rename = "InvoiceID")]
    pub invoice_id: Uuid,
    pub invoice_number: Option<String>,
    pub currency_code: String,
    pub status: String,
    pub total: Box<RawValue>,
    pub amount_paid: Box<RawValue>,
    pub amount_due: Box<RawValue>,
    #[serde(default)]
    pub payments: Vec<PaymentReference>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PaymentReference {
    #[serde(rename = "PaymentID")]
    pub payment_id: Uuid,
    pub amount: Box<RawValue>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct AccountReference {
    #[serde(rename = "AccountID")]
    pub account_id: Uuid,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct InvoiceReference {
    #[serde(rename = "InvoiceID")]
    pub invoice_id: Uuid,
    pub invoice_number: Option<String>,
    pub currency_code: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CreditNote {
    pub date: Option<String>,
    #[serde(rename = "CreditNoteID")]
    pub credit_note_id: Uuid,
    pub credit_note_number: Option<String>,
    pub currency_code: String,
    pub status: String,
    pub total: Box<RawValue>,
    pub remaining_credit: Box<RawValue>,
    #[serde(default)]
    pub payments: Vec<PaymentReference>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CreditNoteReference {
    #[serde(rename = "CreditNoteID")]
    pub credit_note_id: Uuid,
    pub credit_note_number: Option<String>,
    pub currency_code: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Payment {
    pub date: Option<String>,
    #[serde(rename = "PaymentID")]
    pub payment_id: Uuid,
    pub invoice: Option<InvoiceReference>,
    pub credit_note: Option<CreditNoteReference>,
    pub account: AccountReference,
    pub amount: Box<RawValue>,
    pub bank_amount: Option<Box<RawValue>>,
    pub currency_rate: Option<Box<RawValue>>,
    pub payment_type: String,
    pub status: String,
    pub is_reconciled: Option<bool>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct BankTransaction {
    pub date: Option<String>,
    #[serde(rename = "BankTransactionID")]
    pub bank_transaction_id: Uuid,
    pub bank_account: AccountReference,
    #[serde(rename = "Type")]
    pub transaction_type: String,
    pub status: String,
    pub currency_code: String,
    pub total: Box<RawValue>,
    pub currency_rate: Option<Box<RawValue>>,
    pub is_reconciled: Option<bool>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct BankTransfer {
    pub date: Option<String>,
    pub status: Option<String>,
    pub from_is_reconciled: Option<bool>,
    pub to_is_reconciled: Option<bool>,
    pub currency_rate: Option<Box<RawValue>>,
    #[serde(rename = "BankTransferID")]
    pub bank_transfer_id: Uuid,
    pub from_bank_account: AccountReference,
    pub to_bank_account: AccountReference,
    pub amount: Box<RawValue>,
    #[serde(rename = "FromBankTransactionID")]
    pub from_bank_transaction_id: Uuid,
    #[serde(rename = "ToBankTransactionID")]
    pub to_bank_transaction_id: Uuid,
}
#[derive(Clone, Copy)]
pub enum AttachmentParent {
    Invoice,
    CreditNote,
    BankTransaction,
    BankTransfer,
}
impl AttachmentParent {
    fn collection(self) -> &'static str {
        match self {
            Self::Invoice => "Invoices",
            Self::CreditNote => "CreditNotes",
            Self::BankTransaction => "BankTransactions",
            Self::BankTransfer => "BankTransfers",
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Attachment {
    #[serde(rename = "AttachmentID")]
    pub attachment_id: Uuid,
    pub file_name: String,
    pub mime_type: String,
    pub content_length: u64,
}
/// Carries no write or list-all operation. The credential is not printable.
pub struct XeroEvidence {
    client: reqwest::Client,
    token: String,
    tenant: Uuid,
    base: String,
}
impl XeroEvidence {
    pub fn new(token: &str, tenant: Uuid) -> Result<Self, EvidenceError> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| EvidenceError::Transport)?,
            token: token.into(),
            tenant,
            base: "https://api.xero.com/api.xro/2.0".into(),
        })
    }
    async fn get<T: DeserializeOwned>(
        &self,
        collection: &str,
        id: Uuid,
    ) -> Result<(T, Vec<u8>), EvidenceError> {
        let response = self
            .client
            .get(format!("{}/{collection}/{id}", self.base))
            .header("Xero-Tenant-Id", self.tenant.to_string())
            .header("Accept", "application/json")
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| EvidenceError::Transport)?;
        if !response.status().is_success() {
            return Err(EvidenceError::Http(response.status()));
        }
        let bytes = crate::mercury::bounded(response, 1_048_576)
            .await
            .map_err(|_| EvidenceError::Invalid)?;
        // Keep the selected object's raw JSON so numeric tokens remain exact.
        let envelope: std::collections::BTreeMap<String, Box<RawValue>> =
            serde_json::from_slice(&bytes).map_err(|_| EvidenceError::Invalid)?;
        let objects: Vec<Box<RawValue>> = serde_json::from_str(
            envelope
                .get(collection)
                .ok_or(EvidenceError::Invalid)?
                .get(),
        )
        .map_err(|_| EvidenceError::Invalid)?;
        if objects.len() != 1 {
            return Err(EvidenceError::Invalid);
        }
        Ok((
            serde_json::from_str(objects[0].get()).map_err(|_| EvidenceError::Invalid)?,
            bytes,
        ))
    }
    /// Read attachments only beneath an explicitly selected accounting object.
    pub async fn attachments(
        &self,
        parent: AttachmentParent,
        id: Uuid,
    ) -> Result<(Vec<Attachment>, Vec<u8>), EvidenceError> {
        #[derive(Deserialize)]
        struct Envelope {
            #[serde(rename = "Attachments")]
            attachments: Vec<Attachment>,
        }
        let response = self
            .client
            .get(format!(
                "{}/{}/{id}/Attachments",
                self.base,
                parent.collection()
            ))
            .header("Xero-Tenant-Id", self.tenant.to_string())
            .header("Accept", "application/json")
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| EvidenceError::Transport)?;
        if !response.status().is_success() {
            return Err(EvidenceError::Http(response.status()));
        }
        let bytes = crate::mercury::bounded(response, 1_048_576)
            .await
            .map_err(|_| EvidenceError::Invalid)?;
        let envelope: Envelope =
            serde_json::from_slice(&bytes).map_err(|_| EvidenceError::Invalid)?;
        if envelope.attachments.len() > 100
            || envelope.attachments.iter().any(|a| {
                a.content_length > 16 * 1024 * 1024
                    || a.mime_type.len() > 128
                    || a.file_name.len() > 512
            })
        {
            return Err(EvidenceError::Invalid);
        }
        Ok((envelope.attachments, bytes))
    }
    pub async fn attachment(
        &self,
        parent: AttachmentParent,
        id: Uuid,
        attachment: &Attachment,
    ) -> Result<Vec<u8>, EvidenceError> {
        let response = self
            .client
            .get(format!(
                "{}/{}/{id}/Attachments/{}",
                self.base,
                parent.collection(),
                attachment.attachment_id
            ))
            .header("Xero-Tenant-Id", self.tenant.to_string())
            .header("Accept", &attachment.mime_type)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| EvidenceError::Transport)?;
        if !response.status().is_success() {
            return Err(EvidenceError::Http(response.status()));
        }
        let bytes = crate::mercury::bounded(response, 16 * 1024 * 1024)
            .await
            .map_err(|_| EvidenceError::Invalid)?;
        if u64::try_from(bytes.len()).ok() != Some(attachment.content_length) {
            return Err(EvidenceError::Invalid);
        }
        Ok(bytes)
    }
    pub async fn account(&self, id: Uuid) -> Result<(Account, Vec<u8>), EvidenceError> {
        let (a, b) = self.get::<Account>("Accounts", id).await?;
        if a.account_id != id {
            return Err(EvidenceError::Invalid);
        }
        Ok((a, b))
    }
    pub async fn invoice(&self, id: Uuid) -> Result<(Invoice, Vec<u8>), EvidenceError> {
        let (a, b) = self.get::<Invoice>("Invoices", id).await?;
        if a.invoice_id != id {
            return Err(EvidenceError::Invalid);
        }
        Ok((a, b))
    }
    pub async fn credit_note(&self, id: Uuid) -> Result<(CreditNote, Vec<u8>), EvidenceError> {
        let (note, bytes): (CreditNote, _) = self.get("CreditNotes", id).await?;
        if note.credit_note_id != id {
            return Err(EvidenceError::Invalid);
        }
        Ok((note, bytes))
    }
    pub async fn payment(
        &self,
        id: Uuid,
        account: Uuid,
    ) -> Result<(Payment, Vec<u8>), EvidenceError> {
        let (a, b) = self.get::<Payment>("Payments", id).await?;
        if a.payment_id != id || a.account.account_id != account {
            return Err(EvidenceError::Invalid);
        }
        Ok((a, b))
    }
    pub async fn bank_transaction(
        &self,
        id: Uuid,
        account: Uuid,
    ) -> Result<(BankTransaction, Vec<u8>), EvidenceError> {
        let (a, b) = self.get::<BankTransaction>("BankTransactions", id).await?;
        if a.bank_transaction_id != id || a.bank_account.account_id != account {
            return Err(EvidenceError::Invalid);
        }
        Ok((a, b))
    }
    pub async fn bank_transfer(&self, id: Uuid) -> Result<(BankTransfer, Vec<u8>), EvidenceError> {
        let (a, b) = self.get::<BankTransfer>("BankTransfers", id).await?;
        if a.bank_transfer_id != id {
            return Err(EvidenceError::Invalid);
        }
        Ok((a, b))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    #[tokio::test]
    async fn evidence_and_attachment_redirects_stay_on_the_selected_provider() {
        let source = MockServer::start().await;
        let target = MockServer::start().await;
        let tenant = Uuid::now_v7();
        let id = Uuid::now_v7();
        let attachment = Attachment {
            attachment_id: Uuid::now_v7(),
            file_name: "synthetic.pdf".into(),
            mime_type: "application/pdf".into(),
            content_length: 2,
        };
        for request_path in [
            format!("/Invoices/{id}"),
            format!("/Invoices/{id}/Attachments"),
            format!("/Invoices/{id}/Attachments/{}", attachment.attachment_id),
        ] {
            Mock::given(method("GET"))
                .and(path(request_path))
                .and(header("Authorization", "Bearer synthetic"))
                .and(header("Xero-Tenant-Id", tenant.to_string()))
                .respond_with(ResponseTemplate::new(307).insert_header("Location", target.uri()))
                .expect(1)
                .mount(&source)
                .await;
        }
        let mut client = XeroEvidence::new("synthetic", tenant).unwrap();
        assert_eq!(client.base, "https://api.xero.com/api.xro/2.0");
        client.base = source.uri();
        assert!(
            matches!(client.invoice(id).await, Err(EvidenceError::Http(status)) if status.as_u16() == 307)
        );
        assert!(
            matches!(client.attachments(AttachmentParent::Invoice, id).await, Err(EvidenceError::Http(status)) if status.as_u16() == 307)
        );
        assert!(
            matches!(client.attachment(AttachmentParent::Invoice, id, &attachment).await, Err(EvidenceError::Http(status)) if status.as_u16() == 307)
        );
        assert!(target.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn credit_note_identity_and_refund_parent_are_distinct_from_invoice() {
        let server = MockServer::start().await;
        let tenant = Uuid::now_v7();
        let id = Uuid::now_v7();
        Mock::given(method("GET")).and(path(format!("/CreditNotes/{id}"))).and(header("Xero-Tenant-Id",tenant.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"CreditNotes":[{"CreditNoteID":id,"CreditNoteNumber":"CN-SYNTHETIC","CurrencyCode":"CAD","Status":"AUTHORISED","Total":12.01,"RemainingCredit":2.01}]}))).expect(1).mount(&server).await;
        let mut client = XeroEvidence::new("synthetic", tenant).unwrap();
        client.base = server.uri();
        let (note, _) = client.credit_note(id).await.unwrap();
        assert_eq!(note.credit_note_id, id);
        assert_eq!(note.remaining_credit.get(), "2.01");
        let payment: Payment=serde_json::from_value(serde_json::json!({"PaymentID":Uuid::now_v7(),"CreditNote":{"CreditNoteID":id,"CurrencyCode":"CAD"},"Account":{"AccountID":Uuid::now_v7()},"Amount":10,"PaymentType":"ARCRPAYMENT","Status":"AUTHORISED"})).unwrap();
        assert!(payment.invoice.is_none());
        assert_eq!(payment.credit_note.unwrap().credit_note_id, id);
        assert_eq!(AttachmentParent::CreditNote.collection(), "CreditNotes");
    }
    #[tokio::test]
    async fn payment_identity_tenant_and_bank_amount_are_separate_from_invoice() {
        let server = MockServer::start().await;
        let tenant = Uuid::now_v7();
        let payment = Uuid::now_v7();
        let account = Uuid::now_v7();
        let invoice = Uuid::now_v7();
        Mock::given(method("GET")).and(path(format!("/Payments/{payment}"))).and(header("Xero-Tenant-Id",tenant.to_string())).and(header("Accept","application/json")).respond_with(ResponseTemplate::new(200).set_body_string(format!(r#"{{"Payments":[{{"PaymentID":"{payment}","Invoice":{{"InvoiceID":"{invoice}","InvoiceNumber":"INV-SYNTHETIC","CurrencyCode":"EUR"}},"Account":{{"AccountID":"{account}"}},"Amount":10.00,"BankAmount":12.01,"CurrencyRate":0.8326394671,"PaymentType":"ACCRECPAYMENT","Status":"AUTHORISED","IsReconciled":false}}]}}"#))).expect(2).mount(&server).await;
        let mut client = XeroEvidence::new("synthetic", tenant).unwrap();
        client.base = server.uri();
        let (p, _) = client.payment(payment, account).await.unwrap();
        assert_eq!(p.amount.get(), "10.00");
        assert_eq!(p.bank_amount.unwrap().get(), "12.01");
        assert_eq!(p.invoice.unwrap().invoice_id, invoice);
        assert_eq!(p.is_reconciled, Some(false));
        assert!(client.payment(payment, Uuid::now_v7()).await.is_err());
    }
    #[tokio::test]
    async fn attachments_remain_beneath_the_explicit_object_and_validate_length() {
        let server = MockServer::start().await;
        let tenant = Uuid::now_v7();
        let invoice = Uuid::now_v7();
        let attachment_id = Uuid::now_v7();
        Mock::given(method("GET")).and(path(format!("/Invoices/{invoice}/Attachments")))
            .and(header("Xero-Tenant-Id", tenant.to_string())).and(header("Authorization", "Bearer synthetic"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"Attachments":[{
                "AttachmentID":attachment_id,"FileName":"synthetic.pdf","MimeType":"application/pdf","ContentLength":2
            }]}))).expect(1).mount(&server).await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/Invoices/{invoice}/Attachments/{attachment_id}"
            )))
            .and(header("Xero-Tenant-Id", tenant.to_string()))
            .and(header("Accept", "application/pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"xy".to_vec()))
            .expect(2)
            .mount(&server)
            .await;
        let mut client = XeroEvidence::new("synthetic", tenant).unwrap();
        client.base = server.uri();
        let (mut attachments, _) = client
            .attachments(AttachmentParent::Invoice, invoice)
            .await
            .unwrap();
        assert_eq!(
            client
                .attachment(AttachmentParent::Invoice, invoice, &attachments[0])
                .await
                .unwrap(),
            b"xy"
        );
        attachments[0].content_length = 3;
        assert!(client
            .attachment(AttachmentParent::Invoice, invoice, &attachments[0])
            .await
            .is_err());
    }
}
