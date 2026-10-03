// SPDX-License-Identifier: GPL-3.0-or-later

mod backlight;
mod config;
mod display;
mod gesture;
mod haptic;
mod kbdlight;
mod keyboard;
mod levels;
mod mpris;
mod policy;
mod renderer;
mod touchid;
mod usb;

use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::Read,
    os::{
        fd::{AsFd, AsRawFd, OwnedFd},
        unix::{fs::OpenOptionsExt, net::UnixStream},
    },
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use input::{
    Device as InputDevice, DeviceCapability, Libinput, LibinputInterface, SendEventsMode,
    event::{
        DeviceEvent, Event, EventTrait,
        keyboard::{KeyState, KeyboardEvent, KeyboardEventTrait},
        touch::{TouchEvent, TouchEventPosition, TouchEventSlot},
    },
};
use input_linux::Key;
use libc::{O_ACCMODE, O_RDONLY, O_RDWR, O_WRONLY, pollfd};

use backlight::Backlight;
use config::{Config, state_path};
use display::Display;
use gesture::{Swipe, SwipeKind};
use haptic::Haptic;
use keyboard::VirtualKeyboard;
use levels::{Level, Reader as LevelReader};
use mpris::Mpris;
use policy::{PersistentState, TimeoutLearner};
use renderer::{Action, Canvas, Layout, LevelKind};
use touchid::TouchIdState;

/// How long the esc key stays lit after it was used on the dark bar, so even
/// a quick tap is visible.
const ESC_LINGER_MS: u64 = 150;
/// Swipe feedback stays a little after the fingers lift to show the result.
const LEVEL_LINGER_MS: u64 = 700;
/// The desktop applies a volume or brightness key asynchronously, so the
/// value is read back repeatedly for a short while after each step.
const LEVEL_POLL_MS: u64 = 80;
const LEVEL_POLL_WINDOW_MS: u64 = 600;
/// Holding previous/next this long from touch-down starts seeking.
/// The Touch ID arrow swings towards the sensor once per period, at about
/// 30 fps.
const ARROW_PERIOD_MS: u64 = 1_500;
const ARROW_FRAME_MS: u64 = 33;
const MEDIA_HOLD_MS: u64 = 450;
const SEEK_INTERVAL_MS: u64 = 250;
/// Seek steps grow after this long, so long distances stay quick.
const SEEK_FAST_AFTER_MS: u64 = 1_500;
const SEEK_STEP: Duration = Duration::from_secs(5);
const SEEK_FAST_STEP: Duration = Duration::from_secs(15);

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn stop(_: i32) {
    STOP.store(true, Ordering::Relaxed);
}

struct Interface;

impl LibinputInterface for Interface {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> std::result::Result<OwnedFd, i32> {
        let mode = flags & O_ACCMODE;
        OpenOptions::new()
            .custom_flags(flags)
            .read(mode == O_RDONLY || mode == O_RDWR)
            .write(mode == O_WRONLY || mode == O_RDWR)
            .open(path)
            .map(Into::into)
            .map_err(|error| error.raw_os_error().unwrap_or(libc::EIO))
    }

    fn close_restricted(&mut self, fd: OwnedFd) {
        drop(File::from(fd));
    }
}

#[derive(Clone, Copy, Debug)]
enum Contact {
    Wake,
    Button(usize),
    Cancelled,
    /// First finger, waiting whether a second one turns this into a swipe.
    Pending,
    Swipe,
    /// Esc held on the dark bar; the bar stays dark.
    DarkEsc,
    /// Previous/next held: a tap on release, seeking once held long enough.
    MediaHold(usize),
}

/// Previous/next under a finger. Released early it skips the track, held
/// past `MEDIA_HOLD_MS` it seeks in steps until released.
struct MediaHold {
    forward: bool,
    next_ms: u64,
    seeking_since: Option<u64>,
    player: Option<String>,
}

/// A first touch held back for `two_finger_window_ms`. On the dark bar it
/// becomes a wake, on the lit bar a key press, unless a second finger joins.
#[derive(Clone, Copy, Debug)]
struct PendingTouch {
    slot: u32,
    deadline_ms: u64,
    dark: bool,
    button: Option<usize>,
}

