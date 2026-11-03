# Spécification Technique Globale : Intégration Qobuz & Gestion Multi-room pour Moli (Domotique en Rust)

Ce document récapitule l'ensemble complet de la spécification pour le projet **Moli**, de la capture vocale (via satellite ESPHome/ReSpeaker) jusqu'au routage et à la lecture du flux musical Qobuz.

---

## 1. Architecture Globale du Pipeline

1. **Entrée Vocale (Satellite ESPHome / ReSpeaker) :**
   - Le ReSpeaker écoute et détecte le mot-clé (Wake Word).
   - L'audio est transcrit en texte (STT) et envoyé à l'application principale Moli (développée en **Rust**).
2. **Analyse de l'Intention Musicale & Cible (NLP / Parsing) :**
   - Moli analyse le texte pour comprendre la demande musicale (artiste, titre, album, ambiance, best-of).
   - Moli extrait également si une pièce/lecteur cible est mentionné (ex: *"dans la cuisine"*).
3. **Gestion du Routage & Dialogue (Multi-room) :**
   - Si aucun lecteur n'est précisé, Moli consulte sa configuration globale (bouton radio : lecteur fixe ou *"Demander à chaque fois"*).
   - En mode *"Demander à chaque fois"*, Moli bascule dans une machine à états conversationnelle pour demander à l'utilisateur où diffuser le son.
4. **Interrogation de l'API Qobuz :**
   - Traduction de l'intention en requêtes HTTP vers l'API non officielle de Qobuz (`/catalog/search`, etc.).
   - Récupération de l'ID de la piste et de l'URL de streaming sécurisée (`/track/getFileUrl`).
5. **Sortie Audio (Player) :**
   - Moli transmet le flux ou l'URL au lecteur audio cible (via crate `rodio` ou pilotage de lecteur réseau/GStreamer/MPD).

---

