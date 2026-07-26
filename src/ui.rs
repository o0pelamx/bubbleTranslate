//! The floating bubble.
//!
//! One borderless, always-on-top viewport that spends most of its life hidden.
//! When a translation starts it moves to the cursor and shows itself; it never
//! takes keyboard focus, so the app you were reading stays frontmost and its
//! selection stays intact.

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;

use crate::capture::{self, CaptureSource};
use crate::config::{Config, LANGUAGES, Provider, language_name};
use crate::engine::{Engine, Request, UiEvent};
use crate::main_window::{self, MainState};
use crate::monitor;
use crate::status_item;
use crate::translate::{TranslateError, Translation};
use objc2_foundation::MainThreadMarker;

pub const BUBBLE_WIDTH: f32 = 400.0;
const MIN_HEIGHT: f32 = 90.0;
const MAX_HEIGHT: f32 = 460.0;
/// Offset from the pointer so the bubble never lands under the cursor itself.
const CURSOR_OFFSET: (f32, f32) = (14.0, 20.0);

/// Bubble palette.
///
/// The translation is the one thing worth reading, so it gets near-white on a
/// deliberately dark panel; everything else is chrome and steps down from
/// there. These are all well above the dim greys that make small text on a
/// dark background hard to read.
const TEXT_PRIMARY: egui::Color32 = egui::Color32::from_gray(240);
const TEXT_SECONDARY: egui::Color32 = egui::Color32::from_gray(186);
const TEXT_MUTED: egui::Color32 = egui::Color32::from_gray(158);
const BUBBLE_BG: egui::Color32 = egui::Color32::from_rgb(30, 31, 34);
const BUBBLE_BORDER: egui::Color32 = egui::Color32::from_gray(88);
const TEXT_ERROR: egui::Color32 = egui::Color32::from_rgb(255, 150, 150);

/// Multiplier on the font size for line spacing. Translated paragraphs are
/// often long sentences with no visual breaks, and the extra leading is what
/// makes them scannable.
const LINE_HEIGHT_RATIO: f32 = 1.45;

/// A macOS font with broad script coverage, added as a fallback so Chinese,
/// Japanese, Korean, Arabic and Cyrillic output renders instead of showing
/// missing-glyph boxes. egui's bundled fonts are Latin-only.
const FALLBACK_FONT: &str = "/System/Library/Fonts/Supplemental/Arial Unicode.ttf";

enum State {
    Hidden,
    Working {
        source_text: String,
        via: CaptureSource,
    },
    Done {
        source_text: String,
        result: Translation,
    },
    Failed {
        source_text: String,
        errors: Vec<(Provider, TranslateError)>,
    },
}

pub struct BubbleApp {
    config: Arc<Mutex<Config>>,
    engine: Engine,
    events: Receiver<UiEvent>,
    state: State,
    /// Where the bubble should sit, in global points (top-left origin).
    anchor: (f64, f64),
    visible: bool,
    /// Height requested for the viewport last frame, to avoid re-sending an
    /// identical resize every frame.
    last_height: f32,
    shown_at: Instant,
    settings_open: bool,
    copied_at: Option<Instant>,
    permission_missing: bool,
    /// Shared with the main window's deferred viewport callback, which must be
    /// `Send + Sync + 'static` and so cannot borrow from here.
    main: Arc<Mutex<MainState>>,
    config_for_main: Arc<Mutex<Config>>,
    reopen_hooked: bool,
    /// Mirrors the current activation policy so it is only set when it changes.
    dock_visible: bool,
}