struct Runtime {
    config: Config,
    state: PersistentState,
    state_path: std::path::PathBuf,
    learner: TimeoutLearner,
    started: Instant,
    visible: bool,
    touch_id: TouchIdState,
    deadline_ms: Option<u64>,
    fn_down_ms: Option<u64>,
    fn_toggled: bool,
    contacts: HashMap<u32, Contact>,
    positions: HashMap<u32, f64>,
    pending: Option<PendingTouch>,
    swipe: Option<Swipe>,
    /// The dark bar currently lights only the esc key.
    overlay_shown: bool,
    overlay_hide_ms: Option<u64>,
    /// Level feedback while a dark-bar swipe runs.
    level_kind: Option<LevelKind>,
    level: Level,
    /// Leftmost and rightmost finger the feedback was last placed against.
    level_fingers: (f64, f64),
    level_poll_ms: Option<u64>,
    level_poll_until_ms: u64,
    /// The level feedback changed and is drawn once the pending input is
    /// processed, so a burst of touch events costs one frame.
    level_dirty: bool,
    /// Last values seen per kind, shown until a fresh read arrives.
    level_cache: [Level; 2],
    level_reader: LevelReader,
    level_wake: UnixStream,
    key_level: u32,
    media_hold: Option<MediaHold>,
    mpris: Mpris,
    touches_armed: bool,
    active_button: Option<usize>,
    layout: Layout,
    canvas: Canvas,
    display: Display,
    backlight: Backlight,
    keyboard: VirtualKeyboard,
    haptic: Haptic,
    physical_escape: bool,
    animation_start_ms: u64,
    next_animation_ms: Option<u64>,
}

impl Runtime {
    fn new(config: Config, state: PersistentState) -> Result<Self> {
        let mut display = Display::open()?;
        let (width, height) = display.dimensions();
        let canvas = Canvas::new(width, height, config.key_color())?;
        display.present(&canvas.pixels, width, height)?;
        let mut backlight = Backlight::open()?;
        backlight.set(0)?;
        let physical_escape = has_physical_escape();
        let layout = Layout::new(state.mode, width, physical_escape);
        let haptic = Haptic::open(config.haptic_feedback);
        let (level_reader, level_wake) = LevelReader::spawn()?;
        level_wake.set_nonblocking(true)?;
        Ok(Self {
            config,
            state,
            state_path: state_path(),
            learner: TimeoutLearner::new(),
            started: Instant::now(),
            visible: false,
            touch_id: TouchIdState::Idle,
            deadline_ms: None,
            fn_down_ms: None,
            fn_toggled: false,
            contacts: HashMap::new(),
            positions: HashMap::new(),
            pending: None,
            swipe: None,
            overlay_shown: false,
            overlay_hide_ms: None,
            level_kind: None,
            level: Level::default(),
            level_fingers: (0.0, 0.0),
            level_poll_ms: None,
            level_poll_until_ms: 0,
            level_dirty: false,
            level_cache: [Level::default(); 2],
            level_reader,
            level_wake,
            key_level: 100,
            media_hold: None,
            mpris: Mpris::default(),
            touches_armed: true,
            active_button: None,
            layout,
            canvas,
            display,
            backlight,
            keyboard: VirtualKeyboard::open()?,
            haptic,
            physical_escape,
            animation_start_ms: 0,
            next_animation_ms: None,
        })
    }

    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn save(&self) {
        if let Err(error) = self.state.save(&self.state_path) {
            eprintln!("kait2en-touchbar: could not save state: {error:#}");
        }
    }

    fn present_keys(&mut self) -> Result<()> {
        self.layout = Layout::new(self.state.mode, self.canvas.width, self.physical_escape);
        self.canvas.keys(&self.layout, self.active_button);
        self.display
            .present(&self.canvas.pixels, self.canvas.width, self.canvas.height)
    }

    fn wake(&mut self, quarantine_touch: bool) -> Result<()> {
        if self.touch_id != TouchIdState::Idle {
            return Ok(());
        }
        let now = self.now_ms();
        self.refresh_key_level();
        if !self.visible {
            self.learner.wake(now, &self.config);
        }
        self.visible = true;
        self.overlay_shown = false;
        self.overlay_hide_ms = None;
        self.level_kind = None;
        self.level_poll_ms = None;
        if quarantine_touch {
            self.touches_armed = false;
        }
        self.deadline_ms = Some(now + self.state.learned(self.state.mode).timeout_ms);
        self.present_keys()?;
        self.backlight.set(self.config.active_brightness)?;
        Ok(())
    }

    fn go_dark(&mut self, learned_off: bool) -> Result<()> {
        self.keyboard.release()?;
        self.active_button = None;
        self.clear_touches();
        self.touches_armed = true;
        self.deadline_ms = None;
        if self.visible && learned_off {
            self.learner
                .auto_off(self.now_ms(), self.state.mode, &self.config);
        }
        self.visible = false;
        self.overlay_shown = false;
        self.overlay_hide_ms = None;
        self.level_kind = None;
        self.level_poll_ms = None;
        self.canvas.clear();
        self.display
            .present(&self.canvas.pixels, self.canvas.width, self.canvas.height)?;
        self.backlight.set(0)?;
        Ok(())
    }

