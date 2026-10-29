//! Google Cast (TLS 8009, the TV's self-signed certificate pinned): what the
//! app on screen plays (title, picture, playing or paused) and play / pause.
//! Messages are a small protobuf envelope (after a 4-byte length) around
//! JSON. The receiver says which app runs; the app's media channel says
//! what it plays, and both speak up when something changes.

use std::time::Duration;

use anyhow::{Context as _, bail};
use moli_net::PinnedTls;
use serde_json::{Value as Json, json};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::sync::mpsc;

use crate::proto::{Writer, parse};

pub const PORT: u16 = 8009;
const SENDER: &str = "sender-moli";
const RECEIVER: &str = "receiver-0";
const DEFAULT_MEDIA_RECEIVER: &str = "CC1AD845";
const NS_CONNECTION: &str = "urn:x-cast:com.google.cast.tp.connection";
const NS_HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";
const NS_RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";
const NS_MEDIA: &str = "urn:x-cast:com.google.cast.media";
/// Heartbeat and receiver status: the app's media is asked less often
/// (its changes come by themselves).
const POLL: Duration = Duration::from_secs(10);
const MEDIA_EVERY: u32 = 6;
/// The receiver answers every heartbeat: this long silent, it is gone.
const QUIET: Duration = Duration::from_secs(35);
/// How long a spoken sentence may take to launch and play.
const SPEAK_LIMIT: Duration = Duration::from_secs(30);
const MAX_MESSAGE: usize = 256 * 1024;

/// What the screen or speaker plays, as Cast knows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Playing {
    /// The app's name (« Disney+ », « YouTube »); `None`: nothing casts.
    pub app: Option<String>,
    /// `playing`, `paused`, `buffering`, `idle`.
    pub state: Option<String>,
    pub title: Option<String>,
    /// Series, artist or subtitle.
    pub subtitle: Option<String>,
    /// A picture of what plays (a public URL).
    pub image: Option<String>,
    /// Seconds.
    pub duration: Option<f64>,
    /// Seconds, when last told.
    pub position: Option<f64>,
    /// The receiver's volume, 0 to 100.
    pub volume: Option<f64>,
    /// Whether the receiver is muted.
    pub muted: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Playing(Playing),
    /// The connection ended (said by whoever reconnects): nothing is known.
    Lost,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Order {
    Play,
    Pause,
    Stop,
    /// Receiver volume, `0.0..=1.0`.
    Volume(f64),
    Mute(bool),
    /// Launches the default media receiver (if needed) and plays `url`.
    Load {
        url: String,
        mime: String,
    },
}

/// The app being followed and its media session.
#[derive(Default)]
struct Follow {
    app_id: Option<String>,
    transport: Option<String>,
    session: Option<u64>,
    playing: Playing,
}

/// One connection on [`PORT`], until it fails or `orders` closes (`Ok`).
pub async fn session(
    tls: &PinnedTls,
    host: &str,
    orders: &mut mpsc::Receiver<Order>,
    events: &mpsc::Sender<Event>,
) -> anyhow::Result<()> {
    session_port(tls, host, PORT, orders, events).await
}

