//! Errori tipizzati del motore e degli step.
//!
//! Ogni step ritorna un [`StepError`] (dominio, tipizzato con `thiserror`); a
//! livello `main` questi vengono promossi ad `anyhow::Error`. Le varianti sono
//! volutamente generiche in Fase 0 ed estendibili fase per fase, ma ognuna deve
//! sempre portare contesto sufficiente per un post-mortem su macchina cliente
//! (nome comando, exit status, path coinvolto).

use std::path::PathBuf;

use thiserror::Error;

/// Errore di dominio prodotto da uno step (snapshot / run / undo).
#[derive(Debug, Error)]
pub enum StepError {
    /// Un comando esterno (git/apt/psql/systemctl/...) è terminato con errore.
    #[error("comando `{command}` fallito (exit {status}): {stderr}")]
    CommandFailed {
        command: String,
        status: String,
        stderr: String,
    },

    /// Un comando esterno non è terminato entro il timeout ed è stato ucciso.
    ///
    /// Riguarda le sole operazioni di **rete** (clone, tarball, download del
    /// `.deb`): un mirror che non chiude la connessione appenderebbe altrimenti
    /// l'installer a tempo indefinito. È un errore **ritentabile**: il retry del
    /// clone lo tratta come un fallimento qualsiasi e passa al tentativo
    /// successivo, poi al fallback tarball.
    ///
    /// Il nome della variabile citato nel messaggio è
    /// [`crate::system_ops::NETWORK_TIMEOUT_ENV`]; un test verifica che i due
    /// non divergano.
    #[error(
        "comando `{command}` non terminato entro {secs}s: interrotto (timeout di rete). \
         Se la connessione è lenta alza il limite con ODOO_NETWORK_TIMEOUT_SECS=<secondi> \
         (0 = nessun timeout)"
    )]
    Timeout { command: String, secs: u64 },

    /// La fase di snapshot non è riuscita a rilevare lo stato preesistente.
    ///
    /// È un errore grave: senza snapshot affidabile l'undo non ha una fonte di
    /// verità (`PreState`) e il rollback non è sicuro.
    #[error("snapshot fallito per lo step `{step}`: {reason}")]
    SnapshotFailed { step: String, reason: String },

    /// Errore di I/O con il path coinvolto per la diagnostica.
    #[error("errore di I/O su `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Una precondizione non negoziabile è stata violata (es. hard-stop su
    /// init di un DB preesistente). Non è recuperabile: l'installer si ferma.
    #[error("precondizione violata: {0}")]
    Precondition(String),

    /// Il build di gevent è fallito su un interprete più recente di quelli per
    /// cui Odoo pinna (A-MD-7).
    ///
    /// Non è una variante «di comodo» per un caso particolare: è un fallimento
    /// che **non si può diagnosticare dal messaggio del comando**. Chi legge
    /// vede trecento righe di `gcc` che parlano di `_PyLong_AsByteArray`, e la
    /// causa vera — «questa versione di Odoo non ha un pin per questo Python» —
    /// non compare da nessuna parte. Qui la diagnosi precede l'errore originale,
    /// che resta per intero: aggiungere una spiegazione non è togliere una
    /// prova.
    #[error("{diagnosis}\n\n--- errore originale ---\n{original}")]
    PythonTooNew { diagnosis: String, original: String },
}

impl StepError {
    /// Costruttore comodo per errori di I/O che allega il path coinvolto.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        StepError::Io {
            path: path.into(),
            source,
        }
    }
}