    fn clear_touches(&mut self) {
        self.contacts.clear();
        self.positions.clear();
        self.pending = None;
        self.swipe = None;
        self.media_hold = None;
    }

    fn touch_down(&mut self, slot: u32, x: f64, y: f64) -> Result<()> {
        let now = self.now_ms();
        self.resolve_pending(now)?;
        self.positions.insert(slot, x);
        if self.touch_id == TouchIdState::Idle
            && self
                .swipe
                .is_some_and(|swipe| swipe.kind == SwipeKind::Volume && !swipe.fired())
        {
            return self.start_brightness_swipe(slot);
        }
        if self.touch_id != TouchIdState::Idle || self.swipe.is_some() {
            self.contacts.insert(slot, Contact::Cancelled);
            return Ok(());
        }
        if let Some(pending) = self.pending.take() {
            return self.start_swipe(pending, slot);
        }
        if !self.contacts.is_empty()
            || (self.visible && (!self.touches_armed || self.active_button.is_some()))
        {
            self.contacts.insert(slot, Contact::Cancelled);
            return Ok(());
        }
        let button = self.layout.hit(x, y, self.canvas.height);
        // The esc slot never moves, so it stays usable while the bar is dark.
        let button = if self.visible {
            button
        } else {
            button.filter(|&index| self.is_esc(index))
        };
        self.pending = Some(PendingTouch {
            slot,
            deadline_ms: now + self.config.two_finger_window_ms,
            dark: !self.visible,
            button,
        });
        self.contacts.insert(slot, Contact::Pending);
        if let Some(index) = button {
            if self.visible {
                // Highlight at once; only the key event waits for the window.
                self.active_button = button;
                self.present_keys()?;
            } else {
                self.show_dark_esc(index)?;
            }
        }
        Ok(())
    }

    fn start_swipe(&mut self, pending: PendingTouch, slot: u32) -> Result<()> {
        let (kind, step) = if pending.dark {
            (SwipeKind::Volume, self.config.volume_swipe_step_px)
        } else {
            (SwipeKind::Mode, self.config.mode_swipe_px)
        };
        self.contacts.insert(pending.slot, Contact::Swipe);
        self.contacts.insert(slot, Contact::Swipe);
        self.swipe = Some(Swipe::new(kind, self.swipe_centroid(), f64::from(step)));
        if pending.dark {
            return self.begin_level(LevelKind::Volume);
        }
        if pending.button.is_some() {
            self.active_button = None;
            self.present_keys()?;
        }
        Ok(())
    }

    /// A third finger joining a dark-bar swipe before any volume step turns
    /// it into a brightness swipe.
    fn start_brightness_swipe(&mut self, slot: u32) -> Result<()> {
        self.contacts.insert(slot, Contact::Swipe);
        self.swipe = Some(Swipe::new(
            SwipeKind::Brightness,
            self.swipe_centroid(),
            f64::from(self.config.brightness_swipe_step_px),
        ));
        self.begin_level(LevelKind::Brightness)
    }

    fn swipe_span(&self) -> (f64, f64) {
        let xs = self
            .contacts
            .iter()
            .filter(|(_, contact)| matches!(contact, Contact::Swipe))
            .filter_map(|(slot, _)| self.positions.get(slot).copied());
        xs.fold((f64::MAX, f64::MIN), |(first, last), x| {
            (first.min(x), last.max(x))
        })
    }

    fn swipe_centroid(&self) -> f64 {
        let xs: Vec<f64> = self
            .contacts
            .iter()
            .filter(|(_, contact)| matches!(contact, Contact::Swipe))
            .filter_map(|(slot, _)| self.positions.get(slot).copied())
            .collect();
        xs.iter().sum::<f64>() / xs.len().max(1) as f64
    }

    /// Turns a first touch whose window has expired into its single-finger
    /// meaning: a wake on the dark bar, a held key on the lit bar.
    fn resolve_pending(&mut self, now: u64) -> Result<()> {
        let Some(pending) = self.pending.filter(|pending| now >= pending.deadline_ms) else {
            return Ok(());
        };
        self.pending = None;
        if pending.dark && pending.button.is_some() {
            self.contacts.insert(pending.slot, Contact::DarkEsc);
            self.keyboard.press(Key::Esc)?;
            self.haptic.click();
            return Ok(());
        }
        if pending.dark {
            self.contacts.insert(pending.slot, Contact::Wake);
            return self.wake(true);
        }
        match pending.button {
            Some(index) if self.media_direction(index).is_some() => {
                // Stays highlighted; the action is decided on release or hold.
                let touched = pending.deadline_ms - self.config.two_finger_window_ms;
                self.contacts
                    .insert(pending.slot, Contact::MediaHold(index));
                self.media_hold = Some(MediaHold {
                    forward: self.media_direction(index) == Some(true),
                    next_ms: touched + MEDIA_HOLD_MS,
                    seeking_since: None,
                    player: None,
                });
                self.deadline_ms = Some(now + self.state.learned(self.state.mode).timeout_ms);
                Ok(())
            }
            Some(index) => {
                self.contacts.insert(pending.slot, Contact::Button(index));
                self.press_button(index, now)
            }
            None => {
                self.contacts.insert(pending.slot, Contact::Cancelled);
                Ok(())
            }
        }
    }

