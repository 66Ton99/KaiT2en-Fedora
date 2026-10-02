// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    fs::{File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::Path,
    time::{Duration, Instant},
};

const PLAY: &str = "/sys/module/t2_trackpad_actuator/parameters/play";
const MIN_INTERVAL: Duration = Duration::from_millis(40);

pub struct Haptic {
    file: Option<File>,
    last: Option<Instant>,
    enabled: bool,
}

impl Haptic {
    pub fn open(enabled: bool) -> Self {
        let file = enabled
            .then(|| OpenOptions::new().write(true).open(Path::new(PLAY)).ok())
            .flatten();
        if enabled && file.is_none() {
            eprintln!("kait2en-touchbar: haptic actuator is unavailable");
        }
        Self {
            file,
            last: None,
            enabled,
        }
    }

    pub fn click(&mut self) {
        if !self.enabled || self.last.is_some_and(|last| last.elapsed() < MIN_INTERVAL) {
            return;
        }
        let Some(file) = self.file.as_mut() else {
            return;
        };
        if file
            .seek(SeekFrom::Start(0))
            .and_then(|_| file.write_all(b"light\n"))
            .is_ok()
        {
            self.last = Some(Instant::now());
        }
    }
}
