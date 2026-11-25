//! `bess-display`, the touchscreen app (DD-02, DD-87), first preview.
//!
//! There is no API yet, so this preview runs the real `bess-core` core
//! (every task, `Core::run`) inside the app, against a demo MCU over the
//! real UART bytes (see `pipeline` and `sim`).
//! The window shows the screen at the Touch Display 2 size (720 × 1280,
//! portrait, DD-89), scaled to fit, with demo controls on the right.
//!
//! `bess-display --kiosk` runs full-screen at 1:1 without the demo controls,
//! as on the Pi.

mod pipeline;
mod sim;
mod ui;

use bess_core::tasks::hub;
use eframe::egui;
use pipeline::{lock, Preview};
use sim::Mode;
use std::time::Duration;

/// Screen scale in the desktop window: 720 × 1280 shown as 450 × 800.
const PREVIEW_ZOOM: f32 = 0.625;
/// Width of the demo panel, in points.
const DEMO_WIDTH: f32 = 520.0;

struct App {
    /// Runs the core and the demo MCU; dropped when the window closes.
    _runtime: tokio::runtime::Runtime,
    preview: Preview,
    demo: bool,
}

impl App {
    fn new(demo: bool) -> App {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_time()
            .build()
            .expect("tokio runtime");
        let preview = {
            let _entered = runtime.enter();
            pipeline::start()
        };
        App {
            _runtime: runtime,
            preview,
            demo,
        }
    }

    fn demo_panel(&self, ui: &mut egui::Ui) {
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
        ui.label("Core events (newest first)");
        for (at, text) in lock(&self.preview.events).iter() {
            ui.label(
                egui::RichText::new(format!("{at:6.1} s  {text}"))
                    .monospace()
                    .size(13.0),
            );
        }
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
}

fn main() -> eframe::Result {
    let kiosk = std::env::args().any(|a| a == "--kiosk");
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
            Ok(Box::new(App::new(!kiosk)))
        }),
    )
}
