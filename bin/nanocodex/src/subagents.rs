use std::{
    collections::{HashMap, HashSet},
    io,
    sync::{
        Arc, Mutex, MutexGuard, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use nanocodex::{
    AgentEventKind, AgentEvents, AgentHandle, Nanocodex, NanocodexError, Tool, ToolContext,
    ToolDefinition, ToolError, ToolExecution, ToolInput, ToolResult, Tools, ToolsBuildError,
    TurnControl, async_trait,
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
    parent_session_id: String,
    agent: Nanocodex,
    event_task: JoinHandle<()>,
}

impl ChildSession {
    async fn drain_owned(self) -> Result<(), StoredIoError> {
        tracing::debug!(
            child.session_id = self.session_id,
            child.parent_session_id = self.parent_session_id,
            "draining child agent"
        );
        drop(self.agent);
        self.event_task.await.map_err(|error| {
            StoredIoError::new(
                io::ErrorKind::Other,
                format!("child event-drain task failed: {error}"),
            )
        })
    }
}

#[derive(Clone, Debug)]
struct StoredIoError {
    kind: io::ErrorKind,
    message: Arc<str>,
}

impl StoredIoError {
    fn new(kind: io::ErrorKind, message: impl Into<Arc<str>>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    fn from_error(error: impl std::fmt::Display) -> Self {
        Self::new(io::ErrorKind::Other, error.to_string())
    }

    fn to_io_error(&self) -> io::Error {
        io::Error::new(self.kind, self.message.to_string())
    }
}

#[derive(Debug)]
struct ToolCleanupError {
    primary: ToolError,
    cleanup: io::Error,
}

impl std::fmt::Display for ToolCleanupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}; child session cleanup also failed: {}",
            self.primary, self.cleanup
        )
    }
}

impl std::error::Error for ToolCleanupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.primary.as_ref())
    }
}

fn preserve_tool_primary<T, E>(
    result: Result<T, E>,
    cleanup: Result<(), io::Error>,
) -> Result<T, ToolError>
where
    E: std::error::Error + Send + Sync + 'static,
{
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(cleanup)) => Err(Box::new(cleanup)),
        (Err(primary), Ok(())) => Err(Box::new(primary)),
        (Err(primary), Err(cleanup)) => Err(Box::new(ToolCleanupError {
            primary: Box::new(primary),
            cleanup,
        })),
    }
}

struct ActiveInvocation {
    caller: String,
    target: String,
    control: Option<TurnControl>,
    initial_child: Option<u64>,
}

#[derive(Default)]
struct RegistryState {
    admitting: bool,
    pending_creations: usize,
    pending_session_handoffs: usize,
    agents: HashMap<u64, ChildSession>,
    invocations: HashMap<u64, ActiveInvocation>,
    waits: HashMap<String, HashMap<String, HashSet<u64>>>,
    shutdown_result: Option<Result<(), StoredIoError>>,
    shutdown_task: Option<JoinHandle<()>>,
    fallback_session_drains: Vec<JoinHandle<()>>,
    #[cfg(test)]
    cleanup_worker_joins: usize,
    #[cfg(test)]
    cleanup_worker_join_attempts: usize,
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum TestBarrierPoint {
    Creation,
    Prompt,
    Control,
    ShutdownAdmission,
    ShutdownControl,
    CleanupInvocation,
    SessionDrain,
    WorkerJoin,
}

#[cfg(test)]
struct TestBarrier {
    reached: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

#[cfg(test)]
#[derive(Default)]
struct TestBarriers {
    creation: Mutex<Option<TestBarrier>>,
    prompt: Mutex<Option<TestBarrier>>,
    control: Mutex<Option<TestBarrier>>,
    shutdown_admission: Mutex<Option<TestBarrier>>,
    shutdown_control: Mutex<Option<TestBarrier>>,
    cleanup_invocation: Mutex<Option<TestBarrier>>,
    session_drain: Mutex<Option<TestBarrier>>,
    worker_join: Mutex<Option<TestBarrier>>,
}

#[cfg(test)]
impl TestBarriers {
    fn slot(&self, point: TestBarrierPoint) -> &Mutex<Option<TestBarrier>> {
        match point {
            TestBarrierPoint::Creation => &self.creation,
            TestBarrierPoint::Prompt => &self.prompt,
            TestBarrierPoint::Control => &self.control,
            TestBarrierPoint::ShutdownAdmission => &self.shutdown_admission,
            TestBarrierPoint::ShutdownControl => &self.shutdown_control,
            TestBarrierPoint::CleanupInvocation => &self.cleanup_invocation,
            TestBarrierPoint::SessionDrain => &self.session_drain,
            TestBarrierPoint::WorkerJoin => &self.worker_join,
        }
    }
}

#[cfg(test)]
async fn pause_test_barrier(barriers: &TestBarriers, point: TestBarrierPoint) {
    let barrier = barriers
        .slot(point)
        .lock()
        .expect("test barrier mutex should not be poisoned")
        .take();
    if let Some(barrier) = barrier {
        let _ = barrier.reached.send(());
        drop(barrier.release.await);
    }
}

pub(crate) struct ChildAgents {
    next_id: AtomicU64,
    next_invocation: AtomicU64,
    state: Arc<Mutex<RegistryState>>,
    cleanup_tx: tokio::sync::mpsc::UnboundedSender<CleanupRequest>,
    cleanup_task: Mutex<Option<JoinHandle<()>>>,
    cleanup_progress: Arc<tokio::sync::Notify>,
    cleanup_failures: Arc<Mutex<Vec<StoredIoError>>>,
    shutdown_notify: tokio::sync::Notify,
    #[cfg(test)]
    shutdown_panic: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    cleanup_worker_panic: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(test)]
    test_barriers: Arc<TestBarriers>,
}

impl Default for ChildAgents {
    fn default() -> Self {
        let state = Arc::new(Mutex::new(RegistryState {
            admitting: true,
            ..RegistryState::default()
        }));
        let (cleanup_tx, mut cleanup_rx) = tokio::sync::mpsc::unbounded_channel::<CleanupRequest>();
        let cleanup_state = Arc::clone(&state);
        let cleanup_progress = Arc::new(tokio::sync::Notify::new());
        let worker_progress = Arc::clone(&cleanup_progress);
        let cleanup_failures = Arc::new(Mutex::new(Vec::new()));
        let worker_failures = Arc::clone(&cleanup_failures);
        #[cfg(test)]
        let test_barriers = Arc::new(TestBarriers::default());
        #[cfg(test)]
        let worker_barriers = Arc::clone(&test_barriers);
        #[cfg(test)]
        let cleanup_worker_panic = Arc::new(std::sync::atomic::AtomicBool::new(false));
        #[cfg(test)]
        let worker_panic = Arc::clone(&cleanup_worker_panic);
        let cleanup_task = tokio::spawn(async move {
            while let Some(request) = cleanup_rx.recv().await {
                #[cfg(test)]
                assert!(
                    !worker_panic.swap(false, Ordering::SeqCst),
                    "injected child cleanup-worker panic"
                );
                match request {
                    CleanupRequest::Invocation(request) => {
                        #[cfg(test)]
                        pause_test_barrier(&worker_barriers, TestBarrierPoint::CleanupInvocation)
                            .await;
                        let failures = cleanup_dropped_invocation(&cleanup_state, request).await;
                        record_cleanup_failures(&worker_failures, failures);
                        worker_progress.notify_waiters();
                    }
                    CleanupRequest::Session { session, complete } => {
                        #[cfg(test)]
                        pause_test_barrier(&worker_barriers, TestBarrierPoint::SessionDrain).await;
                        let result = session.drain_owned().await;
                        publish_drain_receipt(result, complete, &worker_failures);
                        worker_progress.notify_waiters();
                    }
                    CleanupRequest::Barrier(complete) => {
                        let _ = complete.send(());
                    }
                    CleanupRequest::Shutdown => break,
                }
            }
        });
        Self {
            next_id: AtomicU64::new(0),
            next_invocation: AtomicU64::new(0),
            state,
            cleanup_tx,
            cleanup_task: Mutex::new(Some(cleanup_task)),
            cleanup_progress,
            cleanup_failures,
            shutdown_notify: tokio::sync::Notify::new(),
            #[cfg(test)]
            shutdown_panic: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            cleanup_worker_panic,
            #[cfg(test)]
            test_barriers,
        }
    }
}

impl ChildAgents {
    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn state(&self) -> Result<MutexGuard<'_, RegistryState>, std::io::Error> {
        lock_registry(&self.state)
    }

    fn begin_creation(&self) -> Result<CreationGuard, io::Error> {
        let mut state = self.state()?;
        if !state.admitting {
            return Err(registry_stopped());
        }
        state.pending_creations += 1;
        Ok(CreationGuard {
            state: Arc::clone(&self.state),
            cleanup_progress: Arc::clone(&self.cleanup_progress),
            active: true,
        })
    }

    #[cfg(test)]
    fn install_barrier(
        &self,
        point: TestBarrierPoint,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (reached, reached_rx) = tokio::sync::oneshot::channel();
        let (release, release_rx) = tokio::sync::oneshot::channel();
        *self
            .test_barriers
            .slot(point)
            .lock()
            .expect("test barrier mutex should not be poisoned") = Some(TestBarrier {
            reached,
            release: release_rx,
        });
        (reached_rx, release)
    }

    #[cfg(test)]
    async fn pause_at_barrier(&self, point: TestBarrierPoint) {
        pause_test_barrier(&self.test_barriers, point).await;
    }

    fn insert(
        &self,
        id: u64,
        session_id: String,
        parent_session_id: String,
        agent: Nanocodex,
        event_task: JoinHandle<()>,
    ) -> Result<(), (std::io::Error, ChildSession)> {
        let session = ChildSession {
            session_id,
            parent_session_id,
            agent,
            event_task,
        };
        let mut state = match self.state() {
            Ok(state) => state,
            Err(error) => return Err((error, session)),
        };
        if !state.admitting {
            return Err((registry_stopped(), session));
        }
        state.agents.insert(id, session);
        Ok(())
    }

