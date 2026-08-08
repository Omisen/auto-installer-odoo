#!/usr/bin/env bash
# Costruisce l'albero del sito che GitHub Pages serve come repository apt e dnf.
#
# Perché sta in un file e non dentro il workflow. La logica scritta in uno YAML è
# logica che nessuno può eseguire senza far partire tutto il sistema: si prova
# solo pubblicando una release, cioè nel momento in cui sbagliare costa di più.
# Stando qui è leggibile, `test.yml` ne verifica già la sintassi insieme agli
# altri `scripts/ci/*.sh`, e chi vuole può eseguirla a mano su due pacchetti
# scaricati. È la stessa ragione per cui esiste `journal.sh`.
#
# Questo script NON conosce GPG e NON conosce GitHub: prende dei pacchetti e
# produce un albero di file. La firma la mette il workflow, dopo, sui metadati
# che questo script genera. Confini separati si provano separatamente.
#
# ## Cosa viene servito, e cosa no
#
# I `.deb`/`.rpm` copiati qui sono gli STESSI BYTE degli asset della release:
# non vengono ricompilati, ricompressi né rifirmati. È la ragione per cui i
# file `.sha256` che il README pubblica continuano a dire il vero anche per chi
# installa dal repository invece che a mano — un pacchetto rifirmato al volo
# sarebbe un file diverso con lo stesso nome, cioè due hash per un artefatto
# solo, che è il modo in cui questo progetto si è già fatto male (A-V3-17).
#
# ## La catena di fiducia, per chi si chiede dove sia la firma
#
# La firma sta sui METADATI, non sui pacchetti, in entrambe le famiglie:
#   Release/InRelease firmato → SHA256 di Packages → SHA256 del .deb
#   repomd.xml.asc firmato    → SHA256 di primary.xml → SHA256 dell'.rpm
# Non è una verifica in meno: è la catena completa, ed è esattamente il modello
# di Debian, dove i `.deb` non sono mai firmati singolarmente.
#
# Uso:
#   build-package-repos.sh <dir-pacchetti> <dir-sito> <base-url> <fingerprint>
set -euo pipefail

PKG_DIR="${1:?serve la directory con i pacchetti (.deb e .rpm)}"
SITE_DIR="${2:?serve la directory di uscita del sito}"
BASE_URL="${3:?serve la base URL pubblica del sito, senza slash finale}"
FINGERPRINT="${4:?serve il fingerprint della chiave di firma}"

# Niente slash finale: tutte le URL qui sotto lo aggiungono da sé, e uno doppio
# in un `baseurl` di dnf non è un dettaglio estetico — cambia il percorso.
BASE_URL="${BASE_URL%/}"

ORIGIN="Omisen"
LABEL="invok"

# --- Prerequisiti ------------------------------------------------------------
#
# Si controllano tutti PRIMA di iniziare, e si nominano tutti insieme: scoprire
# il secondo strumento mancante dopo aver rifatto il giro è tempo buttato.
mancanti=""
for cmd in dpkg-scanpackages apt-ftparchive createrepo_c gzip; do
  command -v "$cmd" >/dev/null 2>&1 || mancanti="$mancanti $cmd"
done
if [ -n "$mancanti" ]; then
  printf 'errore: strumenti mancanti:%s\n' "$mancanti" >&2
  printf 'su Ubuntu: apt-get install -y dpkg-dev apt-utils createrepo-c\n' >&2
  exit 1
fi

# --- I pacchetti in ingresso -------------------------------------------------
#
# Esattamente uno per famiglia. Non è pignoleria: il repo dichiara di servire
# UNA versione, e con due `.deb` nella stessa directory `dpkg-scanpackages` li
# indicizza entrambi in silenzio — il repo servirebbe una versione che nessuno
# ha deciso di pubblicare. Fermarsi qui costa un messaggio; non fermarsi costa
# un `apt install` che prende il pacchetto sbagliato.
# Il controllo NON sta dentro una sostituzione di comando: lì un `exit` uscirebbe
# dalla sottoshell e non dallo script, e la guardia diventerebbe una guardia che
# non può fermare niente.
elenca() { find "$1" -maxdepth 1 -type f -name "$2" | sort; }

