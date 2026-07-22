use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use nanocodex::{
    AgentEventKind, AgentEvents, AgentHandle, Nanocodex, Tool, ToolContext, ToolDefinition,
    ToolExecution, ToolInput, ToolResult, Tools, ToolsBuildError, TurnControl, async_trait,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::task::JoinHandle;

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

struct ChildSession {
    session_id: String,
    agent: Nanocodex,
    event_task: JoinHandle<()>,
}

#[derive(Default)]
struct RegistryState {
    admitting: bool,
    agents: HashMap<u64, ChildSession>,
    active: HashMap<u64, TurnControl>,
    waits: HashMap<String, HashMap<String, HashSet<u64>>>,
}

pub(crate) struct ChildAgents {
    next_id: AtomicU64,
    next_invocation: AtomicU64,
    state: Arc<tokio::sync::Mutex<RegistryState>>,
    cleanup_tx: tokio::sync::mpsc::UnboundedSender<CleanupRequest>,
    cleanup_task: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    shutdown_gate: tokio::sync::Mutex<()>,
}

impl Default for ChildAgents {
    fn default() -> Self {
        let state = Arc::new(tokio::sync::Mutex::new(RegistryState {
            admitting: true,
            ..RegistryState::default()
        }));
        let (cleanup_tx, mut cleanup_rx) = tokio::sync::mpsc::unbounded_channel::<CleanupRequest>();
        let cleanup_state = Arc::clone(&state);
        let cleanup_task = tokio::spawn(async move {
            while let Some(request) = cleanup_rx.recv().await {
                let CleanupRequest::Invocation {
                    invocation_id,
                    caller,
                    target,
                } = request
                else {
                    break;
                };
                let control = {
                    let mut state = cleanup_state.lock().await;
                    remove_edge(&mut state.waits, &caller, &target, invocation_id);
                    state.active.remove(&invocation_id)
                };
                if let Some(control) = control {
                    drop(control.cancel().await);
                }
            }
        });
        Self {
            next_id: AtomicU64::new(0),
            next_invocation: AtomicU64::new(0),
            state,
            cleanup_tx,
            cleanup_task: tokio::sync::Mutex::new(Some(cleanup_task)),
            shutdown_gate: tokio::sync::Mutex::new(()),
        }
    }
}

impl ChildAgents {
    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed) + 1
    }

    async fn insert(
        &self,
        id: u64,
        session_id: String,
        agent: Nanocodex,
        event_task: JoinHandle<()>,
    ) -> Result<(), std::io::Error> {
        let mut state = self.state.lock().await;
        if !state.admitting {
            return Err(std::io::Error::other(
                "child-agent registry is shutting down",
            ));
        }
        state.agents.insert(
            id,
            ChildSession {
                session_id,
                agent,
                event_task,
            },
        );
        Ok(())
    }

    async fn get(&self, id: u64) -> Option<(String, Nanocodex)> {
        self.state
            .lock()
            .await
            .agents
            .get(&id)
            .map(|session| (session.session_id.clone(), session.agent.clone()))
    }

    async fn reserve(&self, caller: &str, target: &str) -> Result<InvocationGuard, std::io::Error> {
        let mut state = self.state.lock().await;
        if !state.admitting {
            return Err(std::io::Error::other(
                "child-agent registry is shutting down",
            ));
        }
        if caller == target {
            return Err(std::io::Error::other("an agent cannot prompt itself"));
        }
        if reaches(&state.waits, target, caller) {
            return Err(std::io::Error::other(
                "prompt_agent would create a directed wait cycle",
            ));
        }
        let invocation_id = self.next_invocation.fetch_add(1, Ordering::Relaxed) + 1;
        state
            .waits
            .entry(caller.to_owned())
            .or_default()
            .entry(target.to_owned())
            .or_default()
            .insert(invocation_id);
        Ok(InvocationGuard {
            invocation_id,
            caller: caller.to_owned(),
            target: target.to_owned(),
            cleanup_tx: self.cleanup_tx.clone(),
        })
    }

    async fn attach(
        &self,
        guard: &InvocationGuard,
        control: TurnControl,
    ) -> Result<(), std::io::Error> {
        let mut state = self.state.lock().await;
        let reserved = state
            .waits
            .get(&guard.caller)
            .and_then(|targets| targets.get(&guard.target))
            .is_some_and(|ids| ids.contains(&guard.invocation_id));
        if !state.admitting || !reserved {
            return Err(std::io::Error::other(
                "child-agent registry is shutting down",
            ));
        }
        state.active.insert(guard.invocation_id, control);
        Ok(())
    }

    pub(crate) async fn shutdown(&self) {
        let _shutdown = self.shutdown_gate.lock().await;
        let (sessions, controls) = {
            let mut state = self.state.lock().await;
            state.admitting = false;
            state.waits.clear();
            (
                std::mem::take(&mut state.agents),
                std::mem::take(&mut state.active),
            )
        };
        for control in controls.into_values() {
            drop(control.cancel().await);
        }
        if let Some(cleanup_task) = self.cleanup_task.lock().await.take() {
            drop(self.cleanup_tx.send(CleanupRequest::Shutdown));
            drop(cleanup_task.await);
        }
        let mut event_tasks = Vec::with_capacity(sessions.len());
        for session in sessions.into_values() {
            event_tasks.push(session.event_task);
            drop(session.agent);
        }
        for event_task in event_tasks {
            drop(event_task.await);
        }
    }
}

