//! [`SystemOps`]: the boundary over privileged system commands.
//!
//! steps never call `useradd` or `chown` directly: they go through this trait.
//! [`RealSystemOps`] runs the real commands in production, while a mock in the
//! tests records *which* operation would run, with which arguments, without
//! touching the system or needing root.
//!
//! that is what makes the steps testable without changing
//! [`crate::step::Step`].

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::distro::Distro;
use crate::distro::OsFamily;
use crate::error::StepError;
use crate::packaging::PackageManager;

/// what sits at a path, looked at **without following symlinks**.
///
/// `symlink_exists` answers `true` to too many different questions: a symlink,
/// a regular file and a directory all "exist", but only the first can be
/// removed and recreated identically. telling them apart is the difference
/// between restoring a customer's configuration and destroying it (A-V3-5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathKind {
    /// nothing there.
    Absent,
    /// a symlink, with its target exactly as written.
    Symlink { target: PathBuf },
    /// a regular file: it has contents, and those contents are somebody's.
    RegularFile,
    /// a directory, socket, device… or a symlink whose target cannot be read.
    /// we do not know how to treat it, so the caller must abstain.
    Other,
}

/// a path's numeric owner, serialisable for persistence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerId {
    pub uid: u32,
    pub gid: u32,
}

/// the state of the Odoo sources found in a target directory.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OdooSourceState {
    /// the directory does not exist.
    #[default]
    Absent,
    /// a git clone, on the named branch.
    GitRepo { branch: String },
    /// a directory with `odoo-bin` but no `.git`, e.g. from a tarball.
    TarballPresent,
    /// present but not valid: neither the right git clone nor `odoo-bin`.
    InvalidDir,
}

/// mode for the private files the installer writes: owner only.
const PRIVATE_FILE_MODE: u32 = 0o600;

/// default timeout for **network** operations, in seconds.
///
/// generous enough for a shallow clone or a 15 MB download on a slow line,
/// short enough that a mirror which never closes the connection produces an
/// error instead of looking like a hang. raise it with [`NETWORK_TIMEOUT_ENV`].
pub const DEFAULT_NETWORK_TIMEOUT_SECS: u64 = 300;

/// environment variable overriding [`DEFAULT_NETWORK_TIMEOUT_SECS`].
///
/// `0` disables the timeout entirely; a non-numeric value is ignored.
pub const NETWORK_TIMEOUT_ENV: &str = "ODOO_NETWORK_TIMEOUT_SECS";

/// how often the child's exit is polled while waiting out the timeout.
///
/// negligible against timeouts measured in minutes, and short enough not to
/// delay quick commands.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// the current network timeout; `None` means none.
///
/// # why only the network
///
/// a timeout helps where the wait can be **infinite and fruitless** — a hung
/// connection never progresses. it hurts where the wait is long but legitimate:
/// - `odoo-bin -i base` and `pip install` are local and can take many minutes
///   on a small machine; a timeout would kill a perfectly valid installation;
/// - `apt-get` may legitimately wait on a `dpkg` lock held by
///   `unattended-upgrades`, and killing it mid-transaction leaves dpkg
///   half-configured — **worse** than the wait, and outside what our rollback
///   can repair.
///
/// so the three operations that talk to a remote host are covered, and nothing
/// else.
pub fn network_timeout() -> Option<Duration> {
    timeout_from_setting(std::env::var(NETWORK_TIMEOUT_ENV).ok().as_deref())
}

/// the timeout policy, **pure**: how a textual value becomes a limit.
///
/// separate from [`network_timeout`] so tests can check it without mutating the
/// process environment. absent or non-numeric gives the default, `0` gives
/// `None`, and `n` gives `n` seconds.
pub fn timeout_from_setting(raw: Option<&str>) -> Option<Duration> {
    let secs = raw
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_NETWORK_TIMEOUT_SECS);
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// drains a pipe to EOF on its own thread, returning the join handle.
fn drain_pipe<R: std::io::Read + Send + 'static>(
    pipe: Option<R>,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    })
}

/// waits at most `limit` for the child, **killing** it on expiry.
///
/// `std::process::Command` has no native timeout, and `wait-timeout` would
/// install a process-global SIGCHLD handler, so this polls `try_wait`. the
/// `Child` is never moved, so `kill()` has no pid-reuse race.
///
/// both pipes are drained by dedicated threads, and that is not a detail: `git
/// clone` writes progress to stderr and would otherwise fill the pipe buffer
/// and block — a deadlock of **ours** that the timeout would disguise as a slow
/// network.
///
/// # errors
///
/// [`StepError::Timeout`] on expiry, or [`StepError::Io`] on a wait failure.
fn output_with_timeout(
    mut command: Command,
    rendered: &str,
    limit: Duration,
) -> Result<std::process::Output, StepError> {
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| StepError::CommandFailed {
            command: rendered.to_string(),
            status: "spawn-failed".to_string(),
            stderr: e.to_string(),
        })?;

    let out_reader = drain_pipe(child.stdout.take());
    let err_reader = drain_pipe(child.stderr.take());

    let deadline = std::time::Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    // kill and **reap**, so no zombie is left. with the pipes
                    // closed both readers see EOF and finish.
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = out_reader.join();
                    let _ = err_reader.join();
                    return Err(StepError::Timeout {
                        command: rendered.to_string(),
                        secs: limit.as_secs(),
                    });
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = out_reader.join();
                let _ = err_reader.join();
                return Err(StepError::CommandFailed {
                    command: rendered.to_string(),
                    status: "wait-failed".to_string(),
                    stderr: e.to_string(),
                });
            }
        }
    };

    Ok(std::process::Output {
        status,
        stdout: out_reader.join().unwrap_or_default(),
        stderr: err_reader.join().unwrap_or_default(),
    })
}

/// builds a private temporary path **in the same directory** as `dest`, so the
/// final `move_file` is an atomic rename.
///
/// the random suffix keeps concurrent runs from colliding and stops a local
/// attacker pre-placing a symlink at a known path. it is only defence in depth:
/// the real guarantee is [`SystemOps::create_private_file`], which is
/// fail-closed even if the name were guessed.
pub fn private_temp_path(dest: &Path, fallback_name: &str) -> PathBuf {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| fallback_name.to_string());
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!(".{name}.{}.tmp", random_suffix()))
}

