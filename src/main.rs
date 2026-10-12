#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use rustcut::{app, export, model};

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

/// Writes panics (message, location, backtrace) to `crash.log` in the config folder and,
/// for the UI thread, shows a dialog — the release build has no console to print to.
fn install_crash_handler() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let report = format!(
            "=== RustCut {} crash (unix time {stamp}, thread '{thread}') ===
{info}

{backtrace}
",
            env!("CARGO_PKG_VERSION")
        );
        let dir = rustcut::util::config_dir();
        let path = dir.join("crash.log");
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            use std::io::Write;
            let _ = f.write_all(report.as_bytes());
        }
        if thread == "main" {
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title("RustCut crashed")
                .set_description(format!("{info}

Details were saved to {}", path.display()))
                .show();
        }
        default(info);
    }));
}

fn main() -> eframe::Result<()> {
    install_crash_handler();
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--export") {
        std::process::exit(headless_export(&args[2..]));
    }

    let initial = args.get(1).map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RustCut")
            .with_inner_size([1480.0, 920.0])
            .with_min_inner_size([1100.0, 680.0])
            .with_drag_and_drop(true)
            .with_icon(std::sync::Arc::new(
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/icons/rustcut-256.png")).unwrap_or_default(),
            )),
        ..Default::default()
    };
    eframe::run_native(
        "RustCut",
        options,
        Box::new(move |cc| Ok(Box::new(app::EditorApp::new(cc, initial)))),
    )
}

/// `rustcut --export <project.rcproj> <out.mp4> [short_side] [fps]`
fn headless_export(args: &[String]) -> i32 {
    let (Some(project), Some(out)) = (args.first(), args.get(1)) else {
        eprintln!("usage: rustcut --export <project.rcproj> <out.mp4> [short_side=1080] [fps]");
        return 2;
    };
    let project = match model::Project::load(project.as_ref()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e:#}");
            return 1;
        }
    };
    let settings = export::ExportSettings {
        path: out.into(),
        short_side: args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1080),
        fps: args.get(3).and_then(|s| s.parse().ok()).unwrap_or(project.fps),
        crf: 21,
        preset: "medium".into(),
    };
    let cancel = AtomicBool::new(false);
    let mut last = -1;
    let result = export::run(&project, &settings, &cancel, |p| {
        let pct = (p * 100.0) as i32;
        if pct != last {
            last = pct;
            eprint!("\rExporting… {pct:3}%");
        }
    });
    eprintln!();
    match result {
        Ok(_) => {
            eprintln!("Wrote {}", settings.path.display());
            0
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    }
}
