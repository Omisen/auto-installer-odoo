<!--
Base this on `dev`, not `main`. One change per PR — a diff that mixes two fixes cannot be
validated in the field one at a time.

Delete the sections that do not apply. Do not delete the checklist.
-->

## What this changes, and why

<!-- The problem first, then the fix. If it closes an issue: "Closes #123". -->

## Why this way

<!--
The alternative you did not take, and what decided it. This is the part that is impossible to
reconstruct later from the diff, and it is what the review will ask about first.
-->

## How you convinced yourself it works

<!--
Not "tests pass" — which test, and what did you see go red before it went green?

If you added a guard: what did you break on purpose to check it can fail? A test that cannot fail
is not a test.
-->

## Scope

- [ ] Behaviour visible to whoever installs (flags, `.env` keys, messages, generated files)
- [ ] A new step, or a change to an existing one
- [ ] Something that differs between distributions
- [ ] The rollback, the manifest, or how state is persisted
- [ ] CI, packaging or release
- [ ] Documentation only

**Which distributions and Odoo versions did you exercise this on?**
<!--
The integration CI installs for real, but it does NOT run on pull requests — only on demand and on
main/dev. If your change is likely to show up only there, say so and a maintainer will trigger it.
-->

---

## Checklist

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `cargo test`
- [ ] `cargo build --release`
- [ ] No `.unwrap()` / `.expect()` in production code — every failure is a `Result` that says *what*
      and *where*
- [ ] Documentation updated where behaviour changed: `README.md`, `PACKAGE-README`, the wiki
- [ ] Commit subject in the form `[TAG] What changed`, saying what it does for the system

### If this adds or changes a step

- [ ] `snapshot` runs before the mutation and **mutates nothing**
- [ ] `undo` acts only when the step completed *and* the `PreState` is `CreatedByUs`; it is
      best-effort and idempotent
- [ ] Registered in **both** `steps::build_steps()` and `steps::step_by_name()`
- [ ] `snapshot_value` and `rehydrate` are exact inverses, and the step is in the chain in
      `tests/rehydrate.rs`
- [ ] `steps::artifact_scope` was **chosen** — `OwnInstance` / `Shared` / `Mixed` — not inherited
      from the catch-all
- [ ] Nothing it causes to exist is born outside the reversible perimeter, including files another
      program creates on our behalf

### If it differs between distributions

- [ ] No `match` on the OS inside a step: the divergence sits behind `ops.packages()` or
      `ops.distro()`
- [ ] Package names are groups of alternatives per family, and every `DepId` is covered by both
      catalogues
- [ ] A job in `integration.yml` exercises the new branch — something nobody runs is a promise, not
      a feature

<!--
Before requesting review, the five questions from CONTRIBUTING.md:

  1. Can this check answer *no* in production?
  2. In the scenario I wrote it for, is it even reached?
  3. This data — which question did it originally answer?
  4. Does this remedy survive its own report?
  5. What has to agree with this, and would anything go red if it stopped agreeing?
-->