/// as [`private_temp_path`], but **keeps the extension** of the given name.
///
/// an external constraint, not a preference: `apt-get install <file>` only
/// treats its argument as a local path when it ends in `.deb`, otherwise it
/// reads it as a package name and fails. a `.tmp` temporary would have been
/// unpredictable *and* uninstallable — caught by a test, not by reasoning.
pub fn private_temp_path_keeping_extension(dir: &Path, name: &str) -> PathBuf {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, ext),
        // no extension to preserve: fall back to the plain form.
        _ => return private_temp_path(&dir.join(name), name),
    };
    dir.join(format!(".{stem}.{}.{ext}", random_suffix()))
}

/// a random hex suffix for temporary names.
///
/// reads `/dev/urandom`, degrading to pid plus nanoseconds — enough for
/// *uniqueness*, which is all correctness needs; the security lives in `O_EXCL
/// | O_NOFOLLOW`.
fn random_suffix() -> String {
    use std::io::Read;
    let mut buf = [0u8; 8];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_ok()
    {
        return buf.iter().map(|b| format!("{b:02x}")).collect();
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{:x}{:08x}", std::process::id(), nanos)
}

/// pure builders for the commands that take an **identifier as a positional
/// argument**.
///
/// every name is preceded by `--`, so even a value starting with `-` is read as
/// an operand and never as a flag. this is the downstream net against argument
/// injection; the upstream gate is [`crate::config::validate_identifier`].
/// pure, so tests can assert the `--` without root.
pub mod argv {
    use super::UserSpec;

    /// `useradd [opzioni] -- <login>`.
    pub fn useradd(spec: &UserSpec) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();
        if spec.system {
            args.push("--system".to_string());
        }
        if spec.create_home {
            args.push("--create-home".to_string());
        }
        args.push("--home-dir".to_string());
        args.push(spec.home.to_string_lossy().into_owned());
        if spec.user_group {
            args.push("--user-group".to_string());
        }
        args.push("--shell".to_string());
        args.push(spec.shell.clone());
        args.push("--".to_string());
        args.push(spec.name.clone());
        args
    }

    /// `userdel -- <login>`. **never** `-r`: the home is `PrepareOptRoot`'s.
    pub fn userdel(user: &str) -> Vec<String> {
        vec!["--".to_string(), user.to_string()]
    }

    /// `groupdel -- <group>`.
    pub fn groupdel(group: &str) -> Vec<String> {
        vec!["--".to_string(), group.to_string()]
    }

    /// `sudo -Hiu postgres -- createdb --owner <owner> -- <db>`.
    pub fn createdb(owner: &str, db: &str) -> Vec<String> {
        vec![
            "-n".to_string(),
            "-Hiu".to_string(),
            "postgres".to_string(),
            "--".to_string(),
            "createdb".to_string(),
            "--owner".to_string(),
            owner.to_string(),
            "--".to_string(),
            db.to_string(),
        ]
    }

    /// `sudo -Hiu postgres -- dropdb --if-exists --force -- <db>`.
    pub fn dropdb(db: &str) -> Vec<String> {
        vec![
            "-n".to_string(),
            "-Hiu".to_string(),
            "postgres".to_string(),
            "--".to_string(),
            "dropdb".to_string(),
            "--if-exists".to_string(),
            "--force".to_string(),
            "--".to_string(),
            db.to_string(),
        ]
    }

    /// `getent passwd -- <user>`.
    pub fn getent_passwd(user: &str) -> Vec<String> {
        vec!["passwd".to_string(), "--".to_string(), user.to_string()]
    }
}

/// the arguments for creating a system user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSpec {
    pub name: String,
    pub home: PathBuf,
    pub system: bool,
    pub create_home: bool,
    pub user_group: bool,
    pub shell: String,
}

/// privileged, mutating system operations behind a testable boundary.
///
/// **security note:** `delete_user` never removes the home (no `-r`). removing
/// `/opt/odoo` belongs solely to the step that created it, which runs later in
/// the reverse order.
pub trait SystemOps {
    fn user_exists(&self, user: &str) -> bool;
    fn path_exists(&self, path: &Path) -> bool;
    fn owner_of(&self, path: &Path) -> Result<OwnerId, StepError>;
    /// the permission bits, `chmod`'s counterpart.
    ///
    /// exists because [`Self::chmod`] alone can only *set*: widening somebody
    /// else's directory without having read what it was is a mutation with no
    /// undo (`A-V6-9`). the same asymmetry R11 found on the nginx default site —
    /// the snapshot recorded *whether* it was there, not *what* it was.
    ///
    /// returns the low twelve bits (permissions plus setuid/setgid/sticky), not
    /// the file type: it is meant to be handed back to `chmod` unchanged.
    fn mode_of(&self, path: &Path) -> Result<u32, StepError>;
    fn dir_is_empty(&self, path: &Path) -> Result<bool, StepError>;

    fn create_user(&self, spec: &UserSpec) -> Result<(), StepError>;
    /// removes the user and its primary group. **never** `-r`: the home is not
    /// this command's business.
    fn delete_user(&self, user: &str) -> Result<(), StepError>;
    fn delete_group(&self, group: &str) -> Result<(), StepError>;

    fn chown_named(&self, path: &Path, owner: &str, group: &str) -> Result<(), StepError>;
    fn chown_numeric(&self, path: &Path, id: OwnerId) -> Result<(), StepError>;
    fn chmod(&self, path: &Path, mode: u32) -> Result<(), StepError>;
    fn mkdir(&self, path: &Path) -> Result<(), StepError>;
    fn rmdir(&self, path: &Path) -> Result<(), StepError>;

