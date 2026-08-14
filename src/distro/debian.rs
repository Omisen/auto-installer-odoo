//! the Debian family's conventions, covering Debian and Ubuntu.

use std::path::PathBuf;

use super::{ufw::Ufw, Distro, Firewall, NginxLayout};
use crate::error::StepError;

/// Debian and Ubuntu: `ufw` firewall.
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

    /// the `sites-available` + `sites-enabled` model with a symlink, and a
    /// default site that is a file of its own.
    fn nginx_layout(&self) -> NginxLayout {
        NginxLayout {
            vhost_dir: PathBuf::from("/etc/nginx/sites-available"),
            // `nginx.conf` includes `sites-enabled/*`: **every** file, not only
            // `.conf` ones. no extension needed — and the reason the default
            // site's backup cannot stay in there.
            vhost_extension: "",
            enabled_dir: Some(PathBuf::from("/etc/nginx/sites-enabled")),
            default_site: Some(PathBuf::from("/etc/nginx/sites-enabled/default")),
            default_site_standard_target: Some(PathBuf::from("/etc/nginx/sites-available/default")),
            default_site_backup_dir: PathBuf::from("/etc/nginx"),
        }
    }

    /// `None`: SELinux is not in use here, and AppArmor needs no equivalent for
    /// proxying to a local service.
    fn selinux(&self) -> Option<&dyn super::Selinux> {
        None
    }

    /// `None`: the `postgresql` postinst runs `pg_createcluster` and starts the
    /// service, so there is nothing to initialise and no extra artifact to
    /// record.
    fn postgres_data_dir(&self) -> Option<PathBuf> {
        None
    }

    /// a no-op, and not out of laziness: here the cluster **already exists**
    /// once the package is installed.
    fn init_postgres_cluster(&self) -> Result<(), StepError> {
        Ok(())
    }
}
