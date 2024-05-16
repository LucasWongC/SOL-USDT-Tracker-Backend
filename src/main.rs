use hyper::header::{HeaderValue, CONTENT_TYPE, ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS};
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server};
use serde::Serialize;
use solana_client::client_error::{ClientError, ClientErrorKind};
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_config::RpcBlockConfig;
use solana_client::rpc_request::RpcRequest;
use solana_sdk::clock::UnixTimestamp;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_transaction_status::option_serializer::OptionSerializer;
use solana_transaction_status::{
    EncodedTransaction, EncodedTransactionWithStatusMeta, TransactionDetails, UiConfirmedBlock, UiTransactionEncoding
};
use std::sync::{Arc, Mutex};
use std::convert::Infallible;
use std::net::SocketAddr;
use tokio::task;
use tokio::time::Duration;

// Define a struct to represent a USDC transaction
#[derive(Clone, Serialize)]
struct UsdcTransaction {
    hash: String,
    transaction: EncodedTransactionWithStatusMeta,
    timestamp: Option<UnixTimestamp>,
    slot: u64,
}

// Shared state to store USDC transactions in memory
type AppState = Arc<Mutex<Vec<UsdcTransaction>>>;

// Constant for the USDC token mint address
const USDC_MINT_ADDRESS: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

