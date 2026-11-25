//! `jai --os-look <dir>`: everything J.A.I.OS shows before or outside his own windows, written
//! from his theme and painters, so the boot, the boot menu and KDE's own programs wear what he
//! wears. The `jaios-settings` package runs it when it is built (`os/pkg/jaios-settings`).
//!
//! - `plymouth/jaios/`: the boot animation for Plymouth's two-step plugin. While the PC starts,
//!   a light runs round his reference ring (`throbber-*`); when it has started, his core draws
//!   itself ring by ring from the outside in and settles facing Sir (`animation-*`). The frames
//!   are the real `holo_core` painter's, drawn offscreen.
//! - `limine/`: the boot menu's wallpaper (his drafting table) and its colour lines.
//! - `kde/`: the colour scheme `JAI.colors` and the system's `kdeglobals`.
//! - `konsole/`: the terminal's colours and profile.
//! - `iso/`: the BIOS boot menu of the install stick (syslinux): its picture and its colours.
//! - `icons/<size>/jaios.png`: the OS's logo, his icon.

use crate::holo::{self, a, Face};
use crate::holo_core::{self, Core, CoreLook, CoreStyle, Pose};
use crate::holo_theme::{self, mix, Theme};
use crate::ui_snapshot::Snapshot;
use eframe::egui::{self, pos2, vec2, Align2, Rect, Vec2};
use std::f32::consts::TAU;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Plymouth plays both sets at this pace.
const FPS: f32 = 30.0;
/// The frames his core takes to draw itself, then to settle flat.
pub const REVEAL: usize = 36;
pub const SETTLE: usize = 24;
/// One turn of the light round his reference ring.
pub const THROBBER: usize = 45;
/// A frame's side and the core's radius in it (its glow reaches 1.34 radii).
const SIDE: usize = 560;
const RADIUS: f32 = 190.0;
/// How many steps of 0.07 rad the running light's tail is long.
const TAIL: usize = 26;
/// The sizes the logo is written in (hicolor's).
pub const ICONS: [u32; 6] = [16, 32, 48, 64, 128, 256];

