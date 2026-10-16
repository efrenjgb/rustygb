use crate::apu::Apu;
use crate::cartridge::Cartridge;
use crate::joypad::{Button, Joypad};
use crate::ppu::Ppu;
use crate::serial::Serial;
use crate::timer::Timer;

// Interrupt flag bits, used by IF (0xFF0F) and IE (0xFFFF).
pub const INT_VBLANK: u8 = 0x01;
pub const INT_STAT: u8 = 0x02;
pub const INT_TIMER: u8 = 0x04;
pub const INT_SERIAL: u8 = 0x08;
pub const INT_JOYPAD: u8 = 0x10;

/// The memory bus. Every CPU read and write goes through here and is routed
/// to the right piece of hardware based on the address.
///
/// The CPU calls `tick` once per machine cycle (4 clocks), which keeps the
/// timer, PPU, serial port and DMA in lock step with it.
///
/// Memory map:
///   0000-7FFF  cartridge ROM (writes here go to the cartridge's bank controller)
///   8000-9FFF  video RAM
///   A000-BFFF  cartridge RAM
///   C000-DFFF  work RAM
///   E000-FDFF  echo of work RAM
///   FE00-FE9F  sprite attribute table (OAM)
///   FF00-FF7F  I/O registers
///   FF80-FFFE  high RAM
///   FFFF       interrupt enable
pub struct Bus {
    pub cartridge: Cartridge,
    wram: [u8; 0x2000],
    hram: [u8; 0x7F],
    /// Registers we don't emulate are just stored here.
    io: [u8; 0x80],
    pub interrupt_enable: u8,
    pub interrupt_flag: u8,
    pub ppu: Ppu,
    pub timer: Timer,
    pub joypad: Joypad,
    pub serial: Serial,
    pub apu: Apu,
    dma: Dma,
    /// Clock cycles since power on.
    pub cycles: u64,
    /// Interrupts raised by the PPU reach IF one machine cycle later.
    delayed_ppu_interrupts: u8,
}

/// OAM DMA copies 160 bytes into the sprite table, one byte per machine cycle.
/// While it runs the CPU can't use OAM, so games run the DMA from high RAM and wait.
struct Dma {
    /// Last value written to 0xFF46.
    register: u8,
    active: bool,
    source: u16,
    index: u16,
    /// A new transfer starts after a short setup delay (in machine cycles).
    pending_start: Option<(u16, u8)>,
}

impl Bus {
    pub fn new(cartridge: Cartridge) -> Self {
        Bus {
            cartridge,
            wram: [0; 0x2000],
            hram: [0; 0x7F],
            io: [0; 0x80],
            interrupt_enable: 0,
            interrupt_flag: 0xE1,
            ppu: Ppu::new(),
            timer: Timer::new(),
            joypad: Joypad::new(),
            serial: Serial::new(),
            apu: Apu::new(),
            dma: Dma {
                register: 0xFF,
                active: false,
                source: 0,
                index: 0,
                pending_start: None,
            },
            cycles: 0,
            delayed_ppu_interrupts: 0,
        }
    }

    /// Advances the hardware that runs alongside the CPU by one machine cycle.
    pub fn tick(&mut self) {
        self.cycles += 4;
        self.interrupt_flag |= self.timer.tick();
        self.interrupt_flag |= self.serial.tick(self.timer.counter);
        self.apu.tick(self.timer.counter);
        self.interrupt_flag |= self.delayed_ppu_interrupts;
        self.delayed_ppu_interrupts = self.ppu.tick(4);
        self.tick_dma();
    }

    /// A CPU memory read, which takes one machine cycle.
    pub fn read_cycle(&mut self, addr: u16) -> u8 {
        self.tick();
        self.read(addr)
    }

    /// A CPU memory write, which takes one machine cycle.
    pub fn write_cycle(&mut self, addr: u16, value: u8) {
        self.tick();
        self.write(addr, value);
    }

    /// Interrupts that are both requested and enabled.
    pub fn pending_interrupts(&self) -> u8 {
        self.interrupt_enable & self.interrupt_flag & 0x1F
    }

