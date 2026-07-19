use std::time::Duration;

/// Errors produced by the host-backed Responses WebSocket transport.
#[derive(Debug, thiserror::Error)]
pub enum ResponsesError {
    #[error("failed to connect to the Responses WebSocket: {detail}")]
    Connect { detail: String },
    #[error("failed to send a Responses WebSocket frame: {detail}")]
    Send { detail: String, reconnectable: bool },
    #[error("failed to receive a Responses WebSocket frame: {0}")]
    Receive(String),
    #[error("Responses WebSocket produced no event for {seconds} seconds")]
    IdleTimeout { seconds: u64 },
    #[error("Responses WebSocket closed without a close frame")]
    UnexpectedEnd,
    #[error("Responses WebSocket event was not valid JSON")]
    InvalidJson(#[source] serde_json::Error),
    #[error("Responses WebSocket returned a binary data frame; expected JSON text")]
    UnexpectedBinary,
    #[error("failed to encode a Responses WebSocket request")]
    EncodeRequest(#[source] serde_json::Error),
    #[error("Responses API event did not match its declared type: {event}")]
    InvalidPayload {
        #[source]
        source: serde_json::Error,
        event: String,
    },
    #[error("Responses WebSocket closed {detail}")]
    Closed { detail: String },
    #[error("Responses API returned an error event: {event}")]
    Api { event: String },
    #[error("Responses API rejected invalid image data: {event}")]
    InvalidImageRequest { event: String },
}

impl ResponsesError {
    #[must_use]
    pub fn retry_advice(&self) -> Option<RetryAdvice> {
        let class = match self {
            Self::Connect { .. } => "handshake_transport",
            Self::Send {
                reconnectable: true,
                ..
            } => "send_transport",
            Self::Receive(_) => "receive_transport",
            Self::IdleTimeout { .. } => "event_idle_timeout",
            Self::UnexpectedEnd | Self::Closed { .. } => "premature_close",
            _ => return None,
        };
        Some(RetryAdvice {
            class,
            server_delay: None,
        })
    }

    #[must_use]
    pub const fn class(&self) -> &'static str {
        match self {
            Self::Connect { .. } => "handshake",
            Self::Send { .. } => "send",
            Self::Receive(_) => "receive",
            Self::IdleTimeout { .. } => "event_idle_timeout",
            Self::UnexpectedEnd => "premature_close",
            Self::InvalidJson(_) => "invalid_json",
            Self::UnexpectedBinary => "unexpected_binary",
            Self::EncodeRequest(_) => "encode_request",
            Self::InvalidPayload { .. } => "invalid_payload",
            Self::Closed { .. } => "closed",
            Self::Api { .. } => "api",
            Self::InvalidImageRequest { .. } => "invalid_image_request",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RetryAdvice {
    pub class: &'static str,
    pub server_delay: Option<Duration>,
}