/// One connection on `port`, until it fails or `orders` closes (`Ok`).
#[allow(clippy::too_many_lines)]
pub async fn session_port(
    tls: &PinnedTls,
    host: &str,
    port: u16,
    orders: &mut mpsc::Receiver<Order>,
    events: &mpsc::Sender<Event>,
) -> anyhow::Result<()> {
    let stream = tls.connect(host, port).await.context("cast")?;
    // An order given while the device was away is not run now, late.
    while orders.try_recv().is_ok() {}
    let (mut read, mut write) = tokio::io::split(stream);
    let (frames_tx, mut frames) = mpsc::channel::<Vec<u8>>(16);
    let reader = tokio::spawn(async move {
        while let Ok(frame) = read_frame(&mut read).await {
            if frames_tx.send(frame).await.is_err() {
                return;
            }
        }
    });
    let result: anyhow::Result<()> = async {
        let mut follow = Follow::default();
        let mut told: Option<Playing> = None;
        let mut pending_load: Option<(String, String)> = None;
        let mut request = 1u64;
        send(&mut write, RECEIVER, NS_CONNECTION, &json!({ "type": "CONNECT" })).await?;
        send(&mut write, RECEIVER, NS_RECEIVER, &json!({ "type": "GET_STATUS", "requestId": request })).await?;
        let mut tick = tokio::time::interval(POLL);
        let mut ticks = 0u32;
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    ticks += 1;
                    request += 1;
                    send(&mut write, RECEIVER, NS_HEARTBEAT, &json!({ "type": "PING" })).await?;
                    send(&mut write, RECEIVER, NS_RECEIVER, &json!({ "type": "GET_STATUS", "requestId": request })).await?;
                    if let Some(transport) = follow.transport.clone().filter(|_| ticks.is_multiple_of(MEDIA_EVERY)) {
                        request += 1;
                        send(&mut write, &transport, NS_MEDIA, &json!({ "type": "GET_STATUS", "requestId": request })).await?;
                    }
                }
                frame = tokio::time::timeout(QUIET, frames.recv()) => {
                    let frame = frame.context("the Cast device stopped answering")?.context("the Cast device closed the connection")?;
                    let Some((source, namespace, body)) = envelope(&frame) else { continue };
                    match (namespace.as_str(), body["type"].as_str()) {
                        (NS_HEARTBEAT, Some("PING")) => {
                            send(&mut write, &source, NS_HEARTBEAT, &json!({ "type": "PONG" })).await?;
                        }
                        (NS_RECEIVER, Some("RECEIVER_STATUS")) => {
                            if let Some(transport) = follow.receiver(&body) {
                                request += 1;
                                send(&mut write, &transport, NS_CONNECTION, &json!({ "type": "CONNECT" })).await?;
                                send(&mut write, &transport, NS_MEDIA, &json!({ "type": "GET_STATUS", "requestId": request })).await?;
                            }
                            if follow.app_id.as_deref() == Some(DEFAULT_MEDIA_RECEIVER)
                                && let Some(transport) = follow.transport.clone()
                                && let Some((url, mime)) = pending_load.take()
                            {
                                request += 1;
                                send(&mut write, &transport, NS_MEDIA, &load_body(request, &url, &mime)).await?;
                            }
                        }
                        (NS_MEDIA, Some("MEDIA_STATUS")) => follow.media(&body),
                        (NS_CONNECTION, Some("CLOSE")) if follow.transport.as_deref() == Some(source.as_str()) => {
                            follow.app_id = None;
                            follow.transport = None;
                            follow.session = None;
                        }
                        _ => {}
                    }
                    if told.as_ref() != Some(&follow.playing) {
                        told = Some(follow.playing.clone());
                        events.send(Event::Playing(follow.playing.clone())).await?;
                    }
                }
                order = orders.recv() => {
                    let Some(order) = order else { return Ok(()) };
                    match order {
                        Order::Play | Order::Pause | Order::Stop => {
                            let (Some(transport), Some(session)) = (follow.transport.clone(), follow.session) else {
                                tracing::debug!("cast: nothing to control");
                                continue;
                            };
                            request += 1;
                            let kind = match order {
                                Order::Play => "PLAY",
                                Order::Pause => "PAUSE",
                                Order::Stop => "STOP",
                                _ => unreachable!(),
                            };
                            send(&mut write, &transport, NS_MEDIA, &json!({ "type": kind, "mediaSessionId": session, "requestId": request })).await?;
                        }
                        Order::Volume(level) => {
                            request += 1;
                            let level = level.clamp(0.0, 1.0);
                            send(&mut write, RECEIVER, NS_RECEIVER, &json!({ "type": "SET_VOLUME", "volume": { "level": level }, "requestId": request })).await?;
                        }
                        Order::Mute(muted) => {
                            request += 1;
                            send(&mut write, RECEIVER, NS_RECEIVER, &json!({ "type": "SET_VOLUME", "volume": { "muted": muted }, "requestId": request })).await?;
                        }
                        Order::Load { url, mime } => {
                            if follow.app_id.as_deref() == Some(DEFAULT_MEDIA_RECEIVER)
                                && let Some(transport) = follow.transport.clone()
                            {
                                request += 1;
                                send(&mut write, &transport, NS_MEDIA, &load_body(request, &url, &mime)).await?;
                            } else {
                                pending_load = Some((url, mime));
                                request += 1;
                                send(&mut write, RECEIVER, NS_RECEIVER, &json!({ "type": "LAUNCH", "appId": DEFAULT_MEDIA_RECEIVER, "requestId": request })).await?;
                            }
                        }
                    }
                }
            }
        }
    }
    .await;
    reader.abort();
    result
}

