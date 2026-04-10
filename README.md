# Odoo Auto Installer

Installer bash non interattivo per **Odoo 16 / 17 / 18** su Ubuntu ≥ 22.04 e Debian ≥ 11.  
Gestisce dipendenze di sistema, PostgreSQL, virtualenv Python, servizio systemd e (opzionale) Nginx come reverse proxy.

---

## Requisiti

| Requisito | Dettaglio |
|-----------|-----------|
| OS | Ubuntu ≥ 22.04 **o** Debian ≥ 11 |
| Utente | `root` o accesso `sudo` |
| Disk | ≥ 5 GB liberi |
| Porte | 8069 (Odoo) libera; 80/443 se si usa Nginx |

---

## Installazione rapida

```bash
# 1. Clona il repository
git clone https://github.com/Omisen/auto-installer-odoo.git
cd auto-installer-odoo

# 2. Rendi eseguibile lo script
chmod +x installer.sh

# 3. Avvia l'installazione (come root o con sudo)
sudo ./installer.sh
```

L'installer usa i valori di default:

| Parametro | Default |
|-----------|---------|
| Versione Odoo | `18.0` |
| Utente OS | `odoo` |
| Porta HTTP | `8069` |
| Database | `odoo` |
| Nginx | disabilitato |

---

## Opzioni disponibili

```bash
sudo ./installer.sh [opzioni]

  --version VERSION     Versione Odoo (es. 17.0, 16.0)
  --odoo-user USER      Utente di sistema (default: odoo)
  --port PORT           Porta HTTP (default: 8069)
  --db-name NAME        Nome database (default: odoo)
  --with-nginx          Abilita Nginx come reverse proxy
  --config FILE         Carica variabili da file .env
  --help                Mostra l'aiuto
```

### Esempi

```bash
# Installazione con Nginx e versione 17
sudo ./installer.sh --version 17.0 --with-nginx

# Installazione da file di configurazione production
sudo ./installer.sh --config configs/production.env

# Installazione da file di configurazione dev
sudo ./installer.sh --config configs/dev.env
```

---

## File di configurazione `.env`

Puoi sovrascrivere qualsiasi variabile tramite un file `.env`:

```bash
# configs/production.env
ODOO_VERSION=18.0
ODOO_USER=odoo
ODOO_PORT=8069
DB_NAME=odoo_prod
WITH_NGINX=true
```

Passa il file con `--config configs/production.env`.

---

## Verifica post-installazione

```bash
# Controlla lo stato del servizio
systemctl status odoo18

# Controlla i log
journalctl -u odoo18 -n 50 --no-pager

# Esegui la suite di test non distruttivi
sudo bash tests/check_install.sh
```

---

## Struttura del progetto

```
AutoInstallerOdoo/
├── installer.sh          # Entry point
├── configs/
│   ├── dev.env           # Configurazione sviluppo
│   └── production.env    # Configurazione produzione
├── lib/
│   ├── checks.sh         # Controlli prerequisiti OS/porte/disco
│   ├── system.sh         # Dipendenze di sistema e wkhtmltopdf
│   ├── postgres.sh       # Setup PostgreSQL e utente DB
│   ├── odoo.sh           # Clone repo, virtualenv, dipendenze Python
│   ├── config.sh         # Generazione odoo.conf da template
│   ├── systemd.sh        # Unit file systemd (enable + start)
│   └── nginx.sh          # Configurazione Nginx (opzionale)
├── templates/
│   ├── odoo.conf.tpl     # Template configurazione Odoo
│   ├── odoo.service.tpl  # Template unit systemd
│   └── nginx.conf.tpl    # Template virtualhost Nginx
├── tests/
│   └── check_install.sh  # Suite di verifica post-installazione
└── docs/                 # Documentazione tecnica dei moduli
```

---

## Documentazione tecnica

| Modulo | Descrizione |
|--------|-------------|
| [checks.sh](./docs/check.md) | Controlli prerequisiti (root, OS, porte, disco) |
| [system.sh](./docs/system.md) | Dipendenze APT e wkhtmltopdf |
| [postgres.sh](./docs/postgres.md) | Setup PostgreSQL e utente DB |
| [odoo.sh](./docs/odoo.md) | Installazione Odoo e virtualenv |
| [config.sh](./docs/config.md) | Generazione configurazione da template |
| [systemd.sh](./docs/systemd.md) | Servizio systemd (unit, enable, start) |
| [nginx.sh](./docs/nginx.md) | Reverse proxy Nginx |
| [check_install.sh](./docs/check_install.md) | Suite di test post-installazione |