    // --- package manager and distribution conventions -----------------------
    /// this family's package manager.
    ///
    /// the eleven `apt_*`/`dpkg_*` methods that used to live here now sit
    /// behind [`PackageManager`]: a second family beside them would have taken
    /// this trait past eighty methods and scattered the choice of manager into
    /// every caller.
    fn packages(&self) -> &dyn PackageManager;
    /// this family's distribution conventions.
    fn distro(&self) -> &dyn Distro;

    /// the installed `wkhtmltopdf` version, or `None`.
    fn wkhtmltopdf_version(&self) -> Option<String>;

    // --- systemd services
    // -----------------------------------------------------
    fn service_is_enabled(&self, service: &str) -> bool;
    fn service_is_active(&self, service: &str) -> bool;
    fn service_enable(&self, service: &str) -> Result<(), StepError>;
    fn service_disable(&self, service: &str) -> Result<(), StepError>;
    fn service_start(&self, service: &str) -> Result<(), StepError>;
    fn service_stop(&self, service: &str) -> Result<(), StepError>;
    /// `systemctl restart <service>`, to apply a new config.
    fn service_restart(&self, service: &str) -> Result<(), StepError>;
    /// `systemctl reload <service>`, without downtime.
    fn service_reload(&self, service: &str) -> Result<(), StepError>;
    /// `systemctl daemon-reload`.
    fn daemon_reload(&self) -> Result<(), StepError>;

    // --- Nginx and firewall
    // ---------------------------------------------------
    /// idempotent `ln -sf <src> <link>`.
    fn create_symlink(&self, src: &Path, link: &Path) -> Result<(), StepError>;
    /// removes a symlink. idempotent: absent means no-op.
    fn remove_symlink(&self, link: &Path) -> Result<(), StepError>;
    /// `true` when something is there, dangling links included.
    ///
    /// **careful:** also `true` for a **regular file** or a directory, because
    /// it uses `symlink_metadata`. where the nature matters, use
    /// [`SystemOps::path_kind`]: confusing the two cost a customer their
    /// configuration file (A-V3-5).
    fn symlink_exists(&self, link: &Path) -> bool;
    /// what is at this path, **without following symlinks**.
    ///
    /// for where the difference changes what is allowed: a symlink can be
    /// recreated identically, a regular file holds data to preserve.
    fn path_kind(&self, path: &Path) -> PathKind;
    /// `nginx -t`: is the config valid?
    fn nginx_test(&self) -> bool;

    // --- PostgreSQL
    // -----------------------------------------------------------
    /// `true` when the role exists.
    fn pg_role_exists(&self, role: &str) -> Result<bool, StepError>;
    /// `true` when the database exists.
    fn pg_db_exists(&self, db: &str) -> Result<bool, StepError>;
    /// creates the role. `password` is the plaintext secret, or `None` for peer
    /// auth: escaping and safe delivery happen inside the boundary, so it never
    /// leaks outside.
    fn pg_create_role(&self, role: &str, password: Option<&str>) -> Result<(), StepError>;
    /// `DROP ROLE IF EXISTS "<role>"`.
    fn pg_drop_role(&self, role: &str) -> Result<(), StepError>;
    /// `createdb --owner <owner> <db>`.
    fn createdb(&self, owner: &str, db: &str) -> Result<(), StepError>;
    /// `dropdb --if-exists --force <db>` (chiude le connessioni attive).
    fn dropdb(&self, db: &str) -> Result<(), StepError>;
    /// lists the cluster's non-template databases, for the purge caution.
    fn pg_list_databases(&self) -> Result<Vec<String>, StepError>;

    // --- Odoo sources, all as a non-root user
    // ---------------------------------
    /// runs `sudo -u <user> -- <program> <args>`: least privilege.
    fn run_as_user(&self, user: &str, program: &str, args: &[&str]) -> Result<(), StepError>;
    /// `sudo -u <user> -- mkdir -p <path>`.
    fn mkdir_p_as_user(&self, user: &str, path: &Path) -> Result<(), StepError>;
    /// recursive removal of a directory inside **our** perimeter. idempotent:
    /// an absent directory is a no-op.
    fn remove_dir_all(&self, path: &Path) -> Result<(), StepError>;
    /// detects the state of the Odoo sources in `target`.
    fn detect_odoo_source(&self, user: &str, target: &Path) -> Result<OdooSourceState, StepError>;
    /// a single `git clone` attempt as `user`.
    fn git_clone(
        &self,
        user: &str,
        url: &str,
        branch: &str,
        depth: u32,
        target: &Path,
    ) -> Result<(), StepError>;
    /// fallback: downloads and extracts the branch tarball, as `user`.
    fn tarball_install(&self, user: &str, url: &str, target: &Path) -> Result<(), StepError>;
    /// does `<venv>/bin/python3` exist and is it executable?
    fn venv_python_exists(&self, venv: &Path) -> bool;
    /// can this system **create** a virtualenv?
    ///
    /// not the same question as "does the `venv` module exist": that lives in
    /// the stdlib and is always there, while `ensurepip` — without which
    /// `python3 -m venv` stops halfway — comes with a separate package. the
    /// implementation asks about `ensurepip` for exactly that reason (A-R6-1).
    fn python_venv_available(&self, python: &str) -> bool;
    /// the version of **this** interpreter, or `None` when unknown.
    ///
    /// used to **explain** a failure, not to prevent it (A-MD-7): when pip
    /// cannot build gevent the usual cause is a Python newer than Odoo's pins,
    /// and that is not recoverable from `gcc` output.
    ///
    /// `None` means "unknown", not "fine": nothing is concluded from it.
    fn python_version(&self, python: &str) -> Option<(u32, u32)>;
    /// `sudo -u <user> -- <python> -m venv <venv>`.
    ///
    /// the interpreter is a parameter since M11: where the system `python3` is
    /// newer than Odoo's pins, the venv is built on an alternative one.
    fn create_venv(&self, user: &str, python: &str, venv: &Path) -> Result<(), StepError>;
    /// reads a text file.
    fn read_to_string(&self, path: &Path) -> Result<String, StepError>;