    fn get(&self, id: u64) -> Result<Option<(String, Nanocodex)>, std::io::Error> {
        Ok(self
            .state()?
            .agents
            .get(&id)
            .map(|session| (session.session_id.clone(), session.agent.clone())))
    }

    fn take_child_for_drain(&self, id: u64) -> Result<Option<ChildSession>, std::io::Error> {
        let mut state = self.state()?;
        let session = state.agents.remove(&id);
        if session.is_some() {
            state.pending_session_handoffs += 1;
        }
        Ok(session)
    }

    fn reserve(
        &self,
        caller: &str,
        target: &str,
        initial_child: Option<u64>,
    ) -> Result<InvocationGuard, std::io::Error> {
        let mut state = self.state()?;
        if !state.admitting {
            return Err(registry_stopped());
        }
        if caller == target || reaches(&state.waits, target, caller) {
            return Err(std::io::Error::other(
                "prompt_agent would create a child wait cycle",
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
        state.invocations.insert(
            invocation_id,
            ActiveInvocation {
                caller: caller.to_owned(),
                target: target.to_owned(),
                control: None,
                initial_child,
            },
        );
        Ok(InvocationGuard {
            request: Some(DroppedInvocation {
                invocation_id,
                caller: caller.to_owned(),
                target: target.to_owned(),
                control: None,
                initial_child,
            }),
            state: Arc::clone(&self.state),
            cleanup_tx: self.cleanup_tx.clone(),
            cleanup_progress: Arc::clone(&self.cleanup_progress),
            cleanup_failures: Arc::clone(&self.cleanup_failures),
            #[cfg(test)]
            test_barriers: Arc::clone(&self.test_barriers),
        })
    }

    fn transfer_session(
        &self,
        session: ChildSession,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<(), StoredIoError>>, io::Error> {
        let (complete, completed) = tokio::sync::oneshot::channel();
        match self
            .cleanup_tx
            .send(CleanupRequest::Session { session, complete })
        {
            Ok(()) => Ok(completed),
            Err(error) => {
                let CleanupRequest::Session { session, complete } = error.0 else {
                    return Err(io::Error::other(
                        "child cleanup worker rejected an unexpected request",
                    ));
                };
                let failures = Arc::clone(&self.cleanup_failures);
                #[cfg(test)]
                let barriers = Arc::clone(&self.test_barriers);
                let task = tokio::spawn(async move {
                    #[cfg(test)]
                    pause_test_barrier(&barriers, TestBarrierPoint::SessionDrain).await;
                    let result = session.drain_owned().await;
                    publish_drain_receipt(result, complete, &failures);
                });
                self.state()?.fallback_session_drains.push(task);
                Ok(completed)
            }
        }
    }

    async fn drain_session(
        &self,
        session: ChildSession,
        tracked_handoff: bool,
    ) -> Result<(), io::Error> {
        let completed = self.transfer_session(session)?;
        if tracked_handoff {
            let mut state = self.state()?;
            state.pending_session_handoffs = state.pending_session_handoffs.saturating_sub(1);
            drop(state);
            self.cleanup_progress.notify_waiters();
        }
        completed
            .await
            .map_err(|_| io::Error::other("child cleanup worker dropped a session receipt"))?
            .map_err(|error| error.to_io_error())
    }

    pub(crate) async fn shutdown(self: &Arc<Self>) -> Result<(), io::Error> {
        self.start_shutdown()?;
        loop {
            let notified = self.shutdown_notify.notified();
            if let Some(result) = self.state()?.shutdown_result.clone() {
                return result.map_err(|error| error.to_io_error());
            }
            notified.await;
        }
    }

    fn start_shutdown(self: &Arc<Self>) -> Result<(), io::Error> {
        let mut state = self.state()?;
        if !state.admitting {
            return Ok(());
        }
        state.admitting = false;
        let agents = Arc::clone(self);
        let worker = tokio::spawn(async move { agents.run_shutdown().await });
        let agents = Arc::downgrade(self);
        state.shutdown_task = Some(tokio::spawn(async move {
            let result = match worker.await {
                Ok(result) => result.map_err(StoredIoError::from_error),
                Err(error) => Err(StoredIoError::new(
                    io::ErrorKind::Other,
                    format!("child-agent shutdown worker failed: {error}"),
                )),
            };
            if let Some(agents) = agents.upgrade() {
                agents.publish_shutdown(result);
            }
        }));
        Ok(())
    }

    async fn run_shutdown(&self) -> Result<(), io::Error> {
        let mut failures = Vec::new();
        #[cfg(test)]
        self.pause_at_barrier(TestBarrierPoint::ShutdownAdmission)
            .await;
        if let Err(error) = self.cancel_shutdown_controls(&mut failures).await {
            failures.push(StoredIoError::from_error(error));
        }
        if let Err(error) = self.wait_for_owned_work().await {
            failures.push(StoredIoError::from_error(error));
        }
        if let Err(error) = self.drain_shutdown_sessions(&mut failures).await {
            failures.push(StoredIoError::from_error(error));
        }
        self.join_fallback_session_drains(&mut failures).await;
        self.join_cleanup_worker(&mut failures).await;

        match self.cleanup_failures.lock() {
            Ok(cleanup_failures) => failures.extend(cleanup_failures.iter().cloned()),
            Err(_) => failures.push(StoredIoError::new(
                io::ErrorKind::Other,
                "child cleanup failure mutex poisoned",
            )),
        }
        finish_cleanup(&failures)
    }

    async fn cancel_shutdown_controls(
        &self,
        failures: &mut Vec<StoredIoError>,
    ) -> Result<(), io::Error> {
        let controls = self
            .state()?
            .invocations
            .values()
            .filter_map(|invocation| invocation.control.clone())
            .collect::<Vec<_>>();
        #[cfg(test)]
        self.pause_at_barrier(TestBarrierPoint::ShutdownControl)
            .await;
        for control in controls {
            if let Err(error) = control.cancel().await
                && let Some(error) = cancellation_error("shutdown", &error)
            {
                failures.push(error);
            }
        }
        Ok(())
    }

    async fn wait_for_owned_work(&self) -> Result<(), io::Error> {
        let mut worker_alive = true;
        let mut barrier_error = None;
        loop {
            if worker_alive && let Err(error) = self.cleanup_barrier().await {
                worker_alive = false;
                barrier_error = Some(error);
            }
            let progress = self.cleanup_progress.notified();
            let empty = {
                let state = self.state()?;
                state.pending_creations == 0
                    && state.pending_session_handoffs == 0
                    && state.invocations.is_empty()
            };
            if empty {
                return match barrier_error {
                    Some(error) => Err(error),
                    None => Ok(()),
                };
            }
            progress.await;
        }
    }

    async fn drain_shutdown_sessions(
        &self,
        failures: &mut Vec<StoredIoError>,
    ) -> Result<(), io::Error> {
        let sessions = {
            let mut state = self.state()?;
            state.invocations.clear();
            state.waits.clear();
            std::mem::take(&mut state.agents)
        };
        let mut receipts = Vec::with_capacity(sessions.len());
        for session in sessions.into_values() {
            match self.transfer_session(session) {
                Ok(receipt) => receipts.push(receipt),
                Err(error) => failures.push(StoredIoError::from_error(error)),
            }
        }
        for receipt in receipts {
            match receipt.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => failures.push(error),
                Err(_) => failures.push(StoredIoError::new(
                    io::ErrorKind::Other,
                    "child cleanup worker dropped a session receipt",
                )),
            }
        }
        Ok(())
    }

    async fn join_fallback_session_drains(&self, failures: &mut Vec<StoredIoError>) {
        loop {
            let fallback_drains = match self.state() {
                Ok(mut state) => std::mem::take(&mut state.fallback_session_drains),
                Err(error) => {
                    failures.push(StoredIoError::from_error(error));
                    return;
                }
            };
            if fallback_drains.is_empty() {
                break;
            }
            for fallback in fallback_drains {
                if let Err(error) = fallback.await {
                    failures.push(StoredIoError::new(
                        io::ErrorKind::Other,
                        format!("fallback child session drain failed: {error}"),
                    ));
                }
            }
        }
    }

    async fn join_cleanup_worker(&self, failures: &mut Vec<StoredIoError>) {
        if let Err(error) = self.cleanup_barrier().await {
            failures.push(StoredIoError::from_error(error));
        }
        if self.cleanup_tx.send(CleanupRequest::Shutdown).is_err() {
            failures.push(StoredIoError::new(
                io::ErrorKind::Other,
                "child cleanup worker stopped before shutdown",
            ));
        }
        let cleanup_task = {
            let mut cleanup_task = match self.cleanup_task.lock() {
                Ok(cleanup_task) => cleanup_task,
                Err(poisoned) => {
                    failures.push(StoredIoError::new(
                        io::ErrorKind::Other,
                        "child cleanup task mutex poisoned",
                    ));
                    poisoned.into_inner()
                }
            };
            cleanup_task.take()
        };
        if let Some(cleanup_task) = cleanup_task {
            #[cfg(test)]
            match self.state() {
                Ok(mut state) => state.cleanup_worker_join_attempts += 1,
                Err(error) => failures.push(StoredIoError::from_error(error)),
            }
            let join_result = cleanup_task.await;
            #[cfg(test)]
            match self.state() {
                Ok(mut state) => state.cleanup_worker_joins += 1,
                Err(error) => failures.push(StoredIoError::from_error(error)),
            }
            if let Err(error) = join_result {
                failures.push(StoredIoError::new(
                    io::ErrorKind::Other,
                    format!("child cleanup worker failed: {error}"),
                ));
            }
        } else {
            failures.push(StoredIoError::new(
                io::ErrorKind::Other,
                "child cleanup worker was already joined",
            ));
        }
        #[cfg(test)]
        self.pause_at_barrier(TestBarrierPoint::WorkerJoin).await;
        #[cfg(test)]
        {
            assert!(
                !self.shutdown_panic.swap(false, Ordering::SeqCst),
                "injected child-agent shutdown panic"
            );
        }
    }

    fn publish_shutdown(&self, result: Result<(), StoredIoError>) {
        match self.state() {
            Ok(mut state) => {
                state.shutdown_result = Some(result);
                drop(state.shutdown_task.take());
            }
            Err(error) => tracing::error!(%error, "failed to publish child-agent shutdown"),
        }
        self.shutdown_notify.notify_waiters();
    }

    #[cfg(test)]
    fn inject_shutdown_panic(&self) {
        self.shutdown_panic.store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    fn inject_cleanup_worker_panic(&self) {
        self.cleanup_worker_panic.store(true, Ordering::SeqCst);
    }

    async fn cleanup_barrier(&self) -> Result<(), io::Error> {
        let (complete, completed) = tokio::sync::oneshot::channel();
        self.cleanup_tx
            .send(CleanupRequest::Barrier(complete))
            .map_err(|_| {
                io::Error::other("child cleanup worker stopped before its drain barrier")
            })?;
        completed
            .await
            .map_err(|_| io::Error::other("child cleanup worker dropped its drain barrier"))
    }
}

fn finish_cleanup(failures: &[StoredIoError]) -> Result<(), io::Error> {
    if failures.is_empty() {
        return Ok(());
    }
    let message = failures
        .iter()
        .map(|error| error.message.as_ref())
        .collect::<Vec<_>>()
        .join("; ");
    Err(io::Error::new(failures[0].kind, message))
}

fn record_cleanup_failures(
    failures: &Mutex<Vec<StoredIoError>>,
    new_failures: impl IntoIterator<Item = StoredIoError>,
) {
    match failures.lock() {
        Ok(mut failures) => failures.extend(new_failures),
        Err(error) => tracing::error!(%error, "failed to retain child cleanup errors"),
    }
}

fn publish_drain_receipt(
    result: Result<(), StoredIoError>,
    complete: tokio::sync::oneshot::Sender<Result<(), StoredIoError>>,
    failures: &Mutex<Vec<StoredIoError>>,
) {
    if let Err(result) = complete.send(result)
        && let Err(error) = result
    {
        record_cleanup_failures(failures, [error]);
    }
}

fn cancellation_error(context: &str, error: &NanocodexError) -> Option<StoredIoError> {
    if matches!(
        error,
        NanocodexError::TurnNotCancellable | NanocodexError::AgentStopped
    ) {
        return None;
    }
    Some(StoredIoError::new(
        io::ErrorKind::Other,
        format!("failed to cancel child turn during {context}: {error}"),
    ))
}

fn registry_stopped() -> std::io::Error {
    std::io::Error::other("child-agent registry is shutting down")
}

fn lock_registry(
    state: &Mutex<RegistryState>,
) -> Result<MutexGuard<'_, RegistryState>, std::io::Error> {
    state
        .lock()
        .map_err(|_| std::io::Error::other("child-agent registry state poisoned"))
}

async fn cleanup_dropped_invocation(
    state: &Mutex<RegistryState>,
    request: DroppedInvocation,
) -> Vec<StoredIoError> {
    let mut failures = Vec::new();
    let (control, session) = match lock_registry(state) {
        Ok(mut state) => {
            let stored = state.invocations.remove(&request.invocation_id);
            let (stored_control, stored_initial_child) = if let Some(stored) = stored {
                remove_edge(
                    &mut state.waits,
                    &stored.caller,
                    &stored.target,
                    request.invocation_id,
                );
                (stored.control, stored.initial_child)
            } else {
                remove_edge(
                    &mut state.waits,
                    &request.caller,
                    &request.target,
                    request.invocation_id,
                );
                (None, None)
            };
            let control = request.control.or(stored_control);
            let initial_child = request.initial_child.or(stored_initial_child);
            let session = initial_child.and_then(|id| state.agents.remove(&id));
            (control, session)
        }
        Err(error) => {
            tracing::error!(%error, "failed to remove dropped child invocation");
            failures.push(StoredIoError::from_error(error));
            (request.control, None)
        }
    };
    if let Some(control) = control
        && let Err(error) = control.cancel().await
        && let Some(error) = cancellation_error("dropped invocation cleanup", &error)
    {
        failures.push(error);
    }
    if let Some(session) = session
        && let Err(error) = session.drain_owned().await
    {
        failures.push(error);
    }
    failures
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
    request: Option<DroppedInvocation>,
    state: Arc<Mutex<RegistryState>>,
    cleanup_tx: tokio::sync::mpsc::UnboundedSender<CleanupRequest>,
    cleanup_progress: Arc<tokio::sync::Notify>,
    cleanup_failures: Arc<Mutex<Vec<StoredIoError>>>,
    #[cfg(test)]
    test_barriers: Arc<TestBarriers>,
}

struct CreationGuard {
    state: Arc<Mutex<RegistryState>>,
    cleanup_progress: Arc<tokio::sync::Notify>,
    active: bool,
}

impl Drop for CreationGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        match lock_registry(&self.state) {
            Ok(mut state) => {
                state.pending_creations = state.pending_creations.saturating_sub(1);
            }
            Err(error) => tracing::error!(%error, "failed to finish child creation ownership"),
        }
        self.cleanup_progress.notify_waiters();
    }
}

struct DroppedInvocation {
    invocation_id: u64,
    caller: String,
    target: String,
    control: Option<TurnControl>,
    initial_child: Option<u64>,
}

enum CleanupRequest {
    Invocation(DroppedInvocation),
    Session {
        session: ChildSession,
        complete: tokio::sync::oneshot::Sender<Result<(), StoredIoError>>,
    },
    Barrier(tokio::sync::oneshot::Sender<()>),
    Shutdown,
}

impl InvocationGuard {
    fn attach(&mut self, control: TurnControl) -> Result<(), std::io::Error> {
        let Some(request) = self.request.as_mut() else {
            return Err(std::io::Error::other("child invocation already completed"));
        };
        request.control = Some(control.clone());
        let mut state = lock_registry(&self.state)?;
        if !state.admitting {
            return Err(registry_stopped());
        }
        let Some(invocation) = state.invocations.get_mut(&request.invocation_id) else {
            return Err(registry_stopped());
        };
        invocation.control = Some(control);
        Ok(())
    }

