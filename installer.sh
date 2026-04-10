#!/usr/bin/env bash

#====================================/
# odoo-autoinstaller - Entry Point  /
#==================================/

set -euo pipefail

# ---- Percorsi Base ------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="${SCRIPT_DIR}/lib"
TEMPLATES_DIR="${SCRIPT_DIR}/templates"

# ------ Valori di default------
ODOO_VERSION="18.0"
ODOO_USER="odoo"
ODOO_HOME="/opt/odoo"
ODOO_PORT="8069"
DB_USER="odoo"
DB_NAME="odoo"
WITH_NGINX=false
CONFIG_FILE=""

# Percorsi derivati (calcolati dopo parse_args per rispettare overrides da .env)
ODOO_VERSION_SHORT="${ODOO_VERSION%%.*}"
ODOO_INSTALL_DIR="${ODOO_HOME}/odoo${ODOO_VERSION_SHORT}"
ODOO_REPO_DIR="odoo"
ODOO_MODULES_DIR="repos/modules"
ODOO_VENV_DIR="sandbox"

# --- Colori per output -------------------------------------------------------
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log()    { echo -e "${GREEN}[INFO]${NC}  $*"; }
warn()   { echo -e "${YELLOW}[WARN]${NC}  $*"; }
error()  { echo -e "${RED}[ERROR]${NC} $*" >&2; exit 1; }

# --- Parsing argomenti -------------------------------------------------------
usage() {
  cat <<EOF
Uso: $0 [opzioni]

Opzioni:
  --version VERSION     Versione Odoo da installare (default: 18.0)
  --odoo-user USER      Utente di sistema per Odoo (default: odoo)
  --port PORT           Porta HTTP di Odoo (default: 8069)
  --db-name NAME        Nome del database (default: odoo)
  --with-nginx          Configura Nginx come reverse proxy
  --config FILE         Carica variabili da file .env
  --help                Mostra questo messaggio
EOF
  exit 0
}

parse_args() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --version)    ODOO_VERSION="$2";  shift 2 ;;
      --odoo-user)  ODOO_USER="$2";     shift 2 ;;
      --port)       ODOO_PORT="$2";     shift 2 ;;
      --db-name)    DB_NAME="$2";       shift 2 ;;
      --with-nginx) WITH_NGINX=true;    shift   ;;
      --config)     CONFIG_FILE="$2";   shift 2 ;;
      --help)       usage ;;
      *)            error "Argomento sconosciuto: $1" ;;
    esac
  done

  # Carica .env se passato
  if [[ -n "$CONFIG_FILE" ]]; then
    [[ -f "$CONFIG_FILE" ]] || error "Config file non trovato: $CONFIG_FILE"
    # shellcheck source=/dev/null
    source "$CONFIG_FILE"
    log "Configurazione caricata da $CONFIG_FILE"
  fi
}

# --- Export variabili globali (visibili ai moduli) ----------------------------
export_vars() {
  # Ricalcola i percorsi derivati dopo eventuali override da .env / parse_args
  ODOO_VERSION_SHORT="${ODOO_VERSION%%.*}"
  ODOO_INSTALL_DIR="${ODOO_HOME}/odoo${ODOO_VERSION_SHORT}"

  export ODOO_VERSION ODOO_VERSION_SHORT ODOO_USER ODOO_HOME ODOO_PORT
  export DB_USER DB_NAME WITH_NGINX
  export ODOO_INSTALL_DIR ODOO_REPO_DIR ODOO_MODULES_DIR ODOO_VENV_DIR
  export TEMPLATES_DIR

  # Disabilita needrestart (Ubuntu 22.04+) per evitare prompt interattivi
  # durante apt-get install. 'a' = automatic restart senza chiedere.
  export NEEDRESTART_MODE=a
  export DEBIAN_FRONTEND=noninteractive
}

# --- Sourcing moduli ----------------------------------------------------------
load_modules() {
  source "${LIB_DIR}/checks.sh"
  source "${LIB_DIR}/system.sh"
  source "${LIB_DIR}/postgres.sh"
  source "${LIB_DIR}/odoo.sh"
  source "${LIB_DIR}/config.sh"
  source "${LIB_DIR}/systemd.sh"
  if [[ "$WITH_NGINX" == true ]]; then
    source "${LIB_DIR}/nginx.sh"
  fi
}

# --- Riepilogo finale ---------------------------------------------------------
print_summary() {
  echo ""
  echo -e "${GREEN}====================================================${NC}"
  echo -e "${GREEN}  Installazione completata con successo!${NC}"
  echo -e "${GREEN}====================================================${NC}"
  echo ""
  echo "  URL Odoo   : http://$(hostname -I | awk '{print $1}'):${ODOO_PORT}"
  echo "  Versione   : ${ODOO_VERSION}"
  echo "  Utente OS  : ${ODOO_USER}"
  echo "  Database   : ${DB_NAME}"
  if [[ "$WITH_NGINX" == true ]]; then
    echo "  Nginx      : attivo come reverse proxy"
  fi
  echo ""
  systemd_status 
}

# --- Main --------------------------------------------------------------------
main() {
  parse_args "$@"
  export_vars
  load_modules

  log "Avvio installazione Odoo ${ODOO_VERSION}..."

  check_root
  check_os
  check_disk
  check_commands
  bootstrap_prerequisites
  check_ports

  install_dependencies
  install_wkhtmltopdf
  create_odoo_user
  setup_log_dir
  setup_postgres
  create_db_user
  install_odoo
  generate_config
  setup_systemd
  if [[ "$WITH_NGINX" == true ]]; then
    setup_nginx
  fi

  print_summary
}

main "$@"