DEB="$(elenca "$PKG_DIR" '*.deb')"
RPM="$(elenca "$PKG_DIR" '*.rpm')"

# `if`, non `&&`: sotto `set -e` una lista `test ... && ...` che risulta falsa
# fa uscire lo script con stato 1 PRIMA di stampare il messaggio. Il messaggio è
# metà del valore di una guardia — senza, resta solo un exit code da indovinare.
for coppia in "deb:$DEB" "rpm:$RPM"; do
  famiglia="${coppia%%:*}"
  trovati="${coppia#*:}"
  if [ -z "$trovati" ]; then
    n=0
  else
    n="$(printf '%s\n' "$trovati" | wc -l)"
  fi
  if [ "$n" -ne 1 ]; then
    printf 'errore: atteso 1 pacchetto .%s in %s, trovati %s\n' "$famiglia" "$PKG_DIR" "$n" >&2
    if [ "$n" -gt 1 ]; then
      printf '%s\n' "$trovati" >&2
    fi
    exit 1
  fi
done

printf '→ .deb: %s\n' "$(basename "$DEB")"
printf '→ .rpm: %s\n' "$(basename "$RPM")"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

rm -rf "$SITE_DIR"
mkdir -p "$SITE_DIR/apt" "$SITE_DIR/rpm"

# --- Repository apt (formato *flat*) -----------------------------------------
#
# Flat, non `dists/`+`pool/`: con un pacchetto solo la struttura completa è
# macchinario senza scopo. Il prezzo è il `./` finale nella riga `sources.list`,
# che va scritto nel README perché sembra un refuso e non lo è.
cp "$DEB" "$SITE_DIR/apt/"

(
  cd "$SITE_DIR/apt"

  # `Filename:` finisce nell'indice come percorso RELATIVO alla base del repo,
  # quindi la scansione va fatta da dentro la directory: eseguita da fuori
  # produrrebbe percorsi che il client non sa risolvere.
  dpkg-scanpackages --multiversion . >"$TMP/Packages"
  mv "$TMP/Packages" Packages
  gzip -9c Packages >Packages.gz

  # `Release` elenca i checksum di TUTTI i file della directory, quindi non può
  # essere scritto dentro la directory mentre la si scandisce: la redirezione
  # della shell creerebbe il file prima che `apt-ftparchive` legga l'elenco, e
  # il risultato conterrebbe il checksum di sé stesso a metà scrittura. Si
  # genera fuori e si sposta dentro.
  apt-ftparchive \
    -o "APT::FTPArchive::Release::Origin=$ORIGIN" \
    -o "APT::FTPArchive::Release::Label=$LABEL" \
    -o "APT::FTPArchive::Release::Architectures=amd64" \
    release . >"$TMP/Release"
  mv "$TMP/Release" Release
)

# --- Repository rpm ----------------------------------------------------------
cp "$RPM" "$SITE_DIR/rpm/"
createrepo_c --quiet "$SITE_DIR/rpm"

# Il file `.repo` pronto: si scarica e si mette in /etc/yum.repos.d/.
#
# `repo_gpgcheck=1` + `gpgcheck=0` è deliberato e va letto insieme: verifichiamo
# la firma dei METADATI, e i metadati portano il checksum del pacchetto. Mettere
# `gpgcheck=1` richiederebbe di firmare l'`.rpm` con `rpm --addsign`, cioè di
# alterarlo dopo la release — e a quel punto il file servito qui non sarebbe più
# quello di cui il README pubblica lo sha256.
cat >"$SITE_DIR/rpm/invok.repo" <<EOF
[invok]
name=Invok — installer for Odoo (Omisen)
baseurl=$BASE_URL/rpm
enabled=1
# La firma è sui metadati del repository, non sui singoli pacchetti: i .rpm sono
# byte-identici a quelli allegati alla GitHub Release, e il loro checksum è
# dentro i metadati firmati.
repo_gpgcheck=1
gpgcheck=0
gpgkey=$BASE_URL/KEY.asc
metadata_expire=6h
EOF

