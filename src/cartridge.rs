use std::time::{SystemTime, UNIX_EPOCH};

const ROM_BANK_SIZE: usize = 0x4000;
const RAM_BANK_SIZE: usize = 0x2000;

/// A game cartridge: its ROM, optional RAM, and the memory bank controller
/// (MBC) chip that decides which part of the ROM/RAM the CPU can see.
///
/// The CPU only has a 32KB window for ROM (0000-7FFF) and 8KB for cartridge
/// RAM (A000-BFFF). Bigger games write to the ROM area to tell the MBC which
/// bank to show in those windows.
pub struct Cartridge {
    rom: Vec<u8>,
    ram: Vec<u8>,
    mbc: Mbc,
    /// Battery backed RAM keeps its contents when the Game Boy is off, so we save it to disk.
    pub has_battery: bool,
}

enum Mbc {
    RomOnly,
    Mbc1 {
        ram_enabled: bool,
        /// Lower 5 bits of the ROM bank number.
        rom_bank: u8,
        /// 2 extra bits: the RAM bank, or the upper bits of the ROM bank on big carts.
        upper_bits: u8,
        /// When set, `upper_bits` also applies to 0000-3FFF and the RAM area.
        advanced_mode: bool,
    },
    Mbc3 {
        ram_enabled: bool,
        rom_bank: u8,
        /// 0x00-0x03 selects a RAM bank, 0x08-0x0C selects a clock register.
        ram_bank: u8,
        rtc: Rtc,
        /// Latching happens when 0x00 and then 0x01 are written to 6000-7FFF.
        last_latch_write: u8,
    },
    Mbc5 {
        ram_enabled: bool,
        /// 9-bit ROM bank number. Unlike the others, bank 0 can be selected.
        rom_bank: u16,
        ram_bank: u8,
        /// Rumble carts use bit 3 of the RAM bank register for the motor.
        has_rumble: bool,
    },
}

impl Cartridge {
    pub fn new(rom: Vec<u8>) -> Result<Self, String> {
        if rom.len() < 0x150 {
            return Err("ROM is too small to have a header".to_string());
        }

        let cartridge_type = rom[0x147];
        let ram_size = match rom[0x149] {
            0x02 => RAM_BANK_SIZE,
            0x03 => RAM_BANK_SIZE * 4,
            0x04 => RAM_BANK_SIZE * 16,
            0x05 => RAM_BANK_SIZE * 8,
            _ => 0,
        };

        let mbc1 = Mbc::Mbc1 {
            ram_enabled: false,
            rom_bank: 1,
            upper_bits: 0,
            advanced_mode: false,
        };
        let mbc3 = Mbc::Mbc3 {
            ram_enabled: false,
            rom_bank: 1,
            ram_bank: 0,
            rtc: Rtc::new(),
            last_latch_write: 0xFF,
        };

        let mbc5 = |has_rumble| Mbc::Mbc5 {
            ram_enabled: false,
            rom_bank: 1,
            ram_bank: 0,
            has_rumble,
        };

        let (mbc, has_battery) = match cartridge_type {
            0x00 => (Mbc::RomOnly, false),
            0x01 | 0x02 => (mbc1, false),
            0x03 => (mbc1, true),
            0x11 | 0x12 => (mbc3, false),
            0x0F | 0x10 | 0x13 => (mbc3, true),
            0x19 | 0x1A => (mbc5(false), false),
            0x1B => (mbc5(false), true),
            0x1C | 0x1D => (mbc5(true), false),
            0x1E => (mbc5(true), true),
            other => return Err(format!("unsupported cartridge type {other:#04X}")),
        };

        Ok(Cartridge {
            rom,
            ram: vec![0; ram_size],
            mbc,
            has_battery,
        })
    }

    /// The game title stored in the header.
    pub fn title(&self) -> String {
        self.rom[0x134..0x144]
            .iter()
            .take_while(|&&byte| byte != 0)
            .map(|&byte| byte as char)
            .collect()
    }

