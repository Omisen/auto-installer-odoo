//! Rollback **da stato persistito**: il consumatore di `InstallState` (R4).
//!
//! Il motore ([`crate::engine::Installer`]) sa annullare ciò che ha appena
//! fatto: tiene gli step vivi in memoria, con il loro `PreState`, e ne chiama
//! gli `undo` in ordine inverso. È il rollback *in-process*, e copre il caso
//! normale — uno step fallisce, l'installazione si ritira.
//!
//! Non copre i casi in cui il processo **non arriva** alla gestione dell'errore:
//! un Ctrl-C, un `kill -9`, un OOM, un power-loss. Lì il sistema resta con gli
//! artefatti a metà e il file di stato intatto sul disco, che li descrive tutti.
//! Non copre nemmeno la disinstallazione di un'installazione **riuscita**, che
//! non è un fallimento ma una richiesta legittima ("rimuovi Odoo da questa
//! macchina").
//!
//! Questo modulo chiude entrambi i buchi con lo stesso meccanismo: per ogni
//! record persistito, ricostruisci lo step ([`crate::steps::step_by_name`]),
//! rimettici dentro lo snapshot dell'epoca
//! ([`crate::step::Step::rehydrate`]) ed esegui il suo `undo`. In ordine
//! inverso, best-effort, con le stesse protezioni critiche — che vivono nel
//! `PreState`, e il `PreState` viene *riletto*, mai reindovinato.
//!
//! # Cosa NON fa (deliberatamente)
//!
//! Non chiama `snapshot`. Rieseguirlo fotograferebbe il sistema **dopo** le
//! nostre mutazioni: il database che abbiamo creato risulterebbe già esistente,
//! quindi `Preexisting`, quindi non droppabile — e viceversa, un `.conf` del
//! cliente che avevamo salvato in backup risulterebbe nostro. Lo stato da usare
//! è quello di allora, ed è esattamente ciò che il file di stato contiene.

use std::path::PathBuf;

use tracing::{info, warn};

use crate::context::Context;
use crate::progress::ProgressReporter;
use crate::state::InstallState;
use crate::steps::{self, OpsFactory};

/// Esito dell'`undo` di un singolo step nel rollback da disco.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoOutcome {
    /// L'`undo` è stato eseguito senza errori (può essere stato un NO-OP
    /// legittimo: `PreState` diverso da `CreatedByUs`).
    Undone,
    /// L'`undo` ha fallito. Best-effort: il rollback prosegue, ma ciò che
    /// quell'`undo` non ha rimosso **resta** e va elencato all'utente (A1.3).
    Failed(String),
    /// Nome di step non riconosciuto: lo stato viene da una versione con step
    /// che questo binario non conosce.
    Unknown,
    /// Lo snapshot persistito non è deserializzabile nel tipo dello step.
    /// L'`undo` **non** viene eseguito: agire con uno stato inventato è peggio
    /// che non agire (potrebbe droppare ciò che lo snapshot proteggeva).
    NotRehydrated(String),
}

impl UndoOutcome {
    /// `true` se questo esito lascia qualcosa da ripulire a mano.
    pub fn is_residue(&self) -> bool {
        !matches!(self, UndoOutcome::Undone)
    }
}

/// Esito dell'`undo` di uno step, con il nome per il report finale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepOutcome {
    pub name: String,
    pub outcome: UndoOutcome,
}

