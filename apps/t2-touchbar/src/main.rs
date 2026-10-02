// SPDX-License-Identifier: GPL-3.0-or-later

mod backlight;
mod config;
mod display;
mod haptic;
mod keyboard;
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
        unix::fs::OpenOptionsExt,
    },
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

use anyhow::{Context, Result};
use input::{
    Device as InputDevice, Libinput, LibinputInterface,
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
use haptic::Haptic;
use keyboard::VirtualKeyboard;
use policy::{PersistentState, TimeoutLearner};
use renderer::{Action, Canvas, Layout};
use touchid::TouchIdState;

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
    touches_armed: bool,
    active_button: Option<usize>,
    layout: Layout,
    canvas: Canvas,
    display: Display,
    backlight: Backlight,
    keyboard: VirtualKeyboard,
    haptic: Haptic,
    physical_escape: bool,
    animation_phase: u8,
    animation_rising: bool,
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
            touches_armed: true,
            active_button: None,
            layout,
            canvas,
            display,
            backlight,
            keyboard: VirtualKeyboard::open()?,
            haptic,
            physical_escape,
            animation_phase: 0,
            animation_rising: true,
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
        if !self.visible {
            self.learner.wake(now, &self.config);
        }
        self.visible = true;
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
        self.contacts.clear();
        self.touches_armed = true;
        self.deadline_ms = None;
        if self.visible && learned_off {
            self.learner
                .auto_off(self.now_ms(), self.state.mode, &self.config);
        }
        self.visible = false;
        self.canvas.clear();
        self.display
            .present(&self.canvas.pixels, self.canvas.width, self.canvas.height)?;
        self.backlight.set(0)?;
        Ok(())
    }

    fn touch_down(&mut self, slot: u32, x: f64, y: f64) -> Result<()> {
        if self.touch_id != TouchIdState::Idle {
            self.contacts.insert(slot, Contact::Cancelled);
            return Ok(());
        }
        if !self.visible {
            self.contacts.insert(slot, Contact::Wake);
            return self.wake(true);
        }
        if !self.touches_armed || self.active_button.is_some() {
            self.contacts.insert(slot, Contact::Cancelled);
            return Ok(());
        }
        let Some(index) = self.layout.hit(x, y, self.canvas.height) else {
            self.contacts.insert(slot, Contact::Cancelled);
            return Ok(());
        };
        let Action::Key(key) = self.layout.buttons[index].action;
        self.keyboard.press(key)?;
        self.haptic.click();
        self.active_button = Some(index);
        self.contacts.insert(slot, Contact::Button(index));
        let now = self.now_ms();
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
        let Some(Contact::Button(index)) = self.contacts.get(&slot).copied() else {
            return Ok(());
        };
        if self.layout.hit(x, y, self.canvas.height) != Some(index) {
            self.keyboard.release()?;
            self.active_button = None;
            self.contacts.insert(slot, Contact::Cancelled);
            self.present_keys()?;
        }
        Ok(())
    }

    fn touch_up(&mut self, slot: u32) -> Result<()> {
        if matches!(self.contacts.remove(&slot), Some(Contact::Button(_))) {
            self.keyboard.release()?;
            self.active_button = None;
            self.present_keys()?;
        }
        if self.contacts.is_empty() {
            self.touches_armed = true;
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
        self.contacts.clear();
        self.touches_armed = false;
        self.deadline_ms = None;
        if state == TouchIdState::Idle {
            self.next_animation_ms = None;
            return self.go_dark(false);
        }
        self.visible = true;
        self.animation_phase = 8;
        self.animation_rising = false;
        self.next_animation_ms = matches!(state, TouchIdState::Waiting | TouchIdState::Scanning)
            .then(|| self.now_ms() + animation_interval(state));
        self.render_touch_id()?;
        self.backlight.set(self.config.active_brightness)
    }

    fn render_touch_id(&mut self) -> Result<()> {
        self.canvas
            .touch_id(self.touch_id.as_str(), self.animation_phase);
        self.display
            .present(&self.canvas.pixels, self.canvas.width, self.canvas.height)
    }

    fn tick(&mut self) -> Result<()> {
        let now = self.now_ms();
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
            if self.animation_rising {
                self.animation_phase += 1;
                if self.animation_phase >= 8 {
                    self.animation_rising = false;
                }
            } else {
                self.animation_phase = self.animation_phase.saturating_sub(1);
                if self.animation_phase == 0 {
                    self.animation_rising = true;
                }
            }
            self.render_touch_id()?;
            self.next_animation_ms = Some(now + animation_interval(self.touch_id));
        }
        Ok(())
    }

    fn next_timeout(&self) -> i32 {
        let now = self.now_ms();
        let mut deadlines = Vec::new();
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
        deadlines
            .into_iter()
            .min()
            .map(|deadline| deadline.saturating_sub(now).min(i32::MAX as u64) as i32)
            .unwrap_or(-1)
    }
}

fn animation_interval(state: TouchIdState) -> u64 {
    if state == TouchIdState::Scanning {
        55
    } else {
        110
    }
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
                if let Event::Keyboard(KeyboardEvent::Key(key)) = event
                    && key.key() == Key::Fn as u32
                {
                    runtime.fn_event(key.key_state() == KeyState::Pressed)?;
                }
            }
        }
        runtime.tick()?;
    }
    runtime.go_dark(false)?;
    Ok(())
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
