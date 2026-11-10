//! Complete execution VM, charged to the epoch of its top-level STMT.
//!
//! Scope: this private fixture and the locked SQLx 0.8.6 serial worker protocol.
//! ExecuteIter resets on completion/error/drop before the next command or native
//! lock. No user extension runs reentrant SQL or changes trace/progress/counters.
//! PROFILE is a cumulative checkpoint, including intermediate schema retries;
//! it is never interpreted as the final callback of a logical execution.
use libsqlite3_sys::*;
use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, c_int, c_uint, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Work {
    pub vm: u64,
    raw_ticks: u64,
    starts: u64,
}

struct Execution {
    // An identity only. Never dereference this address: finalize can free it.
    stmt: usize,
    epoch: usize,
    settled: u64,
    raw_checkpoint: u64,
    profiled: bool,
}

struct Handle {
    context: usize,
    live: Arc<AtomicBool>,
    raw: u64,
    execution: Option<Execution>,
}

#[derive(Default)]
struct State {
    epoch: usize,
    windows: HashSet<usize>,
    work: Vec<Work>,
    total: u64,
    untraced_ticks: u64,
    handles: HashMap<usize, Handle>,
    installed: HashSet<usize>,
    removed: HashSet<usize>,
}

#[derive(Default)]
pub(super) struct Meter {
    state: Mutex<State>,
    failed: AtomicBool,
}

// SQLite owns this Box until removal under lock_handle, or CLOSE.
struct Context {
    meter: Arc<Meter>,
    live: Arc<AtomicBool>,
    conn: usize,
}

impl Meter {
    fn update(&self, f: impl FnOnce(&mut State) -> Option<()>) {
        if catch_unwind(AssertUnwindSafe(|| match self.state.lock() {
            Ok(mut state) => {
                if f(&mut state).is_none() {
                    self.failed.store(true, Ordering::SeqCst);
                }
            }
            Err(_) => self.failed.store(true, Ordering::SeqCst),
        }))
        .is_err()
        {
            self.failed.store(true, Ordering::SeqCst);
        }
    }

    fn cleanup_state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poisoned| {
            self.failed.store(true, Ordering::SeqCst);
            poisoned.into_inner()
        })
    }

    pub fn open_window(&self) -> usize {
        let mut state = self.state.lock().unwrap();
        assert!(!state.windows.contains(&state.epoch), "nested SQL window");
        state.epoch = state.epoch.checked_add(1).unwrap();
        let epoch = state.epoch;
        state.work.resize(epoch + 1, Work::default());
        state.windows.insert(epoch);
        epoch
    }

    pub fn close_window(&self, epoch: usize) {
        let mut state = self.state.lock().unwrap();
        assert_eq!(state.epoch, epoch, "SQL window closed out of order");
        state.epoch = state.epoch.checked_add(1).unwrap();
        let len = state.epoch + 1;
        state.work.resize(len, Work::default());
    }

    pub fn assert_installed(&self, capacity: usize) {
        assert_eq!(self.state.lock().unwrap().handles.len(), capacity);
    }

    pub fn finish(&self) -> Vec<Work> {
        let state = self.state.lock().unwrap();
        assert!(
            !self.failed.load(Ordering::SeqCst),
            "SQL measurement incomplete"
        );
        assert!(
            state.handles.is_empty(),
            "SQL instrumentation still installed"
        );
        assert_eq!(
            state.installed, state.removed,
            "application connection missed"
        );
        assert_eq!(
            state
                .work
                .iter()
                .try_fold(0_u64, |sum, w| sum.checked_add(w.vm)),
            Some(state.total)
        );
        for epoch in &state.windows {
            assert!(
                state.work[*epoch].starts > 0 && state.work[*epoch].vm > 0,
                "SQL window has no measured execution"
            );
        }
        eprintln!(
            "title-sql epochs={:?} untraced-prepare-ticks={}",
            state.work, state.untraced_ticks
        );
        state.work.clone()
    }

    pub async fn install(
        self: &Arc<Self>,
        connection: &mut sqlx::SqliteConnection,
    ) -> sqlx::Result<()> {
        let mut handle = connection.lock_handle().await?;
        let raw = handle.as_raw_handle().as_ptr();
        let conn = raw as usize;
        let live = Arc::new(AtomicBool::new(true));
        let context = Box::new(Context {
            meter: self.clone(),
            live: live.clone(),
            conn,
        });
        {
            let mut state = self.state.lock().unwrap();
            assert!(
                state.installed.insert(conn),
                "duplicate native installation"
            );
            if state.work.is_empty() {
                state.work.push(Work::default());
            }
        }
        // No fallible assertion after transferring context ownership to SQLite.
        let context = Box::into_raw(context);
        self.cleanup_state().handles.insert(
            conn,
            Handle {
                context: context as usize,
                live,
                raw: 0,
                execution: None,
            },
        );
        let status = unsafe {
            sqlite3_trace_v2(
                raw,
                (SQLITE_TRACE_STMT | SQLITE_TRACE_PROFILE | SQLITE_TRACE_CLOSE) as c_uint,
                Some(trace),
                context.cast(),
            )
        };
        if status != SQLITE_OK {
            self.failed.store(true, Ordering::SeqCst);
            self.cleanup_state().handles.remove(&conn);
            drop(unsafe { Box::from_raw(context) });
            return Err(sqlx::Error::Protocol("cannot install SQL trace".into()));
        }
        let meter = self.clone();
        handle.set_progress_handler(100, move || {
            meter.update(|state| {
                let h = state.handles.get_mut(&conn)?;
                h.raw = h.raw.checked_add(1)?;
                let work = state.work.get_mut(state.epoch)?;
                work.raw_ticks = work.raw_ticks.checked_add(1)?;
                Some(())
            });
            true
        });
        Ok(())
    }

    pub async fn remove(&self, connection: &mut sqlx::SqliteConnection) -> sqlx::Result<()> {
        let mut handle = connection.lock_handle().await?;
        let raw = handle.as_raw_handle().as_ptr();
        // The worker is parked after prior ExecuteIter drops. Enumerate only
        // live statements from SQLite; retained identities may already be freed.
        let mut stmt = unsafe { sqlite3_next_stmt(raw, std::ptr::null_mut()) };
        while !stmt.is_null() {
            if unsafe { sqlite3_stmt_busy(stmt) } != 0 {
                self.failed.store(true, Ordering::SeqCst);
            }
            stmt = unsafe { sqlite3_next_stmt(raw, stmt) };
        }
        handle.remove_progress_handler();
        let status = unsafe { sqlite3_trace_v2(raw, 0, None, std::ptr::null_mut()) };
        if status != SQLITE_OK {
            self.failed.store(true, Ordering::SeqCst);
            return Err(sqlx::Error::Protocol("cannot remove SQL trace".into()));
        }
        let conn = raw as usize;
        let entry = self
            .cleanup_state()
            .handles
            .remove(&conn)
            .expect("uninstrumented replacement connection");
        if !entry.live.swap(false, Ordering::SeqCst) {
            self.failed.store(true, Ordering::SeqCst);
            return Err(sqlx::Error::Protocol("SQL trace already closed".into()));
        }
        self.update(|state| {
            let checkpoint = if let Some(execution) = entry.execution {
                if !execution.profiled {
                    return None;
                }
                execution.raw_checkpoint
            } else {
                0
            };
            state.untraced_ticks = state
                .untraced_ticks
                .checked_add(entry.raw.checked_sub(checkpoint)?)?;
            state.removed.insert(conn);
            Some(())
        });
        drop(unsafe { Box::from_raw(entry.context as *mut Context) });
        Ok(())
    }
}

