//! the **package manager** boundary: what it can do, and which names it knows.
//!
//! the first of the two multi-distro boundaries; the other is
//! [`crate::distro`]. not a second door onto the system: it is **obtained**
//! from the existing one, with
//! [`SystemOps::packages`](crate::system_ops::SystemOps::packages), so what
//! touches the machine stays in one place and the tests still mock one thing.
//!
//! # a trait and not an `enum`
//!
//! with two families an `enum` plus a `match` per method would be more direct.
//! **the mock** decides it: an enum returned by `packages()` would have to be
//! instantiated by the test mock too, and its branches really run `apt-get` and
//! `dnf`. mocking it would need a third, test-only variant **inside the
//! production type** — a branch that cannot execute in production, which is
//! this project's recurring defect, introduced on purpose. `dyn` costs nothing
//! new here.
//!
//! # what does **not** live here
//!
//! nginx, firewall and path conventions: those are *distribution* divergence,
//! not *packaging*, and live in [`crate::distro`]. nor the rollback policy:
//! this boundary maps 1:1 onto commands so the tests can assert exact
//! sequences.

pub mod apt;
pub mod dnf;

use std::path::Path;

use crate::error::StepError;

/// what the manager can tell us about a package name.
///
/// three values and not two booleans, a distinction won in the field with
/// A5.1-bis. it separates the **mechanism**, which belongs to the manager (apt
/// needs two commands, because a purely virtual name has no candidate yet
/// installs fine), from the **policy** — "a real name beats a virtual one" —
/// which does not depend on the family: a name the manager will not recognise
/// after installation cannot be removed, and a delta containing it lies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// installable, and afterwards the manager knows it by **this** name: it is
    /// removable, so a delta containing it is honest.
    Real,
    /// installable only because another package *provides* it: afterwards the
    /// manager does not know this name, and removing it removes nothing. a
    /// **fallback**, never a first choice.
    VirtualOnly,
    /// not installable under this name — or the manager cannot answer. the two
    /// are told apart with [`PackageManager::index_is_queryable`], not by
    /// guessing.
    Absent,
}

/// the availability decision, over the observed outcomes alone.
///
/// pure, and separate from the commands that produce them — the pattern this
/// project uses wherever the decision matters more than how its inputs were
/// obtained. the code that runs `apt-cache` can only be checked on a real
/// machine, and without this split the rule that matters would sit outside
/// every test.
///
/// the order is A5.1-bis's protection: **a real candidate always beats a
/// virtual name**.
pub fn availability_from(policy_says_real: bool, resolver_accepts: bool) -> Availability {
    if policy_says_real {
        Availability::Real
    } else if resolver_accepts {
        Availability::VirtualOnly
    } else {
        Availability::Absent
    }
}

/// a package requirement: one or more **alternative** names, in order of
/// preference, satisfying the same need.
///
/// alternatives mean "same need, different names **within one family**":
/// `libtiff5-dev` and `libtiff-dev` are the same package on two Debian
/// releases. putting `freetype-devel` beside `libfreetype6-dev` would look free
/// and **break the group**, because the first resolution rule — an
/// already-installed alternative wins — is correct between synonyms of one
/// distro and a trap across families.
///
/// so the family does **not** enter the group: it enters one level up, in
/// [`PackageCatalog`].
///
/// owns its `String`s because tests build groups at runtime; the production
/// lists stay `const` and go through [`PackageSpec::group`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageSpec {
    alternatives: Vec<String>,
    /// `false` means we carry on when no alternative is installable.
    required: bool,
}

impl PackageSpec {
    /// a single name with no alternative: missing, the step stops.
    pub fn one(name: &str) -> Self {
        Self::any(&[name])
    }

    /// alternatives in order of preference; the first is preferred.
    pub fn any(alternatives: &[&str]) -> Self {
        PackageSpec {
            alternatives: alternatives.iter().map(|s| s.to_string()).collect(),
            required: true,
        }
    }

    /// as [`PackageSpec::any`], but an entirely unavailable group warns instead
    /// of failing.
    pub fn optional(alternatives: &[&str]) -> Self {
        PackageSpec {
            required: false,
            ..Self::any(alternatives)
        }
    }

