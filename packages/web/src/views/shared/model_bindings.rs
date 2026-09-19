//! Legacy UI address redirect; the old model-binding API is not retained.
use dioxus::prelude::*;
#[component]
pub fn ModelBindings() -> Element {
    let navigator = use_navigator();
    use_effect(move || {
        navigator.replace(crate::router::Route::UpstreamPassthrough {});
    });
    rsx! {}
}
