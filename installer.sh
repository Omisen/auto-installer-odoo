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
DEFAULT_ODOO_VERSION="18.0"
DEFAULT_ODOO_USER="odoo"
DEFAULT_ODOO_PORT="8069"
DEFAULT_DB_NAME="odoo"
DEFAULT_ODOO_ADMIN_PASSWD="admin"
CONST_ODOO_HOME="/opt/odoo"

ODOO_VERSION="${DEFAULT_ODOO_VERSION}"
ODOO_USER="${DEFAULT_ODOO_USER}"
ODOO_HOME="${CONST_ODOO_HOME}"
ODOO_PORT="${DEFAULT_ODOO_PORT}"
DB_USER=""
DB_NAME="${DEFAULT_DB_NAME}"
ODOO_ADMIN_PASSWD="${DEFAULT_ODOO_ADMIN_PASSWD}"
WITH_NGINX=false
CONFIG_FILE=""

ARG_ODOO_VERSION=""
ARG_ODOO_USER=""
ARG_ODOO_PORT=""
ARG_DB_USER=""
ARG_DB_NAME=""
ARG_ODOO_INSTALL_DIR=""
ARG_ODOO_ADMIN_PASSWD=""
ARG_WITH_NGINX=""

CLI_ODOO_VERSION_SET=false
CLI_ODOO_USER_SET=false
CLI_ODOO_PORT_SET=false
CLI_DB_USER_SET=false
CLI_DB_NAME_SET=false
CLI_ODOO_INSTALL_DIR_SET=false
CLI_ODOO_ADMIN_PASSWD_SET=false

# Percorsi derivati (calcolati dopo parse_args per rispettare overrides da .env)
ODOO_VERSION_SHORT="${ODOO_VERSION%%.*}"
ODOO_INSTALL_DIR=""
ODOO_REPO_DIR="odoo"
ODOO_MODULES_DIR="repos/modules"
ODOO_VENV_DIR="sandbox"

# --- Colori per output -------------------------------------------------------
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log()    { echo -e "${GREEN}[INFO]${NC}  $*"; }
status() { echo -e "${BLUE}[STATUS]${NC} $*"; }
warn()   { echo -e "${YELLOW}[WARN]${NC}  $*"; }
error()  { echo -e "${RED}[ERROR]${NC} $*" >&2; exit 1; }

source "${LIB_DIR}/cli.sh"

# --- Parsing argomenti -------------------------------------------------------
usage() {
  cat <<EOF
Uso: $0 [opzioni]

Opzioni:
  --version VERSION     Versione Odoo da installare (16.0, 17.0, 18.0, 19.0)
  --odoo-user USER      Utente di sistema per Odoo (default: odoo)
  --db-user USER        Utente PostgreSQL (default: uguale a --odoo-user)
  --port PORT           Porta HTTP di Odoo (default: 8069)
  --db-name NAME        Nome del database (default: odoo)
  --install-dir DIR     Directory installazione (deve stare sotto /opt/odoo, default: /opt/odoo/odoo<versione>)
  --admin-passwd PASS   Password admin Odoo (default: admin)
  --with-nginx          Configura Nginx come reverse proxy
  --config FILE         Carica variabili da file .env
  --help                Mostra questo messaggio
EOF
  exit 0
}

parse_args() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --version)
        ARG_ODOO_VERSION="$2"
        CLI_ODOO_VERSION_SET=true
        shift 2
        ;;
      --odoo-user)
        ARG_ODOO_USER="$2"
        CLI_ODOO_USER_SET=true
        shift 2
        ;;
      --db-user)
        ARG_DB_USER="$2"
        CLI_DB_USER_SET=true
        shift 2
        ;;
      --port)
        ARG_ODOO_PORT="$2"
        CLI_ODOO_PORT_SET=true
        shift 2
        ;;
      --db-name)
        ARG_DB_NAME="$2"
        CLI_DB_NAME_SET=true
        shift 2
        ;;
      --install-dir)
        ARG_ODOO_INSTALL_DIR="$2"
        CLI_ODOO_INSTALL_DIR_SET=true
        shift 2
        ;;
      --admin-passwd)
        ARG_ODOO_ADMIN_PASSWD="$2"
        CLI_ODOO_ADMIN_PASSWD_SET=true
        shift 2
        ;;
      --with-nginx)
        ARG_WITH_NGINX=true
        shift
        ;;
      --config)
        CONFIG_FILE="$2"
        shift 2
        ;;
      --help)
        usage
        ;;
      *)
        error "Argomento sconosciuto: $1"
        ;;
    esac
  done

  if [[ -n "$CONFIG_FILE" ]]; then
    [[ -f "$CONFIG_FILE" ]] || error "Config file non trovato: $CONFIG_FILE"
    # shellcheck source=/dev/null
    source "$CONFIG_FILE"
    log "Configurazione caricata da $CONFIG_FILE"
  fi

  if [[ "$CLI_ODOO_VERSION_SET" == true ]]; then
    ODOO_VERSION="$ARG_ODOO_VERSION"
  fi
  if [[ "$CLI_ODOO_USER_SET" == true ]]; then
    ODOO_USER="$ARG_ODOO_USER"
  fi
  if [[ "$CLI_DB_USER_SET" == true ]]; then
    DB_USER="$ARG_DB_USER"
  fi
  if [[ "$CLI_ODOO_PORT_SET" == true ]]; then
    ODOO_PORT="$ARG_ODOO_PORT"
  fi
  if [[ "$CLI_DB_NAME_SET" == true ]]; then
    DB_NAME="$ARG_DB_NAME"
  fi
  if [[ "$CLI_ODOO_INSTALL_DIR_SET" == true ]]; then
    ODOO_INSTALL_DIR="$ARG_ODOO_INSTALL_DIR"
  fi
  if [[ "$CLI_ODOO_ADMIN_PASSWD_SET" == true ]]; then
    ODOO_ADMIN_PASSWD="$ARG_ODOO_ADMIN_PASSWD"
  fi
  if [[ "$ARG_WITH_NGINX" == true ]]; then
    WITH_NGINX=true
  fi

  # ODOO_HOME e' una costante architetturale: non consentiamo override.
  if [[ "${ODOO_HOME:-}" != "$CONST_ODOO_HOME" ]]; then
    warn "ODOO_HOME e' fisso a '${CONST_ODOO_HOME}': ignoro valore '${ODOO_HOME:-<vuoto>}'"
  fi
  ODOO_HOME="$CONST_ODOO_HOME"
}

