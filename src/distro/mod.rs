//! the distribution **family**, and the rule that it is re-read rather than
//! re-derived.
//!
//! a raw `os-release` `ID` answers "which distribution", not "which commands
//! install and remove a package here" — and that question is about the family
//! `ubuntu` and `debian` share, not about either of them.
//!
//! # the rule this type exists for
//!
//! **the family is RE-READ from the manifest, never re-derived from the
//! system.**
//!
//! the same rule that copies "the database was ours" into `setup-data-dir`'s
//! snapshot instead of recomputing it, and that keeps the rollback from
//! re-running `snapshot()`: information that existed at mutation time must not
//! be inferred again afterwards, because the system has changed — or is not
//! even the same system.
//!
//! the case to avoid is the rollback from disk, whose `Context` has `os_info:
//! None`. that was harmless while no undo depended on the OS; with two package
//! managers the package delta's undo would not know which command to invoke.
//!
//! both alternatives were weighed and dropped because they **infer**:
//! re-detecting the OS at rollback time (the manifest may describe another
//! machine), and detecting the manager from which binary exists (a machine with
//! both would answer wrongly, in silence).

pub mod debian;
pub mod fedora;
pub mod firewalld;
pub mod ufw;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::StepError;

/// the **distribution conventions** boundary, second of the two.
///
/// abstracts what differs between families *without* being packaging: where
/// files live, which concepts exist, which tool governs the firewall. obtained
/// from [`SystemOps::distro`](crate::system_ops::SystemOps::distro), so it is
/// not a second door onto the system.
///
/// separate from [`crate::packaging`] because the divergences are of a
/// different **nature**: "which command installs a package" and "which
/// directory nginx reads vhosts from" have nothing in common but their
/// dependence on the distribution, and merging them would abstract two
/// different things at once. where this family keeps things. **data**, not
/// behaviour.
///
/// `Option` rather than different strings because two of the divergences are
/// not "another path" but "**the concept does not exist**": on Fedora there is
/// no `sites-enabled` under another name, and the default server is a block
/// inside `nginx.conf`. a constant pointing at an invented directory would lie,
/// and the step would create symlinks somewhere nginx never reads.
#[derive(Debug, Clone)]
pub struct NginxLayout {
    /// where the vhost is written.
    pub vhost_dir: PathBuf,
    /// the extension the file needs in order to be loaded.
    ///
    /// empty on Debian, where `sites-enabled/*` includes any file; `.conf` on
    /// Fedora, where **only** those are included. a vhost without it would be
    /// invisible there, with nothing to say so.
    pub vhost_extension: &'static str,
    /// the **enabled** sites directory, where the concept exists.
    pub enabled_dir: Option<PathBuf>,
    /// the default site as a separate file, where the concept exists.
    pub default_site: Option<PathBuf>,
    /// the *distribution-standard* target of the default site.
    ///
    /// **only** a fallback for states persisted before R11, which recorded its
    /// existence but not its target.
    pub default_site_standard_target: Option<PathBuf>,
    /// where the backup of a **regular file** default site goes.
    ///
    /// deliberately outside `enabled_dir`: nginx globs that directory, so a
    /// backup left there would still be loaded and port 80 would stay occupied
    /// — the same defect under another name.
    pub default_site_backup_dir: PathBuf,
}

impl NginxLayout {
    /// the vhost path for this instance.
    ///
    /// `base` is [`crate::context::Context::artifact_base`] — `odoo18` for the
    /// unnamed instance, which is the name every release so far wrote, and
    /// `odoo-<name>` for a named one. it takes the finished name rather than the
    /// version so that two instances of the **same** version get two vhosts.
    pub fn vhost_path(&self, base: &str) -> PathBuf {
        self.vhost_dir
            .join(format!("{base}{}", self.vhost_extension))
    }

    /// the enabling symlink's path, where the family has the concept.
    pub fn enabled_link(&self, base: &str) -> Option<PathBuf> {
        self.enabled_dir
            .as_ref()
            .map(|dir| dir.join(format!("{base}{}", self.vhost_extension)))
    }
}

