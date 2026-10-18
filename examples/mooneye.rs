//! Runs Mooneye test suite ROMs and reports pass/fail for each.
//!
//! Mooneye tests finish by executing `LD B,B` followed by an endless loop,
//! leaving B, C, D, E, H, L = 3, 5, 8, 13, 21, 34 when they pass.
//!
//! usage: cargo run --release --example mooneye -- [--boot-rom <boot.bin>] <rom.gb>...

use rustygb::GameBoy;

const PASS_REGISTERS: [u8; 6] = [3, 5, 8, 13, 21, 34];
/// The slowest tests take about 500 frames, and the boot ROM adds about 340 more.
const MAX_FRAMES: u32 = 1200;

fn run_test(path: &str, boot_rom: Option<&Vec<u8>>) -> Result<bool, String> {
    let rom = std::fs::read(path).map_err(|error| error.to_string())?;
    let mut gameboy = match boot_rom {
        Some(boot_rom) => GameBoy::with_boot_rom(rom, boot_rom.clone())?,
        None => GameBoy::new(rom)?,
    };

    for _ in 0..MAX_FRAMES {
        gameboy.run_frame();
        let pc = gameboy.cpu.pc;
        // Stuck on JR -2 means the test has finished.
        if gameboy.bus.read(pc) == 0x18 && gameboy.bus.read(pc.wrapping_add(1)) == 0xFE {
            break;
        }
    }

    let cpu = &gameboy.cpu;
    Ok([cpu.b, cpu.c, cpu.d, cpu.e, cpu.h, cpu.l] == PASS_REGISTERS)
}

fn main() {
    let mut paths: Vec<String> = std::env::args().skip(1).collect();
    let mut boot_rom = None;
    if paths.first().is_some_and(|arg| arg == "--boot-rom") && paths.len() >= 2 {
        boot_rom = Some(std::fs::read(&paths[1]).expect("could not read boot ROM"));
        paths.drain(..2);
    }
    let mut passed = 0;

    for path in &paths {
        match run_test(path, boot_rom.as_ref()) {
            Ok(true) => {
                passed += 1;
                println!("PASS  {path}");
            }
            Ok(false) => println!("FAIL  {path}"),
            Err(error) => println!("ERROR {path}: {error}"),
        }
    }
    println!("{passed}/{} passed", paths.len());
}
