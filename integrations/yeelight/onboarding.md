# Yeelight Wi-Fi — mise en route

1. Repérer l'adresse IP de chaque ampoule (scan du port 55443, voir le
   README de l'intégration).
2. Déclarer le pilote dans `moli.toml` :

   ```toml
   [[driver]]
   id = "yeelight"
   kind = "yeelight"

   [driver.options]
   devices = ["192.168.1.x", "192.168.1.y"]
   # poll_secs = 30
   ```

3. Redémarrer Moli. Chaque ampoule apparaît avec ses points `on`,
   `brightness`, `color_temp`, `color` (si le modèle est colorimétrique)
   et `model`. L'état est relevé toutes les 30 secondes.
4. Dans la maison, renommer les ampoules et les placer dans leurs pièces ;
   `brightness`, `color_temp` et `color` sont aussi pilotables à la voix
   et par les automatisations.

## Limites

- Un appareil = une ampoule : pas de regroupement de scènes Yeelight.
- Pas de contrôle hors du LAN (le cloud Yeelight n'est jamais contacté,
  volontairement).
