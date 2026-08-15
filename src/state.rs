//! the `PreState` pattern and state persistence.
//!
//! implements invariant 4 of `CLAUDE.md`: `completed` plus snapshots go to
//! `/var/lib/invok/state.json` (owned root, `0600`).
//!
//! three consumers read it back: `invok rollback`, a re-run deciding whether to
//! resume ([`start_decision`]), and an uninstall long after the fact.
//!
//! # why the state also carries the configuration
//!
//! a [`StepRecord`] says *what state* an artifact was in, not *which* artifact
//! it was. re-deriving the names from the CLI/`.env`/default cascade is not
//! safe: after `--db-name fatturazione`, a bare `invok rollback` would fall
//! back to `odoo` and drop a database it never created. **no password is
//! persisted**: no undo needs one, and an unwritten secret cannot leak.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::context::Context;
use crate::distro::OsFamily;
use crate::error::StepError;

/// manifest directory in production, removed when empty by
/// [`InstallState::clear`].
pub const DEFAULT_STATE_DIR: &str = "/var/lib/invok";

/// default state file path in production (root, `0600`).
///
/// **outside `/opt/odoo`** (A-V3-2): the manifest is the last artifact to die,
/// so inside that directory it kept it non-empty and `PrepareOptRoot`'s undo —
/// the last to run — always gave up on it.
pub const DEFAULT_STATE_PATH: &str = "/var/lib/invok/state.json";

/// historical manifest path, inside `/opt/odoo`, up to 2.1.0.
pub const LEGACY_STATE_PATH: &str = "/opt/odoo/.installer-state.json";

/// manifest path from 2.2.0 to 2.4.0, before the rename to `invok`.
pub const RENAMED_STATE_PATH: &str = "/var/lib/odoo-installer/state.json";

/// the historical paths, newest first: still **read**, never written.
///
/// customer machines are not renamed along with the repository, and the
/// manifest is the only record of what we created. dropping a path would leave
/// an instance nobody can uninstall without guessing. the next rename adds a
/// line here.
pub const LEGACY_STATE_PATHS: &[&str] = &[RENAMED_STATE_PATH, LEGACY_STATE_PATH];

/// is the state file a trustworthy source for a destructive operation?
///
/// it drives `rm -rf`, `dropdb` and `userdel`, so it must belong to root, not
/// be group- or world-writable, and sit in a directory third parties cannot
/// write (A-V3-8). not applied to `--dry-run`, which only prints.
///
/// # errors
///
/// [`StepError::Precondition`] naming what disqualified the file, or
/// [`StepError::Io`] when it cannot be stat'd.
pub fn ensure_trustworthy(path: &Path) -> Result<(), StepError> {
    use std::os::unix::fs::MetadataExt;

    let meta = fs::metadata(path).map_err(|e| StepError::io(path, e))?;
    let parent_mode = path
        .parent()
        .and_then(|p| fs::metadata(p).ok())
        .map(|m| m.mode());

    trust_verdict(meta.uid(), meta.mode(), parent_mode).map_err(|reason| {
        StepError::Precondition(format!(
            "the state file {} is not a trustworthy source: {reason}.\n\
             \n\
             it drives `rm -rf`, `dropdb` and `userdel`: it is not consumed from somewhere \
             another user could rewrite or replace.",
            path.display()
        ))
    })
}

/// [`ensure_trustworthy`]'s rule, over the numbers alone.
///
/// split out because the positive case — a root-owned `0600` file — cannot be
/// reproduced by an unprivileged test. same reason as
/// `checks::ensure_root_euid`.
pub fn trust_verdict(uid: u32, mode: u32, parent_mode: Option<u32>) -> Result<(), String> {
    if uid != 0 {
        return Err(format!("it does not belong to root (uid {uid})"));
    }
    if mode & 0o022 != 0 {
        return Err(format!(
            "it is writable by group or others (mode {:o})",
            mode & 0o777
        ));
    }
    // a world-writable directory lets the file be replaced without being
    // writable itself; the sticky bit removes that, so `/tmp` is fine.
    if let Some(dir_mode) = parent_mode {
        let sticky = dir_mode & 0o1000 != 0;
        if dir_mode & 0o022 != 0 && !sticky {
            return Err(format!(
                "it lives in a directory writable by third parties (mode {:o}), where anyone \
                 could replace it",
                dir_mode & 0o777
            ));
        }
    }
    Ok(())
}