    fn finish_terminal(
        mut self,
        initial_succeeded: bool,
    ) -> Result<Option<ChildSession>, std::io::Error> {
        let Some(request) = self.request.as_ref() else {
            return Ok(None);
        };
        let mut state = lock_registry(&self.state)?;
        remove_edge(
            &mut state.waits,
            &request.caller,
            &request.target,
            request.invocation_id,
        );
        let stored = state.invocations.remove(&request.invocation_id);
        let initial_child = request
            .initial_child
            .or_else(|| stored.and_then(|entry| entry.initial_child));
        let session = if initial_succeeded {
            None
        } else {
            initial_child.and_then(|id| state.agents.remove(&id))
        };
        if session.is_some() {
            state.pending_session_handoffs += 1;
        }
        self.request = None;
        drop(state);
        self.cleanup_progress.notify_waiters();
        Ok(session)
    }
}

impl Drop for InvocationGuard {
    fn drop(&mut self) {
        let Some(request) = self.request.take() else {
            return;
        };
        let Err(error) = self.cleanup_tx.send(CleanupRequest::Invocation(request)) else {
            return;
        };
        let CleanupRequest::Invocation(request) = error.0 else {
            return;
        };
        let cleanup_state = Arc::clone(&self.state);
        let cleanup_failures = Arc::clone(&self.cleanup_failures);
        let cleanup_progress = Arc::clone(&self.cleanup_progress);
        #[cfg(test)]
        let test_barriers = Arc::clone(&self.test_barriers);
        match lock_registry(&self.state) {
            Ok(mut state) => {
                state.pending_session_handoffs += 1;
                let task = tokio::spawn(async move {
                    #[cfg(test)]
                    pause_test_barrier(&test_barriers, TestBarrierPoint::CleanupInvocation).await;
                    let failures = cleanup_dropped_invocation(&cleanup_state, request).await;
                    record_cleanup_failures(&cleanup_failures, failures);
                    cleanup_progress.notify_waiters();
                });
                state.fallback_session_drains.push(task);
                state.pending_session_handoffs = state.pending_session_handoffs.saturating_sub(1);
                drop(state);
                self.cleanup_progress.notify_waiters();
            }
            Err(error) => {
                record_cleanup_failures(&self.cleanup_failures, [StoredIoError::from_error(error)]);
            }
        }
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
                "Starts a reusable clean-room child agent without the invoking agent's conversation history, runs its first task, and returns its agent_id and report. The child can use the shared workspace tools but is instructed not to modify it; this policy is not a sandbox or security boundary."
            }
            Self::Fork => {
                "Starts a reusable child agent from the invoking agent's latest safe model boundary, runs its first task, and returns its agent_id and report. The child is instructed not to modify the shared workspace, but that policy is not a sandbox or security boundary. During an active turn the fork includes the current prompt and all work completed before the latest model call."
            }
        }
    }

    fn prompt(self, task: &str) -> String {
        let context = match self {
            Self::Spawn => "You have no inherited conversation context.",
            Self::Fork => "Use the inherited conversation only as context for this delegation.",
        };
        format!(
            "You are instructed to operate as a non-modifying specialist child agent. {context} \
             You have normal workspace tools, so this instruction is policy rather than a sandbox \
             or security boundary. Inspect the shared workspace as needed, but do not modify files \
             or run destructive commands. Return a compact, evidence-backed report to the parent \
             agent.\n\nDelegated task:\n{task}"
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
        #[cfg(test)]
        assert_ne!(role, "panic-event-drain", "injected event-drain panic");
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
        let creation = agents.begin_creation()?;
        let agent_id = agents.next_id();
        let (child, events) = match self.kind {
            ChildKind::Spawn => self.agent.spawn().await,
            ChildKind::Fork => self.agent.fork().await,
        }?;
        #[cfg(test)]
        agents.pause_at_barrier(TestBarrierPoint::Creation).await;
        let child_session_id = events.request_id().to_owned();
        let event_task = drain_events(agent_id, role.clone(), self.kind.result_name(), events);
        if let Err((error, session)) = agents.insert(
            agent_id,
            child_session_id.clone(),
            context.session_id.to_owned(),
            child.clone(),
            event_task,
        ) {
            drop(child);
            let cleanup = agents.drain_session(session, false).await;
            return preserve_tool_primary(Err::<ToolExecution, _>(error), cleanup);
        }
        let mut guard = match agents.reserve(context.session_id, &child_session_id, Some(agent_id))
        {
            Ok(guard) => guard,
            Err(error) => {
                if let Some(session) = agents.take_child_for_drain(agent_id)? {
                    drop(child);
                    let cleanup = agents.drain_session(session, true).await;
                    return preserve_tool_primary(Err::<ToolExecution, _>(error), cleanup);
                }
                return Err(error.into());
            }
        };
        drop(creation);
        #[cfg(test)]
        agents.pause_at_barrier(TestBarrierPoint::Prompt).await;
        let turn = match child.prompt(self.kind.prompt(&task)).await {
            Ok(turn) => turn,
            Err(error) => {
                if let Some(session) = guard.finish_terminal(false)? {
                    drop(child);
                    let cleanup = agents.drain_session(session, true).await;
                    return preserve_tool_primary(Err::<ToolExecution, _>(error), cleanup);
                }
                return Err(error.into());
            }
        };
        if let Err(error) = guard.attach(turn.control()) {
            drop(turn);
            drop(guard);
            return Err(error.into());
        }
        #[cfg(test)]
        agents.pause_at_barrier(TestBarrierPoint::Control).await;
        let result = turn.result().await;
        let cleanup = if let Some(session) = guard.finish_terminal(result.is_ok())? {
            drop(child);
            agents.drain_session(session, true).await
        } else {
            Ok(())
        };
        let result = preserve_tool_primary(result, cleanup)?;
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
            .get(agent_id)?
            .ok_or_else(|| std::io::Error::other(format!("unknown agent_id {agent_id}")))?;
        let mut guard = agents.reserve(context.session_id, &child_session_id, None)?;
        let turn = match child.prompt(task).await {
            Ok(turn) => turn,
            Err(error) => {
                drop(guard.finish_terminal(true)?);
                return Err(error.into());
            }
        };
        if let Err(error) = guard.attach(turn.control()) {
            drop(turn);
            drop(guard);
            return Err(error.into());
        }
        let result = turn.result().await;
        drop(guard.finish_terminal(true)?);
        let result = result?;
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
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    use eyre::{Result, WrapErr, eyre};
    use futures_util::{SinkExt, StreamExt};
    use nanocodex::{DEFAULT_TOOL_OUTPUT_TOKENS, Responses, Thinking, ToolOutputBody};
    use serde_json::{Value, value::to_raw_value};
    use tokio::{
        net::TcpListener,
        sync::{Notify, Semaphore, mpsc, oneshot},
        task::JoinSet,
        time::timeout,
    };
    use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};

    use super::*;

    const TEST_TIMEOUT: Duration = Duration::from_secs(5);
    const ROOT_SESSION: &str = "subagent-lifecycle-root";

    struct LifecycleTracker {
        active_tools: AtomicUsize,
        started_tools: AtomicUsize,
        connections: AtomicUsize,
        requests: Mutex<HashMap<String, usize>>,
        request_inputs: Mutex<HashMap<String, Vec<String>>>,
        changed: Notify,
        permits: Semaphore,
    }

    impl Default for LifecycleTracker {
        fn default() -> Self {
            Self {
                active_tools: AtomicUsize::new(0),
                started_tools: AtomicUsize::new(0),
                connections: AtomicUsize::new(0),
                requests: Mutex::new(HashMap::new()),
                request_inputs: Mutex::new(HashMap::new()),
                changed: Notify::new(),
                permits: Semaphore::new(0),
            }
        }
    }

    impl LifecycleTracker {
        async fn wait_for(&self, predicate: impl Fn(&Self) -> bool) -> Result<()> {
            timeout(TEST_TIMEOUT, async {
                loop {
                    let changed = self.changed.notified();
                    if predicate(self) {
                        return;
                    }
                    changed.await;
                }
            })
            .await
            .map_err(|_| {
                eyre!(
                    "lifecycle condition was not reached: active={} started={} connections={} requests={:?}",
                    self.active_tools.load(Ordering::SeqCst),
                    self.started_tools.load(Ordering::SeqCst),
                    self.connections.load(Ordering::SeqCst),
                    self.requests
                        .lock()
                        .expect("request counter mutex should not be poisoned")
                )
            })?;
            Ok(())
        }

        async fn wait_active(&self, expected: usize) -> Result<()> {
            self.wait_for(|tracker| tracker.active_tools.load(Ordering::SeqCst) == expected)
                .await
        }

        async fn wait_started(&self, expected: usize) -> Result<()> {
            self.wait_for(|tracker| tracker.started_tools.load(Ordering::SeqCst) >= expected)
                .await
        }

        async fn wait_connections(&self, expected: usize) -> Result<()> {
            self.wait_for(|tracker| tracker.connections.load(Ordering::SeqCst) == expected)
                .await
        }

        fn requests_for(&self, session_id: &str) -> usize {
            self.requests
                .lock()
                .expect("request counter mutex should not be poisoned")
                .get(session_id)
                .copied()
                .unwrap_or_default()
        }

        fn inputs_for(&self, session_id: &str) -> Vec<String> {
            self.request_inputs
                .lock()
                .expect("request input mutex should not be poisoned")
                .get(session_id)
                .cloned()
                .unwrap_or_default()
        }
    }

    struct ToolActivity {
        tracker: Arc<LifecycleTracker>,
    }

    impl ToolActivity {
        fn start(tracker: Arc<LifecycleTracker>) -> Self {
            tracker.active_tools.fetch_add(1, Ordering::SeqCst);
            tracker.started_tools.fetch_add(1, Ordering::SeqCst);
            tracker.changed.notify_waiters();
            Self { tracker }
        }
    }

    impl Drop for ToolActivity {
        fn drop(&mut self) {
            self.tracker.active_tools.fetch_sub(1, Ordering::SeqCst);
            self.tracker.changed.notify_waiters();
        }
    }

    struct GateTool {
        tracker: Arc<LifecycleTracker>,
    }

    #[async_trait]
    impl Tool for GateTool {
        fn name(&self) -> &'static str {
            "test_gate"
        }

        fn definition(&self) -> ToolDefinition {
            ToolDefinition::function(
                self.name(),
                "Blocks until the deterministic lifecycle test releases it.",
                json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }),
            )
        }

        async fn execute(&self, _input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
            let _activity = ToolActivity::start(Arc::clone(&self.tracker));
            let permit = self.tracker.permits.acquire().await?;
            permit.forget();
            Ok(ToolExecution::text("released"))
        }
    }

    struct ConnectionActivity {
        tracker: Arc<LifecycleTracker>,
    }

    impl ConnectionActivity {
        fn start(tracker: Arc<LifecycleTracker>) -> Self {
            tracker.connections.fetch_add(1, Ordering::SeqCst);
            tracker.changed.notify_waiters();
            Self { tracker }
        }
    }

    impl Drop for ConnectionActivity {
        fn drop(&mut self) {
            self.tracker.connections.fetch_sub(1, Ordering::SeqCst);
            self.tracker.changed.notify_waiters();
        }
    }

    struct MockServer {
        endpoint: String,
        stop: Option<oneshot::Sender<()>>,
        task: JoinHandle<Result<()>>,
    }

    impl MockServer {
        async fn start(tracker: Arc<LifecycleTracker>) -> Result<Self> {
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let endpoint = format!("ws://{}", listener.local_addr()?);
            let (stop, stopped) = oneshot::channel();
            let task = tokio::spawn(serve_mock_responses(listener, tracker, stopped));
            Ok(Self {
                endpoint,
                stop: Some(stop),
                task,
            })
        }

        async fn stop(mut self) -> Result<()> {
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            timeout(TEST_TIMEOUT, self.task)
                .await
                .map_err(|_| eyre!("mock Responses server did not stop"))???;
            Ok(())
        }
    }

    async fn serve_mock_responses(
        listener: TcpListener,
        tracker: Arc<LifecycleTracker>,
        mut stopped: oneshot::Receiver<()>,
    ) -> Result<()> {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                _ = &mut stopped => break,
                accepted = listener.accept() => {
                    let (stream, _) = accepted?;
                    connections.spawn(serve_connection(stream, Arc::clone(&tracker)));
                }
                completed = connections.join_next(), if !connections.is_empty() => {
                    completed.ok_or_else(|| eyre!("connection task disappeared"))???;
                }
            }
        }
        connections.abort_all();
        while let Some(completed) = connections.join_next().await {
            match completed {
                Ok(result) => result?,
                Err(error) if error.is_cancelled() => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    async fn serve_connection(
        stream: tokio::net::TcpStream,
        tracker: Arc<LifecycleTracker>,
    ) -> Result<()> {
        let _connection = ConnectionActivity::start(Arc::clone(&tracker));
        let Ok(mut socket) = accept_async(stream).await else {
            return Ok(());
        };
        let mut warmup = true;
        while let Some(message) = socket.next().await {
            let Ok(Message::Text(text)) = message else {
                continue;
            };
            let request: Value = serde_json::from_str(text.as_str())?;
            let session_id = request["client_metadata"]["session_id"]
                .as_str()
                .unwrap_or("unknown")
                .to_owned();
            *tracker
                .requests
                .lock()
                .expect("request counter mutex should not be poisoned")
                .entry(session_id)
                .or_default() += 1;
            tracker.changed.notify_waiters();
            let response_id = format!(
                "resp-{}",
                tracker.requests_for(
                    request["client_metadata"]["session_id"]
                        .as_str()
                        .unwrap_or("unknown")
                )
            );
            if warmup {
                warmup = false;
                send_completed(&mut socket, &response_id, &[]).await?;
                continue;
            }
            let input = request["input"].to_string();
            tracker
                .request_inputs
                .lock()
                .expect("request input mutex should not be poisoned")
                .entry(
                    request["client_metadata"]["session_id"]
                        .as_str()
                        .unwrap_or("unknown")
                        .to_owned(),
                )
                .or_default()
                .push(input.clone());
            if input.contains("function_call_output") || input.contains("custom_tool_call_output") {
                send_final(&mut socket, &response_id).await?;
            } else if input.contains("FAIL_INITIAL") || input.contains("FAIL_FOLLOWUP") {
                send_failed(&mut socket, &response_id).await?;
            } else if input.contains("SPAWN_GRANDCHILD_BLOCK") {
                send_completed(
                    &mut socket,
                    &response_id,
                    &[json!({
                        "type": "custom_tool_call",
                        "call_id": format!("call-{response_id}"),
                        "name": "exec",
                        "input": r#"const report = await tools.spawn_agent({role: "grandchild", task: "BLOCK_GRANDCHILD"}); text(JSON.stringify(report));"#
                    })],
                )
                .await?;
            } else if input.contains("BLOCK_") || input.contains("GATE_") {
                send_completed(
                    &mut socket,
                    &response_id,
                    &[json!({
                        "type": "custom_tool_call",
                        "call_id": format!("call-{response_id}"),
                        "name": "exec",
                        "input": "const result = await tools.test_gate({}); text(result);"
                    })],
                )
                .await?;
            } else {
                send_final(&mut socket, &response_id).await?;
            }
        }
        Ok(())
    }

    async fn send_completed<S>(
        socket: &mut WebSocketStream<S>,
        response_id: &str,
        output: &[Value],
    ) -> Result<()>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        socket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": response_id,
                        "status": "completed",
                        "output": output,
                        "usage": null
                    }
                })
                .to_string()
                .into(),
            ))
            .await?;
        Ok(())
    }

    async fn send_final<S>(socket: &mut WebSocketStream<S>, response_id: &str) -> Result<()>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        send_completed(
            socket,
            response_id,
            &[json!({
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "done" }]
            })],
        )
        .await
    }

    async fn send_failed<S>(socket: &mut WebSocketStream<S>, response_id: &str) -> Result<()>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        socket
            .send(Message::Text(
                json!({
                    "type": "response.failed",
                    "response": {
                        "id": response_id,
                        "status": "failed",
                        "error": {
                            "code": "invalid_image",
                            "message": "deterministic non-retryable failure"
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await?;
        Ok(())
    }

    struct Harness {
        agents: Arc<ChildAgents>,
        root: Nanocodex,
        root_events: AgentEvents,
        root_handle: AgentHandle,
        tracker: Arc<LifecycleTracker>,
        server: MockServer,
        _workspace: tempfile::TempDir,
    }

    impl Harness {
        async fn new() -> Result<Self> {
            let tracker = Arc::new(LifecycleTracker::default());
            let server = MockServer::start(Arc::clone(&tracker)).await?;
            let agents = Arc::new(ChildAgents::default());
            let base_tools = Tools::builder()
                .without_defaults()
                .tool(GateTool {
                    tracker: Arc::clone(&tracker),
                })
                .build()?;
            let (handles, mut received_handles) = mpsc::unbounded_channel();
            let weak_agents = Arc::downgrade(&agents);
            let workspace = tempfile::tempdir()?;
            let responses = Responses::builder()
                .websocket_url(server.endpoint.clone())
                .build();
            let (root, root_events) = Nanocodex::builder("test-key")
                .thinking(Thinking::Low)
                .workspace(workspace.path())
                .responses(responses)
                .session_id(ROOT_SESSION)
                .tools_factory(move |handle| {
                    drop(handles.send(handle.clone()));
                    with_subagents(base_tools.clone(), handle, weak_agents.clone())
                })
                .build()?;
            let root_handle = received_handles
                .recv()
                .await
                .ok_or_else(|| eyre!("root tool factory did not provide its handle"))?;
            Ok(Self {
                agents,
                root,
                root_events,
                root_handle,
                tracker,
                server,
                _workspace: workspace,
            })
        }

        fn spawn_call(&self, task: &str) -> JoinHandle<ToolResult> {
            self.spawn_call_with_role("worker", task)
        }

        fn spawn_call_with_role(&self, role: &str, task: &str) -> JoinHandle<ToolResult> {
            let tool = ChildAgent::new(
                self.root_handle.clone(),
                Arc::downgrade(&self.agents),
                ChildKind::Spawn,
            );
            let role = role.to_owned();
            let task = task.to_owned();
            tokio::spawn(async move {
                tool.execute(
                    function_input(&json!({ "role": role, "task": task })),
                    tool_context(ROOT_SESSION),
                )
                .await
            })
        }

        fn prompt_call(&self, caller: &str, agent_id: u64, task: &str) -> JoinHandle<ToolResult> {
            let tool = PromptAgent {
                agents: Arc::downgrade(&self.agents),
            };
            let caller = caller.to_owned();
            let task = task.to_owned();
            tokio::spawn(async move {
                tool.execute(
                    function_input(&json!({ "agent_id": agent_id, "task": task })),
                    tool_context(&caller),
                )
                .await
            })
        }

        async fn spawn_child(&self, task: &str) -> Result<u64> {
            let execution = timeout(TEST_TIMEOUT, self.spawn_call(task))
                .await
                .map_err(|_| eyre!("spawn_agent did not finish"))??;
            let execution = tool_execution(execution)?;
            execution_json(execution)?["agent_id"]
                .as_u64()
                .ok_or_else(|| eyre!("spawn_agent result omitted agent_id"))
        }

        fn child_session(&self, agent_id: u64) -> Result<String> {
            self.agents
                .state()?
                .agents
                .get(&agent_id)
                .map(|session| session.session_id.clone())
                .ok_or_else(|| eyre!("missing child {agent_id}"))
        }

        async fn close(self) -> Result<()> {
            timeout(TEST_TIMEOUT, self.agents.shutdown())
                .await
                .map_err(|_| eyre!("child registry shutdown timed out"))??;
            drop(self.root);
            drop(self.root_events);
            self.tracker.wait_connections(0).await?;
            self.server.stop().await
        }
    }

    fn function_input(value: &Value) -> ToolInput {
        ToolInput::Function(to_raw_value(value).expect("test input should encode"))
    }

    fn tool_context(session_id: &str) -> ToolContext<'_> {
        ToolContext {
            model: "test-model",
            session_id,
            call_id: "test-call",
            history: &[],
            output_token_budget: DEFAULT_TOOL_OUTPUT_TOKENS,
        }
    }

    fn execution_json(execution: ToolExecution) -> Result<Value> {
        let ToolOutputBody::Text(output) = execution.output else {
            return Err(eyre!("tool result was not text"));
        };
        serde_json::from_str(&output).wrap_err("tool result was not JSON")
    }

    fn tool_execution(result: ToolResult) -> Result<ToolExecution> {
        result.map_err(|error| eyre!(error.to_string()))
    }

    async fn abort_invocation(invocation: JoinHandle<ToolResult>) -> Result<()> {
        invocation.abort();
        match invocation.await {
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(error.into()),
            Ok(_) => Err(eyre!("aborted child invocation unexpectedly completed")),
        }
    }

    async fn abort_shutdown_waiter(waiter: JoinHandle<Result<(), io::Error>>) -> Result<()> {
        waiter.abort();
        match waiter.await {
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(error.into()),
            Ok(_) => Err(eyre!("aborted shutdown waiter unexpectedly completed")),
        }
    }

    fn shutdown_call(agents: &Arc<ChildAgents>) -> JoinHandle<Result<(), io::Error>> {
        let agents = Arc::clone(agents);
        tokio::spawn(async move { agents.shutdown().await })
    }

    async fn finish_shutdown_after_waiter_abort(
        harness: &Harness,
        reached: oneshot::Receiver<()>,
        release: oneshot::Sender<()>,
    ) -> Result<()> {
        let first = shutdown_call(&harness.agents);
        timeout(TEST_TIMEOUT, reached)
            .await
            .map_err(|_| eyre!("shutdown did not reach the requested phase"))??;
        wait_for_registry(&harness.agents, |state| !state.admitting).await?;
        abort_shutdown_waiter(first).await?;
        let second = shutdown_call(&harness.agents);
        let _ = release.send(());
        timeout(TEST_TIMEOUT, second)
            .await
            .map_err(|_| eyre!("replacement shutdown waiter timed out"))???;
        assert_shutdown_drained(harness).await
    }

    async fn assert_shutdown_drained(harness: &Harness) -> Result<()> {
        harness.tracker.wait_active(0).await?;
        harness.tracker.wait_connections(0).await?;
        let state = harness.agents.state()?;
        assert!(!state.admitting);
        assert_eq!(state.pending_creations, 0);
        assert_eq!(state.pending_session_handoffs, 0);
        assert!(state.agents.is_empty());
        assert!(state.invocations.is_empty());
        assert!(state.waits.is_empty());
        assert!(state.fallback_session_drains.is_empty());
        assert!(state.shutdown_result.is_some());
        assert_eq!(state.cleanup_worker_joins, 1);
        assert_eq!(state.cleanup_worker_join_attempts, 1);
        assert!(state.shutdown_task.is_none());
        drop(state);
        assert!(
            harness
                .agents
                .cleanup_task
                .lock()
                .map_err(|_| eyre!("cleanup task mutex poisoned"))?
                .is_none()
        );
        Ok(())
    }

    async fn wait_for_registry(
        agents: &ChildAgents,
        predicate: impl Fn(&RegistryState) -> bool,
    ) -> Result<()> {
        timeout(TEST_TIMEOUT, async {
            loop {
                if predicate(&agents.state().expect("registry should remain healthy")) {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| eyre!("registry condition was not reached"))?;
        Ok(())
    }

    #[test]
    fn graph_preserves_identical_edges_and_detects_long_cycles() {
        let mut edges = HashMap::new();
        edges.insert(
            "a".to_owned(),
            HashMap::from([("b".to_owned(), HashSet::from([1, 2]))]),
        );
        edges.insert(
            "b".to_owned(),
            HashMap::from([("c".to_owned(), HashSet::from([3]))]),
        );
        assert!(reaches(&edges, "a", "c"));
        assert!(!reaches(&edges, "c", "a"));
        remove_edge(&mut edges, "a", "b", 1);
        assert_eq!(edges["a"]["b"], HashSet::from([2]));
    }

    #[tokio::test]
    async fn cancellation_at_creation_and_prompt_barriers_drains_initial_children() -> Result<()> {
        let harness = Harness::new().await?;
        for point in [TestBarrierPoint::Creation, TestBarrierPoint::Prompt] {
            let (reached, _release) = harness.agents.install_barrier(point);
            let invocation = harness.spawn_call("NORMAL_BARRIER");
            timeout(TEST_TIMEOUT, reached)
                .await
                .map_err(|_| eyre!("child invocation did not reach test barrier"))??;
            abort_invocation(invocation).await?;
            harness.agents.cleanup_barrier().await?;
            wait_for_registry(&harness.agents, |state| {
                state.agents.is_empty() && state.invocations.is_empty()
            })
            .await?;
            harness.tracker.wait_connections(0).await?;
        }
        harness.close().await
    }

    #[tokio::test]
    async fn cancelled_spawn_cancels_unreturned_child_and_shutdown_drains_it() -> Result<()> {
        let harness = Harness::new().await?;
        let (reached, _release) = harness.agents.install_barrier(TestBarrierPoint::Control);
        let invocation = harness.spawn_call("BLOCK_CONTROL");
        timeout(TEST_TIMEOUT, reached)
            .await
            .map_err(|_| eyre!("accepted child turn never attached its control"))??;
        harness.tracker.wait_active(1).await?;
        abort_invocation(invocation).await?;
        timeout(TEST_TIMEOUT, harness.agents.shutdown())
            .await
            .map_err(|_| eyre!("shutdown did not drain dropped initial child"))??;
        harness.tracker.wait_active(0).await?;
        {
            let state = harness.agents.state()?;
            assert!(state.agents.is_empty());
            assert!(state.invocations.is_empty());
        }
        harness.close().await
    }

    #[tokio::test]
    async fn failed_initial_is_drained_and_failed_follow_up_retains_child() -> Result<()> {
        let harness = Harness::new().await?;
        assert!(harness.spawn_call("FAIL_INITIAL").await?.is_err());
        wait_for_registry(&harness.agents, |state| state.agents.is_empty()).await?;
        harness.tracker.wait_connections(0).await?;

        let child = harness.spawn_child("NORMAL_INITIAL").await?;
        let failed = harness
            .prompt_call(ROOT_SESSION, child, "FAIL_FOLLOWUP")
            .await?;
        assert!(failed.is_err());
        assert!(harness.agents.state()?.agents.contains_key(&child));
        tool_execution(
            harness
                .prompt_call(ROOT_SESSION, child, "NORMAL_RECOVERY")
                .await?,
        )?;
        harness.close().await
    }

    #[tokio::test]
    async fn failed_initial_turn_retains_turn_and_session_drain_errors() -> Result<()> {
        let harness = Harness::new().await?;
        let error = harness
            .spawn_call_with_role("panic-event-drain", "FAIL_INITIAL_DUAL_ERROR")
            .await?
            .err()
            .ok_or_else(|| eyre!("failed initial turn unexpectedly succeeded"))?;
        let error = error.to_string();
        assert!(error.contains("deterministic non-retryable failure"));
        assert!(error.contains("child event-drain task failed"));
        harness.close().await
    }

    #[tokio::test]
    async fn rejected_insert_retains_registry_and_session_drain_errors() -> Result<()> {
        let harness = Harness::new().await?;
        let (created, release_creation) =
            harness.agents.install_barrier(TestBarrierPoint::Creation);
        let invocation =
            harness.spawn_call_with_role("panic-event-drain", "NORMAL_INSERT_DUAL_ERROR");
        timeout(TEST_TIMEOUT, created)
            .await
            .map_err(|_| eyre!("late child did not reach creation barrier"))??;
        let shutdown = shutdown_call(&harness.agents);
        wait_for_registry(&harness.agents, |state| !state.admitting).await?;
        let _ = release_creation.send(());
        let error = invocation
            .await?
            .err()
            .ok_or_else(|| eyre!("late child insert unexpectedly succeeded"))?
            .to_string();
        assert!(error.contains("child-agent registry is shutting down"));
        assert!(error.contains("child event-drain task failed"));
        timeout(TEST_TIMEOUT, shutdown)
            .await
            .map_err(|_| eyre!("shutdown did not finish after rejected insert"))???;
        assert_shutdown_drained(&harness).await?;
        harness.close().await
    }

    #[tokio::test]
    async fn cancelled_parent_cancels_child_and_grandchild() -> Result<()> {
        let harness = Harness::new().await?;
        let invocation = harness.spawn_call("SPAWN_GRANDCHILD_BLOCK");
        harness.tracker.wait_active(1).await?;
        wait_for_registry(&harness.agents, |state| state.agents.len() == 2).await?;
        abort_invocation(invocation).await?;
        timeout(TEST_TIMEOUT, harness.agents.shutdown())
            .await
            .map_err(|_| eyre!("recursive child shutdown timed out"))??;
        harness.tracker.wait_active(0).await?;
        {
            let state = harness.agents.state()?;
            assert!(state.agents.is_empty());
            assert!(state.invocations.is_empty());
        }
        harness.close().await
    }

    #[tokio::test]
    async fn prompt_agent_rejects_self_wait_before_queueing() -> Result<()> {
        let harness = Harness::new().await?;
        let child = harness.spawn_child("NORMAL_SELF").await?;
        let session = harness.child_session(child)?;
        let before = harness.tracker.requests_for(&session);
        let error = harness
            .prompt_call(&session, child, "NORMAL_IMPOSSIBLE")
            .await?
            .err()
            .ok_or_else(|| eyre!("self wait unexpectedly succeeded"))?;
        assert_eq!(
            error.to_string(),
            "prompt_agent would create a child wait cycle"
        );
        assert_eq!(harness.tracker.requests_for(&session), before);
        harness.close().await
    }

    #[tokio::test]
    async fn prompt_agent_rejects_multi_child_wait_cycle() -> Result<()> {
        let harness = Harness::new().await?;
        let child_a = harness.spawn_child("NORMAL_A").await?;
        let child_b = harness.spawn_child("NORMAL_B").await?;
        let session_a = harness.child_session(child_a)?;
        let session_b = harness.child_session(child_b)?;

        let a_waits_for_b = harness.prompt_call(&session_a, child_b, "BLOCK_CYCLE");
        harness.tracker.wait_active(1).await?;
        let before_a = harness.tracker.requests_for(&session_a);
        let error = harness
            .prompt_call(&session_b, child_a, "NORMAL_CYCLE")
            .await?
            .err()
            .ok_or_else(|| eyre!("multi-child wait cycle unexpectedly succeeded"))?;
        assert_eq!(
            error.to_string(),
            "prompt_agent would create a child wait cycle"
        );
        assert_eq!(harness.tracker.requests_for(&session_a), before_a);
        abort_invocation(a_waits_for_b).await?;
        harness.agents.cleanup_barrier().await?;
        harness.tracker.wait_active(0).await?;

        let child_c = harness.spawn_child("NORMAL_C").await?;
        let fanout_a = harness.prompt_call(ROOT_SESSION, child_a, "GATE_FANOUT_A");
        let fanout_b = harness.prompt_call(ROOT_SESSION, child_c, "GATE_FANOUT_B");
        harness.tracker.wait_active(2).await?;
        harness.tracker.permits.add_permits(2);
        tool_execution(fanout_a.await?)?;
        tool_execution(fanout_b.await?)?;
        harness.close().await
    }

    #[tokio::test]
    async fn stale_cleanup_preserves_identical_live_wait_edge() -> Result<()> {
        let harness = Harness::new().await?;
        let child = harness.spawn_child("NORMAL_STALE").await?;
        let session = harness.child_session(child)?;
        let first = harness.prompt_call(ROOT_SESSION, child, "GATE_FIRST");
        harness.tracker.wait_started(1).await?;
        let second = harness.prompt_call(ROOT_SESSION, child, "GATE_SECOND");
        wait_for_registry(&harness.agents, |state| state.invocations.len() == 2).await?;
        abort_invocation(second).await?;
        harness.agents.cleanup_barrier().await?;
        {
            let state = harness.agents.state()?;
            assert_eq!(state.invocations.len(), 1);
            assert_eq!(state.waits[ROOT_SESSION][&session].len(), 1);
        }
        harness.tracker.permits.add_permits(1);
        tool_execution(first.await?)?;
        harness.close().await
    }

    #[tokio::test]
    async fn concurrent_follow_ups_retain_child_fifo_order() -> Result<()> {
        let harness = Harness::new().await?;
        let child = harness.spawn_child("NORMAL_FIFO").await?;
        let session = harness.child_session(child)?;
        let first = harness.prompt_call(ROOT_SESSION, child, "GATE_FIFO_FIRST");
        harness.tracker.wait_started(1).await?;
        let second = harness.prompt_call(ROOT_SESSION, child, "GATE_FIFO_SECOND");
        wait_for_registry(&harness.agents, |state| state.invocations.len() == 2).await?;

        harness.tracker.permits.add_permits(1);
        tool_execution(first.await?)?;
        harness.tracker.wait_started(2).await?;
        harness.tracker.permits.add_permits(1);
        tool_execution(second.await?)?;

        let inputs = harness.tracker.inputs_for(&session);
        let first = inputs
            .iter()
            .position(|input| input.contains("GATE_FIFO_FIRST"))
            .ok_or_else(|| eyre!("first FIFO prompt was not sent"))?;
        let second = inputs
            .iter()
            .position(|input| input.contains("GATE_FIFO_SECOND"))
            .ok_or_else(|| eyre!("second FIFO prompt was not sent"))?;
        assert!(first < second, "same-child follow-ups lost FIFO order");
        harness.close().await
    }

    #[tokio::test]
    async fn aborted_shutdown_waiter_after_admission_close_does_not_cancel_shutdown() -> Result<()>
    {
        let harness = Harness::new().await?;
        let (reached, release) = harness
            .agents
            .install_barrier(TestBarrierPoint::ShutdownAdmission);
        finish_shutdown_after_waiter_abort(&harness, reached, release).await?;
        harness.close().await
    }

    #[tokio::test]
    async fn aborted_shutdown_waiter_during_control_cancel_does_not_cancel_shutdown() -> Result<()>
    {
        let harness = Harness::new().await?;
        let child = harness.spawn_child("NORMAL_CONTROL_ABORT").await?;
        let invocation = harness.prompt_call(ROOT_SESSION, child, "BLOCK_CONTROL_ABORT");
        harness.tracker.wait_active(1).await?;
        let (reached, release) = harness
            .agents
            .install_barrier(TestBarrierPoint::ShutdownControl);
        finish_shutdown_after_waiter_abort(&harness, reached, release).await?;
        assert!(invocation.await?.is_err());
        harness.close().await
    }

    #[tokio::test]
    async fn aborted_shutdown_waiter_during_invocation_cleanup_does_not_cancel_shutdown()
    -> Result<()> {
        let harness = Harness::new().await?;
        let (cleanup_reached, cleanup_release) = harness
            .agents
            .install_barrier(TestBarrierPoint::CleanupInvocation);
        let (control_reached, _control_release) =
            harness.agents.install_barrier(TestBarrierPoint::Control);
        let invocation = harness.spawn_call("BLOCK_CLEANUP_ABORT");
        timeout(TEST_TIMEOUT, control_reached)
            .await
            .map_err(|_| eyre!("child invocation did not attach its control"))??;
        abort_invocation(invocation).await?;
        timeout(TEST_TIMEOUT, cleanup_reached)
            .await
            .map_err(|_| eyre!("cleanup worker did not start invocation cleanup"))??;
        let first = shutdown_call(&harness.agents);
        wait_for_registry(&harness.agents, |state| !state.admitting).await?;
        abort_shutdown_waiter(first).await?;
        let second = shutdown_call(&harness.agents);
        let _ = cleanup_release.send(());
        timeout(TEST_TIMEOUT, second)
            .await
            .map_err(|_| eyre!("replacement shutdown waiter timed out"))???;
        assert_shutdown_drained(&harness).await?;
        harness.close().await
    }

    #[tokio::test]
    async fn aborted_shutdown_waiter_during_session_drain_does_not_cancel_shutdown() -> Result<()> {
        let harness = Harness::new().await?;
        harness.spawn_child("NORMAL_SESSION_DRAIN_ABORT").await?;
        let (reached, release) = harness
            .agents
            .install_barrier(TestBarrierPoint::SessionDrain);
        finish_shutdown_after_waiter_abort(&harness, reached, release).await?;
        harness.close().await
    }

    #[tokio::test]
    async fn aborted_shutdown_waiter_before_worker_join_publication_does_not_cancel_shutdown()
    -> Result<()> {
        let harness = Harness::new().await?;
        let (reached, release) = harness.agents.install_barrier(TestBarrierPoint::WorkerJoin);
        finish_shutdown_after_waiter_abort(&harness, reached, release).await?;
        harness.close().await
    }

    #[tokio::test]
    async fn aborted_failed_initial_drain_caller_leaves_cleanup_owned() -> Result<()> {
        let harness = Harness::new().await?;
        let (reached, release) = harness
            .agents
            .install_barrier(TestBarrierPoint::SessionDrain);
        let invocation = harness.spawn_call("FAIL_INITIAL_ABORT_DRAIN");
        timeout(TEST_TIMEOUT, reached)
            .await
            .map_err(|_| eyre!("failed initial child did not enter session drain"))??;
        abort_invocation(invocation).await?;
        let _ = release.send(());
        timeout(TEST_TIMEOUT, harness.agents.shutdown())
            .await
            .map_err(|_| eyre!("shutdown did not await failed-initial drain"))??;
        assert_shutdown_drained(&harness).await?;
        harness.close().await
    }

    #[tokio::test]
    async fn aborted_admission_drain_caller_leaves_cleanup_owned() -> Result<()> {
        let harness = Harness::new().await?;
        let (created, release_creation) =
            harness.agents.install_barrier(TestBarrierPoint::Creation);
        let (draining, release_drain) = harness
            .agents
            .install_barrier(TestBarrierPoint::SessionDrain);
        let invocation = harness.spawn_call("NORMAL_ADMISSION_ABORT_DRAIN");
        timeout(TEST_TIMEOUT, created)
            .await
            .map_err(|_| eyre!("late child did not reach creation barrier"))??;
        let shutdown = shutdown_call(&harness.agents);
        wait_for_registry(&harness.agents, |state| !state.admitting).await?;
        let _ = release_creation.send(());
        timeout(TEST_TIMEOUT, draining)
            .await
            .map_err(|_| eyre!("rejected child did not enter session drain"))??;
        abort_invocation(invocation).await?;
        let _ = release_drain.send(());
        timeout(TEST_TIMEOUT, shutdown)
            .await
            .map_err(|_| eyre!("shutdown did not await rejected-child drain"))???;
        assert_shutdown_drained(&harness).await?;
        harness.close().await
    }

    #[tokio::test]
    async fn shutdown_worker_panic_is_cached_and_never_hangs_repeated_callers() -> Result<()> {
        let harness = Harness::new().await?;
        harness.agents.inject_shutdown_panic();
        let first = timeout(TEST_TIMEOUT, harness.agents.shutdown())
            .await
            .map_err(|_| eyre!("shutdown panic was not published"))?
            .expect_err("injected shutdown panic unexpectedly succeeded");
        let second = timeout(TEST_TIMEOUT, harness.agents.shutdown())
            .await
            .map_err(|_| eyre!("cached shutdown panic was not returned"))?
            .expect_err("cached shutdown panic unexpectedly succeeded");
        assert_eq!(first.kind(), second.kind());
        assert_eq!(first.to_string(), second.to_string());
        assert!(first.to_string().contains("shutdown worker failed"));
        {
            let state = harness.agents.state()?;
            assert!(state.shutdown_task.is_none());
            assert_eq!(state.cleanup_worker_joins, 1);
            assert_eq!(state.cleanup_worker_join_attempts, 1);
        }
        drop(harness.root);
        drop(harness.root_events);
        harness.tracker.wait_connections(0).await?;
        harness.server.stop().await
    }

    #[tokio::test]
    async fn cleanup_worker_panic_is_joined_once_and_cached_for_repeated_shutdown() -> Result<()> {
        let harness = Harness::new().await?;
        harness.agents.inject_cleanup_worker_panic();
        let first = timeout(TEST_TIMEOUT, harness.agents.shutdown())
            .await
            .map_err(|_| eyre!("cleanup-worker panic was not published"))?
            .expect_err("cleanup-worker panic unexpectedly succeeded");
        let second = timeout(TEST_TIMEOUT, harness.agents.shutdown())
            .await
            .map_err(|_| eyre!("cached cleanup-worker panic was not returned"))?
            .expect_err("cached cleanup-worker panic unexpectedly succeeded");
        assert_eq!(first.kind(), second.kind());
        assert_eq!(first.to_string(), second.to_string());
        assert!(first.to_string().contains("drain barrier"));
        assert!(first.to_string().contains("cleanup worker failed"));
        {
            let state = harness.agents.state()?;
            assert!(state.shutdown_task.is_none());
            assert_eq!(state.cleanup_worker_join_attempts, 1);
            assert_eq!(state.cleanup_worker_joins, 1);
        }
        assert!(
            harness
                .agents
                .cleanup_task
                .lock()
                .map_err(|_| eyre!("cleanup task mutex poisoned"))?
                .is_none()
        );
        drop(harness.root);
        drop(harness.root_events);
        harness.tracker.wait_connections(0).await?;
        harness.server.stop().await
    }

    #[tokio::test]
    async fn dead_cleanup_worker_waits_for_paused_creation_and_fallback_drain() -> Result<()> {
        let harness = Harness::new().await?;
        let (creation_reached, release_creation) =
            harness.agents.install_barrier(TestBarrierPoint::Creation);
        let creation = harness.spawn_call("NORMAL_DEAD_CLEANUP_WORKER");
        timeout(TEST_TIMEOUT, creation_reached)
            .await
            .map_err(|_| eyre!("child creation did not pause"))??;

        harness.agents.inject_cleanup_worker_panic();
        let barrier_error = harness
            .agents
            .cleanup_barrier()
            .await
            .expect_err("cleanup worker panic did not drop its barrier");
        assert!(
            barrier_error
                .to_string()
                .contains("dropped its drain barrier")
        );

        let (drain_reached, release_drain) = harness
            .agents
            .install_barrier(TestBarrierPoint::SessionDrain);
        let shutdown = shutdown_call(&harness.agents);
        wait_for_registry(&harness.agents, |state| !state.admitting).await?;
        {
            let state = harness.agents.state()?;
            assert_eq!(state.pending_creations, 1);
            assert!(state.invocations.is_empty());
            assert!(state.waits.is_empty());
        }
        let _ = release_creation.send(());
        timeout(TEST_TIMEOUT, drain_reached)
            .await
            .map_err(|_| eyre!("rejected child did not enter fallback drain"))??;
        assert!(
            !shutdown.is_finished(),
            "shutdown published before fallback drain completed"
        );
        {
            let state = harness.agents.state()?;
            assert_eq!(state.pending_creations, 1);
            assert_eq!(state.fallback_session_drains.len(), 1);
        }

        let _ = release_drain.send(());
        let rejected = timeout(TEST_TIMEOUT, creation)
            .await
            .map_err(|_| eyre!("rejected creation did not finish"))??;
        assert!(rejected.is_err());
        let first = timeout(TEST_TIMEOUT, shutdown)
            .await
            .map_err(|_| eyre!("shutdown hung after fallback drain"))??
            .expect_err("dead cleanup worker shutdown unexpectedly succeeded");
        let second = timeout(TEST_TIMEOUT, harness.agents.shutdown())
            .await
            .map_err(|_| eyre!("cached dead-worker shutdown result hung"))?
            .expect_err("cached dead-worker shutdown unexpectedly succeeded");
        assert_eq!(first.kind(), second.kind());
        assert_eq!(first.to_string(), second.to_string());
        assert!(first.to_string().contains("drain barrier"));
        assert!(first.to_string().contains("cleanup worker failed"));
        assert_shutdown_drained(&harness).await?;

        drop(harness.root);
        drop(harness.root_events);
        harness.tracker.wait_connections(0).await?;
        harness.server.stop().await
    }

    #[tokio::test]
    async fn dead_cleanup_worker_tracks_dropped_active_invocation_fallback() -> Result<()> {
        let harness = Harness::new().await?;
        let (control_reached, _release_control) =
            harness.agents.install_barrier(TestBarrierPoint::Control);
        let invocation = harness.spawn_call("BLOCK_ACTIVE_FALLBACK");
        timeout(TEST_TIMEOUT, control_reached)
            .await
            .map_err(|_| eyre!("active invocation did not attach its control"))??;

        harness.agents.inject_cleanup_worker_panic();
        let barrier_error = harness
            .agents
            .cleanup_barrier()
            .await
            .expect_err("cleanup worker panic did not drop its barrier");
        assert!(
            barrier_error
                .to_string()
                .contains("dropped its drain barrier")
        );
        let (cleanup_reached, release_cleanup) = harness
            .agents
            .install_barrier(TestBarrierPoint::CleanupInvocation);
        let shutdown = shutdown_call(&harness.agents);
        wait_for_registry(&harness.agents, |state| !state.admitting).await?;
        assert!(
            !shutdown.is_finished(),
            "shutdown published before the live invocation owner dropped"
        );
        abort_invocation(invocation).await?;
        timeout(TEST_TIMEOUT, cleanup_reached)
            .await
            .map_err(|_| eyre!("fallback invocation cleanup did not start"))??;

        assert!(
            !shutdown.is_finished(),
            "shutdown detached fallback invocation cleanup"
        );
        {
            let state = harness.agents.state()?;
            assert!(!state.fallback_session_drains.is_empty());
        }
        let _ = release_cleanup.send(());
        let first = timeout(TEST_TIMEOUT, shutdown)
            .await
            .map_err(|_| eyre!("shutdown hung on fallback invocation cleanup"))??
            .expect_err("dead cleanup worker shutdown unexpectedly succeeded");
        let second = timeout(TEST_TIMEOUT, harness.agents.shutdown())
            .await
            .map_err(|_| eyre!("cached fallback invocation shutdown result hung"))?
            .expect_err("cached fallback invocation shutdown unexpectedly succeeded");
        assert_eq!(first.kind(), second.kind());
        assert_eq!(first.to_string(), second.to_string());
        assert!(first.to_string().contains("drain barrier"));
        assert!(first.to_string().contains("cleanup worker failed"));
        assert_shutdown_drained(&harness).await?;

        drop(harness.root);
        drop(harness.root_events);
        harness.tracker.wait_connections(0).await?;
        harness.server.stop().await
    }

    #[tokio::test]
    async fn fallback_session_join_awaits_every_taken_handle_after_failure() -> Result<()> {
        let agents = Arc::new(ChildAgents::default());
        let completed = Arc::new(AtomicUsize::new(0));
        let completed_task = Arc::clone(&completed);
        let (started, started_rx) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let failed = tokio::spawn(async { panic!("injected fallback drain panic") });
        let delayed = tokio::spawn(async move {
            let _ = started.send(());
            let _ = release_rx.await;
            completed_task.fetch_add(1, Ordering::SeqCst);
        });
        {
            let mut state = agents.state()?;
            state.fallback_session_drains.extend([failed, delayed]);
        }
        let join = {
            let agents = Arc::clone(&agents);
            tokio::spawn(async move {
                let mut failures = Vec::new();
                agents.join_fallback_session_drains(&mut failures).await;
                failures
            })
        };
        timeout(TEST_TIMEOUT, started_rx)
            .await
            .map_err(|_| eyre!("delayed fallback drain did not start"))??;
        assert!(!join.is_finished(), "later fallback drain was detached");
        let _ = release.send(());
        let failures = timeout(TEST_TIMEOUT, join)
            .await
            .map_err(|_| eyre!("fallback drains did not all join"))??;
        assert_eq!(failures.len(), 1);
        assert_eq!(completed.load(Ordering::SeqCst), 1);
        agents.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn poisoned_cleanup_task_mutex_still_joins_worker_once() -> Result<()> {
        let agents = Arc::new(ChildAgents::default());
        let poisoned = Arc::clone(&agents);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _cleanup_task = poisoned
                .cleanup_task
                .lock()
                .expect("cleanup task mutex should initially be healthy");
            panic!("inject cleanup task mutex poison");
        }));
        let first = timeout(TEST_TIMEOUT, agents.shutdown())
            .await
            .map_err(|_| eyre!("poisoned cleanup task shutdown hung"))?
            .expect_err("poisoned cleanup task shutdown unexpectedly succeeded");
        let second = timeout(TEST_TIMEOUT, agents.shutdown())
            .await
            .map_err(|_| eyre!("cached poisoned cleanup task error hung"))?
            .expect_err("cached poisoned cleanup task error unexpectedly succeeded");
        assert_eq!(first.to_string(), second.to_string());
        assert!(first.to_string().contains("cleanup task mutex poisoned"));
        {
            let state = agents.state()?;
            assert_eq!(state.cleanup_worker_join_attempts, 1);
            assert_eq!(state.cleanup_worker_joins, 1);
            assert!(state.shutdown_task.is_none());
        }
        assert!(
            agents
                .cleanup_task
                .lock()
                .expect_err("cleanup task mutex poison was lost")
                .into_inner()
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_is_idempotent_rejects_late_insert_and_cancels_queued_turns() -> Result<()> {
        let harness = Harness::new().await?;
        let child = harness.spawn_child("NORMAL_SHUTDOWN").await?;
        let mut queued = Vec::new();
        queued.push(harness.prompt_call(ROOT_SESSION, child, "BLOCK_ACTIVE"));
        harness.tracker.wait_active(1).await?;
        for index in 0..3 {
            queued.push(harness.prompt_call(ROOT_SESSION, child, &format!("GATE_QUEUED_{index}")));
        }
        wait_for_registry(&harness.agents, |state| state.invocations.len() == 4).await?;
        let late_cleanup =
            harness
                .agents
                .reserve("late-cleanup-parent", "late-cleanup-target", None)?;

        let (created, release_creation) =
            harness.agents.install_barrier(TestBarrierPoint::Creation);
        let late = harness.spawn_call("NORMAL_LATE");
        timeout(TEST_TIMEOUT, created)
            .await
            .map_err(|_| eyre!("late child did not reach creation barrier"))??;
        let first_shutdown = {
            let agents = Arc::clone(&harness.agents);
            tokio::spawn(async move { agents.shutdown().await })
        };
        let second_shutdown = {
            let agents = Arc::clone(&harness.agents);
            tokio::spawn(async move { agents.shutdown().await })
        };
        wait_for_registry(&harness.agents, |state| !state.admitting).await?;
        assert!(
            !first_shutdown.is_finished(),
            "shutdown exited while an invocation guard was still outstanding"
        );
        drop(late_cleanup);
        let _ = release_creation.send(());
        timeout(TEST_TIMEOUT, async {
            first_shutdown.await??;
            second_shutdown.await??;
            Result::<()>::Ok(())
        })
        .await
        .map_err(|_| eyre!("concurrent shutdown timed out"))??;
        assert!(late.await?.is_err());
        for invocation in queued {
            assert!(invocation.await?.is_err());
        }
        harness.agents.shutdown().await?;
        harness.tracker.wait_active(0).await?;
        {
            let state = harness.agents.state()?;
            assert!(!state.admitting);
            assert!(state.agents.is_empty());
            assert!(state.invocations.is_empty());
        }
        assert!(
            harness
                .agents
                .cleanup_task
                .lock()
                .expect("cleanup task mutex should not be poisoned")
                .is_none()
        );
        harness.close().await
    }
}
