# Yeelight Wi-Fi

Ampoules et rubans **Yeelight** pilotés **en local**, sans compte cloud :
le pilote parle directement le protocole LAN de Yeelight (JSON sur TCP,
port 55443) — `get_prop`, `set_power`, `set_bright`, `set_ct_abx`,
`set_rgb`, `toggle`.

## Configuration

```toml
[[driver]]
id = "yeelight"
kind = "yeelight"

[driver.options]
devices = ["192.168.1.x", "192.168.1.y"]   # adresses des ampoules
# port = 55443        # port LAN Yeelight (défaut)
# poll_secs = 30      # intervalle de lecture d'état (défaut 30 s)
```

L'adresse peut inclure le port : `"192.168.1.x:55443"`. L'option `port`
est le port par défaut pour les adresses sans port explicite.

## Découverte des adresses

Les ampoules Yeelight ne répondent pas toujours au SSDP : pour trouver
leurs IP, scanner le port 55443 du sous-réseau :

```sh
for i in $(seq 1 254); do
  (echo > /dev/tcp/192.168.1.$i/55443) 2>/dev/null && echo "192.168.1.$i"
done
```

puis vérifier avec `nc 192.168.1.x 55443` ou la sonde `get_prop`.

## Ce que Moli voit

| Point | Sens | Source |
|---|---|---|
| `on` | marche/arrêt | `power` (`set_power`, `toggle`) |
| `brightness` | luminosité 1–100 % | `bright` (`set_bright`) |
| `color_temp` | température de couleur 1700–6500 K | `ct` (`set_ct_abx`) |
| `color` | couleur `#rrggbb` (modèles color/stripe/ceiling…) | `rgb` (`set_rgb`) |
| `model` | modèle de l'ampoule (lecture seule) | `model` |

L'état est relevé toutes les `poll_secs` secondes ; après chaque commande,
la sonde est relancée pour un retour visuel immédiat. Une ampoule
injoignable passe hors ligne jusqu'au prochain relevé réussi.

## Limites

- Pas de scènes, d'effets (`flow`) ni de musique embarquée : seuls les
  points ci-dessus sont exposés.
- Les ampoules **blanches** (`mono*`, `temp*`) n'ont pas de point `color`.
- Le protocole LAN exige que l'ampoule soit sur le même réseau local ;
  aucune donnée ne quitte la maison.
