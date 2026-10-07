//! Single-line text that ends in an ellipsis when its container is narrower than it.
use gpui::Styled;

/// GPUI's `truncate()` measures unwrapped text once, before flex layout has shrunk the
/// element, and reuses that full-width layout; the text is clipped without an ellipsis.
/// Wrapping text is re-measured whenever its width changes, so clamping it to one line
/// truncates at the final width.
pub(crate) trait Ellipsis: Styled + Sized {
    fn ellipsis(self) -> Self {
        self.overflow_hidden()
            .whitespace_normal()
            .text_ellipsis()
            .line_clamp(1)
    }
}
impl<T: Styled> Ellipsis for T {}
