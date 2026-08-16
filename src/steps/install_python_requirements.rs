//! [`InstallPythonRequirements`]: installs the pip dependencies into the venv.
//!
//! # the conceptual heart: no undo of its own
//!
//! the packages live **inside** the venv, so
//! [`CreateVirtualenv`](crate::steps::create_virtualenv)'s undo removes them
//! all. this step's undo is therefore a **documented no-op**: uninstalling
//! packages one by one would be redundant and fragile. every install runs as
//! the **odoo** user, through the venv's `pip`.
//!
//! # pip's cache lives inside our perimeter (A-R5-3)
//!
//! `pip` caches into `$HOME/.cache`, and the `odoo` user's home is the
//! directory the installer marks `Preexisting` and never touches. observed in
//! the field: after a complete rollback, that cache was still there.
//!
//! the correction is **preventive**, not a cleanup: `--cache-dir` moves it
//! inside the venv, which the undo removes wholesale. nothing is born outside
//! the perimeter, so nothing has to be chased with deletion heuristics inside
//! the customer's home — and it stays a cache, so a second run finds it and
//! does not re-download the wheels.
//!
//! # the Cython/gevent workaround (a real fix, to be preserved)
//!
//! Cython 3 removed Python 2's `long` type, and gevent still uses that code in
//! its `.pyx` files. pip builds wheels in an isolated environment that ignores
//! the venv's Cython, so: install `Cython<3` into the venv, then gevent with
//! `--no-build-isolation`, then the rest of the requirements **excluding** it.
//!
//! # which gevent: pip decides, not us (A-R6-3)
//!
//! Odoo's `requirements.txt` does not pin **one** gevent version: it pins four,
//! one per Python version, and as many greenlets, annotated by Odoo itself with
//! the Ubuntu release:
//!
//! ```text
//! gevent==21.8.0  ; … python_version == '3.10'              # (Jammy)
//! gevent==24.2.1  ; … python_version >= '3.12' and < '3.13' # (Noble)
//! greenlet==1.1.2 ; … python_version == '3.10'              # (Jammy)
//! greenlet==3.0.3 ; … python_version >= '3.12' and < '3.13' # (Noble)
//! ```
//!
//! the first version of this step took **the first line** starting with
//! `gevent` and threw the marker away. on 22.04 that is the right line by
//! coincidence; on 24.04 it still picked the older one, which **does not
//! compile** against Python 3.12. no setuptools could have saved it: it was the
//! wrong version.
//!
//! the marker was dropped because `--no-build-isolation` does not tolerate one
//! *on argv* — true, and a problem we made for ourselves. passing a
//! requirements **file** keeps the markers, and pip evaluates them, which is
//! its job. we stop choosing.
//!
//! a welcome side effect: with the right version a prebuilt wheel exists, so
//! nothing is compiled and `--no-build-isolation` stays inert. the Cython<3
//! workaround does its work where it is genuinely needed.
//!
//! # which setuptools: the newest is not a neutral choice (A-V3-26)
//!
//! the same lesson one level down. this step needs setuptools in the venv to
//! have a build backend (A-R6-2), and asked for it with a bare `--upgrade`,
//! which means *whatever PyPI has today*. what PyPI has today is 84, and
//! **82.0.0 removed `pkg_resources`** — which Odoo 16 imports at the top of
//! `odoo/modules/module.py`.
//!
//! so the installer walked into a venv that already worked — on Ubuntu 22.04
//! `venv` seeds setuptools 59.6.0, `pkg_resources` included — replaced it with
//! one missing the module, and Odoo 16 died on the first line of its own code
//! that ever ran. "newest" was never the requirement: *present* was. see
//! [`SETUPTOOLS_REQUIREMENT`].
//!
//! and the same trap sits one version lower: 81 keeps the module but warns on
//! every import, twice per Odoo 16 start, in a branch that cannot filter it. a
//! version chosen for us is worth checking on both sides.

use tracing::info;

use crate::context::Context;
use crate::error::StepError;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::SystemOps;

const VENV_SUBDIR: &str = "sandbox";
const REPO_SUBDIR: &str = "odoo";

