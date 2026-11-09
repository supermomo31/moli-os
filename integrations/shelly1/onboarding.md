# Shelly Gen1 — mise en service

1. Donner une IP fixe au Shelly (baux DHCP du routeur).
2. Éteindre l'authentification web si elle est active (la version
   Gen1 n'est pas encore gérée par le pilote).
3. Déclarer le pilote dans `moli.toml` :

   ```toml
   [[driver]]
   id = "shelly"
   kind = "shelly1"
   [driver.options]
   devices = ["<ip>"]
   ```

4. Redémarrer Moli : l'appareil apparaît avec ses relais ; les
   modèles PM ajoutent puissance, énergie, tension et courant.
