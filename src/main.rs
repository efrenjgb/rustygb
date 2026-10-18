mod audio_output;
mod gamepad;
mod library;
mod picker;
mod settings;

use audio_output::AudioOutput;
use gamepad::Gamepads;
use minifb::{Key, Window, WindowOptions};
use picker::{Choice, Picker};
use rustygb::GameBoy;
use rustygb::joypad::Button;
use rustygb::ppu::{SCREEN_HEIGHT, SCREEN_WIDTH};
use settings::Settings;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The classic green DMG shades, from lightest to darkest.
const PALETTE: [u32; 4] = [0xE0F8D0, 0x88C070, 0x346856, 0x081820];

/// The Game Boy screen is shown 4 times bigger.
const SCALE: usize = 4;
const WINDOW_WIDTH: usize = SCREEN_WIDTH * SCALE;
const WINDOW_HEIGHT: usize = SCREEN_HEIGHT * SCALE;

const KEY_MAP: [(Key, Button); 8] = [
    (Key::Right, Button::Right),
    (Key::Left, Button::Left),
    (Key::Up, Button::Up),
    (Key::Down, Button::Down),
    (Key::Z, Button::A),
    (Key::X, Button::B),
    (Key::Backspace, Button::Select),
    (Key::Enter, Button::Start),
];

/// Battery saves are also written every 10 seconds, so a crash doesn't lose much progress.
const AUTOSAVE_FRAMES: u32 = 600;

/// How much audio to keep queued ahead of the sound card, in video frames
/// (about 50ms). Enough that it never runs dry, small enough not to lag.
const AUDIO_LATENCY_FRAMES: usize = 3;

const USAGE: &str = "usage:
  rustygb <rom.gb>            play a ROM
  rustygb --library <folder>  set the ROM library folder and open the picker
  rustygb                     open the picker for the saved library folder

  --boot-rom <file>           run this boot ROM before every game (remembered)
  --boot-rom off              stop using a boot ROM";

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut settings = Settings::load();

    if let Some(index) = args.iter().position(|arg| arg == "--boot-rom") {
        let Some(value) = args.get(index + 1).cloned() else {
            eprintln!("{USAGE}");
            std::process::exit(1);
        };
        args.drain(index..index + 2);
        set_boot_rom(&mut settings, &value);
    }

    let library = match args.as_slice() {
        [flag, folder] if flag == "--library" => {
            let folder = PathBuf::from(folder);
            if !folder.is_dir() {
                eprintln!("{} is not a folder", folder.display());
                std::process::exit(1);
            }
            let folder = folder.canonicalize().unwrap_or(folder);
            settings.library = Some(folder.clone());
            match settings.save() {
                Ok(()) => println!("library set to {}", folder.display()),
                Err(error) => eprintln!("could not save settings: {error}"),
            }
            Some(folder)
        }
        [rom] if !rom.starts_with("--") => None,
        [] if settings.library.is_some() => settings.library.clone(),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(1);
        }
    };

    let mut frontend = Frontend::new(load_boot_rom(&settings));
    match library {
        Some(folder) => run_library(&mut frontend, &folder),
        None => {
            // A ROM given directly keeps its save next to it.
            let rom_path = PathBuf::from(&args[0]);
            frontend.play(&rom_path, &rom_path.with_extension("sav"), None);
        }
    }
}

fn set_boot_rom(settings: &mut Settings, value: &str) {
    if value == "off" {
        settings.boot_rom = None;
    } else {
        let path = PathBuf::from(value);
        if !path.is_file() {
            eprintln!("{} is not a file", path.display());
            std::process::exit(1);
        }
        settings.boot_rom = Some(path.canonicalize().unwrap_or(path));
    }
    match settings.save() {
        Ok(()) => match &settings.boot_rom {
            Some(path) => println!("boot ROM set to {}", path.display()),
            None => println!("boot ROM turned off"),
        },
        Err(error) => eprintln!("could not save settings: {error}"),
    }
}

/// Reads the boot ROM from the settings, if there is one. A missing or
/// broken boot ROM isn't fatal: games just start without it.
fn load_boot_rom(settings: &Settings) -> Option<Vec<u8>> {
    let path = settings.boot_rom.as_ref()?;
    match std::fs::read(path) {
        Ok(data) if data.len() == 0x100 => Some(data),
        Ok(data) => {
            eprintln!("ignoring boot ROM {}: it's {} bytes, not 256", path.display(), data.len());
            None
        }
        Err(error) => {
            eprintln!("ignoring boot ROM {}: {error}", path.display());
            None
        }
    }
}

/// Shows the picker, plays the chosen game, and comes back to the picker
/// when the game is left with Esc (or the controller's Home button).
fn run_library(frontend: &mut Frontend, folder: &Path) {
    let games = match library::scan(folder) {
        Ok(games) => games,
        Err(error) => {
            eprintln!("could not read {}: {error}", folder.display());
            std::process::exit(1);
        }
    };

    let mut picker = Picker::new();
    loop {
        let choice = picker.run(
            &mut frontend.window,
            &mut frontend.gamepads,
            &games,
            folder,
            &mut frontend.pixels,
        );
        let Choice::Play(index) = choice else {
            return;
        };

        // Library games keep their saves in rustygb's own folder, never in
        // the ROM folder. A save already next to the ROM is copied in once.
        let game = &games[index];
        let save_path = settings::saves_dir().join(format!("{}.sav", game.name));
        let existing_save = game.path.with_extension("sav");
        if frontend.play(&game.path, &save_path, Some(&existing_save)) == GameExit::WindowClosed {
            return;
        }
    }
}

