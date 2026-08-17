//! **non-mutating** preflight checks: preconditions verified before any step.
//! none of these functions touches the system — they measure, and that is all.
//!
//! key correction over Bash (**C4**): `check_disk` no longer creates the
//! directory in order to measure it, and creating `/opt/odoo` is now a
//! reversible step rather than a check's side effect.
//!
//! the paths are **injectable**, so the tests run without root and without
//! touching `/opt/odoo`.

use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::{info, warn};

use crate::distro::OsFamily;
use crate::packaging::{AlternatePython, Availability, DepId, PackageSpec};

/// default `os-release` path.
pub const OS_RELEASE_PATH: &str = "/etc/os-release";
/// default free-space threshold, in GB.
pub const DEFAULT_MIN_DISK_GB: u64 = 5;

/// OS details read from `os-release`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsInfo {
    pub id: String,
    pub version: String,
    pub codename: Option<String>,
    /// the family `id` belongs to.
    ///
    /// **never a fallback**: an unknown `id` is rejected by [`check_os_from`]
    /// before this struct exists. worth noting because the type has a
    /// `Default`, which serves manifest compatibility, not holes here.
    pub family: OsFamily,
}

/// a precondition error. checks have no `undo`, because they do not mutate.
#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    #[error(
        "this installer must be run as root (expected EUID 0, found {euid}). retry with: sudo ..."
    )]
    NotRoot { euid: u32 },

    #[error(
        "run it through sudo from a normal user (SUDO_USER is unset). \
         do not use 'sudo -i', 'su -' or a direct root login"
    )]
    NoSudoUser,

    #[error("OS release file not found: {0}. the operating system cannot be identified")]
    OsReleaseNotFound(PathBuf),

    #[error("cannot determine the OS from {path}: {reason}")]
    OsReleaseParse { path: PathBuf, reason: String },

    #[error(
        "unsupported operating system '{id}'. \
         supported: Ubuntu >= 22.04, Debian >= 11, Fedora >= 40"
    )]
    UnsupportedOs { id: String },

    #[error(
        "unsupported operating system: {id} {version}. \
         minimum version: Ubuntu 22.04, Debian 11, Fedora 40"
    )]
    UnsupportedVersion { id: String, version: String },

    #[error(
        "not enough space on {target}: {available_gb} GB available, {required_gb} GB required"
    )]
    InsufficientDisk {
        target: PathBuf,
        available_gb: u64,
        required_gb: u64,
    },

    #[error("cannot measure the disk space on {path}: {reason}")]
    DiskProbe { path: PathBuf, reason: String },

    #[error("port {port} already in use: free it before proceeding")]
    PortInUse { port: u16 },

    #[error(
        "a mandatory system command is missing: {command}. \
         this needs a system with systemd and its family's package manager \
         (apt-get on Debian/Ubuntu, dnf on Fedora)"
    )]
    MissingCommand { command: String },
}

// --- root and sudo ----------------------------------------------------------

/// pure: is the EUID 0? testable without being root.
pub fn ensure_root_euid(euid: u32) -> Result<(), CheckError> {
    if euid == 0 {
        Ok(())
    } else {
        Err(CheckError::NotRoot { euid })
    }
}

/// are we root? a question, not a check: no error, no log.
///
/// used by `--dry-run`, which may legitimately run unprivileged but then sees
/// less (A-V3-11).
pub fn running_as_root() -> bool {
    nix::unistd::geteuid().is_root()
}

/// requires the installer to run as root.
///
/// # errors
///
/// [`CheckError::NotRoot`] with the observed EUID.
pub fn check_root() -> Result<(), CheckError> {
    let euid = nix::unistd::geteuid().as_raw();
    ensure_root_euid(euid)?;
    info!("✔ running as root confirmed");
    Ok(())
}

/// pure: `SUDO_USER` must be present and non-empty.
pub fn ensure_sudo_user(value: Option<&str>) -> Result<(), CheckError> {
    match value {
        Some(user) if !user.is_empty() => Ok(()),
        _ => Err(CheckError::NoSudoUser),
    }
}

