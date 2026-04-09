#!/usr/bin/env bash

#====================================/
# odoo-autoinstaller - Entry Point  /
#==================================/

set -euo pipefail

# ---- Percorsi Base ------
SCRIPT_DIR="$(cd"$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB_DIR="${SCRIPT_DIR}/lib"
TEMPLATES_DIR="${SCRIPT_DIR}/templates"

# ------ Valori di default------
ODOO_VERSION="18.0"
ODOO_USER="odoo"
ODOO_HOME="/home/odoo"
ODOO_PORT="8069"
DB_USER="odoo"
DB_NAME="odoo"
WITH_NGINX=false
CONFIG_FILE=""