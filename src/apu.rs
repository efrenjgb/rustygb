//! The audio processing unit (APU).
//!
//! The Game Boy has four sound channels:
//!   1. square wave with frequency sweep (NR10-NR14)
//!   2. square wave (NR21-NR24)
//!   3. wave channel playing 32 4-bit samples from wave RAM (NR30-NR34)
//!   4. noise from a linear feedback shift register (NR41-NR44)
//!
//! Each channel produces a digital value 0-15, which its DAC turns into an
//! analog level. The mixer (NR50/NR51) combines them into left and right.
//!
//! The frame sequencer, clocked at 512 Hz by the timer's divider, drives the
//! slower parts: length counters, volume envelopes and the frequency sweep.

const CLOCK_RATE: f64 = 4_194_304.0;

/// Scales the final output down so loud passages with all four channels
/// don't clip after the high-pass filter.
const OUTPUT_GAIN: f32 = 0.5;

/// How many clocks after channel 3 reads a sample the CPU can still reach wave RAM.
const WAVE_ACCESS_WINDOW: u8 = 2;
/// Retriggering channel 3 this many clocks or fewer before it reads a sample corrupts wave RAM.
const RETRIGGER_CORRUPTION_WINDOW: u16 = 2;

/// Which of the 8 steps of each square wave are high, for the 4 duty cycles.
const DUTY_PATTERNS: [[u8; 8]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 1], // 12.5%
    [1, 0, 0, 0, 0, 0, 0, 1], // 25%
    [1, 0, 0, 0, 0, 1, 1, 1], // 50%
    [0, 1, 1, 1, 1, 1, 1, 0], // 75%
];

pub struct Apu {
    powered: bool,
    square1: SquareChannel,
    square2: SquareChannel,
    wave: WaveChannel,
    noise: NoiseChannel,
    /// NR50: master volume for left (bits 4-6) and right (bits 0-2).
    nr50: u8,
    /// NR51: which channels go to the left (bits 4-7) and right (bits 0-3).
    nr51: u8,

    /// The next frame sequencer step to run, 0..8.
    frame_sequencer_step: u8,
    /// The divider bit that clocks the frame sequencer, as of the last cycle.
    last_divider_bit: bool,

    /// Clock cycles per output sample, or None when nobody wants audio
    /// (headless runs), in which case no samples are produced.
    cycles_per_sample: Option<f64>,
    cycles_until_sample: f64,
    /// Running sums used to average the output over each sample period.
    left_sum: f32,
    right_sum: f32,
    sum_count: u32,
    /// The capacitors that remove the DC offset, like the real hardware's high-pass filter.
    left_capacitor: f32,
    right_capacitor: f32,
    capacitor_charge_factor: f32,
    /// Interleaved stereo samples (left, right, left, right, ...) waiting to be played.
    pub samples: Vec<f32>,
}

impl Apu {
    pub fn new() -> Self {
        let mut apu = Apu {
            powered: true,
            square1: SquareChannel::new(true),
            square2: SquareChannel::new(false),
            wave: WaveChannel::new(),
            noise: NoiseChannel::new(),
            nr50: 0x77,
            nr51: 0xF3,
            frame_sequencer_step: 0,
            last_divider_bit: false,
            cycles_per_sample: None,
            cycles_until_sample: 0.0,
            left_sum: 0.0,
            right_sum: 0.0,
            sum_count: 0,
            left_capacitor: 0.0,
            right_capacitor: 0.0,
            capacitor_charge_factor: 1.0,
            samples: Vec::new(),
        };
        // The boot ROM plays its chime on channel 1, so it's left enabled with
        // its volume faded out.
        apu.square1.duty = 2;
        apu.square1.envelope.write(0xF3);
        apu.square1.envelope.volume = 0;
        apu.square1.enabled = true;
        apu
    }