/// requires the installer to be started through `sudo` by a normal user.
///
/// # errors
///
/// [`CheckError::NoSudoUser`] under `sudo -i` or `su -`.
pub fn check_sudo_user() -> Result<(), CheckError> {
    let value = std::env::var("SUDO_USER").ok();
    ensure_sudo_user(value.as_deref())?;
    info!(sudo_user = ?value, "✔ running through sudo confirmed");
    Ok(())
}

/// checks about the **caller**: who is running the installer.
///
/// kept apart from the environment checks because they belong at a different
/// moment (A-R9-1). these answer *who are you*, and the answer is needed at
/// once: the manifest is `0600 root`, so without them an unprivileged reader
/// gets "permission denied" on a file they know nothing about. the environment
/// checks answer *is this machine suitable*, and only matter once we know the
/// installation should happen at all.
///
/// # errors
///
/// propagates [`check_root`] and [`check_sudo_user`].
pub fn check_caller() -> Result<(), CheckError> {
    check_root()?;
    check_sudo_user()
}

// --- OS ---------------------------------------------------------------------

/// reads one key out of an `os-release` file, unquoted.
fn os_release_value(content: &str, key: &str) -> Option<String> {
    for line in content.lines() {
        let line = line.trim();
        if let Some((k, v)) = line.split_once('=') {
            if k.trim() == key {
                return Some(strip_quotes(v.trim()));
            }
        }
    }
    None
}

/// strips one pair of quotes wrapping the whole value.
fn strip_quotes(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

/// reads and validates the OS from an `os-release` file.
///
/// # errors
///
/// [`CheckError::OsReleaseNotFound`], [`CheckError::OsReleaseParse`],
/// [`CheckError::UnsupportedOs`] or [`CheckError::UnsupportedVersion`].
pub fn check_os_from(path: &Path) -> Result<OsInfo, CheckError> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            CheckError::OsReleaseNotFound(path.to_path_buf())
        } else {
            CheckError::OsReleaseParse {
                path: path.to_path_buf(),
                reason: e.to_string(),
            }
        }
    })?;

    let id = os_release_value(&content, "ID")
        .map(|s| s.to_ascii_lowercase())
        .ok_or_else(|| CheckError::OsReleaseParse {
            path: path.to_path_buf(),
            reason: "the ID key is missing".to_string(),
        })?;
    let version =
        os_release_value(&content, "VERSION_ID").ok_or_else(|| CheckError::OsReleaseParse {
            path: path.to_path_buf(),
            reason: "the VERSION_ID key is missing".to_string(),
        })?;
    let codename = os_release_value(&content, "VERSION_CODENAME");

    // the family is the **first** gate and the only place that decides whether
    // a distribution is known: keeping it here stops that list living in two
    // places. `validate_os` handles version thresholds only.
    let family =
        OsFamily::from_os_id(&id).ok_or_else(|| CheckError::UnsupportedOs { id: id.clone() })?;

    let info = OsInfo {
        id,
        version,
        codename,
        family,
    };
    validate_os(&info)?;
    Ok(info)
}

/// the `ID` an `os-release` declares, **unvalidated**.
///
/// separate from [`check_os_from`] because a rollback must run even on a system
/// we would refuse to install on: uninstalling does not require the machine to
/// still be suitable. only ever used to **warn** about a mismatch with the
/// manifest, never to decide an action.
pub fn os_id_from(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    os_release_value(&content, "ID").map(|s| s.to_ascii_lowercase())
}

/// newest Ubuntu the integration CI really installs on.
pub const NEWEST_TESTED_UBUNTU: (u32, u32) = (24, 4);
/// newest Debian the integration CI really runs on.
pub const NEWEST_TESTED_DEBIAN: (u32, u32) = (12, 0);
/// newest Fedora the integration CI really runs on, full cycle, in a container
/// with systemd as PID 1.
///
/// this constant **follows the workflow matrix**, and a test makes that
/// mandatory: if they diverged, [`is_newer_than_tested`]'s warning would lie in
/// one direction or the other.
///
/// **44** since M11, and it installs differently from 41: there the system
/// `python3` is covered by Odoo's pins, here it is 3.14 and the venv is built
/// on `python3.13`. it promises nothing about that release's system Python —
/// that is [`NEWEST_TESTED_PYTHON`], which deliberately does not rise with it.
pub const NEWEST_TESTED_FEDORA: (u32, u32) = (44, 0);