    /// turns a group from the `const` lists into a mandatory spec.
    pub fn group(group: &[&str]) -> Self {
        Self::any(group)
    }

    /// the alternatives, most preferred first.
    pub fn alternatives(&self) -> &[String] {
        &self.alternatives
    }

    /// `true` when an unsatisfiable group must stop the step.
    pub fn is_required(&self) -> bool {
        self.required
    }

    /// the preferred name, for diagnostics. an empty group is not constructible
    /// from the production lists; should one arrive anyway, this names it
    /// rather than panicking.
    pub fn preferred(&self) -> &str {
        self.alternatives
            .first()
            .map(String::as_str)
            .unwrap_or("<empty group>")
    }
}

/// turns a canonical list into mandatory specs.
pub fn specs(groups: &[&[&str]]) -> Vec<PackageSpec> {
    groups.iter().map(|g| PackageSpec::group(g)).collect()
}

/// an installation **need**, independent of the name it has on any
/// distribution.
///
/// it plays no part in resolution, which still works on names. it exists for
/// **one thing**: a test that enumerates these variants and demands every
/// family cover them all.
///
/// R6-hotfix-2's lesson was "freeze the list, so a refactor that loses a
/// package says so at once". with two families it is not enough for each list
/// to be frozen: they must **correspond**. otherwise a dependency added to
/// Debian is found missing on Fedora only when a VM stops compiling.
///
/// the correspondence is not 1:1, and that is fine: one need can cost **several
/// packages** on a family, and one package can satisfy two needs. so a
/// catalogue entry carries a `Vec<PackageSpec>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DepId {
    Git,
    Curl,
    Wget,
    /// `envsubst`, for rendering the templates.
    Gettext,
    PythonPip,
    PythonDev,
    /// the `ensurepip` module, without which `python3 -m venv` stops halfway
    /// (A-R6-1). a separate package on some families and part of the stdlib on
    /// others: the need is there either way.
    PythonVenv,
    PythonWheel,
    PythonSetuptools,
    /// C/C++ compiler and make, for pip's native extensions.
    BuildTools,
    Freetype,
    Xml2,
    Zip,
    Ldap,
    Sasl,
    Jpeg,
    /// the historical variant of the previous one: a transitional package on
    /// some releases, absent on others where it collapses onto [`Self::Jpeg`].
    Jpeg8,
    Zlib,
    /// PostgreSQL client headers, for `psycopg2`.
    PostgresClient,
    Xslt,
    Tiff,
    OpenJpeg,
    Lcms2,
    Webp,
    Harfbuzz,
    Fribidi,
    Xcb,
    Ev,
    CAres,
    /// **optional**: the `.less` asset compiler. modern Odoo uses SCSS and
    /// starts without it, so missing is a warning.
    LessCompiler,
}

impl DepId {
    /// every need, for the catalogue parity test.
    pub const ALL: &'static [DepId] = &[
        DepId::Git,
        DepId::Curl,
        DepId::Wget,
        DepId::Gettext,
        DepId::PythonPip,
        DepId::PythonDev,
        DepId::PythonVenv,
        DepId::PythonWheel,
        DepId::PythonSetuptools,
        DepId::BuildTools,
        DepId::Freetype,
        DepId::Xml2,
        DepId::Zip,
        DepId::Ldap,
        DepId::Sasl,
        DepId::Jpeg,
        DepId::Jpeg8,
        DepId::Zlib,
        DepId::PostgresClient,
        DepId::Xslt,
        DepId::Tiff,
        DepId::OpenJpeg,
        DepId::Lcms2,
        DepId::Webp,
        DepId::Harfbuzz,
        DepId::Fribidi,
        DepId::Xcb,
        DepId::Ev,
        DepId::CAres,
        DepId::LessCompiler,
    ];
}

/// a need, and the packages that satisfy it **on this family**.
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub id: DepId,
    /// one or more specs: a need can cost several packages.
    pub specs: Vec<PackageSpec>,
}

