use eyre::{Result, WrapErr};
use nanocodex::{Mcp, McpServer, Nanocodex, Thinking, Tools};

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("OPENAI_API_KEY").wrap_err("OPENAI_API_KEY is required")?;
    let server = match std::env::var("NANOCODEX_MCP_URL") {
        Ok(url) => {
            let server = McpServer::http(url).description("Application-configured MCP server.");
            if std::env::var_os("NANOCODEX_MCP_BEARER_TOKEN").is_some() {
                server.bearer_token_env("NANOCODEX_MCP_BEARER_TOKEN")
            } else {
                server
            }
        }
        Err(_) => McpServer::http("https://developers.openai.com/mcp")
            .description("Search OpenAI developer documentation."),
    };
    let mcp = Mcp::builder().server("docs", server).build()?;
    let tools = Tools::builder().without_defaults().provider(mcp).build()?;
    let (agent, mut events) = Nanocodex::builder(api_key)
        .thinking(Thinking::Low)
        .tools(tools)
        .build()?;

    let turn = agent
        .prompt(
            "In one Code Mode exec cell, call `tool_search` with `nanocodex.tools/call`, then call one returned read-only MCP tool by passing its name to `nanocodex.tools/call`. Briefly summarize its result.",
        )
        .await?;
    events.write_turn_jsonl(std::io::stdout()).await?;
    eprintln!("final result: {}", turn.result().await?.final_message);
    Ok(())
}
