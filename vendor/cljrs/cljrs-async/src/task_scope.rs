//! Cell-scoped ownership for local async tasks spawned by Clojure evaluation.

use std::cell::RefCell;
use std::rc::Rc;

use cljrs_gc::GcPtr;
use cljrs_value::{CljxFuture, FutureState};
use tokio::task::JoinHandle;

thread_local! {
    static ACTIVE_SCOPES: RefCell<Vec<Rc<ScopeInner>>> = const { RefCell::new(Vec::new()) };
}

struct ScopedTask {
    future: GcPtr<CljxFuture>,
    handle: JoinHandle<()>,
}

#[derive(Default)]
struct ScopeInner {
    tasks: RefCell<Vec<ScopedTask>>,
}

/// Owns every local task spawned while a Code Mode cell is evaluating.
///
/// Cancelling or finishing a cell settles all still-running Clojure futures and
/// joins their local tasks before the next cell may reuse the isolate.
#[derive(Clone, Default)]
pub struct FutureTaskScope {
    inner: Rc<ScopeInner>,
}

impl FutureTaskScope {
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use = "dropping the guard immediately stops assigning tasks to this scope"]
    pub fn install(&self) -> FutureTaskScopeGuard {
        ACTIVE_SCOPES.with(|scopes| scopes.borrow_mut().push(self.inner.clone()));
        FutureTaskScopeGuard
    }

    pub fn cancel_all(&self) {
        for task in self.inner.tasks.borrow().iter() {
            {
                let mut state = task
                    .future
                    .get()
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if matches!(*state, FutureState::Running) {
                    *state = FutureState::Cancelled;
                }
            }
            task.future.get().cond.notify_all();
            task.handle.abort();
        }
    }

    pub async fn join_all(&self) {
        let handles = self
            .inner
            .tasks
            .borrow_mut()
            .drain(..)
            .map(|task| task.handle)
            .collect::<Vec<_>>();
        for handle in handles {
            let _ = handle.await;
        }
    }
}

pub struct FutureTaskScopeGuard;

impl Drop for FutureTaskScopeGuard {
    fn drop(&mut self) {
        ACTIVE_SCOPES.with(|scopes| {
            scopes.borrow_mut().pop();
        });
    }
}

pub(crate) fn register(future: GcPtr<CljxFuture>, handle: JoinHandle<()>) {
    ACTIVE_SCOPES.with(|scopes| {
        let Some(scope) = scopes.borrow().last().cloned() else {
            return;
        };
        scope.tasks.borrow_mut().push(ScopedTask { future, handle });
    });
}
