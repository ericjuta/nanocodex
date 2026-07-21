use std::{
    fs,
    net::TcpListener as StdTcpListener,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
    time::Duration,
};

use alloy_transport_mpp::{
    CloseProvider, CloseRequest, MppApplicationWs, MppApplicationWsConnect, VoucherProvider,
    VoucherRequest,
};
use clap::{ArgAction, Args, builder::NonEmptyStringValueParser};
use eyre::{Context, Result, eyre};
use futures_util::{SinkExt, StreamExt};
use mpp::{
    Address, MppError, PaymentChallenge, PaymentCredential, Signer,
    client::{
        PaymentProvider, TempoSessionProvider,
        tempo::{
            session::store::{
                SqliteChannelStore, SqliteChannelStoreOptions, default_channel_database_path,
            },
            signing::{KeychainVersion, P256Jwk, TempoP256Signer, TempoSigningMode},
        },
    },
};
use serde::Deserialize;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{oneshot, watch},
    task::{JoinHandle, JoinSet},
    time::timeout,
};
use tokio_tungstenite::{
    WebSocketStream, accept_hdr_async,
    tungstenite::{
        Message,
        handshake::server::{Request, Response},
        http::{HeaderName, HeaderValue},
    },
};

const DEFAULT_MPP_WEBSOCKET_URL: &str = "wss://openai.mpp.tempo.xyz/v1/responses";
const DEFAULT_TEMPO_RPC_URL: &str = "https://rpc.mainnet.tempo.xyz";
const DEFAULT_MAX_DEPOSIT: u128 = 1_000_000;

#[derive(Args, Clone)]
pub(crate) struct MppArgs {
    /// Connect directly to `OpenAI`. This is the default provider.
    #[arg(
        long = "provider.openai",
        global = true,
        env = "NANOCODEX_PROVIDER_OPENAI",
        default_value_t = false,
        action = ArgAction::SetTrue,
        conflicts_with = "tempo"
    )]
    openai: bool,

    /// Pay for the Responses WebSocket through MPP.
    #[arg(
        long = "provider.tempo",
        id = "tempo",
        global = true,
        env = "NANOCODEX_PROVIDER_TEMPO",
        default_value_t = false,
        action = ArgAction::SetTrue
    )]
    enabled: bool,

    /// Paid MPP WebSocket endpoint.
    #[arg(
        long = "provider.tempo.responses-websocket-url",
        global = true,
        env = "NANOCODEX_PROVIDER_TEMPO_RESPONSES_WEBSOCKET_URL",
        default_value = DEFAULT_MPP_WEBSOCKET_URL,
        value_parser = NonEmptyStringValueParser::new()
    )]
    mpp_websocket_url: String,

    /// Tempo Wallet state containing the logged-in account and access key.
    #[arg(
        long = "provider.tempo.wallet-store",
        global = true,
        env = "NANOCODEX_PROVIDER_TEMPO_WALLET_STORE"
    )]
    wallet_store: Option<PathBuf>,

    /// `SQLite` channel store shared with Tempo Wallet and `MPPx` CLIs.
    #[arg(
        long = "provider.tempo.channel-store",
        global = true,
        env = "NANOCODEX_PROVIDER_TEMPO_CHANNEL_STORE"
    )]
    channel_store: Option<PathBuf>,

    /// Tempo RPC used for native TIP-1034 channel operations.
    #[arg(
        long = "provider.tempo.rpc-url",
        global = true,
        env = "NANOCODEX_PROVIDER_TEMPO_RPC_URL",
        default_value = DEFAULT_TEMPO_RPC_URL,
        value_parser = NonEmptyStringValueParser::new()
    )]
    rpc_url: String,

    /// Maximum native session deposit in token atomic units.
    #[arg(
        long = "provider.tempo.max-deposit",
        global = true,
        env = "NANOCODEX_PROVIDER_TEMPO_MAX_DEPOSIT",
        default_value_t = DEFAULT_MAX_DEPOSIT
    )]
    max_deposit: u128,

    /// Optional access key for gated MPP deployments such as Moderato staging.
    #[arg(
        long = "provider.tempo.api-key",
        global = true,
        env = "NANOCODEX_PROVIDER_TEMPO_API_KEY",
        hide_env_values = true,
        value_parser = NonEmptyStringValueParser::new()
    )]
    mpp_api_key: Option<String>,
}

