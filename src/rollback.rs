//! rollback **from persisted state**: the consumer of `InstallState` (R4).
//!
//! the engine undoes what it has just done, from steps still in memory. this
//! module covers the two cases it cannot reach: a process that never gets to
//! its error handling (Ctrl-C, `kill -9`, OOM, power loss), and uninstalling a
//! **successful** installation, which is a legitimate request rather than a
//! failure.
//!
//! for each persisted record it rebuilds the step
//! ([`crate::steps::step_by_name`]), puts the original snapshot back in
//! ([`crate::step::Step::rehydrate`]) and runs its undo — reverse order,
//! best-effort, same protections.
//!
//! it deliberately never calls `snapshot`: that would photograph the system
//! **after** our mutations, so the database we created would read as
//! `Preexisting` and survive, while a customer `.conf` we had backed up would
//! read as ours.

use std::path::PathBuf;

use tracing::{info, warn};

use crate::context::Context;
use crate::progress::ProgressReporter;
use crate::state::InstallState;
use crate::steps::{self, OpsFactory};

/// outcome of one step's undo during a rollback from disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoOutcome {
    /// the undo ran without error, possibly as a legitimate no-op when the
    /// `PreState` was not `CreatedByUs`.
    Undone,
    /// the undo failed. best-effort, so the rollback carries on — but what it
    /// did not remove **stays**, and must be listed to the user (A1.3).
    Failed(String),
    /// unrecognised step name: the state comes from a version with steps this
    /// binary does not know.
    Unknown,
    /// the persisted snapshot does not deserialise into the step's type. the
    /// undo is **not** run: acting on invented state could drop the very thing
    /// the snapshot was protecting.
    NotRehydrated(String),
    /// the step owns artifacts shared with the instances named here, so what is
    /// shared was left in place (phase I2).
    ///
    /// a **residue** like the others, and for the same reason: something this
    /// manifest describes is still on the machine. that is what keeps the
    /// manifest — and its record of *who* owns the shared artifacts — alive
    /// until the last instance goes.
    LeftShared(Vec<String>),
}

impl UndoOutcome {
    /// `true` when this outcome leaves something to clean up by hand.
    pub fn is_residue(&self) -> bool {
        !matches!(self, UndoOutcome::Undone)
    }
}

/// a step's undo outcome, with its name for the final report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepOutcome {
    pub name: String,
    pub outcome: UndoOutcome,
}

/// end-of-rollback report (A1.3, B2).
///
/// a best-effort rollback *can* leave leftovers — the price of invariant 3.
/// collecting them here gives the user the exact list of what is left to
/// remove, instead of a `warn!` that scrolls away.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RollbackReport {
    /// outcomes in the order the undos ran, i.e. reversed.
    pub outcomes: Vec<StepOutcome>,
    /// does the installation home **still exist** once the rollback is over?
    ///
    /// A-MD-2: the outcomes and the promise are two different questions, and on
    /// a real run the second one lied — every undo succeeded, including
    /// `PrepareOptRoot`'s, which correctly gives up on a non-empty directory
    /// and returns `Ok`, while `/opt/odoo` was still there. so we check the
    /// **promise**, not the mechanism.
    ///
    /// deliberately not part of [`Self::is_clean`]: that decides whether the
    /// manifest can be consumed, and a manifest describing no artifact must go
    /// anyway (R19).
    pub home_left_behind: Option<PathBuf>,
}

impl RollbackReport {
    /// the steps that left something behind.
    pub fn residue(&self) -> Vec<&StepOutcome> {
        self.outcomes
            .iter()
            .filter(|o| o.outcome.is_residue())
            .collect()
    }

    /// `true` when every undo succeeded, so the state describes nothing live
    /// and can be removed.
    pub fn is_clean(&self) -> bool {
        self.residue().is_empty()
    }

    /// is there anything to tell the user beyond the undo count?
    pub fn has_anything_to_report(&self) -> bool {
        !self.is_clean() || self.home_left_behind.is_some()
    }