impl CatalogEntry {
    /// an entry with a single group of alternatives.
    pub fn new(id: DepId, alternatives: &[&str]) -> Self {
        CatalogEntry {
            id,
            specs: vec![PackageSpec::any(alternatives)],
        }
    }

    /// an **optional** entry: unavailable means carry on.
    pub fn optional(id: DepId, alternatives: &[&str]) -> Self {
        CatalogEntry {
            id,
            specs: vec![PackageSpec::optional(alternatives)],
        }
    }

    /// an entry that costs **several packages**.
    pub fn many(id: DepId, packages: &[&str]) -> Self {
        CatalogEntry {
            id,
            specs: packages.iter().map(|p| PackageSpec::one(p)).collect(),
        }
    }
}

/// the package names a family knows.
///
/// the list lives in the backend because it **is** the manager's knowledge: "on
/// dnf the names are these" is no different from "on dnf you install like
/// this". keeping them apart would mean two places to update per dependency,
/// and R6-hotfix-2's lesson is that lists must be protected from refactors, not
/// multiplied.
///
/// **data**, not methods: readable in one block, and freezable by a test.
#[derive(Debug, Clone)]
pub struct PackageCatalog {
    /// low-risk common utilities, installed first.
    pub bootstrap: Vec<CatalogEntry>,
    /// Odoo's system dependencies, mandatory and optional together, since
    /// `PackageSpec` already carries the distinction.
    pub odoo: Vec<CatalogEntry>,
    /// the packages that install the PostgreSQL server.
    pub postgres: Vec<String>,
    /// the name to ask "is PostgreSQL installed?" with. **not** the first
    /// element of `postgres`: a different question, and on Fedora a different
    /// answer.
    pub postgres_marker: String,
    /// the nginx package.
    pub nginx: String,
    /// the **alternative** Python interpreters this family packages, newest
    /// first.
    ///
    /// used by M11: when the system `python3` is newer than Odoo's pins, the
    /// venv is built on one of these (A-MD-7). empty is a legitimate answer,
    /// not a gap.
    pub alternate_pythons: Vec<AlternatePython>,
}

/// an alternative Python interpreter, as a family packages it.
///
/// the two names travel together because they install together: without the
/// headers no extension compiles, and six of them need building on the chosen
/// branch (measured on Fedora 44).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlternatePython {
    /// `(3, 13)`.
    pub version: (u32, u32),
    /// the interpreter's package, which is also the **command**: on both
    /// families `python3.13` names the package and the binary. should they ever
    /// diverge, this field splits in two rather than being guessed.
    pub interpreter: String,
    /// the headers package.
    pub devel: String,
}

impl AlternatePython {
    pub fn new(version: (u32, u32), interpreter: &str, devel: &str) -> Self {
        AlternatePython {
            version,
            interpreter: interpreter.to_string(),
            devel: devel.to_string(),
        }
    }
}

impl PackageCatalog {
    /// the bootstrap specs, flattened: what the step consumes.
    pub fn bootstrap_specs(&self) -> Vec<PackageSpec> {
        Self::flatten(&self.bootstrap)
    }

    /// the Odoo dependency specs, flattened.
    pub fn odoo_specs(&self) -> Vec<PackageSpec> {
        Self::flatten(&self.odoo)
    }

    fn flatten(entries: &[CatalogEntry]) -> Vec<PackageSpec> {
        entries.iter().flat_map(|e| e.specs.clone()).collect()
    }

    /// the names this family gives a need, flattened.
    ///
    /// used by M11 for **one** thing: knowing which names `DepId::PythonDev`
    /// carries, i.e. the system Python's headers, which an alternative
    /// interpreter makes pointless. never hardcoded in the plan — the catalogue
    /// is the only thing that knows them here.
    pub fn names_for(&self, id: DepId) -> Vec<String> {
        self.bootstrap
            .iter()
            .chain(self.odoo.iter())
            .filter(|e| e.id == id)
            .flat_map(|e| e.specs.iter())
            .flat_map(|s| s.alternatives().to_vec())
            .collect()
    }

