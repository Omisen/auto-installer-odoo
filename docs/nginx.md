# lib/nginx.sh e templates/nginx.conf.tpl

> Modulo **opzionale** per la configurazione di Nginx come reverse proxy davanti a Odoo. Viene caricato e chiamato solo se `WITH_NGINX=true`. Se il flag è assente o falso, il modulo non produce alcun effetto.

---

## Funzioni pubbliche

| Funzione | Descrizione |
|----------|-------------|
| `setup_nginx` | Orchestratore unico: installa Nginx, renderizza il template, abilita il sito, valida la config e ricarica il servizio |

---

## Funzioni interne (private)

| Funzione | Descrizione |
|----------|-------------|
| `_nginx_install` | Installa il pacchetto `nginx` se non già presente |
| `_nginx_render_template` | Renderizza `nginx.conf.tpl` con i valori delle variabili globali |
| `_nginx_enable_site` | Crea il symlink in `sites-enabled/` (usa `ln -sf` per idempotenza) |
| `_nginx_validate` | Esegue `nginx -t` e blocca se la configurazione è malformata |
| `_nginx_reload` | Esegue `reload` se Nginx è attivo, `start` se è fermo |
| `_nginx_open_firewall` | Apre le porte 80 e 443 in UFW (non bloccante se UFW non è presente) |

---

## Idempotenza

| Funzione | Come è idempotente |
|----------|--------------------|
| `_nginx_install` | Controlla `command -v nginx` prima di installare |
| `_nginx_enable_site` | Usa `ln -sf` — sovrascrive il symlink senza errori |
| `_nginx_reload` | Distingue tra `reload` (se attivo) e `start` (se fermo), senza downtime |

---

## Gestione errori

- `set -euo pipefail` ereditato dal caller — qualsiasi comando fallisce blocca l'esecuzione.
- `_nginx_validate` esegue `nginx -t` e termina l'installer se la config è malformata.
- `_nginx_open_firewall` è **non bloccante**: se `ufw` manca o non è attivo emette solo un `warn()` e prosegue senza errori.

---

## Template `nginx.conf.tpl`

Il template copre tutti i casi reali di un deploy Odoo in produzione:

| Sezione | Perché è necessaria |
|---------|---------------------|
| `upstream odoo` con `keepalive` | Riusa le connessioni TCP verso Odoo invece di riaprirle a ogni request |
| `upstream odoo-longpolling` (porta 8072) | Il bus messaggi di Odoo 18 usa una porta separata |
| `/web/websocket` con `Upgrade` | Odoo 18 usa WebSocket per le notifiche real-time |
| `/web/static/` con `Cache-Control: immutable` | Gli asset hanno hash nel nome: si possono cachare in modo aggressivo |
| Timeout 720s sul proxy principale | Import massivi e generazione PDF possono richiedere diversi minuti |
| Blocco `*.py` / `*.cfg` / `*.conf` | Impedisce l'accesso diretto ai file sorgente e di configurazione |
| Blocco HTTPS commentato | Pronto all'uso: basta decommentare e fornire i certificati (o usare Certbot) |

---

## Integrazione in `installer.sh`

```bash
# Il modulo viene caricato solo se necessario
[[ "$WITH_NGINX" == true ]] && source "${LIB_DIR}/nginx.sh"

# Nel flusso principale:
[[ "$WITH_NGINX" == true ]] && setup_nginx
```