    /// how many steps were actually undone.
    pub fn undone(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|o| o.outcome == UndoOutcome::Undone)
            .count()
    }
}

/// the shape the state file found the installation in.
///
/// uninstalling a working instance and cleaning up after an interrupted run
/// have very different consequences, and deserve different wording before the
/// confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallStatus {
    /// every canonical step is recorded as completed.
    Complete { steps: usize },
    /// the installation stopped short (Ctrl-C, crash, error).
    Interrupted { done: usize, total: usize },
}

/// classifies the persisted state.
///
/// [`InstallState::finished`] is the primary source; comparing against the
/// canonical sequence is only a fallback for states written before that flag
/// existed, and stays a heuristic because the sequence changes between
/// versions.
///
/// `total` is passed in rather than derived here, which keeps this function
/// **pure** — checkable without building every step, or naming a family just to
/// count names.
pub fn install_status(state: &InstallState, total: usize) -> InstallStatus {
    let done = state.completed.len();
    if state.finished || done >= total {
        InstallStatus::Complete { steps: done }
    } else {
        InstallStatus::Interrupted { done, total }
    }
}

/// what to do before running the rollback, given how it was invoked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmationGate {
    /// proceed without asking: `--yes`, or `--dry-run`, which mutates nothing.
    Proceed,
    /// there is a terminal: ask for explicit confirmation.
    Ask,
    /// no terminal and no `--yes`: refuse. a destructive operation must not run
    /// by default inside a script that did not ask for it.
    RefuseNonInteractive,
}

/// the rollback's confirmation policy, **pure** so it can be checked without a
/// terminal.
///
/// a dry run asks nothing because it only lists. the case that matters is the
/// last: without a TTY, `--yes` becomes mandatory.
pub fn confirmation_gate(dry_run: bool, yes: bool, interactive: bool) -> ConfirmationGate {
    if dry_run || yes {
        ConfirmationGate::Proceed
    } else if interactive {
        ConfirmationGate::Ask
    } else {
        ConfirmationGate::RefuseNonInteractive
    }
}

/// the step names in the order they will be undone, i.e. reversed.
///
/// pure: feeds the summary shown before the confirmation and the `--dry-run`,
/// without touching the system.
pub fn undo_plan(state: &InstallState) -> Vec<&str> {
    state
        .completed
        .iter()
        .rev()
        .map(|r| r.name.as_str())
        .collect()
}

/// undoes the steps described by `state`, in **reverse order** (invariant 2)
/// and **best-effort** (invariant 3).
///
/// `ctx` must come from the persisted configuration
/// ([`crate::state::InstallConfig::to_context`]): that is what gives the undos
/// the real names of the artifacts created. under `dry_run` no undo mutates.
///
/// returns no `Result`: a rollback does not "fail", it accumulates outcomes.
/// whatever could not be cleaned up ends in the [`RollbackReport`].
pub fn rollback_from_state(
    state: &InstallState,
    ctx: &Context,
    make_ops: OpsFactory<'_>,
    reporter: &dyn ProgressReporter,
) -> RollbackReport {
    rollback_from_state_sharing_with(state, ctx, make_ops, reporter, &[])
}

