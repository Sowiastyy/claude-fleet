//! What the window keeps between runs: the background and how dark it is.

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// `harbor`, `ridge`, `contours`, `none` or `custom`.
    pub background: String,
    pub custom_image: Option<String>,
    /// Percent of black over the image on the new-chat screen.
    pub new_chat_dim: u8,
    /// Percent of black over the image in a conversation.
    pub chat_dim: u8,
    pub accent: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            background: "harbor".into(),
            custom_image: None,
            new_chat_dim: 65,
            chat_dim: 85,
            accent: "#E2B867".into(),
        }
    }
}

pub fn load(path: &Path) -> Settings {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub fn save(path: &Path, s: &Settings) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(s)?)?;
    Ok(())
}
