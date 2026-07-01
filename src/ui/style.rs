use floem::style::Style;

/// Cross-domain extensions for Floem's [`Style`] builder.
pub trait StyleExt {
    /// Show the view when `visible` is true, otherwise hide it.
    ///
    /// Replaces the `if !visible { return s.hide(); }` guard that views
    /// otherwise repeat at the top of their style closures, so visibility can
    /// be expressed as one more step in the normal style chain.
    fn shown(self, visible: bool) -> Self;
}

impl StyleExt for Style {
    fn shown(self, visible: bool) -> Self {
        if visible {
            self
        } else {
            self.hide()
        }
    }
}
