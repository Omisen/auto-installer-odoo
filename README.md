# Odoo Auto Installer

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Installer per **Odoo 16 / 17 / 18 / 19** su Ubuntu ≥ 22.04 e Debian ≥ 11, ora **in Rust** con
**rollback transazionale**: *o l'installazione riesce completamente, o il sistema torna esattamente
com'era prima.* Configura utente di sistema, dipendenze, PostgreSQL, sorgenti Odoo, virtualenv, config,
servizio systemd e (opzionale) Nginx.

---

## Cosa lo distingue

- **Rollback chirurgico verificato** — se un passo fallisce, gli step già eseguiti vengono annullati in
  ordine inverso; le risorse **preesistenti** del cliente non vengono toccate. È una proprietà provata
  con test end-to-end, non una promessa.
- **Binario unico, senza runtime** — un eseguibile nativo; git/apt/psql/venv restano comandi esterni.
- **Idempotente e sicuro** — rileva ciò che esiste già e non lo ricrea; il file `.env` è **parsato in
  modo dichiarativo**, mai eseguito come codice.
- **Un solo flusso, due modi** — guidato (prompt interattivi) oppure non-interattivo
  (`--config`/flag/CI), con la stessa logica.

---

## Requisiti

| Requisito | Dettaglio |
|-----------|-----------|
| OS | Ubuntu ≥ 22.04 **o** Debian ≥ 11 — provati in CI fino a **Ubuntu 24.04** e **Debian 12**; una release più recente viene accettata con un avviso, non rifiutata |
| Privilegi | utente normale con `sudo` (non login diretto come root) |
| Disk | ≥ 5 GB liberi (override `MIN_DISK_GB`) |
| Porte | 8069 (Odoo) libera; 80/443 se si usa Nginx — a meno che a tenerle non sia già Nginx, nel qual caso non è un conflitto |
| Installazione | **binario precompilato** dalle [release](../../releases/latest), oppure build da sorgente (toolchain Rust `cargo`) |

`ODOO_HOME` è **costante** `/opt/odoo` (non sovrascrivibile).

---

## Installazione rapida

### Opzione A — binario precompilato (consigliata, niente Rust)

Scarica l'ultimo binario dalla pagina **[Releases](../../releases/latest)**. Due varianti Linux x86_64:

- `odoo-installer-x86_64-unknown-linux-musl.tar.gz` → **statico**, gira su **qualsiasi** distro (consigliato per i clienti);
- `odoo-installer-x86_64-unknown-linux-gnu.tar.gz` → dinamico, per sistemi con glibc recente.

Ogni archivio ha un file `.sha256` per **verificare l'integrità** del download.

```bash
VER=v2.1.0                       # sostituisci con l'ultima versione
file=odoo-installer-x86_64-unknown-linux-musl.tar.gz
base="https://github.com/Omisen/auto-installer-odoo/releases/download/${VER}"

curl -fsSL -O "${base}/${file}" -O "${base}/${file}.sha256"
sha256sum -c "${file}.sha256"    # deve dire: OK
tar xzf "${file}"

sudo ./odoo-installer                                        # guidato (interattivo)
# oppure non-interattivo:
sudo ./odoo-installer --config production.env --with-nginx
```

### Opzione A-bis — pacchetto `.deb` (esperienza `apt` nativa)

Su Debian/Ubuntu puoi installare l'installer come pacchetto: il comando `odoo-installer`
finisce nel `PATH` ed è rimovibile con `apt remove odoo-installer`. Il `.deb` è **statico**
(musl), quindi gira su qualsiasi distro. Deposita **solo** il binario CLI — non installa
servizi né tocca il sistema: Odoo viene installato a runtime quando lanci il comando.

