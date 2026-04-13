# lib/postgres.sh

> Modulo responsabile dell'installazione e configurazione di PostgreSQL, della creazione del ruolo database per Odoo e del database applicativo (se richiesto). Viene eseguito prima dell'installazione di Odoo, garantendo che il database sia pronto all'uso.

---

## Funzioni pubbliche

| Funzione | Descrizione |
|----------|-------------|
| `setup_postgres` | Installa PostgreSQL se non presente e si assicura che il servizio sia avviato |
| `create_db_user` | Crea il ruolo PostgreSQL per Odoo (idempotente) |
| `create_db_if_missing` | Crea `DB_NAME` se non esiste (idempotente); se `DB_NAME` è vuota non esegue azioni |

---

## Funzioni interne (private)

| Funzione | Descrizione |
|----------|-------------|
| `_postgres_is_installed` | Verifica se il pacchetto PostgreSQL è già installato |
| `_postgres_is_running` | Verifica se il servizio `postgresql` è attivo |
| `_postgres_role_exists` | Controlla se il ruolo `DB_USER` esiste già in `pg_roles` |
| `_postgres_db_exists` | Controlla se il database `DB_NAME` esiste già in `pg_database` |

---

## Scelte progettuali

### Idempotenza su `create_db_user`

Il controllo viene effettuato con una query diretta su `pg_roles` prima di qualsiasi `CREATE ROLE`. Se il ruolo esiste già, viene emesso un `warn` e la funzione ritorna `0` — nessun errore, nessuna azione distruttiva. È l'equivalente esplicito di `CREATE ROLE IF NOT EXISTS`, con il vantaggio di un messaggio chiaro all'utente.

### Sicurezza nella gestione della password

La password non viene mai interpolata in una stringa di shell passata a `bash -c` o simili. Viene passata direttamente a `psql -c` nel contesto SQL — un ambiente già sicuro.

Per evitare che la password compaia nella lista dei processi (`ps aux`) o nei log di PostgreSQL, viene trasmessa tramite variabile d'ambiente:

```bash
PGPASSWORD="${DB_PASSWORD}" psql ...
```

Per gestire password con apici singoli, è sufficiente l'escape SQL standard:

```bash
${DB_PASSWORD//\'/\'\'}
```

### Password opzionale → peer authentication

Se `DB_PASSWORD` è vuota o non impostata, il ruolo viene creato senza password. In installazioni locali dove l'utente OS `odoo` e il ruolo PG `odoo` coincidono, la **peer authentication** funziona senza modificare `pg_hba.conf` — che è il default su Debian/Ubuntu.

### Helper privati con prefisso `_`

`_postgres_is_installed`, `_postgres_is_running`, `_postgres_role_exists` sono funzioni interne. Il prefisso `_` indica chiaramente che solo `setup_postgres` e `create_db_user` costituiscono l'API pubblica chiamata da `installer.sh`.
