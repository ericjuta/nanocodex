use std::{
    io::Write,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use eyre::{OptionExt, Result, WrapErr};
use nanocodex::{
    AgentEvent, Nanocodex, Thinking, Tool, ToolContext, ToolDefinition, ToolExecution, ToolInput,
    Tools, async_trait,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum EventSource {
    Parent,
    Subagent { id: u64, task: Arc<str> },
}

struct RoutedEvent {
    source: EventSource,
    event: AgentEvent,
}

#[derive(Serialize)]
struct UnifiedEvent<'a> {
    stream_seq: u64,
    source: &'a EventSource,
    #[serde(flatten)]
    event: &'a AgentEvent,
}

/// An application-owned tool whose implementation happens to run another agent.
struct SpawnAgent {
    api_key: Arc<str>,
    workspace: PathBuf,
    events: mpsc::UnboundedSender<RoutedEvent>,
    next_id: AtomicU64,
}

impl SpawnAgent {
    fn new(
        api_key: impl Into<Arc<str>>,
        workspace: impl Into<PathBuf>,
        events: mpsc::UnboundedSender<RoutedEvent>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            workspace: workspace.into(),
            events,
            next_id: AtomicU64::new(1),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnAgentArgs {
    task: String,
}

#[async_trait]
impl Tool for SpawnAgent {
    fn name(&self) -> &'static str {
        "spawn_agent"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            self.name(),
            "Runs one focused task in an independent Nanocodex session and returns its final message.",
            json!({
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "A complete, self-contained task for the subagent."
                    }
                },
                "required": ["task"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolExecution {
        let args: SpawnAgentArgs = match input.decode_json() {
            Ok(args) => args,
            Err(error) => return ToolExecution::error(error.to_string()),
        };
        let source = EventSource::Subagent {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            task: Arc::from(args.task.as_str()),
        };

        // Children get the normal local coding tools but not this SpawnAgent
        // value, so delegation is one level deep unless the application chooses
        // to construct a recursive registry itself.
        let child_tools = match Tools::builder().without_defaults().build() {
            Ok(tools) => tools,
            Err(error) => return ToolExecution::error(error.to_string()),
        };
        let (child, mut child_events) = match Nanocodex::builder(self.api_key.to_string())
            .prompt(
                "You are a focused subagent. Complete only the delegated task and return a concise result to the parent agent.",
            )
            .thinking(Thinking::Low)
            .tools(child_tools)
            .workspace(self.workspace.clone())
            .build()
        {
            Ok(child) => child,
            Err(error) => return ToolExecution::error(error.to_string()),
        };

        let turn = match child.prompt(args.task).await {
            Ok(turn) => turn,
            Err(error) => return ToolExecution::error(error.to_string()),
        };
        let mut result = Box::pin(turn.result());
        let mut events_open = true;
        let outcome = loop {
            tokio::select! {
                outcome = &mut result => break outcome,
                event = child_events.recv(), if events_open => match event {
                    Some(event) => drop(self.events.send(RoutedEvent {
                        source: source.clone(),
                        event,
                    })),
                    None => events_open = false,
                }
            }
        };

        // Closing the last command handle lets the child's driver terminate;
        // drain through that close so its terminal event reaches the host
        // before this tool call completes.
        drop(child);
        while let Some(event) = child_events.recv().await {
            drop(self.events.send(RoutedEvent {
                source: source.clone(),
                event,
            }));
        }

        match outcome {
            Ok(result) => ToolExecution::text(result.final_message),
            Err(error) => ToolExecution::error(error.to_string()),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("OPENAI_API_KEY").wrap_err("OPENAI_API_KEY is required")?;
    let workspace = std::env::current_dir().wrap_err("failed to resolve the workspace")?;
    let (subagent_event_tx, mut subagent_events) = mpsc::unbounded_channel();
    let tools = Tools::builder()
        .without_defaults()
        .tool(SpawnAgent::new(
            api_key.clone(),
            workspace.clone(),
            subagent_event_tx,
        ))
        .build()?;
    let (agent, mut parent_events) = Nanocodex::builder(api_key)
        .thinking(Thinking::Low)
        .tools(tools)
        .workspace(workspace)
        .build()?;

    let turn = agent
        .prompt(
            r#"Use code mode to run exactly these two calls concurrently with Promise.all:
- spawn_agent({ task: "Return only the sum of the first ten positive integers." })
- spawn_agent({ task: "Return only the product of 6 and 7." })
Then reply with one sentence containing both returned answers."#,
        )
        .await?;
    let mut result = Box::pin(turn.result());
    let mut completed = None;
    let mut parent_terminal = false;
    let mut stream_seq = 1;
    let mut stdout = std::io::stdout().lock();

    while completed.is_none() || !parent_terminal {
        let routed = tokio::select! {
            outcome = &mut result, if completed.is_none() => {
                completed = Some(outcome?);
                continue;
            }
            event = parent_events.recv(), if !parent_terminal => {
                let event = event.ok_or_eyre("parent event stream closed before its terminal event")?;
                parent_terminal = event.kind.is_terminal();
                RoutedEvent { source: EventSource::Parent, event }
            }
            event = subagent_events.recv() => {
                event.ok_or_eyre("subagent event stream closed while the parent was running")?
            }
        };
        serde_json::to_writer(
            &mut stdout,
            &UnifiedEvent {
                stream_seq,
                source: &routed.source,
                event: &routed.event,
            },
        )?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
        stream_seq += 1;
    }

    // Every child drains its own event stream before returning its tool result,
    // so once the parent completes any remaining records are already queued.
    while let Ok(routed) = subagent_events.try_recv() {
        serde_json::to_writer(
            &mut stdout,
            &UnifiedEvent {
                stream_seq,
                source: &routed.source,
                event: &routed.event,
            },
        )?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
        stream_seq += 1;
    }

    let result = completed.ok_or_eyre("turn completed without a result")?;
    eprintln!("final result: {}", result.final_message);
    Ok(())
}
