use client_api::api::settings::PublicSettings;
use dioxus::prelude::*;

#[derive(Clone, Default)]
pub struct PublicSettingsState {
    pub settings: Option<PublicSettings>,
    pub loaded: bool,
}

#[derive(Clone, Copy)]
pub struct PublicSettingsStore {
    pub state: Signal<PublicSettingsState>,
}

impl PublicSettingsStore {
    pub fn new(state: Signal<PublicSettingsState>) -> Self {
        Self { state }
    }

    pub fn loaded(&self) -> bool {
        self.state.read().loaded
    }

    pub fn distribution_enabled(&self) -> Option<bool> {
        self.state
            .read()
            .settings
            .as_ref()
            .map(|settings| settings.distribution_enabled)
    }

    pub fn set(&mut self, settings: PublicSettings) {
        *self.state.write() = PublicSettingsState {
            settings: Some(settings),
            loaded: true,
        };
    }

    pub fn mark_loaded(&mut self) {
        self.state.write().loaded = true;
    }

    pub fn set_distribution_enabled(&mut self, enabled: bool) {
        let mut state = self.state.write();
        state.loaded = true;
        let settings = state.settings.get_or_insert_with(PublicSettings::default);
        settings.distribution_enabled = enabled;
    }
}