pub trait Distro {
    /// this family's firewall tool.
    fn firewall(&self) -> &dyn Firewall;

    /// where this family keeps its nginx configuration.
    fn nginx_layout(&self) -> NginxLayout;

    /// SELinux, **if** this family has it. `None` means the concept does not
    /// exist.
    ///
    /// an `Option` rather than three methods that do nothing on half the
    /// families: where SELinux is absent the trait is not implemented at all,
    /// so no unreachable branch is left behind.
    fn selinux(&self) -> Option<&dyn Selinux>;

    /// where the PostgreSQL cluster lives, **if** this family needs it
    /// initialised by hand. `None` means the package creates and starts it.
    ///
    /// the heaviest divergence between the families: on Debian the postinst
    /// runs `pg_createcluster` and the service comes up, while on Fedora
    /// `postgresql-server` **initialises nothing** and the service would fail
    /// its final check without saying why.
    ///
    /// an `Option` rather than an empty string: "this family lacks the concept"
    /// is a different answer from "the path is this one".
    fn postgres_data_dir(&self) -> Option<PathBuf>;

    /// the PGDATA the **service declares**, when it can be read.
    ///
    /// A-MD-6. [`Self::postgres_data_dir`] is what *we* know: a constant, and
    /// it must stay one because it drives a `remove_dir_all`. but the cluster
    /// is initialised by `postgresql-setup`, which takes PGDATA **from the
    /// unit** — and an administrator can move it with a drop-in.
    ///
    /// when they diverge the damage is not theoretical: `initdb` would create
    /// the cluster where the unit says, while an aggressive undo would remove
    /// the constant — a directory we never created, and on such a machine
    /// exactly where a previous customer cluster may live.
    ///
    /// so this exists to **refuse**, not to choose: the path read here drives
    /// no removal. `None` means "unknown", and nothing is concluded from it.
    fn declared_postgres_data_dir(&self) -> Option<PathBuf> {
        None
    }

    /// initialises the PostgreSQL cluster.
    ///
    /// called **only** when [`Self::postgres_data_dir`] is `Some` and the
    /// cluster is absent. on families that do not need it this is a total no-op
    /// — not an unreachable branch, but the true answer to "what needs doing
    /// here?".
    fn init_postgres_cluster(&self) -> Result<(), StepError>;
}

/// the firewall tool, in five questions.
///
/// the **rule token is the same** on both families — `ufw allow 80/tcp` and
/// `firewall-cmd --add-port=80/tcp` take and list the same string — which is
/// why the `nginx-firewall` step, and with it the delta protection, does not
/// change a line when the tool underneath does.
///
/// a trait and not a set of constants because what differs are the **commands**
/// and their model: firewalld separates runtime from permanent and has zones,
/// which a constant could not express.
pub trait Firewall {
    /// this tool's name, for the messages.
    ///
    /// "ufw not found" said on Fedora is the same class of error as telling a
    /// dnf user to run `apt-get update`: it sends them looking for something
    /// that does not exist on their machine. observed in the field.
    fn name(&self) -> &'static str;

    /// is the tool installed?
    fn available(&self) -> bool;
    /// is the tool **active**? if not, we do not touch the firewall.
    fn is_active(&self) -> bool;
    /// is the rule already there? if so it is not ours, and the undo leaves it.
    fn rule_exists(&self, rule: &str) -> Result<bool, StepError>;
    /// opens the rule.
    fn allow(&self, rule: &str) -> Result<(), StepError>;
    /// closes the rule again. called **only** on the delta.
    fn delete(&self, rule: &str) -> Result<(), StepError>;
}

