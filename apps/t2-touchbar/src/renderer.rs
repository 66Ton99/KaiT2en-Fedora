// SPDX-License-Identifier: GPL-3.0-or-later

use std::fs;

use ab_glyph::{Font, FontVec, OutlineCurve, VariableFont};
use anyhow::{Context, Result, anyhow};
use input_linux::Key;
use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap, Rect, Stroke, Transform,
};

use crate::policy::Mode;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Key(Key),
}

#[derive(Clone, Copy)]
enum Glyph {
    Text(&'static str),
    Icon(Icon),
}

#[derive(Clone, Copy)]
enum Icon {
    Brightness(bool),
    Microphone,
    Keyboard(bool),
    Previous,
    PlayPause,
    Next,
    Mute,
    Volume(bool),
}

const KEY_GAP: u16 = 10;
const GROUP_GAP: u16 = 56;

#[derive(Clone, Copy)]
pub struct Button {
    pub left: u16,
    pub right: u16,
    glyph: Glyph,
    pub action: Action,
}

pub struct Layout {
    pub buttons: Vec<Button>,
}

impl Layout {
    pub fn new(mode: Mode, width: u16, physical_escape: bool) -> Self {
        // Keys inside a group sit KEY_GAP apart; groups are separated by the
        // wider GROUP_GAP, like the clusters of a hardware function row.
        let mut groups: Vec<Vec<(Glyph, Key)>> = Vec::new();
        if !physical_escape {
            groups.push(vec![(Glyph::Text("esc"), Key::Esc)]);
        }
        match mode {
            Mode::Function => {
                const LABELS: [&str; 12] = [
                    "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
                ];
                let keys = [
                    Key::F1,
                    Key::F2,
                    Key::F3,
                    Key::F4,
                    Key::F5,
                    Key::F6,
                    Key::F7,
                    Key::F8,
                    Key::F9,
                    Key::F10,
                    Key::F11,
                    Key::F12,
                ];
                groups.push(
                    LABELS
                        .into_iter()
                        .zip(keys)
                        .map(|(label, key)| (Glyph::Text(label), key))
                        .collect(),
                );
            }
            Mode::Media => groups.extend([
                vec![
                    (Glyph::Icon(Icon::Brightness(false)), Key::BrightnessDown),
                    (Glyph::Icon(Icon::Brightness(true)), Key::BrightnessUp),
                ],
                vec![
                    (Glyph::Icon(Icon::Keyboard(false)), Key::IllumDown),
                    (Glyph::Icon(Icon::Keyboard(true)), Key::IllumUp),
                ],
                vec![
                    (Glyph::Icon(Icon::Previous), Key::PreviousSong),
                    (Glyph::Icon(Icon::PlayPause), Key::PlayPause),
                    (Glyph::Icon(Icon::Next), Key::NextSong),
                ],
                vec![
                    (Glyph::Icon(Icon::Microphone), Key::MicMute),
                    (Glyph::Icon(Icon::Mute), Key::Mute),
                    (Glyph::Icon(Icon::Volume(false)), Key::VolumeDown),
                    (Glyph::Icon(Icon::Volume(true)), Key::VolumeUp),
                ],
            ]),
        }

        let count = groups.iter().map(Vec::len).sum::<usize>().max(1) as u16;
        let group_count = groups.len().max(1) as u16;
        let spacing = KEY_GAP * (count - group_count) + GROUP_GAP * (group_count - 1);
        let usable = width.saturating_sub(2 * KEY_GAP + spacing);
        let cell = usable / count;
        // Center the row so rounding leftovers end up evenly on both edges.
        let mut left = KEY_GAP + (usable - cell * count) / 2;
        let mut buttons = Vec::new();
        for group in groups {
            for (glyph, key) in group {
                buttons.push(Button {
                    left,
                    right: left + cell,
                    glyph,
                    action: Action::Key(key),
                });
                left += cell + KEY_GAP;
            }
            left += GROUP_GAP - KEY_GAP;
        }
        Self { buttons }
    }

