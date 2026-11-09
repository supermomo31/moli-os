//! Shelly Gen1 driver: the first-generation HTTP API (`/status`,
//! `/shelly`, `/relay/<n>?turn=`), not the Gen2+ websocket JSON-RPC
//! that `moli-shelly` speaks.
//!
//! Every entry of `devices` (`"192.168.1.x"` or `"192.168.1.x:port"`)
//! becomes a Moli device. Relay `i` exposes `state` (or `state_<i+1>`
//! past the first); a relay with a meter also exposes `power`,
//! `energy`, `voltage` and `current` (suffixed the same way).
//! State is polled every `poll_secs` seconds with `GET /status`.

use std::time::Duration;

use anyhow::Context as _;
use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_net::{Method, plain};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;
use serde_json::Value as Json;

/// Driver configuration (`[driver.options]` in `moli.toml`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Device addresses: `"192.168.1.x"` or `"192.168.1.x:8080"`.
    #[serde(default)]
    pub devices: Vec<String>,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Seconds between two `GET /status` polls of every device.
    #[serde(default = "default_poll")]
    pub poll_secs: u64,
}

fn default_port() -> u16 {
    80
}
fn default_poll() -> u64 {
    30
}

#[derive(Debug)]
pub struct Shelly1 {
    config: Config,
}

impl Shelly1 {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Shelly1 {
    fn kind(&self) -> &'static str {
        "shelly1"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

/// One Shelly Gen1 device as Moli knows it.
#[derive(Debug)]
struct Node {
    /// `"host"` or `"host:port"` as configured.
    address: String,
    host: String,
    port: u16,
    device: Device,
    /// `type` from `/shelly` (`"SHSW-1"`…), empty until known.
    model: String,
    /// `hostname` from `/settings`, empty until known.
    name: String,
    /// Relay count learned from `/status`, 0 until the first poll.
    relays: usize,
    /// Meter fields reported per relay (`power`, `energy`…), parallel to
    /// `relays`: a SHSW-1 reports `power` only, a `-PM` reports more.
    meter_keys: Vec<std::collections::BTreeSet<String>>,
}

impl Node {
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
            manufacturer: Some("Shelly".into()),
            model: None,
            description: Some(moli_i18n::tr!("pilotes.shelly1.description").into()),
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
            relays: 0,
            meter_keys: Vec::new(),
        }
    }

    /// Point key for relay `i` (`state`, `state_2`, …) and its meter.
    fn keys(i: usize) -> (String, String, String, String, String) {
        if i == 0 {
            (
                "state".to_owned(),
                "power".to_owned(),
                "energy".to_owned(),
                "voltage".to_owned(),
                "current".to_owned(),
            )
        } else {
            let n = i + 1;
            (
                format!("state_{n}"),
                format!("power_{n}"),
                format!("energy_{n}"),
                format!("voltage_{n}"),
                format!("current_{n}"),
            )
        }
    }

