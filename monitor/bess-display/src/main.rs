//! `bess-display`, the touchscreen app (DD-02, DD-87), first preview.
//!
//! There is no API yet, so this preview runs the real `bess-core` core
//! (every task, `Core::run`) inside the app, against a demo MCU over the
//! real UART bytes (see `pipeline` and `sim`). The core writes a real
//! database, which the demo panel exports as CSV (see `export`).
//! The window shows the screen at the Touch Display 2 size (720 × 1280,
//! portrait, DD-89), scaled to fit, with demo controls on the right.
//!
//! ```text
//! bess-display [--kiosk] [--data DIR]
//! ```
//!
//! `--kiosk` runs full-screen at 1:1 without the demo controls, as on the
//! Pi. `--data` is where the database (`bess.db`) and the exports
//! (`exports/`) go; by default `bess-display-preview` in the temp folder.
//! The database is kept between runs, so 10-min and daily records build up.

mod export;
mod pipeline;
mod sim;
mod ui;

use bess_core::csv::Format;
use bess_core::db::StoredCounts;
use bess_core::history::Tier;
use bess_core::tasks::hub;
use eframe::egui;
use export::{Exported, SPANS, Sources};
use pipeline::{Preview, lock};
use sim::Mode;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Screen scale in the desktop window: 720 × 1280 shown as 450 × 800.
const PREVIEW_ZOOM: f32 = 0.625;
/// Width of the demo panel, in points.
const DEMO_WIDTH: f32 = 520.0;
/// How often the panel re-reads the database row counts.
const COUNTS_EVERY: Duration = Duration::from_secs(2);

/// The export section of the demo panel.
struct ExportPanel {
    sources: Sources,
    /// Index into [`SPANS`].
    span: usize,
    /// A coarser tier than the range needs (DD-49); `None` = automatic.
    coarser: Option<Tier>,
    last: Option<Result<Exported, String>>,
    counts: Option<(Instant, Result<StoredCounts, String>)>,
}

impl ExportPanel {
    /// The range, resolution and Excel format boxes.
    fn choices(&mut self, ui: &mut egui::Ui) {
        egui::ComboBox::from_label("range")
            .selected_text(SPANS[self.span].0)
            .show_ui(ui, |ui| {
                for (i, (label, _)) in SPANS.iter().enumerate() {
                    ui.selectable_value(&mut self.span, i, *label);
                }
            });
        egui::ComboBox::from_label("resolution")
            .selected_text(match self.coarser {
                None => "automatic",
                Some(Tier::Ring) => "1 s",
                Some(Tier::TenMin) => "10-min",
                Some(Tier::Daily) => "daily",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.coarser, None, "automatic");
                ui.selectable_value(&mut self.coarser, Some(Tier::TenMin), "10-min at least");
                ui.selectable_value(&mut self.coarser, Some(Tier::Daily), "daily");
            });
        let format = &mut self.sources.format;
        egui::ComboBox::from_label("Excel number settings")
            .selected_text(format_name(*format))
            .show_ui(ui, |ui| {
                for f in [Format::English, Format::Dutch] {
                    ui.selectable_value(format, f, format_name(f));
                }
            });
    }
}

/// The CSV format as the panel names it (DD-62).
fn format_name(format: Format) -> &'static str {
    match format {
        Format::English => "English: , between cells, . decimal",
        Format::Dutch => "Dutch: ; between cells, , decimal",
    }
}

struct App {
    /// Runs the core and the demo MCU; dropped when the window closes.
    runtime: tokio::runtime::Runtime,
    preview: Preview,
    export: ExportPanel,
    demo: bool,
}

