//! The main window: settings, a scratch translate box, and backend health.
//!
//! Runs as a deferred viewport off the bubble's root, so closing it leaves the
//! translator running in the background — the bubble does not depend on this
//! window existing.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use eframe::egui;

use crate::config::{Config, LANGUAGES, Provider, language_name};
use crate::engine::Request;
use crate::translate::{TranslateError, Translation};

/// Narrow and tall: the panels are a single column of short rows, so height
/// is what keeps the whole thing visible without scrolling, and extra width
/// would only stretch the labels. Clamped to the display at runtime — see
/// `place_window`.
pub const WINDOW_SIZE: [f32; 2] = [470.0, 880.0];

/// Space left around the window when it has to be shrunk to fit the display.
/// The top allowance covers the menu bar, the bottom the Dock.
const SCREEN_MARGIN_X: f32 = 40.0;
const SCREEN_MARGIN_Y: f32 = 120.0;

/// Cap on the activity list. It is a convenience for re-reading a translation
/// that has already scrolled away, not a persistent history.
const MAX_RECENT: usize = 25;

const TEXT_PRIMARY: egui::Color32 = egui::Color32::from_gray(240);
const TEXT_SECONDARY: egui::Color32 = egui::Color32::from_gray(186);
const TEXT_MUTED: egui::Color32 = egui::Color32::from_gray(155);
const OK_GREEN: egui::Color32 = egui::Color32::from_rgb(120, 210, 140);
const WARN_AMBER: egui::Color32 = egui::Color32::from_rgb(245, 195, 130);
const ERR_RED: egui::Color32 = egui::Color32::from_rgb(255, 150, 150);

pub struct MainState {
    /// Whether the window should currently exist.
    pub open: bool,
    /// Cleared whenever the window is (re)opened so its size is reasserted;
    /// see the note in `draw`.
    pub sized: bool,
    /// Set when the user asks for the window while it is already open, so it
    /// gets raised instead of quietly staying behind other apps.
    pub focus_requested: bool,
    pub requests: Sender<Request>,
    pub permission_ok: bool,

    // Scratch translate box.
    pub input: String,
    pub translating: bool,
    pub result: Option<Result<Translation, Vec<(Provider, TranslateError)>>>,

    // Backend health panel.
    pub testing: bool,
    pub statuses: Vec<(Provider, Result<String, String>)>,

    /// What the bubble has translated this session, newest first.
    pub recent: Vec<RecentEntry>,
}

pub struct RecentEntry {
    pub source: String,
    pub translated: String,
    pub provider: Provider,
}

impl MainState {
    pub fn new(requests: Sender<Request>, permission_ok: bool) -> Self {
        Self {
            open: true,
            sized: false,
            focus_requested: false,
            requests,
            permission_ok,
            input: String::new(),
            translating: false,
            result: None,
            testing: false,
            statuses: Vec::new(),
            recent: Vec::new(),
        }
    }

    pub fn push_recent(&mut self, source: String, translated: String, provider: Provider) {
        self.recent.insert(
            0,
            RecentEntry {
                source,
                translated,
                provider,
            },
        );
        self.recent.truncate(MAX_RECENT);
    }
}

pub fn draw(ui: &mut egui::Ui, state: &Arc<Mutex<MainState>>, config: &Arc<Mutex<Config>>) {
    let mut state = state.lock().unwrap();
    let mut cfg = config.lock().unwrap();
    let mut dirty = false;

    if !state.sized {
        place_window(ui.ctx());
        state.sized = true;
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(4.0);
            header(ui, &state);
            ui.add_space(10.0);

            section(ui, "Translate", |ui| translate_box(ui, &mut state, &cfg));
            section(ui, "Languages", |ui| {
                dirty |= languages(ui, &mut state, &mut cfg);
            });
            section(ui, "Providers", |ui| {
                dirty |= providers(ui, &mut state, &mut cfg);
            });
            section(ui, "Behaviour", |ui| {
                dirty |= behaviour(ui, &mut cfg);
            });
            if !state.recent.is_empty() {
                section(ui, "Recent", |ui| recent(ui, &state));
            }

            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(format!("Settings file: {}", Config::path().display()))
                    .size(10.5)
                    .color(TEXT_MUTED),
            );
            ui.add_space(8.0);
        });

    if dirty {
        let _ = cfg.save();
    }
}