    pub fn read_rom(&self, addr: u16) -> u8 {
        let bank = match &self.mbc {
            Mbc::RomOnly => {
                return self.rom.get(addr as usize).copied().unwrap_or(0xFF);
            }
            Mbc::Mbc1 {
                rom_bank,
                upper_bits,
                advanced_mode,
                ..
            } => {
                let bank = if addr < 0x4000 {
                    if *advanced_mode { *upper_bits << 5 } else { 0 }
                } else {
                    (*upper_bits << 5) | *rom_bank
                };
                bank as usize
            }
            Mbc::Mbc3 { rom_bank, .. } => {
                if addr < 0x4000 { 0 } else { *rom_bank as usize }
            }
            Mbc::Mbc5 { rom_bank, .. } => {
                if addr < 0x4000 { 0 } else { *rom_bank as usize }
            }
        };

        let offset = addr as usize % ROM_BANK_SIZE;
        let index = bank * ROM_BANK_SIZE + offset;
        // Bank numbers past the end of the ROM wrap around, like on real hardware.
        self.rom[index % self.rom.len()]
    }

    pub fn write_rom(&mut self, addr: u16, value: u8) {
        match &mut self.mbc {
            Mbc::RomOnly => {}
            Mbc::Mbc1 {
                ram_enabled,
                rom_bank,
                upper_bits,
                advanced_mode,
            } => match addr {
                0x0000..=0x1FFF => *ram_enabled = value & 0x0F == 0x0A,
                // Bank 0 can't be selected here, it turns into bank 1.
                0x2000..=0x3FFF => *rom_bank = (value & 0x1F).max(1),
                0x4000..=0x5FFF => *upper_bits = value & 0x03,
                _ => *advanced_mode = value & 0x01 != 0,
            },
            Mbc::Mbc3 {
                ram_enabled,
                rom_bank,
                ram_bank,
                rtc,
                last_latch_write,
            } => match addr {
                0x0000..=0x1FFF => *ram_enabled = value & 0x0F == 0x0A,
                0x2000..=0x3FFF => *rom_bank = (value & 0x7F).max(1),
                0x4000..=0x5FFF => *ram_bank = value,
                _ => {
                    if *last_latch_write == 0x00 && value == 0x01 {
                        rtc.latch();
                    }
                    *last_latch_write = value;
                }
            },
            Mbc::Mbc5 {
                ram_enabled,
                rom_bank,
                ram_bank,
                has_rumble,
            } => match addr {
                0x0000..=0x1FFF => *ram_enabled = value & 0x0F == 0x0A,
                // The ROM bank is split over two registers: low 8 bits, then bit 8.
                0x2000..=0x2FFF => *rom_bank = (*rom_bank & 0x100) | value as u16,
                0x3000..=0x3FFF => *rom_bank = (*rom_bank & 0xFF) | ((value as u16 & 0x01) << 8),
                0x4000..=0x5FFF => {
                    let mask = if *has_rumble { 0x07 } else { 0x0F };
                    *ram_bank = value & mask;
                }
                _ => {}
            },
        }
    }

    pub fn read_ram(&self, addr: u16) -> u8 {
        match &self.mbc {
            Mbc::RomOnly => 0xFF,
            Mbc::Mbc1 {
                ram_enabled,
                upper_bits,
                advanced_mode,
                ..
            } => {
                if !*ram_enabled {
                    return 0xFF;
                }
                let bank = if *advanced_mode { *upper_bits } else { 0 };
                self.ram_index(bank, addr)
                    .map_or(0xFF, |index| self.ram[index])
            }
            Mbc::Mbc3 {
                ram_enabled,
                ram_bank,
                rtc,
                ..
            } => {
                if !*ram_enabled {
                    return 0xFF;
                }
                match *ram_bank {
                    0x00..=0x03 => self
                        .ram_index(*ram_bank, addr)
                        .map_or(0xFF, |index| self.ram[index]),
                    0x08..=0x0C => rtc.read(*ram_bank),
                    _ => 0xFF,
                }
            }
            Mbc::Mbc5 {
                ram_enabled,
                ram_bank,
                ..
            } => {
                if !*ram_enabled {
                    return 0xFF;
                }
                self.ram_index(*ram_bank, addr)
                    .map_or(0xFF, |index| self.ram[index])
            }
        }
    }

