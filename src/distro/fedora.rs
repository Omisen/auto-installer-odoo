//! the Fedora family's conventions.

use std::path::PathBuf;

use super::{firewalld::Firewalld, Distro, Firewall, NginxLayout, Selinux};
use crate::error::StepError;
use crate::system_ops::{capture_command, run_command};

/// Fedora's default PGDATA.
pub const POSTGRES_DATA_DIR: &str = "/var/lib/pgsql/data";

/// Fedora: `firewalld`, with SELinux enforcing.
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

/// SELinux on Fedora.
#[derive(Debug, Default)]
pub struct FedoraSelinux;

impl Selinux for FedoraSelinux {
    /// `httpd_can_network_connect`: without it SELinux denies nginx the
    /// connection to Odoo and the proxy answers **502**, with a valid `nginx
    /// -t` and a successful reload. confirmed with `ausearch`.
    fn nginx_proxy_boolean(&self) -> &'static str {
        "httpd_can_network_connect"
    }

    /// `getsebool <name>` → `name --> on|off`.
    ///
    /// `None` when the command is not executable, i.e. SELinux disabled or the
    /// tools missing. **not "off"** but "unknown", and the step does not touch
    /// the policy of a system it cannot question.
    fn is_enabled(&self, boolean: &str) -> Option<bool> {
        let out = capture_command("getsebool", &[boolean]).ok()?;
        let state = out.split("-->").nth(1)?.trim();
        match state {
            "on" => Some(true),
            "off" => Some(false),
            _ => None,
        }
    }

    /// `setsebool -P`: **persistent**, surviving a reboot, which is why it is a
    /// recorded artifact and not an incidental command.
    fn set(&self, boolean: &str, value: bool) -> Result<(), StepError> {
        let value = if value { "on" } else { "off" };
        run_command("setsebool", &["-P", boolean, value])
    }
}

impl Distro for Fedora {
    fn firewall(&self) -> &dyn Firewall {
        &self.firewall
    }

    /// `conf.d/*.conf`, and **no** default site as a separate file.
    ///
    /// here nginx's default server is a `server` block inside `nginx.conf`, not
    /// a file that can be moved aside: removing it would mean **rewriting the
    /// main configuration** of a customer's service. so `default_site` is
    /// `None` and `nginx-enable-site` does nothing.
    ///
    /// the declared consequence: on a freshly installed Fedora nginx, a request
    /// to a hostname that does not match `NGINX_SERVER_NAME` still gets the
    /// welcome page. the prudent choice of the two — rewriting `nginx.conf` is
    /// the very class of mutation A-V3-5 taught us to handle with care.
    fn nginx_layout(&self) -> NginxLayout {
        NginxLayout {
            vhost_dir: PathBuf::from("/etc/nginx/conf.d"),
            // `nginx.conf` includes **only** `conf.d/*.conf`. a vhost without
            // the extension would be invisible with nothing to say so: nginx
            // would start, the reload would succeed, Odoo would be unreachable.
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

    /// Fedora's default PGDATA.
    ///
    /// a **constant in the code**, not a value from the manifest, and that
    /// matters because the undo may remove it recursively — a path arriving
    /// from a state file is untrusted data (A-V3-8).
    fn postgres_data_dir(&self) -> Option<PathBuf> {
        Some(PathBuf::from(POSTGRES_DATA_DIR))
    }

    /// the PGDATA the `postgresql.service` unit declares, when readable.
    ///
    /// the same source `postgresql-setup` consults, with drop-ins already
    /// applied by systemd. asking exactly what it asks is the point: another
    /// source would answer another question.
    ///
    /// `None` when the unit does not exist or `systemctl` does not answer:
    /// cases where we do not know, not cases where we know there is none.
    fn declared_postgres_data_dir(&self) -> Option<PathBuf> {
        let out = capture_command(
            "systemctl",
            &["show", "-p", "Environment", "postgresql.service"],
        )
        .ok()?;
        pgdata_from_environment(&out)
    }

    /// `postgresql-setup --initdb`: creates the cluster the package does not.
    ///
    /// a **mutation** producing an artifact — the data directory — which
    /// `setup-postgres` records with a `PreState` of its own. otherwise it
    /// would come into existence unrecorded, and therefore un-undoable.
    fn init_postgres_cluster(&self) -> Result<(), StepError> {
        run_command("postgresql-setup", &["--initdb"])
    }
}

/// extracts `PGDATA` from a `systemctl show` `Environment=` line.
///
/// pure, against a fixture taken from the real command: one line carrying
/// several space-separated assignments. `None` means "PGDATA is not in this
/// output", not "it does not exist".
pub fn pgdata_from_environment(output: &str) -> Option<PathBuf> {
    output
        .lines()
        .filter_map(|line| line.strip_prefix("Environment="))
        .flat_map(|env| env.split_whitespace())
        .filter_map(|assegnazione| assegnazione.strip_prefix("PGDATA="))
        // last one wins, as in systemd: a drop-in redefining the variable comes
        // AFTER the base unit's.
        .next_back()
        .map(PathBuf::from)
}