/// the setuptools the venv gets: the newest one that ships `pkg_resources`
/// **without complaining about it** (A-V3-26).
///
/// setuptools **82.0.0 removed `pkg_resources`**, and Odoo 16 imports it bare
/// at the top of `odoo/modules/module.py`. an unbounded `--upgrade setuptools`
/// therefore took a working venv apart: on Ubuntu 22.04 `venv` seeds setuptools
/// 59.6.0, which has `pkg_resources`, and we replaced it with one that does not
/// — so `initialize-odoo-database` died on `ModuleNotFoundError` at the first
/// line of Odoo it ever executed.
///
/// **two thresholds, and the bound sits under the lower one.** 82 is where the
/// module disappears and the installation breaks. **81** is where setuptools
/// starts printing `pkg_resources is deprecated as an API` on every import —
/// twice in the journal at each Odoo 16 start, on a stable branch that (unlike
/// 17 and 19) does not filter it, so nobody on that machine can turn it off.
/// the extra step down is not our taste: setuptools' own message names the pin
/// that stops it, so the number is read rather than picked.
///
/// **the bound is unconditional, and that is the decision**: the alternative
/// was to apply it only where it is needed, and neither way of telling holds
/// up. reading the cloned sources cannot distinguish *importing*
/// `pkg_resources` from *mentioning* it — Odoo 17 names it inside a
/// `try`/`except ImportError` with an `importlib.metadata` fallback, Odoo 19
/// only inside warning filters, so a grep says "yes" for all three and caps
/// two installations that never needed it. keying it on the Odoo version would
/// be a second table of the kind that diverges in silence (A-MD-5, A-V3-16).
/// with no discriminant there is nothing that can answer wrongly.
///
/// and it costs nothing elsewhere: this setuptools serves **our** build without
/// isolation (A-R6-2) and nothing else — pip's isolated builds fetch their own,
/// so the ceiling does not reach the wheels the requirements pull in.
const SETUPTOOLS_REQUIREMENT: &str = "setuptools<81";
/// pip's cache, **inside** the venv, so it goes with `CreateVirtualenv`'s undo
/// instead of staying in the customer's home (A-R5-3).
const PIP_CACHE_SUBDIR: &str = ".pip-cache";

/// installs the pip dependencies into the venv; it has no undo of its own.
pub struct InstallPythonRequirements {
    ops: Box<dyn SystemOps>,
    installed: bool,
}

impl InstallPythonRequirements {
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            installed: false,
        }
    }

    /// writes a requirements file **inside the venv**, not in `/tmp`, and hands
    /// it to the user who will read it (A-V3-3).
    ///
    /// root writes a file that pip reads as `odoo`: two operations with a
    /// window between them. in a world-writable directory, under a name written
    /// in the source, anyone controlling a local account could replace it in
    /// that window and have pip install arbitrary packages into the venv — code
    /// execution as the owner of the filestore and the database. not the
    /// symlink case, which the kernel mitigates, but replacement of the
    /// **contents**, which it does not.
    ///
    /// the venv's sandbox removes the premise instead of defending against the
    /// attack: it belongs to `odoo` and is not writable by others. the file is
    /// also born and dies inside the reversible perimeter, so an interrupted
    /// run leaves nothing outside.
    ///
    /// the unpredictable name and fail-closed creation remain regardless, and
    /// the final `chown` is needed because the file is born `0600 root` while
    /// `odoo` has to read it.
    fn write_requirements_for_user(
        &self,
        venv: &std::path::Path,
        user: &str,
        name: &str,
        content: &str,
    ) -> Result<std::path::PathBuf, StepError> {
        let path = crate::system_ops::private_temp_path(&venv.join(name), name);
        self.ops.create_private_file(&path, content)?;
        self.ops.chown_named(&path, user, user)?;
        Ok(path)
    }
}

impl Step for InstallPythonRequirements {
    fn name(&self) -> &str {
        "install-python-requirements"
    }