    pub fn write_ram(&mut self, addr: u16, value: u8) {
        let bank = match &mut self.mbc {
            Mbc::RomOnly => return,
            Mbc::Mbc1 {
                ram_enabled,
                upper_bits,
                advanced_mode,
                ..
            } => {
                if !*ram_enabled {
                    return;
                }
                if *advanced_mode { *upper_bits } else { 0 }
            }
            Mbc::Mbc3 {
                ram_enabled,
                ram_bank,
                rtc,
                ..
            } => {
                if !*ram_enabled {
                    return;
                }
                match *ram_bank {
                    0x00..=0x03 => *ram_bank,
                    0x08..=0x0C => {
                        rtc.write(*ram_bank, value);
                        return;
                    }
                    _ => return,
                }
            }
            Mbc::Mbc5 {
                ram_enabled,
                ram_bank,
                ..
            } => {
                if !*ram_enabled {
                    return;
                }
                *ram_bank
            }
        };

        if let Some(index) = self.ram_index(bank, addr) {
            self.ram[index] = value;
        }
    }

    /// Where an A000-BFFF address lands in `ram` for the given bank.
    fn ram_index(&self, bank: u8, addr: u16) -> Option<usize> {
        if self.ram.is_empty() {
            return None;
        }
        let index = bank as usize * RAM_BANK_SIZE + (addr as usize - 0xA000);
        Some(index % self.ram.len())
    }

    /// The contents of a .sav file: the cartridge RAM, followed by the clock
    /// state for MBC3 games (in the same format as other emulators, like BGB and VBA).
    pub fn save_data(&self) -> Vec<u8> {
        let mut data = self.ram.clone();
        if let Mbc::Mbc3 { rtc, .. } = &self.mbc {
            data.extend(rtc.save_data());
        }
        data
    }

    pub fn load_save_data(&mut self, data: &[u8]) {
        let ram_len = self.ram.len().min(data.len());
        self.ram[..ram_len].copy_from_slice(&data[..ram_len]);
        if let Mbc::Mbc3 { rtc, .. } = &mut self.mbc {
            rtc.load_save_data(&data[ram_len..]);
        }
    }
}

/// The MBC3 real time clock. It keeps counting while the emulator is closed
/// by remembering the real world time it was last updated.
#[derive(Clone, Copy)]
struct Rtc {
    seconds: u8,
    minutes: u8,
    hours: u8,
    /// 9-bit day counter.
    days: u16,
    halted: bool,
    /// Set when the day counter overflows past 511, until the game clears it.
    day_carry: bool,
    /// Games read a snapshot of the clock taken when they latch it.
    latched: [u8; 5],
    /// Unix time in seconds of the last update.
    last_update: u64,
}

impl Rtc {
    fn new() -> Self {
        Rtc {
            seconds: 0,
            minutes: 0,
            hours: 0,
            days: 0,
            halted: false,
            day_carry: false,
            latched: [0; 5],
            last_update: unix_time_now(),
        }
    }

    /// The registers as the game sees them: seconds, minutes, hours, day low, day high.
    fn registers(&self) -> [u8; 5] {
        let day_high = ((self.days >> 8) as u8 & 0x01)
            | if self.halted { 0x40 } else { 0 }
            | if self.day_carry { 0x80 } else { 0 };
        [
            self.seconds,
            self.minutes,
            self.hours,
            self.days as u8,
            day_high,
        ]
    }