impl BubbleApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        config: Arc<Mutex<Config>>,
        engine: Engine,
        events: Receiver<UiEvent>,
        permission_missing: bool,
        main: Arc<Mutex<MainState>>,
    ) -> Self {
        install_fonts(&cc.egui_ctx);

        let mut visuals = egui::Visuals::dark();
        visuals.window_fill = BUBBLE_BG;
        visuals.panel_fill = BUBBLE_BG;
        // Applies to widget labels that do not set a colour themselves;
        // explicit RichText colours still win.
        visuals.override_text_color = Some(TEXT_PRIMARY);
        cc.egui_ctx.set_visuals(visuals);

        Self {
            config_for_main: config.clone(),
            main,
            config,
            engine,
            events,
            state: State::Hidden,
            anchor: (0.0, 0.0),
            visible: false,
            last_height: 0.0,
            shown_at: Instant::now(),
            settings_open: false,
            copied_at: None,
            permission_missing,
            reopen_hooked: false,
            dock_visible: false,
        }
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                UiEvent::Working {
                    at,
                    source_text,
                    via,
                } => {
                    self.anchor = at;
                    self.state = State::Working { source_text, via };
                    self.settings_open = false;
                    self.copied_at = None;
                    self.show(ctx);
                }
                UiEvent::Done {
                    source_text,
                    result,
                } => {
                    self.main.lock().unwrap().push_recent(
                        source_text.clone(),
                        result.text.clone(),
                        result.provider,
                    );
                    self.state = State::Done {
                        source_text,
                        result,
                    };
                    self.shown_at = Instant::now();
                }
                UiEvent::Failed {
                    source_text,
                    errors,
                } => {
                    self.state = State::Failed {
                        source_text,
                        errors,
                    };
                    self.shown_at = Instant::now();
                }
                UiEvent::ManualDone(result) => {
                    let mut main = self.main.lock().unwrap();
                    main.translating = false;
                    main.result = Some(Ok(result));
                }
                UiEvent::ManualFailed(errors) => {
                    let mut main = self.main.lock().unwrap();
                    main.translating = false;
                    main.result = Some(Err(errors));
                }
                UiEvent::ProviderStatus(statuses) => {
                    let mut main = self.main.lock().unwrap();
                    main.testing = false;
                    main.statuses = statuses;
                }
            }
        }
    }

    fn show(&mut self, ctx: &egui::Context) {
        let pos = self.clamped_position(ctx);
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
        if !self.visible {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            self.visible = true;
        }
        self.shown_at = Instant::now();
    }

    fn hide(&mut self, ctx: &egui::Context) {
        if self.visible {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.visible = false;
        }
        self.state = State::Hidden;
        self.settings_open = false;
        monitor::set_paused(false);
    }

    /// Keeps the bubble fully on screen, flipping it above/left of the cursor
    /// when there isn't room below/right.
    fn clamped_position(&self, ctx: &egui::Context) -> egui::Pos2 {
        let mut x = self.anchor.0 as f32 + CURSOR_OFFSET.0;
        let mut y = self.anchor.1 as f32 + CURSOR_OFFSET.1;
        let height = self.last_height.max(MIN_HEIGHT);

        if let Some(monitor) = ctx.input(|i| i.viewport().monitor_size) {
            const MARGIN: f32 = 8.0;
            if x + BUBBLE_WIDTH + MARGIN > monitor.x {
                x = (self.anchor.0 as f32 - BUBBLE_WIDTH - CURSOR_OFFSET.0)
                    .max(MARGIN)
                    .min(monitor.x - BUBBLE_WIDTH - MARGIN);
            }
            if y + height + MARGIN > monitor.y {
                y = (self.anchor.1 as f32 - height - CURSOR_OFFSET.1).max(MARGIN);
            }
            x = x.clamp(MARGIN, (monitor.x - BUBBLE_WIDTH - MARGIN).max(MARGIN));
            y = y.clamp(MARGIN, (monitor.y - height - MARGIN).max(MARGIN));
        }
        egui::pos2(x, y)
    }

    /// Registers (or drops) the main window's viewport for this frame.
    ///
    /// A deferred viewport only survives while the parent keeps asking for it,
    /// so while the window is open the root must keep painting even though the
    /// bubble itself may be hidden.
    fn drive_main_window(&mut self, ctx: &egui::Context) {
        // The windowing backend's app delegate does not exist yet when the app
        // is constructed, so keep trying until the reopen hook lands.
        if !self.reopen_hooked {
            if let Some(mtm) = MainThreadMarker::new() {
                self.reopen_hooked = status_item::hook_reopen(mtm);
            }
        }

        if status_item::take_open_request() {
            let mut main = self.main.lock().unwrap();
            // Only a window that was actually closed needs its size reasserted;
            // resetting it on an already-open window would undo a resize the
            // user made by hand.
            if !main.open {
                main.open = true;
                main.sized = false;
            }
            main.focus_requested = true;
        }

        let (open, wants_focus) = {
            let main = self.main.lock().unwrap();
            (main.open, main.focus_requested)
        };

        // Dock presence follows the window: regular while it is open so the app
        // can actually be brought to the front, accessory once it closes so the
        // bubble goes back to never stealing focus.
        if open != self.dock_visible {
            if let Some(mtm) = MainThreadMarker::new() {
                status_item::show_in_dock(mtm, open);
            }
            self.dock_visible = open;
        }

        if !open {
            return;
        }

        let id = egui::ViewportId::from_hash_of("bubbleTranslate-main");

        // Raising happens in two halves, in this order: the process is pulled
        // forward (only possible now that the policy is regular), then this
        // particular window is made key. The flag is cleared here rather than
        // in the window's own draw, so the request fires exactly once — macOS
        // ignores activation that is asked for every frame.
        if wants_focus {
            status_item::activate();
            ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Focus);
            self.main.lock().unwrap().focus_requested = false;
        }
        let builder = egui::ViewportBuilder::default()
            .with_title("Bubble Translate")
            .with_inner_size(main_window::WINDOW_SIZE)
            .with_min_inner_size([400.0, 420.0]);

        let state = self.main.clone();
        let config = self.config_for_main.clone();
        ctx.show_viewport_deferred(id, builder, move |ui, _class| {
            // Closing the window must not take the translator down with it;
            // the app keeps running and the menu bar item brings it back.
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                state.lock().unwrap().open = false;
            }
            main_window::draw(ui, &state, &config);
        });

        // Keep the parent painting so the viewport above is re-registered.
        ctx.request_repaint();
    }

    fn target_lang(&self) -> String {
        self.config.lock().unwrap().target_lang.clone()
    }
}

