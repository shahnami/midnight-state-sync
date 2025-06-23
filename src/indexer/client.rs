//! GraphQL client for Midnight blockchain indexer with session management

use super::types::*;
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde_json::json;
use std::time::Duration;
use tokio_tungstenite::{
	connect_async,
	tungstenite::{Message, client::IntoClientRequest},
};
use tracing::{debug, error, info};

/// Midnight GraphQL indexer client
#[derive(Clone)]
pub struct MidnightIndexerClient {
	http_client: Client,
	indexer_url: String,
	ws_url: String,
}

impl MidnightIndexerClient {
	/// Create a new indexer client
	pub fn new(indexer_url: String, ws_url: String) -> Self {
		let http_client = Client::builder()
			.timeout(Duration::from_secs(30))
			.build()
			.expect("Failed to create HTTP client");

		Self {
			http_client,
			indexer_url,
			ws_url,
		}
	}

	/// Establish wallet session with viewing key
	pub async fn connect_wallet(
		&self,
		viewing_key: &ViewingKeyFormat,
	) -> Result<String, IndexerError> {
		info!(
			"Connecting wallet with viewing key: {}",
			viewing_key.as_str()
		);

		let query = r#"
            mutation ConnectWallet($viewingKey: ViewingKey!) {
                connect(viewingKey: $viewingKey)
            }
        "#;

		let variables = json!({
			"viewingKey": viewing_key.as_str()
		});

		let response = self.execute_query(query, variables).await?;

		let session_id = response
			.get("data")
			.and_then(|data| data.get("connect"))
			.and_then(|connect| connect.as_str())
			.ok_or_else(|| IndexerError::NoData)?
			.to_string();

		info!("Connected wallet with session ID: {}", session_id);
		Ok(session_id)
	}

