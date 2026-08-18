# Contributing to Invok

Thanks for being here. Invok installs Odoo on machines that belong to somebody else, so most of the
rules below exist for one reason: **a defect here is not a crash, it is a customer's data.** Read the
two short sections at the top before writing code; the rest is reference.

By taking part you agree to the [Code of Conduct](CODE_OF_CONDUCT.md). Security problems do **not**
go in a public issue — see [SECURITY.md](SECURITY.md).

---

## The one requirement everything else serves

**Either the installation succeeds completely, or the system goes back exactly as it was.** No dirty
in-between state. That is why this project is in Rust, why every mutation is registered before it
happens, and why a change that creates something the rollback cannot remove is not finished.

Three rules follow, and they are not negotiable:

1. **Every mutation is reversible.** A step carries `snapshot` / `run` / `undo` and a `PreState`. The
   snapshot always runs *before* the mutation and is the only source of truth for the undo.
2. **No `.unwrap()` / `.expect()` in production code.** Every failure is a `Result` that says *what*
   and *where*. (Test code may use `expect` freely — a panic there is a failing test, not a broken
   machine.)
3. **A test that cannot fail is not a test.** Before you trust a new test, break the code on purpose
   and check it goes red.

Two protections in the existing code are the value of the project. Do not weaken them, and if a
change appears to require it, stop and open an issue instead:

- **anti-drop** — a pre-existing database is never dropped. A database with the same name may hold
  the customer's real data. The same rule governs the filestore, which is that data's on-disk half.
- **hard-stop** — the installer refuses `odoo-bin -i base` on a database it did not create. This is a
  precondition, not an undo: it is a mutation that must not begin.

---

## Do not run the installer on your own machine

`invok` creates system users, installs packages, touches PostgreSQL, systemd and Nginx, and writes
under `/opt`. Neither it nor `scripts/ci/integration-test.sh` is safe on a workstation.

Use a throwaway VM or container. `MODE=full bash scripts/ci/integration-test.sh` is runnable by hand,
and it is **destructive by design** — that is what it verifies.

Everything you need for a normal contribution runs without root, because the system lives behind the
`SystemOps` boundary and the tests drive a mock of it.

---

## Setting up

Stable Rust, edition 2021, no other toolchain requirement. `Cargo.lock` is committed and CI builds
with `--locked`.

```bash
git clone https://github.com/Omisen/invok
cd invok
cargo build
```

Before opening a pull request, these four must be clean — they are what the fast CI runs:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

If you touched doc comments, add `cargo doc --no-deps --lib` with `RUSTDOCFLAGS=-D warnings`: the
crate is on crates.io, so docs.rs publishes those pages at every release whether we look after them
or not, and a broken intra-doc link is a page that sends a reader nowhere.

If you touched anything under `scripts/ci/`, add `bash scripts/ci/selftest-journal.sh` and
`bash -n scripts/ci/*.sh`.

### The two levels of testing, and what each one proves

- **`cargo test`** — the whole step catalogue (snapshot → run → undo round trip), the coordination
  between steps, and the end-to-end rollback (injected failure → final state equals initial state,
  pre-existing resources untouched). It runs against a **mock**, so it proves the *logic*, never the
  integration with a real package manager, PostgreSQL or systemd. It runs on every push and PR.
- **`.github/workflows/integration.yml`** — really installs Odoo on ephemeral runners and containers:
  Ubuntu, Debian, Fedora, with and without Nginx, with a pre-existing `odoo` user, with two instances
  side by side, and with a real `SIGINT` mid-installation. Then it checks that `invok rollback` leaves
  the system clean, package by package.

  It is slow and destructive, so it runs **on demand and on `main` / `dev` only** — *not* on your
  pull request. A maintainer triggers it. Say in the PR description if your change is likely to show
  up only there.

Almost every defect in this project's history was found by the integration CI or by a VM, and almost
none by re-reading the code. Write for that.

---

## Adding or changing a step

The `Step` trait does **not** change in order to add a step. If it looks like it must, stop and open
an issue: that is a design question, not an implementation detail.

A new step is done when all of these are true:

- [ ] `snapshot` records whether what the step creates already existed, and **mutates nothing** —
      snapshots are read-only, always.
- [ ] `run` mutates; `undo` acts **only** when the step completed *and* the `PreState` is
      `CreatedByUs`. `Preexisting` means the artifact is not ours to destroy.
- [ ] `undo` is best-effort and idempotent: it does not fail because the artifact is already gone; if
      it fails it logs a warning and lets the rollback continue.
- [ ] The step is registered in **both** `steps::build_steps()` (the canonical order) and
      `steps::step_by_name()` (rebuilding by name, for the rollback from disk). Guarded by
      `tests/rollback_command.rs::the_factory_covers_the_whole_canonical_sequence` — a step known only
      to the first breaks nothing during an installation and surfaces months later on a customer
      machine as "that piece was not removed".
- [ ] `snapshot_value` and `rehydrate` are exact inverses. Guarded by
      `tests/rehydrate.rs::every_step_rehydrates_to_an_identical_snapshot`, and the step belongs in
      that file's chain. The rollback from disk never re-runs `snapshot`: it would photograph the
      system *after* our mutations.
- [ ] The step **chooses** its `steps::artifact_scope` — `OwnInstance`, `Shared` or `Mixed` —
      instead of inheriting the catch-all. Guarded by
      `tests/shared_artifacts.rs::every_step_in_the_sequence_has_a_scope_chosen_on_purpose`. A shared
      artifact belongs to whoever created it and is not touched while another instance is installed.
