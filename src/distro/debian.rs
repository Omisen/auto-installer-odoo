//! Le convenzioni della famiglia Debian (Debian e Ubuntu).

use std::path::PathBuf;

use super::{ufw::Ufw, Distro, Firewall, NginxLayout};
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

    /// Il modello `sites-available` + `sites-enabled` con il symlink, e un
    /// default site che è un file a sé.
    fn nginx_layout(&self) -> NginxLayout {
        NginxLayout {
            vhost_dir: PathBuf::from("/etc/nginx/sites-available"),
            // `nginx.conf` include `sites-enabled/*`: **ogni** file, non solo i
            // `.conf`. Nessuna estensione richiesta — ed è anche la ragione per
            // cui il backup del default site non può restare lì dentro.
            vhost_extension: "",
            enabled_dir: Some(PathBuf::from("/etc/nginx/sites-enabled")),
            default_site: Some(PathBuf::from("/etc/nginx/sites-enabled/default")),
            default_site_standard_target: Some(PathBuf::from("/etc/nginx/sites-available/default")),
            default_site_backup_dir: PathBuf::from("/etc/nginx"),
        }
    }

    /// `None`: su questa famiglia SELinux non è in uso (AppArmor non richiede
    /// nulla di equivalente per il proxy verso un servizio locale).
    fn selinux(&self) -> Option<&dyn super::Selinux> {
        None
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