/// picks the manifest to consume when the user passes no `--state`.
pub fn resolve_state_path() -> PathBuf {
    let legacy: Vec<&Path> = LEGACY_STATE_PATHS.iter().map(Path::new).collect();
    pick_state_path(Path::new(DEFAULT_STATE_PATH), &legacy)
}

/// [`resolve_state_path`]'s rule, with the paths as parameters so it can be
/// checked against fixtures instead of the test machine's filesystem.
///
/// current path if it exists, then each `legacy` one, then the current path
/// again so an error names where to look. no migration is attempted.
pub fn pick_state_path(current: &Path, legacy: &[&Path]) -> PathBuf {
    if current.exists() {
        return current.to_path_buf();
    }
    for candidato in legacy {
        if candidato.exists() {
            return candidato.to_path_buf();
        }
    }
    current.to_path_buf()
}

/// readable and writable by the owner only.
const STATE_FILE_MODE: u32 = 0o600;

/// an artifact's state before the installer touched it.
///
/// the only source of truth for the undo (invariant 1): `undo` acts **only**
/// when the step is completed and `PreState == CreatedByUs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PreState {
    /// `run()` has not executed yet → no undo.
    #[default]
    Untracked,
    /// it was there before us → the undo is a no-op, not ours to destroy.
    Preexisting,
    /// we created it → the undo removes it.
    CreatedByUs,
}

/// a successfully completed step, persisted to disk.
///
/// `snapshot` is opaque JSON: each step serialises its own `PreState`, and
/// [`crate::step::Step::rehydrate`] reads it back to rebuild the undo later.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepRecord {
    pub name: String,
    pub snapshot: serde_json::Value,
}

/// the installation's configuration, persisted alongside the records.
///
/// holds the **identity of the artifacts** the undos must name: which user,
/// which database, which directory. deliberately holds **no password**: no undo
/// needs one, since roles and databases are dropped as `postgres`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallConfig {
    pub odoo_version: String,
    pub odoo_version_short: String,
    /// name of this instance, or `None` for the historical unnamed one.
    ///
    /// persisted because every other name derives from it: the unit to disable,
    /// the vhost to remove, the config file to delete. re-deriving it at
    /// rollback time would mean guessing, and the rule here is the same one that
    /// governs ownership and the OS family — **it is re-read, not re-derived**.
    ///
    /// `serde(default)` reads every manifest written before I0 as `None`, which
    /// is the truth: they all describe the unnamed instance.
    #[serde(default)]
    pub instance: Option<String>,
    pub odoo_user: String,
    pub db_user: String,
    pub db_name: String,
    pub odoo_home: PathBuf,
    pub install_dir: PathBuf,
    pub port: u16,
    /// the gevent (longpolling) port this instance claims.
    ///
    /// persisted for one reason: the preflight of the **next** installation
    /// reads it (`A-V6-3`). no undo uses it — a port is not an artifact — but
    /// "which ports are taken" cannot be answered by looking at the system,
    /// because an instance that is merely stopped holds none of them.
    ///
    /// `serde(default)` reads a manifest written before I3 as **8072**, and
    /// that is not a convenience: those installations have `gevent_port = 8072`
    /// literally in their `odoo.conf`, whatever their HTTP port. deriving
    /// `port + 3` on the way in would invent a claim they never made, and the
    /// check would wave through the collision it exists to catch.
    #[serde(default = "gevent_port_before_i3")]
    pub gevent_port: u16,
    /// `None` means Odoo logs to journal/stdout, so no log dir was created.
    pub odoo_logfile: Option<PathBuf>,
    pub with_nginx: bool,
    /// user who ran `sudo`; owns the control script and the `.bashrc`.
    pub sudo_user: Option<String>,
    /// distribution family the installation happened on, hence which commands
    /// must remove those artifacts.
    ///
    /// stored rather than detected at rollback time: an after-the-fact
    /// inference is how A-V3-1 and A-R8-1 were born. `serde(default)` reads an
    /// older manifest as `Debian`, which is the truth — every earlier
    /// installation was apt.
    #[serde(default)]
    pub os_family: OsFamily,

    /// installer version that **wrote** this manifest.
    ///
    /// makes the "unknown step" warning actionable: "written by 2.3.0, this
    /// binary is 2.1.0 — upgrade before undoing" (A-V3-16). `Option` +
    /// `serde(default)` keeps older manifests readable as `None`; making it
    /// mandatory would strand an already-deployed instance.
    #[serde(default)]
    pub installer_version: Option<String>,
}

