# tests/check_install.sh

> Suite di verifica post-installazione. Esegue controlli **non distruttivi** su un sistema già installato e riporta `PASS` / `FAIL` / `SKIP` per ogni test. Non modifica nulla — può essere rieseguita in qualsiasi momento in sicurezza.

---

## Uso

```bash
# Esecuzione base
sudo bash tests/check_install.sh

# Con config alternativa e output verboso
sudo bash tests/check_install.sh --config /opt/odoo/odoo18/odoo18.conf --verbose

# Override di variabili singole
sudo ODOO_PORT=8070 DB_USER=odoo_prod bash tests/check_install.sh
```

---

## Opzioni disponibili

| Opzione | Descrizione |
|---------|-------------|
| `--config FILE` | Percorso alternativo a `odoo.conf` |
| `--odoo-home DIR` | Override di `ODOO_HOME` |
| `--odoo-user USER` | Override di `ODOO_USER` |
| `--port PORT` | Override di `ODOO_PORT` |
| `--verbose` / `-v` | Mostra dettagli aggiuntivi per ogni test |

---

## Gruppi di test

| Gruppo | Cosa verifica |
|--------|---------------|
| 1 — Sistema | Utente root, OS Ubuntu/Debian, architettura, spazio disco (≥ 5 GB), RAM (≥ 1 GB) |
| 2 — Dipendenze apt | Tutti i pacchetti richiesti da Odoo 18, Python ≥ 3.10, Node.js (opzionale), wkhtmltopdf |
| 3 — Utente e directory | Utente `odoo` esiste, shell `/bin/false` (sicurezza), home, sottocartelle (`odoo/`, `repos/modules/`, `sandbox/`), proprietà, `/var/log/odoo` |
| 4 — PostgreSQL | Servizio attivo, versione ≥ 12, ruolo DB esistente, connessione locale come utente `odoo` |
| 5 — Odoo | `odoo-bin` presente, branch `18.0`, virtualenv, Python nella sandbox, librerie chiave (`psycopg2`, `Pillow`, `lxml`, `werkzeug`, …) |
| 6 — Config | Sezione `[options]` presente, chiavi obbligatorie, ogni path in `addons_path` esiste, log directory scrivibile, porta non privilegiata |
| 7 — Systemd | File service presente, sezioni `[Unit]`/`[Service]`/`[Install]`, `User=odoo`, dipendenza `postgresql.service`, `is-enabled` e `is-active`, policy `Restart=` |
| 8 — HTTP | Risposta su `/web/database/selector`, endpoint JSON-RPC |
| 9 — Nginx (opzionale) | Saltato se Nginx non è installato; altrimenti: servizio attivo, `nginx -t`, `proxy_pass` verso la porta Odoo |
| 10 — Sicurezza | No `sudo NOPASSWD` per `odoo`, `admin_passwd ≠ admin`, porta non esposta in UFW, permessi `640` su `odoo.conf`, proprietà `/var/log/odoo` |

---

## Exit codes

| Codice | Significato |
|--------|-------------|
| `0` | Tutti i test superati |
| `1` | Uno o più test falliti |

---

## Note di design

- I test del gruppo 9 (Nginx) vengono automaticamente saltati (`SKIP`) se Nginx non è installato — non producono `FAIL`.
- I controlli HTTP del gruppo 8 richiedono che il servizio Odoo sia attivo; in caso contrario i test vengono marcati `SKIP` anziché `FAIL` per evitare falsi negativi durante manutenzioni.
- La suite è idempotente e sicura: nessuna scrittura su disco, nessuna modifica a servizi o configurazioni.
