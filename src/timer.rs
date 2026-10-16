use crate::bus::INT_TIMER;

/// DIV / TIMA / TMA / TAC timer registers.
///
/// DIV is the upper byte of a 16-bit counter that increments every cycle.
/// TIMA increments whenever the counter bit selected by TAC goes from 1 to 0
/// while the timer is enabled. Because it is an edge detector, writing DIV or
/// TAC can also cause an extra increment, which some games depend on.
pub struct Timer {
    /// Internal 16-bit counter. DIV is its upper byte.
    pub counter: u16,
    tima: u8,
    tma: u8,
    tac: u8,
    /// The edge detector's input during the last cycle.
    last_signal: bool,
    /// TIMA overflowed and reads as 0 for one machine cycle before TMA is loaded.
    overflow_pending: bool,
    /// TMA was loaded into TIMA during the current machine cycle.
    reloading: bool,
}

impl Timer {
    pub fn new() -> Self {
        Timer {
            counter: 0xABC8,
            tima: 0,
            tma: 0,
            tac: 0,
            last_signal: false,
            overflow_pending: false,
            reloading: false,
        }
    }

    /// Advances the timer by one machine cycle (4 clocks) and returns any interrupt flags it raised.
    pub fn tick(&mut self) -> u8 {
        let mut interrupts = 0;

        self.reloading = false;
        if self.overflow_pending {
            self.overflow_pending = false;
            self.tima = self.tma;
            self.reloading = true;
            interrupts |= INT_TIMER;
        }

        for _ in 0..4 {
            self.counter = self.counter.wrapping_add(1);
            self.detect_edge();
        }
        interrupts
    }

    /// The bit TIMA watches, ANDed with the enable bit.
    fn signal(&self) -> bool {
        let bit = match self.tac & 0x03 {
            0 => 9, // 4096 Hz
            1 => 3, // 262144 Hz
            2 => 5, // 65536 Hz
            _ => 7, // 16384 Hz
        };
        self.tac & 0x04 != 0 && self.counter & (1 << bit) != 0
    }

    /// Increments TIMA on a falling edge of the signal.
    fn detect_edge(&mut self) {
        let signal = self.signal();
        if self.last_signal && !signal {
            self.increment_tima();
        }
        self.last_signal = signal;
    }

    fn increment_tima(&mut self) {
        let (value, overflow) = self.tima.overflowing_add(1);
        self.tima = value;
        if overflow {
            self.overflow_pending = true;
        }
    }

    pub fn read(&self, addr: u16) -> u8 {
        match addr {
            0xFF04 => (self.counter >> 8) as u8,
            0xFF05 => self.tima,
            0xFF06 => self.tma,
            _ => self.tac | 0xF8,
        }
    }

    pub fn write(&mut self, addr: u16, value: u8) {
        match addr {
            0xFF04 => {
                self.counter = 0;
                self.detect_edge();
            }
            0xFF05 => {
                // Writing during the reload cycle is ignored, since TMA wins.
                // Writing during the delay before it cancels the reload.
                if !self.reloading {
                    self.tima = value;
                    self.overflow_pending = false;
                }
            }
            0xFF06 => {
                self.tma = value;
                if self.reloading {
                    self.tima = value;
                }
            }
            _ => {
                self.tac = value & 0x07;
                self.detect_edge();
            }
        }
    }
}