/// Sizes and positions the window on the first frame after it opens.
///
/// Two reasons this cannot be left to the viewport builder. A deferred
/// viewport ignores the builder's `inner_size` and comes up at its minimum
/// size instead. And the default placement takes no account of the display, so
/// the preferred height can put the lower part of the window past the bottom
/// edge — which silently breaks every dropdown, because a popup opens downward
/// into the off-screen remainder of the window and is simply never seen.
///
/// So: clamp to the display, then centre.
fn place_window(ctx: &egui::Context) {
    let monitor = ctx.input(|i| i.viewport().monitor_size);

    let Some(monitor) = monitor else {
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(WINDOW_SIZE.into()));
        return;
    };

    let width = WINDOW_SIZE[0].min(monitor.x - SCREEN_MARGIN_X);
    let height = WINDOW_SIZE[1].min(monitor.y - SCREEN_MARGIN_Y);
    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(width, height)));

    let x = ((monitor.x - width) / 2.0).max(SCREEN_MARGIN_X / 2.0);
    let y = ((monitor.y - height) / 2.0).max(SCREEN_MARGIN_Y / 4.0);
    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(x, y)));
}

fn section(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    ui.label(
        egui::RichText::new(title)
            .size(12.0)
            .color(TEXT_MUTED)
            .strong(),
    );
    ui.add_space(4.0);
    egui::Frame::new()
        .fill(egui::Color32::from_rgb(38, 39, 43))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            body(ui);
        });
    ui.add_space(14.0);
}

fn header(ui: &mut egui::Ui, state: &MainState) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Bubble Translate")
                .size(21.0)
                .color(TEXT_PRIMARY)
                .strong(),
        );
        ui.label(
            egui::RichText::new("by pelamx")
                .size(11.5)
                .color(TEXT_MUTED),
        );
    });
    ui.add_space(2.0);

    if state.permission_ok {
        ui.label(
            egui::RichText::new("● Watching for selections")
                .size(12.5)
                .color(OK_GREEN),
        );
        ui.label(
            egui::RichText::new(
                "Select text in any app — double-click a word, drag a phrase, \
                 or triple-click a line.",
            )
            .size(12.0)
            .color(TEXT_MUTED),
        );
    } else {
        ui.label(
            egui::RichText::new("● Accessibility permission not granted")
                .size(12.5)
                .color(ERR_RED),
        );
        ui.label(
            egui::RichText::new(
                "Selections cannot be read. Enable bubbleTranslate in System Settings › \
                 Privacy & Security › Accessibility, then restart the app — the \
                 event tap is installed at startup.",
            )
            .size(12.0)
            .color(WARN_AMBER),
        );
    }
}

fn translate_box(ui: &mut egui::Ui, state: &mut MainState, cfg: &Config) {
    ui.add(
        egui::TextEdit::multiline(&mut state.input)
            .desired_rows(3)
            .desired_width(f32::INFINITY)
            .hint_text("Type or paste text to translate…"),
    );
    ui.add_space(6.0);

    ui.horizontal(|ui| {
        let can_send = !state.input.trim().is_empty() && !state.translating;
        if ui
            .add_enabled(can_send, egui::Button::new("Translate"))
            .clicked()
        {
            state.translating = true;
            state.result = None;
            let _ = state.requests.send(Request::Manual(state.input.clone()));
        }
        if ui.button("Clear").clicked() {
            state.input.clear();
            state.result = None;
        }
        if state.translating {
            ui.spinner();
        }
        ui.label(
            egui::RichText::new(format!("→ {}", language_name(&cfg.target_lang)))
                .size(11.5)
                .color(TEXT_MUTED),
        );
    });

    match &state.result {
        None => {}
        Some(Ok(result)) => {
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(&result.text)
                    .size(cfg.font_size)
                    .line_height(Some(cfg.font_size * 1.45))
                    .color(TEXT_PRIMARY),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{} · {} → {}",
                        result.provider.label(),
                        language_name(&result.source_lang),
                        language_name(&cfg.target_lang),
                    ))
                    .size(11.0)
                    .color(TEXT_MUTED),
                );
                if ui.small_button("Copy").clicked() {
                    crate::capture::set_clipboard(&result.text);
                }
            });
        }
        Some(Err(errors)) => {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("No provider could translate this")
                    .size(13.0)
                    .color(ERR_RED),
            );
            for (provider, err) in errors {
                ui.label(
                    egui::RichText::new(format!("{}: {err}", provider.label()))
                        .size(11.5)
                        .color(TEXT_MUTED),
                );
            }
        }
    }
}

