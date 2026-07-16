use std::time::Duration;

use eyre::{Result, eyre};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{net::TcpListener, time::timeout};
use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};

use super::ModelRun;
use crate::{
    model::{ModelConfig, ReasoningEffort},
    protocol::{EventWriter, Task},
};

#[tokio::test]
async fn reconnects_before_resending_a_stored_continuation() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut first = accept_async(stream).await?;

        let warmup = next_json(&mut first).await?;
        assert_eq!(warmup["generate"], false);
        assert_eq!(warmup["previous_response_id"], Value::Null);
        send_json(
            &mut first,
            json!({
                "type": "response.completed",
                "response": { "id": "resp-warmup", "usage": null }
            }),
        )
        .await?;

        let first_request = next_json(&mut first).await?;
        assert_eq!(first_request["previous_response_id"], "resp-warmup");
        send_json(
            &mut first,
            completed_response(
                "resp-tool",
                &[json!({
                    "type": "function_call",
                    "call_id": "call-sleep",
                    "name": "exec_command",
                    "arguments": json!({ "cmd": "sleep 0.2" }).to_string(),
                    "caller": { "type": "program", "caller_id": "program-1" }
                })],
            ),
        )
        .await?;
        first.send(Message::Close(None)).await?;
        drop(first);

        let (stream, _) = listener.accept().await?;
        let mut second = accept_async(stream).await?;
        let continuation = next_json(&mut second).await?;
        assert_eq!(continuation["previous_response_id"], "resp-tool");
        assert_eq!(continuation["input"][0]["call_id"], "call-sleep");
        send_json(
            &mut second,
            completed_response(
                "resp-final",
                &[json!({
                    "type": "message",
                    "content": [{ "type": "output_text", "text": "done" }]
                })],
            ),
        )
        .await?;
        Result::<()>::Ok(())
    });

    let task = Task {
        instruction: "exercise reconnect handling".to_owned(),
        workspace: Some(env!("CARGO_MANIFEST_DIR").to_owned()),
    };
    let config = ModelConfig {
        model: "test-model".to_owned(),
        api_key: "test-key".to_owned(),
        effort: ReasoningEffort::Low,
        websocket_url: endpoint,
        max_model_calls: 3,
        compact_threshold: 350_000,
        multi_agent: false,
    };
    let mut output = Vec::new();
    {
        let mut events = EventWriter::new(&mut output, "reconnect-test".to_owned());
        ModelRun::new(&mut events, &task, &config).execute().await?;
    }
    timeout(Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;

    let events = String::from_utf8(output)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<serde_json::Result<Vec<_>>>()?;
    assert!(events.iter().any(|event| {
        event["type"] == "model.connection.retrying"
            && event["payload"]["previous_response_id"] == "resp-tool"
    }));
    let terminal = events
        .last()
        .ok_or_else(|| eyre!("missing terminal event"))?;
    assert_eq!(terminal["type"], "run.completed");
    assert_eq!(terminal["payload"]["connection_attempts"], 2);
    assert_eq!(terminal["payload"]["websocket_reconnects"], 1);
    Ok(())
}

async fn next_json<S>(socket: &mut WebSocketStream<S>) -> Result<Value>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let message = socket
        .next()
        .await
        .ok_or_else(|| eyre!("client closed before sending a request"))??;
    let Message::Text(text) = message else {
        return Err(eyre!("expected a text request, received {message:?}"));
    };
    Ok(serde_json::from_str(text.as_ref())?)
}

async fn send_json<S>(socket: &mut WebSocketStream<S>, event: Value) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    socket.send(Message::Text(event.to_string().into())).await?;
    Ok(())
}

fn completed_response(id: &str, output: &[Value]) -> Value {
    json!({
        "type": "response.completed",
        "response": {
            "id": id,
            "status": "completed",
            "output": output,
            "usage": {
                "input_tokens": 1,
                "input_tokens_details": {
                    "cached_tokens": 0,
                    "cache_write_tokens": 0
                },
                "output_tokens": 1,
                "output_tokens_details": { "reasoning_tokens": 0 },
                "total_tokens": 2
            }
        }
    })
}
