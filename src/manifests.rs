//! the manifests on this machine: where each one lives, and how they are found
//! as **one uniform list** (phase I1).
//!
//! # one file per instance, and why not one file with a list inside
//!
//! a manifest per instance means **a rollback per instance with no new logic in
//! the engine**: the existing `load → step_by_name → rehydrate → undo` path
//! takes a path and does not care which instance it belongs to. it also means a
//! corrupted file takes only its own instance down, instead of every one on the
//! machine.
//!
//! # the two locations, and why the historical one is not migrated
//!
//! - a **named** instance: `/var/lib/invok/instances/<name>.json`;
//! - the **unnamed** instance: `/var/lib/invok/state.json`, exactly where it has
//!   always been — and, failing that, the older paths R7 left readable.
//!
//! moving the historical file into `instances/` was the obvious tidy-up and is
//! deliberately **not** done. a migration is a mutation of the one file whose
//! loss makes an instance impossible to uninstall, and it would have to happen
//! at the least convenient moment — the start of an install, or worse the start
//! of a rollback, which is asked to remove things and not to reorganise them.
//! nothing is gained that [`discover_in`] does not give: it returns one list
//! with one shape, whatever path each entry came from, so the code that counts
//! instances never learns there were two places to look (`A-V6-2`).
//!
//! # what "found" means, and what it deliberately does not
//!
//! discovery reports **problems** separately from findings, and that is the
//! point rather than a nicety: `/var/lib/invok` is `0700` root, so without
//! privileges the directory read fails, and a function that answered "no
//! instances" would be a check that cannot fail — this project's recurring
//! defect. an unreadable path is *not knowing*, never *nothing there*.

use std::fs;
use std::path::{Path, PathBuf};

use crate::instance::UNNAMED_ID;
use crate::state::{self, InstallState, DEFAULT_STATE_DIR, DEFAULT_STATE_PATH, LEGACY_STATE_PATHS};

/// where named instances' manifests live, under [`DEFAULT_STATE_DIR`].
pub const INSTANCES_SUBDIR: &str = "instances";

/// which instance a manifest describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstanceId {
    /// the historical installation, the one made without `--instance`.
    Unnamed,
    Named(String),
}

impl std::fmt::Display for InstanceId {
    /// prints what the user can **type**.
    ///
    /// the unnamed instance shows as `default`, which is also how it is
    /// selected — and the reason [`crate::instance::validate_instance`] refuses
    /// that word as an instance name: a real instance called `default` would
    /// make the selector ambiguous, and an ambiguous selector on a destructive
    /// command is not something to resolve by precedence rules.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstanceId::Unnamed => f.write_str(UNNAMED_ID),
            InstanceId::Named(name) => f.write_str(name),
        }
    }
}

impl InstanceId {
    /// reads an id the way the user typed it.
    pub fn parse(value: &str) -> Self {
        if value == UNNAMED_ID {
            InstanceId::Unnamed
        } else {
            InstanceId::Named(value.to_string())
        }
    }

    /// the name to pass to [`manifest_path_in`], `None` for the unnamed one.
    pub fn as_option(&self) -> Option<&str> {
        match self {
            InstanceId::Unnamed => None,
            InstanceId::Named(name) => Some(name),
        }
    }
}

/// a manifest found on disk, with its identity already resolved.
#[derive(Debug, Clone)]
pub struct Found {
    pub id: InstanceId,
    pub path: PathBuf,
    pub state: InstallState,
}

impl Found {
    /// does this manifest still claim artifacts on the system?
    ///
    /// an empty one does not: after a complete rollback the file is removed
    /// rather than emptied (R19), so an empty manifest is a leftover that says
    /// nothing.
    pub fn owns_anything(&self) -> bool {
        !self.state.completed.is_empty()
    }