    pub fn hit(&self, x: f64, y: f64, height: u16) -> Option<usize> {
        if y < 5.0 || y > f64::from(height.saturating_sub(5)) {
            return None;
        }
        self.buttons
            .iter()
            .position(|button| x >= f64::from(button.left) && x < f64::from(button.right))
    }
}

/// Variable sans fonts that match the San Francisco legends of the keyboard
/// closely enough. Adwaita Sans ships with GNOME on Fedora.
const FONT_PATHS: [&str; 3] = [
    "/usr/share/fonts/adwaita-sans-fonts/AdwaitaSans-Regular.ttf",
    "/usr/share/fonts/rsms-inter-vf-fonts/InterVariable.ttf",
    "/usr/share/fonts/abattis-cantarell-vf-fonts/Cantarell-VF.otf",
];
const FONT_WEIGHT: f32 = 500.0;
const KEY_FONT_SIZE: f32 = 23.0;
const LABEL_FONT_SIZE: f32 = 19.0;

/// Icons are drawn on a 24 px design grid and centered by their bounds so
/// every key gets the same optical margin.
const ICON_SCALE: f32 = 1.3;
const ICON_STROKE: f32 = 2.3;
const KEY_INSET: f32 = 6.5;
const KEY_RADIUS: f32 = 8.0;
const KEY_OUTLINE: f32 = 1.5;

pub struct Canvas {
    pub width: u16,
    pub height: u16,
    pub pixels: Vec<u32>,
    pixmap: Pixmap,
    font: FontVec,
    color: Rgb,
}

#[derive(Clone, Copy)]
struct Rgb(u8, u8, u8);

impl Rgb {
    fn from_u32(color: u32) -> Self {
        Self((color >> 16) as u8, (color >> 8) as u8, color as u8)
    }

    fn scale(self, percent: u32) -> Self {
        let channel = |value: u8| (u32::from(value) * percent / 100).min(255) as u8;
        Self(channel(self.0), channel(self.1), channel(self.2))
    }

    fn paint(self) -> Paint<'static> {
        let mut paint = Paint::default();
        paint.set_color_rgba8(self.0, self.1, self.2, 255);
        paint.anti_alias = true;
        paint
    }
}

const BLACK: Rgb = Rgb(0, 0, 0);

impl Canvas {
    pub fn new(width: u16, height: u16, key_color: u32) -> Result<Self> {
        let pixmap = Pixmap::new(u32::from(width), u32::from(height))
            .ok_or_else(|| anyhow!("invalid canvas size {width}x{height}"))?;
        let mut canvas = Self {
            width,
            height,
            pixels: vec![0; width as usize * height as usize],
            pixmap,
            font: load_font()?,
            color: Rgb::from_u32(key_color),
        };
        canvas.clear();
        Ok(canvas)
    }

    pub fn clear(&mut self) {
        self.pixmap.fill(Color::BLACK);
        self.pixels.fill(0);
    }

    pub fn keys(&mut self, layout: &Layout, active: Option<usize>) {
        self.pixmap.fill(Color::BLACK);
        let top = KEY_INSET;
        let bottom = f32::from(self.height) - KEY_INSET;
        let cy = f32::from(self.height) / 2.0;
        for (index, button) in layout.buttons.iter().enumerate() {
            let (left, right) = (f32::from(button.left), f32::from(button.right));
            let background = if active == Some(index) {
                let fill = self.color.scale(20);
                self.fill(&rounded_rect(left, top, right, bottom, KEY_RADIUS), fill);
                fill
            } else {
                BLACK
            };
            let half = KEY_OUTLINE / 2.0;
            self.stroke(
                &rounded_rect(
                    left + half,
                    top + half,
                    right - half,
                    bottom - half,
                    KEY_RADIUS,
                ),
                self.color
                    .scale(if active == Some(index) { 34 } else { 22 }),
                KEY_OUTLINE,
            );
            let cx = (left + right) / 2.0;
            match button.glyph {
                Glyph::Text(text) => self.center_text(cx, cy, text, KEY_FONT_SIZE, self.color),
                Glyph::Icon(icon) => self.icon(cx, cy, icon, background),
            }
        }
        self.flush();
    }