impl eframe::App for BubbleApp {
    /// Transparent so the rounded corners of the bubble don't sit on a grey
    /// rectangle.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    /// Runs even while the bubble is hidden, so this is where the engine's
    /// results are picked up and the window is shown or dismissed.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events(ctx);
        self.drive_main_window(ctx);

        // While the pointer is inside the bubble, gestures belong to us, not
        // to a new selection in the app underneath.
        let hovered = ctx.input(|i| i.pointer.has_pointer()) && self.visible;
        monitor::set_paused(hovered);

        if matches!(self.state, State::Hidden) {
            if self.visible {
                self.hide(ctx);
            }
            // Nothing to draw; sleep until the engine wakes us.
            ctx.request_repaint_after(Duration::from_secs(3600));
            return;
        }

        // Auto-dismiss, paused while the pointer is inside so a bubble being
        // read never vanishes mid-sentence.
        let auto_hide = self.config.lock().unwrap().auto_hide_secs;
        if hovered {
            self.shown_at = Instant::now();
            ctx.request_repaint_after(Duration::from_millis(100));
        } else if auto_hide > 0 && !self.settings_open {
            let elapsed = self.shown_at.elapsed();
            let budget = Duration::from_secs(auto_hide);
            if elapsed >= budget {
                self.hide(ctx);
                return;
            }
            ctx.request_repaint_after(budget - elapsed);
        }

        if matches!(self.state, State::Working { .. }) {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if matches!(self.state, State::Hidden) {
            return;
        }

        let bubble = egui::Frame::new()
            .fill(BUBBLE_BG)
            .stroke(egui::Stroke::new(1.0, BUBBLE_BORDER))
            .corner_radius(10.0)
            .inner_margin(egui::Margin::symmetric(16, 14))
            .shadow(egui::Shadow {
                offset: [0, 4],
                blur: 18,
                spread: 0,
                color: egui::Color32::from_black_alpha(120),
            });

        let mut dismiss = false;
        let response = bubble.show(ui, |ui| {
            ui.set_width(BUBBLE_WIDTH - 32.0);
            dismiss = self.draw_body(ui);
        });

        let ctx = ui.ctx().clone();

        // Size the window to whatever the content needed. One frame behind,
        // which is invisible in practice because the bubble appears in the
        // "Working" state first and grows into the result.
        let wanted = (response.response.rect.height() + 20.0).clamp(MIN_HEIGHT, MAX_HEIGHT);
        if (wanted - self.last_height).abs() > 1.0 {
            self.last_height = wanted;
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                BUBBLE_WIDTH,
                wanted,
            )));
            // Re-anchor: a taller bubble may no longer fit below the cursor.
            let pos = self.clamped_position(&ctx);
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
        }

        if dismiss {
            self.hide(&ctx);
        }
    }
}

