//! Watches for the gestures that finish a text selection.
//!
//! A `CGEventTap` on its own run-loop thread sees mouse and key events
//! system-wide. Rather than polling the focused element (expensive, and it
//! fires mid-drag), the tap waits for the moment a selection is *completed*:
//! a mouse-up that ended a drag or a multi-click, or a key-up from a
//! shift-navigation.
//!
//! The gesture filter matters more than it looks. Capture can fall back to
//! synthesizing Cmd+C, so triggering on every mouse-up would fire a copy on
//! every single click anywhere in the OS. Only drags past a few points and
//! double/triple clicks get through.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use core_foundation::runloop::CFRunLoop;
use core_graphics::event::{
    CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventType, CallbackResult, EventField,
};

use crate::capture;

/// Minimum drag distance, in points, before a mouse-up counts as a selection
/// rather than a click.
const DRAG_THRESHOLD: f64 = 6.0;

/// Keys that extend a selection when Shift is held.
const NAVIGATION_KEYS: [i64; 8] = [
    123, // left
    124, // right
    125, // down
    126, // up
    115, // home
    119, // end
    116, // page up
    121, // page down
];
const KEYCODE_A: i64 = 0;

/// Set while the pointer is inside the bubble, so interacting with our own
/// window never kicks off another capture of the app behind it.
static PAUSED: AtomicBool = AtomicBool::new(false);

pub fn set_paused(paused: bool) {
    PAUSED.store(paused, Ordering::Relaxed);
}

#[derive(Debug, Clone, Copy)]
pub struct Trigger {
    /// Where to anchor the bubble: global display points, top-left origin.
    pub at: (f64, f64),
}

/// Starts the tap on a dedicated thread and returns immediately.
///
/// Returns an error only if the tap could not be created, which in practice
/// means Accessibility permission has not been granted yet.
pub fn spawn(on_trigger: impl Fn(Trigger) + Send + 'static) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("selection-monitor".into())
        .spawn(move || run(on_trigger))
        .map(|_| ())
}

fn run(on_trigger: impl Fn(Trigger) + Send + 'static) {
    // Where the current drag started. `None` between gestures.
    let press_origin: Mutex<Option<(f64, f64)>> = Mutex::new(None);

    let callback = move |_proxy: _,
                         event_type: CGEventType,
                         event: &core_graphics::event::CGEvent| {
        // Never react to the Cmd+C we post ourselves, or the gesture would
        // feed itself.
        if capture::is_synthesizing() || capture::is_marked_synthetic(event) {
            return CallbackResult::Keep;
        }
        if PAUSED.load(Ordering::Relaxed) {
            crate::trace!("event ignored: pointer is over the bubble");
            return CallbackResult::Keep;
        }

        match event_type {
            CGEventType::LeftMouseDown => {
                let p = event.location();
                crate::trace!("mouse-down at ({:.0}, {:.0})", p.x, p.y);
                *press_origin.lock().unwrap() = Some((p.x, p.y));
            }
            CGEventType::LeftMouseUp => {
                let p = event.location();
                let origin = press_origin.lock().unwrap().take();
                let dragged = origin
                    .map(|(x, y)| ((p.x - x).powi(2) + (p.y - y).powi(2)).sqrt() > DRAG_THRESHOLD)
                    .unwrap_or(false);
                // Click state 2 is a double-click (word), 3 a triple-click
                // (paragraph); both select without any drag.
                let multi_click =
                    event.get_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE) >= 2;

                crate::trace!(
                    "mouse-up  drag={} multi_click={} -> {}",
                    if dragged { "yes" } else { "no " },
                    multi_click,
                    if dragged || multi_click {
                        "TRIGGER"
                    } else {
                        "ignored"
                    },
                );
                if dragged || multi_click {
                    on_trigger(Trigger { at: (p.x, p.y) });
                }
            }
            CGEventType::KeyUp => {
                let flags = event.get_flags();
                let keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
                let shift_select = flags.contains(CGEventFlags::CGEventFlagShift)
                    && NAVIGATION_KEYS.contains(&keycode);
                let select_all =
                    flags.contains(CGEventFlags::CGEventFlagCommand) && keycode == KEYCODE_A;

                if shift_select || select_all {
                    crate::trace!("key-up    keycode={keycode} -> TRIGGER");
                    let p = event.location();
                    on_trigger(Trigger { at: (p.x, p.y) });
                }
            }
            _ => {}
        }

        CallbackResult::Keep
    };

    crate::trace!("installing event tap...");
    let result = CGEventTap::with_enabled(
        CGEventTapLocation::Session,
        CGEventTapPlacement::HeadInsertEventTap,
        // Listen-only: we never modify or swallow the user's events, which
        // also means a slow callback can never stall their typing.
        CGEventTapOptions::ListenOnly,
        vec![
            CGEventType::LeftMouseDown,
            CGEventType::LeftMouseUp,
            CGEventType::KeyUp,
        ],
        callback,
        || CFRunLoop::run_current(),
    );

    if result.is_err() {
        eprintln!(
            "bubbleTranslate: could not install the event tap — grant Accessibility \
             permission in System Settings and restart."
        );
    }
}
