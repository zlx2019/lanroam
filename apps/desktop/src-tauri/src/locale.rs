//! The shell's own words (tray menu, dialogs) in Chinese and English.
//!
//! The window's text lives in the frontend (`src/i18n`). The language is the
//! one chosen in the settings, or the system's.

/// Languages of the interface
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    /// Chinese
    Zh,
    /// English
    En,
}

impl Lang {
    /// The language for the setting `value` (`system`, `zh`, `en`)
    pub fn from_setting(value: &str) -> Self {
        match value {
            "zh" => Self::Zh,
            "en" => Self::En,
            _ => Self::system(),
        }
    }

    /// The system's language: Chinese for any `zh` locale, else English
    fn system() -> Self {
        match sys_locale::get_locale() {
            Some(locale) if locale.to_lowercase().starts_with("zh") => Self::Zh,
            _ => Self::En,
        }
    }
}

/// The shell's words in one language
pub struct Texts {
    /// Tray: sharing works (in a group with another member online)
    pub active: &'static str,
    /// Tray: crossing is paused
    pub paused: &'static str,
    /// Tray: nothing to share with (no group, or no member online)
    pub inactive: &'static str,
    /// Tray: pause crossing
    pub pause: &'static str,
    /// Tray: resume crossing
    pub resume: &'static str,
    /// Tray: lock the pointer
    pub lock: &'static str,
    /// Tray: heading of the device list
    pub switch_to: &'static str,
    /// Tray: marks an offline device in the list
    pub offline: &'static str,
    /// Tray: open the window on its settings page
    pub settings: &'static str,
    /// Tray: quit
    pub quit: &'static str,
    /// Dialog title when the engine cannot start
    pub start_failed_title: &'static str,
    /// Dialog hint when the engine cannot start
    pub start_failed_hint: &'static str,
}

/// The words for `lang`
pub fn texts(lang: Lang) -> &'static Texts {
    match lang {
        Lang::Zh => &ZH,
        Lang::En => &EN,
    }
}

/// Chinese
static ZH: Texts = Texts {
    active: "活跃",
    paused: "已暂停",
    inactive: "未活跃",
    pause: "暂停",
    resume: "恢复",
    lock: "锁定",
    switch_to: "切换到",
    offline: "离线",
    settings: "设置…",
    quit: "退出",
    start_failed_title: "Lanroam 无法启动",
    start_failed_hint: "可能已有另一个 Lanroam 在运行（包括命令行版 lanroam-cli run），请先退出它再打开。",
};

/// English
static EN: Texts = Texts {
    active: "Active",
    paused: "Paused",
    inactive: "Inactive",
    pause: "Pause",
    resume: "Resume",
    lock: "Lock",
    switch_to: "Switch To",
    offline: "offline",
    settings: "Settings…",
    quit: "Quit",
    start_failed_title: "Lanroam cannot start",
    start_failed_hint: "Another Lanroam may be running already (the command-line lanroam-cli run counts too); quit it first.",
};
