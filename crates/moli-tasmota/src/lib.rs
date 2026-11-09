//! Tasmota driver (Sonoff and other ESP8266/ESP32 flashed with Tasmota).
//!
//! Tasmota has no device catalog like Zigbee2MQTT's `bridge/devices`: every
//! device publishes its own `tele/<hostname>/STATE` and
//! `tele/<hostname>/SENSOR` messages, its retained birth message on
//! `stat/<hostname>/LWT`, and answers `cmnd/<hostname>/…` orders. This
//! driver subscribes to `tele/#` and `stat/#` on the broker: each Tasmota
//! device appears on its own, every `POWER<n>` relay becomes a Moli device
//! of its own with a writable `state` point (a one-relay device keeps the
//! hostname as its id), its power monitoring becomes read-only
//! `power`/`energy`/`voltage`/`current` points on the same device, and the
//! probes reported in `SENSOR` become sensor points on the host device.
//!
//! Only the default `tele`/`stat`/`cmnd` roots are spoken; an optional
//! `devices` list restricts which hostnames become Moli devices.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::Duration;

use anyhow::Context as _;
use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, Publish, QoS};
use serde::Deserialize;
use serde_json::Value as Json;

/// Driver configuration (`[driver.options]` in `moli.toml`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub username: Option<String>,
    /// Name of the environment variable holding the password (never the
    /// password itself).
    #[serde(default)]
    pub password_env: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    /// Tasmota hostnames to expose; `None` (the default): every device
    /// publishing under `tele/` and `stat/` on this broker.
    #[serde(default)]
    pub devices: Option<Vec<String>>,
}

fn default_host() -> String {
    "127.0.0.1".into()
}
fn default_port() -> u16 {
    1883
}

#[derive(Debug)]
pub struct Tasmota {
    config: Config,
}

impl Tasmota {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Tasmota {
    fn kind(&self) -> &'static str {
        "tasmota"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

/// An electrical measurement a `POWER<n>` object of `SENSOR` carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ChannelMetric {
    Power,
    Energy,
    ApparentPower,
    Voltage,
    Current,
}

impl ChannelMetric {
    fn base(self) -> &'static str {
        match self {
            Self::Power => "power",
            Self::Energy => "energy",
            Self::ApparentPower => "apparent_power",
            Self::Voltage => "voltage",
            Self::Current => "current",
        }
    }

    fn label_key(self) -> &'static str {
        match self {
            Self::Power => "pilotes.tasmota.point.puissance",
            Self::Energy => "pilotes.tasmota.point.energie",
            Self::ApparentPower => "pilotes.tasmota.point.puissance_app",
            Self::Voltage => "pilotes.tasmota.point.tension",
            Self::Current => "pilotes.tasmota.point.courant",
        }
    }

    fn unit(self) -> Unit {
        match self {
            Self::Power => Unit::Watt,
            Self::Energy => Unit::KiloWattHour,
            Self::ApparentPower => Unit::VoltAmpere,
            Self::Voltage => Unit::Volt,
            Self::Current => Unit::Ampere,
        }
    }

    fn semantic(self) -> Semantic {
        match self {
            Self::Power => Semantic::Power,
            Self::Energy => Semantic::Energy,
            Self::ApparentPower => Semantic::ApparentPower,
            Self::Voltage => Semantic::Voltage,
            Self::Current => Semantic::Current,
        }
    }
}

/// An environmental measurement a named `SENSOR` object carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum EnvMetric {
    Temperature,
    Humidity,
    Pressure,
    Illuminance,
    Battery,
}