/// the gevent port every installation before I3 wrote, hardwired in the
/// template.
fn gevent_port_before_i3() -> u16 {
    8072
}

/// the note to show when a different installer version wrote the manifest.
///
/// `None` when they match, and also when the manifest carries no version:
/// nothing is concluded from absent information. not a refusal — the rollback
/// stays best-effort — but the context the unknown-step warning was missing.
pub fn version_mismatch_note(manifest: Option<&str>, running: &str) -> Option<String> {
    let written_by = manifest?;
    if written_by == running {
        return None;
    }
    Some(format!(
        "this manifest was written by installer {written_by} while you are running version \
         {running}: if a step comes out unknown, that is why — use the same version (or a \
         newer one) to undo the installation"
    ))
}

impl InstallConfig {
    /// extracts from the [`Context`] the fields the rollback from disk needs.
    pub fn from_context(ctx: &Context) -> Self {
        InstallConfig {
            installer_version: Some(crate::INSTALLER_VERSION.to_string()),
            odoo_version: ctx.odoo_version.clone(),
            odoo_version_short: ctx.odoo_version_short.clone(),
            instance: ctx.instance.clone(),
            odoo_user: ctx.odoo_user.clone(),
            db_user: ctx.db_user.clone(),
            db_name: ctx.db_name.clone(),
            odoo_home: ctx.odoo_home.clone(),
            install_dir: ctx.install_dir.clone(),
            port: ctx.port,
            gevent_port: ctx.gevent_port,
            odoo_logfile: ctx.odoo_logfile.clone(),
            with_nginx: ctx.with_nginx,
            sudo_user: ctx.sudo_user.clone(),
            os_family: ctx.os_family,
        }
    }

    /// do the two configurations name the **same artifacts**?
    ///
    /// gates the resume (A-V3-1): resuming with a different `--db-name` would
    /// build a manifest straddling two instances, and the rollback would drop a
    /// database we never created.
    pub fn same_identity(&self, other: &Self) -> bool {
        self.identity() == other.identity()
    }

    /// the fields [`InstallConfig::same_identity`] compares, labelled so the
    /// user can be told **which** one differs.
    ///
    /// left out on purpose: `port` and `odoo_logfile` (no undo names anything
    /// by them), `with_nginx` (adds steps, renames nothing) and `sudo_user` (a
    /// different administrator may legitimately resume).
    pub fn identity(&self) -> Vec<(&'static str, String)> {
        vec![
            ("Odoo version", self.odoo_version.clone()),
            // names the unit, the config file, the vhost and the install dir: a
            // resume that changed it would undo artifacts belonging to another
            // instance. `None` renders as the unnamed instance rather than as an
            // empty string, so the message reads as something instead of
            // nothing.
            (
                "instance",
                self.instance
                    .clone()
                    .unwrap_or_else(|| "(unnamed)".to_string()),
            ),
            ("system user", self.odoo_user.clone()),
            ("database role", self.db_user.clone()),
            ("database name", self.db_name.clone()),
            ("home", self.odoo_home.display().to_string()),
            ("install directory", self.install_dir.display().to_string()),
            // the family does not name an artifact but changes what the
            // recorded names mean: an apt delta is not resumable by dnf.
            ("OS family", self.os_family.to_string()),
        ]
    }

    /// does the manifest describe an installation made by **this** installer?
    ///
    /// A-V3-8: undos delete trees rooted at paths from the state file, and the
    /// old guard compared two values that both came from that same file. this
    /// anchors them to `ODOO_HOME`, a constant no manifest can override, which
    /// covers every undo using `odoo_home` at once.
    ///
    /// # errors
    ///
    /// [`StepError::Precondition`] when the declared home is not `ODOO_HOME`,
    /// or the install dir is not strictly below it.
    pub fn validate_perimeter(&self) -> Result<(), StepError> {
        let expected = Path::new(crate::config::ODOO_HOME);
        if self.odoo_home != expected {
            return Err(StepError::Precondition(format!(
                "the manifest declares '{}' as the home, but this installer only uses '{}' \
                 (an architectural constant).\n\
                 \n\
                 it does not describe an installation made by this program, and the undos \
                 would act on paths we do not know: stopping without touching anything.",
                self.odoo_home.display(),
                expected.display()
            )));
        }
        if !self.install_dir.starts_with(expected) || self.install_dir == expected {
            return Err(StepError::Precondition(format!(
                "the manifest declares '{}' as the install directory, which is not under \
                 '{}': stopping without touching anything.",
                self.install_dir.display(),
                expected.display()
            )));
        }
        Ok(())
    }