- [ ] Nothing the step causes to exist is born **outside** the reversible perimeter — including files
      created on our behalf by another program (pip caches, Odoo's own filestore). An artifact nobody
      records cannot be undone, and no after-the-fact cleanup inside a customer's home is as safe.
- [ ] Anything the installer opens or creates *before* the engine, or that outlives the undos, lives
      outside `/opt/odoo`: that directory is the perimeter the rollback has to be able to remove.

Ordering matters and is part of the design: undos run in reverse. If your step's position is load
bearing, say so in the module doc — as `setup_data_dir` and `nginx_install` do.

## Code that differs between distributions

**No step knows which distribution it is running on.** A `match` on the OS inside a step means the
divergence landed in the wrong place. Everything that differs asks one of two boundaries, both
obtained from `SystemOps`:

- `ops.packages()` — *which commands install, and what the package is called here* (`src/packaging/`);
- `ops.distro()` — *where files live, which concepts exist, who governs the firewall*
  (`src/distro/`).

Package names are **groups of alternatives per family**, never flat lists, and the alternatives must
be synonyms within the *same* family. Every `DepId` must be covered by both catalogues —
`tests/catalog_parity.rs::every_need_is_covered_by_every_family`.

The family is **re-read from the manifest**, never re-detected and never guessed from which package
manager happens to be installed.

And: a family, a version, or any other axis the program *promises* has to be exercised by a job in
`integration.yml`. Something nobody runs is a promise, not a feature. Before adding a value a parser
accepts, ask which job goes through it.

---

## Writing the tests

Name a test as the sentence it asserts — `every_step_rehydrates_to_an_identical_snapshot`,
`preexisting_resources_survive_rollback`. The file reads as a list of claims about the system.

Then check the test can actually fail:

1. Make the smallest change to production code that should break it (invert a condition, drop a
   field, widen a comparison).
2. Confirm it goes red, and that the message names the real problem.
3. Put the code back.

Two failure modes this project has hit more than once, and now looks for on purpose:

- **A comparison that is too wide.** `status.contains("80/tcp")` answers `true` on a machine that only
  has `8080/tcp`. Compare the field you mean, not the line it sits in.
- **A test that distinguishes nothing.** An `expect_err` that never looks at *which* error passes when
  the wrong step fails. In shell and CI, `exit != 0` does not say *why*: assert the message when the
  value of a check is in its reason.

If a mock's answer is more ideal than the real command's, the mock is the bug. Make it reproduce the
real output shape — that is the only way a test can catch a wrong question being asked of the system.

## Comments and documentation

Module-level docs (`//!`) explain **why the code is the way it is**: the case that made it necessary,
what was tried, what was deliberately not done. That history is the part a reader cannot reconstruct
from the code, and it is why this codebase's comments are long. `//` comments are short and lowercase;
prose is English.

Declare limits rather than hiding them. Where behaviour is knowingly incomplete — a residue we accept,
a combination not covered — the code and the docs say so, in those words.

If your change alters user-visible behaviour, update the places that describe it: `README.md`,
`PACKAGE-README` (the short guide shipped inside the packages) and the wiki. A guard on documentation
asserts the **behaviour first**, then that the prose matches; three documents agreeing with each other
and disagreeing with the code has already happened here.

---

## Commits and pull requests

- **Branch off `dev`**, and open the PR against `dev`. `main` is release-only.
- **One change per commit and per PR.** Atomic, reviewable, revertible on its own. A diff that mixes
  two fixes cannot be validated in the field one at a time.
- Commit subjects are English, in the form `[TAG] What changed`, where the tag is one of
  `ADD`, `FIX`, `UPDT`, `DOC`, `TEST`, `REL` (combinations like `[ADD & UPDT]` exist). Say what the
  change *does* for the system, not which files it touched:

  ```
  [FIX] The halfway rollback learns the shared-artifact rule
  [ADD] Give each instance its own gevent port and nginx upstreams
  ```

- In the PR description, cover: the problem, why this fix and not the obvious alternative, and how you
  convinced yourself it works. If the change affects behaviour on one distribution only, say so — the
  integration CI is where that shows up, and it does not run on your PR.
- Bug reports are more useful with the installer's log (`/var/log/invok.log`), the manifest
  (`/var/lib/invok/`), the distribution, and the Odoo version. Redact passwords; the log does not
  contain them, but `.env` files do.

---

## The questions this project asks before shipping anything

Nearly every serious defect found here reduces to one of these. They cost seconds to ask:

1. **Can this check answer *no* in production?** A check that cannot fail is indistinguishable from
   an absent one.
2. **In the scenario I wrote it for, is it even reached?** An unreachable check also gives a
   diagnosis that sends people to fix the wrong thing.
3. **This data — which question did it originally answer?** Reusing a value for a *new* question is
   how a manifest meaning "these steps ran" got read as "these artifacts exist".
4. **Does this remedy survive its own report?** An undo, a restore or a diagnostic must not die on the
   log line describing what it is about to do — least of all in the degraded state it exists for.
5. **What has to agree with this, and would anything go red if it stopped agreeing?** Two sources of
   truth with nobody checking is this project's signature failure.

---

## Releases (maintainers)

The version is raised in **three** places, not two: `version` in `Cargo.toml`, the `invok` entry in
`Cargo.lock`, and the download commands in `README.md` — which spell out full file names and URLs.
Mind the packaging **revision** (`invok_3.3.0-1_amd64.deb`): forgetting it produces a URL that 404s.

A stale README does not go red on its own — the previous release's files still exist, so the commands
keep working and a customer installs an old version with nothing to signal it. Raise the version,
update the README, and let `cargo test` tell you what is missing.

The release and its tag are created **by a human from the GitHub UI**: open a draft, write the new tag
with target `main`, press Publish. That single act fires both events in `release.yml` — the tag push
(build the artifacts, attach them to the release) and `release: published` (publish to crates.io,
**irreversible**). Every job declares which of the two it belongs to; a test enforces it by name.