impl EnvMetric {
    fn base(self) -> &'static str {
        match self {
            Self::Temperature => "temperature",
            Self::Humidity => "humidity",
            Self::Pressure => "pressure",
            Self::Illuminance => "illuminance",
            Self::Battery => "battery",
        }
    }

    fn label_key(self) -> &'static str {
        match self {
            Self::Temperature => "pilotes.tasmota.point.temperature",
            Self::Humidity => "pilotes.tasmota.point.humidite",
            Self::Pressure => "pilotes.tasmota.point.pression",
            Self::Illuminance => "pilotes.tasmota.point.luminance",
            Self::Battery => "pilotes.tasmota.point.batterie",
        }
    }

    fn unit(self) -> Unit {
        match self {
            Self::Temperature => Unit::Celsius,
            Self::Humidity | Self::Battery => Unit::Percent,
            Self::Pressure => Unit::HectoPascal,
            Self::Illuminance => Unit::Lux,
        }
    }

    fn semantic(self) -> Semantic {
        match self {
            Self::Temperature => Semantic::Temperature,
            Self::Humidity => Semantic::Humidity,
            Self::Pressure => Semantic::Pressure,
            Self::Illuminance => Semantic::Illuminance,
            Self::Battery => Semantic::Battery,
        }
    }
}

/// What one `SENSOR` message carried, ready to merge.
#[derive(Default)]
struct SensorData {
    /// Channel -> metric -> value.
    channels: BTreeMap<u8, BTreeMap<ChannelMetric, f64>>,
    /// Sensor slug -> metric -> value.
    env: BTreeMap<String, BTreeMap<EnvMetric, f64>>,
    /// Legacy `ENERGY.Total` (kWh), whole-device cumulative counter.
    legacy_energy: Option<f64>,
    /// `Wifi.RSSI` (dBm).
    rssi: Option<f64>,
}

/// What one Tasmota host reported, before it becomes Moli devices.
#[derive(Debug, Default)]
struct Host {
    /// Relay channel -> its `POWER`/`POWER<n>` key on the wire.
    channels: BTreeMap<u8, String>,
    /// Last reported ON/OFF per channel.
    relay: BTreeMap<u8, bool>,
    /// Channel -> its latest electrical measurements.
    channel_metrics: BTreeMap<u8, BTreeMap<ChannelMetric, f64>>,
    /// Environmental probes: slug -> metric -> value.
    env: BTreeMap<String, BTreeMap<EnvMetric, f64>>,
    legacy_energy: Option<f64>,
    rssi: Option<f64>,
}

/// A Moli device a host maps to: the host itself (a single-relay device,
/// or the shared sensors of a multi-channel one) or one relay channel.
#[derive(Debug)]
struct Entry {
    /// Registry key: the hostname, or `{hostname}/c{channel}`.
    key: String,
    device: Device,
    hostname: String,
    /// `Some(channel)` when the device drives one relay.
    channel: Option<u8>,
}

impl Host {
    /// More than one relay: each channel is a device of its own and the
    /// host device keeps only the shared sensors.
    fn multi(&self) -> bool {
        self.channels.len() > 1
    }

    fn device(
        instance: &InstanceId,
        native: &str,
        name: String,
        points: Vec<PointSpec>,
    ) -> Device {
        Device {
            id: DeviceId::new(instance, native),
            instance: instance.clone(),
            native_name: name.into(),
            manufacturer: None,
            model: None,
            description: Some(moli_i18n::tr!("pilotes.tasmota.description").into()),
            native_room: None,
            members: Vec::new(),
            points,
        }
    }

    /// A read-only numeric point.
    fn numeric(key: String, label: String, unit: Unit, semantic: Semantic) -> PointSpec {
        PointSpec {
            key: key.into(),
            label: label.into(),
            kind: Kind::Numeric {
                min: None,
                max: None,
                step: None,
            },
            access: Access {
                read: true,
                write: false,
            },
            unit: Some(unit),
            semantic,
        }
    }

    /// The writable `state` point of a relay device.
    fn state_point() -> PointSpec {
        PointSpec {
            key: "state".into(),
            label: moli_i18n::tr!("pilotes.tasmota.point.etat").into(),
            kind: Kind::Binary,
            access: Access {
                read: true,
                write: true,
            },
            unit: None,
            semantic: Semantic::OnOff,
        }
    }

