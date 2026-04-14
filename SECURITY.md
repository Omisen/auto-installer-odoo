# Security Policy

## Versioni supportate

Questo progetto viene mantenuto su base rolling. Le correzioni di sicurezza vengono applicate al ramo predefinito del repository.

| Versione | Supportata |
| --- | --- |
| Branch predefinito corrente | Si |
| Snapshot o fork non allineati | No |
| Versioni modificate localmente | No |

## Ambito

Segnalazioni di sicurezza pertinenti per questo repository includono, ad esempio:

- gestione non sicura di credenziali o segreti nei file `.env`, template o log;
- permessi errati su file di configurazione, service file o directory create dall'installer;
- injection tramite input CLI o variabili di configurazione;
- configurazioni generate che espongono Odoo, PostgreSQL o Nginx in modo non previsto;
- uso non sicuro di `sudo`, systemd, shell expansion o file temporanei.

## Come segnalare una vulnerabilita

Non aprire issue pubbliche con dettagli sensibili, proof of concept o credenziali reali.

Se il repository ha il tab Security abilitato, usa `Report a vulnerability` su GitHub per inviare una segnalazione privata.

Se la reportistica privata non e disponibile, contatta il maintainer con un canale non pubblico e condividi solo le informazioni strettamente necessarie per la riproduzione.

## Cosa includere nella segnalazione

Per velocizzare l'analisi, includi:

- descrizione del problema e impatto atteso;
- passaggi minimi per riprodurlo;
- versione del sistema operativo e versione di Odoo installata;
- file o modulo coinvolto, se noto;
- eventuali mitigazioni temporanee gia verificate.

## Processo di gestione

L'obiettivo e:

1. confermare la ricezione della segnalazione;
2. riprodurre e classificare il problema;
3. preparare una correzione o mitigazione;
4. coordinare la disclosure una volta disponibile il fix.

I tempi effettivi dipendono dalla complessita del problema e dalla disponibilita del maintainer.

## Buone pratiche per chi usa l'installer

- usa password forti e non lasciare i default in ambienti di produzione;
- proteggi i file `.env` e i file generati contenenti segreti;
- esegui l'installer solo su host fidati e aggiornati;
- limita l'esposizione di porte e servizi con firewall e reverse proxy correttamente configurati;
- verifica sempre i permessi finali di configurazioni, log e unit systemd.