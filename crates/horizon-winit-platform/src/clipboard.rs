//! Text-only clipboard, backed by `arboard` (evaluated against the task
//! brief's suggestion; chosen for Wayland-data-control + X11 support in one
//! crate — see docs/winit-backend-design.md). winit itself has no
//! clipboard API. Kept behind this crate's own thin wrapper (rather than
//! calling `arboard` directly from `platform.rs`) so the backend choice can
//! change without touching `Platform`'s clipboard methods.
//!
//! Text-only: gpui's `ClipboardItem` also supports images and file lists,
//! but Horizon's own clipboard usage (`src/terminal/mod.rs`'s copy/paste)
//! is text-only, and `arboard`'s image support pulls in a sizeable
//! `image`/`objc2-*` dependency tree for no payoff here (see the
//! `image-data` feature left off in Cargo.toml).

use arboard::{Clipboard, GetExtLinux, LinuxClipboardKind, SetExtLinux};
use gpui::ClipboardItem;
use std::cell::RefCell;

pub(crate) struct WinitClipboard {
    // Lazily opened: `Clipboard::new()` talks to the display server, which
    // isn't guaranteed to be ready the instant `WinitPlatform::new()` runs
    // (before any window/connection exists).
    inner: RefCell<Option<Clipboard>>,
}

impl WinitClipboard {
    pub(crate) fn new() -> Self {
        Self {
            inner: RefCell::new(None),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut Clipboard) -> R) -> Option<R> {
        let mut guard = self.inner.borrow_mut();
        if guard.is_none() {
            match Clipboard::new() {
                Ok(clipboard) => *guard = Some(clipboard),
                Err(error) => {
                    log::warn!("horizon-winit-platform: failed to open clipboard: {error}");
                    return None;
                }
            }
        }
        guard.as_mut().map(f)
    }

    pub(crate) fn read(&self) -> Option<ClipboardItem> {
        let text = self.with(|clipboard| clipboard.get_text().ok()).flatten()?;
        (!text.is_empty()).then(|| ClipboardItem::new_string(text))
    }

    pub(crate) fn write(&self, item: ClipboardItem) {
        let Some(text) = item.text() else { return };
        self.with(|clipboard| {
            if let Err(error) = clipboard.set_text(text) {
                log::warn!("horizon-winit-platform: failed to write clipboard: {error}");
            }
        });
    }

    pub(crate) fn read_primary(&self) -> Option<ClipboardItem> {
        let text = self
            .with(|clipboard| {
                clipboard
                    .get()
                    .clipboard(LinuxClipboardKind::Primary)
                    .text()
                    .ok()
            })
            .flatten()?;
        (!text.is_empty()).then(|| ClipboardItem::new_string(text))
    }

    pub(crate) fn write_primary(&self, item: ClipboardItem) {
        let Some(text) = item.text() else { return };
        self.with(|clipboard| {
            if let Err(error) = clipboard
                .set()
                .clipboard(LinuxClipboardKind::Primary)
                .text(text)
            {
                log::warn!("horizon-winit-platform: failed to write primary selection: {error}");
            }
        });
    }
}