fn hex(c: [u8; 3]) -> String {
    format!("{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

fn rgb(c: [u8; 3]) -> String {
    format!("{},{},{}", c[0], c[1], c[2])
}

/// One frame of `ui` drawn offscreen on the theme's black, as RGB bytes.
fn picture(snap: &mut Snapshot, time: f32, mut ui: impl FnMut(&egui::Context)) -> Vec<u8> {
    let screen = snap.screen();
    let input = egui::RawInput { screen_rect: Some(screen), time: Some(f64::from(time)), predicted_dt: 1.0 / FPS, ..Default::default() };
    let output = snap.frame(input, |ctx| {
        ground(ctx).rect_filled(screen, 0.0, holo::theme().void());
        ui(ctx);
    });
    let canvas = snap.rasterise(output.shapes, output.pixels_per_point);
    (0..canvas.width * canvas.height).flat_map(|i| canvas.pixel(i % canvas.width, i / canvas.width)).collect()
}

fn ground(ctx: &egui::Context) -> egui::Painter {
    ctx.layer_painter(egui::LayerId::background())
}

/// A small picture of its own (the password field's pieces).
fn piece(size: Vec2, ui: impl FnMut(&egui::Context)) -> (u32, u32, Vec<u8>) {
    (size.x as u32, size.y as u32, picture(&mut Snapshot::new(size), 0.0, ui))
}

/// Keeps `shown(radius in core radii, turn clockwise from north 0-1)` of a core frame's light;
/// the rest falls back to the theme's black.
fn masked(frame: &[u8], void: [u8; 3], shown: impl Fn(f32, f32) -> f32) -> Vec<u8> {
    let mid = SIDE as f32 / 2.0;
    let mut out = Vec::with_capacity(frame.len());
    for (i, px) in frame.chunks_exact(3).enumerate() {
        let (x, y) = ((i % SIDE) as f32 + 0.5 - mid, (i / SIDE) as f32 + 0.5 - mid);
        let turn = (x.atan2(-y) / TAU).rem_euclid(1.0);
        let keep = shown(x.hypot(y) / RADIUS, turn).clamp(0.0, 1.0);
        out.extend((0..3).map(|c| (f32::from(void[c]) + (f32::from(px[c]) - f32::from(void[c])) * keep).round() as u8));
    }
    out
}

/// The reference ring with its bearings: the part of him that never moves.
fn on_reference_ring(radius: f32) -> f32 {
    ((radius - 0.945) / 0.02).clamp(0.0, 1.0)
}

/// The boot animation: the throbber's frames, then the end animation's.
fn core_frames(theme: &Theme) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let mut snap = Snapshot::new(Vec2::splat(SIDE as f32));
    let mut core = Core::new(Pose::Gyro);
    core.style = CoreStyle::Simple;
    let centre = snap.screen().center();
    let mut shot = |core: &Core, k: usize| {
        let look = CoreLook { word: "STARTING", ..core.look() };
        picture(&mut snap, k as f32 / FPS, |ctx| holo_core::paint(&ground(ctx), egui::Id::new("os-look-core"), core.style, centre, RADIUS, &look))
    };
    // While the PC starts: his reference ring as it always stands, and a light running round it
    // (the sweep that runs his inner ring while he thinks), ending at twelve o'clock.
    let ring = masked(&shot(&core, 0), theme.void, |radius, _| on_reference_ring(radius));
    let mut sweeps = Snapshot::new(Vec2::splat(SIDE as f32));
    let hot = theme.hot();
    let throbber = (0..THROBBER)
        .map(|k| {
            let head = holo::NORTH + TAU * (k + 1) as f32 / THROBBER as f32;
            let sweep = picture(&mut sweeps, k as f32 / FPS, |ctx| {
                let p = ground(ctx);
                let mut light = holo_core::glow(&p, egui::Id::new("os-look-sweep"), centre, RADIUS * 1.34, false);
                for step in 0..TAIL {
                    let from = head - step as f32 * 0.07;
                    light.arc(centre, RADIUS, from - 0.07, from, 2.2, a(hot, (1.0 - step as f32 / TAIL as f32).powi(2)));
                }
                light.spark(holo::polar(centre, RADIUS, head), 5.0, hot);
                light.paint(&p);
            });
            // (Light adds up.)
            ring.iter().zip(&sweep).enumerate().map(|(i, (still, lit))| still.saturating_add(lit.saturating_sub(theme.void[i % 3]))).collect()
        })
        .collect();
    // Things are drawn, not shown: each ring from twelve o'clock clockwise, the outer ones first.
    let reach = SIDE as f32 / 2.0 / RADIUS;
    let edge = 0.12;
    let mut animation = Vec::with_capacity(REVEAL + SETTLE);
    for k in 0..REVEAL + SETTLE {
        if k == REVEAL {
            core.set_pose(Pose::Front, false);
        }
        core.tick(1.0 / FPS, 0.0, false);
        let frame = shot(&core, k + 1);
        animation.push(if k < REVEAL {
            let done = crate::holo_motion::ease_in_out((k + 1) as f32 / REVEAL as f32);
            masked(&frame, theme.void, |radius, turn| {
                let drawn = (done * (reach + 1.0 + edge) - (reach - radius.min(reach)) - turn) / edge;
                drawn.max(on_reference_ring(radius))
            })
        } else {
            frame
        });
    }
    (throbber, animation)
}

/// His drafting table with the OS's name on it: the boot menus' ground.
fn backdrop(size: Vec2, name_at: Vec2, name_size: f32) -> Vec<u8> {
    picture(&mut Snapshot::new(size), 0.0, |ctx| {
        crate::gui::paint_canvas_in(ctx, &[]);
        holo::text(&ground(ctx), pos2(size.x * name_at.x, size.y * name_at.y), Align2::CENTER_CENTER, Face::Label, "J.A.I.OS", name_size, a(holo::theme().ink(), 0.8));
    })
}

/// The sixteen terminal colours in his light: black is the canvas, red the alert, yellow's
/// place is the accent, white the ink. Green, blue, magenta and cyan keep their hues (programs
/// mean something by them), as soft as the ink.
pub fn ansi(t: &Theme) -> [[u8; 3]; 16] {
    let white = [255; 3];
    let hue = |h: f32, bright: bool| holo_theme::from_hsv(h, if bright { 0.36 } else { 0.48 }, if bright { 1.0 } else { 0.86 });
    [
        t.void,
        t.alert,
        hue(140.0, false),
        t.accent,
        hue(215.0, false),
        hue(300.0, false),
        hue(185.0, false),
        mix(t.void, t.ink, 0.78),
        mix(t.void, t.ink, 0.34),
        mix(t.alert, white, 0.3),
        hue(140.0, true),
        mix(t.accent, white, 0.62),
        hue(215.0, true),
        hue(300.0, true),
        hue(185.0, true),
        t.ink,
    ]
}

/// Where the installer puts the wallpaper on the boot partition (`limine/theme.conf` names it).
pub const LIMINE_WALLPAPER: &str = "boot():/limine/jaios.png";

/// Limine's global lines: his look, three seconds, the entry booted last.
pub fn limine_conf(t: &Theme) -> String {
    let colours = ansi(t);
    let row = |from: usize| colours[from..from + 8].iter().map(|c| hex(*c)).collect::<Vec<_>>().join(";");
    format!(
        "# J.A.I.OS: Limine in J.A.I.'s look (written by `jai --os-look`).\ntimeout: 3\nremember_last_entry: yes\ninterface_branding: J.A.I.OS\ninterface_branding_colour: {}\ninterface_help_colour: {}\nwallpaper: {LIMINE_WALLPAPER}\nwallpaper_style: stretched\nbackdrop: {void}\nterm_margin: 64\nterm_palette: {}\nterm_palette_bright: {}\nterm_background: 40{void}\nterm_foreground: {ink}\nterm_background_bright: {void}\nterm_foreground_bright: {ink}\n",
        hex(t.accent),
        hex(mix(t.void, t.ink, 0.58)),
        row(0),
        row(8),
        void = hex(t.void),
        ink = hex(t.ink),
    )
}

/// The head of the install stick's BIOS menu (syslinux): the chosen entry is one solid block of
/// the accent with black lettering, the rest ink on the canvas.
pub fn syslinux_head(t: &Theme) -> String {
    let (ink, void, accent) = (hex(t.ink).to_lowercase(), hex(t.void).to_lowercase(), hex(t.accent).to_lowercase());
    format!(
        "SERIAL 0 115200\nUI vesamenu.c32\nMENU TITLE J.A.I.OS\nMENU BACKGROUND splash.png\n\nMENU WIDTH 78\nMENU MARGIN 4\nMENU ROWS 7\nMENU VSHIFT 10\nMENU TABMSGROW 14\nMENU CMDLINEROW 14\nMENU HELPMSGROW 16\nMENU HELPMSGENDROW 29\n\nMENU COLOR border       30;44   #40{ink} #a0{void} std\nMENU COLOR title        1;36;44 #ff{ink} #a0{void} std\nMENU COLOR sel          7;37;40 #ff{void} #ff{accent} all\nMENU COLOR unsel        37;44   #c0{ink} #a0{void} std\nMENU COLOR help         37;40   #c0{ink} #a0{void} std\nMENU COLOR timeout_msg  37;40   #80{ink} #00{void} std\nMENU COLOR timeout      1;37;40 #ff{accent} #00{void} std\nMENU COLOR msg07        37;40   #90{ink} #a0{void} std\nMENU COLOR tabmsg       31;40   #60{ink} #00{void} std\n\nMENU CLEAR\nMENU IMMEDIATE\n"
    )
}

/// Plymouth's theme file: the two-step plugin, his black, no script.
pub fn plymouth_conf(t: &Theme) -> String {
    let (void, accent, rest) = (hex(t.void), hex(t.accent), hex(mix(t.void, t.ink, 0.34)));
    let quiet = "UseEndAnimation=false\nUseFirmwareBackground=false\n";
    let update = |title: &str| format!("SuppressMessages=true\nProgressBarShowPercentComplete=true\nUseProgressBar=true\nTitle={title}\nSubTitle=Do not turn the PC off\n");
    format!(
        "[Plymouth Theme]\nName=J.A.I.OS\nDescription=J.A.I.'s core draws itself on black\nModuleName=two-step\n\n[two-step]\nImageDir=/usr/share/plymouth/themes/jaios\nHorizontalAlignment=.5\nVerticalAlignment=.5\nDialogHorizontalAlignment=.5\nDialogVerticalAlignment=.86\nTitleHorizontalAlignment=.5\nTitleVerticalAlignment=.86\nWatermarkHorizontalAlignment=.5\nWatermarkVerticalAlignment=.95\nTransition=none\nTransitionDuration=0.0\nBackgroundStartColor=0x{void}\nBackgroundEndColor=0x{void}\nProgressBarBackgroundColor=0x{rest}\nProgressBarForegroundColor=0x{accent}\nDialogClearsFirmwareBackground=true\nMessageBelowAnimation=true\n\n[boot-up]\nUseEndAnimation=true\nUseFirmwareBackground=false\n\n[shutdown]\n{quiet}\n[reboot]\n{quiet}\n[updates]\n{}\n[system-upgrade]\n{}\n[firmware-upgrade]\n{}",
        update("Installing updates"),
        update("Upgrading J.A.I.OS"),
        update("Upgrading the firmware"),
    )
}

/// One of KDE's colour sets: its ground, the alternate ground, its text and the text that
/// steps back.
pub struct KdeSet {
    pub name: &'static str,
    pub ground: [u8; 3],
    pub alternate: [u8; 3],
    pub text: [u8; 3],
    pub quiet: [u8; 3],
}

/// KDE's colour roles from the five colours: views are the canvas, windows and buttons glass a
/// breath lighter, the selection one solid block of the accent with black lettering.
pub fn kde_sets(t: &Theme) -> Vec<KdeSet> {
    let glass = |share: f32| mix(t.void, t.ink, share);
    let muted = glass(0.58);
    let plain = |name, ground, alternate| KdeSet { name, ground, alternate, text: t.ink, quiet: muted };
    vec![
        plain("View", t.void, glass(0.03)),
        plain("Window", glass(0.035), glass(0.06)),
        plain("Button", glass(0.07), glass(0.09)),
        plain("Header", glass(0.035), glass(0.06)),
        plain("Tooltip", glass(0.06), glass(0.035)),
        plain("Complementary", t.void, glass(0.035)),
        // (The alternate is lighter, never darker: black lettering reads on a red accent too.)
        KdeSet { name: "Selection", ground: t.accent, alternate: mix(t.accent, [255; 3], 0.25), text: t.void, quiet: mix(t.accent, t.void, 0.9) },
    ]
}

/// The `[Colors:…]`, effect and window-manager groups, as `JAI.colors` and `kdeglobals` both
/// carry them (Plasma reads the colours from kdeglobals itself, not from the scheme's name).
fn kde_colour_groups(t: &Theme) -> String {
    let hot = mix(t.accent, [255; 3], 0.62);
    let warn = mix(t.deep, t.alert, 0.35);
    let (muted, faint) = (mix(t.void, t.ink, 0.58), mix(t.void, t.ink, 0.34));
    let mut out = format!(
        "[ColorEffects:Disabled]\nColor={}\nColorAmount=0\nColorEffect=0\nContrastAmount=0.65\nContrastEffect=1\nIntensityAmount=0.1\nIntensityEffect=2\n\n[ColorEffects:Inactive]\nChangeSelectionColor=false\nColor={}\nColorAmount=0.025\nColorEffect=2\nContrastAmount=0.1\nContrastEffect=2\nEnable=false\nIntensityAmount=0\nIntensityEffect=0\n\n",
        rgb(faint),
        rgb(muted)
    );
    for set in kde_sets(t) {
        let chosen = set.name == "Selection";
        // On the accent everything is lettering in black; elsewhere colour is information.
        let on = |colour: [u8; 3]| if chosen { set.text } else { colour };
        // (Nominal is plain ink, never green.)
        out += &format!(
            "[Colors:{}]\nBackgroundAlternate={}\nBackgroundNormal={}\nDecorationFocus={}\nDecorationHover={}\nForegroundActive={}\nForegroundInactive={}\nForegroundLink={}\nForegroundNegative={}\nForegroundNeutral={}\nForegroundNormal={}\nForegroundPositive={}\nForegroundVisited={}\n\n",
            set.name,
            rgb(set.alternate),
            rgb(set.ground),
            rgb(t.accent),
            rgb(hot),
            rgb(on(t.accent)),
            rgb(set.quiet),
            rgb(on(t.accent)),
            rgb(on(t.alert)),
            rgb(on(warn)),
            rgb(set.text),
            rgb(set.text),
            rgb(on(t.deep)),
        );
    }
    let window = mix(t.void, t.ink, 0.035);
    out + &format!(
        "[WM]\nactiveBackground={}\nactiveBlend={}\nactiveForeground={}\ninactiveBackground={}\ninactiveBlend={}\ninactiveForeground={}\n",
        rgb(window),
        rgb(t.ink),
        rgb(t.ink),
        rgb(t.void),
        rgb(muted),
        rgb(muted)
    )
}

/// The colour scheme as System Settings lists it.
pub fn kde_scheme(t: &Theme) -> String {
    format!("{}\n[General]\nColorScheme=JAI\nName=J.A.I.\nshadeSortColumn=true\n\n[KDE]\ncontrast=4\n", kde_colour_groups(t))
}

/// The system's kdeglobals (`/etc/xdg`): his colours, his faces (installed by `jaios-settings`
/// from `assets/fonts`), Breeze's dark icons.
pub fn kdeglobals(t: &Theme) -> String {
    let face = |family: &str, points: u32, weight: u32| format!("{family},{points},-1,5,{weight},0,0,0,0,0,0,0,0,0,0,1");
    let (body, small, mono) = (face("Barlow", 10, 500), face("Barlow", 8, 500), face("Share Tech Mono", 10, 400));
    format!(
        "[General]\nColorScheme=JAI\nfont={body}\nmenuFont={body}\ntoolBarFont={body}\nsmallestReadableFont={small}\nfixed={mono}\n\n[Icons]\nTheme=breeze-dark\n\n[KDE]\nLookAndFeelPackage=org.kde.breezedark.desktop\nwidgetStyle=Breeze\ncontrast=4\n\n{}activeFont={}\n",
        kde_colour_groups(t),
        face("Barlow", 10, 600)
    )
}

/// Konsole's colours.
pub fn konsole_scheme(t: &Theme) -> String {
    let colours = ansi(t);
    let mut out = format!(
        "[General]\nDescription=J.A.I.\nOpacity=1\nWallpaper=\n\n[Background]\nColor={void}\n\n[BackgroundIntense]\nColor={void}\n\n[BackgroundFaint]\nColor={void}\n\n[Foreground]\nColor={ink}\n\n[ForegroundIntense]\nColor={hot}\n\n[ForegroundFaint]\nColor={muted}\n",
        void = rgb(t.void),
        ink = rgb(t.ink),
        hot = rgb(colours[11]),
        muted = rgb(colours[7])
    );
    for i in 0..8 {
        out += &format!("\n[Color{i}]\nColor={}\n\n[Color{i}Intense]\nColor={}\n\n[Color{i}Faint]\nColor={}\n", rgb(colours[i]), rgb(colours[i + 8]), rgb(mix(colours[i], t.void, 0.35)));
    }
    out
}

const KONSOLE_PROFILE: &str = "[General]\nName=J.A.I.\nParent=FALLBACK/\n\n[Appearance]\nColorScheme=JAI\nFont=Share Tech Mono,11,-1,5,400,0,0,0,0,0,0,0,0,0,0,1\n\n[Scrolling]\nHistoryMode=2\n";

/// Writes the whole look into `dir`: the files written, relative to it.
pub fn write(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let theme = holo_theme::preset(holo_theme::DEFAULT_PRESET).unwrap_or(holo_theme::PRESETS[0].2);
    // (His own yellow, whatever theme this thread or a settings file has.)
    let _worn = holo_theme::wear(Some(theme));
    let mut written = Vec::new();
    let mut file = |name: String, bytes: &[u8]| -> io::Result<()> {
        let path = dir.join(&name);
        std::fs::create_dir_all(path.parent().unwrap_or(dir))?;
        std::fs::write(path, bytes)?;
        written.push(PathBuf::from(name));
        Ok(())
    };
    let png = |width: u32, height: u32, pixels: &[u8]| -> io::Result<Vec<u8>> {
        let kind = if pixels.len() == (width * height * 4) as usize { image::ExtendedColorType::Rgba8 } else { image::ExtendedColorType::Rgb8 };
        let mut out = io::Cursor::new(Vec::new());
        image::write_buffer_with_format(&mut out, pixels, width, height, kind, image::ImageFormat::Png).map_err(io::Error::other)?;
        Ok(out.into_inner())
    };
    let side = SIDE as u32;

    let (throbber, animation) = core_frames(&theme);
    for (set, frames) in [("throbber", &throbber), ("animation", &animation)] {
        for (k, frame) in frames.iter().enumerate() {
            file(format!("plymouth/jaios/{set}-{:04}.png", k + 1), &png(side, side, frame)?)?;
        }
    }
    file("plymouth/jaios/jaios.plymouth".into(), plymouth_conf(&theme).as_bytes())?;
    // The password field of an encrypted disk (the plugin does not load without its pieces).
    let ink = theme.ink();
    let pieces = [
        ("entry", piece(vec2(320.0, 36.0), |ctx| holo::outline(&ground(ctx), &holo::plate(Rect::from_min_size(pos2(1.0, 1.0), vec2(318.0, 34.0)), [0.0, 0.0, 8.0, 0.0]), a(ink, 0.7)))),
        (
            "bullet",
            piece(vec2(12.0, 12.0), |ctx| {
                ground(ctx).circle_filled(pos2(6.0, 6.0), 3.5, theme.accent());
            }),
        ),
        (
            "lock",
            piece(vec2(36.0, 44.0), |ctx| {
                let p = ground(ctx);
                holo::path(&p, &holo::arc_points(pos2(18.0, 18.0), 9.0, TAU / 2.0, TAU), ink);
                holo::outline(&p, &holo::plate(Rect::from_min_max(pos2(6.0, 18.0), pos2(30.0, 40.0)), [0.0; 4]), ink);
                p.circle_filled(pos2(18.0, 29.0), 2.5, theme.accent());
            }),
        ),
        (
            "watermark",
            piece(vec2(260.0, 36.0), |ctx| {
                holo::text(&ground(ctx), pos2(130.0, 18.0), Align2::CENTER_CENTER, Face::Label, "J.A.I.OS", 13.0, a(ink, 0.6));
            }),
        ),
    ];
    for (name, (width, height, pixels)) in pieces {
        file(format!("plymouth/jaios/{name}.png"), &png(width, height, &pixels)?)?;
    }

    // (The name stands in the margin Limine keeps free round its menu.)
    file("limine/wallpaper.png".into(), &png(1920, 1080, &backdrop(vec2(1920.0, 1080.0), vec2(0.5, 0.972), 15.0))?)?;
    file("limine/theme.conf".into(), limine_conf(&theme).as_bytes())?;
    file("iso/splash.png".into(), &png(640, 480, &backdrop(vec2(640.0, 480.0), vec2(0.5, 0.1), 20.0))?)?;
    file("iso/archiso_head.cfg".into(), syslinux_head(&theme).as_bytes())?;
    file("kde/JAI.colors".into(), kde_scheme(&theme).as_bytes())?;
    file("kde/kdeglobals".into(), kdeglobals(&theme).as_bytes())?;
    file("konsole/JAI.colorscheme".into(), konsole_scheme(&theme).as_bytes())?;
    file("konsole/JAI.profile".into(), KONSOLE_PROFILE.as_bytes())?;
    for size in ICONS {
        file(format!("icons/{size}/jaios.png"), &png(size, size, &crate::app_icon::render_icon_rgba(size))?)?;
    }
    Ok(written)
}

/// `jai --os-look <dir>`.
pub fn run(dir: &Path) -> ExitCode {
    match write(dir) {
        Ok(files) => {
            println!("J.A.I.OS's look: {} files in {}", files.len(), dir.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("J.A.I.OS's look could not be written into {}: {e}", dir.display());
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Color32;

    fn theme() -> Theme {
        holo_theme::PRESETS[0].2
    }

    fn c32(c: [u8; 3]) -> Color32 {
        Color32::from_rgb(c[0], c[1], c[2])
    }

    /// The value of `key` in `group` of a KDE file.
    fn kde_value(file: &str, group: &str, key: &str) -> String {
        let body = file.split(&format!("[{group}]\n")).nth(1).unwrap_or_else(|| panic!("no group {group}"));
        let body = body.split("\n[").next().unwrap();
        body.lines().find_map(|l| l.strip_prefix(&format!("{key}="))).unwrap_or_else(|| panic!("no {key} in {group}")).to_string()
    }

    /// Everything is written, the frames are counted, and the colours are the theme's.
    #[test]
    fn the_os_look_is_written_in_the_themes_colours() {
        let _blocks = crate::hud_dock::test_lock();
        let t = theme();
        let dir = Path::new("target").join("ui-snapshots").join("os_look");
        let _ = std::fs::remove_dir_all(&dir);
        let files = write(&dir).expect("written");
        for file in &files {
            assert!(std::fs::metadata(dir.join(file)).is_ok_and(|m| m.len() > 0), "{} is missing or empty", file.display());
        }
        let named = |start: &str| files.iter().filter(|f| f.to_string_lossy().replace('\\', "/").starts_with(start)).count();
        assert_eq!(named("plymouth/jaios/animation-"), REVEAL + SETTLE);
        assert_eq!(named("plymouth/jaios/throbber-"), THROBBER);
        assert_eq!(named("icons/"), ICONS.len());
        for needed in ["plymouth/jaios/lock.png", "plymouth/jaios/entry.png", "plymouth/jaios/bullet.png", "limine/wallpaper.png", "iso/splash.png", "kde/kdeglobals", "konsole/JAI.profile"] {
            assert!(dir.join(needed).is_file(), "{needed}");
        }

        let open = |name: &str| image::open(dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}")).to_rgb8();
        let wallpaper = open("limine/wallpaper.png");
        assert_eq!(wallpaper.dimensions(), (1920, 1080));
        // Most of the screen is black: the canvas itself.
        let black = wallpaper.pixels().filter(|p| p.0 == t.void).count();
        assert!(black * 2 > 1920 * 1080, "only {black} pixels of the wallpaper are the canvas");
        assert!(wallpaper.pixels().any(|p| p.0[0] > 120), "no name and no lines on the wallpaper");
        assert_eq!(open("iso/splash.png").dimensions(), (640, 480));

        // He draws himself: light grows from frame to frame, and ends in his yellow.
        let light = |name: &str| open(name).pixels().map(|p| p.0.iter().map(|c| u64::from(*c)).sum::<u64>()).sum::<u64>();
        let frame = |k: usize| format!("plymouth/jaios/animation-{k:04}.png");
        let (first, middle, last) = (light(&frame(1)), light(&frame(REVEAL / 2)), light(&frame(REVEAL + SETTLE)));
        assert!(first < middle && middle < last, "the core does not grow: {first}, {middle}, {last}");
        let end = open(&frame(REVEAL + SETTLE));
        assert_eq!(end.dimensions(), (SIDE as u32, SIDE as u32));
        assert_eq!(end.get_pixel(1, 1).0, t.void, "a frame's corner is the canvas");
        assert!(end.pixels().any(|p| p.0[0] > 200 && p.0[1] > 150 && p.0[2] < 140), "no yellow in the finished core");
        // The throbber lights the reference ring alone, and its light runs round.
        let (start, half) = (open("plymouth/jaios/throbber-0001.png"), open(&format!("plymouth/jaios/throbber-{:04}.png", THROBBER / 2)));
        let middle = start.get_pixel(SIDE as u32 / 2, SIDE as u32 / 2).0;
        assert!((0..3).all(|c| middle[c] <= t.void[c] + 6), "the throbber's middle is empty, not {middle:?}");
        assert!(light("plymouth/jaios/throbber-0001.png") > light(&frame(1)) / 4 && start != half);

        // A strip of the animation for Sir to look at.
        let shown = [1, REVEAL / 4, REVEAL / 2, REVEAL * 3 / 4, REVEAL, REVEAL + SETTLE];
        let mut strip = image::RgbImage::new(SIDE as u32 * shown.len() as u32, SIDE as u32);
        for (i, k) in shown.iter().enumerate() {
            image::imageops::replace(&mut strip, &open(&frame(*k)), (i * SIDE) as i64, 0);
        }
        strip.save(dir.join("plymouth_strip.png")).expect("the strip");

        let read = |name: &str| std::fs::read_to_string(dir.join(name)).expect(name);
        assert!(read("plymouth/jaios/jaios.plymouth").contains(&format!("BackgroundStartColor=0x{}\n", hex(t.void))));
        let limine = read("limine/theme.conf");
        assert!(limine.contains(&format!("term_foreground: {}\n", hex(t.ink))) && limine.contains("timeout: 3\n") && limine.contains("remember_last_entry: yes\n"));
        assert!(limine.contains(&format!("interface_branding_colour: {}\n", hex(t.accent))));
        let palette = limine.lines().find_map(|l| l.strip_prefix("term_palette: ")).expect("a palette");
        assert_eq!(palette.split(';').collect::<Vec<_>>()[..4], [hex(t.void), hex(t.alert), hex(ansi(&t)[2]), hex(t.accent)]);
        for name in ["kde/JAI.colors", "kde/kdeglobals"] {
            let kde = read(name);
            assert_eq!(kde_value(&kde, "Colors:Selection", "BackgroundNormal"), rgb(t.accent), "{name}");
            assert_eq!(kde_value(&kde, "Colors:View", "BackgroundNormal"), rgb(t.void), "{name}");
            assert_eq!(kde_value(&kde, "Colors:Window", "ForegroundNormal"), rgb(t.ink), "{name}");
            assert_eq!(kde_value(&kde, "Colors:View", "ForegroundNegative"), rgb(t.alert), "{name}");
            assert_eq!(kde_value(&kde, "General", "ColorScheme"), "JAI", "{name}");
        }
        assert!(kde_value(&read("kde/kdeglobals"), "General", "font").starts_with("Barlow,"));
        assert_eq!(kde_value(&read("konsole/JAI.colorscheme"), "Color3", "Color"), rgb(t.accent));
        assert!(read("iso/archiso_head.cfg").contains(&format!("#ff{} all\n", hex(t.accent).to_lowercase())));
    }

    /// Text reads on its ground in every preset: KDE's sets and the terminal's colours.
    #[test]
    fn os_text_keeps_its_contrast_in_every_preset() {
        for (name, _, t) in holo_theme::PRESETS {
            for set in kde_sets(&t) {
                for (what, colour) in [("text", set.text), ("quiet text", set.quiet)] {
                    for ground in [set.ground, set.alternate] {
                        let ratio = holo::contrast(c32(colour), c32(ground));
                        assert!(ratio >= 4.5, "{name}: {what} on {} reads at {ratio:.2}", set.name);
                    }
                }
            }
            for (i, colour) in ansi(&t).iter().enumerate().filter(|(i, _)| i % 8 != 0) {
                let ratio = holo::contrast(c32(*colour), c32(t.void));
                assert!(ratio >= 4.5, "{name}: terminal colour {i} reads at {ratio:.2}");
            }
        }
    }
}