    /// is there still an **instance** here, as opposed to a record of shared
    /// artifacts nobody has removed yet?
    ///
    /// after a rollback run while another instance was installed, what stays in
    /// the manifest is exactly the steps that own shared things — `/opt/odoo`,
    /// the packages, the cluster. that file is a **tombstone**: the instance is
    /// gone, its sources, database, unit and home with it, and what remains is
    /// the record of who owns what everybody else is still using.
    ///
    /// the distinction is what stops the machine deadlocking. counting a
    /// tombstone as an instance would mean two half-removed installations each
    /// protecting the other's shared artifacts, with neither ever able to
    /// finish.
    ///
    /// a step of scope `Mixed` does **not** count as life: what it leaves
    /// behind is the shared half, which is the tombstone's whole point.
    pub fn is_live(&self) -> bool {
        let unnamed = self
            .state
            .config
            .as_ref()
            .map(|c| c.instance.is_none())
            // no configuration at all is a pre-R4 manifest, which describes the
            // one installation that could exist then: the unnamed one.
            .unwrap_or(true);
        self.state.completed.iter().any(|r| {
            crate::steps::artifact_scope(&r.name, unnamed)
                == crate::steps::ArtifactScope::OwnInstance
        })
    }
}

/// a manifest that exists but could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub path: PathBuf,
    pub reason: String,
}

/// everything discovery could establish, and everything it could not.
#[derive(Debug, Clone, Default)]
pub struct Discovery {
    pub found: Vec<Found>,
    pub problems: Vec<Problem>,
}

impl Discovery {
    /// the instances **other** than `chosen` that are still installed.
    ///
    /// tombstones are excluded: they hold no running Odoo, only the record of
    /// shared artifacts. see [`Found::is_live`].
    pub fn live_others(&self, chosen: &InstanceId) -> Vec<String> {
        self.found
            .iter()
            .filter(|f| &f.id != chosen && f.is_live())
            .map(|f| f.id.to_string())
            .collect()
    }

    /// the manifests that are **not** live: nothing runs from them, but they
    /// still record shared artifacts nobody has removed.
    ///
    /// what `rollback --all` comes back for once every instance is gone.
    pub fn tombstones(&self) -> Vec<&Found> {
        self.found
            .iter()
            .filter(|f| !f.is_live() && f.owns_anything())
            .collect()
    }
}

/// where this instance's manifest is written.
///
/// the unnamed instance keeps the historical path and its legacy cascade, so an
/// installation made by any earlier version is still found where it is; a named
/// one gets a file of its own under [`INSTANCES_SUBDIR`].
pub fn manifest_path_for(instance: Option<&str>) -> PathBuf {
    match instance {
        None => state::resolve_state_path(),
        Some(name) => manifest_path_in(Path::new(DEFAULT_STATE_DIR), Some(name)),
    }
}

/// [`manifest_path_for`]'s rule with the root as a parameter, so it can be
/// checked without writing under `/var/lib`.
pub fn manifest_path_in(root: &Path, instance: Option<&str>) -> PathBuf {
    match instance {
        None => root.join("state.json"),
        Some(name) => root.join(INSTANCES_SUBDIR).join(format!("{name}.json")),
    }
}

/// every manifest on this machine, in production locations.
pub fn discover() -> Discovery {
    let legacy: Vec<&Path> = LEGACY_STATE_PATHS.iter().map(Path::new).collect();
    discover_in(
        Path::new(DEFAULT_STATE_DIR),
        Path::new(DEFAULT_STATE_PATH),
        &legacy,
    )
}

/// [`discover`]'s rule, with the locations as parameters.
///
/// `unnamed` and `legacy` are passed separately from `root` because the unnamed
/// instance's file may live outside it entirely — `/opt/odoo/.installer-state.json`
/// on an installation from 2.1.0 — and that is precisely the case that must not
/// be lost.
pub fn discover_in(root: &Path, unnamed: &Path, legacy: &[&Path]) -> Discovery {
    let mut out = Discovery::default();

    // --- the unnamed instance ------------------------------------------------
    let unnamed_path = state::pick_state_path(unnamed, legacy);
    if unnamed_path.exists() {
        match InstallState::load(&unnamed_path) {
            Ok(state) => out.found.push(Found {
                id: InstanceId::Unnamed,
                path: unnamed_path,
                state,
            }),
            Err(e) => out.problems.push(Problem {
                path: unnamed_path,
                reason: e.to_string(),
            }),
        }
    }

    // --- the named ones ------------------------------------------------------
    let dir = root.join(INSTANCES_SUBDIR);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        // absent is the normal state of a machine with one installation: there
        // is nothing to report. unreadable is **not knowing**, and is reported.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return out,
        Err(e) => {
            out.problems.push(Problem {
                path: dir,
                reason: e.to_string(),
            });
            return out;
        }
    };

    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e == "json").unwrap_or(false) {
            paths.push(path);
        }
    }
    // read_dir has no order; a stable one keeps the listings and the messages
    // reproducible.
    paths.sort();

    for path in paths {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        match InstallState::load(&path) {
            Ok(state) => {
                let id = resolve_id(&state, &stem, &path);
                out.found.push(Found { id, path, state });
            }
            Err(e) => out.problems.push(Problem {
                path,
                reason: e.to_string(),
            }),
        }
    }

    out
}

