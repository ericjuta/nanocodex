use std::{
    net::TcpListener as StdTcpListener,
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
    MppError, PaymentChallenge, PaymentCredential, PrivateKeySigner,
    client::{PaymentProvider, TempoSessionProvider},
};
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
const DEFAULT_TEMPO_RPC_URL: &str = "https://rpc.moderato.tempo.xyz";
const DEFAULT_MAX_DEPOSIT: u128 = 10_000_000;

#[derive(Args, Clone)]
pub(crate) struct MppArgs {
    /// Pay for the Responses WebSocket through MPP.
    #[arg(
        long = "mpp",
        global = true,
        env = "NANOCODEX_MPP",
        default_value_t = false,
        action = ArgAction::SetTrue
    )]
    enabled: bool,

    /// Paid MPP WebSocket endpoint.
    #[arg(
        long = "mpp-responses-websocket-url",
        global = true,
        env = "MPP_RESPONSES_WEBSOCKET_URL",
        default_value = DEFAULT_MPP_WEBSOCKET_URL,
        value_parser = NonEmptyStringValueParser::new()
    )]
    mpp_websocket_url: String,

    /// Tempo account private key used to open and voucher the native session.
    #[arg(
        long = "tempo-private-key",
        global = true,
        env = "TEMPO_PRIVATE_KEY",
        hide_env_values = true,
        value_parser = NonEmptyStringValueParser::new()
    )]
    private_key: Option<String>,

    /// Tempo RPC used for native TIP-1034 channel operations.
    #[arg(
        long = "tempo-rpc-url",
        global = true,
        env = "TEMPO_RPC_URL",
        default_value = DEFAULT_TEMPO_RPC_URL,
        value_parser = NonEmptyStringValueParser::new()
    )]
    rpc_url: String,

    /// Maximum native session deposit in token atomic units.
    #[arg(
        long = "mpp-max-deposit",
        global = true,
        env = "MPP_MAX_DEPOSIT",
        default_value_t = DEFAULT_MAX_DEPOSIT
    )]
    max_deposit: u128,

    /// Optional access key for gated MPP deployments such as Moderato staging.
    #[arg(
        long = "mpp-api-key",
        global = true,
        env = "MPP_API_KEY",
        hide_env_values = true,
        value_parser = NonEmptyStringValueParser::new()
    )]
    mpp_api_key: Option<String>,
}

impl MppArgs {
    #[cfg(test)]
    pub(crate) const fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn start(
        self,
        direct_websocket_url: String,
    ) -> Result<(String, Option<MppAdapter>)> {
        if !self.enabled {
            return Ok((direct_websocket_url, None));
        }
        let private_key = self
            .private_key
            .ok_or_else(|| eyre!("--mpp requires TEMPO_PRIVATE_KEY or --tempo-private-key"))?;
        let signer = PrivateKeySigner::from_str(&private_key)
            .wrap_err("TEMPO_PRIVATE_KEY is not a valid private key")?;
        let session = TempoSessionProvider::new(signer, &self.rpc_url)
            .wrap_err("failed to configure the native Tempo session provider")?
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
            enabled,
            mpp_websocket_url: DEFAULT_MPP_WEBSOCKET_URL.to_owned(),
            private_key: None,
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
    fn mpp_requires_a_tempo_signer() {
        let error = args(true)
            .start("wss://api.openai.com/v1/responses".to_owned())
            .err()
            .unwrap();
        assert!(
            error
                .to_string()
                .contains("--mpp requires TEMPO_PRIVATE_KEY")
        );
    }
}