    /// Starts producing samples at the given rate.
    pub fn set_sample_rate(&mut self, sample_rate: u32) {
        let cycles_per_sample = CLOCK_RATE / sample_rate as f64;
        self.cycles_per_sample = Some(cycles_per_sample);
        self.cycles_until_sample = cycles_per_sample;
        self.capacitor_charge_factor = 0.999958f32.powf(cycles_per_sample as f32);
    }

    /// Advances the APU by one machine cycle (4 clocks).
    /// `timer_counter` is the timer's internal counter, whose bit 12 clocks the frame sequencer.
    pub fn tick(&mut self, timer_counter: u16) {
        let divider_bit = timer_counter & 0x1000 != 0;
        if self.powered && self.last_divider_bit && !divider_bit {
            self.step_frame_sequencer();
        }
        self.last_divider_bit = divider_bit;

        if self.powered {
            for _ in 0..4 {
                self.square1.tick();
                self.square2.tick();
                self.wave.tick();
                self.noise.tick();
            }
        }

        if let Some(cycles_per_sample) = self.cycles_per_sample {
            self.produce_samples(cycles_per_sample);
        }
    }

    fn step_frame_sequencer(&mut self) {
        let step = self.frame_sequencer_step;
        if step.is_multiple_of(2) {
            self.square1.clock_length();
            self.square2.clock_length();
            self.wave.clock_length();
            self.noise.clock_length();
        }
        if step == 2 || step == 6 {
            self.square1.clock_sweep();
        }
        if step == 7 {
            self.square1.envelope.clock();
            self.square2.envelope.clock();
            self.noise.envelope.clock();
        }
        self.frame_sequencer_step = (step + 1) % 8;
    }

    /// When the frame sequencer's next step doesn't clock the length
    /// counters, enabling a length counter clocks it once immediately.
    fn next_step_skips_length(&self) -> bool {
        !self.frame_sequencer_step.is_multiple_of(2)
    }

    // ---------------------------------------------------------------
    // Mixing and output
    // ---------------------------------------------------------------

    fn produce_samples(&mut self, cycles_per_sample: f64) {
        let (left, right) = self.mix();
        self.left_sum += left;
        self.right_sum += right;
        self.sum_count += 1;

        self.cycles_until_sample -= 4.0;
        if self.cycles_until_sample > 0.0 {
            return;
        }
        self.cycles_until_sample += cycles_per_sample;

        // Averaging over the sample period smooths out frequencies that are
        // too high for the output sample rate.
        let left = self.left_sum / self.sum_count as f32;
        let right = self.right_sum / self.sum_count as f32;
        self.left_sum = 0.0;
        self.right_sum = 0.0;
        self.sum_count = 0;

        let factor = self.capacitor_charge_factor;
        let left = high_pass(&mut self.left_capacitor, left, factor);
        let right = high_pass(&mut self.right_capacitor, right, factor);
        self.samples.push(left * OUTPUT_GAIN);
        self.samples.push(right * OUTPUT_GAIN);
    }

    /// Mixes the four channels into a left and right level, each -1.0 to 1.0.
    fn mix(&self) -> (f32, f32) {
        if !self.powered {
            return (0.0, 0.0);
        }
        let channels = [
            dac(self.square1.dac_enabled(), self.square1.output()),
            dac(self.square2.dac_enabled(), self.square2.output()),
            dac(self.wave.dac_enabled, self.wave.output()),
            dac(self.noise.dac_enabled(), self.noise.output()),
        ];

        let mut left = 0.0;
        let mut right = 0.0;
        for (i, level) in channels.iter().enumerate() {
            if self.nr51 & (0x10 << i) != 0 {
                left += level;
            }
            if self.nr51 & (0x01 << i) != 0 {
                right += level;
            }
        }

        let left_volume = ((self.nr50 >> 4) & 0x07) as f32 + 1.0;
        let right_volume = (self.nr50 & 0x07) as f32 + 1.0;
        (
            left / 4.0 * left_volume / 8.0,
            right / 4.0 * right_volume / 8.0,
        )
    }

    // ---------------------------------------------------------------
    // Registers
    // ---------------------------------------------------------------