/// the newest tested release for `id`, with its display name.
///
/// **one table**, and not for brevity: `is_newer_than_tested` decides *whether*
/// to warn and [`untested_release_warning`] decides *what to say*. two tables
/// can diverge in silence, which is exactly A-MD-5 — the threshold was right
/// and the message named another family.
///
/// an unknown `ID` never reaches here, so `None` means "no upper threshold to
/// compare against", not a fallback.
fn tested_release(id: &str) -> Option<(&'static str, (u32, u32))> {
    match id {
        "ubuntu" => Some(("Ubuntu", NEWEST_TESTED_UBUNTU)),
        "debian" => Some(("Debian", NEWEST_TESTED_DEBIAN)),
        "fedora" => Some(("Fedora", NEWEST_TESTED_FEDORA)),
        _ => None,
    }
}

/// renders `(major, minor)` the way the distribution writes it: `24.04`, not
/// `24.4`.
///
/// public so it can be exercised on values no constant currently has — `(25,
/// 10)` must give `25.10`, not `25.010` — which is unreachable through the
/// constants alone. `minor == 0` is omitted, because nobody writes "Fedora
/// 41.0".
pub fn format_release((major, minor): (u32, u32)) -> String {
    if minor == 0 {
        format!("{major}")
    } else {
        format!("{major}.{minor:02}")
    }
}

/// is this release **newer** than the last one we really exercised?
///
/// A5.3. [`validate_os`]'s thresholds are open upwards and must stay so — a
/// refusal without evidence blocks the good case — but "we accept" must not
/// mean "we keep quiet": whoever installs on an unexercised release needs that
/// fact when something goes wrong. pure, so the upper bound is checkable
/// without that OS at hand.
pub fn is_newer_than_tested(id: &str, version: &str) -> bool {
    release_to_flag(id, version).is_some()
}

/// the tested release **to cite**, when there is something to report.
///
/// one place decides *whether* to warn and already returns what is needed to
/// say it, so there is no case where one function says "yes" and the other has
/// nothing to name.
fn release_to_flag(id: &str, version: &str) -> Option<(&'static str, (u32, u32))> {
    let (name, tested) = tested_release(id)?;
    (parse_version(version) > tested).then_some((name, tested))
}

/// the warning text, or `None` when there is nothing to report.
///
/// **A-MD-5**: this used to be a string hardcoded inside [`check_os`] naming
/// "Ubuntu 24.04, Debian 12" to everyone — including Fedora users, who were
/// never told the one release that had actually been exercised. the constants
/// existed and were right; the message did not read them.
///
/// names **only the family being installed on**, and returns the text rather
/// than logging it: when a check's value is in its wording, asserting its
/// outcome asserts nothing (A-R9-1).
pub fn untested_release_warning(id: &str, version: &str) -> Option<String> {
    let (name, tested) = release_to_flag(id, version)?;
    Some(format!(
        "this release is newer than {name} {}, the latest one the installer is tested on: \
         the installation carries on, but the package names and the wkhtmltopdf package may \
         not be the right ones. if something does not add up, this is the first place to \
         look.",
        format_release(tested)
    ))
}

// --- the Python interpreter (A-MD-7) ----------------------------------------

/// the newest CPython an installation **reaches the end** on, for the Odoo
/// versions that have no exception.
///
/// not the newest that exists, nor the one that "should work": the one the
/// integration CI completes a full cycle on. **revisit it when the matrix
/// moves.**
///
/// unlike [`NEWEST_TESTED_FEDORA`] no test ties it to the workflow: the CI file
/// names the *image*, not the Python inside it, and inventing an image→Python
/// table would be a second source of truth that can diverge in silence.
pub const NEWEST_TESTED_PYTHON: (u32, u32) = (3, 13);