```bash
VER=v2.1.0                       # sostituisci con l'ultima versione
deb=odoo-installer_2.1.0_amd64.deb
base="https://github.com/Omisen/auto-installer-odoo/releases/download/${VER}"

curl -fsSL -O "${base}/${deb}" -O "${base}/${deb}.sha256"
sha256sum -c "${deb}.sha256"     # deve dire: OK

sudo apt install ./"${deb}"      # oppure: sudo dpkg -i ./"${deb}"

sudo odoo-installer                                          # ora è nel PATH
# oppure non-interattivo:
sudo odoo-installer --config production.env --with-nginx
```

### Opzione B — build da sorgente

```bash
# Toolchain Rust (una volta sola)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# Clona il repository
git clone https://github.com/Omisen/auto-installer-odoo.git
cd auto-installer-odoo

cargo build --release            # → target/release/odoo-installer

sudo ./target/release/odoo-installer
# oppure non-interattivo (esempi in configs/):
sudo ./target/release/odoo-installer --config configs/production.env --with-nginx
```

Va eseguito **via `sudo` da un utente normale** (l'utente `SUDO_USER` diventa proprietario del comando
helper `odoo`). La priorità di risoluzione dei parametri è: **CLI → `.env` → prompt interattivo →
default**.

---

## Opzioni CLI

| Flag | Valore | Default |
|------|--------|---------|
| `--version` | `16` \| `17` \| `18` \| `19` (o `NN.0`) | `18.0` |
| `--odoo-user` | utente di sistema | `odoo` |
| `--db-user` | ruolo PostgreSQL | = `--odoo-user` |
| `--db-password` | password del ruolo DB | vuota → autenticazione peer |
| `--port` | porta HTTP (1–65535) | `8069` |
| `--db-name` | nome database | `odoo` |
| `--install-dir` | dir installazione (deve stare sotto `/opt/odoo`) | `/opt/odoo/odoo<N>` |
| `--admin-passwd` | master password Odoo | `admin` (sconsigliata) |
| `--logfile` | file di log di Odoo | assente → journal/stdout |
| `--with-nginx` | reverse proxy Nginx | disattivo |
| `--server-name` | `server_name` del vhost Nginx | `_` (catch-all) |
| `--open-https-port` | apre la 443 sul firewall in vista di TLS (**non** configura TLS; alias storico: `--enable-ssl`) | disattivo |
| `--config <FILE>` | carica un file `.env` (dichiarativo) | — |
| `--dry-run` | mostra il piano senza mutare nulla | disattivo |
| `--aggressive-rollback` | in rollback purga anche pacchetti che di norma resterebbero | disattivo |
| `--force` | installa anche se esiste già un manifesto, mettendolo da parte invece di sovrascriverlo | disattivo |
| `--help` | messaggio d'aiuto | — |

