use crate::bus::INT_SERIAL;

/// Serial port (SB 0xFF01, SC 0xFF02) with no link cable attached.
///
/// With the internal clock selected, a transfer shifts out one bit every
/// 512 clocks (8192 Hz), timed by bit 8 of the timer's internal counter.
/// Since nobody is on the other end, 1s are shifted in, so the received byte is 0xFF.
pub struct Serial {
    data: u8,
    control: u8,
    bits_left: u8,
    last_clock_bit: bool,
    /// Every byte sent. Test ROMs print their results here.
    pub output: Vec<u8>,
}

impl Serial {
    pub fn new() -> Self {
        Serial {
            data: 0,
            control: 0,
            bits_left: 0,
            last_clock_bit: false,
            output: Vec::new(),
        }
    }

    /// Called every machine cycle with the timer's internal counter.
    /// Returns any interrupt flags raised.
    pub fn tick(&mut self, timer_counter: u16) -> u8 {
        let clock_bit = timer_counter & 0x100 != 0;
        let falling_edge = self.last_clock_bit && !clock_bit;
        self.last_clock_bit = clock_bit;

        if !falling_edge || self.bits_left == 0 {
            return 0;
        }

        self.data = (self.data << 1) | 1;
        self.bits_left -= 1;
        if self.bits_left == 0 {
            self.control &= 0x7F;
            return INT_SERIAL;
        }
        0
    }

    pub fn read(&self, addr: u16) -> u8 {
        match addr {
            0xFF01 => self.data,
            _ => self.control | 0x7E,
        }
    }

    pub fn write(&mut self, addr: u16, value: u8) {
        match addr {
            0xFF01 => self.data = value,
            _ => {
                self.control = value;
                let start_transfer = value & 0x80 != 0;
                let internal_clock = value & 0x01 != 0;
                // With an external clock the transfer waits forever for the other Game Boy.
                if start_transfer && internal_clock {
                    self.output.push(self.data);
                    self.bits_left = 8;
                } else {
                    self.bits_left = 0;
                }
            }
        }
    }
}