unsafe extern "C" fn trace(
    mask: c_uint,
    context: *mut c_void,
    stmt: *mut c_void,
    text: *mut c_void,
) -> c_int {
    if mask == SQLITE_TRACE_CLOSE as c_uint {
        let context = unsafe { Box::from_raw(context.cast::<Context>()) };
        context.live.store(false, Ordering::SeqCst);
        context.meter.failed.store(true, Ordering::SeqCst);
        context.meter.update(|state| {
            state.handles.remove(&context.conn)?;
            Some(())
        });
        return 0;
    }
    let context = unsafe { &*context.cast::<Context>() };
    context.meter.update(|state| {
        let handle = state.handles.get_mut(&context.conn)?;
        if mask == SQLITE_TRACE_STMT as c_uint {
            // Only the actual callback-supplied statement is read. SQLx has no
            // reentrant SQL extensions here. Trigger comments belong to the
            // same execution; even SQL starting with a comment is a top level
            // start when the trace text equals sqlite3_sql's original text.
            let sql = unsafe { sqlite3_sql(stmt.cast()) };
            if sql.is_null() || text.is_null() {
                return None;
            }
            let text = unsafe { CStr::from_ptr(text.cast()) }.to_bytes();
            if text != unsafe { CStr::from_ptr(sql) }.to_bytes() {
                let execution = handle.execution.as_ref()?;
                return (text.starts_with(b"-- ") && execution.stmt == stmt as usize).then_some(());
            }
            let checkpoint = if let Some(old) = &handle.execution {
                if !old.profiled {
                    return None;
                }
                old.raw_checkpoint
            } else {
                0
            };
            state.untraced_ticks = state
                .untraced_ticks
                .checked_add(handle.raw.checked_sub(checkpoint)?)?;
            // Start a fresh public counter history. Its returned prior value is
            // deliberately unused; a cached statement may have an older history.
            unsafe { sqlite3_stmt_status(stmt.cast(), SQLITE_STMTSTATUS_VM_STEP, 1) };
            handle.execution = Some(Execution {
                stmt: stmt as usize,
                epoch: state.epoch,
                settled: 0,
                raw_checkpoint: handle.raw,
                profiled: false,
            });
            let work = state.work.get_mut(state.epoch)?;
            work.starts = work.starts.checked_add(1)?;
        } else if mask == SQLITE_TRACE_PROFILE as c_uint {
            let execution = handle.execution.as_mut()?;
            if execution.stmt != stmt as usize {
                return None;
            }
            // Locked SQLite uses u64 nVmStep and invokes progress at least
            // every 100 ops, including vdbe_return. Preparation may add ticks.
            // Bound the entire cumulative history BEFORE reading VM_STEP:
            // its public value is undefined above INT_MAX. This rejects any
            // number of wraps rather than accepting a small wrapped residue.
            let upper = handle
                .raw
                .checked_sub(execution.raw_checkpoint)?
                .checked_add(2)?
                .checked_mul(100)?;
            if execution.settled.checked_add(upper)? > i32::MAX as u64 {
                return None;
            }
            let vm = unsafe { sqlite3_stmt_status(stmt.cast(), SQLITE_STMTSTATUS_VM_STEP, 0) };
            let vm = u64::try_from(vm).ok()?;
            let delta = vm.checked_sub(execution.settled)?;
            if delta > upper {
                return None;
            }
            let work = state.work.get_mut(execution.epoch)?;
            work.vm = work.vm.checked_add(delta)?;
            state.total = state.total.checked_add(delta)?;
            execution.settled = vm;
            execution.raw_checkpoint = handle.raw;
            execution.profiled = true;
        } else {
            return None;
        }
        Some(())
    });
    0
}

#[cfg(test)]
mod tests;