	/// Subscribe to wallet updates using session ID
	pub async fn subscribe_wallet(
		&self,
		session_id: &str,
		start_index: Option<u64>,
		send_progress_updates: Option<bool>,
	) -> Result<
		std::pin::Pin<
			Box<dyn futures_util::Stream<Item = Result<WalletSyncEvent, IndexerError>> + Send>,
		>,
		IndexerError,
	> {
		debug!("Attempting WebSocket connection to: {}", self.ws_url);

		// Create WebSocket request with required subprotocol
		let mut request = self.ws_url.clone().into_client_request()?;
		request.headers_mut().insert(
			"Sec-WebSocket-Protocol",
			"graphql-transport-ws".parse().map_err(|_| {
				IndexerError::GraphQLError("Invalid WebSocket subprotocol header value".to_string())
			})?,
		);

		let (ws_stream, response) = connect_async(request).await?;
		debug!(
			"WebSocket connection established, response status: {}",
			response.status()
		);
		let (mut ws_sender, mut ws_receiver) = ws_stream.split();

		// Send connection init
		let init_message = json!({
			"type": "connection_init"
		});
		ws_sender
			.send(Message::Text(init_message.to_string()))
			.await?;

		// Wait for connection ack
		if let Some(msg) = ws_receiver.next().await {
			match msg? {
				Message::Text(text) => {
					let parsed: serde_json::Value = serde_json::from_str(&text)?;
					if parsed.get("type")
						!= Some(&serde_json::Value::String("connection_ack".to_string()))
					{
						return Err(IndexerError::SessionError(
							"Connection not acknowledged".to_string(),
						));
					}
				}
				_ => {
					return Err(IndexerError::SessionError(
						"Unexpected message type during handshake".to_string(),
					));
				}
			}
		}

		// Start wallet subscription
		let subscription_query = format!(
			r#"
            subscription WalletSync {{
                wallet(sessionId: "{}", index: {}, sendProgressUpdates: {}) {{
                    __typename
                    ... on ViewingUpdate {{
                        index
                        update {{
                            __typename
                            ... on RelevantTransaction {{
                                transaction {{
                                    hash
                                    applyStage
                                    raw
                                    identifiers
                                    merkleTreeRoot
                                    protocolVersion
                                }}
                                start
                                end
                            }}
                            ... on MerkleTreeCollapsedUpdate {{
                                protocolVersion
                                start
                                end
                                update
                            }}
                        }}
                    }}
                    ... on ProgressUpdate {{
                        highestIndex
                        highestRelevantIndex
                        highestRelevantWalletIndex
                    }}
                }}
            }}
            "#,
			session_id,
			start_index.unwrap_or(0),
			send_progress_updates.unwrap_or(true)
		);

		let start_message = json!({
			"id": "wallet-sync",
			"type": "subscribe",
			"payload": {
				"query": subscription_query
			}
		});

		ws_sender
			.send(Message::Text(start_message.to_string()))
			.await?;

		// Return stream of wallet events
		let stream = ws_receiver.filter_map(|msg| async move {
			match msg {
				Ok(Message::Text(text)) => {
					match serde_json::from_str::<serde_json::Value>(&text) {
						Ok(parsed) => {
							// Handle different message types
							if let Some(msg_type) = parsed.get("type").and_then(|t| t.as_str()) {
								match msg_type {
									"next" => {
										if let Some(wallet_data) = parsed
											.get("payload")
											.and_then(|p| p.get("data"))
											.and_then(|d| d.get("wallet"))
										{
											debug!(
												"Raw wallet data: {}",
												serde_json::to_string_pretty(&wallet_data)
													.unwrap_or_else(|_| "Invalid JSON".to_string())
											);
											match serde_json::from_value::<WalletSyncEvent>(
												wallet_data.clone(),
											) {
												Ok(event) => Some(Ok(event)),
												Err(e) => {
													error!(
														"Failed to deserialize wallet event: {}",
														e
													);
													error!(
														"Raw data was: {}",
														serde_json::to_string_pretty(&wallet_data)
															.unwrap_or_else(
																|_| "Invalid JSON".to_string()
															)
													);
													Some(Err(IndexerError::JsonError(e)))
												}
											}
										} else {
											Some(Err(IndexerError::NoData))
										}
									}
									"error" => {
										let error_msg = parsed
											.get("payload")
											.and_then(|p| p.get("message"))
											.and_then(|m| m.as_str())
											.unwrap_or("Unknown subscription error");
										Some(Err(IndexerError::GraphQLError(error_msg.to_string())))
									}
									"complete" => {
										info!("Wallet subscription completed");
										None // End the stream
									}
									_ => {
										debug!("Ignoring message type: {}", msg_type);
										None // Skip other message types
									}
								}
							} else {
								Some(Err(IndexerError::GraphQLError(
									"Message missing type field".to_string(),
								)))
							}
						}
						Err(e) => Some(Err(IndexerError::JsonError(e))),
					}
				}
				Ok(_) => Some(Err(IndexerError::GraphQLError(
					"Unexpected message type".to_string(),
				))),
				Err(e) => Some(Err(IndexerError::WebSocketError(e))),
			}
		});

		Ok(Box::pin(stream))
	}

	/// Execute a GraphQL query
	async fn execute_query(
		&self,
		query: &str,
		variables: serde_json::Value,
	) -> Result<serde_json::Value, IndexerError> {
		let request_body = json!({
			"query": query,
			"variables": variables
		});

		let response = self
			.http_client
			.post(&self.indexer_url)
			.header("Content-Type", "application/json")
			.json(&request_body)
			.send()
			.await?;

		if !response.status().is_success() {
			return Err(IndexerError::GraphQLError(format!(
				"HTTP error: {}",
				response.status()
			)));
		}

		let response_json: serde_json::Value = response.json().await?;

		if let Some(errors) = response_json.get("errors") {
			return Err(IndexerError::GraphQLError(format!(
				"GraphQL errors: {}",
				errors
			)));
		}

		Ok(response_json)
	}
}
