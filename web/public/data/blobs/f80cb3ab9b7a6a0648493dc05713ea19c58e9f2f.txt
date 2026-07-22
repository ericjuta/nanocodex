use std::{collections::HashMap, sync::Arc};

use http::{HeaderName, HeaderValue};
use rmcp::{
    ServiceExt,
    model::Tool,
    service::{RoleClient, RunningService},
    transport::{
        StreamableHttpClientTransport, TokioChildProcess,
        streamable_http_client::StreamableHttpClientTransportConfig,
    },
};

use crate::config::{McpServer, McpTransport};

pub(crate) type Client = Arc<RunningService<RoleClient, ()>>;

pub(crate) struct ConnectedServer {
    pub client: Client,
    pub tools: Vec<Tool>,
}

pub(crate) async fn connect(server: &McpServer) -> Result<ConnectedServer, String> {
    match &server.transport {
        McpTransport::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            if command.trim().is_empty() {
                return Err("stdio command must not be empty".to_owned());
            }
            let mut command = tokio::process::Command::new(command);
            command.args(args).envs(env);
            if let Some(cwd) = cwd {
                command.current_dir(cwd);
            }
            let transport = TokioChildProcess::new(command)
                .map_err(|error| format!("failed to launch stdio transport: {error}"))?;
            let client = tokio::time::timeout(server.startup_timeout, ().serve(transport))
                .await
                .map_err(|_| startup_timeout(server))?
                .map_err(|error| format!("MCP initialize failed: {error}"))?;
            finish_startup(server, client).await
        }
        McpTransport::StreamableHttp {
            url,
            bearer,
            headers,
        } => {
            // rmcp deliberately leaves the rustls crypto provider to its host.
            // Installing ring is idempotent and keeps this crate usable without
            // requiring nanocodex-service to have opened a WebSocket first.
            drop(rustls::crypto::ring::default_provider().install_default());
            if url.trim().is_empty() {
                return Err("Streamable HTTP URL must not be empty".to_owned());
            }
            let mut resolved_headers = HashMap::with_capacity(headers.len());
            for (name, source) in headers {
                let name = name
                    .parse::<HeaderName>()
                    .map_err(|error| format!("invalid HTTP header name `{name}`: {error}"))?;
                let value = source.resolve()?;
                let mut value = HeaderValue::from_str(&value)
                    .map_err(|error| format!("invalid value for HTTP header `{name}`: {error}"))?;
                value.set_sensitive(true);
                resolved_headers.insert(name, value);
            }
            let mut config = StreamableHttpClientTransportConfig::with_uri(url.clone())
                .custom_headers(resolved_headers)
                .reinit_on_expired_session(true);
            if let Some(bearer) = bearer {
                let token = bearer.resolve()?;
                if token.trim().is_empty() {
                    return Err("resolved bearer token must not be empty".to_owned());
                }
                config = config.auth_header(token);
            }
            let transport = StreamableHttpClientTransport::from_config(config);
            let client = tokio::time::timeout(server.startup_timeout, ().serve(transport))
                .await
                .map_err(|_| startup_timeout(server))?
                .map_err(|error| format!("MCP initialize failed: {error}"))?;
            finish_startup(server, client).await
        }
    }
}

async fn finish_startup(
    server: &McpServer,
    client: RunningService<RoleClient, ()>,
) -> Result<ConnectedServer, String> {
    let client = Arc::new(client);
    let tools = tokio::time::timeout(server.startup_timeout, client.list_all_tools())
        .await
        .map_err(|_| startup_timeout(server))?
        .map_err(|error| format!("MCP tools/list failed: {error}"))?
        .into_iter()
        .filter(|tool| server.includes_tool(tool.name.as_ref()))
        .collect();
    Ok(ConnectedServer { client, tools })
}

fn startup_timeout(server: &McpServer) -> String {
    format!(
        "MCP startup exceeded {:.1} seconds",
        server.startup_timeout.as_secs_f64()
    )
}
