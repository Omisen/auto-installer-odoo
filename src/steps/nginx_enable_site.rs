//! [`NginxEnableSite`]: enables the site and frees port 80.
//!
//! # a treacherous mutation: the default site
//!
//! freeing port 80 means moving `sites-enabled/default` out of the way — the
//! customer's **pre-existing configuration**. here the snapshot goes back to
//! being a **protection**: it records *what* was there, and the undo
//! **restores** it.
//!
//! # recording *whether* it was there is not enough (A-V3-5)
//!
//! the snapshot used to be a `bool`, and two defects grew from that one error.
//!
//! **restoration was not faithful.** the undo always recreated a symlink
//! towards the *distribution-standard* target, so a customer whose default
//! pointed elsewhere did not get their config back — they got the usual one.
//! for a project that restores `.bashrc` byte for byte, a double standard.
//!
//! **and a file could be lost.** `symlink_exists` uses `symlink_metadata`,
//! which answers `true` for a **regular file** too, and the removal is a plain
//! `remove_file`. an administrator who had written that path as a real file —
//! not a rare practice — saw its contents **destroyed**, and got a symlink to
//! the distro default back. not a leftover: a loss of configuration.
//!
//! the nature is now read with [`SystemOps::path_kind`] and **persisted**, and
//! each nature has its treatment: a symlink is removed and recreated towards
//! the recorded target, a regular file is **moved to a backup** and put back,
//! and anything else is left alone.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::steps::unix_timestamp;
use crate::system_ops::{PathKind, SystemOps};

/// what was at the default site before us, and what we did with it.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DefaultSite {
    /// nothing there: nothing to remove, nothing to restore.
    #[default]
    Assente,
    /// a symlink: removed, and recreated towards **this** target rather than
    /// the one usually found there.
    Symlink { target: std::path::PathBuf },
    /// a regular file: it holds somebody's configuration, so it is moved to a
    /// backup rather than deleted, and the undo puts it back.
    ///
    /// `backup` stays `None` until the run has actually moved it.
    FileRegolare { backup: Option<String> },
    /// a directory, or an unreadable symlink: we do not know how to treat it,
    /// so we do not. port 80 may stay occupied, and we say so.
    Intoccabile,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct NginxEnableSiteSnapshot {
    /// the state of our own enabling symlink.
    pub link: PreState,
    /// **legacy, pre-R11**: did the default site exist before us?
    ///
    /// no longer the source of truth — [`Self::default_site`] is — but still
    /// written and read: a state persisted by an earlier version lacks the new
    /// field, and without this fallback its rollback would leave port 80
    /// without the default site we removed.
    #[serde(default)]
    pub default_site_existed: bool,
    /// what was really there, and what we did with it. `None` only in states
    /// written before R11.
    #[serde(default)]
    pub default_site: Option<DefaultSite>,
}

pub struct NginxEnableSite {
    ops: Box<dyn SystemOps>,
    snap: NginxEnableSiteSnapshot,
}

impl NginxEnableSite {
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            snap: NginxEnableSiteSnapshot::default(),
        }
    }

    fn layout(&self) -> crate::distro::NginxLayout {
        self.ops.distro().nginx_layout()
    }
    fn src(&self, ctx: &Context) -> std::path::PathBuf {
        self.layout().vhost_path(&ctx.odoo_version_short)
    }
    /// the enabling symlink — `None` on families where **writing the vhost is
    /// enabling it**.
    fn link(&self, ctx: &Context) -> Option<std::path::PathBuf> {
        self.layout().enabled_link(&ctx.odoo_version_short)
    }
    /// the default site as a separate file — `None` where the concept does not
    /// exist.
    fn default_site(&self) -> Option<std::path::PathBuf> {
        self.layout().default_site
    }

    /// where the backup of a **regular file** default site goes.
    ///
    /// deliberately **outside** the enabled directory, which nginx globs — a
    /// backup left there would still be loaded and port 80 would stay occupied,
    /// i.e. the defect we are fixing under another name.
    fn default_site_backup(&self) -> String {
        format!(
            "{}/default-site.bak.{}",
            self.layout().default_site_backup_dir.display(),
            unix_timestamp()
        )
    }

    /// moves the default site out of the way according to its nature, recording
    /// what was done so the undo can reverse it exactly.
    fn displace_default_site(&mut self) -> Result<(), StepError> {
        // no separate default site means nothing to displace: there the default
        // server lives inside `nginx.conf`, and rewriting the customer's main
        // configuration is not something we do.
        let Some(path) = self.default_site() else {
            return Ok(());
        };
        match self.snap.default_site.clone().unwrap_or_default() {
            DefaultSite::Assente => Ok(()),

            DefaultSite::Symlink { .. } => {
                // a symlink has no contents of its own, and the target is
                // recorded so it can be recreated identically.
                if let Err(e) = self.ops.remove_symlink(&path) {
                    warn!(error = %e, "run: rimozione default site fallita, proseguo");
                } else {
                    info!("run: default site nginx rimosso (porta 80 liberata)");
                }
                Ok(())
            }

            DefaultSite::FileRegolare { .. } => {
                // a real file holds somebody's configuration: moved, not
                // deleted. a failed move is a real error — better to stop than
                // to carry on having half-lost the file.
                let backup = self.default_site_backup();
                self.ops.move_file(&path, std::path::Path::new(&backup))?;
                self.snap.default_site = Some(DefaultSite::FileRegolare {
                    backup: Some(backup.clone()),
                });
                warn!(
                    backup = %backup,
                    "run: il default site era un FILE, non un symlink: spostato in backup \
                     (l'undo lo rimetterà al suo posto)"
                );
                Ok(())
            }

            DefaultSite::Intoccabile => {
                warn!(
                    path = %path.display(),
                    "run: il default site non è né un symlink né un file regolare: non lo tocco. \
                     Se occupa la porta 80, nginx potrebbe non partire: rimuovilo a mano."
                );
                Ok(())
            }
        }
    }
}