/// the ceiling **for a given Odoo** — the same number, gaining the dimension it
/// always implicitly had (A-V3-29).
///
/// what an installation completes is a **pair**, `Odoo version × Python`: that
/// is what `A-V3-26` established when it built the grid, and a single number
/// silently claimed every branch shared one ceiling. `A-V3-28` had already
/// named the flaw while fixing only the message — *"what breaks the build is
/// «does **this Odoo** pin a wheel for **this** interpreter?», and the version
/// is not an input of that constant"*. Here it becomes one.
///
/// it is still **one source for two uses** — the warning and the choice — so
/// there is no second table to diverge (A-MD-5).
///
/// # where these numbers come from, and how to redo them
///
/// **read from Odoo's own `requirements.txt`**, not inferred from release
/// dates. Every branch pins `gevent` in brackets keyed on `python_version`, and
/// what decides is whether the **newest bracket has an upper bound**:
///
/// ```text
/// curl -fsSL https://raw.githubusercontent.com/odoo/odoo/<branch>/requirements.txt \
///   | grep -E '^gevent'
/// ```
///
/// - **17, 18, 19** — `gevent==24.11.1 ; python_version >= '3.13'`: a release
///   whose wheels include cp313. Ceiling [`NEWEST_TESTED_PYTHON`].
/// - **16** — `gevent==24.2.1 ; python_version >= '3.12'`, and **nothing above
///   it**. Past 3.12 pip keeps choosing 24.2.1, whose newest wheel is cp312, so
///   it must build from source — and the C those versions generate does not
///   survive a newer CPython's headers. Ceiling **3.12**.
///
/// so the exception exists because 16 is the only branch whose newest bracket
/// is **unbounded**, not because it is old. Measured 2026-08-17 on Fedora 44:
/// `gevent-24.2.1-cp312-…-manylinux…whl` installs from a wheel, no compiler
/// involved.
///
/// the day Odoo adds a bracket above, this table is **re-read** with the
/// command above — not deduced.
pub fn newest_tested_python(odoo_version_short: &str) -> (u32, u32) {
    match odoo_version_short {
        "16" => (3, 12),
        _ => NEWEST_TESTED_PYTHON,
    }
}

/// `Python 3.14.0` → `(3, 14)`.
///
/// `None` when the line is not what we expect: from output we cannot read,
/// **nothing** is concluded — least of all "all is well". "old enough" and
/// "unknown" have different outcomes here.
pub fn parse_python_version(output: &str) -> Option<(u32, u32)> {
    // some builds add a suffix (`3.14.0rc1`, `+`, `free-threading build`), so
    // take the first two numeric components and drop the rest.
    let rest = output.split_whitespace().nth(1)?;
    let mut parts = rest.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts
        .next()?
        .trim_matches(|c: char| !c.is_ascii_digit())
        .parse()
        .ok()?;
    Some((major, minor))
}

/// the tested Python **to cite**, when there is something to report.
///
/// same shape as [`release_to_flag`], and for the same reason: two tables can
/// diverge in silence.
fn python_to_flag(python: (u32, u32), odoo_version_short: &str) -> Option<(u32, u32)> {
    let tested = newest_tested_python(odoo_version_short);
    (python > tested).then_some(tested)
}

/// `true` when this interpreter is newer than the last one the installer
/// reaches the end on. pure: the interesting case needs no such Python.
pub fn python_is_newer_than_tested(python: (u32, u32), odoo_version_short: &str) -> bool {
    python_to_flag(python, odoo_version_short).is_some()
}

/// how a Python version is written: `3.14`, never `3.140`.
pub fn format_python((major, minor): (u32, u32)) -> String {
    format!("{major}.{minor}")
}

/// the warning text, or `None` when there is nothing to report.
///
/// **A-MD-7**: on an interpreter newer than Odoo's pins the installation dies
/// building `gevent`, and the diagnosis — "Odoo pins no gevent for this Python"
/// — is not recoverable from a wall of `gcc` errors. said **here**, before the
/// decision to go ahead.
///
/// a warning and **not** a refusal (A5.1-bis): the day Odoo raises the pin, a
/// refusal would block a working installation.
pub fn untested_python_warning(python: (u32, u32), odoo_version_short: &str) -> Option<String> {
    let tested = python_to_flag(python, odoo_version_short)?;
    Some(format!(
        "this system has Python {}, newer than Python {} — the latest one the installer \
         completes an installation on. Odoo pins gevent and greenlet per interpreter version: \
         with no pin for this Python there is no prebuilt wheel, pip has to compile, and the \
         `install-python-requirements` step fails. this is the first place to look.",
        format_python(python),
        format_python(tested)
    ))
}

