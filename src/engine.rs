//! the engine: [`Installer`], with `execute` and `rollback`.
//!
//! orchestrates a sequence of [`Step`]s under the four invariants of
//! `CLAUDE.md`: snapshot before run, persistence after each successful run,
//! rollback in reverse order and best-effort.
//!
//! the rollback here is **in-process**: it undoes the steps completed in this
//! same run, tracked in `completed`. undoing from the state persisted by
//! [`InstallState::save`] — after a Ctrl-C, a crash, or an uninstall much later
//! — lives in [`crate::rollback`] under the same contract.
//!
//! progress is reported through the
//! [`ProgressReporter`](crate::progress::ProgressReporter) abstraction: the
//! engine does **not** depend on `indicatif`. `execute`/`rollback` delegate to
//! a [`NoopReporter`]; the `*_with_reporter` variants take the observer.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tracing::{error, info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::progress::{NoopReporter, ProgressReporter};
use crate::state::{InstallConfig, InstallState, StepRecord};
use crate::step::Step;

/// runs the steps and, on failure, restores the previous state.
#[derive(Debug, Default)]
pub struct Installer {
    state: InstallState,
    /// raised from outside on `SIGINT`/`SIGTERM` (B-V3-5).
    ///
    /// the engine knows nothing about signals: it watches a boolean. who raises
    /// it — `crate::interrupt` in production, a test elsewhere — is the
    /// caller's business. the default flag is never raised, so behaviour
    /// without [`Installer::watching_interrupt`] is unchanged.
    interrupted: Arc<AtomicBool>,
}

impl Installer {
    /// creates an installer with empty state.
    pub fn new() -> Self {
        Self::default()
    }

    /// wires the interrupt flag, observed **between steps**.
    ///
    /// halfway is not a safe boundary: a truncated `apt` leaves `dpkg`
    /// inconsistent. the wait is usually nil anyway, since the signal reaches
    /// the whole process group and the command in flight dies by itself — but
    /// it must be *said* to the user, not implied.
    pub fn watching_interrupt(mut self, flag: Arc<AtomicBool>) -> Self {
        self.interrupted = flag;
        self
    }

    /// creates an installer that **resumes** from a partial manifest (A-V3-1).
    ///
    /// steps already recorded are not re-run: their snapshot is rehydrated and
    /// they count as completed. re-running would be idempotent in its effects
    /// but amnesic about ownership — the artifacts would come back
    /// `Preexisting` and the anti-drop rule would strand the database we
    /// created. what is inherited is that the step **has already run**, not
    /// merely its verdict: running it anyway would `create_dir` over an
    /// existing directory.
    pub fn resuming_from(state: InstallState) -> Self {
        Self {
            state,
            ..Default::default()
        }
    }

    /// read-only access to the accumulated state.
    pub fn state(&self) -> &InstallState {
        &self.state
    }

    /// as [`Installer::execute_with_reporter`], without progress reporting.
    pub fn execute(&mut self, steps: &mut [Box<dyn Step>], ctx: &Context) -> Result<(), StepError> {
        self.execute_with_reporter(steps, ctx, &NoopReporter)
    }