fn languages(ui: &mut egui::Ui, state: &mut MainState, cfg: &mut Config) -> bool {
    let mut dirty = false;

    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Translate into")
                .size(12.5)
                .color(TEXT_SECONDARY),
        );
        let current = cfg.target_lang.clone();
        let mut picked: Option<String> = None;
        egui::ComboBox::from_id_salt("main-target")
            .selected_text(language_name(&current))
            .width(170.0)
            .show_ui(ui, |ui| {
                for (code, name) in LANGUAGES {
                    if ui.selectable_label(*code == current, *name).clicked() {
                        picked = Some((*code).to_string());
                    }
                }
            });
        if let Some(code) = picked {
            if code != cfg.target_lang {
                cfg.target_lang = code;
                dirty = true;
                // Apply the change to the text already captured, so the bubble
                // updates without needing a fresh selection.
                let _ = state.requests.send(Request::Retranslate);
            }
        }
    });

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Source language")
                .size(12.5)
                .color(TEXT_SECONDARY),
        );
        let current = cfg.source_lang.clone();
        let label = if current == "auto" {
            "Detect automatically"
        } else {
            language_name(&current)
        };
        let mut picked: Option<String> = None;
        egui::ComboBox::from_id_salt("main-source")
            .selected_text(label)
            .width(170.0)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(current == "auto", "Detect automatically")
                    .clicked()
                {
                    picked = Some("auto".to_string());
                }
                for (code, name) in LANGUAGES {
                    if ui.selectable_label(*code == current, *name).clicked() {
                        picked = Some((*code).to_string());
                    }
                }
            });
        if let Some(code) = picked {
            if code != cfg.source_lang {
                cfg.source_lang = code;
                dirty = true;
            }
        }
    });

    dirty
}

fn providers(ui: &mut egui::Ui, state: &mut MainState, cfg: &mut Config) -> bool {
    let mut dirty = false;

    ui.label(
        egui::RichText::new("Tried top to bottom; the first one to answer wins.")
            .size(11.5)
            .color(TEXT_MUTED),
    );
    ui.add_space(6.0);

    // Reordering is applied after the loop so the list is not mutated while
    // it is being drawn.
    let mut swap: Option<(usize, usize)> = None;
    let count = cfg.providers.len();

    for (index, provider) in cfg.providers.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("{}.", index + 1))
                    .size(12.5)
                    .color(TEXT_MUTED),
            );
            ui.label(
                egui::RichText::new(provider.label())
                    .size(13.0)
                    .color(TEXT_PRIMARY),
            );

            // DeepL without a key never reaches the network, so say so here
            // rather than letting it look like a silent failure.
            if *provider == Provider::DeepL && cfg.deepl_api_key.trim().is_empty() {
                ui.label(
                    egui::RichText::new("(skipped — no API key)")
                        .size(11.0)
                        .color(TEXT_MUTED),
                );
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(index + 1 < count, egui::Button::new("↓").small())
                    .clicked()
                {
                    swap = Some((index, index + 1));
                }
                if ui
                    .add_enabled(index > 0, egui::Button::new("↑").small())
                    .clicked()
                {
                    swap = Some((index, index - 1));
                }
            });
        });
    }
    if let Some((a, b)) = swap {
        cfg.providers.swap(a, b);
        dirty = true;
    }

    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("DeepL API key")
                .size(12.5)
                .color(TEXT_SECONDARY),
        );
        if ui
            .add(
                egui::TextEdit::singleline(&mut cfg.deepl_api_key)
                    .password(true)
                    .hint_text("optional — free keys end in :fx")
                    .desired_width(220.0),
            )
            .lost_focus()
        {
            dirty = true;
        }
    });

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("MyMemory email")
                .size(12.5)
                .color(TEXT_SECONDARY),
        );
        if ui
            .add(
                egui::TextEdit::singleline(&mut cfg.mymemory_email)
                    .hint_text("optional — raises the daily quota")
                    .desired_width(220.0),
            )
            .lost_focus()
        {
            dirty = true;
        }
    });

    ui.add_space(10.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(!state.testing, egui::Button::new("Test providers"))
            .clicked()
        {
            state.testing = true;
            state.statuses.clear();
            let _ = state.requests.send(Request::TestProviders);
        }
        if state.testing {
            ui.spinner();
        }
    });

    if !state.statuses.is_empty() {
        ui.add_space(6.0);
        for (provider, outcome) in &state.statuses {
            ui.horizontal(|ui| {
                let (mark, colour, detail) = match outcome {
                    Ok(text) => ("ok", OK_GREEN, text.clone()),
                    Err(err) => ("fail", ERR_RED, err.clone()),
                };
                ui.label(
                    egui::RichText::new(format!("{:<9}", provider.label()))
                        .size(11.5)
                        .color(TEXT_SECONDARY),
                );
                ui.label(egui::RichText::new(mark).size(11.5).color(colour));
                ui.label(egui::RichText::new(detail).size(11.5).color(TEXT_MUTED));
            });
        }
    }

    dirty
}

