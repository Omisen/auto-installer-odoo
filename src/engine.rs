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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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
    /// Alzato da fuori quando arriva `SIGINT`/`SIGTERM` (B-V3-5).
    ///
    /// Il motore **non** conosce i segnali: osserva un booleano. Chi glielo
    /// alza — `crate::interrupt` in produzione, un test altrove — è affare di
    /// chi costruisce l'installer. Di default è un flag mai alzato, quindi il
    /// comportamento senza `watching_interrupt` è identico a prima.
    interrupted: Arc<AtomicBool>,
}

impl Installer {
    /// Crea un installer con stato vuoto.
    pub fn new() -> Self {
        Self::default()
    }

    /// Collega il flag d'interruzione osservato **fra uno step e l'altro**.
    ///
    /// # Perché fra uno step e l'altro, e non «subito»
    ///
    /// Interrompere uno step a metà non è una cosa che si possa fare in modo
    /// sicuro: `apt` a metà lascia `dpkg` inconsistente, un `initdb` troncato
    /// lascia un database inutilizzabile. Il confine sicuro è quello che il
    /// motore già conosce — uno step è un'unità che o è completa o non è
    /// iniziata — ed è lì che si guarda.
    ///
    /// In pratica l'attesa è breve e spesso nulla: il segnale arriva a **tutto
    /// il process group**, quindi il comando esterno in corso (`apt`, `git`,
    /// `pip`) muore da sé e lo step fallisce subito dopo. Il flag serve per i
    /// casi in cui lo step è nostro e non ha figli da uccidere.
    ///
    /// Questo va **detto** all'utente e non lasciato intendere: chi preme
    /// Ctrl-C durante `pip install` non deve credere che l'interruzione sia
    /// istantanea.
    pub fn watching_interrupt(mut self, flag: Arc<AtomicBool>) -> Self {
        self.interrupted = flag;
        self
    }