impl MppArgs {
    pub(crate) const fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn start(
        self,
        direct_websocket_url: String,
    ) -> Result<(String, Option<MppAdapter>)> {
        if self.openai || !self.enabled {
            return Ok((direct_websocket_url, None));
        }
        let wallet_path = self.wallet_store.unwrap_or(
            default_channel_database_path()
                .map_err(|error| eyre!(error))?
                .with_file_name("store.json"),
        );
        let wallet = TempoWallet::load(&wallet_path)?;
        let endpoint = payment_http_url(&self.mpp_websocket_url)?;
        let store = SqliteChannelStore::open(SqliteChannelStoreOptions {
            namespace: endpoint.origin().ascii_serialization(),
            path: self.channel_store,
            request_url: Some(endpoint.to_string()),
        })
        .map_err(|error| eyre!(error))
        .wrap_err("failed to open the Tempo session channel store")?;
        let session = TempoSessionProvider::new(wallet.signer, &self.rpc_url)
            .wrap_err("failed to configure the native Tempo session provider")?
            .with_signing_mode(TempoSigningMode::Keychain {
                wallet: wallet.account,
                key_authorization: None,
                version: KeychainVersion::V2,
            })
            .with_authorized_signer(wallet.access_key)
            .with_channel_store(Arc::new(store))
            .with_default_deposit(self.max_deposit)
            .with_max_deposit(self.max_deposit);
        let payment = NativeSession(session);

        let listener = StdTcpListener::bind("127.0.0.1:0")
            .wrap_err("failed to bind the local MPP WebSocket adapter")?;
        listener
            .set_nonblocking(true)
            .wrap_err("failed to configure the local MPP WebSocket adapter")?;
        let address = listener
            .local_addr()
            .wrap_err("failed to read the local MPP WebSocket adapter address")?;
        let listener = TcpListener::from_std(listener)
            .wrap_err("failed to start the local MPP WebSocket adapter")?;
        let config = Arc::new(BridgeConfig {
            endpoint: self.mpp_websocket_url,
            bootstrap_url: endpoint.to_string(),
            api_key: self.mpp_api_key,
            payment,
        });
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(serve(listener, config, shutdown_rx));
        Ok((
            format!("ws://{address}/v1/responses"),
            Some(MppAdapter {
                shutdown_tx: Some(shutdown_tx),
                task: Some(task),
            }),
        ))
    }
}

#[derive(Deserialize)]
struct TempoWalletFile {
    #[serde(rename = "tempo-cli.store")]
    store: TempoWalletEnvelope,
}

#[derive(Deserialize)]
struct TempoWalletEnvelope {
    state: TempoWalletState,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TempoWalletState {
    active_account: serde_json::Value,
    chain_id: u64,
    accounts: Vec<TempoWalletAccount>,
    access_keys: Vec<TempoAccessKey>,
}

#[derive(Deserialize)]
struct TempoWalletAccount {
    address: Address,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TempoAccessKey {
    address: Address,
    access: Address,
    chain_id: u64,
    key_type: String,
    handle: TempoAccessKeyHandle,
}

#[derive(Deserialize)]
struct TempoAccessKeyHandle {
    kind: String,
    jwk: P256Jwk,
}

struct TempoWallet {
    account: Address,
    access_key: Address,
    signer: TempoP256Signer,
}

impl TempoWallet {
    fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path)
            .wrap_err_with(|| format!("failed to read Tempo Wallet state at {}", path.display()))?;
        let file: TempoWalletFile = serde_json::from_slice(&bytes)
            .wrap_err_with(|| format!("invalid Tempo Wallet state at {}", path.display()))?;
        let state = file.store.state;
        let account = active_account(&state)?;
        let access_key = state
            .access_keys
            .into_iter()
            .find(|key| key.chain_id == state.chain_id && key.access == account)
            .ok_or_else(|| {
                eyre!(
                    "Tempo Wallet has no access key for active chain {}",
                    state.chain_id
                )
            })?;
        if access_key.key_type != "p256" || access_key.handle.kind != "webcrypto-p256" {
            return Err(eyre!(
                "Tempo Wallet access key must be an extractable P-256 JWK"
            ));
        }
        let signer = TempoP256Signer::from_webcrypto_jwk(&access_key.handle.jwk)
            .wrap_err("failed to load the Tempo Wallet access key")?;
        if Signer::address(&signer) != access_key.address {
            return Err(eyre!(
                "Tempo Wallet access-key address does not match its persisted JWK"
            ));
        }
        Ok(Self {
            account,
            access_key: access_key.address,
            signer,
        })
    }
}

