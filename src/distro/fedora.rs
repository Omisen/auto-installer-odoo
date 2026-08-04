//! Le convenzioni della famiglia Fedora.

use std::path::PathBuf;

use super::{firewalld::Firewalld, Distro, Firewall, NginxLayout, Selinux};
use crate::error::StepError;
use crate::system_ops::{capture_command, run_command};

/// Il PGDATA di default su Fedora.
pub const POSTGRES_DATA_DIR: &str = "/var/lib/pgsql/data";

/// Fedora: firewall `firewalld`, politica SELinux in enforcing.
#[derive(Debug, Default)]
pub struct Fedora {
    firewall: Firewalld,
    selinux: FedoraSelinux,
}

impl Fedora {
    pub const fn new() -> Self {
        Fedora {
            firewall: Firewalld,
            selinux: FedoraSelinux,
        }
    }
}

/// SELinux su Fedora.
#[derive(Debug, Default)]
pub struct FedoraSelinux;

impl Selinux for FedoraSelinux {
    /// `httpd_can_network_connect`: senza, SELinux nega a nginx di connettersi a
    /// `127.0.0.1:8069` e il proxy risponde **502** — con `nginx -t` valido e il
    /// reload riuscito. Verificato in campo con `ausearch`.
    fn nginx_proxy_boolean(&self) -> &'static str {
        "httpd_can_network_connect"
    }

    /// `getsebool <nome>` → `nome --> on|off`.
    ///
    /// `None` se il comando non è eseguibile: SELinux disabilitato, o
    /// `policycoreutils` non installato. **Non è «spento»** — è «non lo so», e
    /// da lì non si conclude nulla: lo step non tocca la politica di un sistema
    /// che non sa interrogare.
    fn is_enabled(&self, boolean: &str) -> Option<bool> {
        let out = capture_command("getsebool", &[boolean]).ok()?;
        let stato = out.split("-->").nth(1)?.trim();
        match stato {
            "on" => Some(true),
            "off" => Some(false),
            _ => None,
        }
    }

    /// `setsebool -P`: **persistente**, sopravvive al riavvio. È per questo che
    /// è un artefatto da registrare e non un comando di contorno.
    fn set(&self, boolean: &str, value: bool) -> Result<(), StepError> {
        let valore = if value { "on" } else { "off" };
        run_command("setsebool", &["-P", boolean, valore])
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

    fn selinux(&self) -> Option<&dyn Selinux> {
        Some(&self.selinux)
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

    /// Il PGDATA dichiarato dalla unit `postgresql.service`, se leggibile.
    ///
    /// È la stessa fonte che consulta `postgresql-setup`: `systemctl show -p
    /// Environment postgresql.service`, cioè `Environment=` con i drop-in già
    /// applicati da systemd. Interrogare la stessa cosa che interroga lui è il
    /// punto — un'altra fonte risponderebbe a un'altra domanda.
    ///
    /// `None` quando la unit non esiste (pacchetto non ancora installato) o
    /// `systemctl` non risponde: casi in cui non sappiamo, non casi in cui
    /// sappiamo che non c'è.
    fn declared_postgres_data_dir(&self) -> Option<PathBuf> {
        let out = capture_command(
            "systemctl",
            &["show", "-p", "Environment", "postgresql.service"],
        )
        .ok()?;
        pgdata_from_environment(&out)
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

/// Estrae `PGDATA` da una riga `Environment=…` di `systemctl show`.
///
/// Pura, e con un fixture preso dal comando vero: il formato è
/// `Environment=PGDATA=/var/lib/pgsql/data PGOPTS=…`, cioè **una** riga con più
/// assegnazioni separate da spazi — la stessa che `postgresql-setup` spezza per
/// conto suo. Un `None` significa «in questo output PGDATA non c'è», non «non
/// esiste»: chi chiama non deve poter confondere le due cose.
pub fn pgdata_from_environment(output: &str) -> Option<PathBuf> {
    output
        .lines()
        .filter_map(|line| line.strip_prefix("Environment="))
        .flat_map(|env| env.split_whitespace())
        .filter_map(|assegnazione| assegnazione.strip_prefix("PGDATA="))
        // L'ultima vince, come in systemd: un drop-in che ridefinisce la
        // variabile viene DOPO quella della unit di base.
        .next_back()
        .map(PathBuf::from)
}
