//! Le convenzioni della famiglia Debian (Debian e Ubuntu).

use std::path::PathBuf;

use super::{ufw::Ufw, Distro, Firewall};
use crate::error::StepError;

/// Debian/Ubuntu: firewall `ufw`.
#[derive(Debug, Default)]
pub struct Debian {
    firewall: Ufw,
}

impl Debian {
    pub const fn new() -> Self {
        Debian { firewall: Ufw }
    }
}

impl Distro for Debian {
    fn firewall(&self) -> &dyn Firewall {
        &self.firewall
    }

    /// `None`: il postinst del pacchetto `postgresql` chiama `pg_createcluster` e
    /// avvia il servizio. Non c'è nulla da inizializzare, e non c'è nessun
    /// artefatto in più da registrare.
    fn postgres_data_dir(&self) -> Option<PathBuf> {
        None
    }

    /// No-op, e non per pigrizia: su questa famiglia il cluster **esiste già**
    /// quando il pacchetto è installato. Vedi [`Self::postgres_data_dir`].
    fn init_postgres_cluster(&self) -> Result<(), StepError> {
        Ok(())
    }
}
