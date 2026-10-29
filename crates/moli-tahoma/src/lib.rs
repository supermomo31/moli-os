//! Somfy TaHoma and Connectivity Kit roller shutters, blinds, awnings and
//! curtains (Overkiz API on `ha101-1.overkiz.com`).
//!
//! Credentials (`username` and `password` of the Somfy / TaHoma account) live
//! in Moli's encrypted secret store. To avoid triggering Somfy's rate limiter
//! on bad credentials, a `401` or `403` from `/login` clears the stored
//! password and parks the driver in `waiting` until a person sets it again.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context as _, bail};
use http::{HeaderMap, Method, Request, StatusCode};
use http_body_util::Full;
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_net::Body as Bytes;
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use tokio::time::{Instant, MissedTickBehavior};

const DEFAULT_SERVER: &str = "ha101-1.overkiz.com";
const BASE_PATH: &str = "/enduser-mobile-web/enduserAPI";
const USER_AGENT: &str = "TaHoma/1.0 (MoliOS)";
const USERNAME_KEY: &str = "username";
const PASSWORD_KEY: &str = "password";

const HTTP_LIMIT: Duration = Duration::from_secs(15);
const MAX_BODY: usize = 4 * 1024 * 1024;
const POLL_EVENTS: Duration = Duration::from_secs(30);
const POLL_SETUP: Duration = Duration::from_secs(30 * 60);
const SESSION_LIFETIME: Duration = Duration::from_secs(45 * 60);

const COVER_UI_CLASSES: &[&str] = &[
    "RollerShutter",
    "ExteriorScreen",
    "Screen",
    "Curtain",
    "Awning",
    "Pergola",
    "VenetianBlind",
    "ExteriorVenetianBlind",
    "SwingingShutter",
    "GarageDoor",
    "Gate",
    "Window",
];

fn default_server() -> String {
    DEFAULT_SERVER.into()
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Overkiz server host (`ha101-1.overkiz.com` for Somfy Europe).
    #[serde(default = "default_server")]
    pub server: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: default_server(),
        }
    }
}

#[derive(Debug)]
pub struct Tahoma {
    config: Config,
}