fn active_account(state: &TempoWalletState) -> Result<Address> {
    if let Some(index) = state.active_account.as_u64() {
        return usize::try_from(index)
            .ok()
            .and_then(|index| state.accounts.get(index))
            .map(|account| account.address)
            .ok_or_else(|| eyre!("Tempo Wallet active account index is out of range"));
    }
    if let Some(address) = state.active_account.as_str() {
        let address = Address::from_str(address).wrap_err("invalid Tempo Wallet active account")?;
        if state
            .accounts
            .iter()
            .any(|account| account.address == address)
        {
            return Ok(address);
        }
    }
    Err(eyre!("Tempo Wallet active account is missing or invalid"))
}

fn payment_http_url(websocket_url: &str) -> Result<reqwest13::Url> {
    let mut url = reqwest13::Url::parse(websocket_url)
        .wrap_err("Tempo Responses WebSocket URL is invalid")?;
    let scheme = match url.scheme() {
        "ws" => "http",
        "wss" => "https",
        scheme => return Err(eyre!("unsupported Tempo WebSocket URL scheme {scheme}")),
    };
    url.set_scheme(scheme)
        .map_err(|()| eyre!("failed to derive the Tempo payment bootstrap URL"))?;
    Ok(url)
}

pub(crate) struct MppAdapter {
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<()>>>,
}

impl MppAdapter {
    pub(crate) async fn shutdown(mut self) -> Result<()> {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        let mut task = self
            .task
            .take()
            .ok_or_else(|| eyre!("MPP WebSocket adapter task is missing"))?;
        match timeout(Duration::from_secs(30), &mut task).await {
            Ok(completed) => completed
                .wrap_err("MPP WebSocket adapter task failed")?
                .wrap_err("MPP WebSocket adapter failed"),
            Err(error) => {
                task.abort();
                Err(error).wrap_err("timed out closing the paid MPP session")
            }
        }
    }
}

impl Drop for MppAdapter {
    fn drop(&mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[derive(Clone)]
struct NativeSession(TempoSessionProvider);

impl PaymentProvider for NativeSession {
    fn supports(&self, method: &str, intent: &str) -> bool {
        self.0.supports(method, intent)
    }

    async fn pay(&self, challenge: &PaymentChallenge) -> Result<PaymentCredential, MppError> {
        self.0.pay(challenge).await
    }

    fn accept_payment_header(&self) -> Option<String> {
        self.0.accept_payment_header()
    }
}

impl VoucherProvider for NativeSession {
    async fn next_voucher(&self, request: &VoucherRequest) -> Result<PaymentCredential, MppError> {
        let cumulative = request.required_cumulative.parse().map_err(|error| {
            MppError::InvalidConfig(format!(
                "invalid required cumulative voucher amount: {error}"
            ))
        })?;
        self.0
            .voucher_credential(&request.channel_id, cumulative)
            .await
    }
}

impl CloseProvider for NativeSession {
    async fn close_credential(
        &self,
        request: &CloseRequest,
    ) -> Result<PaymentCredential, MppError> {
        let cumulative = request.cumulative_amount.parse().map_err(|error| {
            MppError::InvalidConfig(format!("invalid close-ready cumulative amount: {error}"))
        })?;
        self.0
            .close_credential_at(&request.channel_id, cumulative)
            .await
    }
}

struct BridgeConfig {
    endpoint: String,
    bootstrap_url: String,
    api_key: Option<String>,
    payment: NativeSession,
}

async fn serve(
    listener: TcpListener,
    config: Arc<BridgeConfig>,
    mut shutdown: oneshot::Receiver<()>,
) -> Result<()> {
    let mut bridges = JoinSet::new();
    let mut bridge_error = None;
    let (bridge_shutdown_tx, _) = watch::channel(false);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else {
                    break;
                };
                let config = Arc::clone(&config);
                let bridge_shutdown = bridge_shutdown_tx.subscribe();
                bridges.spawn(async move { bridge(stream, &config, bridge_shutdown).await });
            }
            completed = bridges.join_next(), if !bridges.is_empty() => {
                record_bridge_result(completed, &mut bridge_error);
            }
        }
    }
    let _ = bridge_shutdown_tx.send(true);
    while let Some(completed) = bridges.join_next().await {
        record_bridge_result(Some(completed), &mut bridge_error);
    }
    if let Some(error) = bridge_error {
        Err(error)
    } else {
        Ok(())
    }
}

