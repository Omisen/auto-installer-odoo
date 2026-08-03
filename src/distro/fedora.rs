//! Le convenzioni della famiglia Fedora.

use std::path::PathBuf;

use super::{firewalld::Firewalld, Distro, Firewall};
use crate::error::StepError;
use crate::system_ops::run_command;

/// Il PGDATA di default su Fedora.
pub const POSTGRES_DATA_DIR: &str = "/var/lib/pgsql/data";

/// Fedora: firewall `firewalld`.
#[derive(Debug, Default)]
pub struct Fedora {
    firewall: Firewalld,
}

impl Fedora {
    pub const fn new() -> Self {
        Fedora {
            firewall: Firewalld,
        }
    }
}

impl Distro for Fedora {
    fn firewall(&self) -> &dyn Firewall {
        &self.firewall
    }

    /// `/var/lib/pgsql/data`, il PGDATA di default di Fedora.
    ///
    /// È una **costante del codice**, non un valore che arriva dal manifesto: e
    /// conta, perché l'undo può arrivare a rimuoverla ricorsivamente. Un percorso
    /// che venisse da un file di stato sarebbe un dato non fidato, con tutto ciò
    /// che A-V3-8 ha insegnato.
    fn postgres_data_dir(&self) -> Option<PathBuf> {
        Some(PathBuf::from(POSTGRES_DATA_DIR))
    }

    /// `postgresql-setup --initdb`: crea il cluster che il pacchetto non crea.
    ///
    /// È una **mutazione**, e produce un artefatto (il data directory) che
    /// `setup-postgres` registra con un `PreState` proprio — altrimenti sarebbe
    /// qualcosa che nasce senza che nessuno lo annoti, cioè non annullabile
    /// (la lezione di A-R5-3 applicata al cluster).
    fn init_postgres_cluster(&self) -> Result<(), StepError> {
        run_command("postgresql-setup", &["--initdb"])
    }
}