impl NginxEnableSite {
    /// puts the default site back **as it was**, not as it usually is.
    ///
    /// best-effort like every undo: a failure is a `warn!`, not an error that
    /// stops the other steps' cleanup.
    fn restore_default_site(&self) {
        let Some(path) = self.default_site() else {
            return;
        };
        match &self.snap.default_site {
            Some(DefaultSite::Assente) | Some(DefaultSite::Intoccabile) => {}

            Some(DefaultSite::Symlink { target }) => {
                // the recorded target, not the constant: a default that pointed
                // elsewhere goes back to pointing *there*.
                if let Err(e) = self.ops.create_symlink(target, &path) {
                    warn!(error = %e, "undo: ripristino default site fallito, proseguo (best-effort)");
                } else {
                    info!(target = %target.display(), "undo: default site nginx ripristinato");
                }
            }

            Some(DefaultSite::FileRegolare {
                backup: Some(backup),
            }) => {
                let backup_path = std::path::Path::new(backup);
                if !self.ops.path_exists(backup_path) {
                    warn!(backup = %backup, "undo: backup del default site non trovato");
                    return;
                }
                if let Err(e) = self.ops.move_file(backup_path, &path) {
                    warn!(error = %e, "undo: ripristino del file default site fallito, proseguo (best-effort)");
                } else {
                    info!("undo: default site nginx (file) rimesso al suo posto");
                }
            }

            // a file the run never got to move: nothing to put back, and
            // rightly so.
            Some(DefaultSite::FileRegolare { backup: None }) => {}

            // a pre-R11 state tells us only *whether* it was there, so we fall
            // back to the historical behaviour: the best that information
            // allows.
            None => {
                if !self.snap.default_site_existed {
                    return;
                }
                let Some(target) = self.layout().default_site_standard_target else {
                    return;
                };
                warn!(
                    target = %target.display(),
                    "undo: stato senza la natura del default site (pre-R11): ripristino un \
                     symlink al target standard. Se il tuo puntava altrove, verificalo."
                );
                if let Err(e) = self.ops.create_symlink(&target, &path) {
                    warn!(error = %e, "undo: ripristino default site fallito, proseguo (best-effort)");
                }
            }
        }
    }
}

impl Step for NginxEnableSite {
    fn name(&self) -> &str {
        "nginx-enable-site"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        if !ctx.with_nginx {
            self.snap = NginxEnableSiteSnapshot::default();
            return Ok(());
        }
        // without an enabled directory there is no symlink to create: writing
        // the vhost **is** enabling it.
        self.snap.link = match self.link(ctx) {
            Some(link) if self.ops.symlink_exists(&link) => PreState::Preexisting,
            _ => PreState::Untracked,
        };
        // the key information for the undo: *what* is at the default site, not
        // *whether* something is. faithful restoration hangs on that
        // distinction (A-V3-5).
        //
        // where it is not a separate file the question does not arise, and
        // "absent" is the honest answer: nothing for us to remove, so nothing
        // to put back.
        let default_site = match self.default_site() {
            None => DefaultSite::Assente,
            Some(path) => match self.ops.path_kind(&path) {
                PathKind::Absent => DefaultSite::Assente,
                PathKind::Symlink { target } => DefaultSite::Symlink { target },
                PathKind::RegularFile => DefaultSite::FileRegolare { backup: None },
                PathKind::Other => DefaultSite::Intoccabile,
            },
        };
        self.snap.default_site_existed = default_site != DefaultSite::Assente;
        self.snap.default_site = Some(default_site);
        info!(
            link = ?self.snap.link,
            default_site = ?self.snap.default_site,
            "snapshot nginx-enable-site"
        );
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if !ctx.with_nginx {
            info!("nginx non richiesto, skip nginx-enable-site");
            return Ok(());
        }
        if ctx.dry_run {
            info!("run (dry-run): rimuoverei il default site e creerei il symlink");
            return Ok(());
        }

        // free port 80 by moving the default site aside. *how* depends on what
        // it is: a symlink is removed, a regular file is moved.
        self.displace_default_site()?;

        match self.link(ctx) {
            Some(link) => {
                self.ops.create_symlink(&self.src(ctx), &link)?;
                if self.snap.link == PreState::Untracked {
                    self.snap.link = PreState::CreatedByUs;
                }
                info!("run: sito abilitato");
            }
            // no enabled directory: the vhost is already loaded where it was
            // written. no extra artifact to record, hence none to undo.
            None => info!(
                "run: su questa famiglia il vhost è già abilitato dove è stato scritto, \
                 nessun symlink da creare"
            ),
        }
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if ctx.dry_run {
            info!("undo (dry-run): rm nostro symlink + ripristino default site se esisteva");
            return Ok(());
        }

        // remove our symlink if we created it.
        if self.snap.link == PreState::CreatedByUs {
            if let Some(link) = self.link(ctx) {
                if let Err(e) = self.ops.remove_symlink(&link) {
                    warn!(error = %e, "undo: rimozione symlink nostro fallita, proseguo (best-effort)");
                }
            }
        }

        self.restore_default_site();
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.snap).unwrap_or(serde_json::Value::Null)
    }

    /// the default site's recorded state is what lets the undo **stitch** the
    /// customer's config back: without rehydrating it, a rollback from disk
    /// would remove our vhost and leave port 80 without the default site we
    /// took away.
    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let snap = decode_snapshot(self.name(), snapshot)?;
        self.snap = snap;
        Ok(())
    }
}
