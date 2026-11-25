//! The main screen, laid out for the Touch Display 2 in portrait, 720 × 1280
//! (DD-89). A first layout to look at, not a designed S8 screen yet: the
//! screen layouts are still to draw (S8, test-plan open points).

use bess_core::colour::{Level, SampleLevels};
use bess_core::faults::{FaultInfo, Severity};
use bess_core::live::{LiveState, Values};
use bess_core::sample::Sample;
use bess_core::state::ShownState;
use eframe::egui::{self, Color32, CornerRadius, Frame, Margin, ProgressBar, RichText, Stroke, Ui};

/// Width the screen is designed for, in points.
pub const SCREEN_WIDTH: f32 = 720.0;

const TILE: Color32 = Color32::from_rgb(38, 42, 50);
const YELLOW: Color32 = Color32::from_rgb(242, 196, 58);
const RED: Color32 = Color32::from_rgb(214, 58, 48);
const GREEN: Color32 = Color32::from_rgb(52, 160, 90);
const BLUE: Color32 = Color32::from_rgb(58, 120, 214);
const GREY: Color32 = Color32::from_gray(110);
const TEXT: Color32 = Color32::from_gray(235);
const DIM: Color32 = Color32::from_gray(160);

/// How the values are to be drawn.
#[derive(Clone, Copy)]
enum Look {
    Live,
    Stale,
    Hidden,
}

fn fmt(value: Option<f64>, decimals: usize, unit: &str) -> String {
    value.map_or_else(|| "N/A".to_owned(), |x| format!("{x:.decimals$} {unit}"))
}

fn fmt_signed(value: Option<f64>, decimals: usize, unit: &str) -> String {
    value.map_or_else(|| "N/A".to_owned(), |x| format!("{x:+.decimals$} {unit}"))
}

/// Fill and text colour for a value tile.
fn colours(level: Level, look: Look) -> (Color32, Color32) {
    match (look, level) {
        (Look::Stale | Look::Hidden, _) => (TILE, GREY),
        (Look::Live, Level::Alarm) => (RED, Color32::WHITE),
        (Look::Live, Level::Warning) => (YELLOW, Color32::BLACK),
        (Look::Live, Level::Normal) => (TILE, TEXT),
    }
}

fn tile(ui: &mut Ui, title: &str, value: &str, level: Level, look: Look, size: f32) {
    let (fill, text) = colours(level, look);
    let value = match look {
        Look::Hidden => "--",
        Look::Live | Look::Stale => value,
    };
    Frame::new()
        .fill(fill)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(title)
                    .size(17.0)
                    .color(if fill == TILE { DIM } else { text }),
            );
            ui.label(RichText::new(value).size(size).strong().color(text));
        });
}

fn banner(ui: &mut Ui, fill: Color32, text_colour: Color32, text: &str) {
    Frame::new()
        .fill(fill)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).size(22.0).strong().color(text_colour));
        });
}

fn state_colour(state: ShownState) -> Color32 {
    match state {
        ShownState::EStop | ShownState::Fault => RED,
        ShownState::Charging => GREEN,
        ShownState::Discharging => BLUE,
        ShownState::Offline => GREY,
        ShownState::Idle => Color32::from_rgb(70, 76, 88),
        ShownState::Unknown(_) => Color32::from_rgb(140, 90, 180),
    }
}

/// Draws the whole screen.
pub fn screen(ui: &mut Ui, v: &LiveState) {
    ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);

    ui.horizontal(|ui| {
        ui.label(
            RichText::new("BESS monitor")
                .size(26.0)
                .strong()
                .color(TEXT),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(format!("link {}", v.link))
                    .size(16.0)
                    .color(DIM),
            );
        });
    });

    if v.critical_banner {
        let first = v
            .faults
            .iter()
            .find(|f| f.severity == Some(Severity::Critical))
            .map_or(String::new(), |f| format!(": {} {}", f.code, f.name));
        banner(ui, RED, Color32::WHITE, &format!("CRITICAL FAULT{first}"));
    }
    if v.shown.comm_lost {
        banner(
            ui,
            Color32::from_rgb(90, 90, 96),
            Color32::WHITE,
            "BMS communication lost",
        );
    } else if v.shown.waiting_for_config {
        banner(
            ui,
            Color32::from_rgb(70, 76, 120),
            Color32::WHITE,
            "Waiting for BMS configuration",
        );
    }

    // State (S8.9).
    Frame::new()
        .fill(state_colour(v.shown.state))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::symmetric(16, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(v.shown.state.to_string())
                        .size(54.0)
                        .strong()
                        .color(Color32::WHITE),
                );
            });
        });

    let (sample, levels, look) = match v.values {
        Values::Live(s, l) => (s, l, Look::Live),
        Values::Stale(s) => (s, SampleLevels::default(), Look::Stale),
        Values::Hidden => (Sample::default(), SampleLevels::default(), Look::Hidden),
    };
    values(ui, &sample, &levels, look);
    fans(ui, v);
    faults(ui, &v.faults, v.faults_stale);

    ui.add_space(6.0);
    let firmware = v
        .firmware
        .map_or_else(|| "--".to_owned(), |f| f.to_string());
    let capacity = v
        .capacity_ah
        .map_or_else(|| "--".to_owned(), |c| format!("{c:.1} Ah"));
    ui.label(
        RichText::new(format!("BMS firmware {firmware}   capacity {capacity}"))
            .size(15.0)
            .color(DIM),
    );
}