## 2. Modélisation des Structures Rust (`MusicRequest` & États)

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MusicRequest {
    Track { title: String, artist: Option<String> },
    Artist { name: String },
    Album { title: String, artist: Option<String> },
    MoodOrGenre { query: String },
    BestOf { artist: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DefaultPlayerConfig {
    Specific(String), // Ex: "player_salon", "player_cuisine"
    AskEveryTime,     // Mode interactif de clarification
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoliSettings {
    pub default_player: DefaultPlayerConfig,
    pub qobuz_app_id: String,
    pub qobuz_user_token: String,
}

#[derive(Debug, Clone)]
pub enum MoliState {
    Idle,
    WaitingForPlayer { pending_music: MusicRequest },
}

être copié-collé pour ton projet Moli :
# Spécification Technique Globale : Intégration Qobuz & Gestion Multi-room pour Moli (Domotique en Rust)

Ce document récapitule l'ensemble complet de la spécification pour le projet **Moli**, de la capture vocale (via satellite ESPHome/ReSpeaker) jusqu'au routage et à la lecture du flux musical Qobuz.

---

## 1. Architecture Globale du Pipeline

1. **Entrée Vocale (Satellite ESPHome / ReSpeaker) :**
   - Le ReSpeaker écoute et détecte le mot-clé (Wake Word).
   - L'audio est transcrit en texte (STT) et envoyé à l'application principale Moli (développée en **Rust**).
2. **Analyse de l'Intention Musicale & Cible (NLP / Parsing) :**
   - Moli analyse le texte pour comprendre la demande musicale (artiste, titre, album, ambiance, best-of).
   - Moli extrait également si une pièce/lecteur cible est mentionné (ex: *"dans la cuisine"*).
3. **Gestion du Routage & Dialogue (Multi-room) :**
   - Si aucun lecteur n'est précisé, Moli consulte sa configuration globale (bouton radio : lecteur fixe ou *"Demander à chaque fois"*).
   - En mode *"Demander à chaque fois"*, Moli bascule dans une machine à états conversationnelle pour demander à l'utilisateur où diffuser le son.
4. **Interrogation de l'API Qobuz :**
   - Traduction de l'intention en requêtes HTTP vers l'API non officielle de Qobuz (`/catalog/search`, etc.).
   - Récupération de l'ID de la piste et de l'URL de streaming sécurisée (`/track/getFileUrl`).
5. **Sortie Audio (Player) :**
   - Moli transmet le flux ou l'URL au lecteur audio cible (via crate `rodio` ou pilotage de lecteur réseau/GStreamer/MPD).

---

## 2. Modélisation des Structures Rust (`MusicRequest` & États)

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MusicRequest {
    Track { title: String, artist: Option<String> },
    Artist { name: String },
    Album { title: String, artist: Option<String> },
    MoodOrGenre { query: String },
    BestOf { artist: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DefaultPlayerConfig {
    Specific(String), // Ex: "player_salon", "player_cuisine"
    AskEveryTime,     // Mode interactif de clarification
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoliSettings {
    pub default_player: DefaultPlayerConfig,
    pub qobuz_app_id: String,
    pub qobuz_user_token: String,
}

#[derive(Debug, Clone)]
pub enum MoliState {
    Idle,
    WaitingForPlayer { pending_music: MusicRequest },
}

3. Logique de Parsing et de Dialogue
A. Analyse de la commande textuelle
Moli doit utiliser un parseur basé sur des expressions régulières (regex) ou des mots-clés pour catégoriser la demande :
⚬	Best of / Meilleurs titres : "meilleur de [artiste]", "best of [artiste]"
⚬	Album : "l'album [titre]"
⚬	Ambiance / Genre : Mots-clés comme "chill", "jazz", "rock", "ambiance"
⚬	Artiste / Piste par défaut
B. Extraction du lecteur cible
Moli recherche des mots-clés de localisation dans la phrase ("salon", "cuisine", "chambre").
C. Machine à états (Dialogue)
pub async fn handle_command(
    text: &str,
    settings: &MoliSettings,
    state: &mut MoliState,
) -> Result<String, Box<dyn std::error::Error>> {
    // 1. Si on attendait une réponse pour le choix du lecteur
    if let MoliState::WaitingForPlayer { pending_music } = state {
        let chosen_player = extract_player_target(text).unwrap_or_else(|| text.to_string());
        *state = MoliState::Idle;
        return execute_play(pending_music.clone(), &chosen_player).await;
    }

    // 2. Traitement standard d'une nouvelle commande
    let music_req = parse_music_command(text);
    let explicit_player = extract_player_target(text);

    let player = match explicit_player {
        Some(p) => p,
        None => match &settings.default_player {
            DefaultPlayerConfig::Specific(p) => p.clone(),
            DefaultPlayerConfig::AskEveryTime => {
                *state = MoliState::WaitingForPlayer { pending_music: music_req };
                return Ok("Sur quel lecteur souhaites-tu écouter cela ?".to_string());
            }
        },
    };

    execute_play(music_req, &player).await
}

4. Intégration de l'API Qobuz
Utilisation de reqwest et serde pour communiquer avec les endpoints Qobuz :
	1.	Recherche de catalogue :
⚬	URL : https://www.qobuz.com/api.json/0.2/catalog/search
⚬	Header : X-App-Id:
⚬	Paramètre : query, limit
	2.	Récupération du lien de streaming direct :
⚬	URL : https://www.qobuz.com/api.json/0.2/track/getFileUrl
⚬	Headers : X-App-Id, X-User-Auth-Token
⚬	Paramètres : track_id, format (ex: 5 pour flux standard/haute qualité).
5. Lecture Audio (Côté Moli)
Une fois l'URL du flux audio récupérée, Moli transmet cette URL au lecteur audio sélectionné (via la crate Rust rodio pour une lecture locale sur le serveur, ou via un protocole de diffusion compatible avec le matériel de la pièce visée).