//! Selected, finalized Solana evidence. RPC calls read one signature; no wallet
//! enumeration, signing, broadcasting, or private-key handling exists here.
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum EvidenceError {
    #[error("Solana evidence is unavailable, unfinalized or unsupported")]
    Unavailable,
    #[error("Solana evidence identity or precision is invalid")]
    Invalid,
}
#[derive(Clone, Copy)]
pub enum Network {
    MainnetBeta,
    Devnet,
    Testnet,
}
impl Network {
    pub fn parse(scope: &str) -> Result<Self, EvidenceError> {
        match scope {
            "solana:mainnet-beta" => Ok(Self::MainnetBeta),
            "solana:devnet" => Ok(Self::Devnet),
            "solana:testnet" => Ok(Self::Testnet),
            _ => Err(EvidenceError::Invalid),
        }
    }
    fn endpoint(self) -> &'static str {
        match self {
            Self::MainnetBeta => "https://api.mainnet-beta.solana.com",
            Self::Devnet => "https://api.devnet.solana.com",
            Self::Testnet => "https://api.testnet.solana.com",
        }
    }
}
#[derive(Debug)]
pub struct Movement {
    pub contract: String,
    pub decimals: u8,
    pub quantity: String,
    pub network_fee: String,
    pub fee_payer: String,
    pub block_time: Option<i64>,
    pub failed: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RpcResult {
    block_time: Option<i64>,
    meta: Option<Meta>,
    transaction: ParsedTransaction,
}
#[derive(Deserialize)]
struct ParsedTransaction {
    signatures: Vec<String>,
    message: Message,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Message {
    account_keys: Vec<AccountKey>,
    instructions: Vec<Instruction>,
}
#[derive(Deserialize)]
struct AccountKey {
    pubkey: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Meta {
    err: Option<Value>,
    fee: u64,
    #[serde(default)]
    inner_instructions: Option<Vec<Inner>>,
    #[serde(default)]
    pre_token_balances: Option<Vec<TokenBalance>>,
    #[serde(default)]
    post_token_balances: Option<Vec<TokenBalance>>,
}
#[derive(Deserialize)]
struct Inner {
    index: usize,
    instructions: Vec<Instruction>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Instruction {
    program_id: String,
    parsed: Option<Parsed>,
}
#[derive(Deserialize)]
struct Parsed {
    #[serde(rename = "type")]
    kind: String,
    info: Value,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenBalance {
    account_index: usize,
    mint: String,
    owner: Option<String>,
    ui_token_amount: TokenAmount,
}
#[derive(Deserialize)]
struct TokenAmount {
    decimals: u8,
}
fn identity(value: &str, bytes: usize) -> Result<(), EvidenceError> {
    if value.len() > 128
        || bs58::decode(value)
            .into_vec()
            .map_err(|_| EvidenceError::Invalid)?
            .len()
            != bytes
    {
        return Err(EvidenceError::Invalid);
    }
    Ok(())
}
fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str, EvidenceError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or(EvidenceError::Invalid)
}
fn instruction<'a>(
    tx: &'a RpcResult,
    meta: &'a Meta,
    locator: &str,
) -> Result<&'a Instruction, EvidenceError> {
    let parts: Vec<_> = locator.split(':').collect();
    match parts.as_slice() {
        ["instruction", index] => tx
            .transaction
            .message
            .instructions
            .get(index.parse::<usize>().map_err(|_| EvidenceError::Invalid)?),
        ["inner", parent, child] => {
            let parent = parent
                .parse::<usize>()
                .map_err(|_| EvidenceError::Invalid)?;
            let child = child.parse::<usize>().map_err(|_| EvidenceError::Invalid)?;
            meta.inner_instructions
                .as_deref()
                .unwrap_or_default()
                .iter()
                .find(|group| group.index == parent)
                .and_then(|group| group.instructions.get(child))
        }
        _ => None,
    }
    .ok_or(EvidenceError::Unavailable)
}
/// Normalize only the selected instruction, or the transaction's single fee.
/// Unknown programs and token extensions require reviewed evidence.
pub fn normalize(
    bytes: &[u8],
    signature: &str,
    wallet: &str,
    locator: Option<&str>,
) -> Result<Movement, EvidenceError> {
    identity(signature, 64)?;
    identity(wallet, 32)?;
    let envelope: Value = serde_json::from_slice(bytes).map_err(|_| EvidenceError::Invalid)?;
    if envelope.get("error").is_some() {
        return Err(EvidenceError::Unavailable);
    }
    let tx: RpcResult = serde_json::from_value(
        envelope
            .get("result")
            .filter(|v| !v.is_null())
            .cloned()
            .ok_or(EvidenceError::Unavailable)?,
    )
    .map_err(|_| EvidenceError::Invalid)?;
    if tx.transaction.signatures.first().map(String::as_str) != Some(signature) {
        return Err(EvidenceError::Invalid);
    }
    let meta = tx.meta.as_ref().ok_or(EvidenceError::Unavailable)?;
    let payer = tx
        .transaction
        .message
        .account_keys
        .first()
        .ok_or(EvidenceError::Invalid)?
        .pubkey
        .clone();
    let failed = meta.err.is_some();
    if locator.is_none() {
        if payer != wallet || meta.fee == 0 {
            return Err(EvidenceError::Invalid);
        }
        return Ok(Movement {
            contract: "native".into(),
            decimals: 9,
            quantity: (-i128::from(meta.fee)).to_string(),
            network_fee: meta.fee.to_string(),
            fee_payer: payer,
            block_time: tx.block_time,
            failed,
        });
    }
    let selected = instruction(&tx, meta, locator.ok_or(EvidenceError::Invalid)?)?;
    let TransferDetails {
        contract,
        decimals,
        amount,
        source_owner,
        destination_owner,
    } = transfer_details(&tx, meta, selected)?;
    let quantity = amount.parse::<i128>().map_err(|_| EvidenceError::Invalid)?;
    if quantity <= 0 || decimals > 38 || (source_owner == wallet) == (destination_owner == wallet) {
        return Err(EvidenceError::Invalid);
    }
    let quantity = if failed {
        0
    } else if source_owner == wallet {
        -quantity
    } else {
        quantity
    };
    Ok(Movement {
        contract,
        decimals,
        quantity: quantity.to_string(),
        network_fee: meta.fee.to_string(),
        fee_payer: payer,
        block_time: tx.block_time,
        failed,
    })
}
struct TransferDetails {
    contract: String,
    decimals: u8,
    amount: String,
    source_owner: String,
    destination_owner: String,
}
fn transfer_details(
    tx: &RpcResult,
    meta: &Meta,
    selected: &Instruction,
) -> Result<TransferDetails, EvidenceError> {
    let parsed = selected.parsed.as_ref().ok_or(EvidenceError::Unavailable)?;
    let source = text(&parsed.info, "source")?;
    let destination = text(&parsed.info, "destination")?;
    let details = if selected.program_id == "11111111111111111111111111111111"
        && matches!(parsed.kind.as_str(), "transfer" | "transferWithSeed")
    {
        TransferDetails {
            contract: "native".to_string(),
            decimals: 9,
            amount: parsed
                .info
                .get("lamports")
                .and_then(Value::as_u64)
                .ok_or(EvidenceError::Invalid)?
                .to_string(),
            source_owner: source.to_string(),
            destination_owner: destination.to_string(),
        }
    } else if selected.program_id == "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        && matches!(parsed.kind.as_str(), "transfer" | "transferChecked")
    {
        let balances: Vec<_> = meta
            .pre_token_balances
            .iter()
            .flatten()
            .chain(meta.post_token_balances.iter().flatten())
            .collect();
        let balance = |account: &str| {
            balances
                .iter()
                .find(|b| {
                    tx.transaction
                        .message
                        .account_keys
                        .get(b.account_index)
                        .is_some_and(|key| key.pubkey == account)
                })
                .copied()
                .ok_or(EvidenceError::Unavailable)
        };
        let src = balance(source)?;
        let dst = balance(destination)?;
        if src.mint != dst.mint || src.ui_token_amount.decimals != dst.ui_token_amount.decimals {
            return Err(EvidenceError::Invalid);
        }
        let amount = if parsed.kind == "transferChecked" {
            if text(&parsed.info, "mint")? != src.mint {
                return Err(EvidenceError::Invalid);
            }
            let token = parsed
                .info
                .get("tokenAmount")
                .ok_or(EvidenceError::Invalid)?;
            if token.get("decimals").and_then(Value::as_u64)
                != Some(u64::from(src.ui_token_amount.decimals))
            {
                return Err(EvidenceError::Invalid);
            }
            text(token, "amount")?.to_string()
        } else {
            text(&parsed.info, "amount")?.to_string()
        };
        TransferDetails {
            contract: src.mint.clone(),
            decimals: src.ui_token_amount.decimals,
            amount,
            source_owner: src.owner.clone().ok_or(EvidenceError::Unavailable)?,
            destination_owner: dst.owner.clone().ok_or(EvidenceError::Unavailable)?,
        }
    } else {
        return Err(EvidenceError::Unavailable);
    };
    Ok(details)
}
/// Read-only JSON-RPC `getTransaction` at finalized commitment, bounded to 4 MiB.
pub async fn fetch(
    network: Network,
    signature: &str,
    wallet: &str,
    locator: Option<&str>,
) -> Result<(Movement, Vec<u8>), EvidenceError> {
    fetch_at(network.endpoint(), signature, wallet, locator).await
}
async fn fetch_at(
    endpoint: &str,
    signature: &str,
    wallet: &str,
    locator: Option<&str>,
) -> Result<(Movement, Vec<u8>), EvidenceError> {
    identity(signature, 64)?;
    identity(wallet, 32)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| EvidenceError::Unavailable)?;
    let response = client.post(endpoint).json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"getTransaction","params":[signature,{"commitment":"finalized","encoding":"jsonParsed","maxSupportedTransactionVersion":0}]})).send().await.map_err(|_| EvidenceError::Unavailable)?;
    if !response.status().is_success() {
        return Err(EvidenceError::Unavailable);
    }
    let bytes = crate::mercury::bounded(response, 4 * 1024 * 1024)
        .await
        .map_err(|_| EvidenceError::Unavailable)?;
    Ok((normalize(&bytes, signature, wallet, locator)?, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(failed: bool) -> (Value, String, String, String) {
        let signature = bs58::encode([42u8; 64]).into_string();
        let sender = bs58::encode([7u8; 32]).into_string();
        let recipient = bs58::encode([8u8; 32]).into_string();
        let transfer = |amount| serde_json::json!({"programId":"11111111111111111111111111111111","parsed":{"type":"transfer","info":{"source":sender,"destination":recipient,"lamports":amount}}});
        (
            serde_json::json!({"result":{"blockTime":1_791_676_800_i64,"meta":{"err":if failed {serde_json::json!({"InstructionError":[0,"Custom"]})} else {Value::Null},"fee":5000,"innerInstructions":[{"index":1,"instructions":[transfer(7)]}]},"transaction":{"signatures":[signature],"message":{"accountKeys":[{"pubkey":sender},{"pubkey":recipient}],"instructions":[transfer(1_000_000_001_u64),transfer(2)]}}}}),
            signature,
            sender,
            recipient,
        )
    }
    #[tokio::test]
    async fn rpc_requests_only_the_selected_signature_at_finalized_commitment() {
        use wiremock::matchers::{body_json, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let (value, signature, sender, _) = fixture(false);
        Mock::given(method("POST")).and(body_json(serde_json::json!({"jsonrpc":"2.0","id":1,"method":"getTransaction","params":[signature,{"commitment":"finalized","encoding":"jsonParsed","maxSupportedTransactionVersion":0}]})))
            .respond_with(ResponseTemplate::new(200).set_body_json(value)).expect(1).mount(&server).await;
        let (movement, _) = fetch_at(&server.uri(), &signature, &sender, Some("instruction:0"))
            .await
            .unwrap();
        assert_eq!(movement.quantity, "-1000000001");
    }
    #[test]
    fn selected_instruction_inner_transfer_and_fee_preserve_exact_quantities() {
        let (value, signature, sender, recipient) = fixture(false);
        let bytes = serde_json::to_vec(&value).unwrap();
        assert_eq!(
            normalize(&bytes, &signature, &sender, Some("instruction:0"))
                .unwrap()
                .quantity,
            "-1000000001"
        );
        assert_eq!(
            normalize(&bytes, &signature, &recipient, Some("inner:1:0"))
                .unwrap()
                .quantity,
            "7"
        );
        let fee = normalize(&bytes, &signature, &sender, None).unwrap();
        assert_eq!(fee.quantity, "-5000");
        assert_eq!(fee.network_fee, "5000");
        assert!(normalize(&bytes, &signature, &recipient, None).is_err());
        assert!(normalize(&bytes, &signature, &sender, Some("instruction:9")).is_err());
    }
    #[test]
    fn failed_and_unfinalized_transactions_cannot_become_successful_transfers() {
        let (value, signature, sender, _) = fixture(true);
        let movement = normalize(
            &serde_json::to_vec(&value).unwrap(),
            &signature,
            &sender,
            Some("instruction:0"),
        )
        .unwrap();
        assert!(movement.failed);
        assert_eq!(movement.quantity, "0");
        assert_eq!(movement.network_fee, "5000");
        assert!(normalize(
            br#"{"result":null}"#,
            &signature,
            &sender,
            Some("instruction:0")
        )
        .is_err());
        let wrong = bs58::encode([41u8; 64]).into_string();
        assert!(normalize(
            &serde_json::to_vec(&value).unwrap(),
            &wrong,
            &sender,
            Some("instruction:0")
        )
        .is_err());
    }
    #[test]
    fn spl_transfer_uses_owner_mint_and_exact_integer_amount() {
        let (mut value, signature, sender, recipient) = fixture(false);
        let mint = bs58::encode([3u8; 32]).into_string();
        value["result"]["meta"]["postTokenBalances"] = serde_json::json!([
            {"accountIndex":0,"mint":mint,"owner":sender,"uiTokenAmount":{"decimals":18}},
            {"accountIndex":1,"mint":mint,"owner":recipient,"uiTokenAmount":{"decimals":18}}
        ]);
        value["result"]["transaction"]["message"]["instructions"][0] = serde_json::json!({"programId":"TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA","parsed":{"type":"transferChecked","info":{"source":sender,"destination":recipient,"mint":mint,"tokenAmount":{"amount":"1000000000000000001","decimals":18}}}});
        let movement = normalize(
            &serde_json::to_vec(&value).unwrap(),
            &signature,
            &recipient,
            Some("instruction:0"),
        )
        .unwrap();
        assert_eq!(movement.quantity, "1000000000000000001");
        assert_eq!(movement.decimals, 18);
        assert_eq!(movement.contract, mint);
        value["result"]["transaction"]["message"]["instructions"][0]["parsed"]["info"]
            ["tokenAmount"]["decimals"] = serde_json::json!(9);
        assert!(normalize(
            &serde_json::to_vec(&value).unwrap(),
            &signature,
            &recipient,
            Some("instruction:0")
        )
        .is_err());
    }
}
