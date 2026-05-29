mod en;
mod zh;

pub use en::EN;
pub use zh::ZH;

/// 语言枚举
#[derive(Clone, Copy, PartialEq, Default)]
#[allow(dead_code)]
pub enum Lang {
    #[default]
    Zh,
    En,
}

impl Lang {
    #[allow(dead_code)]
    pub fn from_str(s: &str) -> Self {
        match s {
            "en" => Self::En,
            _ => Self::Zh,
        }
    }

    #[allow(dead_code)]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Zh => "zh",
            Self::En => "en",
        }
    }
}

/// 国际化结构体，通过 `.t(key)` 获取翻译文本
#[derive(Clone, Copy)]
pub struct I18n {
    lang: Lang,
}

impl I18n {
    pub fn new(lang: Lang) -> Self {
        Self { lang }
    }

    /// 获取翻译文本，未找到 key 时返回 key 本身
    pub fn t(&self, key: &str) -> &'static str {
        let map = match self.lang {
            Lang::Zh => &ZH,
            Lang::En => &EN,
        };
        map.get(key).copied().unwrap_or("?")
    }

    /// 获取带参数的翻译文本。
    /// 在翻译值中使用 `{key}` 作为占位符，例如：
    ///   "hello_user": "Hello, {name}!"
    /// 调用：`i18n.t_with_args("hello_user", &[("name", "Alice")]`
    pub fn t_with_args(&self, key: &str, args: &[(&str, &str)]) -> String {
        let mut s = self.t(key).to_string();
        for (k, v) in args {
            s = s.replace(&format!("{{{}}}", k), v);
        }
        s
    }
}