#[derive(PartialEq, Eq)]
enum GameExit {
    /// Esc or the controller's Home button was pressed.
    Left,
    WindowClosed,
}

/// The window, sound and controllers, shared by the picker and the games.
struct Frontend {
    window: Window,
    audio: Option<AudioOutput>,
    gamepads: Option<Gamepads>,
    pixels: Vec<u32>,
    boot_rom: Option<Vec<u8>>,
}

impl Frontend {
    fn new(boot_rom: Option<Vec<u8>>) -> Self {
        let window = Window::new("rustygb", WINDOW_WIDTH, WINDOW_HEIGHT, WindowOptions::default())
            .expect("could not open window");

        let audio = AudioOutput::new()
            .inspect_err(|error| eprintln!("running without sound: {error}"))
            .ok();
        let gamepads = Gamepads::new()
            .inspect_err(|error| eprintln!("controllers unavailable: {error}"))
            .ok();

        Frontend {
            window,
            audio,
            gamepads,
            pixels: vec![0; WINDOW_WIDTH * WINDOW_HEIGHT],
            boot_rom,
        }
    }

    /// Runs a game until it's left or the window is closed.
    /// `import_save` is a save to start from if `save_path` doesn't exist yet.
    fn play(&mut self, rom_path: &Path, save_path: &Path, import_save: Option<&Path>) -> GameExit {
        let mut gameboy = match std::fs::read(rom_path)
            .map_err(|error| error.to_string())
            .and_then(|rom| match &self.boot_rom {
                Some(boot_rom) => GameBoy::with_boot_rom(rom, boot_rom.clone()),
                None => GameBoy::new(rom),
            }) {
            Ok(gameboy) => gameboy,
            Err(error) => {
                eprintln!("could not load {}: {error}", rom_path.display());
                return GameExit::Left;
            }
        };

        let has_battery = gameboy.bus.cartridge.has_battery;
        if has_battery {
            load_save(&mut gameboy, save_path, import_save);
        }

        let title = format!("rustygb - {}", gameboy.bus.cartridge.title());
        self.window.set_title(&title);

        // With sound, the sound card's clock decides when to run the next frame.
        // Without it, minifb's frame limiter keeps us at 60 fps.
        match &self.audio {
            Some(audio) => {
                gameboy.enable_audio(audio.sample_rate);
                self.window.set_target_fps(0);
            }
            None => self.window.set_target_fps(60),
        }

        let mut frame_count: u32 = 0;
        let exit = loop {
            if !self.window.is_open() {
                break GameExit::WindowClosed;
            }
            if let Some(gamepads) = &mut self.gamepads {
                gamepads.update();
            }
            let menu_pressed = self.gamepads.as_ref().is_some_and(|pads| pads.menu_pressed());
            if self.window.is_key_down(Key::Escape) || menu_pressed {
                break GameExit::Left;
            }

            // A button is held if it's held on the keyboard or on any controller.
            for (key, button) in KEY_MAP {
                let on_controller = self.gamepads.as_ref().is_some_and(|pads| pads.is_pressed(button));
                gameboy.bus.set_button(button, self.window.is_key_down(key) || on_controller);
            }

            gameboy.run_frame();
            if let Some(audio) = &self.audio {
                audio.push(&gameboy.take_audio_samples());
            }

            draw_screen(&mut self.pixels, gameboy.screen());
            self.window
                .update_with_buffer(&self.pixels, WINDOW_WIDTH, WINDOW_HEIGHT)
                .expect("could not draw frame");

            if let Some(audio) = &self.audio {
                let target = audio.sample_rate as usize / 60 * AUDIO_LATENCY_FRAMES;
                while audio.queued_frames() > target {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }

            frame_count += 1;
            if has_battery && frame_count.is_multiple_of(AUTOSAVE_FRAMES) {
                write_save(&gameboy, save_path);
            }
        };

        if has_battery {
            write_save(&gameboy, save_path);
        }
        if let Some(audio) = &self.audio {
            audio.clear();
        }
        exit
    }
}

/// Scales the 160x144 Game Boy screen up to the window, keeping pixels sharp.
fn draw_screen(pixels: &mut [u32], screen: &[u8]) {
    for (y, row) in pixels.chunks_mut(WINDOW_WIDTH).enumerate() {
        let source_row = &screen[(y / SCALE) * SCREEN_WIDTH..][..SCREEN_WIDTH];
        for (x, pixel) in row.iter_mut().enumerate() {
            *pixel = PALETTE[source_row[x / SCALE] as usize];
        }
    }
}

fn load_save(gameboy: &mut GameBoy, save_path: &Path, import_save: Option<&Path>) {
    if let Ok(data) = std::fs::read(save_path) {
        gameboy.bus.cartridge.load_save_data(&data);
    } else if let Some(import_path) = import_save
        && let Ok(data) = std::fs::read(import_path)
    {
        gameboy.bus.cartridge.load_save_data(&data);
        println!("using existing save {}", import_path.display());
    }
}

fn write_save(gameboy: &GameBoy, save_path: &Path) {
    let result = save_path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(save_path, gameboy.bus.cartridge.save_data()));
    if let Err(error) = result {
        eprintln!("could not write {}: {error}", save_path.display());
    }
}