/// the version of **this** interpreter, or `None` when unknown.
///
/// the name is a parameter rather than a hardcoded `python3` (M11): "which
/// Python is installed" and "which Python will we use" became two questions,
/// and the caller must say which one it is asking.
pub fn python_version(command: &str) -> Option<(u32, u32)> {
    let out = capture(Command::new(command).arg("--version"))?;
    parse_python_version(&out)
}

// --- choosing the interpreter (M11) -----------------------------------------

/// the interpreter the venv will be built on, and what it takes to have it.
///
/// **configuration decided at preflight**, not a step result: it lives in the
/// [`Context`](crate::context::Context) like `os_family`, because two steps
/// must use the same answer — one installs the interpreter, the other invokes
/// it.
///
/// the `Default` is the historical behaviour, so any path that decides nothing
/// behaves as it did before M11.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonPlan {
    /// the command that creates the venv: `python3`, or `python3.13`.
    pub command: String,
    /// packages needed for that command to exist **and** be able to build
    /// extensions. empty when using the system interpreter.
    pub packages: Vec<String>,
    /// what this plan makes pointless: the system Python's headers, when the
    /// venv is built on another interpreter.
    ///
    /// not hardcoded here — the preflight asks the family's catalogue, which is
    /// the only thing that knows their names.
    pub supersedes: Vec<String>,
}

impl Default for PythonPlan {
    fn default() -> Self {
        PythonPlan {
            command: "python3".to_string(),
            packages: Vec::new(),
            supersedes: Vec::new(),
        }
    }
}

impl PythonPlan {
    /// `true` when the system interpreter is used, so nothing is installed.
    pub fn is_system(&self) -> bool {
        self.packages.is_empty()
    }

    /// adapts a list of package groups to the chosen interpreter.
    ///
    /// with the system interpreter it returns the list **unchanged**, which is
    /// what makes M11 invisible to Debian, Ubuntu and every Fedora up to 42.
    ///
    /// with an alternative one the system Python's headers drop out — nothing
    /// would use them, and they would pad the delta — and the interpreter and
    /// its own headers come in. pure, so the rule is checkable without dnf or a
    /// Fedora at hand.
    pub fn adapt_specs(&self, specs: &[PackageSpec]) -> Vec<PackageSpec> {
        if self.is_system() {
            return specs.to_vec();
        }
        let mut out: Vec<PackageSpec> = specs
            .iter()
            .filter(|spec| {
                !spec
                    .alternatives()
                    .iter()
                    .any(|name| self.supersedes.contains(name))
            })
            .cloned()
            .collect();
        out.extend(self.packages.iter().map(|p| PackageSpec::one(p)));
        out
    }
}

/// chooses the interpreter. **pure**: measured inputs, a decision out.
///
/// the rule in one line: *the system Python if Odoo's pins cover it, otherwise
/// the newest packaged interpreter that they do.* [`NEWEST_TESTED_PYTHON`],
/// introduced to **warn**, is here the input of the **choice** — one number,
/// two uses, no second table to diverge (A-MD-5).
///
/// three cases, none of them a refusal:
/// - system covered → use it, install nothing;
/// - not covered, alternative available → use the alternative;
/// - not covered, no alternative → **use the system one anyway**, with the
///   warning and, if the build then fails, the diagnosis. a refusal without
///   evidence blocks the good case (A5.1-bis).
///
/// `system == None` behaves as "covered": nothing is concluded from absent
/// information, least of all installing a second interpreter on a customer's
/// machine.
pub fn choose_python(
    system: Option<(u32, u32)>,
    available: &[AlternatePython],
    system_dev_names: &[String],
    odoo_version_short: &str,
) -> PythonPlan {
    let Some(version) = system else {
        return PythonPlan::default();
    };
    if !python_is_newer_than_tested(version, odoo_version_short) {
        return PythonPlan::default();
    }
    let scelto = available
        .iter()
        .filter(|alt| !python_is_newer_than_tested(alt.version, odoo_version_short))
        .max_by_key(|alt| alt.version);
    match scelto {
        None => PythonPlan::default(),
        Some(alt) => PythonPlan {
            command: alt.interpreter.clone(),
            packages: vec![alt.interpreter.clone(), alt.devel.clone()],
            supersedes: system_dev_names.to_vec(),
        },
    }
}

