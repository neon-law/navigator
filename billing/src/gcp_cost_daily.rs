//! Read one UTC billing-export partition without creating a `BigQuery` job.
use std::collections::BTreeMap;
use std::str::FromStr;

use anyhow::{ensure, Context, Result};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::gcp_cost::{AdcToken, TokenProvider};

const FIELDS: [&str; 6] = [
    "service.description",
    "project.id",
    "cost",
    "currency",
    "credits.amount",
    "invoice.month",
];

/// A fully qualified standard or detailed billing export table.
#[derive(Clone)]
pub struct ExportTable {
    reference: String,
    reader: String,
}

impl ExportTable {
    pub fn parse(reference: &str, reader: &str) -> Result<Self> {
        let parts: Vec<_> = reference.split('.').collect();
        ensure!(
            parts.len() == 3
                && parts.iter().all(|part| !part.is_empty()
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))),
            "expected a project.dataset.table billing export"
        );
        let (name, domain) = reader
            .split_once('@')
            .context("expected a billing reader service account")?;
        let project = domain
            .strip_suffix(".iam.gserviceaccount.com")
            .context("expected a billing reader service account")?;
        ensure!(
            [name, project].iter().all(|s| !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')),
            "invalid billing reader service account"
        );
        Ok(Self {
            reference: reference.to_owned(),
            reader: reader.to_owned(),
        })
    }

    #[must_use]
    pub fn reference(&self) -> &str {
        &self.reference
    }

    #[must_use]
    pub fn reader(&self) -> &str {
        &self.reader
    }

    pub async fn aggregate(&self, date: NaiveDate) -> Result<Vec<DailyCost>> {
        let runtime = AdcToken::new("https://www.googleapis.com/auth/cloud-platform").await?;
        let token = self
            .reader_token("https://iamcredentials.googleapis.com", &runtime)
            .await?;

        self.read("https://bigquery.googleapis.com/bigquery/v2", date, &token)
            .await
    }

    async fn reader_token(&self, base: &str, runtime: &dyn TokenProvider) -> Result<String> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct AccessToken {
            access_token: String,
        }
        let url = format!(
            "{base}/v1/projects/-/serviceAccounts/{}:generateAccessToken",
            self.reader
        );
        let request = serde_json::json!({
            "scope": ["https://www.googleapis.com/auth/bigquery.readonly"],
            "lifetime": "3600s"
        });
        let token: AccessToken = reqwest::Client::new()
            .post(url)
            .bearer_auth(runtime.token().await?)
            .json(&request)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(token.access_token)
    }

    async fn read(&self, base: &str, date: NaiveDate, token: &str) -> Result<Vec<DailyCost>> {
        let http = reqwest::Client::new();
        let parts: Vec<_> = self.reference.split('.').collect();
        let table = format!(
            "{base}/projects/{}/datasets/{}/tables/{}",
            parts[0], parts[1], parts[2]
        );
        let fields = selected_fields(&http, &table, token).await?;
        let column = |path: &str| {
            fields
                .iter()
                .position(|field| *field == path)
                .context("incomplete billing export schema")
        };
        let service = column("service.description")?;
        let project = column("project.id")?;
        let invoice = column("invoice.month")?;
        let currency = column("currency")?;
        let cost = column("cost")?;
        let credits = column("credits.amount")?;
        let selected = fields.join(",");
        let partition = format!("{table}${}/data", date.format("%Y%m%d"));
        let mut totals = BTreeMap::new();
        let mut page = None::<String>;
        loop {
            let mut request = http.get(&partition).query(&[
                ("selectedFields", selected.as_str()),
                ("maxResults", "10000"),
            ]);
            if let Some(after) = &page {
                request = request.query(&[("pageToken", after)]);
            }
            let response: Page = request
                .bearer_auth(token)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            for row in response.rows {
                let cell = |index: usize| {
                    row.f
                        .get(index)
                        .map(|cell| &cell.v)
                        .context("billing row missing a selected column")
                };
                let key = (
                    nested(cell(invoice)?)?
                        .context("missing invoice month")?
                        .to_owned(),
                    nested(cell(project)?)?.map(str::to_owned),
                    nested(cell(service)?)?
                        .context("missing service description")?
                        .to_owned(),
                    cell(currency)?
                        .as_str()
                        .context("missing billing currency")?
                        .to_owned(),
                );
                let (gross, credit) = totals.entry(key).or_insert((Decimal::ZERO, Decimal::ZERO));
                *gross = gross
                    .checked_add(amount(cell(cost)?.as_str())?)
                    .context("gross amount overflow")?;
                for item in cell(credits)?
                    .as_array()
                    .context("invalid billing credits")?
                {
                    *credit = credit
                        .checked_add(amount(nested(&item["v"])?)?)
                        .context("credit amount overflow")?;
                }
            }
            let Some(next) = response.page_token.filter(|t| !t.is_empty()) else {
                break;
            };
            ensure!(
                page.as_ref() != Some(&next),
                "billing export repeated a page"
            );
            page = Some(next);
        }
        totals
            .into_iter()
            .map(
                |((invoice_month, project, service, currency), (gross, credits))| {
                    Ok(DailyCost {
                        invoice_month,
                        project,
                        service,
                        currency,
                        gross,
                        credits,
                        net: gross.checked_add(credits).context("net amount overflow")?,
                    })
                },
            )
            .collect()
    }
}

