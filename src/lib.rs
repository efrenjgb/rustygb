pub mod apu;
pub mod bus;
pub mod cartridge;
pub mod cpu;
pub mod joypad;
pub mod ppu;
pub mod serial;
pub mod timer;

use bus::Bus;
use cartridge::Cartridge;
use cpu::Cpu;

/// Number of CPU clock cycles (T-cycles) in one frame (154 lines * 456 dots).
/// At 4194304 Hz that's 59.73 frames per second.
pub const CYCLES_PER_FRAME: u32 = 70224;

/// The whole machine: a CPU connected to everything else through the bus.
pub struct GameBoy {
    pub cpu: Cpu,
    pub bus: Bus,
}

impl GameBoy {
    pub fn new(rom: Vec<u8>) -> Result<Self, String> {
        Ok(GameBoy {
            cpu: Cpu::new(),
            bus: Bus::new(Cartridge::new(rom)?),
        })
    }

    /// Runs the emulator until the PPU finishes drawing a frame (the start of
    /// vblank), so `screen()` always shows one whole frame.
    ///
    /// While the LCD is off there is no vblank, so it stops after one frame's
    /// worth of cycles instead.
    pub fn run_frame(&mut self) {
        let start = self.bus.cycles;
        self.bus.ppu.frame_ready = false;
        while !self.bus.ppu.frame_ready && self.bus.cycles - start < CYCLES_PER_FRAME as u64 {
            self.cpu.step(&mut self.bus);
        }
    }

    /// Starts producing audio samples at the given rate (for example 48000).
    pub fn enable_audio(&mut self, sample_rate: u32) {
        self.bus.apu.set_sample_rate(sample_rate);
    }

    /// Takes the audio produced since the last call, as interleaved stereo
    /// samples (left, right, left, right, ...) from -1.0 to 1.0.
    pub fn take_audio_samples(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.bus.apu.samples)
    }

    /// The current screen as 160x144 shades (0 = lightest, 3 = darkest).
    pub fn screen(&self) -> &[u8] {
        &self.bus.ppu.framebuffer
    }
}