fn behaviour(ui: &mut egui::Ui, cfg: &mut Config) -> bool {
    let mut dirty = false;

    dirty |= ui
        .checkbox(&mut cfg.auto_translate, "Translate on selection")
        .changed();
    dirty |= ui
        .checkbox(
            &mut cfg.clipboard_fallback,
            "Use copy fallback when a app hides its selection",
        )
        .on_hover_text(
            "Needed for terminals and PDF viewers, which expose nothing over the \
             Accessibility API. Briefly borrows the clipboard and restores the \
             text afterwards.",
        )
        .changed();

    ui.add_space(8.0);
    dirty |= slider(
        ui,
        "Bubble text size",
        &mut cfg.font_size,
        12.0..=26.0,
        "pt",
    );

    ui.add_space(4.0);
    let mut hide = cfg.auto_hide_secs as f32;
    if slider(ui, "Auto-hide after", &mut hide, 0.0..=60.0, "s") {
        cfg.auto_hide_secs = hide as u64;
        dirty = true;
    }
    ui.label(
        egui::RichText::new("0 keeps the bubble up until closed. Pauses while hovered.")
            .size(10.5)
            .color(TEXT_MUTED),
    );

    ui.add_space(6.0);
    let mut debounce = cfg.debounce_ms as f32;
    if slider(ui, "Settle delay", &mut debounce, 50.0..=600.0, "ms") {
        cfg.debounce_ms = debounce as u64;
        dirty = true;
    }
    ui.label(
        egui::RichText::new("Raise this if a slow app returns a stale selection.")
            .size(10.5)
            .color(TEXT_MUTED),
    );

    ui.add_space(6.0);
    let mut max = cfg.max_chars as f32;
    if slider(ui, "Longest selection", &mut max, 100.0..=8000.0, " chars") {
        cfg.max_chars = max as usize;
        dirty = true;
    }

    dirty
}

fn slider(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    suffix: &str,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).size(12.5).color(TEXT_SECONDARY));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(format!("{:.0}{suffix}", *value))
                    .size(11.5)
                    .color(TEXT_MUTED),
            );
            changed = ui
                .add(
                    egui::Slider::new(value, range)
                        .show_value(false)
                        .trailing_fill(true),
                )
                .drag_stopped();
        });
    });
    changed
}

fn recent(ui: &mut egui::Ui, state: &MainState) {
    for entry in state.recent.iter().take(10) {
        ui.label(
            egui::RichText::new(truncate(&entry.source, 70))
                .size(11.0)
                .color(TEXT_MUTED),
        );
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(truncate(&entry.translated, 90))
                    .size(12.5)
                    .color(TEXT_PRIMARY),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Copy").clicked() {
                    crate::capture::set_clipboard(&entry.translated);
                }
                ui.label(
                    egui::RichText::new(entry.provider.label())
                        .size(10.0)
                        .color(TEXT_MUTED),
                );
            });
        });
        ui.add_space(8.0);
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    let mut out: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        out.push('…');
    }
    out
}