    /// runs `steps` in sequence: `snapshot` then `run` for each, reporting to
    /// `reporter`.
    ///
    /// every completed step is recorded and **persisted** to disk (invariant 4;
    /// skipped in `dry_run`).
    ///
    /// # errors
    ///
    /// on any failure it rolls back the completed steps and propagates the
    /// error.
    pub fn execute_with_reporter(
        &mut self,
        steps: &mut [Box<dyn Step>],
        ctx: &Context,
        reporter: &dyn ProgressReporter,
    ) -> Result<(), StepError> {
        let total = steps.len();
        let mut completed: Vec<usize> = Vec::new();

        // the config enters the state *before* the first step: it is what lets
        // `invok rollback` know which artifacts to undo if this process never
        // reaches the end. no password goes in.
        //
        // on a resume it is already there and is **not** touched: `main` has
        // checked that the requested one matches on artifact identity (see
        // `InstallConfig::same_identity`). overwriting it here would allow
        // renaming a running installation's artifacts, i.e. pointing the undos
        // somewhere else.
        if self.state.config.is_none() {
            self.state.set_config(InstallConfig::from_context(ctx));
        }

        for idx in 0..steps.len() {
            // checked *before* the step, not after: that way the last completed
            // step is genuinely complete and the rollback starts from a state
            // the engine can describe.
            if self.interrupted.load(Ordering::SeqCst) {
                warn!("interruption requested: undoing the steps already run");
                self.rollback_with_reporter(steps, &completed, ctx, reporter);
                return Err(crate::interrupt::interrupted_error());
            }

            let name = steps[idx].name().to_string();
            reporter.step_start(&name, idx, total);

            // resume: this step ran in an earlier execution. rehydrate its
            // snapshot and treat it as completed, re-running neither `snapshot`
            // (it would photograph the system *after* our mutations) nor `run`
            // (which would fail on an artifact that already exists).
            if let Some(record) = self.state.record_for(&name) {
                let snapshot = record.snapshot.clone();
                if let Err(e) = steps[idx].rehydrate(&snapshot) {
                    // fail-closed, as in the rollback from disk: without a
                    // readable snapshot we do not know who owns the artifact,
                    // and carrying on would build a manifest that lies.
                    error!(
                        step = %name,
                        error = %e,
                        "resume: persisted snapshot unreadable, cannot resume"
                    );
                    reporter.step_failed(&name);
                    self.rollback_with_reporter(steps, &completed, ctx, reporter);
                    return Err(e);
                }
                info!(step = %name, "resume: already run, skipping snapshot and run");
                completed.push(idx);
                reporter.step_done(&name);
                continue;
            }

            info!(step = %name, "snapshot");
            if let Err(e) = steps[idx].snapshot(ctx) {
                error!(step = %name, error = %e, "snapshot failed, rolling back");
                reporter.step_failed(&name);
                self.rollback_with_reporter(steps, &completed, ctx, reporter);
                return Err(e);
            }

            info!(step = %name, dry_run = ctx.dry_run, "run");
            if let Err(e) = steps[idx].run(ctx) {
                error!(step = %name, error = %e, "run failed, rolling back");
                reporter.step_failed(&name);
                // the **failing** step is undone too, first of all (A-V3-24).
                //
                // a `run` that fails halfway has usually already created
                // something: `clone-odoo-repo` makes its directories before
                // going to the network, `create-odoo-user` runs `useradd`
                // before the `chown`. leaving that step out of the rollback —
                // as this did until the field showed it — leaves those
                // artifacts on disk, `/opt/odoo` non-empty, and therefore the
                // whole home behind. worse, it poisons every later run: the
                // next `prepare-opt-root` finds the directory `Preexisting`
                // and its undo is a legitimate no-op forever.
                //
                // safe because an undo acts **only** on `CreatedByUs`, and
                // that verdict is set by the step itself the moment the
                // artifact comes into existence. a step that failed before
                // creating anything undoes nothing.
                let mut to_undo = completed.clone();
                to_undo.push(idx);
                self.rollback_with_reporter(steps, &to_undo, ctx, reporter);
                return Err(e);
            }

            completed.push(idx);
            let record = StepRecord {
                name: name.clone(),
                snapshot: steps[idx].snapshot_value(),
            };
            self.state.record(record);

            if !ctx.dry_run {
                if let Err(e) = self.state.save(&ctx.state_path) {
                    error!(step = %name, error = %e, "persisting the state failed, rolling back");
                    reporter.step_failed(&name);
                    self.rollback_with_reporter(steps, &completed, ctx, reporter);
                    return Err(e);
                }
            } else {
                info!(step = %name, "persistence skipped (dry run)");
            }

            reporter.step_done(&name);
        }

        info!(steps = completed.len(), "run completed");
        Ok(())
    }

    /// marks the installation as finished and persists the state.
    ///
    /// the file left behind is the **uninstall manifest**, which is what lets
    /// `invok rollback` remove the instance later without touching the
    /// customer's things (A-R5-1).
    ///
    /// # errors
    ///
    /// propagates a persistence failure. writes nothing in `dry_run`.
    pub fn mark_finished(&mut self, ctx: &Context) -> Result<(), StepError> {
        self.state.finished = true;
        if ctx.dry_run {
            return Ok(());
        }
        self.state.save(&ctx.state_path)
    }

    /// as [`Installer::rollback_with_reporter`], without progress reporting.
    pub fn rollback(&mut self, steps: &[Box<dyn Step>], completed: &[usize], ctx: &Context) {
        self.rollback_with_reporter(steps, completed, ctx, &NoopReporter);
    }