    /// rebuilds the [`Context`] for a rollback from disk.
    ///
    /// unpersisted fields keep their defaults: passwords are empty and
    /// `os_info` is `None`, since only `run` needs it.
    ///
    /// `os_family` is set **explicitly**: it is the one defaulted field the
    /// undos actually read, and letting it fall through would make every
    /// rollback act as `Debian`, silently, on Fedora too.
    pub fn to_context(
        &self,
        dry_run: bool,
        aggressive_rollback: bool,
        state_path: PathBuf,
    ) -> Context {
        Context {
            odoo_version: self.odoo_version.clone(),
            odoo_version_short: self.odoo_version_short.clone(),
            // explicit, like `os_family`: the undos name artifacts through it,
            // and letting it fall back to the `Default` would make every
            // rollback act on the unnamed instance's names — removing the wrong
            // unit, or nothing at all.
            instance: self.instance.clone(),
            odoo_user: self.odoo_user.clone(),
            db_user: self.db_user.clone(),
            db_name: self.db_name.clone(),
            odoo_home: self.odoo_home.clone(),
            install_dir: self.install_dir.clone(),
            port: self.port,
            gevent_port: self.gevent_port,
            odoo_logfile: self.odoo_logfile.clone(),
            with_nginx: self.with_nginx,
            sudo_user: self.sudo_user.clone(),
            // explicit, never from `..Default::default()`: see the doc above.
            os_family: self.os_family,
            dry_run,
            aggressive_rollback,
            state_path,
            ..Default::default()
        }
    }
}

