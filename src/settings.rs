//! Settings remembered between runs, stored in a small text file:
//!
//!   library=/path/to/roms
//!
//! The file and the save folder live in the platform's usual place for app
//! data, for example ~/Library/Application Support/rustygb on macOS.

use std::path::PathBuf;

pub struct Settings {
    /// The folder the ROM picker lists.
    pub library: Option<PathBuf>,
}

impl Settings {
    pub fn load() -> Self {
        let mut settings = Settings { library: None };
        let Ok(text) = std::fs::read_to_string(settings_file()) else {
            return settings;
        };
        for line in text.lines() {
            if let Some(path) = line.strip_prefix("library=") {
                settings.library = Some(PathBuf::from(path));
            }
        }
        settings
    }

    pub fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(app_dir())?;
        let mut text = String::new();
        if let Some(library) = &self.library {
            text += &format!("library={}\n", library.display());
        }
        std::fs::write(settings_file(), text)
    }
}

/// Where battery saves go for games started from the library.
pub fn saves_dir() -> PathBuf {
    app_dir().join("saves")
}

fn settings_file() -> PathBuf {
    app_dir().join("settings.txt")
}

fn app_dir() -> PathBuf {
    let home = || PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/rustygb")
    } else if cfg!(target_os = "windows") {
        PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("rustygb")
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".config"))
            .join("rustygb")
    }
}