    pub fn touch_id(&mut self, state: &str, phase: u8) {
        self.pixmap.fill(Color::BLACK);
        let (color, label) = match state {
            "matched" => (0x0034c759, "Unlocked"),
            "retry" => (0x00ff9f0a, "Try Again"),
            "failed" => (0x00ff453a, "Use Password"),
            "scanning" => (0x00ff5f7a, "Touch ID"),
            _ => (0x00ff375f, "Touch ID"),
        };
        let pulsing = matches!(state, "waiting" | "scanning");
        let color = Rgb::from_u32(color);
        let color = if pulsing {
            color.scale(45 + phase as u32 * 7)
        } else {
            color
        };
        let height = f32::from(self.height);
        let width = f32::from(self.width);
        let cap = height * 0.82;
        let right = width - 10.0;
        let panel = rounded_rect(
            right - cap,
            (height - cap) / 2.0,
            right,
            (height + cap) / 2.0,
            12.0,
        );
        self.fill(&panel, Rgb(0x1c, 0x1c, 0x1e));
        let cx = right - cap / 2.0;
        self.fingerprint(cx, height / 2.0, color);
        let label_width = self.text_width(label, LABEL_FONT_SIZE);
        self.center_text(
            cx - cap / 2.0 - 18.0 - label_width / 2.0,
            height / 2.0,
            label,
            LABEL_FONT_SIZE,
            self.color,
        );
        self.flush();
    }

    /// Converts the premultiplied, always opaque RGBA pixmap into XRGB8888.
    fn flush(&mut self) {
        for (target, pixel) in self.pixels.iter_mut().zip(self.pixmap.pixels()) {
            *target = (u32::from(pixel.red()) << 16)
                | (u32::from(pixel.green()) << 8)
                | u32::from(pixel.blue());
        }
    }

