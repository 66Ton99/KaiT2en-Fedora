// SPDX-License-Identifier: GPL-3.0-or-later

//! Seeking in the active media player. Desktops treat the rewind and fast
//! forward keys inconsistently, MPRIS `Seek` works with every player.

use std::time::Duration;

use anyhow::Result;
use zbus::blocking::{Connection, connection::Builder};
use zbus::zvariant::OwnedValue;

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