    // --- config and database init
    // ---------------------------------------------
    /// writes `content` to a **private** file (`0600` from creation), so the
    /// master password is never readable by others.
    ///
    /// **semantics: in-place rewrite of a path already ours.** the file is
    /// created if absent and truncated if present, and a symlink **is
    /// followed** — deliberate for the installing user's `.bashrc`, which may
    /// legitimately link into their dotfiles.
    ///
    /// to **create** a new file in a directory owned by someone else, use
    /// [`SystemOps::create_private_file`] instead: here a predictable path plus
    /// symlink following would be a TOCTOU vector.
    fn write_private_file(&self, path: &Path, content: &str) -> Result<(), StepError>;
    /// **creates** a private (`0600`) file, fail-closed on any surprise.
    ///
    /// opens with `O_CREAT | O_EXCL | O_NOFOLLOW`, so an existing path — file,
    /// directory or symlink — fails the open, and a symlink is never followed.
    ///
    /// this is the method for temporaries **root** writes into directories
    /// owned by other users: the worst a local attacker gets is a failed step,
    /// never an arbitrary root write nor hijacked contents. see
    /// [`private_temp_path`].
    fn create_private_file(&self, path: &Path, content: &str) -> Result<(), StepError>;
    /// moves `src` onto `dst`: a rename, falling back to copy+remove across
    /// devices.
    fn move_file(&self, src: &Path, dst: &Path) -> Result<(), StepError>;
    /// copies `src` to `dst`, for backups.
    fn copy_file(&self, src: &Path, dst: &Path) -> Result<(), StepError>;
    /// removes a file. idempotent: absent means no-op.
    fn remove_file(&self, path: &Path) -> Result<(), StepError>;
    /// `true` when the database already has the Odoo schema.
    fn pg_db_initialized(&self, db: &str) -> Result<bool, StepError>;
    /// `sudo -u <user> -- <python> <odoo_bin> -c <conf> -d <db> -i base
    /// --without-demo=all --stop-after-init`.
    fn odoo_init_base(
        &self,
        user: &str,
        python: &Path,
        odoo_bin: &Path,
        conf: &Path,
        db: &str,
    ) -> Result<(), StepError>;

    // --- control script and bashrc
    // --------------------------------------------
    /// the user's home from `getent passwd`; `None` when absent.
    fn getent_home(&self, user: &str) -> Result<Option<String>, StepError>;
    /// `chown <user>:<user> <path>`: the files stay the installing user's.
    fn chown_to_user(&self, path: &Path, user: &str) -> Result<(), StepError>;
    /// appends a single line to a file, creating it if absent. the whole file
    /// is **never** rewritten.
    fn append_line(&self, path: &Path, line: &str) -> Result<(), StepError>;
}

/// the real implementation: it runs the system commands.
///
/// carries the two backends chosen for the family. deliberately no `Default`: a
/// `RealSystemOps` without a family makes no sense, and a default would pick
/// apt in silence.
pub struct RealSystemOps {
    packages: Box<dyn PackageManager>,
    distro: Box<dyn Distro>,
}

impl RealSystemOps {
    /// the Fedora family's implementations: dnf and firewalld.
    pub fn fedora() -> Self {
        RealSystemOps {
            packages: Box::new(crate::packaging::dnf::DnfBackend),
            distro: Box::new(crate::distro::fedora::Fedora::new()),
        }
    }

    /// the Debian family's implementations: apt and ufw.
    ///
    /// there is no "family-less" constructor: choosing one is a decision, and
    /// it belongs in a single place — [`backend_factory`]. a `new()` handing
    /// apt to everyone would be the silent default this work exists to avoid.
    pub fn debian() -> Self {
        RealSystemOps {
            packages: Box::new(crate::packaging::apt::AptBackend),
            distro: Box::new(crate::distro::debian::Debian::new()),
        }
    }
}

/// the backend factory for a family, or `None` when **this binary** has none.
///
/// an `Option` because "I do not have one" must be **sayable**. a `match`
/// handing apt to a family without a backend would be a silent lie: `apt-get`
/// on a machine without apt fails obscurely, and in a rollback it would leave
/// installed everything there was to remove. both families have a backend
/// today, but the shape is the fail-closed that will hold for a third.
///
/// returns a function pointer rather than a built value because steps **own**
/// their `ops`: N instances are needed, not N references. the caller handles
/// the `None` **once**, and from there has a factory that cannot fail.
pub fn backend_factory(family: OsFamily) -> Option<fn() -> Box<dyn SystemOps>> {
    match family {
        OsFamily::Debian => Some(|| Box::new(RealSystemOps::debian()) as Box<dyn SystemOps>),
        OsFamily::Fedora => Some(|| Box::new(RealSystemOps::fedora()) as Box<dyn SystemOps>),
    }
}