/// Report di fine rollback (chiude A1.3 / B2).
///
/// Un rollback best-effort *può* lasciare residui: è la contropartita
/// dell'invariante 3 (un `undo` che fallisce non blocca gli altri). Finora quei
/// residui finivano in un `warn!` e sparivano nello scroll. Qui vengono
/// raccolti, così l'utente ha l'elenco esatto di cosa gli resta da rimuovere.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RollbackReport {
    /// Esiti nell'ordine in cui gli `undo` sono stati eseguiti (inverso).
    pub outcomes: Vec<StepOutcome>,
    /// La home dell'installazione **esiste ancora** a rollback concluso?
    ///
    /// # Perché si guarda il sistema e non solo gli esiti (A-MD-2)
    ///
    /// Perché sono due domande diverse, e su una prova reale la seconda ha
    /// mentito: il rollback ha dichiarato «nessun residuo» mentre `/opt/odoo`
    /// era ancora lì. Tutti gli `undo` erano riusciti, compreso quello di
    /// `PrepareOptRoot` — che davanti a una directory non vuota **rinuncia**,
    /// correttamente e senza mai un `rm -rf`, e restituisce `Ok`. Il verdetto
    /// sugli esiti era vero; la promessa che l'utente legge no.
    ///
    /// È la lezione di R7 rovesciata. Lì un test di CI asseriva il residuo come
    /// atteso; qui è il **report all'utente** a dichiarare pulito ciò che non lo
    /// è. Il correttivo è lo stesso: verificare la **promessa** (`/opt/odoo` non
    /// deve esistere) e non il **meccanismo** (ogni undo è andato bene).
    ///
    /// Non entra in [`Self::is_clean`]: quella decide se il manifesto può essere
    /// consumato, e un manifesto che non descrive più alcun artefatto va rimosso
    /// comunque (R19). Tenerlo in vita per un file che **non abbiamo creato
    /// noi** farebbe credere che ci sia ancora qualcosa da annullare, e un
    /// secondo `rollback` non potrebbe farci nulla.
    pub home_left_behind: Option<PathBuf>,
}

impl RollbackReport {
    /// Gli step che hanno lasciato qualcosa dietro di sé.
    pub fn residue(&self) -> Vec<&StepOutcome> {
        self.outcomes
            .iter()
            .filter(|o| o.outcome.is_residue())
            .collect()
    }

    /// `true` se ogni `undo` è andato a buon fine: lo stato può essere
    /// consumato (rimosso), perché non descrive più nulla di vivo.
    pub fn is_clean(&self) -> bool {
        self.residue().is_empty()
    }

    /// C'è qualcosa da dire all'utente oltre al conteggio degli undo?
    pub fn has_anything_to_report(&self) -> bool {
        !self.is_clean() || self.home_left_behind.is_some()
    }

    /// Numero di step effettivamente annullati.
    pub fn undone(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|o| o.outcome == UndoOutcome::Undone)
            .count()
    }
}

/// Stato in cui il file di stato ha trovato l'installazione.
///
/// Serve a dire all'utente *cosa* sta per annullare: disinstallare un'istanza
/// funzionante e ripulire i resti di una run interrotta sono due operazioni con
/// conseguenze molto diverse, e meritano due frasi diverse prima della conferma.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallStatus {
    /// Tutti gli step canonici risultano completati.
    Complete { steps: usize },
    /// L'installazione si è fermata prima della fine (Ctrl-C, crash, errore).
    Interrupted { done: usize, total: usize },
}

/// Classifica lo stato persistito.
///
/// La fonte primaria è il flag [`InstallState::finished`], scritto
/// dall'installazione quando arriva in fondo. Il confronto con la sequenza
/// canonica resta come ripiego per gli stati scritti prima che il flag
/// esistesse: è un'euristica (la sequenza cambia fra versioni), quindi non ha
/// la precedenza.
///
/// `total` è la lunghezza della sequenza canonica
/// ([`steps::canonical_step_names`]), passata invece che ricavata qui dentro:
/// così questa resta una funzione **pura**, verificabile senza costruire
/// ventitré step — e senza dover nominare una famiglia per contare dei nomi.
pub fn install_status(state: &InstallState, total: usize) -> InstallStatus {
    let done = state.completed.len();
    if state.finished || done >= total {
        InstallStatus::Complete { steps: done }
    } else {
        InstallStatus::Interrupted { done, total }
    }
}

/// Cosa fare prima di eseguire il rollback, dato il contesto d'invocazione.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmationGate {
    /// Si procede senza chiedere: `--yes`, oppure `--dry-run` (non muta nulla).
    Proceed,
    /// C'è un terminale: chiedi conferma esplicita all'utente.
    Ask,
    /// Nessun terminale e nessun `--yes`: rifiuta. Un'operazione distruttiva non
    /// va eseguita "per default" dentro uno script che non l'ha chiesta.
    RefuseNonInteractive,
}

/// Politica di conferma del rollback, **pura** — così è verificabile senza
/// terminale e senza eseguire il comando.
///
/// Il dry-run non chiede nulla perché non c'è nulla da confermare: elenca e
/// basta. Il caso che conta è l'ultimo: senza TTY, `--yes` diventa obbligatorio.
pub fn confirmation_gate(dry_run: bool, yes: bool, interactive: bool) -> ConfirmationGate {
    if dry_run || yes {
        ConfirmationGate::Proceed
    } else if interactive {
        ConfirmationGate::Ask
    } else {
        ConfirmationGate::RefuseNonInteractive
    }
}

