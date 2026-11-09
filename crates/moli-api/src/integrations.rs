//! Keys a person gives Moli for an integration (the Tuya cloud project's
//! Access ID and Secret): typed in the dashboard by a human (code), filed in
//! Moli's encrypted store under the driver's name, never sent back. The
//! driver, waiting for them, restarts with them.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use moli_core::InstanceId;
use moli_runtime::Hub;
use serde::Deserialize;
use serde_json::json;

use crate::caller::Caller;
use crate::session::Sessions;

const ID: &str = "cloud_access_id";
const SECRET: &str = "cloud_access_secret";

fn tuya_instances(hub: &Hub) -> Vec<InstanceId> {
    hub.snapshot()
        .drivers
        .iter()
        .filter(|d| d.kind == "tuya")
        .map(|d| d.instance.clone())
        .collect()
}

fn qobuz_instances(hub: &Hub) -> Vec<InstanceId> {
    hub.snapshot()
        .drivers
        .iter()
        .filter(|d| d.kind == "qobuz")
        .map(|d| d.instance.clone())
        .collect()
}

/// The music service's stream quality (Qobuz's `format_id`), read back and
/// the formats it knows — `null`s when no music service runs.
pub(crate) async fn qobuz(State(hub): State<Hub>) -> Response {
    let music = hub.music();
    Json(json!({
        "format": music.as_ref().and_then(|m| m.format()),
        "formats": music.as_ref().map_or(&[][..], |m| m.formats()),
    }))
    .into_response()
}

#[derive(Deserialize)]
pub(crate) struct Quality {
    format_id: u32,
}

/// The stream quality the house chose: applied to the running music
/// service at once, filed in the store so the next start keeps it (the
/// `qobuz` driver reads the `format_id` secret over `moli.toml`).
pub(crate) async fn set_qobuz(
    State(hub): State<Hub>,
    Extension(humans): Extension<Arc<Sessions>>,
    caller: Caller,
    headers: HeaderMap,
    Json(quality): Json<Quality>,
) -> Response {
    let refuse = |status: StatusCode, message: &str| {
        (status, Json(json!({ "error": message }))).into_response()
    };
    if !humans.is_human(&headers, &caller.key) {
        return refuse(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.cle.qualite_humain"),
        );
    }
    let Some(music) = hub.music() else {
        return refuse(
            StatusCode::NOT_FOUND,
            &moli_i18n::tr!("serveur.pilote.absent", pilote = "qobuz"),
        );
    };
    if !music.set_format(quality.format_id) {
        return refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            &moli_i18n::tr!("serveur.cle.qualite_inconnue"),
        );
    }
    let value = quality.format_id.to_string();
    for instance in qobuz_instances(&hub) {
        if let Err(e) = hub.store_secret(&instance, "format_id", &value).await {
            return refuse(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string());
        }
    }
    tracing::info!(
        format_id = quality.format_id,
        "qobuz stream quality set from the dashboard"
    );
    Json(json!({ "stored": true, "format": quality.format_id })).into_response()
}

/// Whether each Tuya instance has cloud keys filed (never the keys).
pub(crate) async fn tuya_cloud(State(hub): State<Hub>) -> Response {
    let instances: Vec<_> = tuya_instances(&hub)
        .into_iter()
        .map(|i| {
            let configured = hub.secret(&i, ID).is_some() && hub.secret(&i, SECRET).is_some();
            json!({ "instance": i, "configured": configured })
        })
        .collect();
    Json(json!({ "instances": instances })).into_response()
}

#[derive(Deserialize)]
pub(crate) struct Keys {
    #[serde(default)]
    instance: Option<String>,
    access_id: String,
    access_secret: String,
}

fn plausible(value: &str, min: usize) -> bool {
    (min..=64).contains(&value.len()) && value.chars().all(|c| c.is_ascii_alphanumeric())
}

pub(crate) async fn set_tuya_cloud(
    State(hub): State<Hub>,
    Extension(humans): Extension<Arc<Sessions>>,
    caller: Caller,
    headers: HeaderMap,
    Json(keys): Json<Keys>,
) -> Response {
    let refuse = |status: StatusCode, message: &str| {
        (status, Json(json!({ "error": message }))).into_response()
    };
    if !humans.is_human(&headers, &caller.key) {
        return refuse(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.cle.tuya_humain"),
        );
    }
    let instances = tuya_instances(&hub);
    let instance = match keys.instance.as_deref() {
        Some(name) => InstanceId::from(name),
        None => match instances.first() {
            Some(i) => i.clone(),
            None => {
                return refuse(
                    StatusCode::NOT_FOUND,
                    &moli_i18n::tr!("serveur.pilote.absent", pilote = "tuya"),
                );
            }
        },
    };
    if !instances.contains(&instance) {
        return refuse(
            StatusCode::NOT_FOUND,
            &moli_i18n::tr!("serveur.cle.tuya_pilote_inconnu"),
        );
    }
    let (id, secret) = (keys.access_id.trim(), keys.access_secret.trim());
    if !plausible(id, 10) || !plausible(secret, 24) {
        return refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            &moli_i18n::tr!("serveur.cle.tuya_invalide"),
        );
    }
    for (name, value) in [(ID, id), (SECRET, secret)] {
        if let Err(e) = hub.store_secret(&instance, name, value).await {
            return refuse(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string());
        }
    }
    tracing::info!(%instance, "tuya cloud keys filed from the dashboard");
    Json(json!({ "stored": true, "instance": instance })).into_response()
}
