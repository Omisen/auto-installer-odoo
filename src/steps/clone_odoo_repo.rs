//! [`CloneOdooRepo`]: clones the Odoo sources into `<install_dir>/odoo`.
//!
//! the first of the three sub-steps the Bash monolith is broken into.
//! everything runs as the **odoo** user, not root.
//!
//! # perimeter and container
//!
//! this step creates `<install_dir>` and its subdirectories, and does **not**
//! touch `/opt/odoo`, which belongs to
//! [`PrepareOptRoot`](crate::steps::prepare_opt_root). one level below,
//! entirely ours, `rm -rf` is legitimate. the undo removes the repository and
//! the **container** only if empty — the venv and config undos have run first,
//! in the reverse order.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::{OdooSourceState, SystemOps};

const ODOO_GIT_URL: &str = "https://github.com/odoo/odoo.git";
const REPO_SUBDIR: &str = "odoo";
const REPOS_SUBDIR: &str = "repos";
const MODULES_SUBDIR: &str = "repos/modules";
const DEFAULT_DEPTH: u32 = 5;
const DEFAULT_RETRIES: u32 = 3;
const DEFAULT_BACKOFF_SECS: u64 = 2;

/// the clone's serialisable snapshot.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CloneSnapshot {
    pub prestate: PreState,
    /// how the sources got there, or empty when absent.
    pub source_mode: String,
}

/// clones the Odoo sources, reversibly, with retries and a tarball fallback.
pub struct CloneOdooRepo {
    ops: Box<dyn SystemOps>,
    retries: u32,
    depth: u32,
    backoff_base_secs: u64,
    snap: CloneSnapshot,
    /// an existing but invalid directory, removed before cloning.
    had_invalid_dir: bool,
}

impl CloneOdooRepo {
    /// the **production** constructor: reads the network parameters from the
    /// environment and applies the real backoff between attempts.
    ///
    /// not the same as [`Self::with_ops`], which zeroes the backoff because
    /// tests must not sleep. building this for a real `run` with the test
    /// constructor would make the retry **useless**: three instant attempts
    /// against an unresponsive network are one attempt.
    pub fn for_run(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            retries: env_u32("GIT_CLONE_RETRIES", DEFAULT_RETRIES),
            depth: env_u32("GIT_DEPTH", DEFAULT_DEPTH),
            backoff_base_secs: DEFAULT_BACKOFF_SECS,
            snap: CloneSnapshot::default(),
            had_invalid_dir: false,
        }
    }

    /// the test constructor: injectable `SystemOps` and **no backoff**.
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            retries: DEFAULT_RETRIES,
            depth: DEFAULT_DEPTH,
            backoff_base_secs: 0,
            snap: CloneSnapshot::default(),
            had_invalid_dir: false,
        }
    }

    fn repo_dir(ctx: &Context) -> std::path::PathBuf {
        ctx.install_dir.join(REPO_SUBDIR)
    }

    fn sleep_backoff(&self, failed_attempt: u32) {
        if self.backoff_base_secs > 0 {
            std::thread::sleep(std::time::Duration::from_secs(
                failed_attempt as u64 * self.backoff_base_secs,
            ));
        }
    }
}

/// reads an environment variable as a `u32`, with a default.
fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