    /// Adds the real time that passed since the last update to the clock.
    fn update(&mut self) {
        let now = unix_time_now();
        let elapsed = now.saturating_sub(self.last_update);
        self.last_update = now;
        if self.halted || elapsed == 0 {
            return;
        }

        let total_seconds = self.seconds as u64
            + self.minutes as u64 * 60
            + self.hours as u64 * 3600
            + self.days as u64 * 86400
            + elapsed;

        self.seconds = (total_seconds % 60) as u8;
        self.minutes = (total_seconds / 60 % 60) as u8;
        self.hours = (total_seconds / 3600 % 24) as u8;
        let days = total_seconds / 86400;
        if days > 511 {
            self.day_carry = true;
        }
        self.days = (days % 512) as u16;
    }

    fn latch(&mut self) {
        self.update();
        self.latched = self.registers();
    }

    fn read(&self, register: u8) -> u8 {
        self.latched[(register - 0x08) as usize]
    }

    fn write(&mut self, register: u8, value: u8) {
        // Bring the clock up to date first so the time before this write isn't lost.
        self.update();
        match register {
            0x08 => self.seconds = value & 0x3F,
            0x09 => self.minutes = value & 0x3F,
            0x0A => self.hours = value & 0x1F,
            0x0B => self.days = (self.days & 0x100) | value as u16,
            _ => {
                self.days = (self.days & 0xFF) | ((value as u16 & 0x01) << 8);
                self.halted = value & 0x40 != 0;
                self.day_carry = value & 0x80 != 0;
            }
        }
    }

    /// 48 bytes: the 5 registers and the 5 latched registers as 32-bit
    /// little endian values, then the unix timestamp as a 64-bit value.
    fn save_data(&self) -> Vec<u8> {
        let mut rtc = *self;
        rtc.update();

        let mut data = Vec::with_capacity(48);
        for value in rtc.registers().iter().chain(rtc.latched.iter()) {
            data.extend((*value as u32).to_le_bytes());
        }
        data.extend(rtc.last_update.to_le_bytes());
        data
    }

    fn load_save_data(&mut self, data: &[u8]) {
        if data.len() < 48 {
            return;
        }
        let value = |i: usize| data[i * 4];
        self.seconds = value(0);
        self.minutes = value(1);
        self.hours = value(2);
        self.days = value(3) as u16 | ((value(4) as u16 & 0x01) << 8);
        self.halted = value(4) & 0x40 != 0;
        self.day_carry = value(4) & 0x80 != 0;
        for i in 0..5 {
            self.latched[i] = value(5 + i);
        }
        self.last_update = u64::from_le_bytes(data[40..48].try_into().unwrap());
        // Catch up on the time that passed while the emulator was closed.
        self.update();
    }
}