Sottocomandi: `odoo-installer rollback` (alias `uninstall`) — vedi
[Disinstallare / ripulire](#disinstallare--ripulire-odoo-installer-rollback). Senza sottocomando il
comando installa, come sempre.

Esempi:

```bash
# Nginx + versione 17
sudo ./target/release/odoo-installer --version 17 --with-nginx

# Tutto da CLI
sudo ./target/release/odoo-installer --version 18 --odoo-user odoo --db-name odoo --port 8069

# Anteprima (nessuna modifica; senza sudo il piano è più scarno, vedi sotto)
./target/release/odoo-installer --config configs/production.env --dry-run
```

---

## File di configurazione `.env`

Con `--config <FILE>` i parametri sono letti da un file `KEY=VALUE`. A differenza del Bash (che faceva
`source` del file — code-execution come root), qui il parsing è **dichiarativo**: righe `KEY=VALUE`,
commenti `#` e righe vuote ignorati, **nessuna esecuzione** (un valore come `$(...)` resta stringa
letterale). Le chiavi sconosciute producono un warning e vengono ignorate.

Chiavi riconosciute: `ODOO_VERSION`, `ODOO_USER`, `DB_USER`, `DB_PASSWORD`, `ODOO_PORT`, `DB_NAME`,
`ODOO_INSTALL_DIR`, `ODOO_ADMIN_PASSWD`, `ODOO_LOGFILE`, `WITH_NGINX`, `NGINX_SERVER_NAME`,
`NGINX_OPEN_HTTPS_PORT` (alias storico `NGINX_ENABLE_SSL`). (`ODOO_HOME` è costante e viene ignorata.)

```bash
# configs/production.env
ODOO_VERSION=18
ODOO_USER=odoo
ODOO_PORT=8069
DB_NAME=odoo
WITH_NGINX=true
# ODOO_ADMIN_PASSWD=...   # NON usare 'admin' in produzione
```

### Variabili d'ambiente (regolazioni di rete)

Non sono parametri di installazione ma manopole per reti difficili, lette dall'ambiente del processo:

| Variabile | Default | Cosa fa |
|---|---|---|
| `ODOO_NETWORK_TIMEOUT_SECS` | `300` | Timeout delle operazioni di rete (clone Odoo, tarball di fallback, download del `.deb` wkhtmltopdf). Allo scadere il comando viene interrotto con un errore chiaro invece di restare appeso. `0` disattiva il timeout. |
| `GIT_CLONE_RETRIES` | `3` | Tentativi di `git clone` prima del fallback tarball (un timeout consuma un tentativo). |
| `GIT_DEPTH` | `5` | Profondità del clone shallow. |

`apt-get` e le operazioni locali lunghe (init del DB, `pip install`, creazione del venv) **non** hanno
timeout: interromperle a metà farebbe più danni dell'attesa.

---

## Cosa fa, in breve

Preflight non mutanti (root, sudo, OS, disco, porte, comandi) → poi la sequenza reversibile:

1. crea `/opt/odoo` e l'utente di sistema `odoo`;
2. dipendenze di sistema (apt) e **wkhtmltopdf** (build patchata Qt, con verifica checksum);
3. **PostgreSQL**: installa/abilita/avvia, crea ruolo e database;
4. **sorgenti Odoo** (clone git con retry + fallback tarball), virtualenv, dipendenze pip;
5. genera `odoo<N>.conf`, inizializza lo schema base del DB;
6. **servizio systemd** `odoo<N>` (unit hardenizzata, enable + start);
7. **Nginx** opzionale (`--with-nginx`);
8. comando helper `odoo` per l'utente + patch del `PATH` nel suo `~/.bashrc`.

Il dettaglio di ogni step (snapshot/run/undo) è nella
[wiki](https://github.com/Omisen/auto-installer-odoo/wiki).

---

## Rollback

Ogni step, prima di mutare, registra se ciò che sta per creare **esisteva già**. Se un passo fallisce,
gli step precedenti vengono annullati **in ordine inverso** (best-effort, idempotenti). La garanzia
chiave è sulle **risorse preesistenti**, che non vengono mai toccate da un rollback:

- un **database con lo stesso nome** già esistente **non** viene droppato (potrebbe avere dati reali);
- **PostgreSQL** già installato **resta** (di default stop/disable, mai purge senza flag);
- **`/opt/odoo`** già presente **resta**;
- il **`~/.bashrc`** dell'utente torna **byte-per-byte** com'era (solo la nostra riga viene rimossa).

### Rilanciare l'installer

L'installer **non sovrascrive mai** un'installazione già registrata. Al rilancio:

- se l'installazione precedente era **conclusa**, si ferma e ti dice cosa fare — `odoo-installer
  rollback` per rimuoverla, `--force` per reinstallare sopra. Con `--force` il manifesto precedente
  viene *archiviato*, mai cancellato: se quell'installazione aveva creato qualcosa, quel file è
  l'unica traccia di cosa;
- se era **interrotta** (Ctrl-C, crash, spegnimento), riprende da dove si era fermata: gli step già
  eseguiti non vengono rifatti, e resta registrato che quegli artefatti sono **nostri**. È ciò che
  permette al rollback di rimuoverli anche mesi dopo. Per riprendere servono gli **stessi parametri**:
  con un nome di database diverso l'installer si ferma e dice quale campo non coincide, perché un
  manifesto a metà fra due istanze farebbe agire il rollback sugli artefatti sbagliati.

Il rollback esiste in **due forme**, con le stesse regole:

- **automatico (in-process)** — uno step fallisce e l'installazione si ritira da sola;
- **esplicito (`odoo-installer rollback`)** — rilegge lo stato persistito in
  `/var/lib/odoo-installer/state.json` e annulla ciò che quell'installazione aveva creato. Serve sia a
  **disinstallare** un'istanza funzionante, sia a **ripulire** dopo un Ctrl-C, un `kill -9` o uno
  spegnimento — i casi in cui il processo muore prima di poter fare il rollback da sé.

Usa **`--dry-run`** per vedere il piano prima di eseguire davvero.

---

## Disinstallare / ripulire: `odoo-installer rollback`

```bash
# Cosa verrebbe rimosso, senza toccare nulla
sudo ./target/release/odoo-installer rollback --dry-run

# Rimuovi davvero (chiede conferma)
sudo ./target/release/odoo-installer rollback
```

`uninstall` è un alias dello stesso comando.

Il comando legge lo stato lasciato dall'installazione, ricostruisce gli step con lo **snapshot
dell'epoca** e ne esegue gli `undo` in ordine inverso. Vale la stessa garanzia del rollback automatico:
viene rimosso **solo** ciò che l'installer aveva creato. Un database che esisteva già resta al suo
posto — lo dice lo snapshot salvato, non un'ispezione fatta al momento del rollback (che a quel punto
non saprebbe più distinguere i due casi).

Prima di procedere il comando distingue **installazione completata** da **installazione interrotta a
metà** ed elenca gli step da annullare. A fine esecuzione stampa un riepilogo di cosa è stato rimosso
e, se qualche `undo` non è riuscito, l'elenco esatto di cosa resta da togliere a mano: il rollback è
best-effort, e non fa finta del contrario.

| Flag | Valore | Default |
|------|--------|---------|
| `--state <FILE>` | file di stato da consumare | `/var/lib/odoo-installer/state.json` (ripiego sul percorso storico `/opt/odoo/.installer-state.json`) |
| `--dry-run` | elenca senza mutare (non serve `sudo`) | disattivo |
| `--aggressive-rollback` | purga anche PostgreSQL/Nginx installati da noi e le utility comuni | disattivo |
| `--yes` / `-y` | salta la conferma (obbligatorio senza terminale) | disattivo |

A installazione riuscita il file di stato **resta sul disco**: è il *manifesto di disinstallazione*,
l'unica traccia di quali artefatti quella installazione ha creato e quali ha trovato già presenti.
Senza, `odoo-installer rollback` non avrebbe modo di distinguere le due cose e non potrebbe rimuovere
l'istanza. Non va cancellato a mano.

Viene rimosso solo a rollback completo: se qualcosa non è stato ripulito il file resta, e il comando
può essere rieseguito (gli `undo` sono idempotenti).

> **Nota sui file di stato precedenti a questa versione.** Lo stato ora porta con sé anche la
> configurazione dell'installazione (utente, database, directory — mai le password), perché è ciò che
> dice al rollback *quali* artefatti annullare. Un file scritto da una versione precedente non ce l'ha:
> il comando lo rileva, elenca gli step registrati e si ferma, invece di dedurre i nomi dai default e
> rischiare di rimuovere risorse che non aveva creato lui.

---

## Anteprima con `--dry-run`

`--dry-run` esegue solo gli snapshot (in sola lettura) e mostra il **piano** di ciò che verrebbe fatto,
distinguendo "agirebbe" da "no-op (già presente)". Non muta nulla e non persiste stato. Utile per
validare un `.env` o capire cosa succederà su una macchina.

**Con o senza `sudo`, ma non è la stessa cosa.** Gli snapshot *interrogano* il sistema, e alcuni lo
fanno passando da `sudo` (lo stato di PostgreSQL, i pacchetti installati). Senza privilegi quelle
domande non ottengono risposta: gli step interessati compaiono come «snapshot non disponibile» e il
piano, pur vero, è incompleto. L'installer lo dice prima di stamparlo. Per il piano completo:
`sudo odoo-installer --dry-run …`.

```bash
./target/release/odoo-installer --config configs/production.env --dry-run
```

---

## Verifica post-installazione

```bash
# Stato del servizio (N = versione short, es. 18)
systemctl status odoo18
journalctl -u odoo18 -n 50 --no-pager

# Comando helper locale (dopo: source ~/.bashrc)
odoo status        # start | stop | restart | status | dev

# Log dell'installer (post-mortem; sopravvive al rollback, apposta)
sudo cat /var/log/odoo-installer.log
```

---

## Sicurezza — note oneste

- **Password admin `admin`**: sconsigliata. In modalità interattiva richiede conferma esplicita; in
  non-interattiva con `admin_passwd=admin` l'installer **si ferma** (imposta una password diversa).
  La password non finisce mai nei log né nel riepilogo.
- **Checksum wkhtmltopdf (TOFU)**: l'installer verifica lo SHA-256 del `.deb` prima di installarlo.
  Upstream **non** pubblica checksum/firme per i `.deb`, quindi si usa un **pinning manuale TOFU**:
  i pin dei tre `.deb` della release `0.12.6.1-3` (`jammy`, `bullseye`, `bookworm`) sono **precaricati**
  in `default_checksums()`, generati scaricando i `.deb` via HTTPS dalla release ufficiale. Se il
  download non corrisponde al pin — o se un suffisso non ha pin — la verifica *fail-closed*
  rifiuta l'installazione: comportamento mai bypassato.
- **Comando `odoo` non globale**: installato solo per l'utente installatore (`~/.local/bin`), non in
  `/usr/local/bin`, per ridurre l'esposizione.
- **TLS non è configurato dall'installer, ed è deliberato.** Il vhost Nginx generato ascolta solo
  sulla porta 80. Per HTTPS si usa `certbot --nginx`, che ottiene i certificati e **riscrive il vhost
  da sé**, aggiungendo il blocco su 443 e il redirect da 80:

  ```bash
  sudo apt install certbot python3-certbot-nginx
  sudo certbot --nginx -d odoo.example.com
  ```

  `--open-https-port` apre soltanto la 443 sul firewall, in vista di quel passaggio. Si chiamava
  `--enable-ssl`, un nome che prometteva TLS senza fornirlo: il vecchio nome resta accettato.

### Rigenerare i pin checksum wkhtmltopdf

Serve solo quando si cambia la versione pinnata di wkhtmltopdf: i pin valgono per una sola release.

```bash
for cn in jammy bullseye bookworm; do
  url="https://github.com/wkhtmltopdf/packaging/releases/download/0.12.6.1-3/wkhtmltox_0.12.6.1-3.${cn}_amd64.deb"
  echo -n "$cn = "; curl -fsSL "$url" | sha256sum | cut -d' ' -f1
done
```

Inserisci i valori in `default_checksums()` (`src/steps/install_wkhtmltopdf.rs`). Sono pin **TOFU**, non
checksum upstream. Nota: la release `0.12.6.1-3` pubblica `.deb` amd64 **solo** per questi tre suffissi
(non esiste un `focal_amd64.deb`).

---

## Struttura del progetto (port Rust)

```
AutoInstallerOdoo/
├── Cargo.toml
├── src/
│   ├── main.rs              # entry point: install (parse → prompt → checks → lock → execute) | rollback
│   ├── cli.rs               # argomenti CLI (clap) + sottocomando `rollback`/`uninstall`
│   ├── rollback.rs          # rollback dallo stato persistito (reidratazione step + report residui)
│   ├── config.rs            # cascata CLI/.env/default + parser .env dichiarativo + validatori
│   ├── context.rs           # config risolta letta dagli step
│   ├── engine.rs            # motore: execute + rollback (ordine inverso) + dry-run plan
│   ├── step.rs              # trait Step (snapshot/run/undo)
│   ├── state.rs             # PreState + persistenza dello stato
│   ├── system_ops.rs        # confine sui comandi di sistema (mockabile nei test)
│   ├── secret.rs            # password redatta (mai nei log)
│   ├── checks.rs            # preflight non mutanti
│   ├── progress.rs          # ProgressReporter (indicatif/log/noop)
│   ├── prompt.rs            # input interattivo (inquire)
│   ├── logging.rs           # tracing TTY + file
│   ├── lockfile.rs          # lock anti-concorrenza (RAII)
│   └── steps/               # gli step reali (uno per file)
│       ├── prepare_opt_root.rs   create_odoo_user.rs   setup_log_dir.rs
│       ├── apt_packages.rs       install_wkhtmltopdf.rs
│       ├── setup_postgres.rs     create_db_role.rs     create_database.rs
│       ├── clone_odoo_repo.rs    create_virtualenv.rs  install_python_requirements.rs
│       ├── generate_config.rs    initialize_odoo_database.rs   setup_systemd.rs
│       ├── nginx_*.rs (install/write_config/enable_site/firewall/reload)
│       └── write_control_script.rs   patch_bashrc.rs
├── templates/              # odoo.conf.tpl · odoo.service.tpl · nginx.conf.tpl (embedded nel binario)
├── configs/               # esempi .env (dev.env, production.env)
└── tests/                 # test per-step + coordinamenti + rollback end-to-end
```

---

## Sviluppo / test

```bash
cargo build
cargo test          # gira senza root: il sistema è modellato con un mock
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

I test coprono ogni step (round-trip snapshot→run→undo), i coordinamenti fra step, e il **rollback
end-to-end** (fallimento iniettato → stato finale == iniziale; risorse preesistenti intatte). La CI
(`.github/workflows/test.yml`) esegue gli stessi controlli ad ogni push/PR.

### Test di integrazione reale

I test qui sopra girano su un **mock** del sistema: provano la logica, non l'integrazione con apt,
PostgreSQL e systemd veri. Quella la copre `.github/workflows/integration.yml`, che installa Odoo
davvero su runner e container effimeri e poi verifica che `odoo-installer rollback` riporti il sistema
pulito — pacchetto per pacchetto, confrontando il delta apt registrato nel file di stato.

Gira su richiesta (`workflow_dispatch`) e sui rami `main`/`dev`, non su ogni push: sono decine di
minuti per job. Copertura e limiti:

| Ambiente | Come | Cosa copre |
|---|---|---|
| Ubuntu 22.04 / 24.04 | runner nativi (VM con systemd) | ciclo di vita completo: installazione, servizio attivo, Odoo che risponde, disinstallazione, sistema pulito |
| Debian 12 / 11 | container | portabilità: nomi dei pacchetti apt, pin wkhtmltopdf per codename, e la pulizia. **Non** l'avvio del servizio: in un container systemd non è PID 1 |

Lo script (`scripts/ci/integration-test.sh`) è eseguibile anche a mano su una VM usa-e-getta:

```bash
MODE=full bash scripts/ci/integration-test.sh
```

> **È distruttivo**: crea utenti, installa pacchetti, tocca PostgreSQL e systemd. Solo su macchine
> sacrificabili.

---

## Storia

L'installer era originariamente scritto in **Bash**. Quelle versioni sono archiviate ai tag
[`v1.0.0`](../../releases/tag/v1.0.0) e [`v1.2.0`](../../releases/tag/v1.2.0). La versione corrente è un
**rewrite completo in Rust** con rollback transazionale: è l'unica presente nel repo.

---

## Licenza

MIT — vedi [LICENSE](LICENSE).

---

> ## [click -> Documentazione tecnica](https://github.com/Omisen/auto-installer-odoo/wiki)