    /// Point label: the relay number for multi-relay devices.
    fn label(i: usize, base: &str) -> String {
        if i == 0 {
            moli_i18n::tr!(base).to_owned()
        } else {
            format!("{} {}", moli_i18n::tr!(base), i + 1)
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
        let mut points = Vec::new();
        for i in 0..self.relays.max(1) {
            let (state, power, energy, voltage, current) = Self::keys(i);
            points.push(PointSpec {
                key: state.into(),
                label: Self::label(i, "pilotes.shelly1.point.relais").into(),
                kind: Kind::Binary,
                access: rw,
                unit: None,
                semantic: Semantic::OnOff,
            });
            if let Some(keys) = self.meter_keys.get(i) {
                let numeric = |key: String, base: &str, unit: Unit, semantic: Semantic| PointSpec {
                    key: key.into(),
                    label: Self::label(i, base).into(),
                    kind: Kind::Numeric {
                        min: None,
                        max: None,
                        step: None,
                    },
                    access: rw,
                    unit: Some(unit),
                    semantic,
                };
                // Power, energy and voltage are read-only telemetry on
                // Gen1; only the relay is writable.
                for (key, field, base, unit, semantic) in [
                    (
                        power,
                        "power",
                        "pilotes.shelly1.point.puissance",
                        Unit::Watt,
                        Semantic::Power,
                    ),
                    (
                        energy,
                        "energy",
                        "pilotes.shelly1.point.energie",
                        Unit::WattHour,
                        Semantic::Energy,
                    ),
                    (
                        voltage,
                        "voltage",
                        "pilotes.shelly1.point.tension",
                        Unit::Volt,
                        Semantic::Voltage,
                    ),
                    (
                        current,
                        "current",
                        "pilotes.shelly1.point.courant",
                        Unit::Ampere,
                        Semantic::Current,
                    ),
                ] {
                    if !keys.contains(field) {
                        continue;
                    }
                    let mut spec = numeric(key, base, unit, semantic);
                    spec.access.write = false;
                    points.push(spec);
                }
            }
        }
        points.push(PointSpec {
            key: "model".into(),
            label: moli_i18n::tr!("pilotes.shelly1.point.modele").into(),
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
        let name = if self.name.is_empty() {
            self.address.clone()
        } else {
            self.name.clone()
        };
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

/// One GET on the device (`/status`, `/shelly`…).
async fn get(node: &Node, path: &str) -> anyhow::Result<Json> {
    let builder = http::Request::builder()
        .method(Method::GET)
        .uri(path)
        .header(
            "host",
            if node.port == 80 {
                node.host.clone()
            } else {
                format!("{}:{}", node.host, node.port)
            },
        );
    let request = moli_net::empty(builder)?;
    let (status, body) = plain(&node.host, node.port, request).await?;
    if !status.is_success() {
        anyhow::bail!("shelly1 {} -> {status}", node.address);
    }
    serde_json::from_slice(&body).context("bad shelly1 answer")
}

/// Meter fields a Gen1 `/status` may report per relay.
const METER_FIELDS: [&str; 4] = ["power", "energy", "voltage", "current"];

/// Applies a `/status` answer to the published state.
fn apply_status(node: &mut Node, status: &Json, ctx: &DriverCtx) {
    let relays = status.get("relays").and_then(Json::as_array);
    let count = relays.map_or(0, Vec::len);
    if count > 0 {
        node.relays = count;
    }
    let meters = status.get("meters").and_then(Json::as_array);
    node.meter_keys = (0..node.relays)
        .map(|i| {
            meters
                .and_then(|m| m.get(i))
                .map(|meter| {
                    METER_FIELDS
                        .iter()
                        .copied()
                        .filter(|f| meter.get(f).is_some_and(Json::is_number))
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect();
    for i in 0..node.relays {
        let (state, power, energy, voltage, current) = Node::keys(i);
        let device = node.device.id.clone();
        if let Some(on) = relays
            .and_then(|r| r.get(i))
            .and_then(|r| r.get("ison"))
            .and_then(Json::as_bool)
        {
            ctx.set_state(&device, &state, Value::Bool(on));
        }
        if let Some(meter) = meters.and_then(|m| m.get(i)) {
            for (key, field) in [
                (power.as_str(), "power"),
                (energy.as_str(), "energy"),
                (voltage.as_str(), "voltage"),
                (current.as_str(), "current"),
            ] {
                if let Some(v) = meter.get(field).and_then(Json::as_f64) {
                    ctx.set_state(&device, key, Value::Float(v));
                }
            }
        }
    }
}

/// Applies a `/shelly` answer (`type`) and `/settings` (`hostname`).
async fn apply_info(node: &mut Node, ctx: &DriverCtx) {
    if let Ok(info) = get(node, "/shelly").await
        && let Some(model) = info.get("type").and_then(Json::as_str)
        && !model.is_empty()
        && node.model != model
    {
        node.model.clear();
        node.model.push_str(model);
    }
    if let Ok(settings) = get(node, "/settings").await
        && let Some(name) = settings
            .get("device")
            .and_then(|d| d.get("hostname"))
            .and_then(Json::as_str)
        && !name.is_empty()
        && node.name != name
    {
        node.name.clear();
        node.name.push_str(name);
    }
    ctx.set_state(
        &node.device.id,
        "model",
        Value::Text(node.model.clone().into()),
    );
}

/// Polls every device once.
async fn poll_all(ctx: &DriverCtx, nodes: &mut [Node]) {
    for node in nodes {
        match get(node, "/status").await {
            Ok(status) => {
                apply_status(node, &status, ctx);
                if node.refresh_device() {
                    ctx.upsert_device(node.device.clone());
                }
                ctx.set_availability(&node.device.id, true);
            }
            Err(e) => {
                tracing::debug!(address = %node.address, error = %format!("{e:#}"), "shelly1 poll failed");
                ctx.set_availability(&node.device.id, false);
            }
        }
        apply_info(node, ctx).await;
    }
}

/// Sends a relay command, then refreshes the published state.
async fn command_node(
    node: &mut Node,
    relay: usize,
    turn: &str,
    ctx: &DriverCtx,
) -> Result<(), String> {
    let request = http::Request::builder()
        .method(Method::GET)
        .uri(format!("/relay/{relay}?turn={turn}"))
        .header(
            "host",
            if node.port == 80 {
                node.host.clone()
            } else {
                format!("{}:{}", node.host, node.port)
            },
        );
    let request = match moli_net::empty(request) {
        Ok(r) => r,
        Err(e) => return Err(format!("{e:#}")),
    };
    match plain(&node.host, node.port, request).await {
        Ok((status, _)) if status.is_success() => {}
        Ok((status, _)) => return Err(format!("shelly1 {} -> {status}", node.address)),
        Err(e) => return Err(format!("{e:#}")),
    }
    if let Ok(status) = get(node, "/status").await {
        apply_status(node, &status, ctx);
        if node.refresh_device() {
            ctx.upsert_device(node.device.clone());
        }
    }
    Ok(())
}

async fn on_command(nodes: &mut [Node], ctx: &DriverCtx, command: CommandRequest) {
    let Some(node) = nodes.iter_mut().find(|n| n.device.id == command.device.id) else {
        command.reply(Err(moli_i18n::tr!("pilotes.shelly1.appareil_inconnu")));
        return;
    };
    let key: &str = &command.key;
    // `state`, `state_2`… -> relay index; the meter points are read-only.
    let relay = if key == "state" {
        Some(0)
    } else {
        key.strip_prefix("state_")
            .and_then(|n| n.parse::<usize>().ok())
            .and_then(|n| (n >= 2).then_some(n - 1))
    };
    let result = match (relay, &command.value) {
        (Some(i), Value::Bool(on)) if i < node.relays.max(1) => {
            command_node(node, i, if *on { "on" } else { "off" }, ctx).await
        }
        (Some(_), _) => Err(moli_i18n::tr!("pilotes.shelly1.relais_inconnu")),
        (None, _) => Err(moli_i18n::tr!("pilotes.shelly1.lecture_seule")),
    };
    command.reply(result);
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let mut nodes: Vec<Node> = config
        .devices
        .iter()
        .map(|address| Node::new(ctx.instance(), address, config.port))
        .collect();
    for node in &nodes {
        ctx.upsert_device(node.device.clone());
    }
    let mut poll = tokio::time::interval(Duration::from_secs(config.poll_secs.max(5)));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ctx.ready();
    loop {
        tokio::select! {
            _ = poll.tick() => poll_all(ctx, &mut nodes).await,
            command = ctx.next_command() => match command {
                Some(command) => on_command(&mut nodes, ctx, command).await,
                None => return Ok(()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_relay_gets_plain_keys() {
        assert_eq!(Node::keys(0).0, "state");
        assert_eq!(Node::keys(0).1, "power");
    }

    #[test]
    fn extra_relays_are_suffixed() {
        assert_eq!(Node::keys(1).0, "state_2");
        assert_eq!(Node::keys(2).0, "state_3");
        assert_eq!(Node::keys(1).1, "power_2");
    }

    #[test]
    fn addresses_split_host_and_port() {
        let n = Node::new(&InstanceId::from("shelly1"), "192.168.1.y:8080", 80);
        assert_eq!(n.host, "192.168.1.y");
        assert_eq!(n.port, 8080);
        let n = Node::new(&InstanceId::from("shelly1"), "192.168.1.z", 80);
        assert_eq!(n.host, "192.168.1.z");
        assert_eq!(n.port, 80);
    }
}
