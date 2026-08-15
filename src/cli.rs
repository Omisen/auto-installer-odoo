//! command-line arguments (`clap`, derive).
//!
//! every overridable parameter is an `Option<T>`: `Some` means the user passed
//! it explicitly, `None` that they did not. that is what makes the CLI → `.env`
//! → interactive → default cascade possible (see [`crate::config`]), replacing
//! Bash's `CLI_*_SET` booleans with something typed.
//!
//! the subcommand is itself an `Option`, so `invok` with no subcommand still
//! installs — the documented, unchanged behaviour.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// the Odoo installer with surgical rollback.
#[derive(Parser, Debug)]
#[command(
    name = "invok",
    about = "Odoo installer (16/17/18/19) with surgical rollback",
    version = crate::INSTALLER_VERSION,
    // no automatic `--version`: here that flag is Odoo's version. asking for
    // the installer's own is `installer_version` below (A-V3-16).
    disable_version_flag = true
)]
pub struct Cli {
    /// subcommand; absent means install, the historical behaviour.
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Odoo version to install: 16|17|18|19, or 16.0..19.0.
    #[arg(long, value_name = "VERSION")]
    pub version: Option<String>,

    /// name of this instance, for running more than one Odoo on one machine.
    ///
    /// it becomes the name of the unit, the install dir, the system user, the
    /// PostgreSQL role, the database and the `odoo` helper — all prefixed
    /// `odoo-`. absent means the historical, unnamed instance, whose names are
    /// unchanged (`odoo18`, user `odoo`, database `odoo`).
    ///
    /// lowercase letters, digits, `-` and `_`; must start with a letter and stay
    /// within 26 characters (see [`crate::instance`]).
    #[arg(long, value_name = "NAME")]
    pub instance: Option<String>,

    /// print the **installer's** version and exit.
    ///
    /// spelled out rather than `--version`, which already means Odoo's version:
    /// renaming that would break scripts in the field. `-V` stays the short
    /// form everyone tries first.
    #[arg(short = 'V', long = "installer-version", action = clap::ArgAction::Version)]
    pub installer_version: Option<bool>,

    /// system user for Odoo; defaults to `odoo`.
    #[arg(long, value_name = "USER")]
    pub odoo_user: Option<String>,

    /// PostgreSQL role; defaults to `--odoo-user`.
    #[arg(long, value_name = "USER")]
    pub db_user: Option<String>,

    /// password of the PostgreSQL role; empty or absent means peer auth.
    #[arg(long, value_name = "PASS")]
    pub db_password: Option<String>,

    /// Odoo's HTTP port; defaults to 8069.
    #[arg(long, value_name = "PORT")]
    pub port: Option<u16>,

    /// database name; defaults to `odoo`.
    #[arg(long, value_name = "NAME")]
    pub db_name: Option<String>,

    /// install directory; must live under /opt/odoo, and defaults to
    /// /opt/odoo/odoo<version>.
    #[arg(long, value_name = "DIR")]
    pub install_dir: Option<PathBuf>,

    /// Odoo master password; `admin` needs an explicit interactive
    /// confirmation.
    #[arg(long, value_name = "PASS")]
    pub admin_passwd: Option<String>,

    /// configure Nginx as a reverse proxy.
    #[arg(long)]
    pub with_nginx: bool,

    /// `server_name` for the Nginx vhost; defaults to the `_` catch-all.
    #[arg(long, value_name = "NAME")]
    pub server_name: Option<String>,

    /// open port 443 on the firewall, ahead of TLS.
    ///
    /// **does not configure TLS**: the generated vhost listens on 80 only.
    /// certificates and the 443 block come from `certbot --nginx`, which
    /// rewrites the vhost itself.
    ///
    /// formerly `--enable-ssl`, a name that promised what it did not do
    /// (A-V3-6); the old one is still accepted.
    #[arg(long, alias = "enable-ssl")]
    pub open_https_port: bool,

    /// Odoo's log file; absent means journal/stdout.
    #[arg(long, value_name = "FILE")]
    pub logfile: Option<PathBuf>,

    /// load variables from a `.env` file, parsed declaratively and never run.
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// resolve the config and print the plan without touching the system.
    #[arg(long)]
    pub dry_run: bool,

    /// on rollback, also purge the common utilities and the heavy packages that
    /// would otherwise stay installed.
    #[arg(long)]
    pub aggressive_rollback: bool,

    /// install even when a manifest exists, archiving it instead of
    /// overwriting.
    ///
    /// the previous manifest is renamed, never deleted: it is the only record
    /// of what that installation created.
    #[arg(long)]
    pub force: bool,
}

/// the available subcommands.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// undo an installation from the persisted state: remove what the installer
    /// created, leave what was already on the machine.
    ///
    /// serves both to uninstall a working instance and to clean up after an
    /// interrupted run.
    #[command(alias = "uninstall")]
    Rollback(RollbackArgs),
}

/// options for `invok rollback`.
#[derive(Args, Debug)]
pub struct RollbackArgs {
    /// state file to consume; defaults to /var/lib/invok/state.json, falling
    /// back to the historical /opt/odoo/.installer-state.json.
    #[arg(long, value_name = "FILE")]
    pub state: Option<PathBuf>,

    /// list what would be undone without touching the system.
    #[arg(long)]
    pub dry_run: bool,

    /// also purge PostgreSQL and Nginx if we installed them, plus the common
    /// utilities. by default stop and disable are enough.
    #[arg(long)]
    pub aggressive_rollback: bool,

    /// skip the confirmation. required without a TTY, where the command
    /// otherwise stops rather than proceed blindly on a destructive operation.
    #[arg(short = 'y', long)]
    pub yes: bool,
}