    pub fn read(&self, addr: u16) -> u8 {
        match addr {
            0xFF10 => self.square1.read_sweep(),
            0xFF11 => self.square1.read_duty(),
            0xFF12 => self.square1.envelope.read(),
            0xFF14 => self.square1.length.read_control(),
            0xFF16 => self.square2.read_duty(),
            0xFF17 => self.square2.envelope.read(),
            0xFF19 => self.square2.length.read_control(),
            0xFF1A => 0x7F | if self.wave.dac_enabled { 0x80 } else { 0 },
            0xFF1C => 0x9F | (self.wave.volume_code << 5),
            0xFF1E => self.wave.length.read_control(),
            0xFF21 => self.noise.envelope.read(),
            0xFF22 => self.noise.nr43,
            0xFF23 => self.noise.length.read_control(),
            0xFF24 => self.nr50,
            0xFF25 => self.nr51,
            0xFF26 => {
                let mut value = 0x70;
                if self.powered {
                    value |= 0x80;
                }
                let channels = [
                    self.square1.enabled,
                    self.square2.enabled,
                    self.wave.enabled,
                    self.noise.enabled,
                ];
                for (i, enabled) in channels.iter().enumerate() {
                    if *enabled {
                        value |= 1 << i;
                    }
                }
                value
            }
            0xFF30..=0xFF3F => match self.wave.ram_index(addr) {
                Some(index) => self.wave.ram[index],
                None => 0xFF,
            },
            // Write-only registers (the frequencies) and unused addresses.
            _ => 0xFF,
        }
    }

    pub fn write(&mut self, addr: u16, value: u8) {
        if addr == 0xFF26 {
            self.write_power(value);
            return;
        }
        if (0xFF30..=0xFF3F).contains(&addr) {
            if let Some(index) = self.wave.ram_index(addr) {
                self.wave.ram[index] = value;
            }
            return;
        }
        if !self.powered {
            // While powered off only the length counters can be written (on DMG).
            match addr {
                0xFF11 => self.square1.length.load(value & 0x3F),
                0xFF16 => self.square2.length.load(value & 0x3F),
                0xFF1B => self.wave.length.load(value),
                0xFF20 => self.noise.length.load(value & 0x3F),
                _ => {}
            }
            return;
        }

        let skips_length = self.next_step_skips_length();
        match addr {
            0xFF10 => self.square1.write_sweep(value),
            0xFF11 => self.square1.write_duty(value),
            0xFF12 => self.square1.write_envelope(value),
            0xFF13 => self.square1.write_frequency_low(value),
            0xFF14 => self.square1.write_control(value, skips_length),
            0xFF16 => self.square2.write_duty(value),
            0xFF17 => self.square2.write_envelope(value),
            0xFF18 => self.square2.write_frequency_low(value),
            0xFF19 => self.square2.write_control(value, skips_length),
            0xFF1A => self.wave.write_dac(value),
            0xFF1B => self.wave.length.load(value),
            0xFF1C => self.wave.volume_code = (value >> 5) & 0x03,
            0xFF1D => self.wave.frequency = (self.wave.frequency & 0x700) | value as u16,
            0xFF1E => self.wave.write_control(value, skips_length),
            0xFF20 => self.noise.length.load(value & 0x3F),
            0xFF21 => self.noise.write_envelope(value),
            0xFF22 => self.noise.nr43 = value,
            0xFF23 => self.noise.write_control(value, skips_length),
            0xFF24 => self.nr50 = value,
            0xFF25 => self.nr51 = value,
            _ => {}
        }
    }

