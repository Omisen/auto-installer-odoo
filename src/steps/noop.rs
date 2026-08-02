//! [`NoopStep`]: step fittizio per testare il motore end-to-end.
//!
//! Non tocca il sistema. È parametrizzabile per fallire in `snapshot`, `run` o
//! `undo`, e registra le proprie chiamate di `undo` in modo osservabile dai
//! test (contatore + log condiviso), così da poter verificare ordine inverso,
//! best-effort e la regola "undo agisce solo su `CreatedByUs`".

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};

/// Log condiviso dell'ordine in cui gli `undo` compiono un'azione effettiva.
///
/// Solo gli `undo` che *agiscono* (step `CreatedByUs`) vi aggiungono il proprio
/// nome; un `undo` NO-OP (step `Preexisting`) non compare. Più `NoopStep`
/// possono condividere lo stesso log per verificarne l'ordine reciproco.
pub type UndoLog = Arc<Mutex<Vec<String>>>;

/// Step fittizio, senza effetti sul sistema, per i test del motore.
pub struct NoopStep {
    name: String,
    prestate: PreState,
    fail_on_snapshot: bool,
    fail_on_run: bool,
    fail_on_undo: bool,
    /// Numero di volte in cui `undo` è stato *invocato* (anche se NO-OP).
    undo_calls: Arc<AtomicUsize>,
    /// Log condiviso delle azioni di undo effettivamente compiute.
    undo_log: Option<UndoLog>,
    /// Azione arbitraria eseguita **dentro** il `run`.
    ///
    /// Serve a modellare qualcosa che accade *mentre* uno step è in corso —
    /// il caso vero è un segnale che arriva a metà installazione (B-V3-5). Senza
    /// questo gancio si potrebbe solo alzare il flag prima o dopo, cioè provare
    /// tutto tranne il momento che conta.
    #[allow(clippy::type_complexity)]
    on_run: Option<Box<dyn Fn() + Send + Sync>>,
}

impl NoopStep {
    /// Crea uno step che, di default, si comporta come creato da noi
    /// (`CreatedByUs`) e non fallisce mai.
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

    /// Imposta il `PreState` simulato che `snapshot` registrerà.
    pub fn with_prestate(mut self, prestate: PreState) -> Self {
        self.prestate = prestate;
        self
    }

    /// Comodità: marca lo step come `Preexisting` (undo diventerà un NO-OP).
    pub fn preexisting(self) -> Self {
        self.with_prestate(PreState::Preexisting)
    }

    /// Fa fallire `snapshot` (per testare il rollback dei precedenti).
    pub fn fail_on_snapshot(mut self) -> Self {
        self.fail_on_snapshot = true;
        self
    }

    /// Fa fallire `run` (per innescare il rollback).
    pub fn fail_on_run(mut self) -> Self {
        self.fail_on_run = true;
        self
    }

    /// Fa fallire `undo` (per testare il comportamento best-effort).
    pub fn fail_on_undo(mut self) -> Self {
        self.fail_on_undo = true;
        self
    }

    /// Esegue `f` dentro il `run`, prima che lo step si dichiari completato.
    pub fn on_run(mut self, f: impl Fn() + Send + Sync + 'static) -> Self {
        self.on_run = Some(Box::new(f));
        self
    }

    /// Collega un log condiviso su cui annotare le azioni di undo.
    pub fn with_undo_log(mut self, log: UndoLog) -> Self {
        self.undo_log = Some(log);
        self
    }

    /// Handle condiviso al contatore delle *invocazioni* di undo, da leggere
    /// nei test dopo che lo step è stato spostato nel `Vec` del motore.
    pub fn undo_call_handle(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.undo_calls)
    }

    /// Numero di volte in cui `undo` è stato invocato su questo step.
    pub fn undo_call_count(&self) -> usize {
        self.undo_calls.load(Ordering::SeqCst)
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
                reason: "fallimento snapshot simulato".to_string(),
            });
        }
        // Uno step reale rileverebbe qui `Preexisting` vs `CreatedByUs`; il
        // NoopStep usa il valore preconfigurato.
        info!(step = %self.name, prestate = ?self.prestate, "snapshot registrato");
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if ctx.dry_run {
            info!(step = %self.name, "run (dry-run, nessuna mutazione)");
            return Ok(());
        }
        if let Some(f) = &self.on_run {
            f();
        }
        if self.fail_on_run {
            return Err(StepError::CommandFailed {
                command: format!("noop:{}", self.name),
                status: "1".to_string(),
                stderr: "fallimento run simulato".to_string(),
            });
        }
        info!(step = %self.name, "run eseguito");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        // Ogni invocazione è contata, anche quelle che risulteranno NO-OP.
        self.undo_calls.fetch_add(1, Ordering::SeqCst);

        // Invariante 3: undo agisce SOLO su artefatti creati da noi.
        if self.prestate != PreState::CreatedByUs {
            info!(
                step = %self.name,
                prestate = ?self.prestate,
                "undo NO-OP (non CreatedByUs)"
            );
            return Ok(());
        }

        // Da qui in poi è un'azione di undo effettiva: registrala nel log.
        if let Some(log) = &self.undo_log {
            if let Ok(mut entries) = log.lock() {
                entries.push(self.name.clone());
            }
        }

        if ctx.dry_run {
            info!(step = %self.name, "undo (dry-run, nessuna mutazione)");
            return Ok(());
        }

        if self.fail_on_undo {
            warn!(step = %self.name, "undo: fallimento simulato");
            return Err(StepError::Precondition(format!(
                "fallimento undo simulato per {}",
                self.name
            )));
        }

        info!(step = %self.name, "undo eseguito");
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