// Function to monitor Solana blockchain for USDC transfers
async fn monitor_usdc_transfers(api_url: &str, state: AppState) {
    let rpc_client =
        RpcClient::new_with_commitment(api_url.to_string(), CommitmentConfig::confirmed());
    let mut last_checked_slot = rpc_client.get_slot().unwrap();
    log::info!(
        "Starting monitoring USDC transfers from slot {}",
        last_checked_slot
    );
    loop {
        let latest_slot = rpc_client.get_slot().unwrap();
        log::info!(
            "Checking slots from {} to {}",
            last_checked_slot,
            latest_slot
        );
        for slot in last_checked_slot..=latest_slot {
            match fetch_block_with_retry(&rpc_client, slot).await {
                Ok(block) => {
                    log::info!(
                        "Processing block at Slot {}, Hash: {}",
                        slot,
                        block.blockhash
                    );
                    if let Some(transactions) = block.transactions {
                        log::info!("Transactions: {}", transactions.len());
                        for transaction in transactions {
                            // Check if the transaction is a USDC transfer
                            if is_usdc_transfer(&transaction) {
                                // Parse transaction details
                                if let Some(hash) =
                                    extract_transaction_hash(&transaction)
                                {
                                    log::info!(
                                        "Detected USDC transfer - Hash: {}",
                                        hash
                                    );

                                    // let versioned_transaction = transaction.transaction.decode();
                                    // Store the USDC transaction in memory
                                    let usdc_transaction = UsdcTransaction {
                                        hash,
                                        transaction,
                                        timestamp: block.block_time,
                                        slot
                                    };
                                    state.lock().unwrap().push(usdc_transaction);
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    log::error!("Cannot get block at slot {} after retries: {}", slot, err);
                }
            }
        }
        last_checked_slot = latest_slot;
        // Sleep for some time before checking for new blocks again
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}

async fn fetch_block_with_retry(
    rpc_client: &RpcClient,
    slot: u64,
) -> Result<UiConfirmedBlock, ClientError> {
    let mut attempts = 0;
    let max_attempts = 5;
    let mut delay = Duration::from_secs(1);

    while attempts < max_attempts {
        match rpc_client.get_block_with_config(
            slot,
            RpcBlockConfig {
                encoding: Some(UiTransactionEncoding::JsonParsed),
                max_supported_transaction_version: Some(0),
                transaction_details: Some(TransactionDetails::Full),
                rewards: Some(true),
                commitment: Some(CommitmentConfig::confirmed()),
            },
        ) {
            Ok(block) => {
                log::info!("Successfully fetched block at slot {}", slot);
                return Ok(block);
            }
            Err(err) => {
                attempts += 1;
                log::warn!(
                    "Attempt {} to get block at slot {} failed: {:?}",
                    attempts,
                    slot,
                    err
                );
                if attempts < max_attempts {
                    tokio::time::sleep(delay).await;
                    delay *= 2; // Exponential backoff
                }
            }
        }
    }

    Err(ClientError::new_with_request(
        ClientErrorKind::Custom(format!(
            "Failed to get block at slot {} after {} attempts",
            slot, max_attempts
        )),
        RpcRequest::GetBlock,
    ))
}
// Function to check if a transaction is a USDC transfer
fn is_usdc_transfer(transaction: &EncodedTransactionWithStatusMeta) -> bool {
    // Check if the transaction meta exists
    // log::info!("Checking transaction meta");
    if let Some(meta) = &transaction.meta {
        // Check pre and post token balances
        // log::info!("Checked transaction meta");
        if let OptionSerializer::Some(pre_balances) = meta.pre_token_balances.as_ref() {
            if let OptionSerializer::Some(post_balances) = meta.post_token_balances.as_ref() {
                // log::info!("Pre Balances: {} & Post Balances: {}", pre_balances.len(), post_balances.len());
                // Iterate over the balances to find any USDC transfer
                for (pre, post) in pre_balances.iter().zip(post_balances) {
                    // log::info!("Pre Mint: {}, Post Mint: {}", pre.mint, post.mint);
                    if pre.mint == USDC_MINT_ADDRESS && post.mint == USDC_MINT_ADDRESS {
                        if pre.ui_token_amount.amount != post.ui_token_amount.amount {
                            log::info!(
                                "USDC transfer detected in transaction: pre_balance = {}, post_balance = {}",
                                pre.ui_token_amount.amount,
                                post.ui_token_amount.amount
                            );
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}


fn extract_transaction_hash(
    transaction: &EncodedTransactionWithStatusMeta,
) -> Option<String> {
    log::debug!("Extracting Transaction Hash");
    match &transaction.transaction {
        EncodedTransaction::Json(ui_transaction) => {
            let hash = ui_transaction.signatures.get(0)?.to_string();
            log::debug!(
                "Extracted transaction details - Hash: {}",
                hash,
            );
            Some(hash)
        }
        _ => None,
    }
}


// API handler to get all USDC transactions
async fn get_usdc_transactions(state: AppState) -> Result<Response<Body>, Infallible> {
    let transactions = state.lock().unwrap();
    let response_body = serde_json::to_string(&*transactions).unwrap();
    log::info!(
        "Returning all USDC transactions, count: {}",
        transactions.len()
    );
    Ok(Response::builder()
        .status(200)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(response_body))
        .unwrap())
}

// API handler to get a specific USDC transaction by hash
async fn get_usdc_transaction_by_hash(
    state: AppState,
    hash: String,
) -> Result<Response<Body>, Infallible> {
    let transactions = state.lock().unwrap();
    if let Some(transaction) = transactions.iter().find(|tx| tx.hash == hash) {
        let response_body = serde_json::to_string(transaction).unwrap();
        log::info!("Returning USDC transaction for hash: {}", hash);
        Ok(Response::builder()
            .status(200)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(response_body))
            .unwrap())
    } else {
        log::warn!("Transaction not found for hash: {}", hash);
        Ok(Response::builder()
            .status(404)
            .body(Body::from("Transaction not found"))
            .unwrap())
    }
}

async fn router(req: Request<Body>, state: Arc<Mutex<Vec<UsdcTransaction>>>) -> Result<Response<Body>, Infallible> {
    match (req.method(), req.uri().path()) {
        (&Method::OPTIONS, _) => {
            // Handle CORS preflight request
            Ok(Response::builder()
                .header(ACCESS_CONTROL_ALLOW_ORIGIN, "*")
                .header(ACCESS_CONTROL_ALLOW_METHODS, "GET, POST, OPTIONS")
                .header(ACCESS_CONTROL_ALLOW_HEADERS, "Content-Type")
                .body(Body::empty())
                .unwrap())
        }
        (&Method::GET, "/usdc-transactions") => add_cors_headers(get_usdc_transactions(state).await),
        (&Method::GET, path) if path.starts_with("/usdc-transactions/") => {
            let hash = path.trim_start_matches("/usdc-transactions/").to_string();
            add_cors_headers(get_usdc_transaction_by_hash(state, hash).await)
        }
        _ => Ok(Response::builder()
            .status(404)
            .header(ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .body(Body::from("Not Found"))
            .unwrap()),
    }
}

// Helper function to add CORS headers to a response
fn add_cors_headers(response: Result<Response<Body>, Infallible>) -> Result<Response<Body>, Infallible> {
    response.map(|mut res| {
        res.headers_mut().insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
        res
    })
}


#[tokio::main]
async fn main() {
    // Initialize logging (optional: configure it as needed)
    env_logger::init();

    let state = Arc::new(Mutex::new(Vec::<UsdcTransaction>::new()));
    let state_clone = Arc::clone(&state);

    // Set the API URL
    let api_url = "https://api.mainnet-beta.solana.com";
    task::spawn(async move {
        monitor_usdc_transfers(api_url, state_clone).await;
    });

    let make_svc = make_service_fn(move |_| {
        let state = Arc::clone(&state);
        async {
            Ok::<_, Infallible>(service_fn(move |req| {
                let state = Arc::clone(&state);
                router(req, state)
            }))
        }
    });

    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    let server = Server::bind(&addr).serve(make_svc);

    log::info!("Listening on http://{}", addr);

    server.await.expect("Terminated");
}