/// decides the plan by interrogating the system, and **says so**.
///
/// the impure wrapper around [`choose_python`]: reads the system version, asks
/// the catalogue which alternative interpreters this family has, and asks the
/// package manager which of those are actually installable here.
///
/// blindness is not absence: with an unqueryable index, "not available" does
/// not mean "does not exist", so we carry on with the system interpreter
/// **saying the probe was blind** (A5.1-bis).
pub fn plan_python(ops: &dyn crate::system_ops::SystemOps, odoo_version_short: &str) -> PythonPlan {
    let system = ops.python_version("python3");
    let catalog = ops.packages().catalog();
    let candidates: Vec<AlternatePython> = catalog
        .alternate_pythons
        .iter()
        .filter(|alt| !python_is_newer_than_tested(alt.version, odoo_version_short))
        .cloned()
        .collect();

    let available: Vec<AlternatePython> = if candidates.is_empty() {
        Vec::new()
    } else if !ops.packages().index_is_queryable() {
        warn!(
            "the package index cannot be queried, so there is no way to tell whether this \
             distribution offers an alternative Python: proceeding with the system one"
        );
        Vec::new()
    } else {
        candidates
            .into_iter()
            .filter(|alt| ops.packages().availability(&alt.interpreter) == Availability::Real)
            .collect()
    };

    let plan = choose_python(
        system,
        &available,
        &catalog.names_for(DepId::PythonDev),
        odoo_version_short,
    );
    announce_python_plan(system, &plan, odoo_version_short);
    plan
}

/// says which interpreter will be used and why. decides nothing: it
/// **reports**.
///
/// separate from [`plan_python`] because a message only checkable by capturing
/// logs is a message no test looks at (A-R9-1).
fn announce_python_plan(system: Option<(u32, u32)>, plan: &PythonPlan, odoo_version_short: &str) {
    match (system, plan.is_system()) {
        (None, _) => info!("ℹ the Python version cannot be determined at this stage: proceeding"),
        (Some(v), true) => match untested_python_warning(v, odoo_version_short) {
            // not covered and no alternative: the M10 case, where the warning
            // already says what will break and where.
            Some(warning) => warn!(python = %format_python(v), "{warning}"),
            None => info!(python = %format_python(v), "✔ Python interpreter"),
        },
        (Some(v), false) => info!(
            system_python = %format_python(v),
            interpreter = %plan.command,
            packages = %plan.packages.join(" "),
            "the system Python is newer than Odoo's pins: the virtualenv will be built on a \
             supported interpreter, installed for the purpose and removed by the rollback"
        ),
    }
}

// `check_python` is gone: the warning is not a check of its own but a branch of
// `announce_python_plan`. after M11 the same measurement leads to two outcomes
// — warn, or install another interpreter — and two functions would have
// diverged. `None` stays silent: on a minimal image `python3` arrives with the
// packages, i.e. after the preflight.

/// applies the minimum version thresholds.
///
/// **does not** decide whether the distribution is known: [`check_os_from`] is
/// the only gate for that, so the `match` here is exhaustive by construction
/// and has no unreachable "unknown distribution" branch.
///
/// the Fedora threshold of **40** is prudent rather than measured: the oldest
/// release still supported upstream when the dnf backend was written. as for
/// the others it is open upwards, with [`is_newer_than_tested`]'s warning.
///
/// # errors
///
/// [`CheckError::UnsupportedVersion`] below a family's threshold.
pub fn validate_os(info: &OsInfo) -> Result<(), CheckError> {
    let (major, minor) = parse_version(&info.version);
    match info.family {
        OsFamily::Debian => {
            let too_old = if info.id == "ubuntu" {
                major < 22 || (major == 22 && minor < 4)
            } else {
                major < 11
            };
            if too_old {
                return Err(CheckError::UnsupportedVersion {
                    id: info.id.clone(),
                    version: info.version.clone(),
                });
            }
            Ok(())
        }
        OsFamily::Fedora => {
            if major < 40 {
                return Err(CheckError::UnsupportedVersion {
                    id: info.id.clone(),
                    version: info.version.clone(),
                });
            }
            Ok(())
        }
    }
}

