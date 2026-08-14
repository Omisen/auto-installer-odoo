//! the [`Step`] trait: the contract every installer step implements.
//!
//! [`crate::engine::Installer`] orchestrates it without knowing any step's
//! details. it **must not change** to accommodate a new step: if a real case
//! does not fit, raise it instead of widening the trait.

use serde::de::DeserializeOwned;

use crate::context::Context;
use crate::error::StepError;

/// one reversible installation step.
///
/// # contract
///
/// the four invariants of `CLAUDE.md`:
///
/// 1. **`snapshot` always before `run`.** the step detects and records its own
///    `PreState` (`Preexisting` vs `CreatedByUs`) there: the only source of
///    truth for the undo.
/// 2. **undo in reverse order**, from the last completed step to the first.
/// 3. **undo idempotent and best-effort.** it must not fail when the artifact
///    is already gone, and must act **only** on `PreState == CreatedByUs`; on
///    `Preexisting` it is a no-op. a failing undo does not block the other
///    steps' cleanup — the engine logs and carries on.
/// 4. **state persisted.** after a successful `run` the engine persists the
///    step's record (name + [`Step::snapshot_value`]). `invok rollback` reads
///    it back, rebuilds the steps with [`Step::rehydrate`] and runs their
///    undos in reverse order (see [`crate::rollback`]).
///
/// # dry run
///
/// when [`Context::dry_run`] is set, `run` and `undo` must not mutate: they log
/// what they *would* have done.
pub trait Step {
    /// stable, unique name, used in the logs and in the persisted state.
    fn name(&self) -> &str;

    /// records the pre-existing state. called before [`Step::run`].
    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError>;

    /// performs the mutation, honouring `dry_run`.
    fn run(&mut self, ctx: &Context) -> Result<(), StepError>;

    /// undoes the mutation: best-effort, idempotent, active only on `PreState
    /// == CreatedByUs`.
    ///
    /// takes `&self`; test counters rely on interior mutability.
    fn undo(&self, ctx: &Context) -> Result<(), StepError>;

    /// serialisable snapshot, persisted with the step's record.
    ///
    /// the value is opaque to the engine. the default suits stateless steps;
    /// real steps serialise their `PreState` here.
    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::Value::Null
    }

    /// reloads the internal state from a persisted [`Step::snapshot_value`],
    /// **without** re-inspecting the system.
    ///
    /// R4. re-running `snapshot` would photograph the system *after* our
    /// mutations, so an artifact we created would read as `Preexisting` — or,
    /// worse, a customer's database would read as ours and be dropped.
    ///
    /// must be the **exact inverse** of [`Step::snapshot_value`]:
    /// `rehydrate(snapshot_value(s))` must yield an undo indistinguishable from
    /// `s`'s. `tests/rehydrate.rs` checks it step by step.
    ///
    /// # errors
    ///
    /// [`StepError::SnapshotFailed`] when the snapshot cannot be decoded; the
    /// caller then skips that undo, rather than undoing on invented state.
    ///
    /// the default no-op suits steps whose undo consults no internal state
    /// (`initialize-odoo-database`, `install-python-requirements`).
    fn rehydrate(&mut self, _snapshot: &serde_json::Value) -> Result<(), StepError> {
        Ok(())
    }
}

/// deserialises a persisted snapshot into a step's internal type.
///
/// # errors
///
/// turns a format error into [`StepError::SnapshotFailed`], naming `step`.
///
/// shared by every [`Step::rehydrate`] implementation: the reason rehydration
/// is one line per step rather than repeated error handling.
pub fn decode_snapshot<T: DeserializeOwned>(
    step: &str,
    snapshot: &serde_json::Value,
) -> Result<T, StepError> {
    serde_json::from_value(snapshot.clone()).map_err(|e| StepError::SnapshotFailed {
        step: step.to_string(),
        reason: format!("snapshot persistito non deserializzabile: {e}"),
    })
}
