use dioxus::prelude::*;

/// 首页认证弹窗的显式入口意图。
///
/// `/auth/login` 与 `/auth/register` 是可分享的稳定地址；路由组件在回到首页前
/// 把意图写入这里，避免用户只看到首页却不知道下一步该做什么。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthEntry {
    Login,
    Register,
}

#[derive(Clone, Copy)]
pub struct AuthEntryStore {
    pending: Signal<Option<AuthEntry>>,
}

impl AuthEntryStore {
    pub fn new(pending: Signal<Option<AuthEntry>>) -> Self {
        Self { pending }
    }

    pub fn request(&mut self, entry: AuthEntry) {
        self.pending.set(Some(entry));
    }

    pub fn take(&mut self) -> Option<AuthEntry> {
        let value = (self.pending)();
        if value.is_some() {
            self.pending.set(None);
        }
        value
    }
}
