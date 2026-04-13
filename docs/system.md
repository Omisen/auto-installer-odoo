# lib/system.sh

> Modulo responsabile della preparazione dell'ambiente di sistema: installa le dipendenze APT richieste da Odoo, crea l'utente di sistema dedicato, configura (quando richiesto) la directory logfile e installa `wkhtmltopdf` nella versione corretta.

---

## Funzioni pubbliche

| Funzione | Descrizione |
|----------|-------------|
| `install_dependencies` | Installa tutti i pacchetti APT richiesti da Odoo |
| `install_wkhtmltopdf` | Installa `wkhtmltopdf 0.12.6.1` con Qt patch da GitHub releases |
| `create_odoo_user` | Crea l'utente di sistema `odoo` (system user, no login shell) |
| `setup_log_dir` | Crea e configura la directory del `logfile` solo se `ODOO_LOGFILE` è impostato |

---

## Funzioni interne (private)

| Funzione | Descrizione |
|----------|-------------|
| `_apt_packages_odoo` | Restituisce la lista canonica dei pacchetti APT richiesti |
| `_apt_progress_filter` | Filtra l'output rumoroso di `apt`, lasciando solo le righe significative |
| `_verify_odoo_user_homedir` | Verifica e corregge ownership e permessi della home di `odoo` |

---

## Struttura del modulo

### `_apt_packages_odoo()`

Lista canonica separata dalla logica di installazione. Vantaggi:
- Facile da aggiornare senza toccare la logica
- Testabile in isolamento (`_apt_packages_odoo | wc -l`)
- Permette override da `configs/dev.env` o `production.env` in futuro

### `_apt_progress_filter()`

Filtra l'output rumoroso di `apt`, lasciando passare solo le righe significative (`Get:`, `Unpacking`, `Setting up`, errori). Il log finale rimane leggibile anche durante installazioni lunghe.

---

## Idempotenza

| Funzione | Come è idempotente |
|----------|--------------------|
| `install_dependencies` | `apt-get install -y` è no-op se il pacchetto è già alla versione corretta |
| `create_odoo_user` | Controlla `id "${user}"` → skip con `warn` se l'utente esiste già |
| `_verify_odoo_user_homedir` | Controlla ownership e permessi, li corregge solo se necessario |
| `setup_log_dir` | Se `ODOO_LOGFILE` è vuoto fa skip; altrimenti crea/corregge solo la directory del logfile |
| `install_wkhtmltopdf` | Controlla la versione installata → skip se già `0.12.6.1` con Qt patch |

---

## Utente Odoo

```bash
useradd --system          # UID < 1000, non appare nella schermata di login
         --user-group     # crea un gruppo dedicato con il suo stesso nome
         --shell /bin/false   # nessuna shell interattiva (sicurezza)
         --home-dir "$ODOO_HOME"
```

La home viene creata con `chmod 750`:
- Il gruppo può leggere (utile per accesso SSH/SFTP con `chown -R odoo:$USER`)
- Gli altri utenti non hanno accesso

---

## wkhtmltopdf

Il pacchetto APT standard di Ubuntu (`0.12.6` **senza** Qt patch) genera PDF difettosi con Odoo: header/footer mancanti, caratteri errati.

`install_wkhtmltopdf` scarica la build `0.12.6.1-3` **con Qt patch** direttamente da [GitHub releases](https://github.com/wkhtmltopdf/packaging/releases) e risolve le dipendenze con `apt-get install -f`.

Mappa codename → pacchetto utilizzato:

| Ubuntu / Debian | Pacchetto scaricato |
|-----------------|---------------------|
| noble (24.04) | jammy (compatibile, nessun pacchetto nativo) |
| jammy (22.04) | jammy |
| focal (20.04) | focal |
| bookworm (Debian 12) | bookworm |
| bullseye (Debian 11) | bullseye |
