# Tasmota (Sonoff…)

Appareils flashés **Tasmota** (Sonoff, Tuya-to-Tasmota, ESP32…) qui publient
sur un broker MQTT : relais `POWER<n>`, mesures de puissance/énergie par
canal (puces BL0937/HLW8012), capteurs de température, humidité, pression,
lumière et signal Wi-Fi.

## Configuration

```toml
[[driver]]
id = "tasmota"
kind = "tasmota"

[driver.options]
host = "192.168.1.x"   # le broker MQTT
port = 1883
# username = "moli"    # si le broker demande une authentification
# password_env = "MOLI_MQTT_PASSWORD"   # variable d'environnement, jamais le mot de passe
# devices = ["4CHPRO-6249"]   # optionnel : seulement ces hôtes Tasmota
```

Le pilote s'abonne à `tele/#` et `stat/#` : **chaque appareil Tasmota du
broker devient un appareil Moli** dès qu'il publie (son `stat/<hôte>/LWT`
retenu le fait apparaître même avant son premier message). L'option
`devices` permet de ne retenir qu'une liste d'hôtes.

## Ce que Moli voit

Un appareil **multi-relais** devient **plusieurs appareils Moli** :
`<hôte>` pour les capteurs partagés (énergie totale, RSSI, sondes) et
`<hôte>/c<n>` pour chaque canal — renommable et assignable à sa pièce.
Un appareil à **un seul relais** garde son nom d'hôte.

| Point | Sens | Source |
|---|---|---|
| `state` | marche/arrêt du relais (un appareil par canal) | `tele/<hôte>/STATE`, `stat/<hôte>/STATUS*`, `stat/<hôte>/POWER<n>` |
| `power`, `energy`, `apparent_power`, `voltage`, `current` | mesures électriques **du canal** | `tele/<hôte>/SENSOR` → `POWER<n>.*` |
| `temperature`, `humidity`, `pressure`, `illuminance`, `battery` | capteurs ambiants | objets nommés de `SENSOR` (`SHT3X-…`, `DS18B20-…`, `ANALOG`…) ; suffixés par le nom de la sonde s'il y en a plusieurs |
| `energy_total` | compteur total historique | `SENSOR.ENERGY.Total` |
| `rssi` | signal Wi-Fi (dBm) | `SENSOR.Wifi.RSSI` |

## Commandes

Un `state` écrit sur l'appareil d'un canal publie
`cmnd/<hôte>/POWER<n> ON|OFF` (`TOGGLE` accepté en texte). Les mesures
sont en lecture seule.

## Testé sur

- Sonoff 4CH Pro (Tasmota 13.x, 4 relais + mesure BL0939 par canal),
  MQTT Mosquitto.
