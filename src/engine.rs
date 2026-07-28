//! The worker that turns "something got selected" into "here is a bubble".
//!
//! Runs on its own thread so neither the network round trip nor the pasteboard
//! polling in [`crate::capture`] can stall the UI or the event tap.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::capture::{self, CaptureSource};
use crate::config::{Config, Provider};
use crate::monitor::Trigger;
use crate::translate::{TranslateError, Translation, Translator};

/// How long an identical selection is treated as a repeat of the one just
/// handled rather than a fresh request.
const REPEAT_WINDOW: Duration = Duration::from_secs(2);

pub enum Request {
    /// The monitor saw a selection gesture finish.
    Selection(Trigger),
    /// The user changed the target language; redo the last selection.
    Retranslate,
    /// Text typed into the main window's translate box. Deliberately separate
    /// from the selection path: it must not disturb the bubble or the
    /// last-selection state the language picker retranslates.
    Manual(String),
    /// Probe every backend independently for the main window's status panel.
    TestProviders,
}

pub enum UiEvent {
    /// A translation is in flight, so the bubble can anchor itself and spin.
    Working {
        at: (f64, f64),
        via: CaptureSource,
    },
    /// The captured text rides along for the main window's Recent list only.
    /// The bubble shows the translation by itself.
    Done {
        source_text: String,
        result: Translation,
    },
    /// Every provider in the chain refused. Carries each one's reason.
    Failed {
        errors: Vec<(Provider, TranslateError)>,
    },
    /// Result of a main-window translate-box request.
    ManualDone(Translation),
    ManualFailed(Vec<(Provider, TranslateError)>),
    /// Per-provider health, in the order they were probed.
    ProviderStatus(Vec<(Provider, Result<String, String>)>),
}

/// Phrase used to probe the backends. Short, unambiguously non-English, and
/// cheap against every provider's quota.
const PROBE_TEXT: &str = "Merhaba dünya";

pub struct Engine {
    tx: Sender<Request>,
}

impl Engine {
    pub fn start(
        config: Arc<Mutex<Config>>,
        ui: Sender<UiEvent>,
        wake_ui: impl Fn() + Send + 'static,
    ) -> Self {
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("translate-engine".into())
            .spawn(move || run(rx, config, ui, wake_ui))
            .expect("failed to spawn translation engine");
        Self { tx }
    }

    pub fn sender(&self) -> Sender<Request> {
        self.tx.clone()
    }

    pub fn request(&self, request: Request) {
        let _ = self.tx.send(request);
    }
}

fn run(rx: Receiver<Request>, config: Arc<Mutex<Config>>, ui: Sender<UiEvent>, wake_ui: impl Fn()) {
    let translator = Translator::new();
    let mut last_text = String::new();
    let mut last_at = (0.0, 0.0);
    let mut last_started = Instant::now() - REPEAT_WINDOW;

    // Holds a request that arrived mid-debounce and superseded the gesture
    // being settled; it is processed on the next turn of the loop.
    let mut pending: Option<Request> = None;

    loop {
        let mut request = match pending.take() {
            Some(request) => request,
            None => match rx.recv() {
                Ok(request) => request,
                Err(_) => return,
            },
        };

        let (cfg, debounce) = {
            let guard = config.lock().unwrap();
            (guard.clone(), Duration::from_millis(guard.debounce_ms))
        };

        // Requests that need no capture are handled here; the rest fall
        // through to the selection pipeline below.
        request = match request {
            Request::Manual(text) => {
                let event = match translator.translate(&text, &cfg) {
                    Ok(result) => UiEvent::ManualDone(result),
                    Err(errors) => UiEvent::ManualFailed(errors),
                };
                let _ = ui.send(event);
                wake_ui();
                continue;
            }
            Request::TestProviders => {
                let statuses = [Provider::Google, Provider::MyMemory, Provider::DeepL]
                    .into_iter()
                    .map(|provider| {
                        let outcome = translator
                            .translate_with(provider, PROBE_TEXT, &cfg)
                            .map(|t| t.text)
                            .map_err(|e| e.to_string());
                        (provider, outcome)
                    })
                    .collect();
                let _ = ui.send(UiEvent::ProviderStatus(statuses));
                wake_ui();
                continue;
            }
            other => other,
        };

        // Anything that arrives mid-debounce supersedes the gesture and is
        // requeued rather than dropped.
        if let Request::Selection(trigger) = request {
            match settle(&rx, trigger, debounce) {
                Settled::Trigger(settled) => request = Request::Selection(settled),
                Settled::Superseded(other) => {
                    pending = Some(other);
                    continue;
                }
            }
        }

        let (text, at, via) = match request {
            // Already handled above; the compiler cannot see that.
            Request::Manual(_) | Request::TestProviders => continue,
            Request::Retranslate => {
                if last_text.is_empty() {
                    continue;
                }
                (last_text.clone(), last_at, CaptureSource::Accessibility)
            }
            Request::Selection(trigger) => {
                let Some(capture) = capture::selected_text(cfg.clipboard_fallback) else {
                    crate::trace!("capture   nothing (AX empty, clipboard produced nothing)");
                    continue;
                };
                let text = capture.text;
                let chars = text.chars().count();
                crate::trace!(
                    "capture   via={:?} chars={chars} text={:?}",
                    capture.via,
                    truncate_for_log(&text),
                );
                if chars < cfg.min_chars || chars > cfg.max_chars {
                    crate::trace!(
                        "skip      {chars} chars outside [{}, {}]",
                        cfg.min_chars,
                        cfg.max_chars,
                    );
                    continue;
                }
                // Guards against the same selection being read twice in quick
                // succession (a click that lands inside an existing selection
                // re-reads it). Selecting the same words again later is a
                // deliberate act and does go through.
                if text == last_text && last_started.elapsed() < REPEAT_WINDOW {
                    crate::trace!("skip      same text within repeat window");
                    continue;
                }
                (text, trigger.at, capture.via)
            }
        };

        last_started = Instant::now();

        last_text = text.clone();
        last_at = at;

        let _ = ui.send(UiEvent::Working { at, via });
        wake_ui();

        let event = match translator.translate(&text, &cfg) {
            Ok(result) => {
                crate::trace!(
                    "translate [{}] {} -> {:?}",
                    result.provider.label(),
                    result.source_lang,
                    truncate_for_log(&result.text),
                );
                UiEvent::Done {
                    source_text: text,
                    result,
                }
            }
            Err(errors) => {
                for (provider, err) in &errors {
                    crate::trace!("translate {} FAILED: {err}", provider.label());
                }
                UiEvent::Failed { errors }
            }
        };
        let _ = ui.send(event);
        wake_ui();
    }
}

fn truncate_for_log(text: &str) -> String {
    text.chars().take(48).collect()
}

enum Settled {
    /// The debounce window closed; this was the last gesture in it.
    Trigger(Trigger),
    /// A non-gesture request arrived mid-window and takes priority.
    Superseded(Request),
}

/// Waits out the debounce window, returning the most recent trigger seen.
///
/// Two things happen here: the source app gets a moment to finish updating its
/// selection, and the burst of triggers a fast double- or triple-click emits
/// collapses into a single capture.
fn settle(rx: &Receiver<Request>, mut latest: Trigger, window: Duration) -> Settled {
    let deadline = Instant::now() + window;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Settled::Trigger(latest);
        }
        match rx.recv_timeout(remaining) {
            Ok(Request::Selection(next)) => latest = next,
            Ok(other) => return Settled::Superseded(other),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                return Settled::Trigger(latest);
            }
        }
    }
}