    /// undoes the given steps in **reverse order** (invariant 2), best-effort
    /// (invariant 3), reporting to `reporter`.
    ///
    /// # the shared-artifact rule applies here too (A-V6-10)
    ///
    /// this is the rollback that runs when an installation fails **halfway**,
    /// and until now it undid every completed step regardless of who else lives
    /// on this machine. usually harmless: an instance added to a machine that
    /// already has one finds every shared artifact `Preexisting`, so those undos
    /// are no-ops and the `PreState` protects on its own.
    ///
    /// the scenario it did not cover is narrow and real. an instance that
    /// **created** the shared artifacts is interrupted; a second instance is
    /// installed; the first is then resumed and that attempt fails. there the
    /// `PreState` still says `CreatedByUs` — truthfully — and this rollback
    /// would take `/opt/odoo`, the packages and the cluster away from an
    /// instance that is running.
    ///
    /// the rule is [`crate::steps::artifact_scope`], the same one
    /// [`crate::rollback::rollback_from_state_sharing_with`] applies, and it
    /// must be: two rollbacks with different opinions about what is safe to
    /// remove would be a difference nobody can see until it costs a customer.
    ///
    /// **the list comes from the `Context`, not from a parameter of its own.**
    /// The engine still does not read manifests — that is `main`'s job, as with
    /// `state_path` — but taking it from `ctx.other_instances` means the driver's
    /// decision (*whether to call the undo*) and the step's (*what to skip
    /// inside*, via `Context::shared_in_use`) cannot disagree. The disk rollback
    /// takes both and documents that the caller must keep them in step; here
    /// there is nothing to keep in step.
    pub fn rollback_with_reporter(
        &mut self,
        steps: &[Box<dyn Step>],
        completed: &[usize],
        ctx: &Context,
        reporter: &dyn ProgressReporter,
    ) {
        if completed.is_empty() {
            return;
        }
        warn!(
            steps = completed.len(),
            "rollback in progress (reverse order)"
        );
        reporter.rollback_start(completed.len());
        let others = &ctx.other_instances;
        for &idx in completed.iter().rev() {
            let step = &steps[idx];
            let name = step.name();
            reporter.undo_start(name);

            // `unnamed` decides two of the scopes, and it is read from the same
            // context the undos act through.
            let scope = crate::steps::artifact_scope(name, ctx.instance.is_none());
            if !others.is_empty() && scope == crate::steps::ArtifactScope::Shared {
                info!(
                    step = %name,
                    in_use_by = ?others,
                    "undo NOT run: it removes artifacts other instances are still using"
                );
                // the record stays: what it describes is still on the system,
                // and the last instance to leave needs it to know the artifact
                // is its to remove (A-R8-1 — the manifest says what is *still*
                // there, not what was done).
                reporter.undo_done(name);
                continue;
            }

            info!(step = %name, "undo");
            match step.undo(ctx) {
                // a mixed step did its own half and left the shared one, so the
                // record still describes something that exists: keep it, for
                // the same reason as above.
                Ok(()) if !others.is_empty() && scope == crate::steps::ArtifactScope::Mixed => {
                    info!(
                        step = %name,
                        in_use_by = ?others,
                        "undo did this instance's half only: the shared artifacts stay"
                    );
                }
                // undone: the artifact is gone, and the manifest must stop
                // saying otherwise (A-R8-1).
                Ok(()) => self.state.forget(name),
                Err(e) => warn!(
                    step = %name,
                    error = %e,
                    "undo failed, continuing the cleanup (best-effort). the step stays in \
                     the manifest: it is the only record of the leftover"
                ),
            }
            reporter.undo_done(name);
        }

        // if the process dies now, what stays written must be what stayed on
        // the system. and if **nothing** is left, the manifest must go too: a
        // file describing zero artifacts is a leftover that lies, and would
        // make `invok rollback` believe there is something to consume.
        if !ctx.dry_run {
            let outcome = if self.state.completed.is_empty() {
                InstallState::clear(&ctx.state_path)
            } else {
                self.state.save(&ctx.state_path)
            };
            if let Err(e) = outcome {
                warn!(
                    path = %ctx.state_path.display(),
                    error = %e,
                    "cannot update the manifest after the rollback: it may still list steps \
                     that were already undone"
                );
            }
        }
    }
}

/// the `--dry-run` plan: `snapshot` (read-only) then `run` in dry-run mode,
/// which **logs the intent without mutating**.
///
/// no persistence, no rollback. the caller must guarantee `ctx.dry_run`.
///
/// an unavailable snapshot does not interrupt the plan: it is reported and the
/// next step is planned.
pub fn dry_run_plan(steps: &mut [Box<dyn Step>], ctx: &Context, reporter: &dyn ProgressReporter) {
    let total = steps.len();
    for (idx, step) in steps.iter_mut().enumerate() {
        let name = step.name().to_string();
        reporter.step_start(&name, idx, total);

        if let Err(e) = step.snapshot(ctx) {
            warn!(step = %name, error = %e, "dry run: snapshot unavailable, skipping");
            reporter.step_failed(&name);
            continue;
        }
        match step.run(ctx) {
            Ok(()) => reporter.step_done(&name),
            Err(e) => {
                warn!(step = %name, error = %e, "dry run: run");
                reporter.step_failed(&name);
            }
        }
    }
}