    /// NR52 bit 7 turns the whole APU on or off. Turning it off clears every
    /// register except wave RAM (and, on DMG, the length counters).
    fn write_power(&mut self, value: u8) {
        let power_on = value & 0x80 != 0;
        if self.powered && !power_on {
            let wave_ram = self.wave.ram;
            let lengths = [
                self.square1.length.counter,
                self.square2.length.counter,
                self.wave.length.counter,
                self.noise.length.counter,
            ];
            self.square1 = SquareChannel::new(true);
            self.square2 = SquareChannel::new(false);
            self.wave = WaveChannel::new();
            self.noise = NoiseChannel::new();
            self.wave.ram = wave_ram;
            self.square1.length.counter = lengths[0];
            self.square2.length.counter = lengths[1];
            self.wave.length.counter = lengths[2];
            self.noise.length.counter = lengths[3];
            self.nr50 = 0;
            self.nr51 = 0;
        } else if !self.powered && power_on {
            self.frame_sequencer_step = 0;
        }
        self.powered = power_on;
    }
}

/// The DAC turns a channel's 0-15 value into a level from -1.0 to 1.0.
/// A disabled DAC outputs nothing.
fn dac(enabled: bool, digital: u8) -> f32 {
    if enabled {
        digital as f32 / 7.5 - 1.0
    } else {
        0.0
    }
}

/// Removes the constant (DC) part of the signal, so silence sits at 0.
fn high_pass(capacitor: &mut f32, input: f32, charge_factor: f32) -> f32 {
    let output = input - *capacitor;
    *capacitor = input - output * charge_factor;
    output
}

// -------------------------------------------------------------------
// Building blocks shared by the channels
// -------------------------------------------------------------------

/// Turns a channel off after a set time, if enabled (NRx1 and bit 6 of NRx4).
struct LengthCounter {
    enabled: bool,
    counter: u16,
    max: u16,
}

impl LengthCounter {
    fn new(max: u16) -> Self {
        LengthCounter {
            enabled: false,
            counter: 0,
            max,
        }
    }

    /// The game writes how many steps have already passed, so the counter is `max - value`.
    fn load(&mut self, value: u8) {
        self.counter = self.max - value as u16;
    }

    fn read_control(&self) -> u8 {
        0xBF | if self.enabled { 0x40 } else { 0 }
    }

    /// Returns true when the counter runs out and the channel should stop.
    fn clock(&mut self) -> bool {
        if self.enabled && self.counter > 0 {
            self.counter -= 1;
            return self.counter == 0;
        }
        false
    }

    /// Handles a write to NRx4. Returns false if the channel must be turned off.
    ///
    /// Two quirks: if the frame sequencer's next step won't clock the length
    /// counter, enabling the counter clocks it once right away, and a trigger
    /// that reloads an empty counter loses one step the same way.
    fn write_control(&mut self, value: u8, next_step_skips_length: bool) -> bool {
        let was_enabled = self.enabled;
        self.enabled = value & 0x40 != 0;
        let trigger = value & 0x80 != 0;
        let mut keep_playing = true;

        if next_step_skips_length && !was_enabled && self.enabled && self.counter > 0 {
            self.counter -= 1;
            if self.counter == 0 && !trigger {
                keep_playing = false;
            }
        }

        if trigger && self.counter == 0 {
            self.counter = self.max;
            if self.enabled && next_step_skips_length {
                self.counter -= 1;
            }
        }
        keep_playing
    }
}

/// Fades a channel's volume up or down over time (NRx2).
struct Envelope {
    register: u8,
    volume: u8,
    timer: u8,
}

impl Envelope {
    fn new() -> Self {
        Envelope {
            register: 0,
            volume: 0,
            timer: 0,
        }
    }

    fn read(&self) -> u8 {
        self.register
    }

    fn write(&mut self, value: u8) {
        self.register = value;
    }

    /// The DAC is on if the initial volume or the direction bit is set.
    fn dac_enabled(&self) -> bool {
        self.register & 0xF8 != 0
    }

    fn period(&self) -> u8 {
        self.register & 0x07
    }

    fn trigger(&mut self) {
        self.volume = self.register >> 4;
        self.timer = self.period();
    }

