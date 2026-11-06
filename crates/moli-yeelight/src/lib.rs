//! Yeelight driver: Wi-Fi bulbs controlled over the LAN protocol
//! (TCP 55443, one JSON command per line), no cloud account involved.
//!
//! Every entry of `devices` (`"192.168.1.x"` or `"192.168.1.x:55443"`)
//! becomes a Moli device with `on`, `brightness`, `color_temp` and —
//! on colour models — `color` (« #rrggbb ») points. State is polled
//! every `poll_secs` seconds with `get_prop`.

use std::time::Duration;

use anyhow::Context as _;
use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// Driver configuration (`[driver.options]` in `moli.toml`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Bulb addresses: `"192.168.1.x"` or `"192.168.1.x:55443"`.
    #[serde(default)]
    pub devices: Vec<String>,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Seconds between two `get_prop` polls of every bulb.
    #[serde(default = "default_poll")]
    pub poll_secs: u64,
}

fn default_port() -> u16 {
    55443
}
fn default_poll() -> u64 {
    30
}

#[derive(Debug)]
pub struct Yeelight {
    config: Config,
}

impl Yeelight {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Yeelight {
    fn kind(&self) -> &'static str {
        "yeelight"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

/// One bulb as Moli knows it.
#[derive(Debug)]
struct Bulb {
    /// `"host"` or `"host:port"` as configured.
    address: String,
    host: String,
    port: u16,
    device: Device,
    /// `model` prop (`"color"`, `"stripe"`, `"mono6"`…), empty until known.
    model: String,
    /// `name` prop, empty until the bulb reports one.
    name: String,
}

impl Bulb {
    fn new(instance: &InstanceId, address: &str, default_port: u16) -> Self {
        let (host, port) = match address.rsplit_once(':') {
            Some((h, p)) => match p.parse::<u16>() {
                Ok(port) => (h.to_owned(), port),
                Err(_) => (address.to_owned(), default_port),
            },
            None => (address.to_owned(), default_port),
        };
        let device = Device {
            id: DeviceId::new(instance, address),
            instance: instance.clone(),
            native_name: address.into(),
            manufacturer: Some("Yeelight".into()),
            model: None,
            description: Some(moli_i18n::tr!("pilotes.yeelight.description").into()),
            native_room: None,
            members: Vec::new(),
            points: Vec::new(),
        };
        Self {
            address: address.to_owned(),
            host,
            port,
            device,
            model: String::new(),
            name: String::new(),
        }
    }

    /// Whether this model can show colours (`mono*`/`temp*` cannot).
    fn color_capable(&self) -> bool {
        !self.model.starts_with("mono") && !self.model.starts_with("temp")
    }

    /// The display name: the bulb's own `name`, else its model, else address.
    fn display_name(&self) -> String {
        if !self.name.is_empty() {
            self.name.clone()
        } else if !self.model.is_empty() {
            self.model.clone()
        } else {
            self.address.clone()
        }
    }

    fn points(&self) -> Vec<PointSpec> {
        let rw = Access {
            read: true,
            write: true,
        };
        let ro = Access {
            read: true,
            write: false,
        };
        let numeric =
            |key: &str, label: String, min: f64, max: f64, unit: Unit, semantic: Semantic| {
                PointSpec {
                    key: key.into(),
                    label: label.into(),
                    kind: Kind::Numeric {
                        min: Some(min),
                        max: Some(max),
                        step: None,
                    },
                    access: rw,
                    unit: Some(unit),
                    semantic,
                }
            };
        let mut points = vec![
            PointSpec {
                key: "on".into(),
                label: moli_i18n::tr!("pilotes.yeelight.point.etat").into(),
                kind: Kind::Binary,
                access: rw,
                unit: None,
                semantic: Semantic::OnOff,
            },
            numeric(
                "brightness",
                moli_i18n::tr!("pilotes.yeelight.point.luminosite"),
                1.0,
                100.0,
                Unit::Percent,
                Semantic::Brightness,
            ),
            numeric(
                "color_temp",
                moli_i18n::tr!("pilotes.yeelight.point.temperature_couleur"),
                1700.0,
                6500.0,
                Unit::Other("K".into()),
                Semantic::ColorTemp,
            ),
        ];
        if self.color_capable() {
            points.push(PointSpec {
                key: "color".into(),
                label: moli_i18n::tr!("pilotes.yeelight.point.couleur").into(),
                kind: Kind::Text,
                access: rw,
                unit: None,
                semantic: Semantic::Color,
            });
        }
        // `model` is read-only information for the dashboard.
        points.push(PointSpec {
            key: "model".into(),
            label: moli_i18n::tr!("pilotes.yeelight.point.modele").into(),
            kind: Kind::Text,
            access: ro,
            unit: None,
            semantic: Semantic::Other,
        });
        points
    }