/// extracts `(major, minor)` from a version string like `"22.04"` or `"12"`.
/// missing or non-numeric components count as 0.
fn parse_version(version: &str) -> (u32, u32) {
    let mut parts = version.split('.');
    let major = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (major, minor)
}

/// validates the OS from the default `os-release` path.
///
/// # errors
///
/// as [`check_os_from`].
pub fn check_os() -> Result<OsInfo, CheckError> {
    let info = check_os_from(Path::new(OS_RELEASE_PATH))?;
    info!(
        os = %info.id,
        version = %info.version,
        codename = ?info.codename,
        "✔ supported OS"
    );
    if let Some(avviso) = untested_release_warning(&info.id, &info.version) {
        warn!(os = %info.id, version = %info.version, "{avviso}");
    }
    Ok(info)
}

// --- disk (non-mutating: C4 fixed) ------------------------------------------

/// walks up to the first **existing** ancestor of `path`, `/` at worst.
///
/// creates nothing: this is the C4 fix — measure without creating.
fn nearest_existing_ancestor(path: &Path) -> PathBuf {
    let mut current = path;
    loop {
        if current.exists() {
            return current.to_path_buf();
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => return current.to_path_buf(),
        }
    }
}

/// checks free space on `target`'s filesystem **without creating** `target`.
///
/// # errors
///
/// [`CheckError::InsufficientDisk`] below the threshold, or
/// [`CheckError::DiskProbe`] when `statvfs` fails.
pub fn check_disk(target: &Path, required_gb: u64) -> Result<(), CheckError> {
    let measure = nearest_existing_ancestor(target);

    let stat =
        nix::sys::statvfs::statvfs(measure.as_path()).map_err(|e| CheckError::DiskProbe {
            path: measure.clone(),
            reason: e.to_string(),
        })?;

    // space available to an unprivileged user: blocks * fragment size.
    let available_bytes =
        (stat.blocks_available() as u64).saturating_mul(stat.fragment_size() as u64);
    let available_gb = available_bytes / (1024 * 1024 * 1024);

    info!(
        target = %target.display(),
        measured_on = %measure.display(),
        available_gb,
        required_gb,
        "checking the disk space"
    );

    if available_gb < required_gb {
        return Err(CheckError::InsufficientDisk {
            target: target.to_path_buf(),
            available_gb,
            required_gb,
        });
    }
    Ok(())
}

// --- ports ------------------------------------------------------------------

/// outcome of probing one port.
#[derive(Debug, PartialEq, Eq)]
pub enum PortStatus {
    Free,
    InUse,
    /// no probe tool available: non-blocking.
    Unknown,
}

/// checks that the required ports are free. an `Unknown` port counts as free,
/// with a non-blocking warning.
///
/// # errors
///
/// [`CheckError::PortInUse`] naming the busy port.
pub fn check_ports(
    odoo_port: u16,
    gevent_port: u16,
    with_nginx: bool,
    nginx_already_serving: bool,
) -> Result<(), CheckError> {
    for port in ports_to_check(odoo_port, gevent_port, with_nginx, nginx_already_serving) {
        match probe_port(port) {
            PortStatus::Free => info!(port, "✔ port available"),
            PortStatus::Unknown => {
                warn!(
                    port,
                    "cannot check the port (ss/netstat/lsof absent): assuming it is free"
                )
            }
            PortStatus::InUse => return Err(CheckError::PortInUse { port }),
        }
    }
    Ok(())
}