impl Tahoma {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Tahoma {
    fn kind(&self) -> &'static str {
        "tahoma"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

#[derive(Clone, Debug)]
pub struct ShutterInfo {
    pub device_url: String,
    pub name: String,
    pub room: Option<String>,
    pub model: String,
    pub open_cmd: String,
    pub close_cmd: String,
    pub stop_cmd: String,
    pub has_set_closure: bool,
    pub online: bool,
    pub open_percent: Option<f64>,
    pub state_control: Option<&'static str>,
}

fn form_encode(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        out.push_str(k);
        out.push('=');
        for b in v.bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                out.push(char::from(b));
            } else {
                use std::fmt::Write as _;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

fn extract_cookie(headers: &HeaderMap) -> Option<String> {
    for val in headers.get_all(http::header::SET_COOKIE) {
        let Ok(s) = val.to_str() else { continue };
        let pair = s.split(';').next()?.trim();
        if pair.starts_with("JSESSIONID=") {
            return Some(pair.to_owned());
        }
    }
    None
}

enum LoginOutcome {
    Cookie(String),
    Rejected(String),
}

fn looks_corrupted(secret: &str) -> bool {
    secret.is_empty()
        || secret.len() > 256
        || secret.chars().any(|c| matches!(c, '\n' | '\r'))
        || secret.contains("docker compose")
        || secret.contains("secrets set")
}

async fn login(server: &str, username: &str, password: &str) -> anyhow::Result<LoginOutcome> {
    if looks_corrupted(username) || looks_corrupted(password) {
        return Ok(LoginOutcome::Rejected("corrupted credentials".into()));
    }
    let body = form_encode(&[("userId", username), ("userPassword", password)]).into_bytes();
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("{BASE_PATH}/login"))
        .header("user-agent", USER_AGENT)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("content-length", body.len().to_string())
        .header("accept", "application/json")
        .body(Full::new(Bytes::from(body)))?;
    let (status, headers, bytes) =
        moli_net::web_with_headers(server, 443, true, req, HTTP_LIMIT, MAX_BODY).await?;
    if matches!(
        status,
        StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ) {
        let detail = String::from_utf8_lossy(&bytes[..bytes.len().min(160)]).into_owned();
        return Ok(LoginOutcome::Rejected(format!(
            "HTTP {} {detail}",
            status.as_u16()
        )));
    }
    if !status.is_success() {
        bail!("tahoma login: HTTP {}", status.as_u16());
    }
    let cookie = extract_cookie(&headers).context("no JSESSIONID cookie in TaHoma login")?;
    Ok(LoginOutcome::Cookie(cookie))
}

async fn api_call(
    server: &str,
    method: Method,
    subpath: &str,
    cookie: &str,
    body: Option<&Json>,
) -> anyhow::Result<(StatusCode, Json)> {
    let mut builder = Request::builder()
        .method(method)
        .uri(format!("{BASE_PATH}{subpath}"))
        .header("user-agent", USER_AGENT)
        .header("cookie", cookie)
        .header("accept", "application/json");
    let payload = if let Some(b) = body {
        let raw = b.to_string().into_bytes();
        builder = builder
            .header("content-type", "application/json; charset=utf-8")
            .header("content-length", raw.len().to_string());
        Bytes::from(raw)
    } else {
        builder = builder.header("content-length", "0");
        Bytes::new()
    };
    let req = builder.body(Full::new(payload))?;
    let (status, bytes) = moli_net::web(server, 443, true, req, HTTP_LIMIT, MAX_BODY).await?;
    let json = serde_json::from_slice(&bytes).unwrap_or(Json::Null);
    Ok((status, json))
}

fn collect_places(place: &Json, out: &mut HashMap<String, String>) {
    if let (Some(oid), Some(label)) = (place["oid"].as_str(), place["label"].as_str()) {
        let trimmed = label.trim();
        if !trimmed.is_empty() {
            out.insert(oid.to_owned(), trimmed.to_owned());
        }
    }
    if let Some(subs) = place["subPlaces"].as_array() {
        for sub in subs {
            collect_places(sub, out);
        }
    }
}

pub fn sanitize_device_url(url: &str) -> String {
    url.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_owned()
}

fn decode_states(states: &[Json]) -> (Option<f64>, Option<&'static str>) {
    let mut open_percent = None;
    let mut state_control = None;
    for st in states {
        match st["name"].as_str() {
            Some("core:ClosureState" | "core:DeploymentState") => {
                if let Some(closure) = st["value"].as_f64().filter(|v| v.is_finite()) {
                    open_percent = Some((100.0 - closure).clamp(0.0, 100.0).round());
                }
            }
            Some("core:OpenClosedState" | "core:OpenClosedUnknownState") => {
                state_control = match st["value"].as_str() {
                    Some("open") => Some("open"),
                    Some("closed") => Some("close"),
                    _ => None,
                };
            }
            _ => {}
        }
    }
    if state_control.is_none()
        && let Some(pct) = open_percent
    {
        state_control = Some(if pct <= 1.0 { "close" } else { "open" });
    }
    (open_percent, state_control)
}

pub fn parse_setup(setup: &Json) -> Vec<ShutterInfo> {
    let mut places = HashMap::new();
    collect_places(&setup["rootPlace"], &mut places);
    let devices = setup["devices"]
        .as_array()
        .or_else(|| setup.as_array())
        .cloned()
        .unwrap_or_default();

    let mut out = Vec::new();
    for dev in &devices {
        let Some(device_url) = dev["deviceURL"].as_str() else {
            continue;
        };
        let ui_class = dev["definition"]["uiClass"].as_str().unwrap_or_default();
        let cmds: Vec<&str> = dev["definition"]["commands"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["commandName"].as_str())
            .collect();
        let has_open_close = (cmds.contains(&"open") || cmds.contains(&"up"))
            && (cmds.contains(&"close") || cmds.contains(&"down"));
        if !COVER_UI_CLASSES.contains(&ui_class) && !has_open_close {
            continue;
        }
        let open_cmd = if cmds.contains(&"open") {
            "open"
        } else if cmds.contains(&"up") {
            "up"
        } else {
            continue;
        };
        let close_cmd = if cmds.contains(&"close") {
            "close"
        } else if cmds.contains(&"down") {
            "down"
        } else {
            continue;
        };
        let stop_cmd = if cmds.contains(&"stop") {
            "stop"
        } else if cmds.contains(&"my") {
            "my"
        } else {
            "stop"
        };
        let has_set_closure = cmds.contains(&"setClosure");
        let name = dev["label"].as_str().unwrap_or("Volet").trim().to_owned();
        let room = dev["placeOID"]
            .as_str()
            .and_then(|oid| places.get(oid).cloned());
        let model = dev["controllableName"]
            .as_str()
            .and_then(|s| s.split(':').next_back())
            .unwrap_or(if ui_class.is_empty() {
                "RollerShutter"
            } else {
                ui_class
            })
            .to_owned();
        let online = dev["available"].as_bool().unwrap_or(true);
        let states = dev["states"].as_array().map_or(&[][..], Vec::as_slice);
        let (open_percent, state_control) = decode_states(states);
        out.push(ShutterInfo {
            device_url: device_url.to_owned(),
            name,
            room,
            model,
            open_cmd: open_cmd.to_owned(),
            close_cmd: close_cmd.to_owned(),
            stop_cmd: stop_cmd.to_owned(),
            has_set_closure,
            online,
            open_percent,
            state_control,
        });
    }
    out
}

fn build_device(ctx: &DriverCtx, id: &DeviceId, shutter: &ShutterInfo) -> Device {
    let mut points = vec![PointSpec {
        key: "control".into(),
        label: moli_i18n::tr!("pilotes.tahoma.commande").into(),
        kind: Kind::Enum {
            values: vec!["open".into(), "stop".into(), "close".into()],
        },
        access: Access {
            read: true,
            write: true,
        },
        unit: None,
        semantic: Semantic::Control,
    }];
    if shutter.has_set_closure || shutter.open_percent.is_some() {
        points.push(PointSpec {
            key: "percent_control".into(),
            label: moli_i18n::tr!("pilotes.tahoma.ouverture").into(),
            kind: Kind::Numeric {
                min: Some(0.0),
                max: Some(100.0),
                step: Some(1.0),
            },
            access: Access {
                read: true,
                write: shutter.has_set_closure,
            },
            unit: Some(Unit::Percent),
            semantic: Semantic::Other,
        });
    }
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: shutter.name.clone().into(),
        manufacturer: Some("Somfy".into()),
        model: Some(shutter.model.clone().into()),
        description: Some(moli_i18n::tr!("pilotes.tahoma.volet").into()),
        native_room: shutter.room.as_deref().map(Into::into),
        members: Vec::new(),
        points,
    }
}

async fn ensure_cookie(
    config: &Config,
    ctx: &mut DriverCtx,
    username: &str,
    password: &str,
    cached: &mut Option<(String, Instant)>,
) -> anyhow::Result<Option<String>> {
    if let Some((cookie, born)) = cached
        && born.elapsed() < SESSION_LIFETIME
    {
        return Ok(Some(cookie.clone()));
    }
    match login(&config.server, username, password).await? {
        LoginOutcome::Cookie(cookie) => {
            *cached = Some((cookie.clone(), Instant::now()));
            Ok(Some(cookie))
        }
        LoginOutcome::Rejected(why) => {
            tracing::warn!(instance = %ctx.instance(), reason = %why, "tahoma login rejected: clearing password from vault");
            ctx.forget_secret(PASSWORD_KEY).await?;
            *cached = None;
            Ok(None)
        }
    }
}

async fn sync_shutters(
    config: &Config,
    ctx: &mut DriverCtx,
    cookie: &str,
    shutters: &mut HashMap<DeviceId, ShutterInfo>,
) -> anyhow::Result<()> {
    let (status, setup) = api_call(&config.server, Method::GET, "/setup", cookie, None).await?;
    if !status.is_success() {
        bail!("/setup: HTTP {}", status.as_u16());
    }
    let discovered = parse_setup(&setup);
    let mut seen = std::collections::HashSet::new();
    for s in discovered {
        let id = ctx.device_id(&sanitize_device_url(&s.device_url));
        ctx.upsert_device(build_device(ctx, &id, &s));
        ctx.set_availability(&id, s.online);
        if let Some(pct) = s.open_percent {
            ctx.set_state(&id, "percent_control", Value::Float(pct));
        }
        if let Some(ctrl) = s.state_control {
            ctx.set_state(&id, "control", Value::Text(ctrl.into()));
        }
        seen.insert(id.clone());
        shutters.insert(id, s);
    }
    shutters.retain(|id, _| seen.contains(id));
    Ok(())
}

async fn register_listener(config: &Config, cookie: &str) -> Option<String> {
    let (status, body) = api_call(
        &config.server,
        Method::POST,
        "/events/register",
        cookie,
        None,
    )
    .await
    .ok()?;
    if !status.is_success() {
        return None;
    }
    body["id"].as_str().map(str::to_owned)
}

async fn fetch_events(
    config: &Config,
    ctx: &mut DriverCtx,
    cookie: &str,
    listener_id: &mut Option<String>,
    shutters: &mut HashMap<DeviceId, ShutterInfo>,
) -> anyhow::Result<()> {
    if listener_id.is_none() {
        *listener_id = register_listener(config, cookie).await;
    }
    let Some(lid) = listener_id.clone() else {
        return sync_shutters(config, ctx, cookie, shutters).await;
    };
    let path = format!("/events/{lid}/fetch");
    let (status, events) = api_call(&config.server, Method::POST, &path, cookie, None).await?;
    if !status.is_success() {
        *listener_id = None;
        return Ok(());
    }
    let Some(arr) = events.as_array() else {
        return Ok(());
    };
    for ev in arr {
        let Some(url) = ev["deviceURL"].as_str() else {
            continue;
        };
        let id = ctx.device_id(&sanitize_device_url(url));
        let Some(shutter) = shutters.get_mut(&id) else {
            continue;
        };
        if let Some(states) = ev["deviceStates"].as_array() {
            let (pct, ctrl) = decode_states(states);
            if let Some(p) = pct {
                shutter.open_percent = Some(p);
                ctx.set_state(&id, "percent_control", Value::Float(p));
            }
            if let Some(c) = ctrl {
                shutter.state_control = Some(c);
                ctx.set_state(&id, "control", Value::Text(c.into()));
            }
        }
    }
    Ok(())
}

pub fn apply_payload(shutter: &ShutterInfo, key: &str, value: &Value) -> anyhow::Result<Json> {
    let cmd_obj = match (key, value) {
        ("control", Value::Text(cmd)) => {
            let name = match &**cmd {
                "open" => shutter.open_cmd.as_str(),
                "close" => shutter.close_cmd.as_str(),
                "stop" => shutter.stop_cmd.as_str(),
                other => bail!("unknown command {other}"),
            };
            json!({ "name": name, "parameters": [] })
        }
        ("percent_control", v) if shutter.has_set_closure => {
            let open_pct = v.as_f64().context("invalid percentage")?.clamp(0.0, 100.0);
            #[allow(clippy::cast_possible_truncation)]
            let closure = (100.0 - open_pct).round() as i64;
            json!({ "name": "setClosure", "parameters": [closure] })
        }
        _ => bail!(moli_i18n::tr!("pilotes.tahoma.point_inconnu", point = key)),
    };
    Ok(json!({
        "label": "Moli OS",
        "actions": [{
            "deviceURL": shutter.device_url,
            "commands": [cmd_obj],
        }]
    }))
}

async fn handle_command(
    config: &Config,
    ctx: &mut DriverCtx,
    cookie: &str,
    shutters: &mut HashMap<DeviceId, ShutterInfo>,
    cmd: CommandRequest,
) {
    let id = cmd.device.id.clone();
    let Some(shutter) = shutters.get_mut(&id) else {
        cmd.reply(Err(moli_i18n::tr!("pilotes.tahoma.volet_inconnu")));
        return;
    };
    let key = cmd.key.clone();
    let value = cmd.value.clone();
    let outcome: anyhow::Result<()> = async {
        let body = apply_payload(shutter, &key, &value)?;
        let (status, ans) = api_call(
            &config.server,
            Method::POST,
            "/exec/apply",
            cookie,
            Some(&body),
        )
        .await?;
        if !status.is_success() {
            bail!("HTTP {} {}", status.as_u16(), ans);
        }
        match (&*key, &value) {
            ("control", Value::Text(c)) => {
                ctx.set_state(&id, "control", Value::Text(c.clone()));
                if &**c == "open" && shutter.has_set_closure {
                    ctx.set_state(&id, "percent_control", Value::Float(100.0));
                } else if &**c == "close" && shutter.has_set_closure {
                    ctx.set_state(&id, "percent_control", Value::Float(0.0));
                }
            }
            ("percent_control", v) => {
                if let Some(pct) = v.as_f64() {
                    ctx.set_state(&id, "percent_control", Value::Float(pct.round()));
                    ctx.set_state(
                        &id,
                        "control",
                        Value::Text(if pct <= 1.0 { "close" } else { "open" }.into()),
                    );
                }
            }
            _ => {}
        }
        Ok(())
    }
    .await;

    match outcome {
        Ok(()) => cmd.reply(Ok(())),
        Err(e) => cmd.reply(Err(moli_i18n::tr!(
            "pilotes.tahoma.echec",
            why = format!("{e:#}")
        ))),
    }
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let (Some(username), Some(password)) = (ctx.secret(USERNAME_KEY), ctx.secret(PASSWORD_KEY))
    else {
        let missing = [USERNAME_KEY, PASSWORD_KEY]
            .into_iter()
            .filter(|k| ctx.secret(k).is_none())
            .collect::<Vec<_>>()
            .join(", ");
        ctx.wait_for(moli_i18n::tr!(
            "pilotes.tahoma.identifiants_manquants",
            instance = ctx.instance(),
            missing = missing
        ));
        ctx.cancelled().await;
        return Ok(());
    };

    let mut cached_cookie: Option<(String, Instant)> = None;
    let Some(initial_cookie) =
        ensure_cookie(config, ctx, &username, &password, &mut cached_cookie).await?
    else {
        ctx.wait_for(moli_i18n::tr!(
            "pilotes.tahoma.identifiants_manquants",
            instance = ctx.instance(),
            missing = PASSWORD_KEY
        ));
        ctx.cancelled().await;
        return Ok(());
    };

    let mut shutters: HashMap<DeviceId, ShutterInfo> = HashMap::new();
    sync_shutters(config, ctx, &initial_cookie, &mut shutters).await?;
    let mut listener_id = register_listener(config, &initial_cookie).await;
    ctx.ready();

    let mut events_tick = tokio::time::interval(POLL_EVENTS);
    events_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    events_tick.tick().await;

    let mut setup_tick = tokio::time::interval(POLL_SETUP);
    setup_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    setup_tick.tick().await;

    loop {
        tokio::select! {
            _ = events_tick.tick() => {
                let Some(cookie) = ensure_cookie(config, ctx, &username, &password, &mut cached_cookie).await? else {
                    return Ok(());
                };
                fetch_events(config, ctx, &cookie, &mut listener_id, &mut shutters).await?;
            }
            _ = setup_tick.tick() => {
                let Some(cookie) = ensure_cookie(config, ctx, &username, &password, &mut cached_cookie).await? else {
                    return Ok(());
                };
                sync_shutters(config, ctx, &cookie, &mut shutters).await?;
            }
            cmd = ctx.next_command() => {
                let Some(cmd) = cmd else { return Ok(()) };
                let Some(cookie) = ensure_cookie(config, ctx, &username, &password, &mut cached_cookie).await? else {
                    cmd.reply(Err(moli_i18n::tr!("pilotes.tahoma.identifiants_manquants", instance = ctx.instance(), missing = PASSWORD_KEY)));
                    return Ok(());
                };
                handle_command(config, ctx, &cookie, &mut shutters, cmd).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_setup_and_builds_commands() {
        let setup: Json = serde_json::from_slice(include_bytes!(
            "../../../integrations/tahoma/fixtures/setup.json"
        ))
        .unwrap();
        let shutters = parse_setup(&setup);
        assert_eq!(shutters.len(), 2);
        assert_eq!(shutters[0].name, "Volet Salon");
        assert_eq!(shutters[0].room.as_deref(), Some("Salon"));
        assert!(shutters[0].has_set_closure);
        assert_eq!(shutters[0].open_percent, Some(75.0));
        assert_eq!(shutters[0].state_control, Some("open"));

        assert_eq!(shutters[1].name, "Volet Cuisine");
        assert!(!shutters[1].has_set_closure);
        assert_eq!(shutters[1].stop_cmd, "my");

        let payload = apply_payload(&shutters[0], "percent_control", &Value::Float(60.0)).unwrap();
        assert_eq!(payload["actions"][0]["commands"][0]["name"], "setClosure");
        assert_eq!(payload["actions"][0]["commands"][0]["parameters"][0], 40);
    }
}