    fn tick_dma(&mut self) {
        if let Some((source, delay)) = self.dma.pending_start {
            if delay > 1 {
                self.dma.pending_start = Some((source, delay - 1));
            } else {
                // Starting a new transfer also restarts one that's already running.
                self.dma.pending_start = None;
                self.dma.active = true;
                self.dma.source = source;
                self.dma.index = 0;
            }
        }

        if self.dma.active {
            if self.dma.index == 0xA0 {
                self.dma.active = false;
            } else {
                let value = self.read_unblocked(self.dma.source + self.dma.index);
                self.ppu.oam[self.dma.index as usize] = value;
                self.dma.index += 1;
            }
        }
    }

    fn start_dma(&mut self, value: u8) {
        self.dma.register = value;
        // Sources above 0xDFFF read from work RAM through the echo area.
        let source = if value >= 0xE0 { value - 0x20 } else { value };
        // One cycle of setup, then the first byte is copied on the cycle after.
        self.dma.pending_start = Some(((source as u16) << 8, 2));
    }

    pub fn set_button(&mut self, button: Button, pressed: bool) {
        if self.joypad.set_button(button, pressed) {
            self.interrupt_flag |= INT_JOYPAD;
        }
    }

    /// A read as the CPU sees it: VRAM and OAM are unavailable while the PPU
    /// or DMA is using them.
    pub fn read(&self, addr: u16) -> u8 {
        match addr {
            0x8000..=0x9FFF if !self.ppu.vram_accessible() => 0xFF,
            0xFE00..=0xFE9F if self.dma.active || !self.ppu.oam_accessible() => 0xFF,
            _ => self.read_unblocked(addr),
        }
    }

    fn read_unblocked(&self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x7FFF => self.cartridge.read_rom(addr),
            0x8000..=0x9FFF => self.ppu.vram[(addr - 0x8000) as usize],
            0xA000..=0xBFFF => self.cartridge.read_ram(addr),
            0xC000..=0xDFFF => self.wram[(addr - 0xC000) as usize],
            0xE000..=0xFDFF => self.wram[(addr - 0xE000) as usize],
            0xFE00..=0xFE9F => self.ppu.oam[(addr - 0xFE00) as usize],
            0xFEA0..=0xFEFF => 0xFF,
            0xFF00 => self.joypad.read(),
            0xFF01..=0xFF02 => self.serial.read(addr),
            0xFF04..=0xFF07 => self.timer.read(addr),
            0xFF0F => self.interrupt_flag | 0xE0,
            0xFF10..=0xFF3F => self.apu.read(addr),
            0xFF46 => self.dma.register,
            0xFF40..=0xFF4B => self.ppu.read_register(addr),
            0xFF00..=0xFF7F => self.io[(addr - 0xFF00) as usize],
            0xFF80..=0xFFFE => self.hram[(addr - 0xFF80) as usize],
            0xFFFF => self.interrupt_enable,
        }
    }

    pub fn write(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0x7FFF => self.cartridge.write_rom(addr, value),
            0x8000..=0x9FFF if !self.ppu.vram_writable() => {}
            0x8000..=0x9FFF => self.ppu.vram[(addr - 0x8000) as usize] = value,
            0xA000..=0xBFFF => self.cartridge.write_ram(addr, value),
            0xC000..=0xDFFF => self.wram[(addr - 0xC000) as usize] = value,
            0xE000..=0xFDFF => self.wram[(addr - 0xE000) as usize] = value,
            0xFE00..=0xFE9F if self.dma.active || !self.ppu.oam_writable() => {}
            0xFE00..=0xFE9F => self.ppu.oam[(addr - 0xFE00) as usize] = value,
            0xFEA0..=0xFEFF => {}
            0xFF00 => self.joypad.write(value),
            0xFF01..=0xFF02 => self.serial.write(addr, value),
            0xFF04..=0xFF07 => self.timer.write(addr, value),
            0xFF0F => self.interrupt_flag = value & 0x1F,
            0xFF10..=0xFF3F => self.apu.write(addr, value),
            0xFF46 => self.start_dma(value),
            0xFF40..=0xFF4B => self.interrupt_flag |= self.ppu.write_register(addr, value),
            0xFF00..=0xFF7F => self.io[(addr - 0xFF00) as usize] = value,
            0xFF80..=0xFFFE => self.hram[(addr - 0xFF80) as usize] = value,
            0xFFFF => self.interrupt_enable = value,
        }
    }
}