/// runs an external command, with optional env and timeout.
///
/// # errors
///
/// [`StepError::CommandFailed`] on a non-zero exit, [`StepError::Timeout`] on
/// expiry, [`StepError::Io`] when the command cannot be spawned.
fn run_command_full(
    program: &str,
    args: &[&str],
    envs: &[(&str, &str)],
    timeout: Option<Duration>,
) -> Result<(), StepError> {
    let rendered = format!("{program} {}", args.join(" "));
    let mut command = Command::new(program);
    command.args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = match timeout {
        // no timeout: `output()` is simplest and drains the pipes itself.
        None => command.output().map_err(|e| StepError::CommandFailed {
            command: rendered.clone(),
            status: "spawn-failed".to_string(),
            stderr: e.to_string(),
        })?,
        Some(limit) => output_with_timeout(command, &rendered, limit)?,
    };
    if output.status.success() {
        Ok(())
    } else {
        Err(StepError::CommandFailed {
            command: rendered,
            status: output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// runs an external command with extra env and **no** timeout.
pub(crate) fn run_command_with_env(
    program: &str,
    args: &[&str],
    envs: &[(&str, &str)],
) -> Result<(), StepError> {
    run_command_full(program, args, envs, None)
}

/// runs an external command with no extra env and **no** timeout.
pub(crate) fn run_command(program: &str, args: &[&str]) -> Result<(), StepError> {
    run_command_full(program, args, &[], None)
}

/// runs a **network** command under the current [`network_timeout`].
///
/// only for operations that talk to a remote host. see [`network_timeout`] for
/// why the others stay out.
fn run_network_command(program: &str, args: &[&str]) -> Result<(), StepError> {
    run_command_full(program, args, &[], network_timeout())
}

/// runs an external command under an **explicit** timeout.
///
/// the primitive the network operations rest on, exposed so its behaviour —
/// killing on expiry, capturing stderr, never deadlocking on the pipes — is
/// checkable without touching the network or waiting minutes.
pub fn run_with_timeout(program: &str, args: &[&str], limit: Duration) -> Result<(), StepError> {
    run_command_full(program, args, &[], Some(limit))
}

/// adapts the arguments built by [`argv`] to [`run_command`]'s signature.
fn as_refs(args: &[String]) -> Vec<&str> {
    args.iter().map(String::as_str).collect()
}

/// `true` when `apt-cache policy` output declares an installable candidate.
///
/// pure and public so the real cases are checkable without apt at hand:
/// available package, purely virtual one (`Candidate: (none)`), and a name that
/// does not exist at all.
pub fn has_installable_candidate(policy_output: &str) -> bool {
    policy_output.lines().any(|line| {
        line.trim()
            .strip_prefix("Candidate:")
            .map(|value| {
                let value = value.trim();
                !value.is_empty() && value != "(none)"
            })
            .unwrap_or(false)
    })
}

/// how many packages apt knows about, from `apt-cache stats`.
///
/// `None` when the line is missing or not a number — "unknown", which differs
/// from "zero".
pub fn total_package_names(stats_output: &str) -> Option<u64> {
    for line in stats_output.lines() {
        let Some(value) = line.trim().strip_prefix("Total package names:") else {
            continue;
        };
        // the line is `Total package names: 163333 (4573 k)`: the first token
        // is the count, the rest is the in-memory size.
        return value.split_whitespace().next()?.parse().ok();
    }
    None
}

/// runs a command capturing stdout, for psql and systemctl queries.
pub(crate) fn capture_command(program: &str, args: &[&str]) -> Result<String, StepError> {
    capture_command_with_env(program, args, &[])
}

/// as [`capture_command`], with extra environment variables.
///
/// exists for `LC_ALL=C`: `apt-cache` output is **localised**, and a parser
/// looking for `Candidate:` on a localised machine would conclude that no
/// package is installable.
pub(crate) fn capture_command_with_env(
    program: &str,
    args: &[&str],
    envs: &[(&str, &str)],
) -> Result<String, StepError> {
    let rendered = format!("{program} {}", args.join(" "));
    let mut command = Command::new(program);
    command.args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command.output().map_err(|e| StepError::CommandFailed {
        command: rendered.clone(),
        status: "spawn-failed".to_string(),
        stderr: e.to_string(),
    })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(StepError::CommandFailed {
            command: rendered,
            status: output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// runs a command feeding `input` through **stdin**, never argv.
///
/// with `secret`, stderr is left out of the error: psql echoes the failing
/// line, which would contain the password. for commands carrying secrets we
/// lose the diagnostic detail rather than risk a leak.
fn run_command_stdin(
    program: &str,
    args: &[&str],
    input: &str,
    secret: bool,
) -> Result<(), StepError> {
    use std::io::Write;
    use std::process::Stdio;

    let rendered = format!("{program} {}", args.join(" "));
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| StepError::CommandFailed {
            command: rendered.clone(),
            status: "spawn-failed".to_string(),
            stderr: e.to_string(),
        })?;

    // `take()` closes stdin after writing, so the child sees EOF.
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input.as_bytes())
            .map_err(|e| StepError::CommandFailed {
                command: rendered.clone(),
                status: "stdin-write-failed".to_string(),
                stderr: e.to_string(),
            })?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| StepError::CommandFailed {
            command: rendered.clone(),
            status: "wait-failed".to_string(),
            stderr: e.to_string(),
        })?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = if secret {
            "<output suppressed: it may contain secrets>".to_string()
        } else {
            String::from_utf8_lossy(&output.stderr).into_owned()
        };
        Err(StepError::CommandFailed {
            command: rendered,
            status: output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            stderr,
        })
    }
}

/// escapes an SQL literal by doubling single quotes.
///
/// used for the role's password. identifiers are validated upstream and
/// double-quoted anyway.
pub fn escape_sql_literal(value: &str) -> String {
    value.replace('\'', "''")
}

/// converts a nix `Errno` into an `io::Error`, for `StepError::Io`.
fn errno_io(e: nix::errno::Errno) -> std::io::Error {
    std::io::Error::from_raw_os_error(e as i32)
}

impl SystemOps for RealSystemOps {
    fn user_exists(&self, user: &str) -> bool {
        nix::unistd::User::from_name(user).ok().flatten().is_some()
    }

    fn path_exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn owner_of(&self, path: &Path) -> Result<OwnerId, StepError> {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path).map_err(|e| StepError::io(path, e))?;
        Ok(OwnerId {
            uid: meta.uid(),
            gid: meta.gid(),
        })
    }

    fn mode_of(&self, path: &Path) -> Result<u32, StepError> {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(path).map_err(|e| StepError::io(path, e))?;
        Ok(meta.permissions().mode() & 0o7777)
    }

    fn dir_is_empty(&self, path: &Path) -> Result<bool, StepError> {
        let mut entries = std::fs::read_dir(path).map_err(|e| StepError::io(path, e))?;
        Ok(entries.next().is_none())
    }

    fn create_user(&self, spec: &UserSpec) -> Result<(), StepError> {
        run_command("useradd", &as_refs(&argv::useradd(spec)))
    }

    fn delete_user(&self, user: &str) -> Result<(), StepError> {
        // NEVER `-r`: the home belongs to `PrepareOptRoot`'s undo.
        run_command("userdel", &as_refs(&argv::userdel(user)))
    }

    fn delete_group(&self, group: &str) -> Result<(), StepError> {
        run_command("groupdel", &as_refs(&argv::groupdel(group)))
    }

    fn chown_named(&self, path: &Path, owner: &str, group: &str) -> Result<(), StepError> {
        let uid = nix::unistd::User::from_name(owner)
            .ok()
            .flatten()
            .map(|u| u.uid)
            .ok_or_else(|| {
                StepError::Precondition(format!("user '{owner}' not found for chown"))
            })?;
        let gid = nix::unistd::Group::from_name(group)
            .ok()
            .flatten()
            .map(|g| g.gid)
            .ok_or_else(|| {
                StepError::Precondition(format!("group '{group}' not found for chown"))
            })?;
        nix::unistd::chown(path, Some(uid), Some(gid)).map_err(|e| StepError::io(path, errno_io(e)))
    }

    fn chown_numeric(&self, path: &Path, id: OwnerId) -> Result<(), StepError> {
        let uid = nix::unistd::Uid::from_raw(id.uid);
        let gid = nix::unistd::Gid::from_raw(id.gid);
        nix::unistd::chown(path, Some(uid), Some(gid)).map_err(|e| StepError::io(path, errno_io(e)))
    }

    fn chmod(&self, path: &Path, mode: u32) -> Result<(), StepError> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|e| StepError::io(path, e))
    }

    fn mkdir(&self, path: &Path) -> Result<(), StepError> {
        std::fs::create_dir(path).map_err(|e| StepError::io(path, e))
    }

    fn rmdir(&self, path: &Path) -> Result<(), StepError> {
        std::fs::remove_dir(path).map_err(|e| StepError::io(path, e))
    }

    fn packages(&self) -> &dyn PackageManager {
        self.packages.as_ref()
    }

    fn distro(&self) -> &dyn Distro {
        self.distro.as_ref()
    }

    fn wkhtmltopdf_version(&self) -> Option<String> {
        let out = Command::new("wkhtmltopdf").arg("--version").output().ok()?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        // first token that looks like a version: starts with a digit, two dots.
        text.split_whitespace()
            .find(|tok| {
                tok.chars().next().is_some_and(|c| c.is_ascii_digit())
                    && tok.matches('.').count() >= 2
            })
            .map(|s| s.to_string())
    }

    fn service_is_enabled(&self, service: &str) -> bool {
        matches!(
            Command::new("systemctl").args(["is-enabled", service]).output(),
            Ok(out) if String::from_utf8_lossy(&out.stdout).trim() == "enabled"
        )
    }

    fn service_is_active(&self, service: &str) -> bool {
        Command::new("systemctl")
            .args(["is-active", "--quiet", service])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn service_enable(&self, service: &str) -> Result<(), StepError> {
        run_command("systemctl", &["enable", service])
    }

    fn service_disable(&self, service: &str) -> Result<(), StepError> {
        run_command("systemctl", &["disable", service])
    }

    fn service_start(&self, service: &str) -> Result<(), StepError> {
        run_command("systemctl", &["start", service])
    }

    fn service_stop(&self, service: &str) -> Result<(), StepError> {
        run_command("systemctl", &["stop", service])
    }

    fn service_restart(&self, service: &str) -> Result<(), StepError> {
        run_command("systemctl", &["restart", service])
    }

    fn service_reload(&self, service: &str) -> Result<(), StepError> {
        run_command("systemctl", &["reload", service])
    }

    fn daemon_reload(&self) -> Result<(), StepError> {
        run_command("systemctl", &["daemon-reload"])
    }

    fn create_symlink(&self, src: &Path, link: &Path) -> Result<(), StepError> {
        let src = src.to_string_lossy();
        let link = link.to_string_lossy();
        run_command("ln", &["-sf", &src, &link])
    }

    fn remove_symlink(&self, link: &Path) -> Result<(), StepError> {
        match std::fs::remove_file(link) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StepError::io(link, e)),
        }
    }

    fn symlink_exists(&self, link: &Path) -> bool {
        link.symlink_metadata().is_ok()
    }

    fn path_kind(&self, path: &Path) -> PathKind {
        let Ok(meta) = path.symlink_metadata() else {
            return PathKind::Absent;
        };
        if meta.file_type().is_symlink() {
            // a link whose target cannot be read is not recreatable
            // identically, so "unknown" beats recreating it wrong.
            return match std::fs::read_link(path) {
                Ok(target) => PathKind::Symlink { target },
                Err(_) => PathKind::Other,
            };
        }
        if meta.is_file() {
            PathKind::RegularFile
        } else {
            PathKind::Other
        }
    }

    fn nginx_test(&self) -> bool {
        Command::new("nginx")
            .arg("-t")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn pg_role_exists(&self, role: &str) -> Result<bool, StepError> {
        let sql = format!(
            "SELECT 1 FROM pg_roles WHERE rolname = '{}';",
            escape_sql_literal(role)
        );
        let out = capture_command(
            "sudo",
            &["-n", "-Hiu", "postgres", "--", "psql", "-tAc", &sql],
        )?;
        Ok(!out.trim().is_empty())
    }

    fn pg_db_exists(&self, db: &str) -> Result<bool, StepError> {
        let sql = format!(
            "SELECT 1 FROM pg_database WHERE datname = '{}';",
            escape_sql_literal(db)
        );
        let out = capture_command(
            "sudo",
            &["-n", "-Hiu", "postgres", "--", "psql", "-tAc", &sql],
        )?;
        Ok(!out.trim().is_empty())
    }

    fn pg_create_role(&self, role: &str, password: Option<&str>) -> Result<(), StepError> {
        // identifier double-quoted, password as an escaped literal. the SQL
        // goes through stdin, and on error stderr is suppressed.
        let sql = match password {
            Some(pw) => format!(
                "CREATE ROLE \"{role}\" WITH LOGIN CREATEDB PASSWORD '{}';",
                escape_sql_literal(pw)
            ),
            None => format!("CREATE ROLE \"{role}\" WITH LOGIN CREATEDB;"),
        };
        run_command_stdin(
            "sudo",
            &[
                "-n",
                "-Hiu",
                "postgres",
                "--",
                "psql",
                "-v",
                "ON_ERROR_STOP=1",
            ],
            &sql,
            /* secret */ true,
        )
    }

    fn pg_drop_role(&self, role: &str) -> Result<(), StepError> {
        let sql = format!("DROP ROLE IF EXISTS \"{role}\";");
        run_command(
            "sudo",
            &[
                "-n",
                "-Hiu",
                "postgres",
                "--",
                "psql",
                "-v",
                "ON_ERROR_STOP=1",
                "-c",
                &sql,
            ],
        )
    }

    fn createdb(&self, owner: &str, db: &str) -> Result<(), StepError> {
        run_command("sudo", &as_refs(&argv::createdb(owner, db)))
    }

    fn dropdb(&self, db: &str) -> Result<(), StepError> {
        run_command("sudo", &as_refs(&argv::dropdb(db)))
    }

    fn pg_list_databases(&self) -> Result<Vec<String>, StepError> {
        let out = capture_command(
            "sudo",
            &[
                "-n",
                "-Hiu",
                "postgres",
                "--",
                "psql",
                "-tAc",
                "SELECT datname FROM pg_database WHERE datistemplate = false;",
            ],
        )?;
        Ok(out
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }

    fn run_as_user(&self, user: &str, program: &str, args: &[&str]) -> Result<(), StepError> {
        let mut full = vec!["-n", "-u", user, "--", program];
        full.extend_from_slice(args);
        run_command("sudo", &full)
    }

    fn mkdir_p_as_user(&self, user: &str, path: &Path) -> Result<(), StepError> {
        let p = path.to_string_lossy();
        run_command("sudo", &["-n", "-u", user, "--", "mkdir", "-p", &p])
    }

    fn remove_dir_all(&self, path: &Path) -> Result<(), StepError> {
        match std::fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StepError::io(path, e)),
        }
    }

    fn detect_odoo_source(&self, user: &str, target: &Path) -> Result<OdooSourceState, StepError> {
        if target.join(".git").is_dir() {
            let target_str = target.to_string_lossy();
            let branch = capture_command(
                "sudo",
                &[
                    "-n",
                    "-u",
                    user,
                    "--",
                    "git",
                    "-C",
                    &target_str,
                    "rev-parse",
                    "--abbrev-ref",
                    "HEAD",
                ],
            )
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
            return Ok(OdooSourceState::GitRepo { branch });
        }
        if target.is_dir() {
            if target.join("odoo-bin").is_file() {
                return Ok(OdooSourceState::TarballPresent);
            }
            return Ok(OdooSourceState::InvalidDir);
        }
        Ok(OdooSourceState::Absent)
    }

    fn git_clone(
        &self,
        user: &str,
        url: &str,
        branch: &str,
        depth: u32,
        target: &Path,
    ) -> Result<(), StepError> {
        let target_str = target.to_string_lossy();
        let depth_str = depth.to_string();
        // a network operation, so a timeout applies. an expired attempt is a
        // retryable failure, treated like any other.
        run_network_command(
            "sudo",
            &[
                "-n",
                "-u",
                user,
                "--",
                "git",
                "-c",
                "http.version=HTTP/1.1",
                "-c",
                "core.compression=0",
                "clone",
                url,
                "--branch",
                branch,
                "--single-branch",
                "--no-tags",
                "--depth",
                &depth_str,
                &target_str,
            ],
        )
    }

    fn tarball_install(&self, user: &str, url: &str, target: &Path) -> Result<(), StepError> {
        // unpredictable name, and the file is created by us before wget sees
        // the path (A-V3-3). the old fixed name was known to anyone reading the
        // source, and unlike the wkhtmltopdf package the tarball has **no
        // expected checksum** to hold against replaced contents.
        //
        // `create_private_file` is fail-closed, so an occupied path fails the
        // download instead of hijacking it. the `chown` is not a detail: the
        // file is born `0600 root` and `tar` reads it as `odoo`.
        let tmp = private_temp_path_keeping_extension(&std::env::temp_dir(), "odoo-src.tar.gz");
        self.create_private_file(&tmp, "")?;
        self.chown_named(&tmp, user, user)?;
        let tmp_str = tmp.to_string_lossy().into_owned();
        let target_str = target.to_string_lossy().into_owned();

        // only the download is timed: extraction is local and can legitimately
        // take a while on a slow machine.
        let outcome = (|| {
            run_network_command("wget", &["-qO", &tmp_str, url])?;
            run_command(
                "sudo",
                &["-n", "-u", user, "--", "mkdir", "-p", &target_str],
            )?;
            run_command(
                "sudo",
                &[
                    "-n",
                    "-u",
                    user,
                    "--",
                    "tar",
                    "-xzf",
                    &tmp_str,
                    "-C",
                    &target_str,
                    "--strip-components=1",
                ],
            )?;
            if !target.join("odoo-bin").is_file() {
                return Err(StepError::Precondition(format!(
                    "the tarball fallback completed but odoo-bin is missing in {target_str}"
                )));
            }
            Ok(())
        })();
        let _ = std::fs::remove_file(&tmp);
        outcome
    }

    fn venv_python_exists(&self, venv: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        let python = venv.join("bin").join("python3");
        python
            .metadata()
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }

    fn python_venv_available(&self, python: &str) -> bool {
        // `import ensurepip`, NOT `python3 -m venv --help`: the `venv` module
        // is in the stdlib and always answers 0, even where creating a
        // virtualenv is impossible. `ensurepip` is what the venv package
        // actually brings. asking about the wrong module made this a check that
        // could not fail (A-R6-1).
        Command::new(python)
            .args(["-c", "import ensurepip"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// one source for "which Python is this": the read lives in
    /// [`crate::checks::python_version`], asking the same interpreter
    /// `create_venv` invokes below.
    fn python_version(&self, python: &str) -> Option<(u32, u32)> {
        crate::checks::python_version(python)
    }

    fn create_venv(&self, user: &str, python: &str, venv: &Path) -> Result<(), StepError> {
        let venv_str = venv.to_string_lossy();
        run_command(
            "sudo",
            &["-n", "-u", user, "--", python, "-m", "venv", &venv_str],
        )
    }

    fn read_to_string(&self, path: &Path) -> Result<String, StepError> {
        std::fs::read_to_string(path).map_err(|e| StepError::io(path, e))
    }

    fn write_private_file(&self, path: &Path, content: &str) -> Result<(), StepError> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(PRIVATE_FILE_MODE)
            .open(path)
            .map_err(|e| StepError::io(path, e))?;
        file.write_all(content.as_bytes())
            .map_err(|e| StepError::io(path, e))
    }

    fn create_private_file(&self, path: &Path, content: &str) -> Result<(), StepError> {
        create_private_file_at(path, content)
    }

    fn move_file(&self, src: &Path, dst: &Path) -> Result<(), StepError> {
        if std::fs::rename(src, dst).is_ok() {
            return Ok(());
        }
        // cross-device fallback: copy, then remove the source.
        std::fs::copy(src, dst).map_err(|e| StepError::io(dst, e))?;
        std::fs::remove_file(src).map_err(|e| StepError::io(src, e))?;
        Ok(())
    }

    fn copy_file(&self, src: &Path, dst: &Path) -> Result<(), StepError> {
        std::fs::copy(src, dst).map_err(|e| StepError::io(dst, e))?;
        Ok(())
    }

    fn remove_file(&self, path: &Path) -> Result<(), StepError> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StepError::io(path, e)),
        }
    }

    fn pg_db_initialized(&self, db: &str) -> Result<bool, StepError> {
        let sql = "SELECT 1 FROM information_schema.tables \
                   WHERE table_schema='public' AND table_name='ir_module_module';";
        let out = capture_command(
            "sudo",
            &[
                "-n", "-Hiu", "postgres", "--", "psql", "-d", db, "-tAc", sql,
            ],
        )?;
        Ok(!out.trim().is_empty())
    }

    fn odoo_init_base(
        &self,
        user: &str,
        python: &Path,
        odoo_bin: &Path,
        conf: &Path,
        db: &str,
    ) -> Result<(), StepError> {
        let python = python.to_string_lossy();
        let odoo_bin = odoo_bin.to_string_lossy();
        let conf = conf.to_string_lossy();
        run_command(
            "sudo",
            &[
                "-n",
                "-u",
                user,
                "--",
                &python,
                &odoo_bin,
                "-c",
                &conf,
                "-d",
                db,
                "-i",
                "base",
                "--without-demo=all",
                "--stop-after-init",
            ],
        )
    }

    fn getent_home(&self, user: &str) -> Result<Option<String>, StepError> {
        let output = Command::new("getent")
            .args(argv::getent_passwd(user))
            .output()
            .map_err(|e| StepError::CommandFailed {
                command: format!("getent passwd {user}"),
                status: "spawn-failed".to_string(),
                stderr: e.to_string(),
            })?;
        if !output.status.success() {
            return Ok(None); // user not found
        }
        let line = String::from_utf8_lossy(&output.stdout);
        let home = line
            .trim()
            .split(':')
            .nth(5)
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());
        Ok(home)
    }

    fn chown_to_user(&self, path: &Path, user: &str) -> Result<(), StepError> {
        let u = nix::unistd::User::from_name(user)
            .ok()
            .flatten()
            .ok_or_else(|| StepError::Precondition(format!("user '{user}' not found for chown")))?;
        // the same-named group if it exists, else the user's primary group.
        let gid = nix::unistd::Group::from_name(user)
            .ok()
            .flatten()
            .map(|g| g.gid)
            .unwrap_or(u.gid);
        nix::unistd::chown(path, Some(u.uid), Some(gid))
            .map_err(|e| StepError::io(path, errno_io(e)))
    }

    fn append_line(&self, path: &Path, line: &str) -> Result<(), StepError> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| StepError::io(path, e))?;
        writeln!(file, "{line}").map_err(|e| StepError::io(path, e))
    }
}