    /// does this catalogue cover the need, in either list?
    pub fn covers(&self, id: DepId) -> bool {
        self.bootstrap
            .iter()
            .chain(self.odoo.iter())
            .any(|e| e.id == id && !e.specs.is_empty())
    }
}

/// a package manager's commands.
///
/// deliberately **small and 1:1 onto commands**: no policy in here, so the
/// tests can assert exact sequences and the removal-with-recovery strategy
/// lives in one place.
pub trait PackageManager {
    /// is the package installed **under this name**?
    ///
    /// the wording matters: a purely virtual name answers `false` even right
    /// after being "installed", which is why [`Availability::VirtualOnly`] is
    /// only a fallback.
    fn is_installed(&self, pkg: &str) -> bool;

    /// refreshes the repository indices.
    ///
    /// a mutation, so it lives **only** inside a `run` — never in a `snapshot`,
    /// which never mutates (C4). no undo: a refreshed index changes nothing
    /// about what is installed. like a `git fetch`.
    fn refresh_index(&self) -> Result<(), StepError>;

    /// is the index queryable, i.e. do [`Self::availability`]'s answers mean
    /// anything?
    ///
    /// keeps **blindness** apart from **absence**: with a never-refreshed index
    /// every query answers "unavailable", and without this question that would
    /// become "this package does not exist on this release" — the A5.1-bis
    /// false positive.
    fn index_is_queryable(&self) -> bool;

    /// what the manager can tell us about this name. a query, not a mutation.
    fn availability(&self, pkg: &str) -> Availability;

    /// installs, idempotently, without recommends or weak dependencies.
    fn install(&self, pkgs: &[&str]) -> Result<(), StepError>;

    /// removes **exactly** the packages given.
    ///
    /// named `remove` and not `purge` on purpose: "purge" is a deb concept, and
    /// a method name must not promise semantics one implementation lacks.
    ///
    /// **the invariant every implementation must honour**: remove only what was
    /// asked. no orphaned dependencies — that is [`Self::remove_orphans`],
    /// confined to `--aggressive-rollback`. a manager that does it by default
    /// must have it **explicitly disabled**: it would be the global
    /// `autoremove` R0 banned, unbounded by our delta.
    fn remove(&self, pkgs: &[&str]) -> Result<(), StepError>;

    /// removes orphaned dependencies. **only** under `--aggressive-rollback`.
    fn remove_orphans(&self) -> Result<(), StepError>;

    /// tries to bring the package database back to a consistent state.
    ///
    /// a rollback always runs *after* a failure, and that failure may have left
    /// the manager halfway (A-RT-2: on a broken dpkg, apt refuses to operate
    /// and **every** purge fails). a no-op is legitimate on managers with no
    /// "unpacked but unconfigured" state.
    fn try_repair(&self) -> Result<(), StepError>;

    /// the second recovery level, for when [`Self::try_repair`] is not enough
    /// because the manager itself refuses to operate.
    fn try_deep_repair(&self) -> Result<(), StepError>;

    /// installs a package from a **local file**, resolving its dependencies.
    ///
    /// the path must be absolute or start with `./`: managers only treat an
    /// argument as a file when it contains a `/`.
    fn install_local_file(&self, path: &Path) -> Result<(), StepError>;

    /// what a package file of this format is called.
    ///
    /// used by `install-wkhtmltopdf`, which downloads **from upstream**: the
    /// project publishes both formats under different naming schemes. not our
    /// convention, but knowledge of the **package format**, hence of whoever
    /// installs it.
    ///
    /// the extension matters beyond the name: `apt-get install <file>`
    /// recognises a local path **only** by it, which is why the randomly named
    /// temporary preserves it (R9).
    fn local_package_name(&self, version: &str, suffix: &str) -> String;

    /// the command a user would type to refresh the index, **as text**, for the
    /// diagnostics.
    ///
    /// so we never tell a Fedora user to run `apt-get update`. a wrong
    /// suggestion is worse than none: it sends them to a command that does not
    /// exist and casts doubt on the rest.
    fn refresh_command(&self) -> &'static str;

    /// the package names this family knows.
    fn catalog(&self) -> PackageCatalog;
}