/// Plays `url` (`mime`) on a Cast speaker and returns once the sentence has
/// finished playing (or fails if the speaker refuses or times out).
pub async fn speak(
    tls: &PinnedTls,
    host: &str,
    port: u16,
    url: &str,
    mime: &str,
) -> anyhow::Result<()> {
    let stream = tls.connect(host, port).await.context("cast")?;
    tokio::time::timeout(SPEAK_LIMIT, speak_on(stream, url, mime))
        .await
        .context("cast: speech timed out")?
}

async fn speak_on<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    url: &str,
    mime: &str,
) -> anyhow::Result<()> {
    let mut request = 1u64;
    send(
        &mut stream,
        RECEIVER,
        NS_CONNECTION,
        &json!({ "type": "CONNECT" }),
    )
    .await?;
    send(
        &mut stream,
        RECEIVER,
        NS_RECEIVER,
        &json!({ "type": "LAUNCH", "appId": DEFAULT_MEDIA_RECEIVER, "requestId": request }),
    )
    .await?;
    let mut connected_transport: Option<String> = None;
    let mut load_request: Option<u64> = None;
    let mut started = false;
    loop {
        let frame = read_frame(&mut stream).await?;
        let Some((source, namespace, body)) = envelope(&frame) else {
            continue;
        };
        match (namespace.as_str(), body["type"].as_str()) {
            (NS_HEARTBEAT, Some("PING")) => {
                send(
                    &mut stream,
                    &source,
                    NS_HEARTBEAT,
                    &json!({ "type": "PONG" }),
                )
                .await?;
            }
            (NS_RECEIVER, Some("LAUNCH_ERROR")) => {
                let reason = body["reason"].as_str().unwrap_or("launch refused");
                bail!("cast launch failed: {reason}");
            }
            (NS_RECEIVER, Some("RECEIVER_STATUS")) if connected_transport.is_none() => {
                let transport = body["status"]["applications"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|a| a["appId"].as_str() == Some(DEFAULT_MEDIA_RECEIVER))
                    .and_then(|a| a["transportId"].as_str())
                    .map(str::to_owned);
                if let Some(transport) = transport {
                    send(
                        &mut stream,
                        &transport,
                        NS_CONNECTION,
                        &json!({ "type": "CONNECT" }),
                    )
                    .await?;
                    request += 1;
                    load_request = Some(request);
                    send(
                        &mut stream,
                        &transport,
                        NS_MEDIA,
                        &load_body(request, url, mime),
                    )
                    .await?;
                    connected_transport = Some(transport);
                }
            }
            (NS_MEDIA, Some("LOAD_FAILED" | "LOAD_CANCELLED" | "INVALID_REQUEST")) => {
                bail!(
                    "cast media refused ({})",
                    body["type"].as_str().unwrap_or("error")
                );
            }
            (NS_MEDIA, Some("MEDIA_STATUS")) if connected_transport.is_some() => {
                let Some(status) = body["status"].as_array().and_then(|s| s.first()) else {
                    continue;
                };
                let state = status["playerState"].as_str().unwrap_or_default();
                let reason = status["idleReason"].as_str();
                let answered_load =
                    load_request.is_some_and(|id| body["requestId"].as_u64() == Some(id));
                if matches!(state, "BUFFERING" | "PLAYING" | "PAUSED") || answered_load {
                    started = true;
                }
                if reason == Some("ERROR") {
                    bail!("cast playback error");
                }
                if state == "IDLE"
                    && (matches!(reason, Some("FINISHED" | "CANCELLED" | "INTERRUPTED"))
                        || (started && reason.is_some()))
                {
                    return Ok(());
                }
            }
            _ => {}
        }
    }
}

fn load_body(request: u64, url: &str, mime: &str) -> Json {
    json!({
        "type": "LOAD",
        "requestId": request,
        "autoplay": true,
        "media": {
            "contentId": url,
            "contentType": mime,
            "streamType": "BUFFERED",
        }
    })
}

impl Follow {
    /// The receiver's status: which app runs and the receiver's volume.
    /// Returns the app's transport when it is new (to connect to and ask).
    fn receiver(&mut self, body: &Json) -> Option<String> {
        let volume = body["status"]["volume"]["level"]
            .as_f64()
            .map(|v| (v * 100.0).round())
            .or(self.playing.volume);
        let muted = body["status"]["volume"]["muted"]
            .as_bool()
            .or(self.playing.muted);
        let apps = body["status"]["applications"].as_array();
        let app = apps
            .into_iter()
            .flatten()
            .find(|a| a["isIdleScreen"].as_bool() != Some(true));
        let Some(app) = app else {
            *self = Self {
                playing: Playing {
                    volume,
                    muted,
                    ..Playing::default()
                },
                ..Self::default()
            };
            return None;
        };
        self.app_id = app["appId"].as_str().map(str::to_owned);
        let name = app["displayName"].as_str().map(str::to_owned);
        if self.playing.app == name {
            self.playing.volume = volume;
            self.playing.muted = muted;
        } else {
            self.playing = Playing {
                app: name,
                volume,
                muted,
                ..Playing::default()
            };
        }
        let speaks_media = app["namespaces"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|n| n["name"].as_str() == Some(NS_MEDIA));
        let transport = app["transportId"]
            .as_str()
            .filter(|_| speaks_media)
            .map(str::to_owned);
        if transport == self.transport {
            return None;
        }
        self.transport.clone_from(&transport);
        self.session = None;
        transport
    }