fn record_bridge_result(
    completed: Option<std::result::Result<Result<()>, tokio::task::JoinError>>,
    first_error: &mut Option<eyre::Report>,
) {
    match completed {
        Some(Ok(Err(error))) => {
            tracing::warn!(error = ?error, "MPP WebSocket adapter closed");
            if first_error.is_none() {
                *first_error = Some(error);
            }
        }
        Some(Err(error)) => {
            tracing::warn!(%error, "MPP WebSocket adapter task failed");
            if first_error.is_none() {
                *first_error = Some(error.into());
            }
        }
        Some(Ok(Ok(()))) | None => {}
    }
}

#[expect(
    clippy::result_large_err,
    reason = "tungstenite fixes the handshake callback's rejection response type"
)]
async fn bridge(
    stream: TcpStream,
    config: &BridgeConfig,
    shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let downstream_headers = Arc::new(Mutex::new(None));
    let captured = Arc::clone(&downstream_headers);
    let downstream = accept_hdr_async(stream, move |request: &Request, response: Response| {
        if let Ok(mut headers) = captured.lock() {
            *headers = Some(request.headers().clone());
        }
        Ok(response)
    })
    .await
    .wrap_err("local Responses WebSocket handshake failed")?;
    let headers = downstream_headers
        .lock()
        .map_err(|_| eyre!("local Responses WebSocket header capture was poisoned"))?
        .take()
        .ok_or_else(|| eyre!("local Responses WebSocket headers were not captured"))?;

    let mut bootstrap_headers = reqwest13::header::HeaderMap::new();
    if let Some(api_key) = &config.api_key {
        bootstrap_headers.insert(
            reqwest13::header::HeaderName::from_static("x-api-key"),
            reqwest13::header::HeaderValue::from_str(api_key)?,
        );
    }
    config
        .payment
        .0
        .bootstrap_with_headers(
            &reqwest13::Client::new(),
            &config.bootstrap_url,
            bootstrap_headers,
        )
        .await
        .wrap_err("failed to rehydrate the Tempo MPP session")?;

    let mut connector = MppApplicationWsConnect::new(
        &config.endpoint,
        config.payment.clone(),
        config.payment.clone(),
    );
    for name in [
        "openai-beta",
        "x-openai-internal-codex-responses-lite",
        "session-id",
        "thread-id",
        "x-client-request-id",
        "x-responsesapi-include-timing-metrics",
        "user-agent",
    ] {
        if let Some(value) = headers.get(name) {
            connector = connector.with_header(
                HeaderName::from_static(name),
                HeaderValue::from_bytes(value.as_bytes())?,
            );
        }
    }
    if let Some(api_key) = &config.api_key {
        connector = connector.with_header(
            HeaderName::from_static("x-api-key"),
            HeaderValue::from_str(api_key)?,
        );
    }
    let upstream = connector
        .connect()
        .await
        .wrap_err("failed to open the paid MPP WebSocket")?;
    relay(downstream, upstream, shutdown).await
}