fn soc(ui: &mut Ui, s: &Sample, l: &SampleLevels, look: Look) {
    let (bar, text) = colours(l.soc, look);
    let soc = s.soc_pct;
    Frame::new()
        .fill(TILE)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("State of charge").size(17.0).color(DIM));
            let label = match look {
                Look::Hidden => "--".to_owned(),
                Look::Live | Look::Stale => fmt(soc, 1, "%"),
            };
            #[expect(clippy::cast_possible_truncation, reason = "a progress fraction")]
            let fraction = soc.map_or(0.0, |x| (x / 100.0) as f32);
            let fill = if bar == TILE {
                match look {
                    Look::Live => GREEN,
                    Look::Stale | Look::Hidden => GREY,
                }
            } else {
                bar
            };
            ui.add(
                ProgressBar::new(fraction)
                    .desired_height(44.0)
                    .fill(fill)
                    .text(
                        RichText::new(label)
                            .size(26.0)
                            .strong()
                            .color(if bar == TILE { TEXT } else { text }),
                    ),
            );
        });
}

fn values(ui: &mut Ui, s: &Sample, l: &SampleLevels, look: Look) {
    soc(ui, s, l, look);

    ui.columns(3, |c| {
        tile(
            &mut c[0],
            "Voltage",
            &fmt(s.pack_v, 2, "V"),
            Level::Normal,
            look,
            30.0,
        );
        tile(
            &mut c[1],
            "Current",
            &fmt_signed(s.current_a, 2, "A"),
            l.current,
            look,
            30.0,
        );
        tile(
            &mut c[2],
            "Power",
            &fmt_signed(s.power_w, 1, "W"),
            Level::Normal,
            look,
            30.0,
        );
    });

    ui.label(RichText::new("Cell groups").size(18.0).color(DIM));
    ui.columns(4, |c| {
        for (i, col) in c.iter_mut().enumerate() {
            tile(
                col,
                &format!("Group {}", i + 1),
                &fmt(s.cell_v[i], 3, "V"),
                l.cell[i],
                look,
                24.0,
            );
        }
    });
    let spread = s.cell_range().map(|(lo, hi)| (hi - lo) * 1000.0);
    if matches!(look, Look::Live) {
        ui.label(
            RichText::new(format!("Spread {}", fmt(spread, 0, "mV")))
                .size(15.0)
                .color(DIM),
        );
    }

    ui.label(RichText::new("Temperatures").size(18.0).color(DIM));
    ui.columns(2, |c| {
        tile(
            &mut c[0],
            "Cell (NTC1)",
            &fmt(s.t_cell_c, 1, "°C"),
            l.t_cell,
            look,
            28.0,
        );
        tile(
            &mut c[1],
            "Terminal (NTC2)",
            &fmt(s.t_terminal_c, 1, "°C"),
            l.t_terminal,
            look,
            28.0,
        );
    });
    ui.columns(2, |c| {
        tile(
            &mut c[0],
            "Air out (NTC3)",
            &fmt(s.t_air_out_c, 1, "°C"),
            l.t_air_out,
            look,
            28.0,
        );
        tile(
            &mut c[1],
            "Air in (NTC4)",
            &fmt(s.t_air_in_c, 1, "°C"),
            l.t_air_in,
            look,
            28.0,
        );
    });
}

fn fans(ui: &mut Ui, v: &LiveState) {
    Frame::new()
        .fill(TILE)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Fans").size(17.0).color(DIM));
            if let Some(fan) = v.fan {
                let label = if fan.duty_pct == 0 {
                    format!("Off - {}", fan.reason)
                } else {
                    format!("{} % - {}", fan.duty_pct, fan.reason)
                };
                ui.add(
                    ProgressBar::new(f32::from(fan.duty_pct) / 100.0)
                        .desired_height(36.0)
                        .fill(BLUE)
                        .text(RichText::new(label).size(20.0).strong().color(TEXT)),
                );
            } else {
                ui.label(RichText::new("--").size(20.0).color(GREY));
            }
        });
}

fn faults(ui: &mut Ui, list: &[FaultInfo], stale: bool) {
    let title = if stale {
        "Active faults (last known)"
    } else {
        "Active faults"
    };
    ui.label(RichText::new(title).size(18.0).color(DIM));
    if list.is_empty() {
        ui.label(RichText::new("None").size(18.0).color(TEXT));
        return;
    }
    for f in list {
        let (edge, severity) = match f.severity {
            Some(Severity::Critical) => (RED, "Critical"),
            Some(Severity::LatchedProtection) => (RED, "Latched protection"),
            Some(Severity::Protection) => (YELLOW, "Protection"),
            Some(Severity::Warning) => (YELLOW, "Warning"),
            None => (Color32::from_rgb(140, 90, 180), "Unknown code"),
        };
        Frame::new()
            .fill(TILE)
            .stroke(Stroke::new(2.0, if stale { GREY } else { edge }))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new(format!("{} {}  ({severity})", f.code, f.name))
                        .size(19.0)
                        .strong()
                        .color(if stale { GREY } else { TEXT }),
                );
                ui.label(RichText::new(f.description).size(15.0).color(DIM));
            });
    }
}
