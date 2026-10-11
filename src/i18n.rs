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
}