async fn relay(
    mut downstream: WebSocketStream<TcpStream>,
    mut upstream: MppApplicationWs<NativeSession>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let relay_result = loop {
        tokio::select! {
            _ = shutdown.changed() => break Ok(()),
            inbound = downstream.next() => match inbound {
                Some(Ok(Message::Text(text))) => {
                    if let Err(error) = upstream.send(text.to_string()).await {
                        return Err(error.into());
                    }
                }
                Some(Ok(Message::Ping(payload))) => {
                    if let Err(error) = downstream.send(Message::Pong(payload)).await {
                        if *shutdown.borrow() {
                            break Ok(());
                        }
                        break Err(error.into());
                    }
                }
                Some(Ok(Message::Close(_))) | None => break Ok(()),
                Some(Ok(Message::Pong(_) | Message::Frame(_))) => {}
                Some(Ok(Message::Binary(_))) => {
                    break Err(eyre!("Responses WebSocket sent a binary frame"));
                }
                Some(Err(error)) => {
                    if *shutdown.borrow() {
                        break Ok(());
                    }
                    break Err(error.into());
                }
            },
            outbound = upstream.next() => {
                let text = outbound.wrap_err("paid MPP WebSocket receive failed")?;
                if let Err(error) = downstream.send(Message::Text(text.into())).await {
                    if *shutdown.borrow() {
                        break Ok(());
                    }
                    break Err(error.into());
                }
            }
        }
    };
    upstream
        .close()
        .await
        .wrap_err("failed to close the paid MPP session")?;
    relay_result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(enabled: bool) -> MppArgs {
        MppArgs {
            openai: false,
            enabled,
            mpp_websocket_url: DEFAULT_MPP_WEBSOCKET_URL.to_owned(),
            wallet_store: None,
            channel_store: None,
            rpc_url: DEFAULT_TEMPO_RPC_URL.to_owned(),
            max_deposit: DEFAULT_MAX_DEPOSIT,
            mpp_api_key: None,
        }
    }

    #[test]
    fn mpp_is_opt_in() {
        let (url, adapter) = args(false)
            .start("wss://api.openai.com/v1/responses".to_owned())
            .unwrap();
        assert_eq!(url, "wss://api.openai.com/v1/responses");
        assert!(adapter.is_none());
    }

    #[test]
    fn derives_payment_bootstrap_url() {
        let url = payment_http_url("wss://openai.mpp.tempo.xyz/v1/responses").unwrap();
        assert_eq!(url.as_str(), "https://openai.mpp.tempo.xyz/v1/responses");
    }

    #[test]
    fn loads_accounts_sdk_access_key() {
        let path = std::env::temp_dir().join(format!(
            "nanocodex-tempo-wallet-{}.json",
            std::process::id()
        ));
        let json = r#"{
          "tempo-cli.store": {
            "version": 0,
            "state": {
              "activeAccount": 0,
              "chainId": 4217,
              "accounts": [{"address":"0x1111111111111111111111111111111111111111"}],
              "accessKeys": [{
                "address":"0xf0159a522607cd6ab1097204c9fafb7bbe6afb6c",
                "access":"0x1111111111111111111111111111111111111111",
                "chainId":4217,
                "keyType":"p256",
                "handle":{"kind":"webcrypto-p256","jwk":{
                  "kty":"EC","crv":"P-256",
                  "x":"OtOGGpViE5JRa7WT7wVYPtLlhm9ctiYKMBcjf9ibkK8",
                  "y":"0JYcfjcHWmeRo5xh9WKVsCttJlZ7YV5gqkHuHI6DOI0",
                  "d":"QkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkI"
                }}
              }]
            }
          }
        }"#;
        fs::write(&path, json).unwrap();
        let wallet = TempoWallet::load(&path).unwrap();
        fs::remove_file(path).unwrap();

        assert_eq!(
            wallet.account,
            "0x1111111111111111111111111111111111111111"
                .parse::<Address>()
                .unwrap()
        );
        assert_eq!(
            wallet.access_key,
            "0xf0159a522607cd6ab1097204c9fafb7bbe6afb6c"
                .parse::<Address>()
                .unwrap()
        );
    }
}