    fn clock(&mut self) {
        if self.period() == 0 {
            return;
        }
        if self.timer > 0 {
            self.timer -= 1;
        }
        if self.timer == 0 {
            self.timer = self.period();
            let increase = self.register & 0x08 != 0;
            if increase && self.volume < 15 {
                self.volume += 1;
            } else if !increase && self.volume > 0 {
                self.volume -= 1;
            }
        }
    }
}

/// Channel 1's frequency sweep (NR10): periodically raises or lowers the frequency.
struct Sweep {
    period: u8,
    negate: bool,
    shift: u8,
    timer: u8,
    enabled: bool,
    shadow_frequency: u16,
    /// Switching from subtract to add after a subtraction was calculated
    /// turns the channel off.
    negate_used: bool,
}

// -------------------------------------------------------------------
// Channels
// -------------------------------------------------------------------

/// Channels 1 and 2: a square wave with selectable duty cycle.
struct SquareChannel {
    enabled: bool,
    length: LengthCounter,
    envelope: Envelope,
    /// Only channel 1 has a sweep unit.
    sweep: Option<Sweep>,
    duty: u8,
    duty_step: u8,
    frequency: u16,
    timer: u16,
}

impl SquareChannel {
    fn new(has_sweep: bool) -> Self {
        let sweep = has_sweep.then_some(Sweep {
            period: 0,
            negate: false,
            shift: 0,
            timer: 0,
            enabled: false,
            shadow_frequency: 0,
            negate_used: false,
        });
        SquareChannel {
            enabled: false,
            length: LengthCounter::new(64),
            envelope: Envelope::new(),
            sweep,
            duty: 0,
            duty_step: 0,
            frequency: 0,
            timer: 0,
        }
    }

    fn dac_enabled(&self) -> bool {
        self.envelope.dac_enabled()
    }

    fn output(&self) -> u8 {
        if !self.enabled {
            return 0;
        }
        DUTY_PATTERNS[self.duty as usize][self.duty_step as usize] * self.envelope.volume
    }

    /// Advances the wave by one clock. One duty step lasts (2048 - frequency) * 4 clocks.
    fn tick(&mut self) {
        if self.timer > 0 {
            self.timer -= 1;
        }
        if self.timer == 0 {
            self.timer = (2048 - self.frequency) * 4;
            self.duty_step = (self.duty_step + 1) % 8;
        }
    }

    fn clock_length(&mut self) {
        if self.length.clock() {
            self.enabled = false;
        }
    }

    fn read_sweep(&self) -> u8 {
        let Some(sweep) = &self.sweep else {
            return 0xFF;
        };
        0x80 | (sweep.period << 4) | if sweep.negate { 0x08 } else { 0 } | sweep.shift
    }

    fn write_sweep(&mut self, value: u8) {
        let Some(sweep) = &mut self.sweep else {
            return;
        };
        sweep.period = (value >> 4) & 0x07;
        sweep.negate = value & 0x08 != 0;
        sweep.shift = value & 0x07;
        if !sweep.negate && sweep.negate_used {
            self.enabled = false;
        }
    }

    fn read_duty(&self) -> u8 {
        (self.duty << 6) | 0x3F
    }

    fn write_duty(&mut self, value: u8) {
        self.duty = value >> 6;
        self.length.load(value & 0x3F);
    }

    fn write_envelope(&mut self, value: u8) {
        self.envelope.write(value);
        if !self.dac_enabled() {
            self.enabled = false;
        }
    }

    fn write_frequency_low(&mut self, value: u8) {
        self.frequency = (self.frequency & 0x700) | value as u16;
    }

    fn write_control(&mut self, value: u8, next_step_skips_length: bool) {
        self.frequency = (self.frequency & 0xFF) | ((value as u16 & 0x07) << 8);
        if !self.length.write_control(value, next_step_skips_length) {
            self.enabled = false;
        }
        if value & 0x80 != 0 {
            self.trigger();
        }
    }

