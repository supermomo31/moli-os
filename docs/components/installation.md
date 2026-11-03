# Installer et paramétrer Moli chez soi

Moli tourne sur n'importe quelle machine Linux allumée en permanence (mini-PC, NAS, serveur, conteneur LXC Proxmox), soit **dans Docker**, soit **en service système (`systemd`)**. Home Assistant n'est pas nécessaire.

---

## 1. Installation de Moli OS

### Option A — Installation avec Docker Compose (recommandé)

```sh
git clone https://github.com/rudy-io/moli-os && cd moli-os
mkdir -p data && sudo chown 1000:1000 data    # le conteneur tourne en 1000:1000
docker compose up -d --build
docker compose logs moli-os                   # chercher « code d'installation »
```

### Option B — Installation native / LXC (`systemd`)

Sur un conteneur LXC Debian/Ubuntu ou une machine dédiée :

1. **Compiler le binaire** (depuis une machine de build ou sur place) :
   ```sh
   cd web && npm ci && npm run build && cd ..
   cargo build --release -p moli-os
   sudo install -m 0755 target/release/moli-os /usr/local/bin/moli-os
   ```
2. **Créer l'utilisateur et le dossier de données `/data`** :
   ```sh
   sudo useradd --system --home-dir /data --shell /usr/sbin/nologin moli || true
   sudo mkdir -p /data
   sudo chown -R moli:moli /data
   ```
3. **Créer l'unité `/etc/systemd/system/moli-os.service`** :
   ```ini
   [Unit]
   Description=Moli OS Home Automation
   After=network-online.target
   Wants=network-online.target

   [Service]
   Type=simple
   User=moli
   Group=moli
   WorkingDirectory=/data
   Environment=MOLI_CONFIG=/data/moli.toml
   ExecStart=/usr/local/bin/moli-os serve
   Restart=always
   RestartSec=2

   [Install]
   WantedBy=multi-user.target
   ```
4. **Activer et démarrer le service** :
   ```sh
   sudo systemctl daemon-reload
   sudo systemctl enable --now moli-os
   sudo journalctl -u moli-os -n 50 --no-pager   # chercher « code d'installation »
   ```

### Premier démarrage et code de la maison

Au premier démarrage, Moli :
- écrit une configuration de départ `data/moli.toml` ;
- génère sa **clé maître** `data/master.key` (64 caractères hexadécimaux), qui chiffre le coffre `data/secrets.enc` ;
- affiche dans ses journaux un **code d'installation** (`ABCD-EFGH`), valable une heure.

Ouvrir `http://<adresse de la machine>:8790`, saisir le code d'installation, puis choisir le **code de la maison** (6 à 8 chiffres). Ce code sert ensuite à valider les automatismes et les actions dans les pièces protégées.

> **Important : mettre `data/master.key` à l'abri.**
> Les sauvegardes (`moli-os backup`) ne contiennent **jamais** `master.key`. Sans cette clé, `secrets.enc` est indéchiffrable. En conserver une copie dans un gestionnaire de mots de passe.

---

## 2. Gestion du coffre des secrets (`secrets.enc`)

Aucun mot de passe, jeton ou clé API ne doit jamais figurer dans `moli.toml`. Tous les secrets des pilotes et de l'assistant vivent dans le coffre chiffré `data/secrets.enc` et sont identifiés par `<instance>/<nom>`.

### Commandes en mode Docker

- **Lister les secrets présents** (affiche uniquement `<instance>/<nom>`, jamais les valeurs) :
  ```sh
  docker compose exec -T moli-os /moli-os secrets list
  ```
- **Enregistrer ou modifier un secret** (toujours par un tube, jamais en argument) :
  ```sh
  IFS= read -rs -p "Valeur : " V; printf %s "$V" | docker compose exec -T moli-os /moli-os secrets set <instance> <nom>; unset V; echo
  docker compose restart moli-os
  ```
- **Supprimer un secret** :
  ```sh
  docker compose exec -T moli-os /moli-os secrets forget <instance> <nom>
  docker compose restart moli-os
  ```

### Commandes en mode `systemd` (natif / LXC)

- **Lister les secrets présents** :
  ```sh
  cd /data && MOLI_MASTER_KEY=$(cat /data/master.key) /usr/local/bin/moli-os secrets list
  ```