    fn show_dark_esc(&mut self, index: usize) -> Result<()> {
        self.refresh_key_level();
        self.level_kind = None;
        self.overlay_hide_ms = None;
        self.canvas.single_key(&self.layout, index);
        self.present_overlay()
    }

    fn begin_level(&mut self, kind: LevelKind) -> Result<()> {
        self.refresh_key_level();
        self.level_kind = Some(kind);
        self.level = self.level_cache[cache_slot(kind)];
        self.level_reader.request(kind);
        self.level_fingers = self.swipe_span();
        self.overlay_hide_ms = None;
        self.level_dirty = true;
        Ok(())
    }

    /// Draws the level feedback if anything changed since the last frame.
    fn flush_level(&mut self) -> Result<()> {
        if !std::mem::take(&mut self.level_dirty) {
            return Ok(());
        }
        let Some(kind) = self.level_kind.filter(|_| !self.visible) else {
            return Ok(());
        };
        self.canvas.level(
            kind,
            self.level.percent,
            self.level.muted,
            (self.level_fingers.0 as f32, self.level_fingers.1 as f32),
        );
        self.present_overlay()
    }

    /// Shows whatever the canvas holds on the otherwise dark bar.
    fn present_overlay(&mut self) -> Result<()> {
        self.display
            .present(&self.canvas.pixels, self.canvas.width, self.canvas.height)?;
        if !self.overlay_shown {
            self.backlight.set(self.config.active_brightness)?;
            self.overlay_shown = true;
        }
        Ok(())
    }

    fn hide_overlay(&mut self) -> Result<()> {
        self.overlay_hide_ms = None;
        self.level_kind = None;
        self.level_poll_ms = None;
        if !self.overlay_shown || self.visible {
            return Ok(());
        }
        self.overlay_shown = false;
        self.canvas.clear();
        self.display
            .present(&self.canvas.pixels, self.canvas.width, self.canvas.height)?;
        self.backlight.set(0)
    }

    fn poll_level(&mut self, now: u64) {
        self.level_poll_ms = (now < self.level_poll_until_ms).then_some(now + LEVEL_POLL_MS);
        if let Some(kind) = self.level_kind {
            self.level_reader.request(kind);
        }
    }

    fn levels_read(&mut self) {
        let now = self.now_ms();
        let results: Vec<_> = self.level_reader.results().collect();
        for (kind, level) in results {
            self.level_cache[cache_slot(kind)] = level;
            if self.level_kind != Some(kind) || level == self.level {
                continue;
            }
            self.level = level;
            self.level_dirty = true;
            // Keep a lingering overlay alive until the value has settled.
            self.overlay_hide_ms = self
                .overlay_hide_ms
                .map(|deadline| deadline.max(now + LEVEL_POLL_MS));
        }
    }

    /// Follows the keyboard backlight; returns whether the level changed.
    fn refresh_key_level(&mut self) -> bool {
        let level = if self.config.follow_keyboard_backlight {
            kbdlight::key_level().unwrap_or(100)
        } else {
            100
        };
        if level == self.key_level {
            return false;
        }
        self.key_level = level;
        self.canvas.set_level(level);
        true
    }

    fn keyboard_backlight_changed(&mut self) -> Result<()> {
        if !self.refresh_key_level() || self.touch_id != TouchIdState::Idle {
            return Ok(());
        }
        if self.visible {
            return self.present_keys();
        }
        self.level_dirty = true;
        Ok(())
    }

    /// `Some(true)` for next, `Some(false)` for previous.
    fn media_direction(&self, index: usize) -> Option<bool> {
        match self.layout.buttons[index].action {
            Action::Key(Key::NextSong) => Some(true),
            Action::Key(Key::PreviousSong) => Some(false),
            _ => None,
        }
    }