    fn fill(&mut self, path: &Path, color: Rgb) {
        self.pixmap.fill_path(
            path,
            &color.paint(),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    fn stroke(&mut self, path: &Path, color: Rgb, width: f32) {
        let stroke = Stroke {
            width,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Stroke::default()
        };
        self.pixmap
            .stroke_path(path, &color.paint(), &stroke, Transform::identity(), None);
    }

    fn icon(&mut self, cx: f32, cy: f32, icon: Icon, background: Rgb) {
        let shapes = icon_shapes(icon);
        let mut bounds: Option<Rect> = None;
        for shape in &shapes {
            let pad = shape.pad();
            let Some(path) = shape
                .path()
                .clone()
                .transform(Transform::from_scale(ICON_SCALE, ICON_SCALE))
            else {
                continue;
            };
            let path_bounds = path.bounds();
            let rect = Rect::from_ltrb(
                path_bounds.left() - pad,
                path_bounds.top() - pad,
                path_bounds.right() + pad,
                path_bounds.bottom() + pad,
            );
            bounds = match (bounds, rect) {
                (Some(old), Some(new)) => Rect::from_ltrb(
                    old.left().min(new.left()),
                    old.top().min(new.top()),
                    old.right().max(new.right()),
                    old.bottom().max(new.bottom()),
                ),
                (old, new) => old.or(new),
            };
        }
        let Some(bounds) = bounds else { return };
        let dx = (cx - (bounds.left() + bounds.right()) / 2.0).round();
        let dy = (cy - (bounds.top() + bounds.bottom()) / 2.0).round();
        let transform = Transform::from_row(ICON_SCALE, 0.0, 0.0, ICON_SCALE, dx, dy);
        for shape in shapes {
            match shape {
                Shape::Stroke(path) => {
                    if let Some(path) = path.transform(transform) {
                        self.stroke(&path, self.color, ICON_STROKE);
                    }
                }
                Shape::Solid(path) => {
                    if let Some(path) = path.transform(transform) {
                        // A thin round-joined stroke softens the corners the
                        // same way the keycap glyphs are rounded.
                        self.fill(&path, self.color);
                        self.stroke(&path, self.color, 1.4);
                    }
                }
                Shape::Slash(path) => {
                    if let Some(path) = path.transform(transform) {
                        self.stroke(&path, background, ICON_STROKE * 2.6);
                        self.stroke(&path, self.color, ICON_STROKE);
                    }
                }
            }
        }
    }

    fn fingerprint(&mut self, cx: f32, cy: f32, color: Rgb) {
        for (index, radius) in [4.5f32, 9.0, 13.5, 18.0].into_iter().enumerate() {
            let gap = 40.0 + index as f32 * 8.0;
            let mut path = PathBuilder::new();
            arc(&mut path, cx, cy, radius, 90.0 + gap, 360.0 + 90.0 - gap);
            if let Some(path) = path.finish() {
                self.stroke(&path, color, 2.2);
            }
        }
    }

    fn text_width(&self, text: &str, size: f32) -> f32 {
        let scale = size / self.font.units_per_em().unwrap_or(1000.0);
        let mut width = 0.0;
        let mut previous = None;
        for character in text.chars() {
            let id = self.font.glyph_id(character);
            if let Some(previous) = previous {
                width += self.font.kern_unscaled(previous, id) * scale;
            }
            width += self.font.h_advance_unscaled(id) * scale;
            previous = Some(id);
        }
        width
    }

    fn center_text(&mut self, cx: f32, cy: f32, text: &str, size: f32, color: Rgb) {
        let scale = size / self.font.units_per_em().unwrap_or(1000.0);
        let cap_height = self
            .font
            .outline(self.font.glyph_id('H'))
            .map_or(size * 0.7, |outline| outline.bounds.height().abs() * scale);
        let mut x = (cx - self.text_width(text, size) / 2.0).round();
        let baseline = (cy + cap_height / 2.0).round();
        let mut path = PathBuilder::new();
        let mut previous = None;
        for character in text.chars() {
            let id = self.font.glyph_id(character);
            if let Some(previous) = previous {
                x += self.font.kern_unscaled(previous, id) * scale;
            }
            if let Some(outline) = self.font.outline(id) {
                let point = |p: ab_glyph::Point| (x + p.x * scale, baseline - p.y * scale);
                let mut last = None;
                for curve in outline.curves {
                    let start = match curve {
                        OutlineCurve::Line(a, _)
                        | OutlineCurve::Quad(a, _, _)
                        | OutlineCurve::Cubic(a, _, _, _) => a,
                    };
                    if last != Some(start) {
                        let (sx, sy) = point(start);
                        path.move_to(sx, sy);
                    }
                    last = Some(match curve {
                        OutlineCurve::Line(_, b) => {
                            let (bx, by) = point(b);
                            path.line_to(bx, by);
                            b
                        }
                        OutlineCurve::Quad(_, c, b) => {
                            let ((cx, cy), (bx, by)) = (point(c), point(b));
                            path.quad_to(cx, cy, bx, by);
                            b
                        }
                        OutlineCurve::Cubic(_, c1, c2, b) => {
                            let ((ax, ay), (cx, cy), (bx, by)) = (point(c1), point(c2), point(b));
                            path.cubic_to(ax, ay, cx, cy, bx, by);
                            b
                        }
                    });
                }
            }
            x += self.font.h_advance_unscaled(id) * scale;
            previous = Some(id);
        }
        if let Some(path) = path.finish() {
            self.fill(&path, color);
        }
    }
}

fn load_font() -> Result<FontVec> {
    for path in FONT_PATHS {
        let Ok(data) = fs::read(path) else { continue };
        let mut font = FontVec::try_from_vec(data).with_context(|| format!("parse font {path}"))?;
        font.set_variation(b"wght", FONT_WEIGHT);
        return Ok(font);
    }
    Err(anyhow!(
        "no Touch Bar font found; install adwaita-sans-fonts ({})",
        FONT_PATHS.join(", ")
    ))
}

enum Shape {
    Stroke(Path),
    Solid(Path),
    /// A crossing stroke that cuts a gap into the strokes below it.
    Slash(Path),
}

impl Shape {
    fn path(&self) -> &Path {
        match self {
            Self::Stroke(path) | Self::Solid(path) | Self::Slash(path) => path,
        }
    }

    fn pad(&self) -> f32 {
        match self {
            Self::Stroke(_) | Self::Slash(_) => ICON_STROKE / 2.0,
            Self::Solid(_) => 0.7,
        }
    }
}

fn icon_shapes(icon: Icon) -> Vec<Shape> {
    let mut shapes = Vec::new();
    let build = |build: &dyn Fn(&mut PathBuilder)| {
        let mut path = PathBuilder::new();
        build(&mut path);
        path.finish()
    };
    match icon {
        Icon::Brightness(up) => {
            let (core, inner, outer) = if up {
                (4.6, 8.2, 11.0)
            } else {
                (3.6, 7.0, 8.0)
            };
            if let Some(path) = build(&|path| {
                path.push_circle(0.0, 0.0, core);
                rays(path, 0.0, 0.0, inner, outer, 0.0, 360.0, 8);
            }) {
                shapes.push(Shape::Stroke(path));
            }
        }
        Icon::Keyboard(up) => {
            let (inner, outer) = if up { (7.5, 10.5) } else { (7.0, 8.0) };
            if let Some(path) = build(&|path| {
                arc(path, 0.0, 3.0, 4.2, 180.0, 360.0);
                path.close();
                rays(path, 0.0, 3.0, inner, outer, 180.0, 360.0, 5);
                path.move_to(-6.0, 8.0);
                path.line_to(6.0, 8.0);
            }) {
                shapes.push(Shape::Stroke(path));
            }
        }
        Icon::Microphone => {
            if let Some(path) = build(&|path| {
                path.push_path(&rounded_rect(-3.6, -11.0, 3.6, 2.5, 3.6));
                arc(path, 0.0, -2.0, 7.4, 0.0, 180.0);
                path.move_to(0.0, 5.4);
                path.line_to(0.0, 9.5);
            }) {
                shapes.push(Shape::Stroke(path));
            }
            if let Some(path) = build(&|path| {
                path.move_to(-8.5, -10.0);
                path.line_to(8.5, 9.0);
            }) {
                shapes.push(Shape::Slash(path));
            }
        }
        Icon::Previous | Icon::Next => {
            let side = if matches!(icon, Icon::Next) {
                1.0
            } else {
                -1.0
            };
            if let Some(path) = build(&|path| {
                for offset in [-9.0f32, 0.5] {
                    path.move_to(side * offset, -6.8);
                    path.line_to(side * (offset + 9.5), 0.0);
                    path.line_to(side * offset, 6.8);
                    path.close();
                }
            }) {
                shapes.push(Shape::Solid(path));
            }
        }
        Icon::PlayPause => {
            if let Some(path) = build(&|path| {
                path.move_to(-11.0, -6.8);
                path.line_to(-1.5, 0.0);
                path.line_to(-11.0, 6.8);
                path.close();
                path.push_path(&rounded_rect(2.5, -6.8, 5.5, 6.8, 0.6));
                path.push_path(&rounded_rect(8.0, -6.8, 11.0, 6.8, 0.6));
            }) {
                shapes.push(Shape::Solid(path));
            }
        }
        Icon::Mute | Icon::Volume(_) => {
            if let Some(path) = build(&|path| {
                path.move_to(-10.0, -3.6);
                path.line_to(-6.0, -3.6);
                path.line_to(-0.8, -8.4);
                path.line_to(-0.8, 8.4);
                path.line_to(-6.0, 3.6);
                path.line_to(-10.0, 3.6);
                path.close();
            }) {
                shapes.push(Shape::Solid(path));
            }
            let waves: &[f32] = match icon {
                Icon::Volume(true) => &[4.6, 8.4, 12.2],
                Icon::Volume(false) => &[4.6],
                _ => &[],
            };
            if let Some(path) = build(&|path| {
                for radius in waves {
                    let sweep = 46.0 - radius * 1.2;
                    arc(path, 0.8, 0.0, *radius, -sweep, sweep);
                }
            }) {
                shapes.push(Shape::Stroke(path));
            }
            if matches!(icon, Icon::Mute)
                && let Some(path) = build(&|path| {
                    path.move_to(-11.0, -9.5);
                    path.line_to(6.0, 9.5);
                })
            {
                shapes.push(Shape::Slash(path));
            }
        }
    }
    shapes
}

/// Appends an open arc; angles are in degrees, clockwise from +x.
fn arc(path: &mut PathBuilder, cx: f32, cy: f32, radius: f32, from: f32, to: f32) {
    let steps = ((to - from).abs() / 6.0).ceil().max(2.0) as usize;
    for step in 0..=steps {
        let angle = (from + (to - from) * step as f32 / steps as f32).to_radians();
        let (x, y) = (cx + radius * angle.cos(), cy + radius * angle.sin());
        if step == 0 {
            path.move_to(x, y);
        } else {
            path.line_to(x, y);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn rays(
    path: &mut PathBuilder,
    cx: f32,
    cy: f32,
    inner: f32,
    outer: f32,
    from: f32,
    to: f32,
    count: usize,
) {
    let full = (to - from - 360.0).abs() < f32::EPSILON;
    let divisions = if full { count } else { count - 1 };
    for index in 0..count {
        let angle = (from + (to - from) * index as f32 / divisions as f32).to_radians();
        let (sin, cos) = angle.sin_cos();
        path.move_to(cx + inner * cos, cy + inner * sin);
        path.line_to(cx + outer * cos, cy + outer * sin);
    }
}

fn rounded_rect(left: f32, top: f32, right: f32, bottom: f32, radius: f32) -> Path {
    let radius = radius.min((right - left) / 2.0).min((bottom - top) / 2.0);
    let mut path = PathBuilder::new();
    // Cubic quarter circles with the usual 0.5523 control distance.
    let k = radius * (1.0 - 0.552_284_8);
    path.move_to(left + radius, top);
    path.line_to(right - radius, top);
    path.cubic_to(right - k, top, right, top + k, right, top + radius);
    path.line_to(right, bottom - radius);
    path.cubic_to(right, bottom - k, right - k, bottom, right - radius, bottom);
    path.line_to(left + radius, bottom);
    path.cubic_to(left + k, bottom, left, bottom - k, left, bottom - radius);
    path.line_to(left, top + radius);
    path.cubic_to(left, top + k, left + k, top, left + radius, top);
    path.close();
    path.finish()
        .expect("rounded rectangle has a non-empty outline")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_have_no_overlapping_buttons() {
        for mode in [Mode::Media, Mode::Function] {
            let layout = Layout::new(mode, 2170, false);
            for pair in layout.buttons.windows(2) {
                assert!(pair[0].right < pair[1].left);
            }
        }
    }

    #[test]
    fn touch_id_draws_only_near_the_sensor_and_label() {
        let mut canvas = Canvas::new(2170, 60, 0x00dce6ff).unwrap();
        canvas.touch_id("waiting", 4);
        assert!(canvas.pixels.iter().any(|pixel| *pixel != 0));
        assert_eq!(canvas.pixels[0], 0);
    }

    /// Renders both layers and the Touch ID prompt for design review:
    /// `KAIT2EN_TOUCHBAR_PREVIEW=/tmp/tb cargo test preview -- --ignored`
    #[test]
    #[ignore]
    fn preview() {
        let dir = std::env::var("KAIT2EN_TOUCHBAR_PREVIEW").unwrap_or_else(|_| ".".into());
        let mut canvas = Canvas::new(2170, 60, 0x00dce6ff).unwrap();
        let save = |name: &str, canvas: &Canvas| {
            let mut ppm = format!("P6 {} {} 255\n", canvas.width, canvas.height).into_bytes();
            for pixel in &canvas.pixels {
                ppm.extend([(pixel >> 16) as u8, (pixel >> 8) as u8, *pixel as u8]);
            }
            fs::write(format!("{dir}/{name}.ppm"), ppm).unwrap();
        };
        canvas.keys(&Layout::new(Mode::Media, 2170, false), Some(8));
        save("media", &canvas);
        canvas.keys(&Layout::new(Mode::Function, 2170, false), None);
        save("function", &canvas);
        canvas.touch_id("waiting", 8);
        save("touchid", &canvas);
    }
}