fn reaches(
    edges: &HashMap<String, HashMap<String, HashSet<u64>>>,
    start: &str,
    goal: &str,
) -> bool {
    let mut pending = vec![start];
    let mut visited = HashSet::new();
    while let Some(node) = pending.pop() {
        if node == goal {
            return true;
        }
        if visited.insert(node)
            && let Some(next) = edges.get(node)
        {
            pending.extend(next.keys().map(String::as_str));
        }
    }
    false
}

fn remove_edge(
    edges: &mut HashMap<String, HashMap<String, HashSet<u64>>>,
    caller: &str,
    target: &str,
    invocation_id: u64,
) {
    if let Some(targets) = edges.get_mut(caller) {
        if let Some(invocations) = targets.get_mut(target) {
            invocations.remove(&invocation_id);
            if invocations.is_empty() {
                targets.remove(target);
            }
        }
        if targets.is_empty() {
            edges.remove(caller);
        }
    }
}

struct InvocationGuard {
    invocation_id: u64,
    caller: String,
    target: String,
    cleanup_tx: tokio::sync::mpsc::UnboundedSender<CleanupRequest>,
}

enum CleanupRequest {
    Invocation {
        invocation_id: u64,
        caller: String,
        target: String,
    },
    Shutdown,
}

impl Drop for InvocationGuard {
    fn drop(&mut self) {
        drop(self.cleanup_tx.send(CleanupRequest::Invocation {
            invocation_id: self.invocation_id,
            caller: std::mem::take(&mut self.caller),
            target: std::mem::take(&mut self.target),
        }));
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
                "Starts a reusable clean-room child agent without the invoking agent's conversation history, runs its first task, and returns its agent_id and report. The child may inspect the shared workspace but is instructed not to modify it."
            }
            Self::Fork => {
                "Starts a reusable read-only child agent from the invoking agent's latest safe model boundary, runs its first task, and returns its agent_id and report. During an active turn this includes the current prompt and all work completed before the latest model call."
            }
        }
    }

    fn prompt(self, task: &str) -> String {
        let context = match self {
            Self::Spawn => "You have no inherited conversation context.",
            Self::Fork => "Use the inherited conversation only as context for this delegation.",
        };
        format!(
            "Act as a read-only specialist child agent. {context} Inspect the shared workspace as \
             needed, but do not modify files or run destructive commands. Return a compact, \
             evidence-backed report to the parent agent.\n\nDelegated task:\n{task}"
        )
    }
}

struct ChildAgent {
    agent: AgentHandle,
    agents: Weak<ChildAgents>,
    kind: ChildKind,
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

fn drain_events(
    agent_id: u64,
    role: String,
    kind: &'static str,
    mut events: AgentEvents,
) -> JoinHandle<()> {
    let log_jsonl = std::env::var_os("NANOCODEX_SUBAGENT_JSONL").is_some();
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if log_jsonl
                && matches!(
                    event.kind,
                    AgentEventKind::RunStarted
                        | AgentEventKind::RunCompleted
                        | AgentEventKind::RunFailed
                )
            {
                eprintln!(
                    "{}",
                    json!({
                        "agent_id": agent_id,
                        "role": role,
                        "kind": kind,
                        "event": event,
                    })
                );
            }
        }
    })
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

    async fn execute(&self, input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        let AgentTask { role, task } = input.decode_json()?;
        let agents = self
            .agents
            .upgrade()
            .ok_or_else(|| std::io::Error::other("child-agent registry stopped"))?;
        let agent_id = agents.next_id();
        let (child, events) = match self.kind {
            ChildKind::Spawn => self.agent.spawn().await,
            ChildKind::Fork => self.agent.fork().await,
        }?;
        let child_session_id = events.request_id().to_owned();
        let event_task = drain_events(agent_id, role.clone(), self.kind.result_name(), events);
        agents
            .insert(
                agent_id,
                child_session_id.clone(),
                child.clone(),
                event_task,
            )
            .await?;
        let guard = agents
            .reserve(context.session_id, &child_session_id)
            .await?;
        let turn = child.prompt(self.kind.prompt(&task)).await?;
        if let Err(error) = agents.attach(&guard, turn.control()).await {
            drop(turn.cancel().await);
            return Err(error.into());
        }
        let _guard = guard;
        let result = turn.result().await?;
        Ok(ToolExecution::json(&WorkerResult {
            agent_id,
            kind: self.kind.result_name(),
            role,
            report: result.final_message,
        }))
    }
}

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
            "Runs a follow-up turn on a previously spawned or forked child, preserving that child's conversation, response chain, cache lineage, WebSocket, and tools.",
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
                        "description": "The next prompt for that child agent."
                    }
                },
                "required": ["agent_id", "task"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        let FollowUpTask { agent_id, task } = input.decode_json()?;
        let agents = self
            .agents
            .upgrade()
            .ok_or_else(|| std::io::Error::other("child-agent registry stopped"))?;
        let (child_session_id, child) = agents
            .get(agent_id)
            .await
            .ok_or_else(|| std::io::Error::other(format!("unknown agent_id {agent_id}")))?;
        let guard = agents
            .reserve(context.session_id, &child_session_id)
            .await?;
        let turn = child.prompt(task).await?;
        if let Err(error) = agents.attach(&guard, turn.control()).await {
            drop(turn.cancel().await);
            return Err(error.into());
        }
        let _guard = guard;
        let result = turn.result().await?;
        Ok(ToolExecution::json(&FollowUpResult {
            agent_id,
            report: result.final_message,
        }))
    }
}