    /// Crea un installer che **riprende** da un manifesto parziale (A-V3-1).
    ///
    /// Gli step già registrati in `state` non vengono rieseguiti: il motore ne
    /// reidrata lo snapshot e li considera completati. È l'unico modo di
    /// mantenere la promessa «rilancia e prosegui» senza perdere la
    /// **proprietà** degli artefatti.
    ///
    /// # Perché non basta rieseguire
    ///
    /// Rilanciare da zero è idempotente negli *effetti* — ogni snapshot vede il
    /// proprio artefatto già presente e il `run` non fa nulla — ma è amnesico
    /// sulla *proprietà*: quegli artefatti li avevamo creati **noi**, e il nuovo
    /// manifesto li dichiarerebbe `Preexisting`. Il database creato dal primo
    /// giro finirebbe protetto dall'anti-drop e non verrebbe rimosso mai più.
    /// La proprietà, come per il rollback da disco, si **rilegge** — non si
    /// rideduce.
    ///
    /// # Perché non basta ereditare il `PreState`
    ///
    /// Reidratare lo step e poi eseguirne comunque il `run` è peggio che
    /// inutile: `PrepareOptRoot::run` con `CreatedByUs` ereditato chiamerebbe
    /// `create_dir` su una directory che esiste già e fallirebbe. Ciò che si
    /// eredita è il fatto che lo step **è già stato eseguito**, non solo il suo
    /// esito.
    pub fn resuming_from(state: InstallState) -> Self {
        Self {
            state,
            ..Default::default()
        }
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
        //
        // In un resume la configurazione è già nello stato e **non** viene
        // toccata: `main` ha già verificato che quella richiesta coincida
        // sull'identità degli artefatti (vedi `InstallConfig::same_identity`).
        // Sovrascriverla qui significherebbe poter rinominare gli artefatti di
        // un'installazione in corso — cioè far puntare gli undo altrove.
        if self.state.config.is_none() {
            self.state.set_config(InstallConfig::from_context(ctx));
        }

        for idx in 0..steps.len() {
            // Interruzione richiesta: si annulla ciò che è stato fatto e si
            // esce. Il controllo sta **prima** dello step, non dopo: così
            // l'ultimo step completato è davvero completo, e il rollback parte
            // da uno stato che il motore sa descrivere.
            if self.interrupted.load(Ordering::SeqCst) {
                warn!("interruzione richiesta: annullo gli step già eseguiti");
                self.rollback_with_reporter(steps, &completed, ctx, reporter);
                return Err(crate::interrupt::interrupted_error());
            }

            let name = steps[idx].name().to_string();
            reporter.step_start(&name, idx, total);

            // Resume: lo step risulta già eseguito in un'esecuzione precedente.
            // Si reidrata il suo snapshot e lo si considera completato, senza
            // rieseguire né `snapshot` (fotograferebbe il sistema DOPO le nostre
            // mutazioni) né `run` (che su un artefatto già creato fallirebbe).
            if let Some(record) = self.state.record_for(&name) {
                let snapshot = record.snapshot.clone();
                if let Err(e) = steps[idx].rehydrate(&snapshot) {
                    // Fail-closed, come nel rollback da disco: senza uno
                    // snapshot leggibile non sappiamo di chi sia l'artefatto, e
                    // proseguire significherebbe costruire un manifesto che
                    // mente. Meglio fermarsi prima di mutare altro.
                    error!(
                        step = %name,
                        error = %e,
                        "resume: snapshot persistito illeggibile, non posso riprendere"
                    );
                    reporter.step_failed(&name);
                    self.rollback_with_reporter(steps, &completed, ctx, reporter);
                    return Err(e);
                }
                info!(step = %name, "resume: già eseguito, salto snapshot e run");
                completed.push(idx);
                reporter.step_done(&name);
                continue;
            }

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

    /// Marca l'installazione come conclusa e persiste lo stato.
    ///
    /// Da chiamare a esecuzione riuscita. Il file che resta sul disco è il
    /// **manifesto di disinstallazione**: dice quali artefatti abbiamo creato e
    /// quali abbiamo trovato già presenti, ed è ciò che permette a
    /// `odoo-installer rollback` di rimuovere l'istanza in un secondo momento
    /// senza toccare nulla del cliente (A-R5-1).
    ///
    /// In `dry_run` non scrive nulla: una preview non lascia artefatti.
    pub fn mark_finished(&mut self, ctx: &Context) -> Result<(), StepError> {
        self.state.finished = true;
        if ctx.dry_run {
            return Ok(());
        }
        self.state.save(&ctx.state_path)
    }

    /// Rollback senza progresso (delega a [`NoopReporter`]).
    pub fn rollback(&mut self, steps: &[Box<dyn Step>], completed: &[usize], ctx: &Context) {
        self.rollback_with_reporter(steps, completed, ctx, &NoopReporter);
    }

    /// Esegue l'`undo` degli step indicati in **ordine inverso** (invariante 2),
    /// best-effort (invariante 3), notificando il `reporter`.
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
            "rollback in corso (ordine inverso)"
        );
        reporter.rollback_start(completed.len());
        for &idx in completed.iter().rev() {
            let step = &steps[idx];
            let name = step.name();
            reporter.undo_start(name);
            info!(step = %name, "undo");
            match step.undo(ctx) {
                // Annullato: l'artefatto non c'è più, e il manifesto non deve
                // continuare a dire il contrario (A-R8-1, vedi sotto).
                Ok(()) => self.state.forget(name),
                Err(e) => warn!(
                    step = %name,
                    error = %e,
                    "undo fallito, proseguo con la pulizia (best-effort). Lo step resta \
                     nel manifesto: e' l'unica traccia del residuo"
                ),
            }
            reporter.undo_done(name);
        }

        // Il manifesto aggiornato va su disco: se il processo muore ora, ciò che
        // resta scritto dev'essere ciò che è rimasto sul sistema.
        if !ctx.dry_run {
            if let Err(e) = self.state.save(&ctx.state_path) {
                warn!(
                    path = %ctx.state_path.display(),
                    error = %e,
                    "impossibile aggiornare il manifesto dopo il rollback: potrebbe elencare \
                     step gia' annullati"
                );
            }
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
