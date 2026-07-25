//! Il motore: [`Installer`] con `execute` + `rollback`.
//!
//! Orchestra una sequenza di [`Step`] rispettando le 4 invarianti di
//! `CLAUDE.md`: snapshot prima di run, persistenza dopo ogni run riuscito,
//! rollback in ordine inverso e best-effort.

use tracing::{error, info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::{InstallState, StepRecord};
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

    /// Esegue in sequenza `steps`: per ognuno `snapshot` → `run`.
    ///
    /// Ogni step completato con successo viene registrato e **persistito** su
    /// disco subito dopo (invariante 4; saltato in `dry_run`). Se `snapshot`,
    /// `run` o la persistenza falliscono, il motore esegue il [`rollback`] degli
    /// step già completati e propaga l'errore.
    ///
    /// [`rollback`]: Installer::rollback
    pub fn execute(
        &mut self,
        steps: &mut [Box<dyn Step>],
        ctx: &Context,
    ) -> Result<(), StepError> {
        // Indici degli step completati con successo, in ordine di esecuzione.
        let mut completed: Vec<usize> = Vec::new();

        // Loop per indice (non `iter_mut`) per poter riprestare `steps` in
        // sola lettura a `rollback` senza conflitti di borrow.
        for idx in 0..steps.len() {
            let name = steps[idx].name().to_string();

            info!(step = %name, "snapshot");
            if let Err(e) = steps[idx].snapshot(ctx) {
                // Snapshot fallito: questo step non ha mutato nulla, ma senza
                // snapshot non è sicuro proseguire. Rollback dei precedenti.
                error!(step = %name, error = %e, "snapshot fallito, rollback in corso");
                self.rollback(steps, &completed, ctx);
                return Err(e);
            }

            info!(step = %name, dry_run = ctx.dry_run, "run");
            if let Err(e) = steps[idx].run(ctx) {
                error!(step = %name, error = %e, "run fallito, rollback in corso");
                self.rollback(steps, &completed, ctx);
                return Err(e);
            }

            // Run riuscito: registra e persisti prima di procedere.
            completed.push(idx);
            let record = StepRecord {
                name: name.clone(),
                snapshot: steps[idx].snapshot_value(),
            };
            self.state.record(record);

            if !ctx.dry_run {
                if let Err(e) = self.state.save(&ctx.state_path) {
                    // Lo stato su disco è ora incoerente col sistema: meglio
                    // ripristinare piuttosto che lasciare uno stato sporco.
                    error!(step = %name, error = %e, "persistenza stato fallita, rollback in corso");
                    self.rollback(steps, &completed, ctx);
                    return Err(e);
                }
            } else {
                info!(step = %name, "persistenza saltata (dry-run)");
            }
        }

        info!(steps = completed.len(), "esecuzione completata");
        Ok(())
    }

    /// Esegue l'`undo` degli step indicati in **ordine inverso** (invariante 2).
    ///
    /// Best-effort (invariante 3): un `undo` che fallisce viene loggato a
    /// `warn` e la pulizia prosegue con gli altri step. `completed` contiene gli
    /// indici in `steps` degli step da annullare, in ordine di esecuzione.
    pub fn rollback(
        &self,
        steps: &[Box<dyn Step>],
        completed: &[usize],
        ctx: &Context,
    ) {
        if completed.is_empty() {
            return;
        }
        warn!(steps = completed.len(), "rollback in corso (ordine inverso)");
        for &idx in completed.iter().rev() {
            let step = &steps[idx];
            info!(step = %step.name(), "undo");
            if let Err(e) = step.undo(ctx) {
                warn!(
                    step = %step.name(),
                    error = %e,
                    "undo fallito, proseguo con la pulizia (best-effort)"
                );
            }
        }
    }
}
