# Onboarding : Somfy TaHoma / Connectivity Kit

1. Déclarer `[[driver]]` avec `id = "tahoma"`, `kind = "tahoma"` dans `data/moli.toml`.
2. Enregistrer l'identifiant (e-mail Somfy) et le mot de passe du compte TaHoma dans le coffre chiffré de Moli (`username` et `password`).
3. Redémarrer Moli (`docker compose restart moli-os`).
4. Tester : les volets roulants et stores apparaissent dans leurs pièces avec les boutons Ouvrir / Stop / Fermer (`control`) et, pour les volets IO, le pourcentage d'ouverture (`percent_control`).
