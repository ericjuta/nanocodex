use std::{
    collections::HashMap,
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use eyre::{Result, WrapErr};
use nanocodex::{
    AgentEventKind, AgentEvents, AgentHandle, Nanocodex, Thinking, Tool, ToolContext,
    ToolDefinition, ToolExecution, ToolInput, ToolResult, Tools, ToolsBuildError, async_trait,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentTask {
    role: String,
    task: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FollowUpTask {
    agent_id: u64,
    task: String,
}

#[derive(Serialize)]
struct WorkerResult {
    agent_id: u64,
    kind: &'static str,
    role: String,
    report: String,
}

#[derive(Serialize)]
struct FollowUpResult {
    agent_id: u64,
    report: String,
}

const ORCHESTRATION_BRIEF: &str = r"We are choosing Nanocodex's next orchestration slice.

Decision context:
- The primary user experience is one long-running root agent with 3-8 short-lived specialist
  branches instructed to operate read-only. This instruction-based policy is not a sandbox or
  capability boundary.
- Fast live branching matters more than provider-side prompt privacy.
- We must remain a headless, library-first SDK: no app server and no generic core scheduler.
- `/btw` currently provides one ephemeral fork of the latest safe root boundary.
- Branches share the workspace but receive fresh drivers, WebSockets, and tool runtimes.
- The release should prefer correctness and explicit lifecycle behavior over adding more UI surface.

Candidate next slices:
A. Multiple named `/btw` panes.
B. Turn cancellation plus safe branch cleanup.
C. Durable serializable conversation snapshots with checkpoint acceleration.

Treat this as private product context that independent agents do not inherit.";

const DEFAULT_GOAL: &str = r"Recommend which candidate orchestration slice should be implemented
next. Investigate the repository and return an evidence-backed decision, the most important
tradeoffs, a minimal vertical implementation plan, and concrete acceptance tests.

Use Code Mode and the available child-agent tools wherever they improve the result. You own the
orchestration: decide how to decompose the work, which workers need inherited context, what can run
concurrently, which workers deserve follow-up prompts, whether another synthesis pass is useful,
and when enough evidence has been gathered.";

#[derive(Default)]
struct ChildAgents {
    next_id: AtomicU64,
    agents: tokio::sync::Mutex<HashMap<u64, Nanocodex>>,
}

impl ChildAgents {
    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed) + 1
    }

    async fn insert(&self, id: u64, agent: Nanocodex) {
        self.agents.lock().await.insert(id, agent);
    }

    async fn get(&self, id: u64) -> Option<Nanocodex> {
        self.agents.lock().await.get(&id).cloned()
    }
}

#[derive(Clone, Copy)]
enum ChildKind {
    Spawn,
    Fork,
}

impl ChildKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Spawn => "spawn_agent",
            Self::Fork => "fork_agent",
        }
    }

    const fn result_name(self) -> &'static str {
        match self {
            Self::Spawn => "independent",
            Self::Fork => "fork",
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::Spawn => {
                "Starts a reusable clean agent without the invoking agent's conversation history, runs its first task, and returns its agent_id and report."
            }
            Self::Fork => {
                "Starts a reusable agent from the invoking agent's latest safe model/tool boundary, runs its first task, and returns its agent_id and report."
            }
        }
    }

    fn prompt(self, task: String) -> String {
        match self {
            Self::Spawn => format!(
                "Act as an independent research subagent with no inherited conversation. Complete \
                 only this delegated task. You may inspect the workspace with Code Mode commands. \
                 You are instructed to operate read-only: do not modify files or run destructive \
                 commands. This is an instruction-based policy, not a sandbox or capability \
                 boundary. Return a compact evidence-backed report.\n\nDelegated task:\n{task}"
            ),
            Self::Fork => task,
        }
    }
}

/// Application-defined Code Mode tool for either a clean or contextual child.
struct ChildAgent {
    agent: AgentHandle,
    agents: Weak<ChildAgents>,
    kind: ChildKind,
}

