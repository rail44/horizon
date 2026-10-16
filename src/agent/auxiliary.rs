//! Title calls capture the `small_model` connection last accepted by Reload
//! Config. No valid `small_model` means no connection, and tabs keep their
//! raw first-message titles.

use std::sync::Arc;

use gpui::{App, Global};
use horizon_agent::auxiliary::{AuxiliaryClient, AuxiliaryConfig};

struct TitleProvider(Option<Arc<AuxiliaryClient>>);
impl Global for TitleProvider {}

pub(crate) fn reload(raw: &horizon_config::RawConfig, cx: &mut App) {
    let client = raw.resolved_small_model().ok().map(|small| {
        Arc::new(AuxiliaryClient::new(AuxiliaryConfig::from_env(
            small.provider.base_url,
            small.provider.api_key_env,
            small.model,
        )))
    });
    cx.set_global(TitleProvider(client));
}

pub(super) fn title_client(cx: &App) -> Option<Arc<AuxiliaryClient>> {
    cx.try_global::<TitleProvider>()
        .and_then(|provider| provider.0.clone())
}