# --- Export variabili globali (visibili ai moduli) ----------------------------
export_vars() {
  if [[ -z "${DB_USER:-}" ]]; then
    DB_USER="$ODOO_USER"
  fi
  if [[ -z "${ODOO_INSTALL_DIR:-}" ]]; then
    ODOO_INSTALL_DIR="$(build_default_install_dir "$ODOO_HOME" "$ODOO_VERSION")"
  fi

  ODOO_VERSION_SHORT="${ODOO_VERSION%%.*}"

  export ODOO_VERSION ODOO_VERSION_SHORT ODOO_USER ODOO_HOME ODOO_PORT
  export DB_USER DB_NAME DB_PASSWORD WITH_NGINX
  export ODOO_ADMIN_PASSWD
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
  source "${LIB_DIR}/control_script.sh"
  if [[ "$WITH_NGINX" == true ]]; then
    source "${LIB_DIR}/nginx.sh"
  fi
}

print_start_banner() {
  cat <<'EOF'

  /$$$$$$        /$$                           /$$$$$$                       /$$               /$$ /$$
 /$$__  $$      | $$                          |_  $$_/                      | $$              | $$| $$
| $$  \ $$  /$$$$$$$  /$$$$$$   /$$$$$$         | $$   /$$$$$$$   /$$$$$$$ /$$$$$$    /$$$$$$ | $$| $$  /$$$$$$   /$$$$$$
| $$  | $$ /$$__  $$ /$$__  $$ /$$__  $$        | $$  | $$__  $$ /$$_____/|_  $$_/   |____  $$| $$| $$ /$$__  $$ /$$__  $$
| $$  | $$| $$  | $$| $$  \ $$| $$  \ $$        | $$  | $$  \ $$|  $$$$$$   | $$      /$$$$$$$| $$| $$| $$$$$$$$| $$  \__/
| $$  | $$| $$  | $$| $$  | $$| $$  | $$        | $$  | $$  | $$ \____  $$  | $$ /$$ /$$__  $$| $$| $$| $$_____/| $$
|  $$$$$$/|  $$$$$$$|  $$$$$$/|  $$$$$$/       /$$$$$$| $$  | $$ /$$$$$$$/  |  $$$$/|  $$$$$$$| $$| $$|  $$$$$$$| $$
 \______/  \_______/ \______/  \______/       |______/|__/  |__/|_______/    \___/   \_______/|__/|__/ \_______/|__/

EOF
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

  if [[ -n "${ODOO_CONTROL_SCRIPT_PATH:-}" && -n "${ODOO_CONTROL_BIN_PATH:-}" ]]; then
    echo ""
    echo "==============================================================================="
    echo "  Attivazione comando locale (utente: ${ODOO_CONTROL_TARGET_USER:-n/a}):"
    echo "  Esegui ora nel terminale aperto: source ~/.bashrc"
    echo "================================================================================"
    echo "  Usa il comando 'odoo' per controllare il service quando vuoi."
    echo "  Esempi: odoo status | odoo start | odoo stop | odoo restart | odoo dev"
    echo "${YELLOW}__________________________________________________________________${NC}"
    echo "${YELLOW}  Se 'odoo' non funziona dopo source, verifica i due percorsi:    ${NC}"
    echo "  Control script : ${ODOO_CONTROL_SCRIPT_PATH}"
    echo "  Symlink comando: ${ODOO_CONTROL_BIN_PATH}"
    echo "${YELLOW}__________________________________________________________________${NC}"
  fi
}

# --- Main --------------------------------------------------------------------
main() {
  parse_args "$@"
  collect_inputs
  validate_selected_inputs
  export_vars
  load_modules

  echo ""
  print_start_banner
  echo ""
  sleep 3
  print_installation_configuration
  sleep 3
  log "Avvio installazione Odoo ${ODOO_VERSION}..."

  check_root
  check_sudo_user
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
  install_odoo_control_script

  print_summary
}

main "$@"