- **Enregistrer ou modifier un secret** :
  ```sh
  IFS= read -rs -p "Valeur : " V; printf %s "$V" | sh -c 'cd /data && export MOLI_MASTER_KEY=$(cat /data/master.key) && /usr/local/bin/moli-os secrets set <instance> <nom> && chown moli:moli /data/secrets.enc'; unset V; echo
  sudo systemctl restart moli-os
  ```
- **Supprimer un secret** :
  ```sh
  cd /data && MOLI_MASTER_KEY=$(cat /data/master.key) /usr/local/bin/moli-os secrets forget <instance> <nom>
  sudo chown moli:moli /data/secrets.enc && sudo systemctl restart moli-os
  ```

### Secrets globaux (hors pilotes)

| Instance | Nom du secret | Rôle |
|---|---|---|
| `assistant` | `api_key` | Clé API du modèle compatible OpenAI (conversation Moli, transcription STT et voix TTS cloud) |
| `core` | `ui_pin` | Empreinte du code PIN de la maison (gérée par l'interface ; `secrets forget core ui_pin` + `ui_sessions` réinitialise le code) |
| `core` | `ui_sessions` | Sessions actives du tableau de bord |

---

## 3. Paramétrage des 32 intégrations et de leurs secrets

Chaque intégration se déclare par un bloc `[[driver]]` dans `data/moli.toml` (vérifiable avec `moli-os check-config`), et possède un guide détaillé dans `integrations/<id>/onboarding.md`.

### 3.1 Audio, musique et voix

| Intégration (`kind`) | Exemple `moli.toml` | Secrets dans `secrets.enc` (`<instance>/<nom>`) | Notes |
|---|---|---|---|
| **Google Cast** (`cast`) | `id = "cast_salon"`<br>`kind = "cast"`<br>`[driver.options]`<br>`host = "192.168.1.x"`<br>`name = "Salon"`<br>`media_base = "http://192.168.1.y:8790"` | `cast_device_pin` *(auto au 1er contact : identité Cast DeviceAuth durable)* | Expose `playing`, `volume`, `mute`, `title`, `artist`, `image`, `next`, `previous`, `say`, `announce`, `play`. |
| **Sonos** (`sonos`) | `id = "sonos_salon"`<br>`kind = "sonos"`<br>`[driver.options]`<br>`host = "192.168.1.x"` | `ws_cert` *(auto : empreinte TLS pour les annonces)* | Expose la lecture, `next`/`previous`, la pochette `image`, les annonces et le point `play`. |
| **Qobuz** (`qobuz`) | `id = "qobuz"`<br>`kind = "qobuz"`<br>`[driver.options]`<br>`format_id = 6` *(FLAC 16/44.1)*<br>`queue = 5` | `app_id`, `app_secret`, et soit `user_auth_token` *(en-tête `X-User-Auth-Token` du player web)*, soit `email` + `password` | Service musical partagé pour le point `play` des enceintes `cast` et `sonos`. |
| **Amazon Echo / Alexa** (`alexa`) | `id = "alexa"`<br>`kind = "alexa"`<br>`[driver.options]`<br>`domain = "amazon.fr"`<br>`music = "QOBUZ"` | `refresh_token`, `device_serial` *(créés depuis Système → Alexa en collant l'URL `maplanding`)* | Pilote les enceintes Echo et groupes multi-pièces du compte Amazon. |
| **Satellite vocal ESPHome** (`esphome`) | `id = "sat-cuisine"`<br>`kind = "esphome"`<br>`[driver.options]`<br>`host = "192.168.1.x"`<br>`media_base = "http://192.168.1.y:8790"` | `api_key` *(32 octets en base64, générée et poussée au 1er contact ou reprise d'ESPHome)* | Micro wake-word (`hey_jarvis`…), VAD, STT/TTS avec Moli. |

### 3.2 Lumières, prises, volets et chauffage

| Intégration (`kind`) | Exemple `moli.toml` | Secrets dans `secrets.enc` (`<instance>/<nom>`) | Notes |
|---|---|---|---|
| **Zigbee2MQTT & Matter-over-MQTT** (`z2m`) | `id = "z2m"`<br>`kind = "z2m"`<br>`[driver.options]`<br>`host = "192.168.1.x"`<br>`port = 1883`<br>`base_topic = "zigbee2mqtt"` | Mot de passe MQTT éventuel via `password_env` | Sert aussi pour un pont Matter-to-MQTT (second bloc `kind = "z2m"` avec `base_topic = "mattermqtt"`). |
| **Philips Hue** (`hue`) | `id = "hue"`<br>`kind = "hue"`<br>`[driver.options]`<br>`host = "192.168.1.x"` | `cert_sha256`, `app_key`, `client_key` *(enregistrés automatiquement après appui sur le bouton du pont)* | API locale CLIP v2 + flux d'événements SSE. |
| **Tuya Wi-Fi** (`tuya`) | `id = "tuya"`<br>`kind = "tuya"`<br>`[driver.options]`<br>`devices_file = "/data/tuya-devices.json"` | Local : `key-<device_id>` *(ou import `moli-os tuya import`)*<br>Cloud : `cloud_access_id`, `cloud_access_secret` *(ou `/run/secrets/tuya-cloud`)* | Prises et appareils en local (TCP 6668) et/ou capteurs par le cloud Tuya. |
| **TP-Link Tapo** (`tapo`) | `id = "tapo_lampe"`<br>`kind = "tapo"`<br>`[driver.options]`<br>`host = "192.168.1.x"` | `auth_hash` *(ou `username_env` + `password_env`)*, `klap_seed` | Protocole local KLAP v2 (ampoules L530, prises P110…). |
| **tado°** (`tado`) | `id = "tado"`<br>`kind = "tado"` | `refresh_token` *(obtenu par OAuth2 Device Code au 1er démarrage : URL affichée dans Système)* | Thermostats, têtes thermostatiques et climatisation tado°. |
| **Somfy TaHoma / Overkiz** (`tahoma`) | `id = "tahoma"`<br>`kind = "tahoma"`<br>`[driver.options]`<br>`server = "ha101-1.overkiz.com"` | `username`, `password` *(identifiants Somfy)* | Volets roulants, stores, BSO (TaHoma, Connectivity Kit). |
| **Daikin BRP069** (`profile`) | `id = "clim_salon"`<br>`kind = "profile"`<br>`[driver.options]`<br>`profile = "daikin-brp069"`<br>`host = "192.168.1.x"` | Aucun | Profil déclaratif HTTP local pour climatiseurs Daikin. |

### 3.3 Caméras, téléviseur et imprimantes

| Intégration (`kind`) | Exemple `moli.toml` | Secrets dans `secrets.enc` (`<instance>/<nom>`) | Notes |
|---|---|---|---|
| **Caméras Netatmo** (`netatmo`) | `id = "netatmo"`<br>`kind = "netatmo"` | `client_id`, `client_secret`, `refresh_token` *(app sur `dev.netatmo.com`, scopes `read_camera read_presence access_camera access_presence`)* | Welcome (`NACamera`), Presence (`NOC`), Doorbell (`NDB`), Indoor Advance (`NPC`). `refresh_token` tourné automatiquement. |
| **Reolink Home Hub** (`reolink`) | `id = "reolink"`<br>`kind = "reolink"`<br>`[driver.options]`<br>`host = "192.168.1.x"` | `username`, `password`, et `cert` *(auto : empreinte TLS)* | Caméras, sonnette, batterie, sirène et snapshots. |
| **Frigate** (`frigate`) | `id = "frigate"`<br>`kind = "frigate"`<br>`[driver.options]`<br>`mqtt_host = "192.168.1.x"`<br>`http_host = "192.168.1.x"` | `password_env` éventuel pour MQTT | Détections IA (personne, voiture, animal, colis) et snapshots. |
| **Télé Philips** (`philips`) | `id = "tv_salon"`<br>`kind = "philips"`<br>`[driver.options]`<br>`host = "192.168.1.x"`<br>`mac = "aa:bb:cc:dd:ee:ff"` | `username`, `password`, `atv_cert`, `atv_key`, et `cert`/`atv_pin`/`cast_pin` *(auto)* | JointSpace + Android TV Remote v2 + Cast + Wake-on-LAN + Ambilight. |
| **Bambu Lab** (`bambu`) | `id = "bambu"`<br>`kind = "bambu"`<br>`[driver.options]`<br>`host = "192.168.1.x"`<br>`serial = "…"` | `access_code` *(code LAN de l'imprimante)*, `cert` *(auto)* | État d'impression, buses, plateau, AMS et caméra. |
| **Klipper / Moonraker** (`klipper`, `moonraker`) | `id = "klipper"`<br>`kind = "klipper"`<br>`[driver.options]`<br>`host = "192.168.1.x"` | Aucun | Imprimantes 3D sous Klipper / Moonraker. |
| **Imprimante IPP** (`ipp`) | `id = "imprimante"`<br>`kind = "ipp"`<br>`[driver.options]`<br>`host = "192.168.1.x"` | Aucun | Niveau d'encre/toner et impression de PDF (IPP Everywhere / AirPrint). |

### 3.4 Maison, présence, météo, énergie et système

| Intégration (`kind`) | Exemple `moli.toml` | Secrets dans `secrets.enc` (`<instance>/<nom>`) | Notes |
|---|---|---|---|
| **Météo & Air Open-Meteo** (`profile`) | `id = "meteo"` / `id = "air"`<br>`kind = "profile"`<br>`profile = "open-meteo"` / `"open-meteo-air"`<br>`params = { latitude = "…", longitude = "…" }` | Aucun | Prévisions météo, UV, lever/coucher du soleil, qualité de l'air (AQI). |
| **EDF Tempo** (`profile`) | `id = "tempo"`<br>`kind = "profile"`<br>`profile = "tempo"` | Aucun | Couleur Tempo du jour et du lendemain. |
| **Piscine iopool** (`profile`) | `id = "piscine"`<br>`kind = "profile"`<br>`profile = "iopool"`<br>`params = { pool_id = "…" }` | `api_key` *(clé API iopool)* | Température de l'eau, pH, chlore (redox ORP), filtration conseillée. |
| **Compteur Meross EM06** (`profile`) | `id = "em06"`<br>`kind = "profile"`<br>`profile = "meross-em06"`<br>`host = "192.168.1.x"` | `key` *(clé Meross partagée)* | Mesure triphasée / 6 pinces en local. |
| **Téléphones Moli** (`phones`) | `id = "telephones"`<br>`kind = "phones"` | Jetons d'appairage gérés par Moli (`data/phones.json`) | Application mobile Moli (présence, batterie, Wi-Fi, notifications). |
| **Présence Wi-Fi** (`presence`) | `id = "presence"`<br>`kind = "presence"` | Aucun | Détection de présence sur le réseau local (ARP / ping). |
| **Box Internet UPnP** (`igd`) | `id = "box"`<br>`kind = "igd"` | Aucun | État de la connexion WAN et débits UPnP IGD. |
| **Machine hôte & PC** (`host`) | `id = "serveur"`<br>`kind = "host"` | Aucun | CPU, mémoire, disques, températures, conteneurs, Wake-on-LAN. |
| **Telegram** (`telegram`) | `id = "telegram"`<br>`kind = "telegram"`<br>`[driver.options]`<br>`chat_id = "…"` | `token_env` ou fichier `token_file = "/run/secrets/telegram.token"` | Envoi et réception de messages avec la maison, sans relais tiers. |
| **Aides de la maison** (`helpers`) | `id = "maison"`<br>`kind = "helpers"` | Aucun | Boutons de mode (`Mode Maison`), budgets, consignes partagées. |

---

## 4. Scripts externes, leurs secrets et l'installation des crons

Certaines données (gaz GRDF Gazpar, électricité Enedis Linky Data Connect, imports Home Assistant) sont récupérées par des scripts Python situés dans `tools/` et envoyées dans Moli via `moli-os energy import`.

### 4.1 Emplacement des fichiers et des secrets des scripts

Les identifiants de ces scripts ne sont **jamais** passés sur la ligne de commande (visibles dans `ps`) ni commis dans Git : ils sont placés dans des fichiers `.env` protégés (`chmod 600`) :

| Mode d'installation | Dossier des scripts et du venv | Fichiers de secrets (`chmod 600`) |
|---|---|---|
| **Natif / LXC (`--systemd`)** | `/opt/moli/` (`/opt/moli/venv/bin/python`) | `/opt/moli/grdf.env`<br>`/opt/moli/enedis.env` |
| **Docker (`--docker`)** | Dépôt `tools/` + `~/.venvs/gazpar/bin/python` | `~/.config/moli/grdf.env`<br>`~/.config/moli/enedis.env` |

#### Contenu de `grdf.env` (pour `tools/grdf/gazpar-export.py`)
```sh
GRDF_USERNAME=votre.email@example.com
GRDF_PASSWORD=votre_mot_de_passe_grdf
GRDF_PCE=12345678901234
```
*(Nécessite `pip install pygazpar` dans le `venv`, et un compteur `[[energy.meter]]` avec `point = "import:gaz/index"`, `role = "grid"`, `fuel = "gas"`, `price = …` dans `moli.toml`.)*

#### Contenu de `enedis.env` (pour `tools/enedis/enedis-export.py`)
```sh
ENEDIS_TOKEN=votre_jeton_myelectricaldata
ENEDIS_PDL=12345678901234
```
*(Le jeton s'obtient après consentement Enedis sur <https://www.myelectricaldata.fr>. Nécessite un compteur `[[energy.meter]]` avec `point = "import:elec/index"`, `role = "grid"`, `price = …` dans `moli.toml`.)*

#### Autres scripts ponctuels (`tools/home-assistant/`)
Ces scripts s'exécutent une seule fois lors d'une migration depuis Home Assistant (variables d'environnement passées à la volée) :
- `tools/home-assistant/ha-energy-export.py` : export de l'historique d'énergie SQLite de Home Assistant vers `moli-os energy import`.
- `tools/home-assistant/tuya-export.py` : extraction des `local_key` Tuya depuis `.storage/core.config_entries` vers `moli-os tuya import`.
- `tools/home-assistant/esphome-key-export.py` : lecture de la clé Noise ESPHome depuis `.storage/`.
- `tools/home-assistant/ha-areas-import.py` et `ha-automations-export.py` : reprise des pièces et des automatismes (en brouillons à valider).

### 4.2 Installation automatique des tâches planifiées (`cron`)

Le script `tools/install-energy-cron.sh` installe ou met à jour les deux tâches quotidiennes (`gazpar-export.py` à `07h15` et `enedis-export.py` à `07h30`) sans écraser les autres entrées de la crontab :

- **Sur une installation native / LXC (`systemd`)** :
  ```sh
  sudo cp tools/grdf/gazpar-export.py tools/enedis/enedis-export.py tools/install-energy-cron.sh /opt/moli/
  sudo chown moli:moli /opt/moli/*.env && sudo chmod 600 /opt/moli/*.env
  sudo sh /opt/moli/install-energy-cron.sh --systemd
  ```
  Vérifier la crontab installée :
  ```sh
  sudo crontab -u moli -l
  ```

- **Sur une installation Docker** :
  ```sh
  chmod 600 ~/.config/moli/*.env
  sh tools/install-energy-cron.sh --docker
  ```
  Vérifier la crontab installée :
  ```sh
  crontab -l
  ```

---

## 5. Accès hors de chez soi et récupération du code

### Accéder à Moli hors de chez soi (facultatif)

Ne jamais ouvrir le port 8790 directement sur Internet, ni placer de proxy inverse générique sur la même machine devant Moli. Deux méthodes sûres :
- **Tailscale** (ou autre VPN) configuré en **routeur de sous-réseau** (`subnet router`).
- **Cloudflare Tunnel + Cloudflare Access** : Moli vérifie lui-même le jeton signé par Access (section `[server.access]` de `moli.toml` avec `team`, `aud`, `owners`).

### Changer ou réinitialiser le code de la maison

- **Changer le code** : page **Système → Code du tableau de bord**.
- **Code oublié** : oublier `core/ui_pin` et `core/ui_sessions` dans le coffre puis redémarrer Moli ; un nouveau code d'installation (`ABCD-EFGH`) s'affiche dans les journaux :
  - **Docker** :
    ```sh
    docker compose stop moli-os
    docker compose run --rm moli-os secrets forget core ui_pin
    docker compose run --rm moli-os secrets forget core ui_sessions
    docker compose start moli-os && docker compose logs moli-os
    ```
  - **Systemd** :
    ```sh
    sudo systemctl stop moli-os
    cd /data && MOLI_MASTER_KEY=$(cat /data/master.key) /usr/local/bin/moli-os secrets forget core ui_pin
    cd /data && MOLI_MASTER_KEY=$(cat /data/master.key) /usr/local/bin/moli-os secrets forget core ui_sessions
    sudo chown moli:moli /data/secrets.enc
    sudo systemctl start moli-os && sudo journalctl -u moli-os -n 30 --no-pager
    ```
