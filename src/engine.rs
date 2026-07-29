//! Il motore: [`Installer`] con `execute` + `rollback`.
//!
//! Orchestra una sequenza di [`Step`] rispettando le 4 invarianti di
//! `CLAUDE.md`: snapshot prima di run, persistenza dopo ogni run riuscito,
//! rollback in ordine inverso e best-effort.
//!
//! Il rollback di questo modulo è **in-process**: annulla gli step completati
//! nella stessa esecuzione, tenuti in `completed`. Il rollback a partire dallo
//! stato persistito da [`InstallState::save`] — per un Ctrl-C, un crash o una
//! disinstallazione a posteriori — vive in [`crate::rollback`] e usa lo stesso
//! contratto: `undo` in ordine inverso, best-effort, `PreState` come sola fonte
//! di verità.
//!
//! Il progresso è notificato tramite l'astrazione
//! [`ProgressReporter`](crate::progress::ProgressReporter): il motore **non**
//! dipende da `indicatif`. Le firme storiche `execute`/`rollback` restano
//! invariate (delegano a un [`NoopReporter`]); le varianti `*_with_reporter`
//! accettano l'observer.

use tracing::{error, info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::progress::{NoopReporter, ProgressReporter};
use crate::state::{InstallConfig, InstallState, StepRecord};
use crate::step::Step;

/// Esegue gli step e, in caso di fallimento, ripristina lo stato precedente.
#[derive(Debug, Default)]
pub struct Installer {
    state: InstallState,
}

impl Installer {
    /// Crea un installer con stato vuoto.
    pub fn new() -> Self {
        Self::default()
    }

    /// Accesso in sola lettura allo stato accumulato (utile per test/ispezione).
    pub fn state(&self) -> &InstallState {
        &self.state
    }

    /// Come [`execute_with_reporter`](Installer::execute_with_reporter) ma senza
    /// progresso (usato dai test e dai chiamlanti che non ne hanno bisogno).
    pub fn execute(&mut self, steps: &mut [Box<dyn Step>], ctx: &Context) -> Result<(), StepError> {
        self.execute_with_reporter(steps, ctx, &NoopReporter)
    }

    /// Esegue in sequenza `steps`: per ognuno `snapshot` → `run`, notificando il
    /// `reporter`. Ogni step completato viene registrato e **persistito** su
    /// disco (invariante 4; saltato in `dry_run`). Su fallimento: rollback dei
    /// completati e propagazione dell'errore.
    pub fn execute_with_reporter(
        &mut self,
        steps: &mut [Box<dyn Step>],
        ctx: &Context,
        reporter: &dyn ProgressReporter,
    ) -> Result<(), StepError> {
        let total = steps.len();
        let mut completed: Vec<usize> = Vec::new();

        // La configurazione va nello stato *prima* del primo step: è ciò che
        // permette a `odoo-installer rollback` di sapere quali artefatti
        // annullare se questo processo non arriva mai alla fine (vedi
        // `crate::state::InstallConfig`). Nessuna password vi entra.
        self.state.set_config(InstallConfig::from_context(ctx));

        for idx in 0..steps.len() {
            let name = steps[idx].name().to_string();
            reporter.step_start(&name, idx, total);

            info!(step = %name, "snapshot");
            if let Err(e) = steps[idx].snapshot(ctx) {
                error!(step = %name, error = %e, "snapshot fallito, rollback in corso");
                reporter.step_failed(&name);
                self.rollback_with_reporter(steps, &completed, ctx, reporter);
                return Err(e);
            }

            info!(step = %name, dry_run = ctx.dry_run, "run");
            if let Err(e) = steps[idx].run(ctx) {
                error!(step = %name, error = %e, "run fallito, rollback in corso");
                reporter.step_failed(&name);
                self.rollback_with_reporter(steps, &completed, ctx, reporter);
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
                    error!(step = %name, error = %e, "persistenza stato fallita, rollback in corso");
                    reporter.step_failed(&name);
                    self.rollback_with_reporter(steps, &completed, ctx, reporter);
                    return Err(e);
                }
            } else {
                info!(step = %name, "persistenza saltata (dry-run)");
            }

            reporter.step_done(&name);
        }

        info!(steps = completed.len(), "esecuzione completata");
        Ok(())
    }

    /// Rollback senza progresso (delega a [`NoopReporter`]).
    pub fn rollback(&self, steps: &[Box<dyn Step>], completed: &[usize], ctx: &Context) {
        self.rollback_with_reporter(steps, completed, ctx, &NoopReporter);
    }

    /// Esegue l'`undo` degli step indicati in **ordine inverso** (invariante 2),
    /// best-effort (invariante 3), notificando il `reporter`.
    pub fn rollback_with_reporter(
        &self,
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
            "rollback in corso (ordine inverso)"
        );
        reporter.rollback_start(completed.len());
        for &idx in completed.iter().rev() {
            let step = &steps[idx];
            let name = step.name();
            reporter.undo_start(name);
            info!(step = %name, "undo");
            if let Err(e) = step.undo(ctx) {
                warn!(
                    step = %name,
                    error = %e,
                    "undo fallito, proseguo con la pulizia (best-effort)"
                );
            }
            reporter.undo_done(name);
        }
    }
}

/// Piano `--dry-run`: per ogni step esegue `snapshot` (read-only) e `run` in
/// dry-run (che **logga l'intenzione senza mutare**). Nessuna persistenza,
/// nessun rollback. Il chiamante deve garantire `ctx.dry_run == true`.
///
/// Uno snapshot non disponibile (es. query di sistema non ancora possibile) non
/// interrompe il piano: viene segnalato e si prosegue col prossimo step.
pub fn dry_run_plan(steps: &mut [Box<dyn Step>], ctx: &Context, reporter: &dyn ProgressReporter) {
    let total = steps.len();
    for (idx, step) in steps.iter_mut().enumerate() {
        let name = step.name().to_string();
        reporter.step_start(&name, idx, total);

        if let Err(e) = step.snapshot(ctx) {
            warn!(step = %name, error = %e, "dry-run: snapshot non disponibile, salto");
            reporter.step_failed(&name);
            continue;
        }
        match step.run(ctx) {
            Ok(()) => reporter.step_done(&name),
            Err(e) => {
                warn!(step = %name, error = %e, "dry-run: run");
                reporter.step_failed(&name);
            }
        }
    }
}
