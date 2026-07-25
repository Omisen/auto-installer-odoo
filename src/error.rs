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
