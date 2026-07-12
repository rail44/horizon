//! Minimal `PlatformDisplay`. gpui uses this for multi-monitor placement
//! and DPI queries we don't exercise in leg 1 (single window, no explicit
//! monitor picking) — a fixed-size stub is enough to satisfy the trait.

use gpui::{point, px, size, Bounds, DisplayId, Pixels, PlatformDisplay};

pub(crate) struct WinitDisplay {
    id: DisplayId,
}

impl WinitDisplay {
    pub(crate) fn new() -> Self {
        Self {
            id: DisplayId::new(0),
        }
    }
}

impl std::fmt::Debug for WinitDisplay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WinitDisplay")
            .field("id", &self.id)
            .finish()
    }
}

impl PlatformDisplay for WinitDisplay {
    fn id(&self) -> DisplayId {
        self.id
    }

    fn uuid(&self) -> anyhow::Result<uuid::Uuid> {
        Ok(uuid::Uuid::nil())
    }

    fn bounds(&self) -> Bounds<Pixels> {
        Bounds {
            origin: point(px(0.0), px(0.0)),
            size: size(px(1920.0), px(1080.0)),
        }
    }
}
