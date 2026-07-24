//! Cell-scoped ownership for local async tasks spawned by Clojure evaluation.

use std::cell::RefCell;
use std::rc::Rc;

use cljrs_gc::GcPtr;
use cljrs_value::CljxFuture;

use crate::cancel;

thread_local! {
    static ACTIVE_SCOPES: RefCell<Vec<Rc<ScopeInner>>> = const { RefCell::new(Vec::new()) };
}

#[derive(Default)]
struct ScopeInner {
    futures: RefCell<Vec<GcPtr<CljxFuture>>>,
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
        let futures = self.inner.futures.borrow().clone();
        for future in futures {
            cancel::cancel_future(&future);
        }
    }

    pub async fn join_all(&self) {
        // Cancellation aborts JoinHandles; give the LocalSet a chance to drop
        // aborted tasks before the next cell starts.
        tokio::task::yield_now().await;
        self.inner.futures.borrow_mut().clear();
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

pub(crate) fn register(future: GcPtr<CljxFuture>) {
    ACTIVE_SCOPES.with(|scopes| {
        let Some(scope) = scopes.borrow().last().cloned() else {
            return;
        };
        scope.futures.borrow_mut().push(future);
    });
}