/// the identity of a manifest found under `instances/`.
///
/// the name comes from the **manifest**, not from the file name: the undos act
/// through the recorded configuration, so that is the name that decides what
/// gets removed. the file name is only how the file was found — and if the two
/// disagree, somebody renamed the file, which is worth saying out loud rather
/// than resolving silently in favour of the wrong one.
fn resolve_id(state: &InstallState, stem: &str, path: &Path) -> InstanceId {
    let Some(config) = &state.config else {
        // pre-R4 manifest, with no configuration at all. the rollback refuses it
        // anyway; the file name is the only handle left for listing it.
        return InstanceId::Named(stem.to_string());
    };
    match config.instance.as_deref() {
        Some(name) => {
            if name != stem {
                tracing::warn!(
                    path = %path.display(),
                    file_name = stem,
                    recorded = name,
                    "the manifest's file name and the instance it records differ: going by \
                     what the manifest records, since that is what the undos will act on"
                );
            }
            InstanceId::Named(name.to_string())
        }
        // a manifest under `instances/` that records no instance describes the
        // unnamed installation: it was copied there by hand. it is *not* the
        // unnamed instance's canonical file, so it keeps the name it is filed
        // under rather than being confused with it.
        None => InstanceId::Named(stem.to_string()),
    }
}

/// what a rollback with no `--state` decided to operate on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// exactly one candidate: its index in `found`.
    One(usize),
    /// no manifest at all — the normal state of a clean machine.
    Nothing,
    /// several, and nothing said which: list them and stop.
    Ambiguous(Vec<String>),
    /// `--instance` named something that is not here.
    NotFound {
        requested: String,
        available: Vec<String>,
    },
}

/// picks the instance to roll back.
///
/// a **pure policy**, like [`crate::rollback::confirmation_gate`] and
/// [`crate::state::start_decision`]: `main` applies it and writes the messages.
/// the rule is the one the command already follows when it does not know
/// something — *it does not guess, it says what it found*.
pub fn select(found: &[Found], requested: Option<&str>) -> Selection {
    if let Some(name) = requested {
        let wanted = InstanceId::parse(name);
        return match found.iter().position(|f| f.id == wanted) {
            Some(i) => Selection::One(i),
            None => Selection::NotFound {
                requested: name.to_string(),
                available: found.iter().map(|f| f.id.to_string()).collect(),
            },
        };
    }
    match found.len() {
        0 => Selection::Nothing,
        1 => Selection::One(0),
        _ => Selection::Ambiguous(found.iter().map(|f| f.id.to_string()).collect()),
    }
}

/// does another instance already claim `port`?
///
/// asked of the **manifests**, not of the system, and that is the point: a
/// listening socket says who is holding the port *now*, so an instance that is
/// merely stopped — for maintenance, or never started — leaves 8069 looking
/// free. it would be recorded a second time and the two would collide at the
/// first simultaneous start, with a diagnosis naming neither of them.
///
/// the same rule as everywhere else here: what is **recorded** is re-read, and
/// only what cannot be recorded is observed. `checks::probe_port` still runs
/// too — they answer different questions, and something else on the machine may
/// hold the port without any manifest knowing.
///
/// only the HTTP port for now: the gevent port is still hardwired in the
/// template and is not persisted, so there is nothing to compare (`A-V6-3`, and
/// phase I3).
pub fn port_conflict(found: &[Found], chosen: &InstanceId, port: u16) -> Option<(String, u16)> {
    found
        .iter()
        .filter(|f| &f.id != chosen && f.is_live())
        .find_map(|f| {
            let config = f.state.config.as_ref()?;
            (config.port == port).then(|| (f.id.to_string(), config.port))
        })
}
