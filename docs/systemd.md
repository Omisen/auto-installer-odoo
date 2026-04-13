# lib/systemd.sh e templates/odoo.service.tpl

> Modulo responsabile della creazione, installazione e avvio del servizio systemd per Odoo. Renderizza il template dell'unit file, lo installa in `/etc/systemd/system/`, abilita il servizio all'avvio e lo avvia. Viene eseguito come ultimo step prima del riepilogo finale.

---

## Funzioni pubbliche

| Funzione | Descrizione |
|----------|-------------|
| `setup_systemd` | Orchestratore: renderizza il template, valida, installa l'unit file, abilita e avvia il servizio |
| `systemd_status` | Stampa lo stato corrente del servizio Odoo (chiamata da `print_summary` in `installer.sh`) |

---

## Funzioni interne (private)

| Funzione | Descrizione |
|----------|-------------|
| `_odoo_version_short` | Deriva il tag breve della versione (es. `18.0` → `18`) |
| `_unit_name` | Restituisce il nome dell'unit (es. `odoo18`) |
| `_unit_file` | Restituisce il path completo dell'unit file in `/etc/systemd/system/` |
| `_render_template` | Sostituisce i placeholder `{{...}}` nel template con `sed` |
| `_validate_template` | Verifica che non rimangano placeholder non sostituiti e che `ExecStart` esista |
| `_install_unit_file` | Copia l'unit renderizzato in `/etc/systemd/system/` e ricarica il daemon |
| `_enable_service` | Abilita il servizio all'avvio (idempotente) |
| `_start_service` | Avvia o riavvia il servizio e verifica che sia attivo dopo 3 secondi |

---

## Template `odoo.service.tpl`

Rispetto a un unit file base, sono stati aggiunti blocchi di hardening per la sicurezza e la stabilità:

| Direttiva | Motivazione |
|-----------|-------------|
| `NoNewPrivileges=true` | Impedisce a Odoo di acquisire privilegi extra via setuid/capabilities |
| `PrivateTmp=true` | Isola `/tmp` del processo — utile se Odoo scrive file temporanei |
| `LimitNOFILE` / `LimitNPROC` | Senza questi, su sistemi con molti worker Odoo esaurisce i file descriptor |
| `StartLimitIntervalSec` + `StartLimitBurst` | Evita restart loop infiniti in caso di errore strutturale all'avvio |
| `network-online.target` | Più conservativo di `network.target`: garantisce che le interfacce abbiano un IP prima che Odoo parta |
| `WorkingDirectory` | Imposta la directory di lavoro del processo al path di installazione |
| `RuntimeDirectory=odoo` | systemd crea `/var/run/odoo` ad ogni avvio con i permessi corretti, senza script esterni |

---

## Scelte architetturali

### `_render_template` via `sed` invece di `envsubst`

`envsubst` sostituisce **tutte** le variabili shell del file, rischiando collisioni con variabili di sistema (`$HOME`, `$USER`, ecc.). Con `sed` i placeholder `{{...}}` sono controllati esattamente e non confondibili con variabili shell.

### `_validate_template`: controllo a due livelli

1. Cerca placeholder rimasti (`{{...}}`) prima che il file finisca in `/etc` — errore **bloccante**.
2. Verifica l'esistenza del binario in `ExecStart` — solo un **warning**, perché `odoo.sh` potrebbe non aver ancora girato al momento del controllo.
3. Esegue `systemd-analyze verify` (se disponibile) per intercettare errori sintattici/semantici dell'unit prima dell'installazione.

### `_start_service` con `sleep 3` + check `is-active`

systemd può tornare subito da `start` mentre il processo sta ancora inizializzando. Il controllo posticipato dà un feedback reale all'utente invece di un falso positivo.

### `trap "rm -f '${tmp_unit}'" RETURN`

Il file temporaneo viene rimosso automaticamente anche se la funzione esce con errore, senza inquinare `/tmp`.

### `systemd_status` come funzione pubblica separata

`installer.sh` la chiama in `print_summary` per mostrare lo stato reale del servizio al termine dell'installazione, senza duplicare la logica di ispezione systemd.

---

## Integrazione in `installer.sh`

```bash
source "${LIB_DIR}/systemd.sh"

# Nel flusso principale:
setup_systemd

# In print_summary:
systemd_status
```