fn unix_time_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake MBC3 cartridge with 128 ROM banks (each bank's first byte is its
    /// number) and 32KB of RAM.
    fn mbc3_cartridge() -> Cartridge {
        let mut rom = vec![0; ROM_BANK_SIZE * 128];
        for bank in 0..128 {
            rom[bank * ROM_BANK_SIZE] = bank as u8;
        }
        rom[0x147] = 0x10; // MBC3 + timer + RAM + battery
        rom[0x149] = 0x03; // 32KB RAM
        Cartridge::new(rom).unwrap()
    }

    /// A fake MBC5 cartridge with 512 ROM banks (each bank starts with its
    /// number as 2 bytes) and 128KB of RAM.
    fn mbc5_cartridge() -> Cartridge {
        let mut rom = vec![0; ROM_BANK_SIZE * 512];
        for bank in 0..512 {
            rom[bank * ROM_BANK_SIZE] = bank as u8;
            rom[bank * ROM_BANK_SIZE + 1] = (bank >> 8) as u8;
        }
        rom[0x147] = 0x1B; // MBC5 + RAM + battery
        rom[0x149] = 0x04; // 128KB RAM
        Cartridge::new(rom).unwrap()
    }

    #[test]
    fn mbc5_switches_all_512_rom_banks() {
        let mut cart = mbc5_cartridge();
        let bank = |cart: &Cartridge| {
            u16::from_le_bytes([cart.read_rom(0x4000), cart.read_rom(0x4001)])
        };
        assert_eq!(bank(&cart), 1);
        cart.write_rom(0x2000, 0x34);
        cart.write_rom(0x3000, 0x01);
        assert_eq!(bank(&cart), 0x134);
        // Bank 0 is allowed in the switchable area on MBC5.
        cart.write_rom(0x2000, 0x00);
        cart.write_rom(0x3000, 0x00);
        assert_eq!(bank(&cart), 0);
    }

    #[test]
    fn mbc5_has_16_ram_banks() {
        let mut cart = mbc5_cartridge();
        cart.write_rom(0x0000, 0x0A);
        for bank in 0..16 {
            cart.write_rom(0x4000, bank);
            cart.write_ram(0xBFFF, 0x80 + bank);
        }
        for bank in 0..16 {
            cart.write_rom(0x4000, bank);
            assert_eq!(cart.read_ram(0xBFFF), 0x80 + bank);
        }
    }

    #[test]
    fn mbc3_switches_rom_banks() {
        let mut cart = mbc3_cartridge();
        assert_eq!(cart.read_rom(0x4000), 1);
        cart.write_rom(0x2000, 0x7F);
        assert_eq!(cart.read_rom(0x4000), 0x7F);
        cart.write_rom(0x2000, 0x00);
        assert_eq!(cart.read_rom(0x4000), 1);
    }

    #[test]
    fn mbc3_ram_banks_are_separate_and_need_enabling() {
        let mut cart = mbc3_cartridge();
        cart.write_ram(0xA000, 0x11);
        assert_eq!(cart.read_ram(0xA000), 0xFF);

        cart.write_rom(0x0000, 0x0A);
        for bank in 0..4 {
            cart.write_rom(0x4000, bank);
            cart.write_ram(0xA000, 0x10 + bank);
        }
        for bank in 0..4 {
            cart.write_rom(0x4000, bank);
            assert_eq!(cart.read_ram(0xA000), 0x10 + bank);
        }
    }

    #[test]
    fn mbc3_clock_registers_latch() {
        let mut cart = mbc3_cartridge();
        cart.write_rom(0x0000, 0x0A);
        // Halt the clock so real time can't change it during the test.
        cart.write_rom(0x4000, 0x0C);
        cart.write_ram(0xA000, 0x40);
        cart.write_rom(0x4000, 0x0A);
        cart.write_ram(0xA000, 13);

        // Reads return the old latched value until the game latches again.
        assert_eq!(cart.read_ram(0xA000), 0);
        cart.write_rom(0x6000, 0x00);
        cart.write_rom(0x6000, 0x01);
        assert_eq!(cart.read_ram(0xA000), 13);
    }

    #[test]
    fn mbc3_save_data_round_trips() {
        let mut cart = mbc3_cartridge();
        cart.write_rom(0x0000, 0x0A);
        cart.write_rom(0x4000, 0x02);
        cart.write_ram(0xA123, 0x99);
        cart.write_rom(0x4000, 0x08);
        cart.write_ram(0xA000, 42);

        let data = cart.save_data();
        assert_eq!(data.len(), RAM_BANK_SIZE * 4 + 48);

        let mut loaded = mbc3_cartridge();
        loaded.load_save_data(&data);
        loaded.write_rom(0x0000, 0x0A);
        loaded.write_rom(0x4000, 0x02);
        assert_eq!(loaded.read_ram(0xA123), 0x99);
        loaded.write_rom(0x6000, 0x00);
        loaded.write_rom(0x6000, 0x01);
        loaded.write_rom(0x4000, 0x08);
        assert!(loaded.read_ram(0xA000) >= 42);
    }
}