    /// The app's media status. A status may leave out what did not change
    /// (the media itself): only what it says is updated.
    fn media(&mut self, body: &Json) {
        let Some(status) = body["status"].as_array().and_then(|s| s.first()) else {
            // Nothing loaded any more.
            self.session = None;
            self.playing = Playing {
                app: self.playing.app.take(),
                volume: self.playing.volume,
                muted: self.playing.muted,
                ..Playing::default()
            };
            return;
        };
        self.session = status["mediaSessionId"].as_u64().or(self.session);
        let p = &mut self.playing;
        if let Some(state) = status["playerState"].as_str() {
            p.state = Some(state.to_ascii_lowercase());
        }
        if let Some(position) = status["currentTime"].as_f64() {
            p.position = Some(position.round());
        }
        let media = &status["media"];
        if media.is_object() {
            let meta = &media["metadata"];
            let text = |key: &str| {
                meta[key]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
            };
            p.title = text("title");
            p.subtitle = text("subtitle")
                .or_else(|| text("seriesTitle"))
                .or_else(|| text("artist"));
            p.image = meta["images"][0]["url"].as_str().map(str::to_owned);
            p.duration = media["duration"]
                .as_f64()
                .filter(|d| *d > 0.0)
                .map(f64::round);
        }
    }
}

/// A message's sender, namespace and JSON payload.
fn envelope(frame: &[u8]) -> Option<(String, String, Json)> {
    let m = parse(frame)?;
    let body = serde_json::from_str(m.str(6)?).ok()?;
    Some((m.str(2)?.to_owned(), m.str(4)?.to_owned(), body))
}

async fn send<W: AsyncWrite + Unpin>(
    write: &mut W,
    destination: &str,
    namespace: &str,
    payload: &Json,
) -> anyhow::Result<()> {
    let message = Writer::new()
        .uint(1, 0)
        .str(2, SENDER)
        .str(3, destination)
        .str(4, namespace)
        .uint(5, 0)
        .str(6, &payload.to_string())
        .finish();
    let len = u32::try_from(message.len()).context("message too long")?;
    let mut frame = len.to_be_bytes().to_vec();
    frame.extend_from_slice(&message);
    write.write_all(&frame).await?;
    write.flush().await?;
    Ok(())
}