    /// The electrical measurements of one channel, unsuffixed.
    fn channel_points(&self, channel: u8) -> Vec<PointSpec> {
        self.channel_metrics
            .get(&channel)
            .map(|metrics| {
                metrics
                    .keys()
                    .map(|metric| {
                        Self::numeric(
                            metric.base().to_owned(),
                            moli_i18n::tr!(metric.label_key()),
                            metric.unit(),
                            metric.semantic(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The point key of an environmental metric (`temperature` or
    /// `temperature_sht3x_0x44` when several probes report).
    fn env_key(&self, metric: EnvMetric, slug: &str) -> String {
        if self.env.len() > 1 {
            format!("{}_{}", metric.base(), slug)
        } else {
            metric.base().to_owned()
        }
    }

    /// The read-only points shared by the whole host: environmental
    /// probes, the legacy total energy and the Wi-Fi signal.
    fn shared_points(&self) -> Vec<PointSpec> {
        let mut points = Vec::new();
        for (slug, metrics) in &self.env {
            for metric in metrics.keys() {
                points.push(Self::numeric(
                    self.env_key(*metric, slug),
                    if self.env.len() > 1 {
                        format!("{} · {slug}", moli_i18n::tr!(metric.label_key()))
                    } else {
                        moli_i18n::tr!(metric.label_key())
                    },
                    metric.unit(),
                    metric.semantic(),
                ));
            }
        }
        if self.legacy_energy.is_some() {
            points.push(Self::numeric(
                "energy_total".to_owned(),
                moli_i18n::tr!("pilotes.tasmota.point.energie_totale"),
                Unit::KiloWattHour,
                Semantic::Energy,
            ));
        }
        if self.rssi.is_some() {
            points.push(Self::numeric(
                "rssi".to_owned(),
                moli_i18n::tr!("pilotes.tasmota.point.signal"),
                Unit::Decibel,
                Semantic::SignalStrength,
            ));
        }
        points
    }

    /// The `(key, device, channel)` triples this host maps to.
    fn specs(&self, instance: &InstanceId, hostname: &str) -> Vec<(String, Device, Option<u8>)> {
        let mut specs = Vec::new();
        if self.multi() {
            let points = self.shared_points();
            if !points.is_empty() {
                specs.push((
                    hostname.to_owned(),
                    Self::device(instance, hostname, hostname.into(), points),
                    None,
                ));
            }
            for channel in self.channels.keys() {
                let key = format!("{hostname}/c{channel}");
                let name =
                    moli_i18n::tr!("pilotes.tasmota.appareil.canal", host = hostname, n = *channel);
                let mut points = vec![Self::state_point()];
                points.extend(self.channel_points(*channel));
                specs.push((
                    key.clone(),
                    Self::device(instance, &key, name, points),
                    Some(*channel),
                ));
            }
        } else {
            let channel = self.channels.keys().next().copied();
            let mut points = Vec::new();
            if channel.is_some() {
                points.push(Self::state_point());
            }
            if let Some(n) = channel {
                points.extend(self.channel_points(n));
            }
            points.extend(self.shared_points());
            specs.push((
                hostname.to_owned(),
                Self::device(instance, hostname, hostname.into(), points),
                channel,
            ));
        }
        specs
    }
}

impl Entry {
    /// Writes every known value of `host` this device owns as state.
    fn publish_states(&self, ctx: &DriverCtx, host: &Host) {
        let relay_and_metrics = |channel: u8, device: &DeviceId| {
            if let Some(on) = host.relay.get(&channel) {
                ctx.set_state(device, "state", Value::Bool(*on));
            }
            if let Some(metrics) = host.channel_metrics.get(&channel) {
                for (metric, value) in metrics {
                    ctx.set_state(
                        device,
                        metric.base(),
                        Value::from_json(&Json::from(*value)),
                    );
                }
            }
        };
        match self.channel {
            Some(channel) => relay_and_metrics(channel, &self.device.id),
            None if host.multi() => {}
            None => {
                if let Some(channel) = host.channels.keys().next().copied() {
                    relay_and_metrics(channel, &self.device.id);
                }
            }
        }
        if self.channel.is_none() {
            for (slug, metrics) in &host.env {
                for (metric, value) in metrics {
                    ctx.set_state(
                        &self.device.id,
                        &host.env_key(*metric, slug),
                        Value::from_json(&Json::from(*value)),
                    );
                }
            }
            if let Some(energy) = host.legacy_energy {
                ctx.set_state(
                    &self.device.id,
                    "energy_total",
                    Value::from_json(&Json::from(energy)),
                );
            }
            if let Some(rssi) = host.rssi {
                ctx.set_state(
                    &self.device.id,
                    "rssi",
                    Value::from_json(&Json::from(rssi)),
                );
            }
        }
    }
}

/// `POWER`, `POWER1`… -> channel number (`POWER` is channel 1).
fn power_channel(key: &str) -> Option<u8> {
    let digits = key.strip_prefix("POWER")?;
    if digits.is_empty() {
        return Some(1);
    }
    digits.parse::<u8>().ok().filter(|n| *n >= 1)
}

/// A `tele/<host>/STATE` payload: the relay states.
fn parse_state(host: &mut Host, payload: &Json) {
    let Some(object) = payload.as_object() else {
        return;
    };
    for (key, value) in object {
        let Some(channel) = power_channel(key) else {
            continue;
        };
        host.channels
            .entry(channel)
            .or_insert_with(|| key.clone());
        // `TOGGLE`/`HOLD` are transitions, not states: the next STATE
        // settles them, so only ON/OFF become values.
        if let Some(on) = value.as_str().and_then(|v| match v {
            "ON" => Some(true),
            "OFF" => Some(false),
            _ => None,
        }) {
            host.relay.insert(channel, on);
        }
    }
}

/// A `stat/<host>/STATUS*` payload: sections like `StatusSTS` carry the
/// relay states under `POWER`/`POWER<n>` keys.
fn parse_status(host: &mut Host, payload: &Json) {
    let Some(object) = payload.as_object() else {
        return;
    };
    for section in object.values() {
        let Some(section) = section.as_object() else {
            continue;
        };
        for (key, value) in section {
            let Some(channel) = power_channel(key) else {
                continue;
            };
            host.channels
                .entry(channel)
                .or_insert_with(|| key.clone());
            if let Some(on) = value.as_str().and_then(|v| match v {
                "ON" => Some(true),
                "OFF" => Some(false),
                _ => None,
            }) {
                host.relay.insert(channel, on);
            }
        }
    }
}

/// A `tele/<host>/SENSOR` payload: electrical measurements per relay
/// channel, environmental probes, the Wi-Fi signal and the legacy total
/// energy counter.
fn parse_sensor(host: &mut Host, payload: &Json) {
    let Some(object) = payload.as_object() else {
        return;
    };
    let fahrenheit = object
        .get("TempUnit")
        .and_then(Json::as_str)
        .is_some_and(|u| u.eq_ignore_ascii_case("f"));
    let mut data = SensorData::default();
    for (key, value) in object {
        match key.as_str() {
            "Time" | "TempUnit" | "Uptime" => {}
            "ENERGY" => {
                if let Some(total) = value.get("Total").and_then(Json::as_f64) {
                    data.legacy_energy = Some(total);
                }
            }
            "Wifi" => {
                if let Some(rssi) = value.get("RSSI").and_then(Json::as_f64) {
                    data.rssi = Some(rssi);
                }
            }
            _ => {
                if let Some(channel) = power_channel(key)
                    && let Some(metrics) = value.as_object()
                {
                    let mut found = BTreeMap::new();
                    for (field, metric) in [
                        ("Power", ChannelMetric::Power),
                        ("Energy", ChannelMetric::Energy),
                        ("ApparentPower", ChannelMetric::ApparentPower),
                        ("Voltage", ChannelMetric::Voltage),
                        ("Current", ChannelMetric::Current),
                    ] {
                        if let Some(v) = metrics.get(field).and_then(Json::as_f64) {
                            found.insert(metric, v);
                        }
                    }
                    if !found.is_empty() {
                        host.channels
                            .entry(channel)
                            .or_insert_with(|| format!("POWER{channel}"));
                        data.channels.insert(channel, found);
                    }
                    continue;
                }
                if let Some(metrics) = env_metrics(value, fahrenheit)
                    && !metrics.is_empty()
                {
                    data.env.insert(slug(key), metrics);
                }
            }
        }
    }
    // A SENSOR message is a complete snapshot: replace what it covers.
    host.channel_metrics = data.channels;
    host.env = data.env;
    if data.legacy_energy.is_some() {
        host.legacy_energy = data.legacy_energy;
    }
    if data.rssi.is_some() {
        host.rssi = data.rssi;
    }
}

/// The known measurements inside one named sensor object.
fn env_metrics(value: &Json, fahrenheit: bool) -> Option<BTreeMap<EnvMetric, f64>> {
    let object = value.as_object()?;
    let mut found = BTreeMap::new();
    for (field, metric) in [
        ("Temperature", EnvMetric::Temperature),
        ("Humidity", EnvMetric::Humidity),
        ("Pressure", EnvMetric::Pressure),
        ("Illuminance", EnvMetric::Illuminance),
        ("Battery", EnvMetric::Battery),
    ] {
        if let Some(mut v) = object.get(field).and_then(Json::as_f64) {
            if metric == EnvMetric::Temperature && fahrenheit {
                v = (v - 32.0) * 5.0 / 9.0;
            }
            found.insert(metric, v);
        }
    }
    (!found.is_empty()).then_some(found)
}

/// A sensor object's name made into a point-key fragment.
fn slug(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let slug = slug.trim_matches('_');
    if slug.is_empty() {
        "capteur".to_owned()
    } else {
        slug.to_owned()
    }
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let client_id = config
        .client_id
        .clone()
        .unwrap_or_else(|| format!("moli-os-{}", ctx.instance()));
    let mut options = MqttOptions::new(client_id, &config.host, config.port);
    options.set_keep_alive(Duration::from_secs(30));
    options.set_max_packet_size(1024 * 1024, 256 * 1024);
    if let Some(username) = &config.username {
        let password = match &config.password_env {
            Some(var) => std::env::var(var).with_context(|| format!("env var {var} not set"))?,
            None => String::new(),
        };
        options.set_credentials(username, password);
    }

    let allowed: Option<BTreeSet<&str>> = config
        .devices
        .as_ref()
        .map(|d| d.iter().map(String::as_str).collect());

    let (client, mut events) = AsyncClient::new(options, 64);
    let mut registry = Registry::default();
    let mut connected = false;

    loop {
        tokio::select! {
            event = events.poll() => match event.context("mqtt connection")? {
                Event::Incoming(Packet::ConnAck(_)) => {
                    connected = true;
                    client.try_subscribe("tele/#", QoS::AtLeastOnce)?;
                    client.try_subscribe("stat/#", QoS::AtLeastOnce)?;
                    tracing::info!(instance = %ctx.instance(), "connected to mqtt");
                    ctx.ready();
                }
                Event::Incoming(Packet::Publish(publish)) => {
                    on_publish(ctx, &mut registry, allowed.as_ref(), &client, &publish);
                }
                _ => {}
            },
            command = ctx.next_command() => match command {
                Some(command) if !connected => {
                    command.reply(Err(moli_i18n::tr!("pilotes.tasmota.broker_injoignable")));
                }
                Some(command) => on_command(&client, &registry, command),
                None => return Ok(()),
            },
        }
    }
}

/// `tele/<host>/<leaf>` or `stat/<host>/<leaf>` -> (host, leaf).
fn split_topic(topic: &str) -> Option<(&str, &str)> {
    let rest = topic
        .strip_prefix("tele/")
        .or_else(|| topic.strip_prefix("stat/"))?;
    let (host, leaf) = rest.split_once('/')?;
    (!host.is_empty() && !leaf.is_empty()).then_some((host, leaf))
}

/// The hosts reported so far and the Moli devices they map to.
#[derive(Default)]
struct Registry {
    hosts: HashMap<String, Host>,
    devices: HashMap<String, Entry>,
}

impl Registry {
    fn host(&mut self, hostname: &str) -> &mut Host {
        self.hosts.entry(hostname.to_owned()).or_default()
    }

    /// Rebuilds the devices of `hostname` from what it reported: one
    /// device per relay channel, plus the host device for shared sensors
    /// (or for the single relay of a one-channel device).
    fn sync(&mut self, ctx: &DriverCtx, hostname: &str) {
        let Some(host) = self.hosts.get(hostname) else {
            return;
        };
        let specs = host.specs(ctx.instance(), hostname);
        let keep: BTreeSet<&str> = specs.iter().map(|(key, _, _)| key.as_str()).collect();
        let stale: Vec<String> = self
            .devices
            .iter()
            .filter(|(_, e)| e.hostname == hostname && !keep.contains(e.key.as_str()))
            .map(|(key, _)| key.clone())
            .collect();
        for key in stale {
            if let Some(entry) = self.devices.remove(&key) {
                ctx.remove_device(&entry.device.id);
            }
        }
        for (key, device, channel) in specs {
            match self.devices.get_mut(&key) {
                Some(entry) if entry.device.points == device.points => {}
                Some(entry) => {
                    entry.device = device.clone();
                    ctx.upsert_device(device);
                }
                None => {
                    ctx.upsert_device(device.clone());
                    self.devices.insert(
                        key.clone(),
                        Entry {
                            key,
                            device,
                            hostname: hostname.to_owned(),
                            channel,
                        },
                    );
                }
            }
        }
    }

    /// The ids of every device `hostname` currently maps to.
    fn ids(&self, hostname: &str) -> Vec<DeviceId> {
        self.devices
            .values()
            .filter(|e| e.hostname == hostname)
            .map(|e| e.device.id.clone())
            .collect()
    }

    fn set_online(&self, ctx: &DriverCtx, hostname: &str, online: bool) {
        for id in self.ids(hostname) {
            ctx.set_availability(&id, online);
        }
    }

    /// Writes every known value of `hostname`'s devices as state.
    fn publish(&self, ctx: &DriverCtx, hostname: &str) {
        let Some(host) = self.hosts.get(hostname) else {
            return;
        };
        for entry in self
            .devices
            .values()
            .filter(|e| e.hostname == hostname)
        {
            entry.publish_states(ctx, host);
        }
    }
}

fn on_publish(
    ctx: &DriverCtx,
    registry: &mut Registry,
    allowed: Option<&BTreeSet<&str>>,
    client: &AsyncClient,
    publish: &Publish,
) {
    let Some((host, leaf)) = split_topic(&publish.topic) else {
        return;
    };
    if let Some(list) = allowed
        && !list.contains(host)
    {
        return;
    }
    let root_tele = publish.topic.starts_with("tele/");
    match (root_tele, leaf) {
        // `LWT` is retained on `stat/` by default; some builds keep it on
        // `tele/`. Either way it announces (or denies) the device.
        (_, "LWT") => {
            let online = publish.payload.trim_ascii() == b"Online";
            registry.sync(ctx, host);
            registry.set_online(ctx, host, online);
            if online {
                // Ask for a fresh STATE: Tasmota answers `Status` with one.
                let _ = client.try_publish(
                    format!("cmnd/{host}/Status"),
                    QoS::AtMostOnce,
                    false,
                    "0",
                );
            }
        }
        (true, "STATE") => {
            if let Ok(payload) = serde_json::from_slice::<Json>(&publish.payload) {
                parse_state(registry.host(host), &payload);
            }
            registry.sync(ctx, host);
            registry.publish(ctx, host);
            registry.set_online(ctx, host, true);
        }
        (true, "SENSOR") => {
            if let Ok(payload) = serde_json::from_slice::<Json>(&publish.payload) {
                parse_sensor(registry.host(host), &payload);
            }
            registry.sync(ctx, host);
            registry.publish(ctx, host);
            registry.set_online(ctx, host, true);
        }
        (true, _) if leaf.starts_with("STATUS") => {
            // `Status` answers: nothing to publish, the device exists.
            registry.sync(ctx, host);
            registry.set_online(ctx, host, true);
        }
        // `Status 0` answers land on `stat/<host>/STATUS*` sections; the
        // relay states sit in the section carrying `POWER<n>` keys.
        (false, leaf) if leaf.starts_with("STATUS") => {
            if let Ok(payload) = serde_json::from_slice::<Json>(&publish.payload) {
                parse_status(registry.host(host), &payload);
            }
            registry.sync(ctx, host);
            registry.publish(ctx, host);
            registry.set_online(ctx, host, true);
        }
        (false, _) => {
            let Some(channel) = power_channel(leaf) else {
                return;
            };
            let on = match publish.payload.trim_ascii() {
                b"ON" => Some(true),
                b"OFF" => Some(false),
                _ => None,
            };
            let host_entry = registry.host(host);
            // The stat leaf is the wire key itself: keep it.
            host_entry
                .channels
                .entry(channel)
                .or_insert_with(|| leaf.to_owned());
            if let Some(on) = on {
                host_entry.relay.insert(channel, on);
            }
            registry.sync(ctx, host);
            registry.publish(ctx, host);
            registry.set_online(ctx, host, true);
        }
        _ => {}
    }
}

fn on_command(client: &AsyncClient, registry: &Registry, command: CommandRequest) {
    let Some(entry) = registry
        .devices
        .values()
        .find(|e| e.device.id == command.device.id)
    else {
        command.reply(Err(moli_i18n::tr!("pilotes.tasmota.appareil_inconnu")));
        return;
    };
    let Some(channel) = entry.channel.filter(|_| command.key.as_ref() == "state") else {
        command.reply(Err(moli_i18n::tr!("pilotes.tasmota.lecture_seule")));
        return;
    };
    let order = match &command.value {
        Value::Bool(on) => (if *on { "ON" } else { "OFF" }).to_owned(),
        Value::Text(text) if matches!(text.to_ascii_uppercase().as_str(), "ON" | "OFF" | "TOGGLE") => {
            text.to_ascii_uppercase()
        }
        _ => {
            command.reply(Err(moli_i18n::tr!("pilotes.tasmota.valeur_invalide")));
            return;
        }
    };
    let wire = registry
        .hosts
        .get(&entry.hostname)
        .and_then(|host| host.channels.get(&channel))
        .map_or("POWER", String::as_str);
    let topic = format!("cmnd/{}/{wire}", entry.hostname);
    tracing::debug!(%topic, %order, "tasmota command");
    match client.try_publish(&topic, QoS::AtLeastOnce, false, order) {
        Ok(()) => command.reply(Ok(())),
        Err(e) => command.reply(Err(format!("mqtt publish failed: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn host() -> Host {
        Host::default()
    }

    fn specs(host: &Host) -> Vec<(String, Device, Option<u8>)> {
        host.specs(&InstanceId::from("tasmota"), "4CHPRO-6249")
    }

    fn keys(device: &Device) -> Vec<String> {
        device.points.iter().map(|p| p.key.to_string()).collect()
    }

    #[test]
    fn state_payload_fills_relay_channels() {
        let mut h = host();
        parse_state(
            &mut h,
            &json!({"POWER1":"ON","POWER2":"OFF","POWER3":"ON","POWER4":"OFF","Time":"x"}),
        );
        assert_eq!(h.channels.len(), 4);
        assert!(h.relay[&1]);
        assert!(!h.relay[&4]);
        // One device per channel, each with a single writable `state`.
        let specs = specs(&h);
        assert_eq!(specs.len(), 4);
        for (key, device, channel) in &specs {
            assert!(key.starts_with("4CHPRO-6249/c"));
            assert_eq!(keys(device), vec!["state"]);
            assert!(channel.is_some());
        }
    }

    #[test]
    fn single_relay_keeps_the_hostname() {
        let mut h = host();
        parse_state(&mut h, &json!({"POWER":"ON"}));
        let specs = specs(&h);
        assert_eq!(specs.len(), 1);
        let (key, device, channel) = &specs[0];
        assert_eq!(key.as_str(), "4CHPRO-6249");
        assert_eq!(keys(device), vec!["state"]);
        assert_eq!(*channel, Some(1));
    }

    #[test]
    fn status_sections_fill_relay_channels() {
        let mut h = host();
        parse_status(
            &mut h,
            &json!({
                "StatusSTS": {"POWER1":"ON","POWER2":"OFF","POWER3":"ON","POWER4":"OFF"},
                "StatusNET": {"IPAddress":"192.168.1.x"}
            }),
        );
        assert_eq!(h.channels.len(), 4);
        assert!(h.relay[&1]);
        assert!(!h.relay[&2]);
        assert!(h.relay[&3]);
        assert!(!h.relay[&4]);
    }

    #[test]
    fn sensor_payload_splits_channels_and_shared() {
        let mut h = host();
        parse_state(&mut h, &json!({"POWER1":"ON","POWER2":"OFF"}));
        parse_sensor(
            &mut h,
            &json!({
                "POWER1":{"Power":12.5,"Energy":0.42,"ApparentPower":13.0,"Voltage":230.0,"Current":0.06},
                "POWER2":{"Power":0.0,"Energy":1.5,"Voltage":230.0,"Current":0.0},
                "TempUnit":"C",
                "SHT3X-0x44":{"Temperature":21.5,"Humidity":45.0},
                "Wifi":{"RSSI":-58},
                "ENERGY":{"Total":12.34}
            }),
        );
        let specs = specs(&h);
        // Host device (shared sensors) + one device per channel.
        assert_eq!(specs.len(), 3);
        let (_, main, _) = specs.iter().find(|(_, _, c)| c.is_none()).unwrap();
        for expected in ["temperature", "humidity", "energy_total", "rssi"] {
            assert!(
                keys(main).contains(&expected.to_owned()),
                "main missing {expected}"
            );
        }
        // Channel 1 reports every metric, channel 2 has no ApparentPower.
        for (channel, expected) in [
            (
                1u8,
                &["state", "power", "energy", "apparent_power", "voltage", "current"][..],
            ),
            (2u8, &["state", "power", "energy", "voltage", "current"][..]),
        ] {
            let (_, device, _) = specs
                .iter()
                .find(|(_, _, c)| *c == Some(channel))
                .unwrap();
            for key in expected {
                assert!(
                    keys(device).contains(&key.to_string()),
                    "channel {channel} missing {key}"
                );
            }
        }
        assert_eq!(h.channel_metrics[&1][&ChannelMetric::Power], 12.5);
        assert_eq!(h.env["sht3x_0x44"][&EnvMetric::Humidity], 45.0);
    }

    #[test]
    fn fahrenheit_is_converted() {
        let mut h = host();
        parse_sensor(
            &mut h,
            &json!({"TempUnit":"F","DS18B20-1":{"Temperature":71.6}}),
        );
        let temp = h.env["ds18b20_1"][&EnvMetric::Temperature];
        assert!((temp - 22.0).abs() < 0.01);
    }

    #[test]
    fn topics_split() {
        assert_eq!(
            split_topic("tele/4CHPRO-6249/STATE"),
            Some(("4CHPRO-6249", "STATE"))
        );
        assert_eq!(
            split_topic("stat/4CHPRO-6249/LWT"),
            Some(("4CHPRO-6249", "LWT"))
        );
        assert_eq!(split_topic("cmnd/x/POWER1"), None);
        assert_eq!(split_topic("tele/"), None);
    }

    #[test]
    fn real_4chpro_messages_map_fully() {
        let mut h = host();
        parse_state(
            &mut h,
            &serde_json::from_slice::<Json>(include_bytes!(
                "../../../integrations/tasmota/fixtures/4chpro-state.json"
            ))
            .unwrap(),
        );
        parse_sensor(
            &mut h,
            &serde_json::from_slice::<Json>(include_bytes!(
                "../../../integrations/tasmota/fixtures/4chpro-sensor.json"
            ))
            .unwrap(),
        );
        let specs = specs(&h);
        assert_eq!(specs.len(), 5); // host (sensors) + 4 channels
        let (_, main, _) = specs.iter().find(|(_, _, c)| c.is_none()).unwrap();
        for expected in ["temperature", "humidity", "rssi"] {
            assert!(
                keys(main).contains(&expected.to_owned()),
                "main missing {expected}"
            );
        }
        for channel in 1..=4u8 {
            let (_, device, _) = specs
                .iter()
                .find(|(_, _, c)| *c == Some(channel))
                .unwrap();
            assert_eq!(keys(device)[0], "state");
        }
        assert_eq!(h.channel_metrics[&3][&ChannelMetric::Power], 87.6);
        assert!(h.relay[&1]);
        assert!(!h.relay[&4]);
    }

    #[test]
    fn power_channel_parsing() {
        assert_eq!(power_channel("POWER"), Some(1));
        assert_eq!(power_channel("POWER4"), Some(4));
        assert_eq!(power_channel("POWER0"), None);
        assert_eq!(power_channel("POWERX"), None);
        assert_eq!(power_channel("STATE"), None);
    }
}
