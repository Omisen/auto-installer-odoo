# lib/config.sh e templates/odoo.conf.tpl

> Modulo responsabile della **generazione del file di configurazione** `odoo.conf` a partire da un template. Viene chiamato dopo l'installazione di Odoo e prima del setup systemd, in modo che il servizio trovi già il file pronto.

---

## Funzioni pubbliche

| Funzione | Descrizione |
|----------|-------------|
| `generate_config` | Orchestratore: imposta i default, renderizza il template, valida l'output e salva `odoo.conf` con i permessi corretti |

---

## Funzioni interne (private)

| Funzione | Descrizione |
|----------|-------------|
| `_config_set_defaults` | Imposta i valori di default per le variabili di configurazione non ancora definite |
| `_config_render_template` | Renderizza `odoo.conf.tpl` sostituendo i placeholder con `envsubst` |
| `_config_validate_conf` | Verifica che nessun placeholder `${...}` sia rimasto non sostituito |

---

## Template `odoo.conf.tpl`

Il template usa placeholder nella forma `${VAR}` compatibili con `envsubst`. Sono incluse tutte le sezioni significative per un'installazione reale:

| Sezione | Note |
|---------|------|
| `admin_passwd` | Sovrascrivibile da `.env`; il valore `admin` richiede conferma esplicita durante il setup ed e' accettabile solo per demo o ambienti temporanei |
| `db_*` | Se `DB_HOST`, `DB_PORT` o `DB_PASSWORD` sono vuoti, nel file generato la direttiva viene commentata in forma standard (`; db_port =`) per evitare valori invalidi |
| `http_interface` + `proxy_mode` | Pronti per Nginx reverse proxy (`proxy_mode = True`) |
| `workers` / `max_cron_threads` | `0` = modalità thread (dev); da aumentare in produzione |
| `limit_*` | Valori consigliati da Odoo upstream per produzione |
| `log_level` | `info` di default; sovrascrivibile con `debug` in dev |

Nota logging:
Di default `ODOO_LOGFILE` è vuoto, quindi Odoo logga su stdout/stderr (visibile via `journalctl` nel servizio systemd).
Se vuoi un file log su disco, imposta `ODOO_LOGFILE` nel tuo `.env`.

---

## Decisioni chiave

### Default con override da `.env`

`_config_set_defaults()` usa la sintassi `: "${VAR:=default}"` — assegna il valore solo se la variabile non è già impostata. Questo permette ai file `configs/dev.env` e `configs/production.env` di fare override senza modificare il modulo.

Nel caso di `ODOO_ADMIN_PASSWD`, il fallback resta disponibile per compatibilita' con demo rapide, ma il valore `admin` viene trattato come scelta esplicitamente confermata e non supera la verifica finale di release.

### Rendering selettivo con `envsubst`

`_config_render_template()` passa a `envsubst` la lista **esatta** delle variabili presenti nel template (estratta con `grep -oE`), invece di fare un `envsubst` globale. Questo evita di espandere accidentalmente variabili di shell come `$HOME`, `$PATH` o `$USER` che potrebbero trovarsi in un contesto esterno.

### Validazione post-rendering

`_config_validate_conf()` cerca eventuali `${QUALCOSA}` rimasti nel file dopo il rendering — sintomo di una variabile non esportata. Fallisce in modo esplicito invece di lasciare un `.conf` silenziosamente incompleto.

Inoltre, durante il rendering, quando `DB_PORT` è vuoto la riga `db_port` viene commentata automaticamente (`; db_port =`) per evitare l'errore di startup Odoo `option db_port: invalid integer value: ''`.

### Idempotenza con backup automatico

Se `odoo18.conf` esiste già, viene creato un backup con timestamp prima di sovrascrivere:

```
odoo18.conf.bak.20260410143022
```

È sicuro rieseguire `installer.sh` senza rischio di perdere configurazioni precedenti.

### Permessi

Il file `.conf` viene scritto con:

```bash
chmod 640   # lettura solo da odoo e root
chown odoo:odoo
```

La password del database non è leggibile da altri utenti di sistema.