/// the network-download boundary, separate from [`SystemOps`] so tests can mock
/// it without touching the network.
pub trait Downloader {
    /// downloads `url` to `dest`. verifying integrity is the caller's job (see
    /// [`sha256_hex`]): a download is not trusted by itself.
    fn download(&self, url: &str, dest: &Path) -> Result<(), StepError>;
}

/// the real downloader, through `wget`.
#[derive(Debug, Default)]
pub struct RealDownloader;

impl RealDownloader {
    pub fn new() -> Self {
        Self
    }
}

impl Downloader for RealDownloader {
    fn download(&self, url: &str, dest: &Path) -> Result<(), StepError> {
        // **we** create the destination, fail-closed, before wget sees the path
        // (A-V3-3): `wget -O` opens by name and follows symlinks, so on its own
        // it would happily write wherever a planted link points.
        create_private_file_at(dest, "")?;
        let rendered = dest.to_string_lossy();
        // a download truncated by the kill is not installable anyway: the
        // caller verifies the checksum and removes the partial file.
        run_network_command("wget", &["-q", "-O", &rendered, url])
    }
}

/// creates a private (`0600`) file, fail-closed with `O_CREAT | O_EXCL |
/// O_NOFOLLOW`.
///
/// the body of [`SystemOps::create_private_file`], pulled out as a free
/// function because [`RealDownloader`] needs it too, without a `SystemOps` at
/// hand (A-V3-3). one implementation of the delicate primitive: if it changes,
/// it changes for everyone.
pub fn create_private_file_at(path: &Path, content: &str) -> Result<(), StepError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true) // O_CREAT | O_EXCL
        .custom_flags(nix::libc::O_NOFOLLOW)
        .mode(PRIVATE_FILE_MODE)
        .open(path)
        .map_err(|e| StepError::io(path, e))?;
    file.write_all(content.as_bytes())
        .map_err(|e| StepError::io(path, e))
}

/// the SHA-256 of a file, as a lowercase hex string.
pub fn sha256_hex(path: &Path) -> Result<String, StepError> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path).map_err(|e| StepError::io(path, e))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|e| StepError::io(path, e))?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
