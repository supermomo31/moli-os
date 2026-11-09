# Tasmota — mise en route

1. Faire pointer l'appareil Tasmota vers le broker MQTT de la maison
   (console série ou OTA : `SetOption11 1`, `Hostname`, `MqttHost`,
   `MqttPort`, identifiants si le broker en demande).
2. Déclarer le pilote dans `moli.toml` :

   ```toml
   [[driver]]
   id = "tasmota"
   kind = "tasmota"

   [driver.options]
   host = "192.168.1.x"   # le broker MQTT
   port = 1883
   ```

   Si le broker exige une authentification : `username` + `password_env`
   (nom de la variable d'environnement portant le mot de passe — jamais le
   mot de passe en clair). `devices = ["nom1", "nom2"]` limite la liste des
   hôtes Tasmota exposés.
3. Redémarrer Moli. Chaque appareil Tasmota apparaît dans les minutes qui
   suivent (son message `LWT` retenu suffit à le faire naître ; un
   `cmnd/<hôte>/Status 0` lui demande un `STATE` frais).
4. Dans la maison, placer les appareils dans leurs pièces et les renommer ;
   les points `power`/`energy` de chaque canal peuvent servir de compteurs
   d'énergie (`[[energy.meter]]`, rôle `appliance`).

## Limites

- Seuls les topics `tele/`, `stat/` et `cmnd/` par défaut sont parlés ; un
  préfixe MQTT personnalisé côté Tasmota n'est pas (encore) géré.
- Les mesures par canal supposent le format `POWER<n>.{Power,Energy,…}` de
  `SENSOR` ; le compteur global historique `ENERGY.Total` devient
  `energy_total`.