impl Step for CloneOdooRepo {
    fn name(&self) -> &str {
        "clone-odoo-repo"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        let repo_dir = Self::repo_dir(ctx);
        match self.ops.detect_odoo_source(&ctx.odoo_user, &repo_dir)? {
            OdooSourceState::Absent => {
                self.snap.prestate = PreState::Untracked;
                self.snap.source_mode.clear();
            }
            OdooSourceState::GitRepo { branch } => {
                if branch == ctx.odoo_version {
                    self.snap.prestate = PreState::Preexisting;
                    self.snap.source_mode = "git-existing".to_string();
                } else {
                    // a different branch may hold work in progress: do NOT
                    // regenerate.
                    return Err(StepError::Precondition(format!(
                        "existing repository on branch '{branch}', expected '{}'. \
                         remove {} by hand if you want to re-clone",
                        ctx.odoo_version,
                        repo_dir.display()
                    )));
                }
            }
            OdooSourceState::TarballPresent => {
                self.snap.prestate = PreState::Preexisting;
                self.snap.source_mode = "tarball-existing".to_string();
            }
            OdooSourceState::InvalidDir => {
                // inside our perimeter with neither marker: not a valid
                // checkout. regenerated, but said out loud.
                warn!(
                    dir = %repo_dir.display(),
                    "the sources directory exists but is not valid: it will be regenerated"
                );
                self.snap.prestate = PreState::Untracked;
                self.had_invalid_dir = true;
            }
        }
        info!(prestate = ?self.snap.prestate, mode = %self.snap.source_mode, "snapshot: clone-odoo-repo");
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if self.snap.prestate == PreState::Preexisting {
            info!(mode = %self.snap.source_mode, "run: sources already present, skipping the clone");
            return Ok(());
        }
        if ctx.dry_run {
            info!("run (dry run): would create the directories and clone the Odoo sources");
            return Ok(());
        }

        let user = &ctx.odoo_user;
        let repo_dir = Self::repo_dir(ctx);

        // directory structure, as the `odoo` user.
        self.ops.mkdir_p_as_user(user, &ctx.install_dir)?;
        self.ops
            .mkdir_p_as_user(user, &ctx.install_dir.join(MODULES_SUBDIR))?;
        // ours from here (A-V3-24): everything below can fail — the network
        // above all — and these directories would otherwise stay, keeping
        // `install_dir` and with it `/opt/odoo` alive through the rollback.
        self.snap.prestate = PreState::CreatedByUs;

        // a pre-existing invalid directory goes before cloning.
        if self.had_invalid_dir {
            self.ops.remove_dir_all(&repo_dir)?;
        }

        // clone, with retries and backoff.
        let mut cloned = false;
        for attempt in 1..=self.retries {
            match self
                .ops
                .git_clone(user, ODOO_GIT_URL, &ctx.odoo_version, self.depth, &repo_dir)
            {
                Ok(()) => {
                    self.snap.source_mode = "git".to_string();
                    cloned = true;
                    info!(attempt, "run: clone completed");
                    break;
                }
                Err(e) => {
                    warn!(attempt, retries = self.retries, error = %e, "run: clone failed");
                    // clean partial artifacts before retrying.
                    let _ = self.ops.remove_dir_all(&repo_dir);
                    if attempt < self.retries {
                        self.sleep_backoff(attempt);
                    }
                }
            }
        }

        // tarball fallback once every clone attempt has failed.
        if !cloned {
            warn!(
                retries = self.retries,
                "run: clone failed, falling back to the tarball"
            );
            let tar_url = format!(
                "https://codeload.github.com/odoo/odoo/tar.gz/refs/heads/{}",
                ctx.odoo_version
            );
            self.ops.tarball_install(user, &tar_url, &repo_dir)?;
            self.snap.source_mode = "tarball".to_string();
            info!("run: sources installed through the tarball fallback");
        }

        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if self.snap.prestate != PreState::CreatedByUs {
            info!(prestate = ?self.snap.prestate, "undo NO-OP (sources not created by us)");
            return Ok(());
        }
        if ctx.dry_run {
            info!("undo (dry run): rm -rf of the sources, plus the container if it is empty");
            return Ok(());
        }

        let repo_dir = Self::repo_dir(ctx);
        // our perimeter, so the recursive removal is legitimate.
        if let Err(e) = self.ops.remove_dir_all(&repo_dir) {
            warn!(error = %e, "undo: rm -rf of the sources failed, proceeding (best-effort)");
        }
        // the modules directory, created by us.
        if let Err(e) = self.ops.remove_dir_all(&ctx.install_dir.join(REPOS_SUBDIR)) {
            warn!(error = %e, "undo: rm -rf of the repos failed, proceeding (best-effort)");
        }

        // the container goes ONLY if empty: the venv and config undos have
        // already run. anything else left inside, pre-existing material
        // included, stays.
        match self.ops.dir_is_empty(&ctx.install_dir) {
            Ok(true) => {
                if let Err(e) = self.ops.rmdir(&ctx.install_dir) {
                    warn!(error = %e, "undo: removing the container failed, proceeding");
                } else {
                    info!(dir = %ctx.install_dir.display(), "undo: install_dir container removed (it was empty)");
                }
            }
            Ok(false) => {
                info!(dir = %ctx.install_dir.display(), "undo: the container is not empty, leaving it");
            }
            Err(e) => warn!(error = %e, "undo: cannot inspect the container, not removing it"),
        }
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.snap).unwrap_or(serde_json::Value::Null)
    }

    /// `had_invalid_dir` is deliberately not rehydrated: it governs the `run`,
    /// not the undo, and is never serialised.
    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let snap = decode_snapshot(self.name(), snapshot)?;
        self.snap = snap;
        Ok(())
    }
}
