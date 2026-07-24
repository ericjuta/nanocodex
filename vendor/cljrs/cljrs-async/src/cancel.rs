//! Idempotent cancellation hooks for [`CljxFuture`] values.
//!
//! `cljrs-value`'s `CljxFuture` has no cancel-hook field, so hooks and abort
//! handles live in a cell-local side table keyed by future identity. The first
//! terminal state (done/failed/cancelled/gas) wins; later cancel attempts are
//! no-ops.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use cljrs_gc::GcPtr;
use cljrs_value::{CljxFuture, FutureState};
use tokio::task::JoinHandle;

type CancelHook = Rc<dyn Fn()>;

struct CancelEntry {
    hook: Option<CancelHook>,
    handle: Option<JoinHandle<()>>,
}

thread_local! {
    static CANCEL_TABLE: RefCell<HashMap<usize, CancelEntry>> = RefCell::new(HashMap::new());
}

fn future_key(future: &GcPtr<CljxFuture>) -> usize {
    future.get() as *const CljxFuture as usize
}

/// Register the local task handle that owns `future`.
pub(crate) fn register_task(future: GcPtr<CljxFuture>, handle: JoinHandle<()>) {
    CANCEL_TABLE.with(|table| {
        let mut table = table.borrow_mut();
        let entry = table.entry(future_key(&future)).or_insert(CancelEntry {
            hook: None,
            handle: None,
        });
        entry.handle = Some(handle);
    });
}

/// Attach an idempotent cancellation hook. Replaces any previous hook.
pub fn set_cancel_hook(future: &GcPtr<CljxFuture>, hook: impl Fn() + 'static) {
    let hook: CancelHook = Rc::new(hook);
    CANCEL_TABLE.with(|table| {
        let mut table = table.borrow_mut();
        let entry = table.entry(future_key(future)).or_insert(CancelEntry {
            hook: None,
            handle: None,
        });
        entry.hook = Some(hook);
    });
}

/// Attempt to cancel `future`. Returns true when this call wins the terminal
/// transition into [`FutureState::Cancelled`].
pub fn cancel_future(future: &GcPtr<CljxFuture>) -> bool {
    let key = future_key(future);
    let won = {
        let mut state = future
            .get()
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(*state, FutureState::Running) {
            *state = FutureState::Cancelled;
            true
        } else {
            false
        }
    };
    if won {
        future.get().cond.notify_all();
        let entry = CANCEL_TABLE.with(|table| table.borrow_mut().remove(&key));
        if let Some(entry) = entry {
            if let Some(hook) = entry.hook {
                hook();
            }
            if let Some(handle) = entry.handle {
                handle.abort();
            }
        }
    }
    won
}

/// Drop bookkeeping after a future settles normally so cancelled cells do not
/// retain hooks.
pub(crate) fn clear(future: &GcPtr<CljxFuture>) {
    CANCEL_TABLE.with(|table| {
        table.borrow_mut().remove(&future_key(future));
    });
}