    fn trigger(&mut self) {
        self.enabled = self.dac_enabled();
        self.timer = (2048 - self.frequency) * 4;
        self.envelope.trigger();

        let frequency = self.frequency;
        if let Some(sweep) = &mut self.sweep {
            sweep.shadow_frequency = frequency;
            sweep.timer = if sweep.period == 0 { 8 } else { sweep.period };
            sweep.enabled = sweep.period != 0 || sweep.shift != 0;
            sweep.negate_used = false;
            if sweep.shift != 0 {
                self.calculate_sweep();
            }
        }
    }

    /// Computes the next sweep frequency. Going past 2047 turns the channel off.
    fn calculate_sweep(&mut self) -> u16 {
        let Some(sweep) = &mut self.sweep else {
            return self.frequency;
        };
        let delta = sweep.shadow_frequency >> sweep.shift;
        let new_frequency = if sweep.negate {
            sweep.negate_used = true;
            sweep.shadow_frequency - delta
        } else {
            sweep.shadow_frequency + delta
        };
        if new_frequency > 2047 {
            self.enabled = false;
        }
        new_frequency
    }

    fn clock_sweep(&mut self) {
        let Some(sweep) = &mut self.sweep else {
            return;
        };
        if sweep.timer > 0 {
            sweep.timer -= 1;
        }
        if sweep.timer != 0 {
            return;
        }
        sweep.timer = if sweep.period == 0 { 8 } else { sweep.period };
        if !sweep.enabled || sweep.period == 0 {
            return;
        }

        let shift = sweep.shift;
        let new_frequency = self.calculate_sweep();
        if new_frequency <= 2047 && shift != 0 {
            self.frequency = new_frequency;
            if let Some(sweep) = &mut self.sweep {
                sweep.shadow_frequency = new_frequency;
            }
            // Check again with the new frequency, only to detect overflow.
            self.calculate_sweep();
        }
    }
}

/// Channel 3: plays 32 4-bit samples from wave RAM.
struct WaveChannel {
    enabled: bool,
    dac_enabled: bool,
    length: LengthCounter,
    /// 0 = mute, 1 = 100%, 2 = 50%, 3 = 25%.
    volume_code: u8,
    frequency: u16,
    timer: u16,
    position: u8,
    sample: u8,
    /// Clocks since the channel last read a sample from wave RAM.
    clocks_since_fetch: u8,
    ram: [u8; 16],
}

impl WaveChannel {
    fn new() -> Self {
        WaveChannel {
            enabled: false,
            dac_enabled: false,
            length: LengthCounter::new(256),
            volume_code: 0,
            frequency: 0,
            timer: 0,
            position: 0,
            sample: 0,
            clocks_since_fetch: u8::MAX,
            ram: [0; 16],
        }
    }

    fn output(&self) -> u8 {
        if !self.enabled || self.volume_code == 0 {
            return 0;
        }
        self.sample >> (self.volume_code - 1)
    }

    /// Advances one clock. Each sample lasts (2048 - frequency) * 2 clocks.
    fn tick(&mut self) {
        self.clocks_since_fetch = self.clocks_since_fetch.saturating_add(1);
        if self.timer > 0 {
            self.timer -= 1;
        }
        if self.timer == 0 {
            self.timer = (2048 - self.frequency) * 2;
            self.clocks_since_fetch = 0;
            self.position = (self.position + 1) % 32;
            // Each byte holds two samples, high nibble first.
            let byte = self.ram[self.position as usize / 2];
            self.sample = if self.position.is_multiple_of(2) { byte >> 4 } else { byte & 0x0F };
        }
    }

    fn clock_length(&mut self) {
        if self.length.clock() {
            self.enabled = false;
        }
    }

    /// Which wave RAM byte a CPU access to `addr` reaches.
    ///
    /// While the channel plays, wave RAM is busy. On DMG the CPU only gets
    /// through in the moment the channel reads a sample, and then it reaches
    /// the byte the channel is reading, whatever address it used. The rest of
    /// the time reads give 0xFF and writes are lost.
    fn ram_index(&self, addr: u16) -> Option<usize> {
        if !self.enabled {
            return Some((addr - 0xFF30) as usize);
        }
        if self.clocks_since_fetch < WAVE_ACCESS_WINDOW {
            Some(self.position as usize / 2)
        } else {
            None
        }
    }