    fn snapshot(&mut self, _ctx: &Context) -> Result<(), StepError> {
        // a light snapshot: the state that matters to the rollback is the
        // venv's, not a per-package `PreState`.
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if ctx.dry_run {
            info!("run (dry run): pip upgrade + Cython<3 + gevent (no-build-isolation) + requirements");
            return Ok(());
        }

        let user = &ctx.odoo_user;
        let venv = ctx.install_dir.join(VENV_SUBDIR);
        let pip = venv.join("bin").join("pip");
        let pip = pip.to_string_lossy();
        let requirements = ctx.install_dir.join(REPO_SUBDIR).join("requirements.txt");
        // cache inside our perimeter: see the module docs (A-R5-3).
        let cache_dir = venv.join(PIP_CACHE_SUBDIR);
        let cache_dir = cache_dir.to_string_lossy();

        // the requirements file must exist; reading it fails if not.
        let content = self.ops.read_to_string(&requirements)?;

        // `setuptools` is not decorative, and its absence is why this step
        // failed on 24.04 (A-R6-2): from Python 3.12 `venv` no longer seeds it,
        // while the gevent step builds with what it finds in the venv. without
        // it the build backend does not exist. the system package is
        // irrelevant: the venv is isolated.
        //
        // it is **bounded**, and that is A-V3-26: what this step owes the venv
        // is that setuptools *exists*, never that it is the newest one. see
        // `SETUPTOOLS_REQUIREMENT`.
        self.ops.run_as_user(
            user,
            &pip,
            &[
                "install",
                "--quiet",
                "--cache-dir",
                &cache_dir,
                "--upgrade",
                "pip",
                "wheel",
                SETUPTOOLS_REQUIREMENT,
            ],
        )?;

        // a Cython the sources can still build against.
        self.ops.run_as_user(
            user,
            &pip,
            &["install", "--quiet", "--cache-dir", &cache_dir, "Cython<3"],
        )?;

        // gevent and greenlet from their requirement lines **with the
        // markers**, built without isolation. through a file and not argv, so
        // the markers survive and pip picks the version (A-R6-3).
        let gevent_lines = gevent_stack_lines(&content);
        if gevent_lines.trim().is_empty() {
            info!("run: no gevent line in the requirements, skipping the dedicated step");
        } else {
            let tmp_gevent = self.write_requirements_for_user(
                &venv,
                user,
                "requirements-gevent.txt",
                &gevent_lines,
            )?;
            let tmp_gevent_str = tmp_gevent.to_string_lossy().into_owned();
            let outcome = self.ops.run_as_user(
                user,
                &pip,
                &[
                    "install",
                    "--quiet",
                    "--cache-dir",
                    &cache_dir,
                    "--no-build-isolation",
                    "--requirement",
                    &tmp_gevent_str,
                ],
            );
            let _ = self.ops.remove_file(&tmp_gevent);
            // on failure the likeliest cause is not in the message, so it is
            // prepended (A-MD-7). with a covered Python, or an unknown one,
            // pip's error passes through untouched.
            outcome.map_err(|e| {
                explain_gevent_failure(
                    e,
                    self.ops.python_version(&ctx.python.command),
                    &gevent_lines,
                )
            })?;
        }

        // the rest of the requirements, minus what the previous step installed.
        let filtered = filter_out_gevent_stack(&content);
        let tmp_req =
            self.write_requirements_for_user(&venv, user, "requirements-filtered.txt", &filtered)?;
        let tmp_str = tmp_req.to_string_lossy().into_owned();
        let outcome = self.ops.run_as_user(
            user,
            &pip,
            &[
                "install",
                "--quiet",
                "--cache-dir",
                &cache_dir,
                "--prefer-binary",
                "--requirement",
                &tmp_str,
            ],
        );
        let _ = self.ops.remove_file(&tmp_req);
        outcome?;

        self.installed = true;
        info!("run: Python dependencies installed");
        Ok(())
    }

    fn undo(&self, _ctx: &Context) -> Result<(), StepError> {
        // a deliberate no-op: the packages live in the venv, whose own undo
        // removes them.
        info!(
            "undo NO-OP: the pip packages live in the venv, and their removal is covered \
             by CreateVirtualenv's undo"
        );
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::Value::Bool(self.installed)
    }

    /// rehydrated for symmetry: the undo is a no-op, but the `snapshot_value` ⇄
    /// `rehydrate` contract holds for every step.
    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let installed = decode_snapshot(self.name(), snapshot)?;
        self.installed = installed;
        Ok(())
    }
}