# --- Pagina indice -----------------------------------------------------------
#
# Non è decorazione: è il posto in cui atterra chi apre la URL del repo trovata
# in un `sources.list`, e deve trovarci le istruzioni, il fingerprint con cui
# controllare la chiave, e il disclaimer di non affiliazione — che sta qui per
# la stessa ragione per cui sta nella descrizione dei pacchetti: chi installa da
# `apt` il README non lo apre.
VERSIONE="$(basename "$DEB" | sed -n 's/^invok_\([^_]*\)_.*$/\1/p')"

cat >"$SITE_DIR/index.html" <<EOF
<!doctype html>
<html lang="it">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>invok — repository apt / dnf</title>
<style>
  :root { color-scheme: light dark; }
  body { max-width: 46rem; margin: 3rem auto; padding: 0 1.2rem;
         font: 16px/1.6 system-ui, sans-serif; }
  pre { background: rgba(127,127,127,.14); padding: .9rem 1rem;
        border-radius: 6px; overflow-x: auto; }
  code { font-family: ui-monospace, monospace; }
  .nota { border-left: 3px solid rgba(127,127,127,.5); padding-left: 1rem; }
  footer { margin-top: 3rem; font-size: .9em; opacity: .8; }
</style>

<h1>invok — repository <code>apt</code> / <code>dnf</code></h1>

<p>Repository di pacchetti per <strong>invok</strong>, l'installer di Odoo con rollback
transazionale. Versione attualmente servita: <strong>$VERSIONE</strong>.</p>

<p class="nota">Il repository serve <strong>solo l'ultima versione</strong>. Le versioni storiche
restano scaricabili dalla
<a href="https://github.com/Omisen/invok/releases">pagina Releases</a>, che non viene
mai modificata.</p>

<h2>Ubuntu / Debian</h2>
<pre><code>sudo install -d -m 0755 /etc/apt/keyrings
sudo curl -fsSL -o /etc/apt/keyrings/invok.asc $BASE_URL/KEY.asc
echo "deb [signed-by=/etc/apt/keyrings/invok.asc] $BASE_URL/apt ./" \\
  | sudo tee /etc/apt/sources.list.d/invok.list
sudo apt update &amp;&amp; sudo apt install invok</code></pre>

<p>Il <code>./</code> finale non è un refuso: è la sintassi dei repository <em>flat</em>.</p>

<h2>Fedora</h2>
<pre><code>sudo curl -fsSL -o /etc/yum.repos.d/invok.repo $BASE_URL/rpm/invok.repo
sudo dnf install invok</code></pre>

<h2>Chiave di firma</h2>
<p><a href="KEY.asc"><code>KEY.asc</code></a> — fingerprint:</p>
<pre><code>$FINGERPRINT</code></pre>
<p>La firma copre i <strong>metadati</strong> del repository (<code>InRelease</code>,
<code>repomd.xml.asc</code>), che a loro volta contengono il checksum di ogni pacchetto: i
<code>.deb</code> e gli <code>.rpm</code> serviti qui sono byte-identici a quelli allegati alla
GitHub Release, e i loro <code>.sha256</code> pubblicati restano validi.</p>

<footer>
<p><strong>Progetto indipendente.</strong> Non è affiliato a Odoo S.A., né sostenuto o sponsorizzato
da essa. «Odoo» è un marchio di Odoo S.A. e qui è usato solo per indicare il software che questo
strumento installa. L'installer non redistribuisce codice Odoo: lo scarica a runtime dal repository
ufficiale <a href="https://github.com/odoo/odoo">odoo/odoo</a>, sulla macchina di destinazione.</p>
<p><a href="https://github.com/Omisen/invok">Codice sorgente</a> — MIT.</p>
</footer>
</html>
EOF

printf '✔ sito costruito in %s (versione %s)\n' "$SITE_DIR" "$VERSIONE"
