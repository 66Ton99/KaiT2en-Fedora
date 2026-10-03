// SPDX-License-Identifier: GPL-3.0-or-later

//! Media players over MPRIS: seeking in the active player (desktops treat
//! the rewind and fast forward keys inconsistently) and track changes for
//! the dark bar.

use std::{
    collections::HashMap,
    io::Write,
    os::unix::net::UnixStream,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use zbus::blocking::{Connection, MessageIterator, connection::Builder};
use zbus::zvariant::{OwnedValue, Value};
use zbus::{MatchRule, message::Type};

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
/// A hung player must not stall the Touch Bar.
const CALL_TIMEOUT: Duration = Duration::from_millis(200);

#[derive(Default)]
pub struct Mpris {
    connection: Option<Connection>,
}

impl Mpris {
    /// Picks the player a seek should go to: a playing one first, then a
    /// paused one, then any.
    pub fn active_player(&mut self) -> Option<String> {
        let connection = self.connection().ok()?;
        let names: Vec<String> = connection
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "ListNames",
                &(),
            )
            .ok()?
            .body()
            .deserialize()
            .ok()?;
        names
            .into_iter()
            .filter(|name| name.starts_with(PREFIX))
            .map(|name| {
                let rank = match playback_status(connection, &name).as_deref() {
                    Some("Playing") => 0,
                    Some("Paused") => 1,
                    _ => 2,
                };
                (rank, name)
            })
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, name)| name)
    }

    pub fn seek(&mut self, player: &str, offset: Duration, forward: bool) -> Result<()> {
        let micros = offset.as_micros() as i64;
        let micros = if forward { micros } else { -micros };
        self.connection()?
            .call_method(Some(player), PATH, Some(PLAYER), "Seek", &(micros,))?;
        Ok(())
    }

    fn connection(&mut self) -> Result<&Connection> {
        if self.connection.is_none() {
            self.connection = Some(Builder::session()?.method_timeout(CALL_TIMEOUT).build()?);
        }
        Ok(self
            .connection
            .as_ref()
            .expect("connection was just created"))
    }
}

fn playback_status(connection: &Connection, name: &str) -> Option<String> {
    let value: OwnedValue = connection
        .call_method(
            Some(name),
            PATH,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(PLAYER, "PlaybackStatus"),
        )
        .ok()?
        .body()
        .deserialize()
        .ok()?;
    String::try_from(value).ok()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Track {
    pub artist: String,
    pub title: String,
}

/// Reports a track once it is playing and differs from the last reported
/// one. Players repeat their metadata often (cover art, position updates),
/// so duplicates are dropped here.
pub fn watch_tracks() -> Result<(Receiver<Track>, UnixStream)> {
    let (sender, receiver) = mpsc::channel();
    let (mut wake_writer, wake_reader) = UnixStream::pair()?;
    thread::Builder::new()
        .name("mpris-tracks".to_owned())
        .spawn(move || {
            let mut reported = false;
            loop {
                if let Err(error) = listen_tracks(&sender, &mut wake_writer) {
                    if !reported {
                        eprintln!("kait2en-touchbar: track listener: {error:#}");
                        reported = true;
                    }
                    thread::sleep(Duration::from_secs(30));
                }
            }
        })?;
    Ok((receiver, wake_reader))
}

#[derive(Default)]
struct PlayerState {
    playing: bool,
    track: Option<Track>,
}

fn listen_tracks(sender: &Sender<Track>, wake: &mut UnixStream) -> Result<()> {
    let connection = Builder::session()?
        .method_timeout(CALL_TIMEOUT)
        .build()
        .context("connect session bus")?;
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .path(PATH)?
        .interface("org.freedesktop.DBus.Properties")?
        .member("PropertiesChanged")?
        .arg(0, PLAYER)?
        .build();
    let iterator = MessageIterator::for_match_rule(rule, &connection, None)?;
    let mut players: HashMap<String, PlayerState> = HashMap::new();
    let mut last: Option<Track> = None;
    for message in iterator {
        let message = message?;
        let Some(name) = message.header().sender().map(ToString::to_string) else {
            continue;
        };
        let Ok((_, changed, _)) = message
            .body()
            .deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>()
        else {
            continue;
        };
        let known = players.contains_key(&name);
        let state = players.entry(name.clone()).or_default();
        if let Some(status) = changed.get("PlaybackStatus") {
            state.playing = matches!(&**status, Value::Str(status) if status.as_str() == "Playing");
        } else if !known {
            state.playing = playback_status(&connection, &name).as_deref() == Some("Playing");
        }
        if let Some(metadata) = changed.get("Metadata") {
            state.track = parse_track(metadata);
        }
        let Some(track) = state.track.clone().filter(|_| state.playing) else {
            continue;
        };
        if last.as_ref() == Some(&track) {
            continue;
        }
        last = Some(track.clone());
        if sender.send(track).is_err() {
            return Ok(());
        }
        let _ = wake.write_all(&[1]);
    }
    Err(anyhow::anyhow!("session bus iterator ended"))
}

fn parse_track(metadata: &OwnedValue) -> Option<Track> {
    let Value::Dict(dict) = unwrap_variant(metadata) else {
        return None;
    };
    let mut artist = String::new();
    let mut title = String::new();
    for (key, value) in dict.iter() {
        let Value::Str(key) = unwrap_variant(key) else {
            continue;
        };
        match (key.as_str(), unwrap_variant(value)) {
            ("xesam:title", Value::Str(value)) => title = value.to_string(),
            ("xesam:artist", Value::Array(values)) => {
                artist = values
                    .iter()
                    .filter_map(|value| match unwrap_variant(value) {
                        Value::Str(name) => Some(name.to_string()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
            }
            // Some players send a single string instead of the list.
            ("xesam:artist", Value::Str(value)) => artist = value.to_string(),
            _ => {}
        }
    }
    let (artist, title) = (artist.trim().to_owned(), title.trim().to_owned());
    (!title.is_empty()).then_some(Track { artist, title })
}

fn unwrap_variant<'a>(value: &'a Value<'a>) -> &'a Value<'a> {
    match value {
        Value::Value(inner) => unwrap_variant(inner),
        value => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(entries: Vec<(&str, Value<'static>)>) -> OwnedValue {
        let map: HashMap<String, Value<'static>> = entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect();
        OwnedValue::try_from(Value::from(map)).unwrap()
    }

    #[test]
    fn artist_list_and_title_are_read() {
        let value = metadata(vec![
            ("xesam:title", Value::from("Duality")),
            (
                "xesam:artist",
                Value::from(vec!["Slipknot", "Corey Taylor"]),
            ),
            ("mpris:length", Value::from(252_000_000i64)),
        ]);
        assert_eq!(
            parse_track(&value),
            Some(Track {
                artist: "Slipknot, Corey Taylor".to_owned(),
                title: "Duality".to_owned(),
            })
        );
    }

    #[test]
    fn missing_title_is_no_track() {
        let value = metadata(vec![("xesam:artist", Value::from("Heino"))]);
        assert_eq!(parse_track(&value), None);
    }
}