    /// Triggering the channel just as it's about to read a sample corrupts
    /// the start of wave RAM on DMG: the bytes it was about to read get
    /// copied over the first ones.
    fn corrupt_ram_on_retrigger(&mut self) {
        let next_byte = ((self.position as usize + 1) % 32) / 2;
        if next_byte < 4 {
            self.ram[0] = self.ram[next_byte];
        } else {
            let start = next_byte & !3;
            for i in 0..4 {
                self.ram[i] = self.ram[start + i];
            }
        }
    }

    fn write_dac(&mut self, value: u8) {
        self.dac_enabled = value & 0x80 != 0;
        if !self.dac_enabled {
            self.enabled = false;
        }
    }

    fn write_control(&mut self, value: u8, next_step_skips_length: bool) {
        self.frequency = (self.frequency & 0xFF) | ((value as u16 & 0x07) << 8);
        if !self.length.write_control(value, next_step_skips_length) {
            self.enabled = false;
        }
        if value & 0x80 != 0 {
            if self.enabled && self.timer <= RETRIGGER_CORRUPTION_WINDOW {
                self.corrupt_ram_on_retrigger();
            }
            self.enabled = self.dac_enabled;
            self.position = 0;
            // The first sample is fetched after a short delay.
            self.timer = (2048 - self.frequency) * 2 + 6;
        }
    }
}

/// Channel 4: pseudo-random noise from a linear feedback shift register.
struct NoiseChannel {
    enabled: bool,
    length: LengthCounter,
    envelope: Envelope,
    /// NR43: clock shift (bits 4-7), 7-bit mode (bit 3), divisor code (bits 0-2).
    nr43: u8,
    timer: u32,
    lfsr: u16,
}

impl NoiseChannel {
    fn new() -> Self {
        NoiseChannel {
            enabled: false,
            length: LengthCounter::new(64),
            envelope: Envelope::new(),
            nr43: 0,
            timer: 0,
            lfsr: 0,
        }
    }

    fn dac_enabled(&self) -> bool {
        self.envelope.dac_enabled()
    }

    fn output(&self) -> u8 {
        if !self.enabled {
            return 0;
        }
        // The output is the inverted lowest bit.
        (!self.lfsr & 0x01) as u8 * self.envelope.volume
    }

    fn period(&self) -> u32 {
        let divisor = match self.nr43 & 0x07 {
            0 => 8,
            code => code as u32 * 16,
        };
        divisor << (self.nr43 >> 4)
    }

    /// Advances one clock, shifting the LFSR once per period.
    fn tick(&mut self) {
        if self.timer > 0 {
            self.timer -= 1;
        }
        if self.timer == 0 {
            self.timer = self.period();
            let feedback = (self.lfsr ^ (self.lfsr >> 1)) & 0x01;
            self.lfsr = (self.lfsr >> 1) | (feedback << 14);
            // In 7-bit mode the feedback also goes into bit 6, for a shorter, more tonal pattern.
            if self.nr43 & 0x08 != 0 {
                self.lfsr = (self.lfsr & !0x40) | (feedback << 6);
            }
        }
    }

    fn clock_length(&mut self) {
        if self.length.clock() {
            self.enabled = false;
        }
    }

    fn write_envelope(&mut self, value: u8) {
        self.envelope.write(value);
        if !self.dac_enabled() {
            self.enabled = false;
        }
    }

    fn write_control(&mut self, value: u8, next_step_skips_length: bool) {
        if !self.length.write_control(value, next_step_skips_length) {
            self.enabled = false;
        }
        if value & 0x80 != 0 {
            self.enabled = self.dac_enabled();
            self.lfsr = 0x7FFF;
            self.timer = self.period();
            self.envelope.trigger();
        }
    }
}
