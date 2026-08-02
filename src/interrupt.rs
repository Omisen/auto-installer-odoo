//! Interruzione dall'esterno: Ctrl-C e `kill` gestiti invece che subiti (B-V3-5).
//!
//! # Il problema, e perché è più piccolo di quanto sembri
//!
//! Fino alla R18 l'azione di default di `SIGINT` valeva anche per noi: un Ctrl-C
//! **uccideva il processo all'istante**, quindi il rollback in-process non
//! partiva mai e il sistema restava a metà. Al suo posto R4 aveva messo un
//! avviso stampato prima delle mutazioni, che indirizzava a
//! `odoo-installer rollback` — utile, ma scorreva via nel log molto prima di
//! servire.
//!
//! Il punto che rende la correzione semplice: **il segnale va a tutto il process
//! group**, quindi lo ricevono anche i figli — `apt`, `git`, `pip`. Quelli
//! muoiono comunque. A noi basta *sopravvivere*: il comando figlio risulta
//! fallito, e per quel caso il motore ha già il rollback. L'handler non deve
//! quindi fare niente di complicato — alza un flag, e basta.
//!
//! # Perché nessun `unsafe`
//!
//! Un handler scritto a mano dev'essere async-signal-safe, e in Rust si scrive
//! con `unsafe`. `signal_hook` incapsula quella parte dietro un'API safe, ed è
//! già nell'albero delle dipendenze (transitiva di `crossterm`, via `inquire`):
//! usarlo costa **zero** dipendenze nuove e toglie l'`unsafe` da un programma
//! che gira come root.
//!
//! # Il secondo Ctrl-C esce davvero
//!
//! Chi preme Ctrl-C una seconda volta vuole andarsene, e un rollback può durare
//! minuti. `register_conditional_shutdown` esce con 130 (la convenzione shell
//! per «terminato da SIGINT») **se il flag è già alzato**: registrato per primo,
//! vede il flag ancora falso al primo segnale e lascia proseguire. L'ordine di
//! registrazione è quindi parte del comportamento, non un dettaglio.
//!
//! In quel caso il sistema resta a metà per davvero — ed è una scelta
//! dell'utente, non un difetto: il manifesto è sul disco e
//! `odoo-installer rollback` lo ripulisce.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use tracing::warn;

use crate::error::StepError;

/// Codice d'uscita convenzionale per «terminato da SIGINT» (128 + 2).
const EXIT_SIGINT: i32 = 130;

/// Registra gli handler e ritorna il flag che il motore osserva.
///
/// Copre `SIGINT` (Ctrl-C) e `SIGTERM` (`kill`, `systemctl stop`, spegnimento):
/// sono due modi diversi di chiedere la stessa cosa, e trattarne solo uno
/// lascerebbe scoperto proprio il caso non presidiato — la macchina che si
/// spegne mentre l'installazione è in corso.
///
/// Un fallimento nella registrazione **non** è fatale: si prosegue senza la
/// gestione, che è esattamente com'era prima. Impedire un'installazione perché
/// non si è potuto installare un handler sarebbe sproporzionato.
pub fn install() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));

    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        // L'ordine conta: prima l'uscita condizionata (che agisce solo se il
        // flag è GIÀ alzato, quindi dalla seconda volta in poi), poi quella che
        // lo alza. Invertendoli, il primo Ctrl-C ucciderebbe il processo.
        if let Err(e) =
            signal_hook::flag::register_conditional_shutdown(signal, EXIT_SIGINT, Arc::clone(&flag))
        {
            warn!(signal, error = %e, "impossibile registrare l'uscita al secondo segnale");
        }
        if let Err(e) = signal_hook::flag::register(signal, Arc::clone(&flag)) {
            warn!(
                signal,
                error = %e,
                "impossibile gestire questo segnale: un'interruzione ucciderà il processo \
                 senza annullare nulla (usa `odoo-installer rollback` per ripulire)"
            );
        }
    }

    flag
}

/// Errore da usare quando l'esecuzione si ferma per un'interruzione.
///
/// È un `StepError::Precondition` e non una variante propria per una ragione:
/// non è un difetto di uno step, è una decisione presa da fuori. Il messaggio
/// dice cosa è successo **e cosa è stato fatto di conseguenza**, perché la
/// domanda che si fa chi ha appena premuto Ctrl-C è «e adesso il sistema com'è?».
pub fn interrupted_error() -> StepError {
    StepError::Precondition(
        "installazione interrotta su richiesta (Ctrl-C o segnale di terminazione).\n\
         Gli step già eseguiti vengono annullati: al termine il sistema sarà come prima.\n\
         Un secondo Ctrl-C esce subito — in quel caso il sistema resta a metà e si \
         ripulisce con `sudo odoo-installer rollback`."
            .to_string(),
    )
}
