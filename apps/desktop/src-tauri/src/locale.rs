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
    /// Tray: not in a group
    pub no_group: &'static str,
    /// Tray: input stays here
    pub idle: &'static str,
    /// Tray: controlling `{name}`
    pub controlling: &'static str,
    /// Tray: `{name}` controls this device
    pub controlled: &'static str,
    /// Tray: crossing is paused
    pub paused: &'static str,
    /// Tray: pause sharing
    pub pause: &'static str,
    /// Tray: resume sharing
    pub resume: &'static str,
    /// Tray: lock the pointer
    pub lock: &'static str,
    /// Tray: heading of the device list
    pub switch_to: &'static str,
    /// Tray: marks this device in the list
    pub this_device: &'static str,
    /// Tray: marks an offline device in the list
    pub offline: &'static str,
    /// Tray: open the window
    pub open: &'static str,
    /// Tray: quit
    pub quit: &'static str,
    /// Dialog title when the engine cannot start
    pub start_failed_title: &'static str,
    /// Dialog hint when the engine cannot start
    pub start_failed_hint: &'static str,
}

impl Texts {
    /// `template` with `{name}` filled in
    pub fn fill(template: &str, name: &str) -> String {
        template.replace("{name}", name)
    }
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
    no_group: "未加入桌面组",
    idle: "键鼠在本机",
    controlling: "正在控制 {name}",
    controlled: "{name} 正在控制本机",
    paused: "已暂停共享",
    pause: "暂停共享",
    resume: "恢复共享",
    lock: "锁定光标",
    switch_to: "切换到",
    this_device: "本机",
    offline: "离线",
    open: "打开 Lanroam…",
    quit: "退出 Lanroam",
    start_failed_title: "Lanroam 无法启动",
    start_failed_hint: "可能已有另一个 Lanroam 在运行（包括命令行版 lanroam-cli run），请先退出它再打开。",
};

/// English
static EN: Texts = Texts {
    no_group: "Not in a desk group",
    idle: "Keyboard and mouse here",
    controlling: "Controlling {name}",
    controlled: "{name} is controlling this device",
    paused: "Sharing paused",
    pause: "Pause Sharing",
    resume: "Resume Sharing",
    lock: "Lock the Pointer",
    switch_to: "Switch To",
    this_device: "this device",
    offline: "offline",
    open: "Open Lanroam…",
    quit: "Quit Lanroam",
    start_failed_title: "Lanroam cannot start",
    start_failed_hint: "Another Lanroam may be running already (the command-line lanroam-cli run counts too); quit it first.",
};