/// [`rollback_from_state`], told which **other instances** are still installed.
///
/// with `others` empty this is the historical behaviour exactly. otherwise the
/// steps whose undo reaches shared artifacts are handled by
/// [`crate::steps::artifact_scope`]:
///
/// - `Shared`: the undo is **not run at all**, and the outcome is
///   [`UndoOutcome::LeftShared`];
/// - `Mixed`: the undo *is* run — it does the instance's own half and reads
///   `Context::shared_in_use` to leave the rest — and the outcome is still
///   `LeftShared`, because part of what the record describes is still there;
/// - `OwnInstance`: nothing changes.
///
/// the caller must set `Context::shared_in_use` to match; the two are separate
/// because the driver decides *whether to call*, and the step decides *what to
/// skip inside*. only the mixed ones need both.
pub fn rollback_from_state_sharing_with(
    state: &InstallState,
    ctx: &Context,
    make_ops: OpsFactory<'_>,
    reporter: &dyn ProgressReporter,
    others: &[String],
) -> RollbackReport {
    let mut report = RollbackReport::default();
    if state.completed.is_empty() {
        info!("rollback: no step to undo");
        return report;
    }

    warn!(
        steps = state.completed.len(),
        dry_run = ctx.dry_run,
        "rollback from the persisted state (reverse order)"
    );
    reporter.rollback_start(state.completed.len());

    for record in state.completed.iter().rev() {
        let name = record.name.clone();
        reporter.undo_start(&name);

        let Some(mut step) = steps::step_by_name(&name, make_ops) else {
            warn!(
                step = %name,
                "rollback: step unknown to this binary, cannot be undone (proceeding)"
            );
            report.outcomes.push(StepOutcome {
                name: name.clone(),
                outcome: UndoOutcome::Unknown,
            });
            // an un-undoable step is still an **examined** one, so the bar must
            // advance (A-V3-10): otherwise progress froze in exactly the
            // degraded scenario where the user is watching it.
            reporter.undo_done(&name);
            continue;
        };

        if let Err(e) = step.rehydrate(&record.snapshot) {
            warn!(
                step = %name,
                error = %e,
                "rollback: persisted snapshot unreadable, undo skipped for safety (proceeding)"
            );
            report.outcomes.push(StepOutcome {
                name: name.clone(),
                outcome: UndoOutcome::NotRehydrated(e.to_string()),
            });
            reporter.undo_done(&name);
            continue;
        }

        // the shared-artifact rule (phase I2). `unnamed` decides two of the
        // scopes, and it is read from the manifest's configuration like
        // everything else the undos act on.
        let scope = steps::artifact_scope(&name, ctx.instance.is_none());
        if !others.is_empty() && scope == steps::ArtifactScope::Shared {
            info!(
                step = %name,
                in_use_by = ?others,
                "undo NOT run: it removes artifacts other instances are still using"
            );
            report.outcomes.push(StepOutcome {
                name: name.clone(),
                outcome: UndoOutcome::LeftShared(others.to_vec()),
            });
            reporter.undo_done(&name);
            continue;
        }

        info!(step = %name, "undo (from the persisted state)");
        let outcome = match step.undo(ctx) {
            // a mixed step did its own half and left the shared one: what the
            // record describes is still partly there, so it is a residue.
            Ok(()) if !others.is_empty() && scope == steps::ArtifactScope::Mixed => {
                UndoOutcome::LeftShared(others.to_vec())
            }
            Ok(()) => UndoOutcome::Undone,
            Err(e) => {
                warn!(
                    step = %name,
                    error = %e,
                    "undo failed, continuing the cleanup (best-effort)"
                );
                UndoOutcome::Failed(e.to_string())
            }
        };
        reporter.undo_done(&name);
        report.outcomes.push(StepOutcome { name, outcome });
    }

    // the **promise**, not the mechanism: is `/opt/odoo` still there? a read,
    // not a mutation, and this is the only point that sees the finished state.
    //
    // skipped in dry-run, where the directory is still there by construction
    // and the warning would be a guaranteed false alarm.
    //
    // skipped with other instances installed for the same reason (`A-V6-11`,
    // found in the field): there the shared root is one of the artifacts we
    // deliberately kept, so its presence is the rule working, not a residue.
    // reporting it anyway printed two sentences that were plainly false — "it
    // holds something we did not create" (it holds the *other instance*, which
    // we created) and "everything the installer had created has been removed"
    // (eight steps' worth had just been listed as left in place, right above).
    if !ctx.dry_run && others.is_empty() {
        let ops = make_ops();
        if ops.path_exists(&ctx.odoo_home) {
            warn!(
                home = %ctx.odoo_home.display(),
                "the rollback finished but the home still exists: it holds something we did \
                 not create, and we do not remove it (never an rm -rf on other people's \
                 things)"
            );
            report.home_left_behind = Some(ctx.odoo_home.clone());
        }
    }

    report
}
