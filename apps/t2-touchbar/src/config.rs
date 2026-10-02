// SPDX-License-Identifier: GPL-3.0-or-later

use std::{env, fs, path::PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const DEFAULT_MIN_TIMEOUT_MS: u64 = 5_000;
pub const DEFAULT_MAX_TIMEOUT_MS: u64 = 30_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub minimum_timeout_ms: u64,
    pub maximum_timeout_ms: u64,
    pub continuation_window_ms: u64,
    pub fn_long_press_ms: u64,
    pub active_brightness: u32,
    pub haptic_feedback: bool,
    pub key_color: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            minimum_timeout_ms: DEFAULT_MIN_TIMEOUT_MS,
            maximum_timeout_ms: DEFAULT_MAX_TIMEOUT_MS,
            continuation_window_ms: 15_000,
            fn_long_press_ms: 600,
            active_brightness: 128,
            haptic_feedback: true,
            key_color: "#dce6ff".into(),
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path();
        let mut config = match fs::read_to_string(&path) {
            Ok(body) => toml::from_str(&body)
                .with_context(|| format!("invalid configuration {}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.minimum_timeout_ms >= DEFAULT_MIN_TIMEOUT_MS,
            "minimum_timeout_ms must be at least {DEFAULT_MIN_TIMEOUT_MS}"
        );
        anyhow::ensure!(
            self.maximum_timeout_ms >= self.minimum_timeout_ms,
            "maximum_timeout_ms must not be below minimum_timeout_ms"
        );
        anyhow::ensure!(
            self.fn_long_press_ms >= 300,
            "fn_long_press_ms is too short"
        );
        anyhow::ensure!(
            self.active_brightness <= 255,
            "active_brightness must be 0..255"
        );
        parse_color(&self.key_color)
            .with_context(|| format!("key_color {:?} must be #rrggbb", self.key_color))?;
        self.continuation_window_ms = self.continuation_window_ms.max(1_000);
        Ok(())
    }

    pub fn key_color(&self) -> u32 {
        parse_color(&self.key_color).unwrap_or(0x00dce6ff)
    }
}

fn parse_color(value: &str) -> Option<u32> {
    let hex = value.strip_prefix('#')?;
    (hex.len() == 6)
        .then(|| u32::from_str_radix(hex, 16).ok())
        .flatten()
}

pub fn config_path() -> PathBuf {
    if let Some(path) = env::var_os("KAIT2EN_TOUCHBAR_CONFIG") {
        return path.into();
    }
    let user_path = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config")
        })
        .join("kait2en-touchbar/config.toml");
    if user_path.exists() {
        user_path
    } else {
        PathBuf::from("/etc/kait2en/touchbar.toml")
    }
}

pub fn state_path() -> PathBuf {
    if let Some(path) = env::var_os("KAIT2EN_TOUCHBAR_STATE") {
        return path.into();
    }
    env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local/state")
        })
        .join("kait2en-touchbar/state.toml")
}
