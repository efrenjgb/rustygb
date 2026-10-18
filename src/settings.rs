//! Settings remembered between runs, stored in a small text file:
//!
//!   library=/path/to/roms
//!   boot_rom=/path/to/dmg_boot.bin
//!
//! The file and the save folder live in the platform's usual place for app
//! data, for example ~/Library/Application Support/rustygb on macOS.

use std::path::PathBuf;

pub struct Settings {
    /// The folder the ROM picker lists.
    pub library: Option<PathBuf>,
    /// A boot ROM to run before each game. Without one, games start in the
    /// state the boot ROM would leave behind.
    pub boot_rom: Option<PathBuf>,
}

impl Settings {
    pub fn load() -> Self {
        let mut settings = Settings {
            library: None,
            boot_rom: None,
        };
        let Ok(text) = std::fs::read_to_string(settings_file()) else {
            return settings;
        };
        for line in text.lines() {
            if let Some(path) = line.strip_prefix("library=") {
                settings.library = Some(PathBuf::from(path));
            } else if let Some(path) = line.strip_prefix("boot_rom=") {
                settings.boot_rom = Some(PathBuf::from(path));
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
        if let Some(boot_rom) = &self.boot_rom {
            text += &format!("boot_rom={}\n", boot_rom.display());
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
