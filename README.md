<p align="center">
  <img src=".github/assets/invok-logo.png" alt="Invok" width="600">
</p>

> Installer for Odoo

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![CI](https://github.com/Omisen/invok/actions/workflows/test.yml/badge.svg)](https://github.com/Omisen/invok/actions/workflows/test.yml)
[![Integration](https://github.com/Omisen/invok/actions/workflows/integration.yml/badge.svg)](https://github.com/Omisen/invok/actions/workflows/integration.yml)
![Odoo 16–19](https://img.shields.io/badge/Odoo-16%20%7C%2017%20%7C%2018%20%7C%2019-875A7B)
![Ubuntu · Debian · Fedora](https://img.shields.io/badge/Ubuntu%20%C2%B7%20Debian%20%C2%B7%20Fedora-supported-informational)

Installer for **Odoo 16 / 17 / 18 / 19** on Ubuntu ≥ 22.04, Debian ≥ 11 and Fedora ≥ 40, written in
**Rust**, with **transactional rollback**: *either the installation succeeds completely, or the system
goes back to exactly what it was.* It sets up the system user, dependencies, PostgreSQL, the Odoo
sources, a virtualenv, the config file, a systemd service, optionally Nginx, and an `odoo` helper
command.

The command is **`invok`**. The `.deb` and `.rpm` packages also install the short alias **`vok`**, a
symlink to the same program: `vok --dry-run` and `invok --dry-run` do exactly the same thing.

*Invok — from “invoke”: to call something into being.*

> **Independent project.** Not affiliated with Odoo S.A., nor endorsed or sponsored by it. “Odoo” is a
> trademark of Odoo S.A. and is used here only to name the software this tool installs. The installer
> **does not redistribute Odoo code**: it downloads it at runtime from the official
> [`odoo/odoo`](https://github.com/odoo/odoo) repository, onto the target machine.

Full technical documentation — engine, step-by-step reference, rollback model, multi-distro support —
lives in the **[wiki](https://github.com/Omisen/invok/wiki)**.

---

## What sets it apart

- **Surgical, verified rollback** — if a step fails, the steps already executed are undone in reverse
  order, and resources that were **already on the machine** are never touched. It is proven by
  end-to-end tests *and* by a CI job that installs and uninstalls on real machines.
- **One binary, no runtime** — a native executable; git, the package manager, psql and venv stay
  external commands.
- **Three families, no scattered `if`s** — `apt` and `dnf` sit behind two boundaries, and no step
  knows which distribution it runs on.
- **Resumable, and never destructive with itself** — an interrupted installation resumes where it
  stopped; a completed one is never silently overwritten. The `.env` file is parsed **declaratively**,
  never executed as code.
- **Interruptible** — Ctrl-C rolls back and restores the system instead of leaving it half-done.
- **One flow, two modes** — guided (interactive prompts) or non-interactive (`--config`/flags/CI).

---

## Requirements

| Requirement | Detail |
|---|---|
| OS | **Ubuntu ≥ 22.04**, **Debian ≥ 11** or **Fedora ≥ 40** — exercised in CI up to Ubuntu 24.04, Debian 12 and Fedora 44 (full cycle: install, service up, rollback). A newer release is accepted with a warning, not refused |
| Python | the installer **picks the interpreter**: the system one when Odoo's pins cover it, otherwise the newest interpreter packaged by the distribution that they do cover. On **Fedora ≥ 43** the venv is built on `python3.13`, installed for the occasion and removed by the rollback |
| Privileges | a normal user with `sudo` (not a direct root login) |
| Disk | ≥ 5 GB free (override with `MIN_DISK_GB`) |
| Ports | 8069 free; 80/443 too when using Nginx — unless it is Nginx itself holding them, which is not a conflict |

`ODOO_HOME` is the **constant** `/opt/odoo` and cannot be overridden.

---

## Install

Same program in every case, and in every case Odoo itself is installed **at runtime**, when you run
the command. The difference that matters is **who compiles**: A, B and C give you a ready-made static
musl binary with no dependencies; D and E build it on your machine.

| | For | |
|---|---|---|
| **A** | any distro, nothing to install | `.tar.gz` |
| **B** | Ubuntu / Debian, command in `PATH` + `vok` alias | `.deb` |
| **C** | Fedora, same | `.rpm` |
| **D** | you already have Rust | `cargo install` |
| **E** | from a clone of this repository | `cargo build` |

> Updates come from the **[Releases](../../releases/latest)** page: every version is there with its
> `sha256`, and you update by downloading again. There is no `apt`/`dnf` repository to add to the
> machine's sources.

The commands below point at **v3.0.0**, the release this README describes. If a newer one exists, find
it on [Releases](../../releases/latest) and change the version in the URLs and file names.

### A — Any distro: prebuilt binary

Two Linux x86_64 variants: `…-musl.tar.gz` is **static** and runs anywhere (recommended);
`…-gnu.tar.gz` is dynamic, for systems with a recent glibc. Each archive ships a `.sha256`.

```bash
curl -fsSL -O https://github.com/Omisen/invok/releases/download/v3.0.0/invok-x86_64-unknown-linux-musl.tar.gz
curl -fsSL -O https://github.com/Omisen/invok/releases/download/v3.0.0/invok-x86_64-unknown-linux-musl.tar.gz.sha256

sha256sum -c invok-x86_64-unknown-linux-musl.tar.gz.sha256   # must say: OK
tar xzf invok-x86_64-unknown-linux-musl.tar.gz

./invok -V              # which version is this
sudo ./invok            # guided (interactive)
sudo ./invok --config production.env --with-nginx   # or non-interactive
```

### B — Ubuntu / Debian: `.deb` package

Puts `invok` in `PATH`, removable with `apt remove invok`. It ships **only** the CLI binary: no
services, no system changes.

```bash
curl -fsSL -O https://github.com/Omisen/invok/releases/download/v3.0.0/invok_3.0.0-1_amd64.deb
curl -fsSL -O https://github.com/Omisen/invok/releases/download/v3.0.0/invok_3.0.0-1_amd64.deb.sha256

sha256sum -c invok_3.0.0-1_amd64.deb.sha256   # must say: OK
sudo apt install ./invok_3.0.0-1_amd64.deb

invok -V                # which version is installed
sudo invok              # now on PATH — `sudo vok` is the same program
```

The package creates `/usr/bin/vok` as a link to `/usr/bin/invok`. If a `/usr/bin/vok` already exists
and is **not** a link, the alias is skipped and the installation says so: someone else's file does not
get overwritten.

### C — Fedora: `.rpm` package

The same binary in the other wrapper. Removable with `sudo dnf remove invok`.

```bash
curl -fsSL -O https://github.com/Omisen/invok/releases/download/v3.0.0/invok-3.0.0-1.x86_64.rpm
curl -fsSL -O https://github.com/Omisen/invok/releases/download/v3.0.0/invok-3.0.0-1.x86_64.rpm.sha256

sha256sum -c invok-3.0.0-1.x86_64.rpm.sha256   # must say: OK
sudo dnf install ./invok-3.0.0-1.x86_64.rpm

invok -V
sudo invok
```

### D — Any distro with Rust: `cargo install`

The crate is on **[crates.io](https://crates.io/crates/invok)**.

```bash
cargo install invok
sudo "$(command -v invok)"   # cargo installs into ~/.cargo/bin, which root has no PATH entry for
```

This is the path people get wrong, so: `cargo install` **compiles from source** (options A, B and C
hand you the *same* executable ready-made), the binary lands in `~/.cargo/bin/` which is not on root's
`PATH` — a bare `sudo invok` answers `command not found` — and there is no `vok` alias, which only B
and C create. Update with `cargo install invok --force`.

### E — Any distro: build from source

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust toolchain, once
source "$HOME/.cargo/env"

git clone https://github.com/Omisen/invok.git && cd invok
cargo build --release            # → target/release/invok

sudo ./target/release/invok
```

Run it **via `sudo` from a normal user**: `SUDO_USER` becomes the owner of the `odoo` helper command.
Parameters resolve in this order: **CLI → `.env` → interactive prompt → default**.

---

## Configuration

| Flag | Value | Default |
|---|---|---|
| `--version` | `16` \| `17` \| `18` \| `19` (or `NN.0`) | `18.0` |
| `--odoo-user` | system user | `odoo` |
| `--db-user` | PostgreSQL role | = `--odoo-user` |
| `--db-password` | password for the DB role | empty → peer authentication |
| `--port` | HTTP port (1–65535) | `8069` |
| `--db-name` | database name | `odoo` |
| `--install-dir` | install directory (must live under `/opt/odoo`) | `/opt/odoo/odoo<N>` |
| `--admin-passwd` | Odoo master password | `admin` (discouraged) |
| `--with-nginx` | Nginx reverse proxy | off |
| `--server-name` | `server_name` of the Nginx vhost | `_` (catch-all) |
| `--config <FILE>` | load a `.env` file (declarative) | — |
| `--dry-run` | print the plan without changing anything | off |
| `--force` | install even if a manifest exists, archiving it instead of overwriting | off |

Also: `--install-dir`, `--logfile`, `--open-https-port` (opens 443 on the firewall ahead of TLS; it
does **not** configure TLS — legacy alias `--enable-ssl`), `--aggressive-rollback`, and
`-V`/`--installer-version` for the **installer** version (`--version` is Odoo's). `--help` and the
wiki have them all. Subcommand: `invok rollback` (alias `uninstall`), below; with no subcommand, the
command installs.

```bash
sudo invok --version 17 --with-nginx
sudo invok --with-nginx --server-name '[your-domain.example.com]'
sudo invok --version 18 --db-name odoo --port 8069 --admin-passwd '[YOUR_ADMIN_PASSWORD]'
```

### The `.env` file

With `--config <FILE>` the parameters come from a `KEY=VALUE` file. Unlike the old Bash version (which
`source`d the file — code execution as root), parsing here is **declarative**: `KEY=VALUE` lines, `#`
comments and blank lines ignored, **nothing executed** (a value like `$(...)` stays a literal string).
Unknown keys produce a warning and are ignored.

Recognised keys: `ODOO_VERSION`, `ODOO_USER`, `DB_USER`, `DB_PASSWORD`, `ODOO_PORT`, `DB_NAME`,
`ODOO_INSTALL_DIR`, `ODOO_ADMIN_PASSWD`, `ODOO_LOGFILE`, `WITH_NGINX`, `NGINX_SERVER_NAME`,
`NGINX_OPEN_HTTPS_PORT` (legacy alias `NGINX_ENABLE_SSL`). `ODOO_HOME` is constant and ignored.

```bash
# production.env
ODOO_VERSION=18
ODOO_USER=odoo
ODOO_PORT=8069
DB_NAME=odoo
WITH_NGINX=true
# ODOO_ADMIN_PASSWD=...   # do NOT use 'admin' in production
```

> **You write this file.** The repository ships no `production.env`: a `.env` holds the admin and
> database passwords, so `configs/*.env` is git-ignored by design. The only exceptions are the CI
> presets — `configs/ci.env` and `configs/ci-nginx.env`, throwaway files for ephemeral runners — which
> serve as a complete example to copy.

### Environment variables (network tuning)

Not installation parameters, but knobs for difficult networks, read from the process environment.

| Variable | Default | What it does |
|---|---|---|
| `ODOO_NETWORK_TIMEOUT_SECS` | `300` | Timeout for network operations (Odoo clone, fallback tarball, wkhtmltopdf `.deb` download). On expiry the command is interrupted with a clear error instead of hanging. `0` disables it |
| `GIT_CLONE_RETRIES` | `3` | `git clone` attempts before falling back to the tarball (a timeout consumes one attempt) |
| `GIT_DEPTH` | `5` | Shallow clone depth |

`apt-get` and long local operations (DB init, `pip install`, venv creation) have **no** timeout:
cutting them short does more damage than waiting.

---

## Preview, and life after the installation

`--dry-run` runs the snapshots only (read-only) and prints the **plan**, telling "would act" apart from
"no-op (already present)". Nothing is changed and no state is persisted. It works with or without
`sudo`, but not identically: snapshots *interrogate* the system, and some do it through `sudo`
(PostgreSQL state, installed packages). Without privileges those steps show up as “snapshot
unavailable” and the plan, though true, is incomplete — the installer says so before printing it.

```bash
sudo invok --config production.env --dry-run   # the plan, before anything happens

systemctl status odoo18                  # N = short version, e.g. 18
journalctl -u odoo18 -n 50 --no-pager

odoo status                              # helper command; after: source ~/.bashrc
                                         # start | stop | restart | status | dev

sudo cat /var/log/invok.log              # installer log (post-mortem; survives rollback, by design)
```

---

## Rollback

Before mutating anything, every step records whether what it is about to create **already existed**.
If a step fails, the previous ones are undone **in reverse order** (best-effort, idempotent). The key
guarantee is about **pre-existing resources**, which a rollback never touches:

- an **existing database with the same name** is never dropped (it may hold real data);
- an already-installed **PostgreSQL** stays (stop/disable by default, never purged without the flag);
- an existing **`/opt/odoo`** stays;
- the user's **`~/.bashrc`** comes back **byte for byte** (only our line is removed).

**Re-running the installer.** A registered installation is never silently overwritten. If the previous
one was **complete**, the installer stops and tells you what to do — `invok rollback` to remove it, or
`--force` to install over it, which *archives* the old manifest rather than deleting it. If it was
**interrupted** (Ctrl-C, crash, power loss), it resumes where it stopped: steps already executed are
not redone, and the record that those artifacts are **ours** is preserved — which is what lets the
rollback remove them months later. Resuming needs the **same parameters**: with a different database
name the installer stops and says which field does not match.

**Ctrl-C.** A Ctrl-C (or a `kill`/`systemctl stop`) no longer kills the installer: the installation
**rolls itself back**. The interruption takes effect *between* steps — the one in progress is carried
to completion, because stopping an `apt` halfway would leave `dpkg` inconsistent. The wait is short:
the signal reaches the whole process group, so the running command ends by itself. A **second Ctrl-C
exits immediately** with code 130, leaving the system half-done by your choice; clean it up with
`sudo invok rollback`.

> **From a script, signal the installer only.** “Two Ctrl-C” means *two signals received*. A
> `sudo pkill -INT -f invok` hits **two** processes — the `sudo` and the installer — and so counts as
> the second press: immediate exit, no rollback. Use `sudo pkill -INT -x invok` (`-x` = exact process
> name). From a terminal the problem does not arise.

---

## Uninstalling / cleaning up

```bash
sudo invok rollback --dry-run   # what would be removed, touching nothing
sudo invok rollback             # remove for real (asks for confirmation)
```

`uninstall` is an alias of the same command. It reads the state left by the installation, rebuilds the
steps with **the snapshot taken back then** and runs their undos in reverse order. The same guarantee
holds: only what the installer created is removed. A database that already existed stays where it is —
because the saved snapshot says so, not because of an inspection made at rollback time, which by then
could no longer tell the two cases apart.

| Flag | Value | Default |
|---|---|---|
| `--state <FILE>` | state file to consume | `/var/lib/invok/state.json` (falls back to the historical `/opt/odoo/.installer-state.json`) |
| `--dry-run` | list without mutating (no `sudo` needed) | off |
| `--aggressive-rollback` | also purge PostgreSQL/Nginx installed by us, and the common utilities | off |
| `--yes` / `-y` | skip the confirmation (required with no terminal) | off |

On a successful installation the state file **stays on disk**: it is the *uninstall manifest*, the only
record of which artifacts that installation created and which it found already there. Without it
`invok rollback` could not tell the two apart, and could not remove the instance. Do not delete it by
hand. It is removed only after a complete rollback; if something could not be cleaned up the file
stays and the command can be re-run (undos are idempotent).

Three files live **outside** `/opt/odoo`, which is the perimeter the rollback must be able to remove
whole: the manifest `/var/lib/invok/state.json`, the lock `/run/invok.lock` (prevents two concurrent
installations, gone after a reboot) and the log `/var/log/invok.log`.

---

## Security notes

- **Admin password `admin`**: discouraged. Interactively it needs an explicit confirmation; in
  non-interactive mode with `admin_passwd=admin` the installer **stops**. The password never reaches
  the logs or the summary.
- **wkhtmltopdf checksum (TOFU)**: the SHA-256 of the package is verified before installing. Upstream
  publishes no checksums or signatures, so the pins are **manual, trust-on-first-use** and preloaded in
  the source. A download that does not match its pin — or a variant with no pin — is *fail-closed*
  refused, with no bypass.
- **The `odoo` helper is not global**: installed for the installing user only (`~/.local/bin`).
- **TLS is not configured by the installer, deliberately.** The generated Nginx vhost listens on port
  80 only. For HTTPS use `certbot --nginx`, which obtains the certificates and **rewrites the vhost
  itself**, adding the 443 block and the redirect; `--open-https-port` only opens 443 on the firewall
  ahead of that step.

  ```bash
  sudo apt install certbot python3-certbot-nginx
  sudo certbot --nginx -d odoo.example.com
  ```

Two behaviours worth knowing before you deploy — how the Python interpreter is chosen, and the one
visible difference between the two Nginx layouts (on Fedora the default site lives *inside*
`nginx.conf` and is deliberately left alone) — are in the
**[wiki](https://github.com/Omisen/invok/wiki)**. Vulnerability reports: [SECURITY.md](SECURITY.md).

---

## Development

```bash
cargo build
cargo test          # runs without root: the system is behind a mock
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

The tests cover every step (snapshot→run→undo round trip), the coordination between steps and the
**end-to-end rollback** (injected failure → final state == initial state; pre-existing resources
untouched). `.github/workflows/test.yml` runs them on every push. They run against a **mock** of the
system, so they prove the logic, not the integration with a real package manager, PostgreSQL and
systemd. That is `.github/workflows/integration.yml`, which really installs Odoo on ephemeral runners
and containers — Ubuntu, Debian, Fedora, with and without Nginx, with a pre-existing `odoo` user, and
with a real `SIGINT` mid-installation — then checks that `invok rollback` leaves the system clean,
package by package. It runs on demand and on `main`/`dev`. The matrix and its declared limits are in
the wiki.

```bash
MODE=full bash scripts/ci/integration-test.sh   # runnable by hand — DESTRUCTIVE: throwaway VMs only
```

Contributions welcome. Run the four commands above before opening a PR, and keep to three rules this
project takes seriously:

1. **Every mutation is reversible.** A new step carries `snapshot`/`run`/`undo` and a `PreState`; if it
   creates something the rollback cannot remove, it is not finished. The `Step` trait is not to be
   modified in order to add a step.
2. **No `.unwrap()`/`.expect()` in production code**: every failure is a `Result` that says *what* and
   *where*.
3. **A test that cannot fail is not a test.** Check that your red case really goes red first.

If the change affects behaviour on one distribution, say so in the PR: the integration CI is where that
shows up.

---

## History and licence

The installer was originally written in **Bash**; those versions are archived at tags
[`v1.0.0`](../../releases/tag/v1.0.0) and [`v1.2.0`](../../releases/tag/v1.2.0). The current version is
a complete **rewrite in Rust** with transactional rollback, and is the only one in this repository.

MIT — see [LICENSE](LICENSE). The published `.tar.gz`, `.deb` and `.rpm` contain **only** the `invok`
binary and this README: no third-party code is redistributed. Odoo and wkhtmltopdf (both LGPLv3) are
downloaded at runtime from their official sources and remain subject to their own licences.

This project is independent and is **not affiliated with Odoo S.A.**, nor endorsed by it; “Odoo” is a
trademark of Odoo S.A.

---
<a href="https://github.com/Omisen/invok/wiki"><h2>→ Technical documentation</h2></a>