    fn seek_step(&mut self, now: u64) -> Result<()> {
        let Some(mut hold) = self.media_hold.take() else {
            return Ok(());
        };
        let since = match hold.seeking_since {
            Some(since) => since,
            None => {
                // The hold turned into seeking: no track skip on release.
                hold.player = self.mpris.active_player();
                self.haptic.click();
                if self
                    .learner
                    .action(now, self.state.mode, &mut self.state, &self.config)
                {
                    self.save();
                }
                *hold.seeking_since.insert(now)
            }
        };
        let step = if now - since >= SEEK_FAST_AFTER_MS {
            SEEK_FAST_STEP
        } else {
            SEEK_STEP
        };
        let seeked = hold
            .player
            .as_deref()
            .is_some_and(|player| self.mpris.seek(player, step, hold.forward).is_ok());
        if !seeked {
            // No MPRIS player: leave it to whoever handles the seek keys.
            let key = if hold.forward {
                Key::FastForward
            } else {
                Key::Rewind
            };
            self.keyboard.press(key)?;
            self.keyboard.release()?;
        }
        hold.next_ms = now + SEEK_INTERVAL_MS;
        self.deadline_ms = Some(now + self.state.learned(self.state.mode).timeout_ms);
        self.media_hold = Some(hold);
        Ok(())
    }

    fn is_esc(&self, index: usize) -> bool {
        self.layout.buttons[index].action == Action::Key(Key::Esc)
    }

    fn press_button(&mut self, index: usize, now: u64) -> Result<()> {
        let Action::Key(key) = self.layout.buttons[index].action;
        self.keyboard.press(key)?;
        self.haptic.click();
        self.active_button = Some(index);
        if self
            .learner
            .action(now, self.state.mode, &mut self.state, &self.config)
        {
            self.save();
        }
        self.deadline_ms = Some(now + self.state.learned(self.state.mode).timeout_ms);
        self.present_keys()
    }