async fn read_frame<R: AsyncRead + Unpin>(read: &mut R) -> anyhow::Result<Vec<u8>> {
    let len = usize::try_from(read.read_u32().await?)?;
    if len > MAX_MESSAGE {
        bail!("message of {len} bytes from the TV");
    }
    let mut frame = vec![0; len];
    read.read_exact(&mut frame).await?;
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receiver(apps: &Json) -> Json {
        json!({ "type": "RECEIVER_STATUS", "status": { "applications": apps } })
    }

    #[test]
    fn follows_the_app_then_its_media() {
        let mut f = Follow::default();
        let disney = json!([{ "appId": "x", "displayName": "Disney+", "transportId": "t-1",
            "namespaces": [{ "name": NS_MEDIA }] }]);
        assert_eq!(f.receiver(&receiver(&disney)).as_deref(), Some("t-1"));
        assert_eq!(
            f.receiver(&receiver(&disney)),
            None,
            "same app: nothing new"
        );
        f.media(&json!({ "type": "MEDIA_STATUS", "status": [{
            "mediaSessionId": 7, "playerState": "PLAYING", "currentTime": 12.4,
            "media": { "duration": 496.0, "metadata": { "title": "Bluey", "seriesTitle": "Bluey S2",
                "images": [{ "url": "https://img.example/b.jpg" }] } } }] }));
        assert_eq!(
            f.playing,
            Playing {
                app: Some("Disney+".into()),
                state: Some("playing".into()),
                title: Some("Bluey".into()),
                subtitle: Some("Bluey S2".into()),
                image: Some("https://img.example/b.jpg".into()),
                duration: Some(496.0),
                position: Some(12.0),
                volume: None,
                muted: None,
            }
        );
        assert_eq!(f.session, Some(7));
        // A partial status (no media): the title stays.
        f.media(&json!({ "status": [{ "mediaSessionId": 7, "playerState": "PAUSED", "currentTime": 30.0 }] }));
        assert_eq!(f.playing.state.as_deref(), Some("paused"));
        assert_eq!(f.playing.title.as_deref(), Some("Bluey"));
        // Back to the home screen: nothing plays, volume is read from the receiver.
        let idle = json!({
            "type": "RECEIVER_STATUS",
            "status": {
                "volume": { "level": 0.35, "muted": false },
                "applications": [{ "displayName": "Backdrop", "isIdleScreen": true, "transportId": "t-0" }]
            }
        });
        assert_eq!(f.receiver(&idle), None);
        assert_eq!(
            f.playing,
            Playing {
                volume: Some(35.0),
                muted: Some(false),
                ..Playing::default()
            }
        );
        assert_eq!(f.transport, None);
    }

    #[tokio::test]
    async fn speaks_and_waits_until_finished() {
        let (client, mut speaker) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move {
            // CONNECT + LAUNCH on receiver-0
            let (_, ns1, b1) = envelope(&read_frame(&mut speaker).await.unwrap()).unwrap();
            assert_eq!(
                (ns1.as_str(), b1["type"].as_str()),
                (NS_CONNECTION, Some("CONNECT"))
            );
            let (_, ns2, b2) = envelope(&read_frame(&mut speaker).await.unwrap()).unwrap();
            assert_eq!(
                (ns2.as_str(), b2["type"].as_str()),
                (NS_RECEIVER, Some("LAUNCH"))
            );
            assert_eq!(b2["appId"].as_str(), Some(DEFAULT_MEDIA_RECEIVER));
            send(
                &mut speaker,
                SENDER,
                NS_RECEIVER,
                &json!({
                    "type": "RECEIVER_STATUS",
                    "status": {
                        "applications": [{
                            "appId": DEFAULT_MEDIA_RECEIVER,
                            "displayName": "Default Media Receiver",
                            "transportId": "web-9",
                            "namespaces": [{ "name": NS_MEDIA }]
                        }]
                    }
                }),
            )
            .await
            .unwrap();
            // CONNECT + LOAD on web-9
            let (_, ns3, b3) = envelope(&read_frame(&mut speaker).await.unwrap()).unwrap();
            assert_eq!(
                (ns3.as_str(), b3["type"].as_str()),
                (NS_CONNECTION, Some("CONNECT"))
            );
            let (_, ns4, b4) = envelope(&read_frame(&mut speaker).await.unwrap()).unwrap();
            assert_eq!(
                (ns4.as_str(), b4["type"].as_str()),
                (NS_MEDIA, Some("LOAD"))
            );
            assert_eq!(
                b4["media"]["contentId"].as_str(),
                Some("http://192.168.1.x:8790/api/voice/abc.flac")
            );
            send(
                &mut speaker,
                SENDER,
                NS_MEDIA,
                &json!({ "type": "MEDIA_STATUS", "status": [{ "mediaSessionId": 1, "playerState": "PLAYING" }] }),
            )
            .await
            .unwrap();
            send(
                &mut speaker,
                SENDER,
                NS_MEDIA,
                &json!({ "type": "MEDIA_STATUS", "status": [{ "mediaSessionId": 1, "playerState": "IDLE", "idleReason": "FINISHED" }] }),
            )
            .await
            .unwrap();
        });
        speak_on(
            client,
            "http://192.168.1.x:8790/api/voice/abc.flac",
            "audio/flac",
        )
        .await
        .unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn envelopes_round_trip() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        send(
            &mut a,
            "t-1",
            NS_MEDIA,
            &json!({ "type": "PAUSE", "mediaSessionId": 7 }),
        )
        .await
        .unwrap();
        let frame = read_frame(&mut b).await.unwrap();
        let m = parse(&frame).unwrap();
        assert_eq!(m.str(3), Some("t-1"));
        assert_eq!(m.str(4), Some(NS_MEDIA));
        // The sender is us: an envelope read back names it.
        let (source, namespace, body) = envelope(&frame).unwrap();
        assert_eq!((source.as_str(), namespace.as_str()), (SENDER, NS_MEDIA));
        assert_eq!(body["mediaSessionId"], 7);
    }
}