pub(crate) fn with_subagents(
    tools: Tools,
    agent: AgentHandle,
    agents: Weak<ChildAgents>,
) -> Result<Tools, ToolsBuildError> {
    tools
        .into_builder()
        .tool(ChildAgent::new(
            agent.clone(),
            agents.clone(),
            ChildKind::Spawn,
        ))
        .tool(ChildAgent::new(agent, agents.clone(), ChildKind::Fork))
        .tool(PromptAgent { agents })
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_detects_directed_cycles() {
        let mut edges = HashMap::new();
        edges.insert(
            "a".to_owned(),
            HashMap::from([("b".to_owned(), HashSet::from([1]))]),
        );
        edges.insert(
            "b".to_owned(),
            HashMap::from([("c".to_owned(), HashSet::from([2]))]),
        );

        assert!(reaches(&edges, "a", "c"));
        assert!(!reaches(&edges, "c", "a"));
        assert!(reaches(&edges, "a", "a"));
    }

    #[tokio::test]
    async fn dropped_guard_requests_cleanup() {
        let agents = Arc::new(ChildAgents::default());
        agents
            .state
            .lock()
            .await
            .waits
            .entry("parent".to_owned())
            .or_default()
            .entry("child".to_owned())
            .or_default()
            .insert(1);
        drop(InvocationGuard {
            invocation_id: 1,
            caller: "parent".to_owned(),
            target: "child".to_owned(),
            cleanup_tx: agents.cleanup_tx.clone(),
        });

        for _ in 0..10 {
            if agents.state.lock().await.waits.is_empty() {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("dropped invocation guard did not request cleanup");
    }

    #[test]
    fn cleanup_removes_only_its_invocation_edge() {
        let mut edges = HashMap::from([(
            "parent".to_owned(),
            HashMap::from([("child".to_owned(), HashSet::from([7, 8]))]),
        )]);

        remove_edge(&mut edges, "parent", "child", 7);

        assert_eq!(edges["parent"]["child"], HashSet::from([8]));
    }

    #[tokio::test]
    async fn stale_cleanup_token_does_not_remove_newer_follow_up() {
        let agents = ChildAgents::default();
        let first = agents.reserve("parent", "child").await.unwrap();
        let second = agents.reserve("parent", "child").await.unwrap();
        drop(first);

        for _ in 0..10 {
            let remaining = agents
                .state
                .lock()
                .await
                .waits
                .get("parent")
                .and_then(|targets| targets.get("child"))
                .cloned();
            if remaining == Some(HashSet::from([second.invocation_id])) {
                drop(second);
                agents.shutdown().await;
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("stale cleanup removed a newer reservation");
    }

    #[tokio::test]
    async fn cycle_is_rejected_before_turn_acceptance() {
        let agents = ChildAgents::default();
        let _first = agents.reserve("a", "b").await.unwrap();
        let _second = agents.reserve("b", "c").await.unwrap();

        let error = match agents.reserve("c", "a").await {
            Ok(_) => panic!("cycle reservation unexpectedly succeeded"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("directed wait cycle"));
        assert_eq!(agents.next_invocation.load(Ordering::Relaxed), 2);
        agents.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_closes_admission() {
        let agents = ChildAgents::default();
        agents.shutdown().await;

        assert!(!agents.state.lock().await.admitting);
    }

    #[tokio::test]
    async fn shutdown_is_idempotent() {
        let agents = Arc::new(ChildAgents::default());
        let first = {
            let agents = Arc::clone(&agents);
            tokio::spawn(async move { agents.shutdown().await })
        };
        let second = {
            let agents = Arc::clone(&agents);
            tokio::spawn(async move { agents.shutdown().await })
        };

        first.await.unwrap();
        second.await.unwrap();

        let state = agents.state.lock().await;
        assert!(!state.admitting);
        assert!(state.agents.is_empty());
        assert!(state.active.is_empty());
    }
}
