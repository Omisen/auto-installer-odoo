//! Il trait [`Step`]: il contratto che ogni step dell'installer deve rispettare.
//!
//! Questo trait è la fondazione su cui poggiano tutte le fasi successive. Il
//! motore ([`crate::engine::Installer`]) lo orchestra senza conoscere i dettagli
//! del singolo step. **Non deve cambiare** quando si aggiungono nuovi step: se
//! un caso reale non ci sta, va segnalato, non forzato dentro a martellate.

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
///    dello step (nome + [`Step::snapshot_value`]) su disco. Attenzione: oggi lo
///    stato è **scritto ma non riletto** — il rollback è solo in-process; il
///    resume da stato persistito arriverà in R4 (vedi
///    [`crate::state`]).
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
}