/// which ports are worth checking. a **pure** decision, split from the probe.
///
/// split because the interesting case — nginx already listening on 80 — is not
/// reproducible in a test: it depends on what runs on the test machine, where a
/// wrong check would pass anyway. as a return value the rule is checkable in
/// both directions.
pub fn ports_to_check(
    odoo_port: u16,
    gevent_port: u16,
    with_nginx: bool,
    nginx_already_serving: bool,
) -> Vec<u16> {
    // both, and the gevent one is not a detail: it is the port nobody names in
    // a `.env`, so it is the one a second instance takes without noticing. Odoo
    // binds it at **startup**, so a conflict there does not fail the
    // installation — it fails the service, later, on somebody else's machine.
    let mut ports = vec![odoo_port, gevent_port];
    // 80 and 443 are only checked when they must be taken by an nginx that is
    // **not already serving** (A-V3-15). adding a vhost to a running reverse
    // proxy is the supported scenario, and there port 80 is held by the very
    // program we are configuring — same distinction as
    // `InstallState::owns_the_http_port`. if nginx is not serving and 80 is
    // taken, the conflict is real and the check must say no.
    if with_nginx && !nginx_already_serving {
        ports.push(80);
        ports.push(443);
    } else if with_nginx {
        info!("nginx is already listening: ports 80/443 are its own, no conflict to check");
    }
    ports
}

/// probes a port through the `ss → netstat → lsof` cascade.
fn probe_port(port: u16) -> PortStatus {
    if command_exists("ss") {
        if let Some(out) = capture(Command::new("ss").args(["-lntuH"])) {
            return classify_listing(&out, port);
        }
    }
    if command_exists("netstat") {
        if let Some(out) = capture(Command::new("netstat").args(["-lntu"])) {
            return classify_listing(&out, port);
        }
    }
    if command_exists("lsof") {
        let arg = format!("-iTCP:{port}");
        if let Some(out) = capture(Command::new("lsof").args([arg.as_str(), "-sTCP:LISTEN"])) {
            return if out.trim().is_empty() {
                PortStatus::Free
            } else {
                PortStatus::InUse
            };
        }
    }
    PortStatus::Unknown
}

/// looks for `:PORT` followed by a space in ss/netstat output.
fn classify_listing(listing: &str, port: u16) -> PortStatus {
    let needle = format!(":{port} ");
    if listing.lines().any(|line| line.contains(&needle)) {
        PortStatus::InUse
    } else {
        PortStatus::Free
    }
}

/// runs a command capturing stdout; `None` when it is not executable.
fn capture(cmd: &mut Command) -> Option<String> {
    cmd.output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
}

// --- commands ---------------------------------------------------------------

/// `true` when `command` exists and is executable somewhere on `PATH`.
///
/// does not run it: only scans the filesystem.
fn command_exists(command: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        let candidate = dir.join(command);
        candidate
            .metadata()
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    })
}

/// the commands that **must** be present, for this family.
///
/// pure, so the choice is checkable without those commands on the test machine
/// — otherwise it would only be checkable on a real Fedora, i.e. in no test at
/// all.
///
/// until M2 this list named `apt-get` outright, so a Fedora run stopped here,
/// before anything else. the package manager is by definition what differs
/// between families.
pub fn required_commands(family: OsFamily) -> [&'static str; 2] {
    match family {
        OsFamily::Debian => ["apt-get", "systemctl"],
        OsFamily::Fedora => ["dnf", "systemctl"],
    }
}

/// checks the system prerequisites the installer cannot install itself: the
/// family's package manager and `systemctl`. `nginx` and `certbot` are
/// optional, so they are info only.
///
/// # errors
///
/// [`CheckError::MissingCommand`] naming the first one missing.
pub fn check_commands(family: OsFamily) -> Result<(), CheckError> {
    for command in required_commands(family) {
        if command_exists(command) {
            info!(command, "✔ present");
        } else {
            return Err(CheckError::MissingCommand {
                command: command.to_string(),
            });
        }
    }
    for command in ["nginx", "certbot"] {
        if command_exists(command) {
            info!(command, "✔ present");
        } else {
            info!(command, "ℹ optional, not found (installable if needed)");
        }
    }
    Ok(())
}