impl App {
    fn new(demo: bool, data: &std::path::Path) -> App {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_time()
            .build()
            .expect("tokio runtime");
        let preview = {
            let _entered = runtime.enter();
            pipeline::start(Some(data.join("bess.db")))
        };
        let export = ExportPanel {
            sources: Sources {
                ring: preview.ring.clone(),
                db: preview.db.clone(),
                dir: data.join("exports"),
                tz: jiff::tz::TimeZone::system(),
                // Matches this PC's Excel, so the columns split on double-click.
                format: export::system_format(),
            },
            span: 1,
            coarser: None,
            last: None,
            counts: None,
        };
        App {
            runtime,
            preview,
            export,
            demo,
        }
    }

    fn demo_panel(&mut self, ui: &mut egui::Ui) {
        let now = hub::now();
        ui.heading("Demo controls");
        ui.label("Simulated MCU, not part of the Pi app. The Pi side runs the real bess-core tasks on the real UART bytes.");
        ui.separator();
        let mut mcu = lock(&self.preview.mcu);
        let c = &mut mcu.controls;
        ui.horizontal(|ui| {
            ui.radio_value(&mut c.mode, Mode::Idle, "Idle");
            ui.radio_value(&mut c.mode, Mode::Charging, "Charging");
            ui.radio_value(&mut c.mode, Mode::Discharging, "Discharging");
        });
        ui.add(
            egui::Slider::new(&mut c.current_a, 0.0..=10.0)
                .text("current")
                .suffix(" A"),
        );
        ui.add(
            egui::Slider::new(&mut c.cell_temp_c, -20.0..=70.0)
                .text("cell temp (NTC1)")
                .suffix(" °C"),
        );
        ui.add(
            egui::Slider::new(&mut c.air_temp_c, 0.0..=60.0)
                .text("air temp (NTC3)")
                .suffix(" °C"),
        );
        ui.label("E05 above 55 °C cell, E17 above 45 °C air; fans: 30 °C on, 27 °C off.");
        ui.checkbox(&mut c.e_stop, "E-STOP pressed (E08)");
        ui.checkbox(&mut c.ntc1_fault, "NTC1 sensor fault (E09)");
        ui.checkbox(&mut c.imbalance, "Cell imbalance (E13)");
        ui.checkbox(&mut c.unknown_fault, "Unknown fault bit (E24)");
        ui.checkbox(&mut c.link_cut, "Cut the UART link");
        if ui.button("Restart MCU (new boot_counter)").clicked() {
            mcu.restart(now);
        }
        ui.separator();
        let identity = mcu.identity();
        ui.label(format!(
            "MCU boot {} revision {}, SoC {:.1} %",
            identity.boot_counter,
            identity.config_revision,
            mcu.soc_pct()
        ));
        match mcu.pi_heartbeat(now) {
            Some(h) if h == identity => {
                ui.label("MCU hears an identity-matched heartbeat: charging allowed.")
            }
            Some(_) => ui.label("MCU hears a heartbeat for another identity."),
            None => ui.label("MCU hears no accepted heartbeat (identity 0): charging inhibited."),
        };
        drop(mcu);
        let link = self.preview.link.borrow().clone();
        ui.label(format!(
            "Pi link: {}, {} outages since start",
            link.state, link.outages
        ));
        ui.separator();
        self.export_section(ui);
        ui.separator();
        ui.label("Core events (newest first)");
        for (at, text) in lock(&self.preview.events).iter() {
            ui.label(
                egui::RichText::new(format!("{at:6.1} s  {text}"))
                    .monospace()
                    .size(13.0),
            );
        }
    }

