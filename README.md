# Odoo Auto Installer

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Installer bash con raccolta input guidata per **Odoo 16 / 17 / 18 / 19** su Ubuntu ≥ 22.04 e Debian ≥ 11.  
Gestisce dipendenze di sistema, PostgreSQL, virtualenv Python, servizio systemd e (opzionale) Nginx come reverse proxy.

---

## Requisiti

| Requisito | Dettaglio |
|-----------|-----------|
| OS | Ubuntu ≥ 22.04 **o** Debian ≥ 11 |
| Utente | utente normale con accesso `sudo` (non login diretto come root) |
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

# 3. Avvia l'installazione (utente normale con sudo)
sudo ./installer.sh
```

L'installer raccoglie i parametri principali con questa priorita:

1. argomento CLI
2. input interattivo
3. default finale

Premendo Invio su un prompt viene confermato subito il valore suggerito, che viene anche segnalato esplicitamente nel log.

I default iniziali sono:

| Parametro | Default |
|-----------|---------|
| Versione Odoo | `18.0` |
| Utente OS | `odoo` |
| Porta HTTP | `8069` |
| Database | `odoo` |
| ODOO_HOME (fisso) | `/opt/odoo` |
| Install dir | `/opt/odoo/odoo{Versione Odoo scelta}` |
| Nginx | disabilitato |

---

## Opzioni disponibili

```bash
sudo ./installer.sh [opzioni]

  --version VERSION     Versione Odoo (es. 17.0, 16.0)
  --odoo-user USER      Utente di sistema (default: odoo)
  --db-user USER        Utente PostgreSQL (default: uguale a --odoo-user)
  --port PORT           Porta HTTP (default: 8069)
  --db-name NAME        Nome database (default: odoo)
  --install-dir DIR     Directory installazione (solo sotto /opt/odoo, default: /opt/odoo/odoo<versione>)
  --admin-passwd PASS   Password admin Odoo (se `admin`, richiede conferma esplicita e il check finale fallisce)
  --with-nginx          Abilita Nginx come reverse proxy
  --config FILE         Carica variabili da file .env
  --help                Mostra l'aiuto
```

Se lasci `admin` come master password, l'installer chiede una conferma esplicita. Questa scelta resta consentita per demo o ambienti temporanei, ma la suite finale [docs/check_install.md](docs/check_install.md) la considera non release-ready.

### Esempi

```bash
# Installazione con Nginx e versione 17
sudo ./installer.sh --version 17.0 --with-nginx

# Installazione completamente parametrizzata da CLI
sudo ./installer.sh --version 19.0 --odoo-user odoo19 --db-name odoo19 --port 8079 --install-dir /opt/odoo/odoo19 --admin-passwd change-me

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

`DB_NAME` è obbligatorio: l'installer crea automaticamente il database PostgreSQL se non esiste già.

Per `ODOO_ADMIN_PASSWD`, il valore `admin` e' tollerato solo con conferma esplicita e va considerato adatto esclusivamente a demo o ambienti temporanei.

---

## Verifica post-installazione

```bash
# Controlla lo stato del servizio
systemctl status odoo18

# Controlla i log
journalctl -u odoo18 -n 50 --no-pager

# Nota: di default l'installer non forza un logfile su disco.
# I log vanno su journal/stdout; per avere un file log imposta ODOO_LOGFILE nel tuo .env.

# Esegui la suite di test non distruttivi
sudo bash tests/check_install.sh
```

## Comando helper locale `odoo`

Al termine dell'installazione, l'installer configura anche un comando helper locale `odoo` per `start`, `stop`, `restart`, `status` e `dev`.

Per scelta progettuale, questo comando **non** viene installato globalmente in `/usr/local/bin` o in un path condiviso di sistema: viene reso disponibile solo all'utente che ha eseguito l'installazione via `sudo`.

Questa limitazione e' intenzionale e serve a ridurre l'esposizione del comando su altri utenti del sistema o in contesti di automazione non previsti.

Dopo l'installazione, l'utente installatore puo' renderlo disponibile nella shell corrente con:

```bash
source ~/.bashrc
command -v odoo
```

Se vuoi usare il helper da un altro account, la procedura supportata resta l'uso diretto di `systemctl`.

---

## Struttura del progetto

```
AutoInstallerOdoo/
├── installer.sh          # Entry point
├── configs/
│   ├── dev.env           # Configurazione sviluppo
│   └── production.env    # Configurazione produzione
├── lib/
│   ├── cli.sh            # Prompt, validazione e normalizzazione input CLI
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

> ## [click -> Documentazione tecnica](https://github.com/Omisen/auto-installer-odoo/wiki)