/// I nomi degli step nell'ordine in cui verranno annullati (inverso).
///
/// Puro: serve al riepilogo mostrato prima della conferma e al `--dry-run`,
/// senza toccare il sistema.
pub fn undo_plan(state: &InstallState) -> Vec<&str> {
    state
        .completed
        .iter()
        .rev()
        .map(|r| r.name.as_str())
        .collect()
}

/// Esegue il rollback degli step descritti da `state`, in **ordine inverso**
/// (invariante 2) e **best-effort** (invariante 3).
///
/// `ctx` va costruito dalla configurazione persistita
/// ([`crate::state::InstallConfig::to_context`]): è ciò che dà agli `undo` i
/// nomi reali degli artefatti creati. Con `ctx.dry_run == true` nessun `undo`
/// muta il sistema — ognuno si limita a loggare cosa farebbe.
///
/// Non ritorna `Result`: un rollback non "fallisce", accumula esiti. Quello che
/// non è riuscito a pulire finisce nel [`RollbackReport`], che è la risposta
/// onesta da dare all'utente.
pub fn rollback_from_state(
    state: &InstallState,
    ctx: &Context,
    make_ops: OpsFactory<'_>,
    reporter: &dyn ProgressReporter,
) -> RollbackReport {
    let mut report = RollbackReport::default();
    if state.completed.is_empty() {
        info!("rollback: nessuno step da annullare");
        return report;
    }

    warn!(
        steps = state.completed.len(),
        dry_run = ctx.dry_run,
        "rollback da stato persistito (ordine inverso)"
    );
    reporter.rollback_start(state.completed.len());

    for record in state.completed.iter().rev() {
        let name = record.name.clone();
        reporter.undo_start(&name);

        let Some(mut step) = steps::step_by_name(&name, make_ops) else {
            warn!(
                step = %name,
                "rollback: step sconosciuto a questo binario, non annullabile (proseguo)"
            );
            report.outcomes.push(StepOutcome {
                name: name.clone(),
                outcome: UndoOutcome::Unknown,
            });
            // Anche uno step non annullabile è uno step **esaminato**: la barra
            // deve avanzare (A-V3-10). Ometterlo lasciava il progresso fermo
            // proprio nello scenario degradato, che è quello in cui l'utente la
            // guarda — e faceva sembrare bloccato un rollback che stava andando.
            reporter.undo_done(&name);
            continue;
        };

        if let Err(e) = step.rehydrate(&record.snapshot) {
            warn!(
                step = %name,
                error = %e,
                "rollback: snapshot persistito illeggibile, undo saltato per sicurezza (proseguo)"
            );
            report.outcomes.push(StepOutcome {
                name: name.clone(),
                outcome: UndoOutcome::NotRehydrated(e.to_string()),
            });
            reporter.undo_done(&name);
            continue;
        }

        info!(step = %name, "undo (da stato persistito)");
        let outcome = match step.undo(ctx) {
            Ok(()) => UndoOutcome::Undone,
            Err(e) => {
                warn!(
                    step = %name,
                    error = %e,
                    "undo fallito, proseguo con la pulizia (best-effort)"
                );
                UndoOutcome::Failed(e.to_string())
            }
        };
        reporter.undo_done(&name);
        report.outcomes.push(StepOutcome { name, outcome });
    }

    // La **promessa**, non il meccanismo: dopo tutti gli undo, `/opt/odoo` c'è
    // ancora? È una lettura, non una mutazione, e va fatta qui perché è l'unico
    // punto che vede il sistema a pulizia conclusa.
    //
    // In dry-run non si guarda: nessun undo ha rimosso nulla, quindi la
    // directory c'è per costruzione e segnalarla sarebbe un allarme garantito
    // che insegna a ignorare gli allarmi.
    if !ctx.dry_run {
        let ops = make_ops();
        if ops.path_exists(&ctx.odoo_home) {
            warn!(
                home = %ctx.odoo_home.display(),
                "rollback concluso ma la home esiste ancora: contiene qualcosa che non \
                 abbiamo creato noi, e non la rimuoviamo (mai un rm -rf su roba altrui)"
            );
            report.home_left_behind = Some(ctx.odoo_home.clone());
        }
    }

    report
}
