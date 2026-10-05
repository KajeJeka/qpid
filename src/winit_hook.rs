//! The only module allowed to name slint's `unstable-winit-030` API
//! (Phase 4 spec decisions 2.2 and gate 4). Observes minimize/occlusion
//! and forwards visibility transitions to the sink ui.rs registers.
//! Observation only: every winit event propagates to Slint unchanged.

use std::cell::RefCell;

use slint::winit_030::{winit, CustomApplicationHandler, EventResult};
use winit::event_loop::ActiveEventLoop;
use winit::event::WindowEvent;
use winit::window::WindowId;

thread_local! {
    static SINK: RefCell<Option<Box<dyn FnMut(bool)>>> = RefCell::new(None);
}

thread_local! {
    static DROP_SINK: RefCell<Option<Box<dyn FnMut(std::path::PathBuf)>>> = RefCell::new(None);
}

/// Registers the visibility sink. Main/event-loop thread only (both this
/// hook and `window.run()` live there; `slint::Timer` is not Send).
/// Called once from `ui::wire` before `window.run()`.
pub fn set_sink(sink: impl FnMut(bool) + 'static) {
    SINK.with(|s| *s.borrow_mut() = Some(Box::new(sink)));
}

/// Registers the dropped-file sink. Main/event-loop thread only (same
/// constraints as set_sink). Called once from `ui::wire`.
pub fn set_drop_sink(sink: impl FnMut(std::path::PathBuf) + 'static) {
    DROP_SINK.with(|s| *s.borrow_mut() = Some(Box::new(sink)));
}

fn notify(visible: bool) {
    SINK.with(|s| {
        if let Some(f) = s.borrow_mut().as_mut() {
            f(visible);
        }
    });
}

/// Installs the hook through the backend selector. Must run before the
/// first Slint window exists. The `SLINT_BACKEND` env var (set in `main`)
/// supplies backend/renderer names; this call exists to attach the handler.
pub fn install() {
    slint::BackendSelector::new()
        .with_winit_custom_application_handler(Hook::new())
        .select()
        .expect("backend selection failed");
}

pub struct Hook {
    occluded: bool,
    last_visible: Option<bool>,
}

impl Hook {
    fn new() -> Self {
        Self { occluded: false, last_visible: None }
    }

    /// Spec 4.2: visible = !minimized && !occluded && size != (0,0).
    /// Emitted on every change, including the first evaluation: the
    /// startup seed (visible=true) is emitted too, because `last_visible`
    /// starts as None (probe: 5 `[vis]` lines per run, not 4).
    fn evaluate(&mut self, window: &winit::window::Window) {
        let size = window.inner_size();
        let minimized = window.is_minimized().unwrap_or(false);
        let visible = !minimized && !self.occluded && size.width > 0 && size.height > 0;
        let changed = self.last_visible != Some(visible);
        self.last_visible = Some(visible);
        if changed {
            if cfg!(debug_assertions) {
                eprintln!(
                    "[vis] min={minimized} occ={} size={}x{} -> visible={visible}",
                    self.occluded, size.width, size.height
                );
            }
            notify(visible);
        }
    }
}

impl CustomApplicationHandler for Hook {
    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        winit_window: Option<&winit::window::Window>,
        _slint_window: Option<&slint::Window>,
        event: &WindowEvent,
    ) -> EventResult {
        if let WindowEvent::Occluded(o) = event {
            self.occluded = *o;
            if cfg!(debug_assertions) {
                eprintln!("[vis] occluded-event {o}");
            }
        }
        if let WindowEvent::DroppedFile(path) = event {
            DROP_SINK.with(|s| {
                if let Some(f) = s.borrow_mut().as_mut() {
                    f(path.clone());
                }
            });
        }
        if let Some(window) = winit_window {
            self.evaluate(window);
        }
        // Never swallow anything from Slint (spec 4.2).
        EventResult::Propagate
    }
}
