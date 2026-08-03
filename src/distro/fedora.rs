//! Le convenzioni della famiglia Fedora.

use std::path::PathBuf;

use super::{firewalld::Firewalld, Distro, Firewall, NginxLayout};
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

    /// `conf.d/*.conf`, e **nessun** default site come file separato.
    ///
    /// # La conseguenza va dichiarata, non nascosta
    ///
    /// Su Fedora il server di default di nginx è un blocco `server { listen 80
    /// default_server; ... }` dentro `/etc/nginx/nginx.conf`. Non è un file che
    /// si possa spostare: toglierlo significherebbe **riscrivere la
    /// configurazione principale** di un servizio del cliente.
    ///
    /// Perciò qui `default_site` è `None` e lo step `nginx-enable-site` non fa
    /// nulla. L'effetto pratico: su una Fedora con nginx appena installato, una
    /// richiesta a un hostname che non combacia con `NGINX_SERVER_NAME` riceve
    /// ancora la pagina di benvenuto invece di Odoo.
    ///
    /// È la scelta **più prudente delle due**. L'alternativa — modificare
    /// `nginx.conf` — è esattamente la classe di mutazione che A-V3-5 ha
    /// insegnato a trattare con la massima cautela: lì un `bool` al posto della
    /// natura del file è costato la distruzione di configurazione del cliente.
    fn nginx_layout(&self) -> NginxLayout {
        NginxLayout {
            vhost_dir: PathBuf::from("/etc/nginx/conf.d"),
            // `nginx.conf` include `conf.d/*.conf`: **solo** i `.conf`. Un vhost
            // senza estensione sarebbe invisibile, e nulla lo direbbe — nginx
            // partirebbe, il reload riuscirebbe, e Odoo non sarebbe raggiungibile.
            vhost_extension: ".conf",
            enabled_dir: None,
            default_site: None,
            default_site_standard_target: None,
            default_site_backup_dir: PathBuf::from("/etc/nginx"),
        }
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
