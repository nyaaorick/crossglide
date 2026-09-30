//! The Mac's edge hint: while the PC has control, a thin frosted-glass strip covers the edge the
//! pointer left through. It's one borderless, click-through window that lives in the tray app's
//! event loop, since only that thread may own a window. Other systems show nothing.

use crossglide_agent::touch::Hint;
use tao::event_loop::EventLoopWindowTarget;

/// How wide the strip is, in points.
#[cfg(target_os = "macos")]
const THICKNESS: f64 = 28.0;

#[derive(Default)]
pub struct HintWindow {
    #[cfg(target_os = "macos")]
    window: Option<tao::window::Window>,
}

impl HintWindow {
    /// Covers the edge in `hint`, or takes the strip away for `None`.
    pub fn set<T>(&mut self, target: &EventLoopWindowTarget<T>, hint: Option<Hint>) {
        #[cfg(target_os = "macos")]
        self.set_on_mac(target, hint);
        #[cfg(not(target_os = "macos"))]
        let _ = (target, hint);
    }
}

#[cfg(target_os = "macos")]
impl HintWindow {
    fn set_on_mac<T>(&mut self, target: &EventLoopWindowTarget<T>, hint: Option<Hint>) {
        use tao::dpi::{LogicalPosition, LogicalSize};

        let Some(hint) = hint else {
            if let Some(window) = &self.window {
                window.set_visible(false);
            }
            return;
        };
        if self.window.is_none() {
            match make(target) {
                Ok(window) => self.window = Some(window),
                Err(e) => return tracing::warn!("touch: can't show the edge hint: {e}"),
            }
        }
        let Some(window) = &self.window else { return };
        let strip = crossglide_touch::edge::strip_rect(hint.display, hint.edge, THICKNESS);
        window.set_outer_position(LogicalPosition::new(strip.x, strip.y));
        window.set_inner_size(LogicalSize::new(strip.w, strip.h));
        window.set_visible(true);
    }
}

#[cfg(target_os = "macos")]
fn make<T>(target: &EventLoopWindowTarget<T>) -> Result<tao::window::Window, String> {
    use tao::window::WindowBuilder;
    use window_vibrancy::NSVisualEffectMaterial;

    let window = WindowBuilder::new()
        .with_decorations(false)
        .with_transparent(true)
        .with_resizable(false)
        .with_always_on_top(true)
        .with_focusable(false)
        .with_visible(false)
        .with_visible_on_all_workspaces(true)
        .build(target)
        .map_err(|e| e.to_string())?;
    // Clicks and the pointer go through to whatever is underneath.
    window
        .set_ignore_cursor_events(true)
        .map_err(|e| e.to_string())?;
    window_vibrancy::apply_vibrancy(&window, NSVisualEffectMaterial::Sidebar, None, None)
        .map_err(|e| e.to_string())?;
    Ok(window)
}
