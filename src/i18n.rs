use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Lang {
    Pt,
    En,
}

impl Lang {
    /// Pick `pt` or `en` text for the active language.
    pub fn t(self, pt: &'static str, en: &'static str) -> &'static str {
        match self {
            Lang::Pt => pt,
            Lang::En => en,
        }
    }

    /// Follow the macOS preferred language (Portuguese → Pt, anything else → En).
    pub fn detect() -> Lang {
        let out = std::process::Command::new("defaults").args(["read", "-g", "AppleLanguages"]).output();
        let text = out.map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase()).unwrap_or_default();
        match text.split('"').nth(1) {
            Some(first) if first.starts_with("pt") => Lang::Pt,
            Some(_) => Lang::En,
            None => Lang::Pt,
        }
    }
}