/// what a starting installation decided to do (A-V3-1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartDecision {
    /// no usable manifest: first installation.
    Fresh,
    /// partial and compatible manifest: resume.
    Resume,
    /// `--force` over an existing manifest: archive it and start over.
    Replace,
    /// manifest of a **finished** installation: refuse.
    RefuseFinished,
    /// partial manifest naming other artifacts, as (field, recorded,
    /// requested).
    RefuseIdentityMismatch(Vec<(&'static str, String, String)>),
    /// partial manifest with no configuration (pre-R4): identity cannot be
    /// established, so we do not resume.
    RefuseUnknownIdentity,
}

/// the start rule: install, resume or refuse.
///
/// A-V3-1: before R8 the install path never opened the manifest, so a second
/// run truncated it into one where nothing is ours — and the rollback then
/// declared "nothing left" and cleared it, stranding the instance. the partial
/// case resumes rather than refuses, because "run it again and carry on" is a
/// supported flow, but only when the artifact identity matches.
///
/// a **pure policy**, like `rollback::confirmation_gate`: `main` applies it and
/// formats the messages. the defect it closes used to live in `main`, where no
/// test reached it.
pub fn start_decision(
    state: &InstallState,
    requested: &InstallConfig,
    force: bool,
) -> StartDecision {
    if state.completed.is_empty() {
        return StartDecision::Fresh;
    }
    if force {
        return StartDecision::Replace;
    }
    if state.finished {
        return StartDecision::RefuseFinished;
    }

    let Some(precedente) = &state.config else {
        return StartDecision::RefuseUnknownIdentity;
    };

    let differences: Vec<(&'static str, String, String)> = precedente
        .identity()
        .into_iter()
        .zip(requested.identity())
        .filter(|((_, before), (_, now))| before != now)
        .map(|((field, before), (_, now))| (field, before, now))
        .collect();

    if differences.is_empty() {
        StartDecision::Resume
    } else {
        StartDecision::RefuseIdentityMismatch(differences)
    }
}

/// the installation state persisted to disk.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InstallState {
    /// completed steps in execution order; the rollback walks them backwards.
    pub completed: Vec<StepRecord>,
    /// configuration of this installation, so the rollback from disk knows
    /// *which* artifacts to undo.
    ///
    /// `Option` + `serde(default)`: a pre-R4 file stays readable, and
    /// `rollback` stops with an explicit message rather than guessing names.
    #[serde(default)]
    pub config: Option<InstallConfig>,
    /// set once the installation reached the end.
    ///
    /// separates "a working installation to uninstall" from "the leftovers of
    /// an interrupted run", which need different messages. inferring it from
    /// the step count would misread a complete state written by a version with
    /// fewer steps.
    #[serde(default)]
    pub finished: bool,
}

impl InstallState {
    /// loads the state from `path`.
    ///
    /// # errors
    ///
    /// [`StepError::Io`] when the file exists but cannot be read or parsed. a
    /// missing file yields an empty state: the normal first-run condition.
    pub fn load(path: &Path) -> Result<Self, StepError> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
                StepError::io(
                    path,
                    std::io::Error::new(std::io::ErrorKind::InvalidData, e),
                )
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(StepError::io(path, e)),
        }
    }

    /// writes the state, creating the file `0600` from creation so there is no
    /// window at wider permissions.
    ///
    /// # errors
    ///
    /// [`StepError::Io`] on any filesystem or serialisation failure.
    pub fn save(&self, path: &Path) -> Result<(), StepError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|e| StepError::io(parent, e))?;
            }
        }

        let json = serde_json::to_vec_pretty(self).map_err(|e| {
            StepError::io(
                path,
                std::io::Error::new(std::io::ErrorKind::InvalidData, e),
            )
        })?;

        // `mode()` applies at creation only.
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(STATE_FILE_MODE)
            .open(path)
            .map_err(|e| StepError::io(path, e))?;
        file.write_all(&json).map_err(|e| StepError::io(path, e))?;

        // an already-existing file keeps its own permissions, so force them.
        fs::set_permissions(path, fs::Permissions::from_mode(STATE_FILE_MODE))
            .map_err(|e| StepError::io(path, e))?;

        Ok(())
    }

    /// removes the state file. idempotent: an absent file is not an error.
    ///
    /// called only after a rollback has cleaned everything up. NOT on a
    /// successful installation (A-R5-1): there the complete state is the
    /// uninstall manifest, and clearing it left `invok rollback` answering
    /// "nothing to undo" on a working instance.
    ///
    /// also removes [`DEFAULT_STATE_DIR`] and its `instances/` subdirectory
    /// when they are empty, restricted to those constants so a `--state`
    /// elsewhere never removes somebody else's directory. an instance still
    /// installed leaves its manifest there, so the directories survive without
    /// anyone having to ask whether they should.
    ///
    /// # errors
    ///
    /// [`StepError::Io`] when the file exists but cannot be removed.
    pub fn clear(path: &Path) -> Result<(), StepError> {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(StepError::io(path, e)),
        }

        // the empty shells, innermost first, and only the ones we own. never
        // forced: `remove_dir` refuses by itself if anything is left, which is
        // the whole safety of doing this — another instance's manifest in
        // `instances/` keeps both directories alive without a check of ours.
        let instances_dir = Path::new(DEFAULT_STATE_DIR).join(crate::manifests::INSTANCES_SUBDIR);
        if path.parent() == Some(instances_dir.as_path()) {
            let _ = fs::remove_dir(&instances_dir);
        }
        if path.parent() == Some(Path::new(DEFAULT_STATE_DIR))
            || path.parent() == Some(instances_dir.as_path())
        {
            let _ = fs::remove_dir(DEFAULT_STATE_DIR);
        }

        Ok(())
    }

    /// appends a completed step to the in-memory state.
    pub fn record(&mut self, record: StepRecord) {
        self.completed.push(record);
    }

    /// records the installation's configuration, once, before the first step.
    pub fn set_config(&mut self, config: InstallConfig) {
        self.config = Some(config);
    }

    /// forgets a step: its artifact no longer exists.
    ///
    /// A-R8-1: the manifest describes what is *still* on the system. a record
    /// left behind would make a re-run skip that step and carry on over
    /// artifacts that no longer exist. a **failed** undo keeps its record — it
    /// is the only trace of the leftover to retry.
    pub fn forget(&mut self, name: &str) {
        self.completed.retain(|r| r.name != name);
    }

    /// the record of an already-completed step, if any.
    ///
    /// looked up by **name**, not by position: the canonical sequence changes
    /// between versions, so an older manifest must not be read by index.
    pub fn record_for(&self, name: &str) -> Option<&StepRecord> {
        self.completed.iter().find(|r| r.name == name)
    }

    /// is the HTTP port held by a service of **this** installation?
    ///
    /// true once the manifest records `setup-systemd`. the resume path uses it
    /// to skip the port check (A-R9-1): otherwise the installation would be
    /// rejected by the service it had just installed. read from the manifest,
    /// not inferred — "who holds the port" is not observable, "who opened it"
    /// is written down.
    pub fn owns_the_http_port(&self) -> bool {
        self.record_for("setup-systemd").is_some()
    }
}
