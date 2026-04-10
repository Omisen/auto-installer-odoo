# Cosa fa ogni funzione
- `check_root` — controlla `EUID -ne 0`, esce con suggerimento sudo se necessario.
- `check_os` — legge `/etc/os-release`, esporta `OS_ID`, `OS_VERSION`, `OS_CODENAME` e delega la verifica di versione a due helper privati (`_check_ubuntu_version` / `_check_debian_version`). Soglie: Ubuntu ≥ 22.04, Debian ≥ 11.
- `check_ports` — controlla la porta Odoo (`ODOO_PORT`) e, se `WITH_NGINX=true`, anche 80 e 443. Il rilevamento della porta usa `ss` → `netstat` → `lsof` come cascata di fallback.
- `check_disk` — misura il filesystem di `ODOO_HOME` con `df -Pk`, confronta con `MIN_DISK_GB` (default 5 GB). La soglia è configurabile dal file `.env`.
- `check_commands` — lista separata di comandi obbligatori (blocca) e opzionali (solo log informativo). `envsubst` è già incluso perché serve a `config.sh` per i template.

## Note di design

- ### Tutte le funzioni private iniziano con _ — chiaro che non devono essere chiamate da `install.sh`.
- ### Nessuna funzione sourca altri moduli, rispettando la convenzione del progetto.
- ### Le variabili `OS_*` vengono esportate così tutti i moduli successivi le trovano disponibili (utile ad es. in `system.sh` per scegliere il PPA corretto).
- ### `check_disk` crea la `target_dir` se non esiste — idempotente e non rompe `df`.