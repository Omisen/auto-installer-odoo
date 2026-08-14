//! [`NoopStep`]: a dummy step for testing the engine end to end.
//!
//! touches nothing. it can be made to fail in `snapshot`, `run` or `undo`, and
//! records its undo calls observably, so the reverse order, the best-effort
//! rule and "the undo acts only on `CreatedByUs`" are all checkable.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};

/// a shared log of the order in which undos actually act.
///
/// only undos that *act* append their name; a no-op one does not appear.
/// several steps can share one log to check their relative order.
pub type UndoLog = Arc<Mutex<Vec<String>>>;

/// a dummy step with no effect on the system.
pub struct NoopStep {
    name: String,
    prestate: PreState,
    fail_on_snapshot: bool,
    fail_on_run: bool,
    fail_on_undo: bool,
    /// how many times `undo` was *invoked*, no-ops included.
    undo_calls: Arc<AtomicUsize>,
    /// shared log of the undo actions actually performed.
    undo_log: Option<UndoLog>,
    /// an arbitrary action run **inside** `run`.
    ///
    /// models something happening *while* a step is in flight — the real case
    /// being a signal arriving mid-installation (B-V3-5). without this hook one
    /// could only raise the flag before or after, i.e. test everything except
    /// the moment that matters.
    #[allow(clippy::type_complexity)]
    on_run: Option<Box<dyn Fn() + Send + Sync>>,
}

impl NoopStep {
    /// a step that behaves as `CreatedByUs` and never fails.
    pub fn new(name: impl Into<String>) -> Self {
        NoopStep {
            name: name.into(),
            prestate: PreState::CreatedByUs,
            fail_on_snapshot: false,
            fail_on_run: false,
            fail_on_undo: false,
            undo_calls: Arc::new(AtomicUsize::new(0)),
            undo_log: None,
            on_run: None,
        }
    }

    /// sets the simulated `PreState` the snapshot will record.
    pub fn with_prestate(mut self, prestate: PreState) -> Self {
        self.prestate = prestate;
        self
    }

    /// marks the step `Preexisting`, making its undo a no-op.
    pub fn preexisting(self) -> Self {
        self.with_prestate(PreState::Preexisting)
    }

    /// makes `snapshot` fail, to test the rollback of the previous steps.
    pub fn fail_on_snapshot(mut self) -> Self {
        self.fail_on_snapshot = true;
        self
    }

    /// makes `run` fail, to trigger the rollback.
    pub fn fail_on_run(mut self) -> Self {
        self.fail_on_run = true;
        self
    }

    /// makes `undo` fail, to test the best-effort behaviour.
    pub fn fail_on_undo(mut self) -> Self {
        self.fail_on_undo = true;
        self
    }

    /// runs `f` inside `run`, before the step declares itself complete.
    pub fn on_run(mut self, f: impl Fn() + Send + Sync + 'static) -> Self {
        self.on_run = Some(Box::new(f));
        self
    }

    /// attaches a shared log to record undo actions on.
    pub fn with_undo_log(mut self, log: UndoLog) -> Self {
        self.undo_log = Some(log);
        self
    }

    /// a shared handle to the invocation counter, readable after the step has
    /// been moved into the engine's `Vec`.
    pub fn undo_call_handle(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.undo_calls)
    }
}

impl Step for NoopStep {
    fn name(&self) -> &str {
        &self.name
    }

    fn snapshot(&mut self, _ctx: &Context) -> Result<(), StepError> {
        if self.fail_on_snapshot {
            return Err(StepError::SnapshotFailed {
                step: self.name.clone(),
                reason: "simulated snapshot failure".to_string(),
            });
        }
        // a real step would detect the state here; this one is preconfigured.
        info!(step = %self.name, prestate = ?self.prestate, "snapshot recorded");
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if ctx.dry_run {
            info!(step = %self.name, "run (dry run, nothing changed)");
            return Ok(());
        }
        if let Some(f) = &self.on_run {
            f();
        }
        if self.fail_on_run {
            return Err(StepError::CommandFailed {
                command: format!("noop:{}", self.name),
                status: "1".to_string(),
                stderr: "simulated run failure".to_string(),
            });
        }
        info!(step = %self.name, "run executed");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        // every invocation counts, no-ops included.
        self.undo_calls.fetch_add(1, Ordering::SeqCst);

        // invariant 3: the undo acts ONLY on artifacts we created.
        if self.prestate != PreState::CreatedByUs {
            info!(
                step = %self.name,
                prestate = ?self.prestate,
                "undo NO-OP (not CreatedByUs)"
            );
            return Ok(());
        }

        // from here it is a real undo action: record it.
        if let Some(log) = &self.undo_log {
            if let Ok(mut entries) = log.lock() {
                entries.push(self.name.clone());
            }
        }

        if ctx.dry_run {
            info!(step = %self.name, "undo (dry run, nothing changed)");
            return Ok(());
        }

        if self.fail_on_undo {
            warn!(step = %self.name, "undo: simulated failure");
            return Err(StepError::Precondition(format!(
                "simulated undo failure for {}",
                self.name
            )));
        }

        info!(step = %self.name, "undo executed");
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.prestate).unwrap_or(serde_json::Value::Null)
    }

    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let prestate = decode_snapshot(self.name(), snapshot)?;
        self.prestate = prestate;
        Ok(())
    }
}
