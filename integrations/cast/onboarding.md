# Onboarding : enceinte Google Cast (Google Home, Nest)

1. Repérer l'adresse IP de l'enceinte sur le réseau local (mDNS `_googlecast._tcp`, port `8009`) ;
   une réservation DHCP sur le routeur est conseillée.
2. Ajouter un bloc `[[driver]]` dans `moli.toml` avec `kind = "cast"`, `host`, et `media_base`
   (`http://<ip-de-moli>:8790` pour que l'enceinte puisse lire la voix de Moli) :
   ```toml
   [[driver]]
   id = "google_home"
   kind = "cast"

   [driver.options]
   host = "192.168.1.x"
   name = "Google Home"
   media_base = "http://192.168.1.y:8790"
   ```
3. Vérifier avec `moli-os check-config`, puis redémarrer Moli.
4. Pour utiliser cette enceinte comme haut-parleur d'un satellite vocal ESPHome sans haut-parleur
   intégré (ex. galet rotatif Waveshare), ajouter `cast_speaker = "192.168.1.x"` dans les options du
   pilote `esphome`.
