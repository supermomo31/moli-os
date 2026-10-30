# Historique et énergie

## Historique (`crates/moli-history`)

- SQLite `data/history.db`, enregistrement **au changement** (le hub n'émet que les
  changements), 30 jours (`retention_days`), la dernière valeur de chaque point jamais
  purgée (une valeur stable a toujours une valeur « avant la fenêtre »).
- Lecture : `series(point, from, to, max_points)` → valeurs brutes, ou seaux (moyenne, min,
  max, nombre) quand il y en a trop.
- Surfaces : `/api/history` (courbes du dashboard), outil MCP `history` (statistiques +
  série compacte) et **`history_sql`** pour les agents : une seule requête `SELECT`, `ATTACH`,
  `PRAGMA`, `BEGIN` interdits, 2 s et 200 lignes maximum, 64 Ko par valeur, sur la vue
  `history(point, ts, kind, num, txt)`.
- Utilisé par : les courbes, les passages du jour (caméras), « dernier passage » à la porte,
  Moli (questions sur le passé), le nœud d'automatisme… (les automatismes lisent l'état
  courant, pas l'historique).
- Ordre de grandeur : ~2,5 Mo pour environ 70 appareils sur 30 jours.

## Énergie (`crates/moli-energy`)

Les compteurs cumulés deviennent des **kWh et des euros heure par heure**, gardés à vie dans
`data/energy.db`.

- Configuration `[energy]` de `moli.toml` : tarif (période courante lue sur un point, prix
  par période : par exemple HC 0,14 €, HP 0,17 €), et `[[energy.meter]]` avec un rôle :
  - `grid` : ce que le fournisseur facture (index Linky HC / HP, au kWh près) ;
  - `total` : toute la maison mesurée finement (par exemple un compteur EM06 sur l'arrivée
    générale, au Wh) ;
  - `circuit` : un circuit du tableau (par exemple « Chauffe-eau »).
  - `appliance` : un **appareil** mesuré à sa prise (par exemple la télé) : affiché
    **dans** son circuit (`within = "<id du circuit>"`) ou dans « le reste de la maison »
    (sans `within`), jamais ajouté au total ni retranché du reste (pas de double compte).
  - `integrate = true` : `point` est une **puissance** (W) ; Moli en fait l'énergie
    (puissance tenue × temps, au plus 15 min d'un coup, un tic toutes les 5 min, rien de
    compté hors ligne ni pendant un arrêt de Moli). Pour les prises Tuya, dont `add_ele`
    repart de zéro à chaque envoi et n'est pas un compteur fiable.
  - `estimate = true` (appareil seulement, sans `integrate` ni `power`) : une **lumière connectée** qui ne mesure rien ; `point` = un point de la lampe ou d'un luminaire `lumieres:…`. Moli estime sa puissance d'après le profil mesuré de son modèle (powercalc, licence MIT, condensé par `scripts/light-profiles.py` dans `crates/moli-energy/src/estimate.rs` : LCA006, LTW001, LWA017, L530) : luminosité, blanc ou couleur, veille éteinte, **0 quand elle est coupée au mur** ; un luminaire = la somme de ses ampoules. Comptée dans le temps comme `integrate`, jamais ajoutée aux totaux, `estimated: true` dans l'API, « ≈ estimé » à l'écran. Modèle inconnu : pas d'estimation (jamais de chiffre inventé).
  - Pas repris de HA : ses capteurs « powercalc » (estimations, pas des mesures).
  - `feeds = ["<device id>"]` : les appareils qu'un compteur alimente (par exemple le
    circuit d'une clim → cette clim) : leur carte montre sa puissance (« circuit partagé »
    s'il alimente autre chose). Les adaptateurs Daikin BRP069 vérifiés ne mesurent **pas**
    leur conso (`get_day_power_ex` / `get_year_power_ex` à 0 même clim en marche,
    `cmpfreq=999`) : seul le tableau la donne.
  - `[energy.live]` (optionnel) : `power` = le point de puissance instantanée du compteur du
    fournisseur (Linky PAPP, en VA), `max` = l'abonnement dans la même unité (par
    exemple 9 000 pour un abonnement de 9 kVA), `warn` = à partir d'où la conso est
    « soutenue » (par exemple 2 000 ; défaut un quart de `max`) : la jauge vire à l'orange,
    puis au rouge au double. Sert la jauge « en direct » et la ligne du prompt de Moli.
  - `monthly_fee` (optionnel) : l'abonnement par mois TTC (par exemple 20 €) : la facture
    estimée = kWh du mois au rythme actuel + abonnement, comme le « Projection facture fin de
    mois » de HA. `budget` (optionnel) : le point qui tient le budget mensuel (par exemple
    une aide `maison:budget_elec_mensuel/value`).
