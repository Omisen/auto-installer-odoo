[Unit]
Description=Odoo {{ODOO_VERSION}} - ERP & CRM
Documentation=https://www.odoo.com
Requires=postgresql.service
After=network.target network-online.target postgresql.service

[Service]
Type=simple
SyslogIdentifier=odoo{{ODOO_VERSION_SHORT}}

# Security hardening
User={{ODOO_USER}}
Group={{ODOO_USER}}
WorkingDirectory={{ODOO_INSTALL_DIR}}
PermissionsStartOnly=true
NoNewPrivileges=true
PrivateTmp=true
RuntimeDirectory=odoo
RuntimeDirectoryMode=0750

# Binary & config
ExecStart={{ODOO_INSTALL_DIR}}/{{ODOO_VENV_DIR}}/bin/python3 \
    {{ODOO_INSTALL_DIR}}/{{ODOO_REPO_DIR}}/odoo-bin \
    -c {{ODOO_INSTALL_DIR}}/odoo{{ODOO_VERSION_SHORT}}.conf

StandardOutput=journal+console
StandardError=journal+console

# Restart policy
Restart=on-failure
RestartSec=5s
StartLimitInterval=60s
StartLimitBurst=3

# Resource limits (tunable per environment)
LimitNOFILE=65536
LimitNPROC=65536

[Install]
WantedBy=multi-user.target