/// SELinux, for the one thing that concerns us: **letting the proxy through**.
///
/// on Fedora SELinux is enforcing and denies nginx a connection to a local
/// service on an unreserved port:
///
/// ```text
/// avc: denied { name_connect } for comm="nginx" dest=8069
///      scontext=httpd_t tcontext=unreserved_port_t permissive=0
/// ```
///
/// the vhost is correct, `nginx -t` passes, the reload succeeds — and `curl`
/// answers **502**. a defect with no symptom *in the installer's logs*, right
/// up to the first user who opens a browser.
///
/// `setsebool -P` writes the policy **persistently**, so it is a system
/// artifact like any other and carries a `PreState`. an already-enabled
/// boolean — common on a machine hosting other web services — is
/// `Preexisting`, and turning it off would break somebody else's proxy.
pub trait Selinux {
    /// the boolean that lets nginx proxy to a local service.
    fn nginx_proxy_boolean(&self) -> &'static str;

    /// is the boolean on? `None` when SELinux cannot be queried, which differs
    /// from "off" and leads to touching nothing.
    fn is_enabled(&self, boolean: &str) -> Option<bool>;

    /// sets the boolean **persistently** (`setsebool -P`).
    fn set(&self, boolean: &str, value: bool) -> Result<(), StepError>;
}

/// the distribution family this system belongs to.
///
/// groups the distributions that share a package manager and conventions.
/// **not** the distribution: `ubuntu` and `debian` are two `ID`s of one family,
/// and where the difference matters — version thresholds, the wkhtmltopdf
/// package suffix — the `id` is still what is consulted.
///
/// the `Debian` default is manifest compatibility, not convenience: a state
/// written before this field existed does not declare one, and **every existing
/// installation is apt**. making a manifest unreadable would make an instance
/// un-uninstallable.
///
/// so the default cannot become a silent lie, the rollback **logs** the family
/// it is working with and compares it against the system (see
/// [`family_mismatch`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum OsFamily {
    /// Debian and derivatives: `apt`/`dpkg`, `sites-available`, `ufw`.
    #[default]
    Debian,
    /// Fedora: `dnf`/`rpm`, `conf.d`, `firewalld`.
    Fedora,
}

impl OsFamily {
    /// derives the family from the `ID` declared in `/etc/os-release`.
    ///
    /// `None` means a distribution we do not know how to handle. the **only**
    /// place that decision is taken, so it cannot live in two diverging spots.
    ///
    /// `ID_LIKE` is deliberately not read: Rocky, AlmaLinux and CentOS Stream
    /// declare `ID_LIKE=fedora`, and honouring it would let them in **without
    /// anyone ever having tried them**. for a new family we start closed.
    ///
    /// no contradiction with A5.1-bis, which was about not rejecting a *newer*
    /// release of an already supported family — those thresholds stay open
    /// upwards. this is a different distribution, with no evidence either way.
    pub fn from_os_id(id: &str) -> Option<Self> {
        match id {
            "ubuntu" | "debian" => Some(OsFamily::Debian),
            "fedora" => Some(OsFamily::Fedora),
            _ => None,
        }
    }

    /// the family's stable name, used in the logs and in the manifest.
    pub fn as_str(&self) -> &'static str {
        match self {
            OsFamily::Debian => "debian",
            OsFamily::Fedora => "fedora",
        }
    }
}

impl std::fmt::Display for OsFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// do the manifest and the system underneath agree on the family?
///
/// returns the warning text when they do **not**, and `None` when they agree or
/// the system cannot be identified.
///
/// a warning and not a refusal: a pre-2.3 manifest falls back to `Debian`
/// (almost always the truth), and `--state` accepts any path, so one machine
/// can inspect another's manifest. refusing would make an instance
/// un-uninstallable — A-V3-1's damage by another route — while inferring from
/// the system would break this module's rule. in practice the case degrades on
/// its own: a purge on a system without that manager fails, the undo is
/// best-effort, and the leftovers reach the report.
///
/// pure, with the system as a **parameter**: the interesting case is not
/// reproducible on the machine running the tests.
pub fn family_mismatch(recorded: OsFamily, detected: Option<OsFamily>) -> Option<String> {
    let detected = detected?;
    if detected == recorded {
        return None;
    }
    Some(format!(
        "the manifest was written by an installation on '{recorded}', but this system is \
         '{detected}'. proceeding with '{recorded}', which is what the manifest records: the \
         artifacts to remove are its own, not the ones this machine would suggest. if the \
         removal commands fail, the report will list what is left."
    ))
}
