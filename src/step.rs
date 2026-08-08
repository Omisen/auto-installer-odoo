//! Il trait [`Step`]: il contratto che ogni step dell'installer deve rispettare.
//!
//! Questo trait è la fondazione su cui poggiano tutte le fasi successive. Il
//! motore ([`crate::engine::Installer`]) lo orchestra senza conoscere i dettagli
//! del singolo step. **Non deve cambiare** quando si aggiungono nuovi step: se
//! un caso reale non ci sta, va segnalato, non forzato dentro a martellate.

use serde::de::DeserializeOwned;

use crate::context::Context;
use crate::error::StepError;

/// Un passo reversibile dell'installazione.
///
/// # Contratto (le 4 invarianti di `CLAUDE.md`)
///
/// 1. **`snapshot` sempre prima di `run`.** Il motore chiama `snapshot` prima di
///    `run`. In `snapshot` lo step rileva e registra il proprio `PreState`
///    (`Preexisting` vs `CreatedByUs`): è la sola fonte di verità per l'undo.
/// 2. **Undo in ordine inverso.** Il motore esegue gli `undo` degli step
///    completati dall'ultimo al primo.
/// 3. **Undo idempotente e best-effort.** `undo` non deve fallire se l'artefatto
///    è già assente, e deve agire **solo** se lo step ha creato l'artefatto
///    (`PreState == CreatedByUs`); su `Preexisting` è un NO-OP. Un `undo` che
///    fallisce non blocca la pulizia degli altri step (il motore logga e
///    prosegue).
/// 4. **Stato persistito.** Dopo un `run` riuscito il motore persiste il record
///    dello step (nome + [`Step::snapshot_value`]) su disco. Lo stato ha un
///    consumatore: `invok rollback` lo rilegge, ricostruisce gli step
///    con [`Step::rehydrate`] e ne esegue gli `undo` in ordine inverso (vedi
///    [`crate::rollback`]).
///
/// # `dry_run`
///
/// Quando [`Context::dry_run`] è `true`, `run` e `undo` non devono mutare il
/// sistema: si limitano a loggare cosa *avrebbero* fatto.
pub trait Step {
    /// Nome stabile e univoco dello step (usato nei log e nello stato persistito).
    fn name(&self) -> &str;

    /// Rileva e registra lo stato preesistente (`PreState`) prima di mutare.
    /// Chiamato dal motore **prima** di [`Step::run`].
    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError>;

    /// Esegue la mutazione. Deve rispettare `dry_run` (nessuna mutazione reale).
    fn run(&mut self, ctx: &Context) -> Result<(), StepError>;

    /// Annulla la mutazione. Best-effort, idempotente, e attivo solo su
    /// `PreState == CreatedByUs`. Prende `&self`: eventuali contatori interni
    /// per i test usano interior mutability.
    fn undo(&self, ctx: &Context) -> Result<(), StepError>;

    /// Snapshot serializzabile da persistere insieme al record dello step.
    ///
    /// Il valore è opaco al motore (JSON). Il default è `null` per step senza
    /// stato; gli step reali serializzano qui il proprio `PreState`.
    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::Value::Null
    }

    /// Ricarica lo stato interno da uno `snapshot_value` persistito, **senza**
    /// re-ispezionare il sistema.
    ///
    /// # Perché esiste (R4)
    ///
    /// `undo` decide cosa fare leggendo il `PreState` che `snapshot` ha
    /// registrato *prima* del run. Per annullare da disco — Ctrl-C, `kill -9`,
    /// o un `invok rollback` a installazione conclusa — quel
    /// `PreState` va rimesso nello step **così com'era all'epoca**. Rieseguire
    /// `snapshot` darebbe la risposta sbagliata: fotograferebbe il sistema
    /// *dopo* le nostre mutazioni, e un artefatto che abbiamo creato noi
    /// risulterebbe `Preexisting` (undo NO-OP) o viceversa — nel caso peggiore
    /// un database del cliente marcato come nostro e droppato.
    ///
    /// # Contratto
    ///
    /// `rehydrate` deve essere l'**inversa esatta** di [`Step::snapshot_value`]:
    /// per ogni stato interno `s`, `rehydrate(snapshot_value(s))` deve
    /// riprodurre un `undo` indistinguibile da quello di `s`. È la proprietà
    /// che rende affidabile il rollback da disco, ed è verificata step per step
    /// in `tests/rehydrate.rs`.
    ///
    /// Il default è un NO-OP, corretto per gli step il cui `undo` non consulta
    /// alcuno stato interno (`initialize-odoo-database`,
    /// `install-python-requirements`). Uno snapshot illeggibile è un errore:
    /// meglio dichiarare che quello step non è annullabile che annullarlo con
    /// uno stato inventato.
    fn rehydrate(&mut self, _snapshot: &serde_json::Value) -> Result<(), StepError> {
        Ok(())
    }
}

/// Deserializza uno snapshot persistito nel tipo interno di uno step,
/// trasformando un errore di formato in [`StepError::SnapshotFailed`].
///
/// Helper condiviso da tutte le implementazioni di [`Step::rehydrate`]: il
/// motivo per cui la reidratazione è una riga per step invece di venti righe di
/// gestione errori ripetute.
pub fn decode_snapshot<T: DeserializeOwned>(
    step: &str,
    snapshot: &serde_json::Value,
) -> Result<T, StepError> {
    serde_json::from_value(snapshot.clone()).map_err(|e| StepError::SnapshotFailed {
        step: step.to_string(),
        reason: format!("snapshot persistito non deserializzabile: {e}"),
    })
}