/// the packages installed separately, without build isolation.
///
/// `greenlet` travels with `gevent` because it is its C counterpart: Odoo pins
/// both with the same markers, and installing them apart would let pip resolve
/// greenlet from gevent's *metadata* instead — which is how the wrong version
/// ended up compiling against a newer Python (A-R6-3).
const BUILD_ISOLATED_PACKAGES: [&str; 2] = ["gevent", "greenlet"];

/// the `gevent`/`greenlet` lines **verbatim**, environment markers included.
///
/// verbatim is the point: the markers are the only thing separating the right
/// version from one that will not compile, and evaluating them is not our job.
/// the result goes into a file for pip, which keeps the applicable line.
///
/// empty when the requirements name neither, in which case the dedicated step
/// has no reason to exist and is skipped.
pub fn gevent_stack_lines(requirements: &str) -> String {
    let selected: Vec<&str> = requirements
        .lines()
        .filter(|line| is_build_isolated_requirement(line))
        .collect();
    if selected.is_empty() {
        return String::new();
    }
    let mut out = selected.join("\n");
    out.push('\n');
    out
}

/// the complement of [`gevent_stack_lines`]: everything else.
pub fn filter_out_gevent_stack(requirements: &str) -> String {
    let mut out: String = requirements
        .lines()
        .filter(|line| !is_build_isolated_requirement(line))
        .collect::<Vec<_>>()
        .join("\n");
    out.push('\n');
    out
}

/// prepends the likely cause to a failed gevent build (A-MD-7).
///
/// "does Odoo pin a gevent for this Python?" **cannot be answered from the
/// requirements file**: the markers are open upwards, so the applicable line
/// *is* applicable and pip picks it correctly. that its wheel is missing for
/// this interpreter is a fact about PyPI, not about the file — pretending to
/// derive it from the lines would be a check answering a different question
/// from the one it appears to ask.
///
/// so nothing is prevented: it is **explained**, when the failure actually
/// happens. on a covered Python the error passes through untouched, because
/// there the cause is something else and a wrong diagnosis is worse than none.
/// an unknown version behaves like a covered one.
pub fn explain_gevent_failure(
    error: StepError,
    python: Option<(u32, u32)>,
    gevent_lines: &str,
) -> StepError {
    let Some(python) = python.filter(|v| crate::checks::python_is_newer_than_tested(*v)) else {
        return error;
    };
    let version = crate::checks::format_python(python);
    let tested = crate::checks::format_python(crate::checks::NEWEST_TESTED_PYTHON);
    let diagnosis = format!(
        "the gevent build failed, and this system runs Python {version} — newer than \
         Python {tested}, the latest one the installer is known to get through.\n\
         Odoo pins gevent and greenlet per interpreter version, and for a Python newer than \
         its pins there is no prebuilt wheel: pip has to build from source, and the C those \
         versions generate does not survive a newer CPython's headers. this is not a compiler \
         problem nor a missing system package: it is the version, and no build flag gets \
         around it.\n\
         the lines this Odoo version declares:\n{}\n\
         there are two ways out, both beyond the installer's reach: a release with a Python \
         those pins cover, or an Odoo version that pins this interpreter. installing a gevent \
         other than the pinned one is not one of them — it would be a combination nobody has \
         tried.",
        gevent_lines
            .lines()
            .map(|l| format!("  {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    StepError::PythonTooNew {
        diagnosis,
        original: error.to_string(),
    }
}

/// `true` when the line is a requirement for one of
/// [`BUILD_ISOLATED_PACKAGES`]: the name at the start, followed by a boundary —
/// operator, marker, space or end — case-insensitively. the boundary is what
/// keeps `gevent-websocket` out.
fn is_build_isolated_requirement(line: &str) -> bool {
    let lower = line.trim().to_ascii_lowercase();
    BUILD_ISOLATED_PACKAGES.iter().any(|pkg| {
        lower
            .strip_prefix(pkg)
            .and_then(|rest| rest.chars().next())
            .map(|c| matches!(c, '>' | '=' | '<' | '!' | ';' | ' ' | '\t' | '#'))
            // an empty remainder means the line is exactly the name.
            .unwrap_or_else(|| lower == *pkg)
    })
}
