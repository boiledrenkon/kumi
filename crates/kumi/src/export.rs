//! A conversation as a file to keep or share: the hooks Kumi's exports draw with (step words, pictures, the time zone,
//! the redactor `kumi report` uses), and files written beside the producer, private and never over another.
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use chrono::{FixedOffset, Local, Offset};
use kumi_runtime::core::contracts::ChangePicture;
use kumi_runtime::core::export::{html, json, markdown, Format, Options, Source};
use kumi_runtime::system::{self, Env};
use kumi_runtime::KUMI_VERSION;

use crate::report::{known_secrets, redactor, username};
use crate::tui::pictures::scene::{scene_of, Shape, GHOST, GUIDE, NOW};
use crate::tui::style::{palette, Rgb};
use crate::tui::transcript::step_label;

/// The most `/export` copies through the terminal; a longer conversation is saved as a file instead.
pub const COPY_MOST: usize = 64 * 1024;
/// A picture's size on the page, in its own units: wide, as the terminal draws it.
const WIDTH: f64 = 400.;
const HEIGHT: f64 = 160.;

/// The redactor `kumi report` uses, with what this computer knows to be secret: sign-ins, keys in the environment, the
/// home folder and the account name.
pub async fn redactor_for(env: &Env) -> Box<dyn Fn(&str) -> String> {
    let secrets = known_secrets(env).await;
    let home = home::home_dir().unwrap_or_default().display().to_string();
    Box::new(redactor(&secrets, &home, &username()))
}

/// A conversation in `format`. `redact` is None only for a file the producer asked for as it is (`--raw`).
pub fn render(format: Format, source: &Source, full: bool, redact: Option<&dyn Fn(&str) -> String>, now: i64) -> String {
    let same = |text: &str| text.to_string();
    let options = Options {
        full,
        redact: redact.unwrap_or(&same),
        redacted: redact.is_some(),
        label: &step_label,
        picture: &svg_of,
        zone: local_zone(),
        now,
        version: KUMI_VERSION,
    };
    match format {
        Format::Markdown => markdown(source, &options),
        Format::Html => html(source, &options),
        Format::Json => json(source, &options),
    }
}

/// This computer's time zone, as it is now.
pub fn local_zone() -> FixedOffset {
    Local::now().offset().fix()
}

/// A change's picture as an `<svg>` the page colours itself, light or dark: curves as lines, notes as boxes. None for a
/// picture that reads as values (the page writes those out), or one with nothing to draw.
pub fn svg_of(picture: &ChangePicture) -> Option<String> {
    let scene = scene_of(picture)?;
    let mut shapes = String::new();
    for shape in &scene.shapes {
        match shape {
            Shape::Line { points, color, alpha, width } => {
                let points: Vec<String> = points
                    .iter()
                    .filter(|(x, y)| x.is_finite() && y.is_finite())
                    .map(|(x, y)| format!("{:.1},{:.1}", x * WIDTH, (1. - y) * HEIGHT))
                    .collect();
                if points.len() < 2 {
                    continue;
                }
                shapes.push_str(&format!(
                    "<polyline{} points=\"{}\" stroke-width=\"{:.2}\" stroke-opacity=\"{:.2}\"/>",
                    paint(*color, "stroke"),
                    points.join(" "),
                    width / 1000. * HEIGHT,
                    alpha.clamp(0., 1.)
                ));
            }
            Shape::Rect { x0, y0, x1, y1, color, alpha } => {
                if ![x0, y0, x1, y1].iter().all(|value| value.is_finite()) {
                    continue;
                }
                shapes.push_str(&format!(
                    "<rect{} x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" fill-opacity=\"{:.2}\"/>",
                    paint(*color, "fill"),
                    x0.min(*x1) * WIDTH,
                    (1. - y0.max(*y1)) * HEIGHT,
                    (x1 - x0).abs() * WIDTH,
                    (y1 - y0).abs() * HEIGHT,
                    alpha.clamp(0., 1.)
                ));
            }
        }
    }
    (!shapes.is_empty())
        .then(|| format!("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {WIDTH} {HEIGHT}\" aria-hidden=\"true\">{shapes}</svg>"))
}

/// A shape's colour: the page's own for what's there now, what came before, a guide or what stayed (so it follows light
/// and dark), else the colour itself.
fn paint(color: Rgb, attribute: &str) -> String {
    let role = if color == NOW {
        "now"
    } else if color == GHOST {
        "ghost"
    } else if color == GUIDE {
        "guide"
    } else if color == palette::DIM {
        "dim"
    } else {
        return format!(" {attribute}=\"#{:02x}{:02x}{:02x}\"", color[0], color[1], color[2]);
    };
    format!(" class=\"{role}\"")
}

/// Writes `text` to `path`, or beside it as `name-2.ext`, `name-3.ext`… when that's taken: never over another file. Only
/// the producer can read it, where the system allows. Returns where it went.
pub fn write_new(path: &Path, text: &str) -> io::Result<PathBuf> {
    let stem = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    let extension = path.extension().map(|extension| format!(".{}", extension.to_string_lossy())).unwrap_or_default();
    for number in 1..=999 {
        let candidate = if number == 1 { path.to_path_buf() } else { path.with_file_name(format!("{stem}-{number}{extension}")) };
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&candidate) {
            Ok(mut file) => {
                if let Err(error) = file.write_all(text.as_bytes()).and_then(|()| file.flush()) {
                    drop(file);
                    let _ = std::fs::remove_file(&candidate);
                    return Err(error);
                }
                return Ok(candidate);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "999 files of that name are there already"))
}

/// Shows a file in the system's file manager: selected in Finder or Explorer, its folder elsewhere.
pub fn reveal(path: &Path) {
    let mut command = match system::platform() {
        "darwin" => {
            let mut command = std::process::Command::new("open");
            command.arg("-R").arg(path);
            command
        }
        "win32" => {
            let mut command = std::process::Command::new("explorer");
            command.arg(format!("/select,{}", path.display()));
            command
        }
        _ => {
            let mut command = std::process::Command::new("xdg-open");
            command.arg(path.parent().unwrap_or(Path::new(".")));
            command
        }
    };
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0000_0008 | 0x0000_0200);
    }
    if let Ok(mut child) = command.spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}