- **Appareil « Énergie »** (`energie:maison`, `points.rs`) : Moli le fait tourner tout seul
  dès que `[energy]` existe (l'id de pilote `energie` est réservé). Points en lecture seule,
  recalculés toutes les 5 min : `today_kwh/_cost`, `yesterday_kwh/_cost`, `month_kwh/_cost`
  (kWh seulement), `projection_kwh`, `bill_projection` (abonnement compris, absent le premier
  jour du mois), `over_budget` (si `budget` : facture estimée > budget, jamais avant 5 jours
  écoulés, comme HA qui attendait le 6). Sémantique `other`, jamais `energy` : ce sont des
  résumés, pas des compteurs à additionner. Sert les automatismes (par exemple une alerte de
  budget mensuel) et les agents.
- Robustesse : saut impossible (> 36 kW) ou baisse = suspect ; remise à zéro comptée une fois
  confirmée ; nouveau socle après 3 relevés cohérents ; chaque heure au prix de sa période
  (l'index HC n'est étalé que sur du temps HC).
- Chiffre principal = la plus complète des deux mesures (`grid` et `total`, par exemple Linky
  et EM06).
- **Historique repris de HA** (statistiques long terme, lecture seule) :
  `tools/home-assistant/ha-energy-export.py … | docker exec -i moli-os /moli-os energy
  import`, relançable sans doublon.
- **Gazpar (gaz, quotidien)** : GRDF n'a pas d'API publique pour les particuliers
  (ADICT est réservé aux entreprises). `tools/grdf/gazpar-export.py` lit le
  « Mon espace » via PyGazpar — identifiants en variables d'environnement, jamais
  dans un fichier — et sort les kWh de chaque jour, à donner à `energy import`
  en tâche planifiée (J-1 ou J-2, rien de temps réel : le compteur ne transmet
  qu'une fois par jour). Les jours « estimés » sont ignorés : GRDF les corrige,
  et l'import ne reprend jamais une heure connue. Le compteur se déclare
  `role = "grid"` avec `price` (€/kWh TTC) et un `point` jamais publié
  (par exemple `import:gaz/index`) : c'est ce que le fournisseur facture, les
  kWh du gaz sont bien des kWh — `grid` les fait entrer dans le total, le coût
  et la projection de la page Énergie (un `appliance` n'y apparaîtrait pas : il
  n'entre ni dans le total ni dans « le reste », qui exige un compteur `total`
  et des circuits). `fuel = "gas"` marque le compteur comme gaz : quand tous
  les compteurs facturés en sont, la page Énergie se met en mode gaz — libellé,
  icône, pas de jauge direct ni d'analyse horaire ni de répartition par
  appareil, historique par jour (ces écrans n'auraient rien à montrer d'un
  relevé quotidien).
- **Linky (électricité, par heure)** : l'API d'Enedis pour les particuliers est
  Data Connect, atteinte ici via la passerelle **MyElectricalData** : le
  titulaire consent une fois sur leur site (ça active aussi la « collecte
  horaire » chez Enedis, qui peut mettre des jours à démarrer) et reçoit un
  jeton valable jusqu'à trois ans. `tools/enedis/enedis-export.py` —
  `ENEDIS_TOKEN` et `ENEDIS_PDL` en variables d'environnement, jamais dans un
  fichier — sort les kWh de chaque **heure** (la courbe de charge 30 min
  sommée, 7 jours par appel, borne d'Enedis), à donner à `energy import` en
  tâche planifiée (J-1 ou J-2, rien de temps réel). `--granularity daily`
  (kWh par jour, années entières par appel) sert au backfill : chaque jour
  tombe dans sa première heure, donc **jamais de daily sur un jour que
  l'horaire couvrira** — double comptage ; `--until N` arrête le backfill N
  jours avant aujourd'hui, pile avant la fenêtre `--days` de la tâche
  quotidienne. Un 5xx Enedis (plage sans données ou faute transitoire) saute
  le morceau, un 401/429 arrête. Le compteur se déclare `role = "grid"` avec
  `price` et un `point` jamais publié (`import:elec/index`).
- Surfaces : `/api/energy` (en direct, aujourd'hui, hier, mois + projection, mois dernier,
  part non mesurée), `/api/energy/series?step=hour|day|month`, outil MCP `energy`, page
  Énergie, carte Accueil ; la ligne « Électricité en direct » du prompt de Moli.
- **Brique future : production** (panneaux solaires, injection, batterie).
