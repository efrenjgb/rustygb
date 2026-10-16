//! Runs a ROM without a window and prints whatever it sends over the serial
//! port. Handy for test ROMs like Blargg's cpu_instrs.
//!
//! usage: cargo run --release --example headless -- <rom.gb> [frames]

use rustygb::GameBoy;

fn main() {
    let mut args = std::env::args().skip(1);
    let rom_path = args.next().expect("usage: headless <rom.gb> [frames]");
    let frames: u32 = args.next().map_or(3000, |n| n.parse().expect("invalid frame count"));

    let rom = std::fs::read(&rom_path).expect("could not read ROM file");
    let mut gameboy = GameBoy::new(rom).expect("could not load ROM");

    for _ in 0..frames {
        gameboy.run_frame();
        let output = String::from_utf8_lossy(&gameboy.bus.serial.output);
        if output.contains("Passed") || output.contains("Failed") {
            break;
        }
    }

    println!("{}", String::from_utf8_lossy(&gameboy.bus.serial.output));
}