    /// Export to CSV what the core stored (S10.11), as the dashboard
    /// download will once `api_server` exists.
    fn export_section(&mut self, ui: &mut egui::Ui) {
        let e = &mut self.export;
        ui.heading("Export (CSV)");
        if let Some(error) = &self.preview.db_error {
            ui.colored_label(
                egui::Color32::LIGHT_RED,
                format!("No database file, only the ring buffer: {error}"),
            );
        }
        e.choices(ui);
        let now = jiff::Timestamp::now();
        let span_ms = SPANS[e.span].1;
        ui.label(format!(
            "Reads the {} (DD-49).",
            export::tier_name(export::tier_for(span_ms, now, e.coarser))
        ));
        ui.horizontal(|ui| {
            if ui.button("Export CSV").clicked() {
                e.last = Some(e.sources.export(span_ms, e.coarser, now));
                e.counts = None;
            }
            if ui.button("Export latest fault snapshot").clicked() {
                e.last = Some(e.sources.export_latest_snapshot(now));
            }
        });
        let mut open = None;
        match &e.last {
            None => {}
            Some(Err(error)) => {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            }
            Some(Ok(Exported {
                rows,
                path: Some(path),
                ..
            })) => {
                let name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                ui.label(format!("Wrote {rows} rows to {name}"));
                ui.horizontal(|ui| {
                    if ui.button("Open file").clicked() {
                        open = Some(path.clone());
                    }
                    if ui.button("Open folder").clicked() {
                        open = Some(e.sources.dir.clone());
                    }
                });
            }
            Some(Ok(Exported {
                tier, path: None, ..
            })) => {
                let hint = tier.map_or("", export::empty_hint);
                ui.label(format!("Nothing stored in that range yet: {hint}."));
            }
        }
        if let Some(path) = open
            && let Err(error) = export::open_path(&path)
        {
            e.last = Some(Err(error));
        }
        let stale = e
            .counts
            .as_ref()
            .is_none_or(|(at, _)| at.elapsed() > COUNTS_EVERY);
        if stale && e.sources.db.is_some() {
            e.counts = Some((Instant::now(), e.sources.counts()));
        }
        match e.counts.as_ref().map(|(_, c)| c) {
            Some(Ok(c)) => ui.label(format!(
                "Stored: {} 10-min records, {} daily records, {} snapshots, {} events.",
                c.ten_min, c.daily, c.snapshots, c.events
            )),
            Some(Err(error)) => ui.colored_label(egui::Color32::LIGHT_RED, error),
            None => ui.label("Stored: nothing (no database file)."),
        };
        ui.label(
            egui::RichText::new(format!("Folder: {}", e.sources.dir.display()))
                .small()
                .weak(),
        );
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let view = self.preview.live.borrow().clone();
        if self.demo {
            egui::Panel::right("demo")
                .resizable(false)
                .exact_size(DEMO_WIDTH)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| self.demo_panel(ui));
                });
        }
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.set_max_width(ui::SCREEN_WIDTH);
                ui::screen(ui, &view);
            });
        });
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }

    /// The window closes: stop the core cleanly, so the open 10-min
    /// interval and `SHUTDOWN` are written.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.preview.stop(&self.runtime);
    }
}

struct Args {
    kiosk: bool,
    data: PathBuf,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        kiosk: false,
        data: std::env::temp_dir().join("bess-display-preview"),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--kiosk" => args.kiosk = true,
            "--data" => args.data = it.next().ok_or("--data needs a folder")?.into(),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(args)
}

fn main() -> eframe::Result {
    let args = match parse_args() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("bess-display: {e}\nusage: bess-display [--kiosk] [--data DIR]");
            std::process::exit(2);
        }
    };
    let kiosk = args.kiosk;
    let zoom = if kiosk { 1.0 } else { PREVIEW_ZOOM };
    let viewport = if kiosk {
        egui::ViewportBuilder::default().with_fullscreen(true)
    } else {
        egui::ViewportBuilder::default()
            .with_inner_size([(ui::SCREEN_WIDTH + DEMO_WIDTH + 30.0) * zoom, 1280.0 * zoom])
    };
    let options = eframe::NativeOptions {
        viewport: viewport.with_title("BESS monitor (preview)"),
        ..Default::default()
    };
    eframe::run_native(
        "bess-display",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_zoom_factor(zoom);
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            Ok(Box::new(App::new(!kiosk, &args.data)))
        }),
    )
}