fn show_child_lifecycle(label: String, mut events: AgentEvents) {
    if std::env::var_os("NANOCODEX_SUBAGENT_JSONL").is_none() {
        return;
    }
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if matches!(
                event.kind,
                AgentEventKind::RunStarted
                    | AgentEventKind::RunCompleted
                    | AgentEventKind::RunFailed
            ) {
                eprintln!("{}", json!({ "agent": label.as_str(), "event": event }));
            }
        }
    });
}

impl ChildAgent {
    const fn new(agent: AgentHandle, agents: Weak<ChildAgents>, kind: ChildKind) -> Self {
        Self {
            agent,
            agents,
            kind,
        }
    }
}

#[async_trait]
impl Tool for ChildAgent {
    fn name(&self) -> &'static str {
        self.kind.name()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            self.name(),
            self.kind.description(),
            json!({
                "type": "object",
                "properties": {
                    "role": {
                        "type": "string",
                        "description": "A short worker role for result attribution."
                    },
                    "task": {
                        "type": "string",
                        "description": "A complete, focused task for the child agent."
                    }
                },
                "required": ["role", "task"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let AgentTask { role, task } = input.decode_json()?;
        let agents = self
            .agents
            .upgrade()
            .ok_or_else(|| std::io::Error::other("child-agent registry stopped"))?;
        let (child, events) = match self.kind {
            ChildKind::Spawn => self.agent.spawn().await,
            ChildKind::Fork => self.agent.fork().await,
        }?;
        let agent_id = agents.next_id();

        show_child_lifecycle(
            format!("agent-{agent_id}:{}:{role}", self.kind.result_name()),
            events,
        );

        let result = child.prompt(self.kind.prompt(task)).await?.result().await?;
        agents.insert(agent_id, child).await;
        Ok(ToolExecution::json(&WorkerResult {
            agent_id,
            kind: self.kind.result_name(),
            role,
            report: result.final_message,
        }))
    }
}

/// Sends another prompt through an existing child's retained session.
struct PromptAgent {
    agents: Weak<ChildAgents>,
}

#[async_trait]
impl Tool for PromptAgent {
    fn name(&self) -> &'static str {
        "prompt_agent"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            self.name(),
            "Runs a follow-up turn on a previously spawned or forked agent, preserving that agent's conversation, response chain, cache lineage, WebSocket, and tools.",
            json!({
                "type": "object",
                "properties": {
                    "agent_id": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "The agent_id returned by spawn_agent or fork_agent."
                    },
                    "task": {
                        "type": "string",
                        "description": "The next prompt for that agent."
                    }
                },
                "required": ["agent_id", "task"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let FollowUpTask { agent_id, task } = input.decode_json()?;
        let agents = self
            .agents
            .upgrade()
            .ok_or_else(|| std::io::Error::other("child-agent registry stopped"))?;
        let child = agents
            .get(agent_id)
            .await
            .ok_or_else(|| std::io::Error::other(format!("unknown agent_id {agent_id}")))?;
        let result = child.prompt(task).await?.result().await?;
        Ok(ToolExecution::json(&FollowUpResult {
            agent_id,
            report: result.final_message,
        }))
    }
}

fn subagent_tools(
    agent: AgentHandle,
    agents: Weak<ChildAgents>,
) -> std::result::Result<Tools, ToolsBuildError> {
    Tools::builder()
        .tool(ChildAgent::new(
            agent.clone(),
            agents.clone(),
            ChildKind::Spawn,
        ))
        .tool(ChildAgent::new(agent, agents.clone(), ChildKind::Fork))
        .tool(PromptAgent { agents })
        .build()
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let api_key = std::env::var("OPENAI_API_KEY").wrap_err("OPENAI_API_KEY is required")?;
    let workspace = std::env::current_dir().wrap_err("failed to resolve the workspace")?;
    let child_agents = Arc::new(ChildAgents::default());
    let tools_agents = Arc::downgrade(&child_agents);
    let (agent, events) = Nanocodex::builder(api_key)
        .instructions(
            "You are the lead engineering orchestrator. Code Mode exposes spawn_agent for a reusable clean child, fork_agent for a reusable child with the invoking agent's latest safe context, and prompt_agent for follow-up turns using a returned agent_id. Decide your own decomposition, concurrency, sequencing, follow-ups, and synthesis. Treat worker outputs as attributed evidence rather than fabricating them.",
        )
        .thinking(Thinking::Low)
        .tools_factory(move |agent| subagent_tools(agent, tools_agents.clone()))
        .workspace(workspace)
        .build()?;
    drop(events);

    agent
        .prompt(format!(
            "Without using tools, commit this orchestration brief as the decision context for later workers. Reply exactly BRIEF_COMMITTED.\n\n{ORCHESTRATION_BRIEF}"
        ))
        .await?
        .result()
        .await?;

    let result = agent
        .prompt(
            std::env::args()
                .nth(1)
                .unwrap_or_else(|| DEFAULT_GOAL.to_owned()),
        )
        .await?
        .result()
        .await?;
    println!("{}", result.final_message);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures_util::{SinkExt, StreamExt};
    use serde_json::Value;
    use tokio::{net::TcpListener, time::timeout};
    use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};

    use super::*;
    use nanocodex::Responses;

    #[tokio::test]
    async fn subagent_example_exposes_workspace_and_agent_tools() -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("ws://{}", listener.local_addr()?);
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            let mut socket = accept_async(stream).await?;
            let warmup = next_json(&mut socket).await?;
            send_json(
                &mut socket,
                json!({
                    "type": "response.completed",
                    "response": { "id": "resp-warmup", "usage": null }
                }),
            )
            .await?;

            let _generation = next_json(&mut socket).await?;
            send_json(
                &mut socket,
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp-final",
                        "status": "completed",
                        "output": [{
                            "type": "message",
                            "role": "assistant",
                            "content": [{ "type": "output_text", "text": "done" }]
                        }],
                        "usage": null
                    }
                }),
            )
            .await?;
            Ok::<_, eyre::Report>(warmup)
        });

        let child_agents = Arc::new(ChildAgents::default());
        let tools_agents = Arc::downgrade(&child_agents);
        let responses = Responses::builder().websocket_url(endpoint).build();
        let workspace = std::env::current_dir().wrap_err("failed to resolve the workspace")?;
        let (agent, events) = Nanocodex::builder("test-key")
            .instructions("Inspect the available tools.")
            .thinking(Thinking::Low)
            .tools_factory(move |agent| subagent_tools(agent, tools_agents.clone()))
            .workspace(workspace)
            .responses(responses)
            .session_id("subagent-example-tools-test")
            .build()?;
        drop(events);

        agent
            .prompt("List the available tools.")
            .await?
            .result()
            .await?;
        drop(agent);
        let warmup = timeout(Duration::from_secs(5), server)
            .await
            .wrap_err("mock Responses server did not finish")???;
        let description = warmup["input"][0]["tools"][0]["description"]
            .as_str()
            .ok_or_else(|| eyre::eyre!("warmup request omitted the Code Mode description"))?;

        for name in [
            "exec_command",
            "hashline__read",
            "spawn_agent",
            "fork_agent",
            "prompt_agent",
        ] {
            assert!(
                description.contains(&format!("### `{name}`")),
                "Code Mode description omitted {name}"
            );
        }
        Ok(())
    }

    async fn next_json<S>(socket: &mut WebSocketStream<S>) -> Result<Value>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        loop {
            let message = socket
                .next()
                .await
                .ok_or_else(|| eyre::eyre!("client closed before sending a request"))??;
            if let Message::Text(text) = message {
                return Ok(serde_json::from_str(text.as_str())?);
            }
        }
    }

    async fn send_json<S>(socket: &mut WebSocketStream<S>, value: Value) -> Result<()>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        socket.send(Message::Text(value.to_string().into())).await?;
        Ok(())
    }
}
