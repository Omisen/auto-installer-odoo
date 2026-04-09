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
| `_create_install_dirs` | Crea `ODOO_INSTALL_DIR`, `ODOO_MODULES_DIR` con owner `odoo` |
| `_clone_odoo` | Clona il branch `ODOO_VERSION` da GitHub con `--depth` configurabile |
| `_create_virtualenv` | Crea il virtualenv in `ODOO_VENV_DIR` se non esiste già |
| `_install_python_deps` | Installa i requirements Odoo nel virtualenv via `pip` |
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

Se si usano nomi diversi nei file `.env`, è sufficiente allinearli in `export_vars()` dentro `installer.sh`.

---

## Scelte progettuali

### Idempotenza in tre punti critici

**Clone** — controlla se `.git` esiste e se il branch corrisponde a `ODOO_VERSION`. In caso di mismatch blocca invece di sovrascrivere silenziosamente (comportamento più sicuro).

**Virtualenv** — controlla se `bin/python3` è già eseguibile e salta la creazione se positivo.

**pip** — è idempotente by design: aggiorna solo i pacchetti fuori versione senza reinstallare tutto.

### Sicurezza con `sudo -u odoo`

Ogni operazione su filesystem e Git viene eseguita come utente `odoo`, mai come `root`. Questo garantisce che tutti i file abbiano la proprietà corretta fin dalla creazione.

### Verifica post-installazione

`_verify_installation()` esegue `python3 -c "import odoo"` dentro il virtualenv: è il modo più affidabile per confermare che `pip` abbia installato tutto correttamente senza dover avviare il server completo.
