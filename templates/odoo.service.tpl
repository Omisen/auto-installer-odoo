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
PermissionsStartOnly=true
NoNewPrivileges=true
PrivateTmp=true

# Binary & config
ExecStart={{ODOO_HOME}}/odoo{{ODOO_VERSION_SHORT}}/sandbox/bin/python3 \
    {{ODOO_HOME}}/odoo{{ODOO_VERSION_SHORT}}/odoo/odoo-bin \
    -c {{ODOO_HOME}}/odoo{{ODOO_VERSION_SHORT}}/odoo{{ODOO_VERSION_SHORT}}.conf

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
