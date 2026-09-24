//! Title calls capture the auxiliary connection last accepted by Reload Config.

use std::sync::Arc;

use gpui::{App, Global};
use horizon_agent::auxiliary::{AuxiliaryClient, AuxiliaryConfig};

struct TitleProvider(Option<Arc<AuxiliaryClient>>);
impl Global for TitleProvider {}

pub(crate) fn reload(raw: &horizon_config::RawConfig, cx: &mut App) {
    let client = raw.resolved_auxiliary_provider().ok().map(|entry| {
        Arc::new(AuxiliaryClient::new(AuxiliaryConfig::from_env(
            entry.base_url,
            entry.api_key_env,
        )))
    });
    cx.set_global(TitleProvider(client));
}

pub(super) fn title_client(cx: &App) -> Option<Arc<AuxiliaryClient>> {
    cx.try_global::<TitleProvider>()
        .and_then(|provider| provider.0.clone())
}
