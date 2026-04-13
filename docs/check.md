# lib/checks.sh

> Modulo di verifica dei prerequisiti. Viene eseguito come **primo blocco** in `main()` di `installer.sh`, prima di qualsiasi modifica al sistema. Se uno dei controlli fallisce, l'installer termina con un messaggio esplicativo.

---

## Funzioni pubbliche

Queste sono le uniche funzioni chiamate direttamente da `installer.sh`:

| Funzione | Descrizione |
|----------|-------------|
| `check_root` | Verifica che lo script giri con privilegi elevati (EUID = 0), normalmente tramite `sudo` |
| `check_os` | Verifica che il sistema operativo sia Ubuntu ≥ 22.04 o Debian ≥ 11 |
| `check_ports` | Verifica che le porte necessarie siano libere (Odoo + Nginx se abilitato) |
| `check_disk` | Verifica che ci sia almeno `MIN_DISK_GB` (default 5 GB) di spazio libero |
| `check_commands` | Verifica la presenza dei comandi di sistema richiesti |

---

## Funzioni interne (private)

Prefissate con `_`, non devono essere chiamate da `installer.sh`:

| Funzione | Descrizione |
|----------|-------------|
| `_check_ubuntu_version` | Confronta la versione Ubuntu con la soglia minima (22.04) |
| `_check_debian_version` | Confronta la versione Debian con la soglia minima (11) |

---

## Dettaglio funzioni

### `check_os`

Legge `/etc/os-release` ed esporta tre variabili globali:

```bash
OS_ID        # es. "ubuntu" o "debian"
OS_VERSION   # es. "22.04" o "11"
OS_CODENAME  # es. "jammy" o "bullseye"
```

Le variabili `OS_*` vengono esportate così che i moduli successivi (es. `system.sh`) possano usarle per scegliere il PPA o il pacchetto corretto senza rileggere il file.

### `check_ports`

Controlla `ODOO_PORT` e, se `WITH_NGINX=true`, anche le porte `80` e `443`.  
Il rilevamento usa una cascata di fallback per massima compatibilità:

```
ss  →  netstat  →  lsof
```

### `check_disk`

Misura il filesystem di `ODOO_HOME` con `df -Pk` e confronta con `MIN_DISK_GB`.  
La soglia è sovrascrivibile dal file `.env`:

```bash
MIN_DISK_GB=10  # in configs/production.env
```

Se la directory non esiste ancora, viene creata temporaneamente per non rompere `df` — comportamento idempotente.

### `check_commands`

Mantiene due liste distinte:
- **Obbligatori** — blocca l'installazione se mancano (es. `git`, `python3`, `psql`)
- **Opzionali** — emette solo un log informativo (es. `node`, `npm`)

`envsubst` è incluso tra gli obbligatori perché richiesto da `config.sh` per il rendering dei template.

---

## Note di design

- Tutte le funzioni private iniziano con `_` — chiaro segnale che non devono essere chiamate da `installer.sh`.
- Nessuna funzione effettua `source` di altri moduli, rispettando la convenzione del progetto.
- Il modulo non modifica nulla sul sistema — è puramente read-only.