    fn touch_motion(&mut self, slot: u32, x: f64, y: f64) -> Result<()> {
        let now = self.now_ms();
        self.resolve_pending(now)?;
        self.positions.insert(slot, x);
        match self.contacts.get(&slot).copied() {
            Some(Contact::Swipe) => self.swipe_moved(now),
            Some(Contact::Pending) => {
                let Some(pending) = self.pending else {
                    return Ok(());
                };
                if let Some(index) = pending.button
                    && self.layout.hit(x, y, self.canvas.height) != Some(index)
                {
                    if pending.dark {
                        // Slid off the dark esc slot: an ordinary wake touch.
                        self.pending = Some(PendingTouch {
                            button: None,
                            ..pending
                        });
                        return self.hide_overlay();
                    }
                    self.pending = None;
                    self.active_button = None;
                    self.contacts.insert(slot, Contact::Cancelled);
                    self.present_keys()?;
                }
                Ok(())
            }
            Some(Contact::Button(index)) => {
                if self.layout.hit(x, y, self.canvas.height) != Some(index) {
                    self.keyboard.release()?;
                    self.active_button = None;
                    self.contacts.insert(slot, Contact::Cancelled);
                    self.present_keys()?;
                }
                Ok(())
            }
            Some(Contact::MediaHold(index)) => {
                if self.layout.hit(x, y, self.canvas.height) != Some(index) {
                    self.media_hold = None;
                    self.active_button = None;
                    self.contacts.insert(slot, Contact::Cancelled);
                    self.present_keys()?;
                }
                Ok(())
            }
            Some(Contact::DarkEsc) => {
                if !self
                    .layout
                    .hit(x, y, self.canvas.height)
                    .is_some_and(|index| self.is_esc(index))
                {
                    self.keyboard.release()?;
                    self.contacts.insert(slot, Contact::Cancelled);
                    self.hide_overlay()?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn swipe_moved(&mut self, now: u64) -> Result<()> {
        let centroid = self.swipe_centroid();
        let Some(swipe) = self.swipe.as_mut() else {
            return Ok(());
        };
        let steps = swipe.advance(centroid);
        let kind = swipe.kind;
        let span = self.swipe_span();
        if self.level_kind.is_some()
            && ((span.0 - self.level_fingers.0).abs() >= 3.0
                || (span.1 - self.level_fingers.1).abs() >= 3.0)
        {
            // The feedback follows the fingers.
            self.level_fingers = span;
            self.level_dirty = true;
        }
        if steps == 0 {
            return Ok(());
        }
        match kind {
            SwipeKind::Volume | SwipeKind::Brightness => {
                self.level_poll_ms = Some(now + LEVEL_POLL_MS);
                self.level_poll_until_ms = now + LEVEL_POLL_WINDOW_MS;
                let key = match (kind, steps > 0) {
                    (SwipeKind::Volume, true) => Key::VolumeUp,
                    (SwipeKind::Volume, false) => Key::VolumeDown,
                    (_, true) => Key::BrightnessUp,
                    (_, false) => Key::BrightnessDown,
                };
                for _ in 0..steps.unsigned_abs() {
                    self.keyboard.press(key)?;
                    self.keyboard.release()?;
                }
                self.haptic.click();
                Ok(())
            }
            SwipeKind::Mode => {
                self.haptic.click();
                self.switch_mode(now)
            }
        }
    }

    fn touch_up(&mut self, slot: u32) -> Result<()> {
        let now = self.now_ms();
        self.resolve_pending(now)?;
        self.positions.remove(&slot);
        match self.contacts.remove(&slot) {
            Some(Contact::Pending) => {
                // Lifted within the window: a plain tap.
                if let Some(pending) = self.pending.take() {
                    if pending.dark && pending.button.is_some() {
                        self.keyboard.press(Key::Esc)?;
                        self.keyboard.release()?;
                        self.haptic.click();
                        self.overlay_hide_ms = Some(now + ESC_LINGER_MS);
                    } else if pending.dark {
                        self.wake(true)?;
                    } else if let Some(index) = pending.button {
                        self.press_button(index, now)?;
                        self.keyboard.release()?;
                        self.active_button = None;
                        self.present_keys()?;
                    }
                }
            }
            Some(Contact::Button(_)) => {
                self.keyboard.release()?;
                self.active_button = None;
                self.present_keys()?;
            }
            Some(Contact::MediaHold(index)) => {
                let seeking = self
                    .media_hold
                    .take()
                    .is_some_and(|hold| hold.seeking_since.is_some());
                if !seeking {
                    self.press_button(index, now)?;
                    self.keyboard.release()?;
                }
                self.active_button = None;
                self.present_keys()?;
            }
            Some(Contact::DarkEsc) => {
                self.keyboard.release()?;
                self.overlay_hide_ms = Some(now + ESC_LINGER_MS);
            }
            Some(Contact::Swipe) => {
                // The centroid would jump to the remaining finger, so the
                // first lift ends the swipe.
                self.swipe = None;
                if self.level_kind.is_some() {
                    self.overlay_hide_ms = Some(now + LEVEL_LINGER_MS);
                }
                for contact in self.contacts.values_mut() {
                    if matches!(contact, Contact::Swipe) {
                        *contact = Contact::Cancelled;
                    }
                }
            }
            _ => {}
        }
        if self.contacts.is_empty() {
            self.touches_armed = true;
            self.swipe = None;
        }
        Ok(())
    }

    fn fn_event(&mut self, pressed: bool) -> Result<()> {
        if self.touch_id != TouchIdState::Idle {
            return Ok(());
        }
        let now = self.now_ms();
        if pressed {
            self.fn_down_ms = Some(now);
            self.fn_toggled = false;
            self.wake(false)?;
        } else {
            self.fn_down_ms = None;
            self.fn_toggled = false;
            if self.visible {
                self.deadline_ms = Some(now + self.state.learned(self.state.mode).timeout_ms);
            }
        }
        Ok(())
    }

    fn toggle_mode_if_due(&mut self, now: u64) -> Result<()> {
        let Some(down) = self.fn_down_ms else {
            return Ok(());
        };
        if self.fn_toggled || !long_press_due(down, now, self.config.fn_long_press_ms) {
            return Ok(());
        }
        self.fn_toggled = true;
        self.switch_mode(now)
    }

    fn switch_mode(&mut self, now: u64) -> Result<()> {
        self.keyboard.release()?;
        self.active_button = None;
        self.state.mode = self.state.mode.toggled();
        self.save();
        self.deadline_ms = Some(now + self.state.learned(self.state.mode).timeout_ms);
        self.present_keys()
    }

    fn touch_id_changed(&mut self, state: TouchIdState) -> Result<()> {
        self.touch_id = state;
        self.keyboard.release()?;
        self.active_button = None;
        self.clear_touches();
        self.touches_armed = false;
        self.deadline_ms = None;
        if state == TouchIdState::Idle {
            self.next_animation_ms = None;
            return self.go_dark(false);
        }
        self.visible = true;
        let now = self.now_ms();
        // Waiting, scanning and retry share the arrow; keep its swing smooth
        // across those transitions.
        if self.next_animation_ms.is_none() {
            self.animation_start_ms = now;
        }
        self.next_animation_ms = None;
        self.render_touch_id(now)?;
        self.backlight.set(self.config.active_brightness)
    }

    /// Draws the prompt; while it shows an arrow, schedules the next frame.
    fn render_touch_id(&mut self, now: u64) -> Result<()> {
        let animated = matches!(
            self.touch_id,
            TouchIdState::Waiting | TouchIdState::Scanning | TouchIdState::Retry
        );
        let nudge = if animated {
            self.next_animation_ms = Some(now + ARROW_FRAME_MS);
            let elapsed = now.saturating_sub(self.animation_start_ms) % ARROW_PERIOD_MS;
            arrow_nudge(elapsed as f32 / ARROW_PERIOD_MS as f32)
        } else {
            self.next_animation_ms = None;
            0.0
        };
        self.canvas.touch_id(self.touch_id.as_str(), nudge);
        self.display
            .present(&self.canvas.pixels, self.canvas.width, self.canvas.height)
    }

    fn tick(&mut self) -> Result<()> {
        let now = self.now_ms();
        self.resolve_pending(now)?;
        if self.level_poll_ms.is_some_and(|deadline| now >= deadline) {
            self.poll_level(now);
        }
        if self.overlay_hide_ms.is_some_and(|deadline| now >= deadline) {
            self.hide_overlay()?;
        }
        if self
            .media_hold
            .as_ref()
            .is_some_and(|hold| now >= hold.next_ms)
        {
            self.seek_step(now)?;
        }
        self.toggle_mode_if_due(now)?;
        if self.touch_id == TouchIdState::Idle
            && self.visible
            && self.deadline_ms.is_some_and(|deadline| now >= deadline)
        {
            self.go_dark(true)?;
        }
        if self.learner.settle(now, &mut self.state, &self.config) {
            self.save();
        }
        if self
            .next_animation_ms
            .is_some_and(|deadline| now >= deadline)
        {
            self.render_touch_id(now)?;
        }
        Ok(())
    }

    fn next_timeout(&self) -> i32 {
        let now = self.now_ms();
        let mut deadlines = Vec::new();
        if let Some(hold) = &self.media_hold {
            deadlines.push(hold.next_ms);
        }
        if let Some(deadline) = self.deadline_ms {
            deadlines.push(deadline);
        }
        if let Some(down) = self.fn_down_ms
            && !self.fn_toggled
        {
            deadlines.push(down + self.config.fn_long_press_ms);
        }
        if let Some(deadline) = self.learner.next_settle_ms(&self.config) {
            deadlines.push(deadline);
        }
        if let Some(deadline) = self.next_animation_ms {
            deadlines.push(deadline);
        }
        if let Some(pending) = self.pending {
            deadlines.push(pending.deadline_ms);
        }
        if let Some(deadline) = self.level_poll_ms {
            deadlines.push(deadline);
        }
        if let Some(deadline) = self.overlay_hide_ms {
            deadlines.push(deadline);
        }
        deadlines
            .into_iter()
            .min()
            .map(|deadline| deadline.saturating_sub(now).min(i32::MAX as u64) as i32)
            .unwrap_or(-1)
    }
}

/// Eased swing: rest at 0, fully nudged halfway through the period.
fn arrow_nudge(phase: f32) -> f32 {
    (1.0 - (phase * std::f32::consts::TAU).cos()) / 2.0
}

fn main() -> Result<()> {
    if std::env::args().any(|arg| arg == "--attach") {
        return usb::ensure_display_configuration();
    }
    if std::env::args().any(|arg| arg == "--detach") {
        return usb::restore_firmware_configuration();
    }
    let config = Config::load()?;
    let state_file = state_path();
    if std::env::args().any(|arg| arg == "--reset-learning") {
        let state = PersistentState::reset(&config);
        state.save(&state_file)?;
        println!("reset {}", state_file.display());
        return Ok(());
    }
    if std::env::args().any(|arg| arg == "--status") {
        let state = PersistentState::load(&state_file, &config);
        println!(
            "mode={:?}\nmedia_timeout_ms={}\nfunction_timeout_ms={}",
            state.mode, state.media.timeout_ms, state.function.timeout_ms
        );
        return Ok(());
    }

    let state = PersistentState::load(&state_file, &config);
    let mut runtime = Runtime::new(config, state)?;
    runtime.go_dark(false)?;

    unsafe {
        libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
    }

    let mut touch_input = Libinput::new_with_udev(Interface);
    let mut main_input = Libinput::new_with_udev(Interface);
    touch_input
        .udev_assign_seat("seat-touchbar")
        .map_err(|()| anyhow::anyhow!("assign Touch Bar seat"))?;
    main_input
        .udev_assign_seat("seat0")
        .map_err(|()| anyhow::anyhow!("assign main seat"))?;
    let (touch_id_rx, mut touch_id_wake) = touchid::watch()?;
    touch_id_wake.set_nonblocking(true)?;
    let mut kbd_wake = kbdlight::watch()?;
    kbd_wake.set_nonblocking(true)?;
    let mut touch_device: Option<InputDevice> = None;

    while !STOP.load(Ordering::Relaxed) {
        let mut poll_fds = [
            pollfd {
                fd: touch_input.as_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            pollfd {
                fd: main_input.as_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            pollfd {
                fd: touch_id_wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            pollfd {
                fd: kbd_wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            pollfd {
                fd: runtime.level_wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let result = unsafe {
            libc::poll(
                poll_fds.as_mut_ptr(),
                poll_fds.len() as _,
                runtime.next_timeout(),
            )
        };
        if result < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return Err(std::io::Error::last_os_error()).context("poll input devices");
        }

        if poll_fds[2].revents & libc::POLLIN != 0 {
            let mut discard = [0u8; 64];
            while touch_id_wake.read(&mut discard).is_ok() {}
            while let Ok(state) = touch_id_rx.try_recv() {
                runtime.touch_id_changed(state)?;
            }
        }

        if poll_fds[3].revents & libc::POLLIN != 0 {
            let mut discard = [0u8; 64];
            while kbd_wake.read(&mut discard).is_ok() {}
            runtime.keyboard_backlight_changed()?;
        }

        if poll_fds[4].revents & libc::POLLIN != 0 {
            let mut discard = [0u8; 64];
            while (&runtime.level_wake).read(&mut discard).is_ok() {}
            runtime.levels_read();
        }

        if poll_fds[0].revents & libc::POLLIN != 0 {
            touch_input.dispatch()?;
            for event in &mut touch_input {
                match event {
                    Event::Device(DeviceEvent::Added(added))
                        if is_touch_bar(added.device().name()) =>
                    {
                        touch_device = Some(added.device());
                    }
                    Event::Touch(event)
                        if touch_device
                            .as_ref()
                            .is_none_or(|device| *device == event.device()) =>
                    {
                        match event {
                            TouchEvent::Down(down) => runtime.touch_down(
                                down.seat_slot(),
                                down.x_transformed(runtime.canvas.width as u32),
                                down.y_transformed(runtime.canvas.height as u32),
                            )?,
                            TouchEvent::Motion(motion) => runtime.touch_motion(
                                motion.seat_slot(),
                                motion.x_transformed(runtime.canvas.width as u32),
                                motion.y_transformed(runtime.canvas.height as u32),
                            )?,
                            TouchEvent::Up(up) => runtime.touch_up(up.seat_slot())?,
                            TouchEvent::Cancel(cancel) => runtime.touch_up(cancel.seat_slot())?,
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }

        if poll_fds[1].revents & libc::POLLIN != 0 {
            main_input.dispatch()?;
            for event in &mut main_input {
                match event {
                    // seat0 hands over every readable device, including the
                    // virtual keyboard this daemon writes to. Only Fn is
                    // needed; closing the rest avoids wakeups from every
                    // pointer motion and our own key events looping back.
                    Event::Device(DeviceEvent::Added(added)) if !provides_fn(&added.device()) => {
                        let _ = added
                            .device()
                            .config_send_events_set_mode(SendEventsMode::DISABLED);
                    }
                    Event::Keyboard(KeyboardEvent::Key(key)) if key.key() == Key::Fn as u32 => {
                        runtime.fn_event(key.key_state() == KeyState::Pressed)?;
                    }
                    _ => {}
                }
            }
        }
        runtime.tick()?;
        runtime.flush_level()?;
    }
    runtime.go_dark(false)?;
    Ok(())
}

fn provides_fn(device: &InputDevice) -> bool {
    device.has_capability(DeviceCapability::Keyboard)
        && device.keyboard_has_key(Key::Fn as u32) == Ok(true)
}

fn cache_slot(kind: LevelKind) -> usize {
    match kind {
        LevelKind::Volume => 0,
        LevelKind::Brightness => 1,
    }
}

fn is_touch_bar(name: &str) -> bool {
    name.contains("Touch Bar") || name.contains("TouchBar")
}

fn long_press_due(down_ms: u64, now_ms: u64, threshold_ms: u64) -> bool {
    now_ms.saturating_sub(down_ms) >= threshold_ms
}

fn has_physical_escape() -> bool {
    let Ok(product) = std::fs::read_to_string("/sys/class/dmi/id/product_name") else {
        return false;
    };
    product
        .trim()
        .strip_prefix("MacBookPro")
        .and_then(|rest| rest.split(',').next())
        .and_then(|major| major.parse::<u32>().ok())
        .is_some_and(|major| major >= 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_bar_names_are_narrow() {
        assert!(is_touch_bar("Apple Inc. Touch Bar Display Touchpad"));
        assert!(!is_touch_bar("Apple Internal Keyboard / Trackpad"));
    }

    #[test]
    fn fn_hold_switches_at_the_configured_boundary() {
        assert!(!long_press_due(1_000, 1_599, 600));
        assert!(long_press_due(1_000, 1_600, 600));
    }
}
