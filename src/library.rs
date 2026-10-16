//! Finds the games in the ROM library folder.

use rustygb::cartridge::Cartridge;
use std::io::Read;
use std::path::{Path, PathBuf};

pub struct Game {
    pub path: PathBuf,
    /// The file name without its extension, shown in the picker.
    pub name: String,
    /// Why the game won't run, if it won't (for example "GBC only").
    pub problem: Option<String>,
}

/// Lists the .gb and .gbc files directly inside `folder`, sorted by name.
pub fn scan(folder: &Path) -> std::io::Result<Vec<Game>> {
    let mut games = Vec::new();
    for entry in std::fs::read_dir(folder)? {
        let path = entry?.path();
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase());
        if !matches!(extension.as_deref(), Some("gb" | "gbc")) {
            continue;
        }
        let name = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let problem = check_header(&path);
        games.push(Game {
            path,
            name,
            problem,
        });
    }
    games.sort_by_key(|game| game.name.to_lowercase());
    Ok(games)
}

/// Reads the cartridge header to spot games the emulator can't run yet.
fn check_header(path: &Path) -> Option<String> {
    let mut header = vec![0; 0x150];
    let read = std::fs::File::open(path).and_then(|mut file| file.read_exact(&mut header));
    if read.is_err() {
        return Some("unreadable".to_string());
    }
    // 0xC0 in the CGB flag means the game only runs on a Game Boy Color.
    if header[0x143] == 0xC0 {
        return Some("GBC only".to_string());
    }
    // The header alone is enough for the cartridge to check its controller type.
    match Cartridge::new(header) {
        Ok(_) => None,
        Err(_) => Some("unsupported".to_string()),
    }
}