impl BubbleApp {
    /// Draws the bubble contents. Returns true when the user asked to close.
    fn draw_body(&mut self, ui: &mut egui::Ui) -> bool {
        let mut dismiss = false;

        // -- header: source text and the close button --------------------
        ui.horizontal_top(|ui| {
            let source = match &self.state {
                State::Working { source_text, .. }
                | State::Done { source_text, .. }
                | State::Failed { source_text, .. } => source_text.as_str(),
                State::Hidden => "",
            };
            ui.vertical(|ui| {
                ui.set_max_width(BUBBLE_WIDTH - 76.0);
                ui.label(
                    egui::RichText::new(truncate(source, 160))
                        .size(12.5)
                        .color(TEXT_SECONDARY),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                if ui
                    .add(egui::Button::new(egui::RichText::new("✕").size(14.0)).frame(false))
                    .on_hover_text("Close")
                    .clicked()
                {
                    dismiss = true;
                }
            });
        });

        ui.add_space(6.0);

        // -- body ---------------------------------------------------------
        match &self.state {
            State::Hidden => {}
            State::Working { via, .. } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        egui::RichText::new(match via {
                            CaptureSource::Accessibility => "Translating…",
                            CaptureSource::Clipboard => "Translating (via copy)…",
                        })
                        .size(13.5)
                        .color(TEXT_SECONDARY),
                    );
                });
            }
            State::Done { result, .. } => {
                let size = self.config.lock().unwrap().font_size;
                egui::ScrollArea::vertical()
                    .max_height(MAX_HEIGHT - 100.0)
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(&result.text)
                                .size(size)
                                .line_height(Some(size * LINE_HEIGHT_RATIO))
                                .color(TEXT_PRIMARY),
                        );
                    });
            }
            State::Failed { errors, .. } => {
                ui.label(
                    egui::RichText::new("No provider could translate this")
                        .size(14.0)
                        .color(TEXT_ERROR),
                );
                ui.add_space(2.0);
                for (provider, err) in errors {
                    ui.label(
                        egui::RichText::new(format!("{}: {err}", provider.label()))
                            .size(12.0)
                            .color(TEXT_MUTED),
                    );
                }
            }
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);
        dismiss |= self.draw_footer(ui);

        if self.settings_open {
            ui.add_space(6.0);
            self.draw_settings(ui);
        }

        dismiss
    }

    fn draw_footer(&mut self, ui: &mut egui::Ui) -> bool {
        let dismiss = false;
        let target = self.target_lang();

        ui.horizontal(|ui| {
            // Provider and detected source language: says which of the three
            // backends actually answered, which matters when the chain fell
            // through to a fallback.
            let caption = match &self.state {
                State::Done { result, .. } => format!(
                    "{} · {} → {}",
                    result.provider.label(),
                    language_name(&result.source_lang),
                    language_name(&target),
                ),
                _ => format!("→ {}", language_name(&target)),
            };
            ui.label(egui::RichText::new(caption).size(11.5).color(TEXT_MUTED));

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(egui::Button::new(egui::RichText::new("⚙").size(14.0)).frame(false))
                    .on_hover_text("Settings")
                    .clicked()
                {
                    self.settings_open = !self.settings_open;
                }

                if let State::Done { result, .. } = &self.state {
                    let just_copied = self
                        .copied_at
                        .is_some_and(|t| t.elapsed() < Duration::from_secs(2));
                    let label = if just_copied { "Copied" } else { "Copy" };
                    if ui
                        .add(egui::Button::new(egui::RichText::new(label).size(12.0)).frame(false))
                        .clicked()
                    {
                        capture::set_clipboard(&result.text);
                        self.copied_at = Some(Instant::now());
                    }
                }

                // Language switching lives in the settings panel rather than a
                // dropdown here: the bubble never becomes the active window, by
                // design, and a popup menu in a window that cannot take focus
                // does not open.
                if ui
                    .add(
                        egui::Button::new(egui::RichText::new(language_name(&target)).size(12.0))
                            .frame(false),
                    )
                    .on_hover_text("Change target language")
                    .clicked()
                {
                    self.settings_open = !self.settings_open;
                }
            });
        });

        dismiss
    }

    fn draw_settings(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        let mut retranslate = false;
        let mut cfg = self.config.lock().unwrap();
        let mut dirty = false;

        if self.permission_missing {
            ui.label(
                egui::RichText::new(
                    "Accessibility permission is off — selections cannot be read. \
                     Enable bubbleTranslate in System Settings › Privacy & Security › \
                     Accessibility, then restart.",
                )
                .size(11.5)
                .color(egui::Color32::from_rgb(245, 195, 130)),
            );
            ui.add_space(4.0);
        }

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Providers, in order").size(12.0));
            ui.label(
                egui::RichText::new(
                    cfg.providers
                        .iter()
                        .map(|p| p.label())
                        .collect::<Vec<_>>()
                        .join(" → "),
                )
                .size(12.0)
                .color(TEXT_SECONDARY),
            );
        });

        if ui
            .checkbox(
                &mut cfg.auto_translate,
                egui::RichText::new("Translate on selection").size(12.0),
            )
            .changed()
        {
            dirty = true;
        }

        ui.label(
            egui::RichText::new("Translate into")
                .size(12.0)
                .color(TEXT_SECONDARY),
        );
        let current = cfg.target_lang.clone();
        let mut chosen: Option<String> = None;
        ui.horizontal_wrapped(|ui| {
            for (code, name) in LANGUAGES {
                if ui.selectable_label(*code == current, *name).clicked() {
                    chosen = Some((*code).to_string());
                }
            }
        });
        if let Some(code) = chosen {
            if code != cfg.target_lang {
                cfg.target_lang = code;
                dirty = true;
                retranslate = true;
            }
        }
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Text size").size(12.0));
            if ui
                .add(
                    egui::Slider::new(&mut cfg.font_size, 12.0..=26.0)
                        .show_value(false)
                        .trailing_fill(true),
                )
                .drag_stopped()
            {
                dirty = true;
            }
            ui.label(
                egui::RichText::new(format!("{:.0}pt", cfg.font_size))
                    .size(11.0)
                    .color(TEXT_MUTED),
            );
        });

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("DeepL key").size(12.0));
            if ui
                .add(
                    egui::TextEdit::singleline(&mut cfg.deepl_api_key)
                        .password(true)
                        .hint_text("optional")
                        .desired_width(180.0),
                )
                .lost_focus()
            {
                dirty = true;
            }
        });

        ui.label(
            egui::RichText::new(format!("Config: {}", Config::path().display()))
                .size(10.5)
                .color(TEXT_MUTED),
        );

        if dirty {
            let _ = cfg.save();
        }
        // The config lock must be released before asking the engine to redo the
        // translation, or the engine thread blocks on it.
        drop(cfg);
        if retranslate {
            self.engine.request(Request::Retranslate);
        }
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    let mut out: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        out.push('…');
    }
    out
}

fn install_fonts(ctx: &egui::Context) {
    let Ok(bytes) = std::fs::read(FALLBACK_FONT) else {
        // Latin-only rendering is degraded but still usable, so this is not
        // worth failing startup over.
        eprintln!("bubbleTranslate: {FALLBACK_FONT} not found; non-Latin text may not render");
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "unicode-fallback".to_owned(),
        Arc::new(egui::FontData::from_owned(bytes)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("unicode-fallback".to_owned());
    }
    ctx.set_fonts(fonts);
}
