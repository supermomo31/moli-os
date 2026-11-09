//! The home's music service. A driver (a `qobuz` instance) registers it on
//! the hub while it runs ([`crate::Hub::set_music`]), and the speaker drivers
//! ask it to turn a search phrase into stream addresses their devices can
//! play, at each `play` order.

use crate::BoxFuture;

/// One track ready to stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    /// Service-specific track identifier (e.g. Qobuz track ID).
    pub id: u64,
    pub title: String,
    pub artist: String,
    /// A picture of the album (a public address), if the service gives one.
    pub image: Option<String>,
    /// Where the audio is fetched.
    pub url: String,
    /// `audio/mpeg`, `audio/flac`…
    pub mime: String,
}

/// What a speaker asks of the home's music service: the tracks a search
/// phrase resolves to, in play order.
pub trait MusicService: Send + Sync + std::fmt::Debug {
    /// What `phrase` plays, first track first.
    fn resolve(&self, phrase: &str) -> BoxFuture<'_, Result<Vec<Track>, String>>;

    /// The stream formats the service can resolve, as its own identifiers
    /// (Qobuz's `format_id`), best last. Empty: not tunable.
    fn formats(&self) -> &'static [u32] {
        &[]
    }

    /// The stream format `resolve` currently uses, when tunable.
    fn format(&self) -> Option<u32> {
        None
    }

    /// Changes the stream format for the next `resolve`; `false` when the
    /// service does not know it or does not let it be tuned.
    fn set_format(&self, format: u32) -> bool {
        let _ = format;
        false
    }
}
