# Invok

> Installer for Odoo

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![CI](https://github.com/Omisen/invok/actions/workflows/test.yml/badge.svg)](https://github.com/Omisen/invok/actions/workflows/test.yml)
[![Integration](https://github.com/Omisen/invok/actions/workflows/integration.yml/badge.svg)](https://github.com/Omisen/invok/actions/workflows/integration.yml)
![Odoo 16–19](https://img.shields.io/badge/Odoo-16%20%7C%2017%20%7C%2018%20%7C%2019-875A7B)
![Ubuntu · Debian · Fedora](https://img.shields.io/badge/Ubuntu%20%C2%B7%20Debian%20%C2%B7%20Fedora-supportati-informational)

Installer per **Odoo 16 / 17 / 18 / 19** su Ubuntu ≥ 22.04, Debian ≥ 11 e Fedora ≥ 40, ora **in Rust** con
**rollback transazionale**: *o l'installazione riesce completamente, o il sistema torna esattamente
com'era prima.* Configura utente di sistema, dipendenze, PostgreSQL, sorgenti Odoo, virtualenv, config,
servizio systemd e (opzionale) Nginx.

**Il comando si chiama `invok`.** Il pacchetto installa anche l'alias breve **`vok`**, che è un
collegamento allo stesso programma: `vok --dry-run` e `invok --dry-run` fanno esattamente la stessa
cosa. Negli esempi qui sotto si usa la forma estesa; l'alias è sempre equivalente.

*Invok — da «invoke»: chiamare qualcosa perché prenda forma.*

> **Progetto indipendente.** Non è affiliato a Odoo S.A., né sostenuto o sponsorizzato da essa.
> «Odoo» è un marchio di Odoo S.A. e qui è usato solo per indicare il software che questo strumento
> installa. L'installer **non redistribuisce codice Odoo**: lo scarica a runtime dal repository
> ufficiale [`odoo/odoo`](https://github.com/odoo/odoo), sulla macchina di destinazione.

---

## 📑 Indice

**Se stai installando** — dal nulla a Odoo che risponde.

| | |
|---|---|
| [📋 Requisiti](#-requisiti) | OS, Python, privilegi, spazio, porte |
| [🚀 Installazione rapida](#-installazione-rapida) | cinque strade, [A](#opzione-a--qualsiasi-distro-binario-precompilato-consigliata-niente-rust) binario · [B](#opzione-b--ubuntu--debian-pacchetto-deb-esperienza-apt-nativa) `.deb` · [C](#opzione-c--fedora-pacchetto-rpm-esperienza-dnf-nativa) `.rpm` · [D](#opzione-d--repository-aptdnf-aggiornamenti-automatici) repository · [E](#opzione-e--qualsiasi-distro-build-da-sorgente) sorgente |
| [👀 Anteprima `--dry-run`](#-anteprima-con---dry-run) | cosa farebbe, senza toccare niente |
| [✅ Verifica post-installazione](#-verifica-post-installazione) | è partito? dove sono log e helper |

**Se stai configurando** — i due modi di passare i parametri.

| | |
|---|---|
| [⚙️ Opzioni CLI](#-opzioni-cli) | ogni flag, con il suo default |
| [📄 File `.env`](#-file-di-configurazione-env) | configurazione dichiarativa, mai eseguita come codice |
| [↳ Variabili d'ambiente](#variabili-dambiente-regolazioni-di-rete) | timeout di rete, soglia disco |

**Se vuoi sapere cosa garantisce** — la parte che distingue questo installer.

| | |
|---|---|
| [✨ Cosa lo distingue](#-cosa-lo-distingue) | in cinque righe |
| [🔍 Cosa fa, in breve](#-cosa-fa-in-breve) | i 25 step, in ordine |
| [↩️ Rollback](#-rollback) | cosa viene annullato e cosa no · [rilancio](#rilanciare-linstaller) · [Ctrl-C](#ctrl-c) |
| [🧹 Disinstallare](#-disinstallare--ripulire-invok-rollback) | `invok rollback`, e cosa resta di proposito |
| [🔒 Sicurezza — note oneste](#-sicurezza--note-oneste) | password, checksum, TLS · [Python](#python-linterprete-si-sceglie-non-si-subisce) · [Nginx per famiglia](#nginx-una-differenza-fra-le-famiglie-dichiarata) |

**Se lavori sul codice.**

| | |
|---|---|
| [🗂️ Struttura del progetto](#-struttura-del-progetto-port-rust) | dove sta cosa |
| [🧪 Sviluppo / test](#-sviluppo--test) | mock, e la [CI che installa davvero](#test-di-integrazione-reale) |
| [🤝 Contribuire](#-contribuire) | cosa far girare prima di una PR |
| [📜 Storia](#-storia) · [⚖️ Licenza](#-licenza) | l'era Bash · MIT |

---

## ✨ Cosa lo distingue

- **Rollback chirurgico verificato** — se un passo fallisce, gli step già eseguiti vengono annullati in
  ordine inverso; le risorse **preesistenti** del cliente non vengono toccate. È una proprietà provata
  con test end-to-end, non una promessa.
- **Binario unico, senza runtime** — un eseguibile nativo; git, il gestore di pacchetti, psql e venv
  restano comandi esterni.
- **Tre famiglie, nessun `if` sparso** — `apt` e `dnf` stanno dietro due confini, e gli step non sanno
  su quale distribuzione girano: ciò che diverge è dichiarato in un posto solo.
- **Riprendibile, e mai distruttivo con sé stesso** — un'installazione interrotta si riprende da dove
  si era fermata; una già completata non viene sovrascritta per sbaglio. Il file `.env` è **parsato in
  modo dichiarativo**, mai eseguito come codice.
- **Interrompibile** — Ctrl-C annulla e riporta il sistema com'era, invece di lasciarlo a metà.
- **Un solo flusso, due modi** — guidato (prompt interattivi) oppure non-interattivo
  (`--config`/flag/CI), con la stessa logica.

---

## 📋 Requisiti

| Requisito | Dettaglio |
|-----------|-----------|
| OS | **Ubuntu ≥ 22.04**, **Debian ≥ 11** o **Fedora ≥ 40** — provati in CI fino a Ubuntu 24.04, Debian 12 e **Fedora 44** (ciclo completo: installazione, servizio attivo, rollback); una release più recente viene accettata con un avviso, non rifiutata |
| Python | l'installer **sceglie l'interprete**: quello di sistema se i pin di Odoo lo coprono (fino a 3.13), altrimenti il più recente interprete impacchettato dalla distribuzione che lo sia. Su **Fedora ≥ 43**, dove il `python3` di sistema è 3.14, il virtualenv nasce su `python3.13` — installato per l'occasione e rimosso dal rollback. Se non esiste un'alternativa (Debian, Ubuntu) l'installazione prosegue lo stesso, con un avviso al preflight e, se pip non riesce a costruire, un errore che dice che è la versione di Python e non l'ambiente di build |
| Privilegi | utente normale con `sudo` (non login diretto come root) |
| Disk | ≥ 5 GB liberi (override `MIN_DISK_GB`) |
| Porte | 8069 (Odoo) libera; 80/443 se si usa Nginx — a meno che a tenerle non sia già Nginx, nel qual caso non è un conflitto |
| Installazione | **binario precompilato** dalle [release](../../releases/latest), oppure build da sorgente (toolchain Rust `cargo`) |

`ODOO_HOME` è **costante** `/opt/odoo` (non sovrascrivibile).

---

## 🚀 Installazione rapida

**Quale scegliere**, in una riga: se vuoi il comando `invok` nel `PATH` e rimovibile con il
gestore di pacchetti, prendi la confezione della tua famiglia (**B** per Ubuntu/Debian, **C** per
Fedora); se ti basta un binario da lanciare dove l'hai scaricato, **A** va su qualsiasi distro; se
gestisci più macchine e vuoi che gli aggiornamenti arrivino da soli con `apt upgrade` / `dnf upgrade`,
configura il **repository** (**D**). In tutti i casi è **lo stesso eseguibile** — musl statico, nessuna
dipendenza — e in tutti Odoo viene installato a runtime, quando lanci il comando.

### Opzione A — Qualsiasi distro: binario precompilato (consigliata, niente Rust)

Scarica il binario dalla pagina **[Releases](../../releases/latest)**. Due varianti Linux x86_64:

- `invok-x86_64-unknown-linux-musl.tar.gz` → **statico**, gira su **qualsiasi** distro (consigliato per i clienti);
- `invok-x86_64-unknown-linux-gnu.tar.gz` → dinamico, per sistemi con glibc recente.

Ogni archivio ha un file `.sha256` per **verificare l'integrità** del download.

> I comandi qui sotto puntano alla **v3.0.0**, che è la release descritta da questo README. Se ne è
> uscita una più recente, la trovi su [Releases](../../releases/latest): cambia il numero nelle due
> URL e nei nomi dei file.

```bash
curl -fsSL -O https://github.com/Omisen/invok/releases/download/v3.0.0/invok-x86_64-unknown-linux-musl.tar.gz
curl -fsSL -O https://github.com/Omisen/invok/releases/download/v3.0.0/invok-x86_64-unknown-linux-musl.tar.gz.sha256

sha256sum -c invok-x86_64-unknown-linux-musl.tar.gz.sha256   # deve dire: OK
tar xzf invok-x86_64-unknown-linux-musl.tar.gz

./invok -V                                          # quale versione è questa

sudo ./invok                                        # guidato (interattivo)
# oppure non-interattivo:
sudo ./invok --config production.env --with-nginx
```

> L'alias `vok` lo creano i **pacchetti** (opzioni B, C, D): qui hai un file solo, che puoi
> rinominare come preferisci.

### Opzione B — Ubuntu / Debian: pacchetto `.deb` (esperienza `apt` nativa)

Su Ubuntu (22.04, 24.04…) e Debian (11, 12, 13…) puoi installare l'installer come pacchetto: il
comando `invok` finisce nel `PATH` ed è rimovibile con `apt remove invok`. Il `.deb` è **statico**
(musl), quindi gira su qualsiasi distro. Deposita **solo** il binario CLI — non installa
servizi né tocca il sistema: Odoo viene installato a runtime quando lanci il comando.

```bash
curl -fsSL -O https://github.com/Omisen/invok/releases/download/v3.0.0/invok_3.0.0-1_amd64.deb
curl -fsSL -O https://github.com/Omisen/invok/releases/download/v3.0.0/invok_3.0.0-1_amd64.deb.sha256

sha256sum -c invok_3.0.0-1_amd64.deb.sha256         # deve dire: OK

sudo apt install ./invok_3.0.0-1_amd64.deb          # oppure: sudo dpkg -i ./invok_3.0.0-1_amd64.deb

invok -V                                            # quale versione è installata
dpkg -l invok                                       # oppure, chiedendolo ad apt

sudo invok                                          # ora è nel PATH
sudo vok                                            # alias breve: stesso programma
# oppure non-interattivo:
sudo invok --config production.env --with-nginx
```

Il pacchetto crea `/usr/bin/vok` come collegamento a `/usr/bin/invok`. Se sulla macchina esiste già un
`/usr/bin/vok` che **non** è un collegamento, l'alias non viene creato e l'installazione lo dice: un
file di qualcun altro non si sovrascrive.

### Opzione C — Fedora: pacchetto `.rpm` (esperienza `dnf` nativa)

Su Fedora (40, 41, …, 44) la stessa cosa con l'altra confezione. È **lo stesso binario** del `.deb`
e del `tar.gz` — musl statico, nessuna dipendenza — impacchettato per l'altro gestore.

```bash
curl -fsSL -O https://github.com/Omisen/invok/releases/download/v3.0.0/invok-3.0.0-1.x86_64.rpm
curl -fsSL -O https://github.com/Omisen/invok/releases/download/v3.0.0/invok-3.0.0-1.x86_64.rpm.sha256

sha256sum -c invok-3.0.0-1.x86_64.rpm.sha256        # deve dire: OK

sudo dnf install ./invok-3.0.0-1.x86_64.rpm

invok -V                                            # quale versione è installata
rpm -q invok                                        # oppure, chiedendolo a rpm

sudo invok                                          # ora è nel PATH
sudo vok                                            # alias breve: stesso programma
```

Rimovibile con `sudo dnf remove invok`. Come il `.deb`, deposita **solo** il binario CLI, più il
collegamento `vok`.

### Opzione D — Repository `apt`/`dnf` (aggiornamenti automatici)

Le opzioni B e C installano **un file**: per passare alla versione successiva bisogna accorgersi che è
uscita e rifare il download. Con il repository configurato, `invok` diventa un pacchetto come
gli altri e l'aggiornamento arriva con `apt upgrade` / `dnf upgrade`.

```bash
# --- Ubuntu / Debian
sudo install -d -m 0755 /etc/apt/keyrings
sudo curl -fsSL -o /etc/apt/keyrings/invok.asc https://omisen.github.io/invok/KEY.asc
echo "deb [signed-by=/etc/apt/keyrings/invok.asc] https://omisen.github.io/invok/apt ./" \
  | sudo tee /etc/apt/sources.list.d/invok.list

sudo apt update
sudo apt install invok
```

```bash
# --- Fedora
sudo curl -fsSL -o /etc/yum.repos.d/invok.repo https://omisen.github.io/invok/rpm/invok.repo
sudo dnf install invok
```

Tre cose dette per intero, perché nessuna delle tre è indovinabile:

- **Il repository serve solo l'ultima versione.** Non è una limitazione temporanea: è il contratto del
  canale. `apt`/`dnf` mostrano comunque solo la più recente, quindi nell'uso normale non cambia nulla;
  quello che non puoi fare è `apt install invok=3.0.0-1` per tornare indietro. Le versioni
  storiche restano tutte su [Releases](../../releases), che non viene mai modificata — è da lì che si
  fa un downgrade, con le opzioni B o C.
- **Il `./` finale nella riga `deb` non è un refuso.** È la sintassi dei repository *flat*, che è il
  formato usato qui: senza, `apt update` non trova nulla.
- **`repo_gpgcheck=1` con `gpgcheck=0` non è una verifica in meno.** La firma GPG copre i *metadati*
  del repository, e i metadati contengono lo SHA-256 di ogni pacchetto: la catena è completa. I
  pacchetti non sono firmati singolarmente perché così restano **byte-identici** a quelli allegati
  alla release — gli stessi di cui questo README pubblica lo `sha256`. È lo stesso modello di Debian,
  dove nessun `.deb` è firmato per conto suo.

La chiave pubblica è [`KEY.asc`](https://omisen.github.io/invok/KEY.asc); il fingerprint
con cui confrontarla è stampato sulla [pagina del repository](https://omisen.github.io/invok/).

### Opzione E — Qualsiasi distro: build da sorgente

```bash
# Toolchain Rust (una volta sola)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# Clona il repository
git clone https://github.com/Omisen/invok.git
cd invok

cargo build --release            # → target/release/invok

sudo ./target/release/invok
# oppure non-interattivo (esempi in configs/):
sudo ./target/release/invok --config configs/production.env --with-nginx
```

Va eseguito **via `sudo` da un utente normale** (l'utente `SUDO_USER` diventa proprietario del comando
helper `odoo`). La priorità di risoluzione dei parametri è: **CLI → `.env` → prompt interattivo →
default**.

---

## ⚙️ Opzioni CLI

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
| `-V` / `--installer-version` | stampa la versione **dell'installer** ed esce (`--version` è quella di Odoo) | — |
| `--help` | messaggio d'aiuto | — |

Sottocomandi: `invok rollback` (alias `uninstall`) — vedi
[Disinstallare / ripulire](#-disinstallare--ripulire-invok-rollback). Senza sottocomando il
comando installa, come sempre.

Esempi:

```bash
# Nginx + versione 17
sudo ./target/release/invok --version 17 --with-nginx

# Tutto da CLI (sostituisci i valori fra parentesi quadre)
sudo ./target/release/invok \
  --version 18 --odoo-user odoo --db-name odoo --port 8069 \
  --admin-passwd '[LA_TUA_PASSWORD_ADMIN]'

# Con Nginx su un dominio tuo
sudo ./target/release/invok --with-nginx --server-name '[il-tuo-dominio.example.com]'

# Anteprima (nessuna modifica; senza sudo il piano è più scarno, vedi sotto)
./target/release/invok --config configs/production.env --dry-run
```

---

## 📄 File di configurazione `.env`

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

## 🔍 Cosa fa, in breve

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
[wiki](https://github.com/Omisen/invok/wiki).

---

## ↩️ Rollback

Ogni step, prima di mutare, registra se ciò che sta per creare **esisteva già**. Se un passo fallisce,
gli step precedenti vengono annullati **in ordine inverso** (best-effort, idempotenti). La garanzia
chiave è sulle **risorse preesistenti**, che non vengono mai toccate da un rollback:

- un **database con lo stesso nome** già esistente **non** viene droppato (potrebbe avere dati reali);
- **PostgreSQL** già installato **resta** (di default stop/disable, mai purge senza flag);
- **`/opt/odoo`** già presente **resta**;
- il **`~/.bashrc`** dell'utente torna **byte-per-byte** com'era (solo la nostra riga viene rimossa).

### Rilanciare l'installer

L'installer **non sovrascrive mai** un'installazione già registrata. Al rilancio:

- se l'installazione precedente era **conclusa**, si ferma e ti dice cosa fare — `invok
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
- **esplicito (`invok rollback`)** — rilegge lo stato persistito in
  `/var/lib/invok/state.json` e annulla ciò che quell'installazione aveva creato. Serve sia a
  **disinstallare** un'istanza funzionante, sia a **ripulire** dopo un `kill -9` o uno spegnimento —
  i casi in cui il processo muore prima di poter fare il rollback da sé.

### Ctrl-C

Un **Ctrl-C** (o un `kill`/`systemctl stop`) non uccide più l'installer: l'installazione **si annulla
da sé** e il sistema torna come prima, senza che tu debba lanciare nulla.

L'interruzione ha effetto **fra uno step e il successivo**: lo step in corso viene portato a termine.
Non è una limitazione da nascondere — fermare a metà un `apt` lascerebbe `dpkg` inconsistente, e
troncare l'inizializzazione di un database lascerebbe qualcosa di peggio di ciò che si voleva evitare.
In pratica l'attesa è breve: il segnale arriva a tutto il gruppo di processi, quindi il comando in
corso (`apt`, `git`, `pip`) termina da sé.

Un **secondo Ctrl-C esce subito**, con codice 130. Lì il sistema resta a metà per tua scelta, e si
ripulisce con `sudo invok rollback`.

> **Da script, manda il segnale al solo installer.** «Due Ctrl-C» significa *due segnali ricevuti*.
> Un `sudo pkill -INT -f invok` colpisce **due** processi — il `sudo` e l'installer — e vale
> quindi come seconda pressione: uscita immediata, nessun annullamento. Usa invece
> `sudo pkill -INT -x invok` (`-x` = nome esatto del processo). Da terminale il problema non
> si pone.

Usa **`--dry-run`** per vedere il piano prima di eseguire davvero.

---

## 🧹 Disinstallare / ripulire: `invok rollback`

```bash
# Cosa verrebbe rimosso, senza toccare nulla
sudo ./target/release/invok rollback --dry-run

# Rimuovi davvero (chiede conferma)
sudo ./target/release/invok rollback
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
| `--state <FILE>` | file di stato da consumare | `/var/lib/invok/state.json` (ripiego sul percorso storico `/opt/odoo/.installer-state.json`) |
| `--dry-run` | elenca senza mutare (non serve `sudo`) | disattivo |
| `--aggressive-rollback` | purga anche PostgreSQL/Nginx installati da noi e le utility comuni | disattivo |
| `--yes` / `-y` | salta la conferma (obbligatorio senza terminale) | disattivo |

A installazione riuscita il file di stato **resta sul disco**: è il *manifesto di disinstallazione*,
l'unica traccia di quali artefatti quella installazione ha creato e quali ha trovato già presenti.
Senza, `invok rollback` non avrebbe modo di distinguere le due cose e non potrebbe rimuovere
l'istanza. Non va cancellato a mano.

Viene rimosso solo a rollback completo: se qualcosa non è stato ripulito il file resta, e il comando
può essere rieseguito (gli `undo` sono idempotenti).

> **Nota sui file di stato precedenti a questa versione.** Lo stato ora porta con sé anche la
> configurazione dell'installazione (utente, database, directory — mai le password), perché è ciò che
> dice al rollback *quali* artefatti annullare. Un file scritto da una versione precedente non ce l'ha:
> il comando lo rileva, elenca gli step registrati e si ferma, invece di dedurre i nomi dai default e
> rischiare di rimuovere risorse che non aveva creato lui.

---

## 👀 Anteprima con `--dry-run`

`--dry-run` esegue solo gli snapshot (in sola lettura) e mostra il **piano** di ciò che verrebbe fatto,
distinguendo "agirebbe" da "no-op (già presente)". Non muta nulla e non persiste stato. Utile per
validare un `.env` o capire cosa succederà su una macchina.

**Con o senza `sudo`, ma non è la stessa cosa.** Gli snapshot *interrogano* il sistema, e alcuni lo
fanno passando da `sudo` (lo stato di PostgreSQL, i pacchetti installati). Senza privilegi quelle
domande non ottengono risposta: gli step interessati compaiono come «snapshot non disponibile» e il
piano, pur vero, è incompleto. L'installer lo dice prima di stamparlo. Per il piano completo:
`sudo invok --dry-run …`.

```bash
./target/release/invok --config configs/production.env --dry-run
```

---

## ✅ Verifica post-installazione

```bash
# Stato del servizio (N = versione short, es. 18)
systemctl status odoo18
journalctl -u odoo18 -n 50 --no-pager

# Comando helper locale (dopo: source ~/.bashrc)
odoo status        # start | stop | restart | status | dev

# Log dell'installer (post-mortem; sopravvive al rollback, apposta)
sudo cat /var/log/invok.log
```

---

## 🔒 Sicurezza — note oneste

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

### Python: l'interprete si sceglie, non si subisce

Odoo pinna `gevent` e `greenlet` **per versione di Python**. Su un interprete più recente dei suoi pin
non esiste una wheel già compilata: pip prova a costruire dai sorgenti e il C generato non regge gli
header di un CPython più nuovo — non è un problema di compilatore né di pacchetti mancanti, ed è il caso
di **Fedora ≥ 43**, dove il `python3` di sistema è 3.14.

La regola: *il Python di sistema se i pin di Odoo lo coprono, altrimenti il più recente interprete
**impacchettato dalla distribuzione** che lo sia.* Su Fedora 44 il venv nasce quindi su `python3.13`,
installato insieme alle altre dipendenze e **rimosso dal rollback** come qualunque pacchetto del delta.

Su Ubuntu e Debian non cambia nulla: nei repository base c'è un solo Python ed è dentro i pin. Se un
domani non lo fosse, l'installer **non rifiuta** — avvisa al preflight, e se il build salta dice che è
la versione di Python invece di lasciare parlare trecento righe di `gcc`. Un rifiuto sarebbe una soglia
cablata, e una soglia cablata invecchia bloccando il caso buono il giorno in cui Odoo alza il pin.

Quello che sta a valle non se ne accorge: unit systemd, control-script e init del database passano da
`<install_dir>/sandbox/bin/python3`, che il venv crea con quel nome qualunque sia l'interprete di base.

### Nginx: una differenza fra le famiglie, dichiarata

Le due famiglie organizzano nginx in modo diverso, e una delle differenze **si vede**:

| | Ubuntu / Debian | Fedora |
|---|---|---|
| vhost | `/etc/nginx/sites-available/odooN` | `/etc/nginx/conf.d/odooN.conf` |
| come si abilita | symlink in `sites-enabled/` | scrivere il file **è** abilitarlo |
| firewall | `ufw` | `firewalld` |
| server di default | file `sites-enabled/default`, che l'installer **sposta** per liberare la porta 80 (e rimette al suo posto al rollback) | blocco `server` dentro `nginx.conf`: **non lo tocchiamo** |

L'ultima riga è la differenza che conta. Su Fedora il server di default non è un file
separato: toglierlo significherebbe riscrivere `/etc/nginx/nginx.conf`, cioè la configurazione
principale di un servizio che sulla macchina potrebbe servire altro. L'installer non lo fa.

**Conseguenza pratica**: su una Fedora con nginx appena installato, una richiesta a un hostname
che non combacia con `--server-name` continua a ricevere la pagina di benvenuto di nginx invece di
Odoo. Se `--server-name` è il catch-all `_` (il default) la cosa non si nota; se hai impostato un
dominio e vuoi che *qualunque* hostname arrivi a Odoo, togli o modifica quel blocco a mano.

È la scelta più prudente delle due: modificare la configurazione principale di nginx è
esattamente la classe di mutazione che questo installer tratta con la massima cautela.

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

## 🗂️ Struttura del progetto (port Rust)

```
invok/
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
│   ├── interrupt.rs         # Ctrl-C/SIGTERM: alza un flag, il motore lo osserva
│   └── steps/               # gli step reali (uno per file)
│       ├── prepare_opt_root.rs   create_odoo_user.rs   setup_log_dir.rs
│       ├── setup_cache_dir.rs    setup_data_dir.rs
│       ├── apt_packages.rs       install_wkhtmltopdf.rs
│       ├── setup_postgres.rs     create_db_role.rs     create_database.rs
│       ├── clone_odoo_repo.rs    create_virtualenv.rs  install_python_requirements.rs
│       ├── generate_config.rs    initialize_odoo_database.rs   setup_systemd.rs
│       ├── nginx_*.rs (install/write_config/enable_site/firewall/reload)
│       └── write_control_script.rs   patch_bashrc.rs
├── templates/              # odoo.conf.tpl · odoo.service.tpl · nginx.conf.tpl (embedded nel binario)
├── configs/               # esempi .env (dev, production) + i preset della CI (ci, ci-nginx)
├── debian/                # postinst/postrm del .deb: creano e rimuovono l'alias `vok`
├── scripts/ci/            # integration-test.sh: installazione reale + verifica della pulizia
├── .github/workflows/     # test.yml (rapido, mock) · integration.yml (reale) · release.yml
└── tests/                 # test per-step + coordinamenti + rollback end-to-end
```

I **percorsi che l'installer usa per sé** — manifesto, lock, log — vivono fuori da `/opt/odoo`, che è
il perimetro che il rollback deve poter rimuovere per intero:

| File | Percorso | A cosa serve |
|---|---|---|
| Manifesto | `/var/lib/invok/state.json` | dice cosa è stato creato e cosa era già presente: è ciò che rende disinstallabile un'istanza |
| Lock | `/run/invok.lock` | impedisce due installazioni simultanee; sparisce al reboot |
| Log | `/var/log/invok.log` | post-mortem; sopravvive al rollback, di proposito |

---

## 🧪 Sviluppo / test

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

I test qui sopra girano su un **mock** del sistema: provano la logica, non l'integrazione con il
gestore di pacchetti,
PostgreSQL e systemd veri. Quella la copre `.github/workflows/integration.yml`, che installa Odoo
davvero su runner e container effimeri e poi verifica che `invok rollback` riporti il sistema
pulito — pacchetto per pacchetto, confrontando il delta registrato nel file di stato.

Gira su richiesta (`workflow_dispatch`) e sui rami `main`/`dev`, non su ogni push: sono decine di
minuti per job. Copertura e limiti:

| Scenario | Come | Cosa copre |
|---|---|---|
| Ubuntu 22.04 / 24.04 | runner nativi (VM con systemd) | ciclo di vita completo: installazione, servizio attivo, Odoo che risponde, seconda installazione rifiutata, disinstallazione, sistema pulito |
| Debian 12 / 11 | container | portabilità: nomi dei pacchetti apt, pin wkhtmltopdf per codename, e la pulizia. **Non** l'avvio del servizio: in un container systemd non è PID 1 |
| Fedora 41 | container **privilegiato con systemd come PID 1** | ciclo di vita completo sull'altra famiglia: `dnf`, i nomi dei pacchetti rpm, l'**inizializzazione del cluster** PostgreSQL (che su Fedora il pacchetto non fa), il `.rpm` di wkhtmltopdf, e la disinstallazione. Serve PID 1 perché senza si fermerebbe *prima* dell'init del cluster — cioè prima della divergenza più rischiosa |
| Fedora 41 + Nginx | container privilegiato, `firewalld` attivo | l'unico posto dove **firewalld** e **SELinux** girano davvero: la porta aperta col delta, il booleano `httpd_can_network_connect` acceso e rispento |
| Fedora 44 | container privilegiato | l'altro ramo della scelta dell'interprete: il `python3` di sistema è **3.14**, fuori dai pin di Odoo, quindi il venv nasce su `python3.13` — installato per l'occasione e rimosso dal rollback |
| Con Nginx | runner nativo, `ufw` attivo | i sei step Nginx: vhost, `nginx -t`, la porta 80 che serve Odoo, le regole firewall. In matrice sulle **due nature** del default site (symlink e file regolare), perché il rollback deve ripristinare entrambe |
| Utente `odoo` preesistente | runner nativo | la home consegnata a un utente che c'è già, e il rifiuto esplicito quando `/opt/odoo` preesiste di root. Al rollback l'utente **sopravvive**: non era nostro |
| Ctrl-C reale | runner nativo | un `SIGINT` mandato a metà installazione: l'installer deve annullare da sé e lasciare il sistema pulito |

Lo script (`scripts/ci/integration-test.sh`) è eseguibile anche a mano su una VM usa-e-getta:

```bash
MODE=full bash scripts/ci/integration-test.sh
```

> **È distruttivo**: crea utenti, installa pacchetti, tocca PostgreSQL e systemd. Solo su macchine
> sacrificabili.

---

## 🤝 Contribuire

Contributi benvenuti. Prima di aprire una PR:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test                       # senza root: il sistema è mockato
```

Tre regole che questo progetto prende sul serio, e che rendono una PR accettabile in fretta:

1. **Ogni mutazione è reversibile.** Uno step nuovo porta `snapshot`/`run`/`undo` e un `PreState`: se
   crea qualcosa che il rollback non sa rimuovere, non è finito. Il trait `Step` non va modificato per
   aggiungere uno step.
2. **Niente `.unwrap()`/`.expect()` nel codice di produzione**: ogni fallimento è un `Result` con un
   messaggio che dice *cosa* e *dove*.
3. **Un test che non può fallire non è un test.** Verifica che il tuo caso rosso diventi davvero rosso
   (cambia il codice e guardalo fallire) prima di proporlo.

Se la modifica tocca il comportamento su una distribuzione, dillo nella PR: la
[CI di integrazione](#test-di-integrazione-reale) installa davvero su Ubuntu, Debian e Fedora, ed è lì
che si vede.

Il dettaglio tecnico — motore, step uno per uno, modello di rollback, supporto multi-distribuzione — sta
nella **[wiki](https://github.com/Omisen/invok/wiki)**.

---

## 📜 Storia

L'installer era originariamente scritto in **Bash**. Quelle versioni sono archiviate ai tag
[`v1.0.0`](../../releases/tag/v1.0.0) e [`v1.2.0`](../../releases/tag/v1.2.0). La versione corrente è un
**rewrite completo in Rust** con rollback transazionale: è l'unica presente nel repo.

---

## ⚖️ Licenza

MIT — vedi [LICENSE](LICENSE).

Il `.tar.gz`, il `.deb` e l'`.rpm` pubblicati contengono **solo** il binario `invok` e questo
README: nessun codice di terze parti viene ridistribuito. Odoo e wkhtmltopdf (entrambi LGPLv3) sono
scaricati a runtime dalle rispettive fonti ufficiali e restano soggetti alle loro licenze.

Questo progetto è indipendente e non è affiliato a Odoo S.A. né sostenuto da essa; «Odoo» è un marchio
di Odoo S.A.

---

> ## [click -> Documentazione tecnica](https://github.com/Omisen/invok/wiki)