async fn selected_fields(
    http: &reqwest::Client,
    table: &str,
    token: &str,
) -> Result<Vec<&'static str>> {
    let metadata: Value = http
        .get(table)
        .query(&[("fields", "schema(fields(name)),timePartitioning(type)")])
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    ensure!(
        metadata
            .pointer("/timePartitioning/type")
            .and_then(Value::as_str)
            == Some("DAY"),
        "billing export requires daily partitions"
    );
    // TableDataList returns cells in table schema order. Request the same order,
    // projecting just one leaf from each nested record.
    let schema = metadata
        .pointer("/schema/fields")
        .and_then(Value::as_array)
        .context("missing billing export schema")?;
    Ok(schema
        .iter()
        .filter_map(|field| {
            let name = field["name"].as_str()?;
            FIELDS
                .into_iter()
                .find(|path| path.split('.').next() == Some(name))
        })
        .collect())
}

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    rows: Vec<Row>,
    #[serde(rename = "pageToken")]
    page_token: Option<String>,
}

#[derive(Deserialize)]
struct Row {
    f: Vec<Cell>,
}

#[derive(Deserialize)]
struct Cell {
    v: Value,
}

fn nested(value: &Value) -> Result<Option<&str>> {
    if value.is_null() {
        return Ok(None);
    }
    let leaf = value
        .pointer("/f/0/v")
        .context("invalid nested billing cell")?;
    if leaf.is_null() {
        Ok(None)
    } else {
        Ok(Some(leaf.as_str().context("invalid billing label")?))
    }
}

fn amount(value: Option<&str>) -> Result<Decimal> {
    let text = value.context("missing billing amount")?;
    Decimal::from_str(text)
        .or_else(|_| Decimal::from_scientific(text))
        .context("invalid billing amount")
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DailyCost {
    pub invoice_month: String,
    pub project: Option<String>,
    pub service: String,
    pub currency: String,
    pub gross: Decimal,
    pub credits: Decimal,
    pub net: Decimal,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::{
        matchers::{body_json, header, method, path, query_param, query_param_is_missing},
        Mock, MockServer, ResponseTemplate,
    };

    const READER: &str = concat!("reader@", "x", ".iam.gserviceaccount.com");

    #[tokio::test]
    async fn partition_pages_use_the_scoped_reader_and_aggregate_all_credits() {
        let server = MockServer::start().await;
        let export = ExportTable::parse("x.sample.billing", READER).unwrap();
        Mock::given(method("POST"))
            .and(path(format!(
                "/v1/projects/-/serviceAccounts/{READER}:generateAccessToken"
            )))
            .and(header("authorization", "Bearer source"))
            .and(body_json(json!({
                "scope":["https://www.googleapis.com/auth/bigquery.readonly"], "lifetime":"3600s"
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"accessToken":"reader-token"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let token = export
            .reader_token(
                &server.uri(),
                &crate::gcp_cost::StaticToken("source".into()),
            )
            .await
            .unwrap();
        let base = "/projects/x/datasets/sample/tables/billing";
        let schema = [
            "currency", "invoice", "labels", "project", "cost", "service", "credits",
        ]
        .map(|name| json!({"name":name}));
        Mock::given(method("GET"))
            .and(path(base))
            .and(header("authorization", "Bearer reader-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"schema":{"fields":schema},"timePartitioning":{"type":"DAY"}}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        let page = || {
            Mock::given(method("GET"))
                .and(path(format!("{base}$20261121/data")))
                .and(header("authorization", "Bearer reader-token"))
                .and(query_param(
                    "selectedFields",
                    "currency,invoice.month,project.id,cost,service.description,credits.amount",
                ))
        };
        let record = |label: &str| json!({"f":[{"v":label}]});
        let row = |currency: &str, project: Value, cost: &str, credits: &[&str]| {
            json!({"f":[
            {"v":currency},{"v":record("202611")},{"v":project},{"v":cost},{"v":record("Compute")},
            {"v":credits.iter().map(|v|json!({"v":record(v)})).collect::<Vec<_>>()}]})
        };
        page()
            .and(query_param_is_missing("pageToken"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"pageToken":"next","rows":[
                row("USD",record("sample"),"1.002",&["-0.1","-2e-3"])]})),
            )
            .expect(1)
            .mount(&server)
            .await;
        page()
            .and(query_param("pageToken", "next"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"rows":[
                row("USD",record("sample"),"0.6",&[]),row("CAD",Value::Null,"4",&[])]})))
            .expect(1)
            .mount(&server)
            .await;
        let rows = export
            .read(
                &server.uri(),
                NaiveDate::from_ymd_opt(2026, 11, 21).unwrap(),
                &token,
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (&rows[0].project, rows[0].currency.as_str()),
            (&None, "CAD")
        );
        assert_eq!(rows[1].gross, Decimal::new(1602, 3));
        assert_eq!(rows[1].credits, Decimal::new(-102, 3));
        assert_eq!(rows[1].net, Decimal::new(15, 1));
        assert_eq!(server.received_requests().await.unwrap().len(), 4);
    }
}