    /// Rebuilds `device` from the current name/model/points; `true` when
    /// the published device changed.
    fn refresh_device(&mut self) -> bool {
        let points = self.points();
        let name = self.display_name();
        let model = (!self.model.is_empty()).then_some(self.model.as_str());
        let changed = self.device.points != points
            || self.device.native_name.as_ref() != name
            || self.device.model.as_deref() != model;
        if changed {
            self.device.points = points;
            self.device.native_name = name.into();
            self.device.model = model.map(Into::into);
        }
        changed
    }
}

/// `get_prop` probe: returns the `result` array, positionally aligned
/// with the requested properties.
const PROPS: [&str; 9] = [
    "power",
    "bright",
    "ct",
    "rgb",
    "name",
    "model",
    "color_mode",
    "hue",
    "sat",
];

/// One request on a bulb: connect, send, read the answer line.
async fn request(bulb: &Bulb, method: &str, params: Json) -> anyhow::Result<Json> {
    let mut stream = TcpStream::connect((bulb.host.as_str(), bulb.port))
        .await
        .with_context(|| format!("yeelight {} unreachable", bulb.address))?;
    let request = json!({"id": 1, "method": method, "params": params});
    stream.write_all(request.to_string().as_bytes()).await?;
    stream.write_all(b"\r\n").await?;
    stream.flush().await?;
    let mut lines = BufReader::new(stream).lines();
    let line = lines
        .next_line()
        .await?
        .context("yeelight closed without answering")?;
    let answer: Json = serde_json::from_str(line.trim()).context("bad yeelight answer")?;
    if let Some(error) = answer.get("error") {
        anyhow::bail!("yeelight {method}: {error}");
    }
    Ok(answer["result"].clone())
}

/// Applies a `get_prop` result array to the bulb's published state.
fn apply_props(bulb: &mut Bulb, result: &Json, ctx: &DriverCtx) {
    let Some(values) = result.as_array() else {
        return;
    };
    let get = |i: usize| values.get(i).cloned().unwrap_or(Json::Null);
    let device = bulb.device.id.clone();
    ctx.set_state(&device, "on", Value::Bool(get(0).as_str() == Some("on")));
    if let Some(v) = get(1).as_str().and_then(|s| s.parse::<f64>().ok()) {
        ctx.set_state(&device, "brightness", Value::Float(v));
    }
    if let Some(v) = get(2).as_str().and_then(|s| s.parse::<f64>().ok()) {
        ctx.set_state(&device, "color_temp", Value::Float(v));
    }
    if let Some(color) = reported_color(&get) {
        ctx.set_state(&device, "color", Value::Text(color.into()));
    }
    let name_value = get(4);
    let name = name_value.as_str().unwrap_or_default();
    if !name.is_empty() && bulb.name != name {
        bulb.name.clear();
        bulb.name.push_str(name);
    }
    let model_value = get(5);
    let model = model_value.as_str().unwrap_or_default();
    if !model.is_empty() && bulb.model != model {
        bulb.model.clear();
        bulb.model.push_str(model);
    }
    ctx.set_state(&device, "model", Value::Text(bulb.model.clone().into()));
}

/// Polls every bulb once.
async fn poll_all(ctx: &DriverCtx, bulbs: &mut [Bulb]) {
    for bulb in bulbs {
        match request(bulb, "get_prop", json!(PROPS)).await {
            Ok(result) => {
                apply_props(bulb, &result, ctx);
                if bulb.refresh_device() {
                    ctx.upsert_device(bulb.device.clone());
                }
                ctx.set_availability(&bulb.device.id, true);
            }
            Err(e) => {
                tracing::debug!(address = %bulb.address, error = %format!("{e:#}"), "yeelight poll failed");
                ctx.set_availability(&bulb.device.id, false);
            }
        }
    }
}

/// Sends a command to a bulb, then refreshes its published state.
async fn command_bulb(
    bulb: &mut Bulb,
    method: &str,
    params: Json,
    ctx: &DriverCtx,
) -> Result<(), String> {
    request(bulb, method, params)
        .await
        .map_err(|e| format!("{e:#}"))?;
    if let Ok(result) = request(bulb, "get_prop", json!(PROPS)).await {
        apply_props(bulb, &result, ctx);
        if bulb.refresh_device() {
            ctx.upsert_device(bulb.device.clone());
        }
    }
    Ok(())
}

async fn on_command(bulbs: &mut [Bulb], ctx: &DriverCtx, command: CommandRequest) {
    let Some(bulb) = bulbs.iter_mut().find(|b| b.device.id == command.device.id) else {
        command.reply(Err(moli_i18n::tr!("pilotes.yeelight.appareil_inconnu")));
        return;
    };
    let result = match (&*command.key, &command.value) {
        ("on", Value::Bool(on)) => {
            command_bulb(
                bulb,
                "set_power",
                json!([if *on { "on" } else { "off" }]),
                ctx,
            )
            .await
        }
        ("on", Value::Text(text)) => match text.to_ascii_lowercase().as_str() {
            "on" | "off" => {
                command_bulb(bulb, "set_power", json!([text.to_ascii_lowercase()]), ctx).await
            }
            "toggle" => command_bulb(bulb, "toggle", json!([]), ctx).await,
            _ => Err(moli_i18n::tr!("pilotes.yeelight.valeur_invalide")),
        },
        ("brightness", Value::Int(v)) => {
            let v = (*v).clamp(1, 100);
            command_bulb(bulb, "set_bright", json!([v]), ctx).await
        }
        ("brightness", Value::Float(v)) => {
            #[allow(clippy::cast_possible_truncation)]
            let v = v.clamp(1.0, 100.0).round() as i64;
            command_bulb(bulb, "set_bright", json!([v]), ctx).await
        }
        ("color_temp", Value::Int(v)) => {
            let v = (*v).clamp(1700, 6500);
            command_bulb(bulb, "set_ct_abx", json!([v, "sudden", 0]), ctx).await
        }
        ("color_temp", Value::Float(v)) => {
            #[allow(clippy::cast_possible_truncation)]
            let v = v.clamp(1700.0, 6500.0).round() as i64;
            command_bulb(bulb, "set_ct_abx", json!([v, "sudden", 0]), ctx).await
        }
        ("color", Value::Text(text)) => {
            let Some(rgb) = parse_hex(text) else {
                command.reply(Err(moli_i18n::tr!("pilotes.yeelight.couleur_invalide")));
                return;
            };
            command_bulb(
                bulb,
                "set_rgb",
                json!([(rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff]),
                ctx,
            )
            .await
        }
        _ => Err(moli_i18n::tr!("pilotes.yeelight.lecture_seule")),
    };
    command.reply(result);
}

/// `"#rrggbb"` from the `get_prop` fields, honouring `color_mode`:
/// `1` = packed RGB, `2` = colour temperature (the `rgb` prop is stale
/// then), `3` = HSV. Anything else falls back to the packed `rgb`.
fn reported_color(get: &dyn Fn(usize) -> Json) -> Option<String> {
    let mode_value = get(6);
    let mode = mode_value.as_str().unwrap_or_default();
    let rgb = match mode {
        "2" => kelvin_rgb(get(2).as_str()?.parse::<f64>().ok()?)?,
        "3" => {
            let h = get(7).as_str()?.parse::<f64>().ok()?;
            let s = get(8).as_str()?.parse::<f64>().ok()? / 100.0;
            // `bright` is a percentage; HSV value uses the same scale.
            let v = get(1).as_str()?.parse::<f64>().ok()? / 100.0;
            hsv_rgb(h, s, v)?
        }
        _ => get(3).as_str()?.parse::<u32>().ok()?,
    };
    Some(format!("#{rgb:06x}"))
}

/// Approximate sRGB of a blackbody temperature (Tanner Helland fit).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names
)]
fn kelvin_rgb(ct: f64) -> Option<u32> {
    if !(1000.0..=40_000.0).contains(&ct) {
        return None;
    }
    let t = ct / 100.0;
    let r = if t <= 66.0 {
        255.0
    } else {
        329.698_73 * (t - 60.0).powf(-0.133_2)
    };
    let g = if t <= 66.0 {
        99.470_8 * t.ln() - 161.119_6
    } else {
        288.122_2 * (t - 60.0).powf(-0.075_5)
    };
    let b = if t >= 66.0 {
        255.0
    } else if t <= 19.0 {
        0.0
    } else {
        138.517_7 * (t - 10.0).ln() - 305.044_8
    };
    let channel = |c: f64| c.round().clamp(0.0, 255.0) as u32;
    Some((channel(r) << 16) | (channel(g) << 8) | channel(b))
}

/// RGB packed integer from hue (0–360), saturation and value (0–1).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names
)]
fn hsv_rgb(h: f64, s: f64, v: f64) -> Option<u32> {
    if !(0.0..=1.0).contains(&s) || !(0.0..=1.0).contains(&v) {
        return None;
    }
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let channel = |f: f64| ((f + m) * 255.0).round().clamp(0.0, 255.0) as u32;
    Some((channel(r) << 16) | (channel(g) << 8) | channel(b))
}

