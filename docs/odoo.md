# lib/odoo.sh

> Modulo che esegue l'installazione vera e propria di Odoo: clona il repository, crea il virtualenv Python, installa le dipendenze e verifica l'installazione. Tutte le operazioni vengono eseguite come utente `odoo`, mai come `root`.

---

## Funzioni pubbliche

| Funzione | Descrizione |
|----------|-------------|
| `install_odoo` | Orchestratore: crea le directory, clona il repo, crea il virtualenv e installa le dipendenze Python |

---

## Funzioni interne (private)

| Funzione | Descrizione |
|----------|-------------|
| `_create_directories` | Crea `ODOO_INSTALL_DIR`, `ODOO_MODULES_DIR` con owner `odoo` |
| `_clone_odoo_repo` | Clona il branch `ODOO_VERSION` da GitHub con retry e fallback |
| `_fetch_odoo_tarball_fallback` | Scarica i sorgenti come tarball se il clone Git fallisce |
| `_odoo_set_source_mode` | Traccia la provenienza dei sorgenti (`git`, `tarball`, ecc.) |
| `_create_virtualenv` | Crea il virtualenv in `ODOO_VENV_DIR` se non esiste già |
| `_install_python_requirements` | Installa i requirements Odoo nel virtualenv via `pip` |
| `_verify_installation` | Verifica l'installazione eseguendo `python3 -c "import odoo"` nel venv |

---

## Variabili richieste

Il modulo richiede che `installer.sh` esporti le seguenti variabili prima del `source`:

| Variabile | Esempio | Descrizione |
|-----------|---------|-------------|
| `ODOO_VERSION` | `18.0` | Branch Git da clonare |
| `ODOO_USER` | `odoo` | Utente di sistema |
| `ODOO_HOME` | `/opt/odoo` | Home dell'utente |
| `ODOO_INSTALL_DIR` | `/opt/odoo/odoo18` | Directory di installazione |
| `ODOO_REPO_DIR` | `odoo` | Sottocartella del clone (relativa a `ODOO_INSTALL_DIR`) |
| `ODOO_MODULES_DIR` | `repos/modules` | Addons extra (relativa a `ODOO_INSTALL_DIR`) |
| `ODOO_VENV_DIR` | `sandbox` | Virtualenv (relativo a `ODOO_INSTALL_DIR`) |
| `GIT_DEPTH` | `5` | Profondità del clone (opzionale, default 5) |
| `GIT_CLONE_RETRIES` | `3` | Numero tentativi clone Git prima del fallback |
| `TARBALL_DOWNLOAD_RETRIES` | `3` | Numero tentativi download tarball fallback |
| `ODOO_SOURCE_MODE` | `git` | Modalità sorgente usata (esposta nel riepilogo finale) |

Se si usano nomi diversi nei file `.env`, è sufficiente allinearli in `export_vars()` dentro `installer.sh`.

---

## Scelte progettuali

### Idempotenza in tre punti critici

**Clone** — controlla se `.git` esiste e se il branch corrisponde a `ODOO_VERSION`. In caso di mismatch blocca invece di sovrascrivere silenziosamente (comportamento più sicuro).

**Fallback rete** — se il clone fallisce per errori TLS/RPC intermittenti, il modulo scarica automaticamente il tarball GitHub del branch richiesto, mantenendo l'installazione operativa anche su reti instabili.

**Virtualenv** — controlla se `bin/python3` è già eseguibile e salta la creazione se positivo.

**pip** — è idempotente by design: aggiorna solo i pacchetti fuori versione senza reinstallare tutto.

### Sicurezza con `sudo -u odoo`

Ogni operazione su filesystem e Git viene eseguita come utente `odoo`, mai come `root`. Questo garantisce che tutti i file abbiano la proprietà corretta fin dalla creazione.

### Comando helper locale solo per l'utente installatore

Il comando helper `odoo` configurato a fine installazione e' intenzionalmente limitato all'utente che ha lanciato l'installer tramite `sudo`.

Non viene pubblicato in un path globale di sistema: il link viene predisposto nel profilo utente dell'account installatore, che deve ricaricare la propria shell per usarlo comodamente.

La motivazione e' di sicurezza operativa: evitare di esporre un wrapper di controllo servizio a utenti diversi da chi ha effettuato il setup o a sessioni non interattive che non dovrebbero ereditarne il comportamento.

Per tutti gli altri casi, l'interfaccia amministrativa supportata resta quella nativa di systemd (`systemctl start|stop|restart|status odoo<versione>`).

### Verifica post-installazione

`_verify_installation()` esegue `python3 -c "import odoo"` dentro il virtualenv: è il modo più affidabile per confermare che `pip` abbia installato tutto correttamente senza dover avviare il server completo.
