//! Minimal `PlatformDisplay`. Multi-window and multi-monitor placement are
//! explicitly out of scope for this crate (see docs/winit-backend-design.md
//! "Out of scope") — gpui only uses this for the initial window's centered
//! placement and DPI queries, both of which tolerate an approximate single
//! display. A fixed-size stub (unchanged from the spike) is enough to
//! satisfy the trait; `docs/winit-backend-design.md` records this as a
//! known gap to close before multi-monitor support lands.

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