/// `"#rrggbb"` or `"rrggbb"` -> the packed RGB integer.
fn parse_hex(text: &str) -> Option<u32> {
    let hex = text.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    u32::from_str_radix(hex, 16).ok()
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let mut bulbs: Vec<Bulb> = config
        .devices
        .iter()
        .map(|address| Bulb::new(ctx.instance(), address, config.port))
        .collect();
    for bulb in &bulbs {
        ctx.upsert_device(bulb.device.clone());
    }
    let mut poll = tokio::time::interval(Duration::from_secs(config.poll_secs.max(5)));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ctx.ready();
    loop {
        tokio::select! {
            _ = poll.tick() => poll_all(ctx, &mut bulbs).await,
            command = ctx.next_command() => match command {
                Some(command) => on_command(&mut bulbs, ctx, command).await,
                None => return Ok(()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bulb(model: &str) -> Bulb {
        let mut b = Bulb::new(&InstanceId::from("yeelight"), "192.168.1.x", 55443);
        b.model = model.to_owned();
        b
    }

    #[test]
    fn colour_models_get_a_colour_point() {
        for model in ["color", "stripe", "ceiling"] {
            let keys: Vec<_> = bulb(model)
                .points()
                .iter()
                .map(|p| p.key.to_string())
                .collect();
            assert!(keys.contains(&"color".to_owned()), "{model}");
        }
        for model in ["mono6", "mono12", "temp"] {
            let keys: Vec<_> = bulb(model)
                .points()
                .iter()
                .map(|p| p.key.to_string())
                .collect();
            assert!(!keys.contains(&"color".to_owned()), "{model}");
        }
    }

    #[test]
    fn every_bulb_has_the_base_points() {
        let keys: Vec<_> = bulb("color")
            .points()
            .iter()
            .map(|p| p.key.to_string())
            .collect();
        for expected in ["on", "brightness", "color_temp", "model"] {
            assert!(keys.contains(&expected.to_owned()), "missing {expected}");
        }
    }

    #[test]
    fn hex_colours_parse() {
        assert_eq!(parse_hex("#ff8000"), Some(0xff_8000));
        assert_eq!(parse_hex("00ff00"), Some(0x0000_ff00));
        assert_eq!(parse_hex("#fff"), None);
        assert_eq!(parse_hex("red"), None);
    }

    #[test]
    fn kelvin_colours_are_warm_or_cool() {
        // 2700 K ≈ warm white, 6500 K ≈ daylight: red grows, blue shrinks.
        let warm = kelvin_rgb(2700.0).unwrap();
        let cool = kelvin_rgb(6500.0).unwrap();
        // Warm white is redder, cool daylight is bluer.
        assert!(warm & 0xff < cool & 0xff, "{warm:#x} vs {cool:#x}");
        assert!(warm >> 16 >= cool >> 16, "{warm:#x} vs {cool:#x}");
        assert!(kelvin_rgb(0.0).is_none());
    }

    #[test]
    fn hsv_colours_match_primaries() {
        assert_eq!(hsv_rgb(0.0, 1.0, 1.0), Some(0xff_0000));
        assert_eq!(hsv_rgb(120.0, 1.0, 1.0), Some(0x00_ff00));
        assert_eq!(hsv_rgb(240.0, 1.0, 1.0), Some(0x00_00_ff));
        assert_eq!(hsv_rgb(0.0, 2.0, 1.0), None);
    }

    #[test]
    fn colour_follows_the_reported_mode() {
        let fields = |mode: &str, rgb: &str, ct: &str, hue: &str, sat: &str, bright: &str| {
            let values = [
                json!("on"),
                json!(bright),
                json!(ct),
                json!(rgb),
                json!(""),
                json!(""),
                json!(mode),
                json!(hue),
                json!(sat),
            ];
            let get = |i: usize| values.get(i).cloned().unwrap_or(Json::Null);
            reported_color(&get)
        };
        // Colour mode: the packed rgb prop wins.
        assert_eq!(
            fields("1", "16711680", "4000", "0", "100", "50"),
            Some("#ff0000".to_owned())
        );
        // Temperature mode: rgb is meaningless, kelvin wins.
        let warm = fields("2", "255", "2700", "240", "100", "50").unwrap();
        assert!(warm.starts_with('#'), "{warm}");
        assert_ne!(warm, "#0000ff");
        // HSV mode.
        assert_eq!(
            fields("3", "0", "4000", "120", "100", "100"),
            Some("#00ff00".to_owned())
        );
    }

    #[test]
    fn addresses_split_host_and_port() {
        let b = Bulb::new(&InstanceId::from("yeelight"), "192.168.1.x:55443", 55443);
        assert_eq!(b.host, "192.168.1.x");
        assert_eq!(b.port, 55443);
        let b = Bulb::new(&InstanceId::from("yeelight"), "192.168.1.y", 55443);
        assert_eq!(b.host, "192.168.1.y");
        assert_eq!(b.port, 55443);
    }
}
