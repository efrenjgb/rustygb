use crate::bus::Bus;

// Flag bits in the F register.
const FLAG_Z: u8 = 0x80; // zero
const FLAG_N: u8 = 0x40; // subtract
const FLAG_H: u8 = 0x20; // half carry
const FLAG_C: u8 = 0x10; // carry

/// The Sharp LR35902 CPU.
///
/// Timing is accurate to the machine cycle (4 clocks): every memory access
/// goes through `bus.read_cycle` / `bus.write_cycle`, and cycles where the
/// CPU is busy internally call `bus.tick()`. That keeps the rest of the
/// hardware in step with the CPU, so instruction lengths come out right on
/// their own instead of from a table.
pub struct Cpu {
    pub a: u8,
    pub f: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub h: u8,
    pub l: u8,
    pub sp: u16,
    pub pc: u16,
    /// Interrupt master enable.
    pub ime: bool,
    /// EI enables interrupts only after the next instruction.
    ime_pending: bool,
    pub halted: bool,
    /// HALT with interrupts disabled and one already pending doesn't halt.
    /// Instead the CPU fails to advance PC past the next opcode, so it runs twice.
    halt_bug: bool,
}

impl Cpu {
    /// Creates a CPU in the state the boot ROM leaves it in.
    pub fn new() -> Self {
        Cpu {
            a: 0x01,
            f: 0xB0,
            b: 0x00,
            c: 0x13,
            d: 0x00,
            e: 0xD8,
            h: 0x01,
            l: 0x4D,
            sp: 0xFFFE,
            pc: 0x0100,
            ime: false,
            ime_pending: false,
            halted: false,
            halt_bug: false,
        }
    }

    /// Creates a CPU in its power-on state, ready to run a boot ROM from 0x0000.
    pub fn power_on() -> Self {
        Cpu {
            a: 0,
            f: 0,
            b: 0,
            c: 0,
            d: 0,
            e: 0,
            h: 0,
            l: 0,
            sp: 0,
            pc: 0,
            ..Cpu::new()
        }
    }

    // ---------------------------------------------------------------
    // Register helpers
    // ---------------------------------------------------------------

    pub fn bc(&self) -> u16 {
        u16::from_be_bytes([self.b, self.c])
    }

    pub fn de(&self) -> u16 {
        u16::from_be_bytes([self.d, self.e])
    }

    pub fn hl(&self) -> u16 {
        u16::from_be_bytes([self.h, self.l])
    }

    pub fn af(&self) -> u16 {
        u16::from_be_bytes([self.a, self.f])
    }

    fn set_bc(&mut self, value: u16) {
        [self.b, self.c] = value.to_be_bytes();
    }

    fn set_de(&mut self, value: u16) {
        [self.d, self.e] = value.to_be_bytes();
    }

    fn set_hl(&mut self, value: u16) {
        [self.h, self.l] = value.to_be_bytes();
    }

    fn set_af(&mut self, value: u16) {
        [self.a, self.f] = value.to_be_bytes();
        self.f &= 0xF0; // the low 4 bits of F are always zero
    }

    fn flag(&self, flag: u8) -> bool {
        self.f & flag != 0
    }

    fn set_flag(&mut self, flag: u8, on: bool) {
        if on {
            self.f |= flag;
        } else {
            self.f &= !flag;
        }
    }

    fn set_flags(&mut self, z: bool, n: bool, h: bool, c: bool) {
        self.f = 0;
        self.set_flag(FLAG_Z, z);
        self.set_flag(FLAG_N, n);
        self.set_flag(FLAG_H, h);
        self.set_flag(FLAG_C, c);
    }

    /// Many opcodes encode an 8-bit register in 3 bits:
    /// 0=B 1=C 2=D 3=E 4=H 5=L 6=(HL) 7=A
    fn read_r8(&self, bus: &mut Bus, index: u8) -> u8 {
        match index {
            0 => self.b,
            1 => self.c,
            2 => self.d,
            3 => self.e,
            4 => self.h,
            5 => self.l,
            6 => bus.read_cycle(self.hl()),
            _ => self.a,
        }
    }

    fn write_r8(&mut self, bus: &mut Bus, index: u8, value: u8) {
        match index {
            0 => self.b = value,
            1 => self.c = value,
            2 => self.d = value,
            3 => self.e = value,
            4 => self.h = value,
            5 => self.l = value,
            6 => bus.write_cycle(self.hl(), value),
            _ => self.a = value,
        }
    }

    /// 16-bit register in 2 bits, as used by LD/INC/DEC/ADD: 0=BC 1=DE 2=HL 3=SP
    fn read_r16(&self, index: u8) -> u16 {
        match index {
            0 => self.bc(),
            1 => self.de(),
            2 => self.hl(),
            _ => self.sp,
        }
    }

    fn write_r16(&mut self, index: u8, value: u16) {
        match index {
            0 => self.set_bc(value),
            1 => self.set_de(value),
            2 => self.set_hl(value),
            _ => self.sp = value,
        }
    }

    /// Branch condition in 2 bits: 0=NZ 1=Z 2=NC 3=C
    fn condition(&self, index: u8) -> bool {
        match index {
            0 => !self.flag(FLAG_Z),
            1 => self.flag(FLAG_Z),
            2 => !self.flag(FLAG_C),
            _ => self.flag(FLAG_C),
        }
    }

    // ---------------------------------------------------------------
    // Memory helpers (each read or write is one machine cycle)
    // ---------------------------------------------------------------

    fn fetch_byte(&mut self, bus: &mut Bus) -> u8 {
        let value = bus.read_cycle(self.pc);
        self.pc = self.pc.wrapping_add(1);
        value
    }

    fn fetch_word(&mut self, bus: &mut Bus) -> u16 {
        let low = self.fetch_byte(bus);
        let high = self.fetch_byte(bus);
        u16::from_le_bytes([low, high])
    }

    /// Takes 3 machine cycles: one internal while SP is decremented, then two writes.
    fn push(&mut self, bus: &mut Bus, value: u16) {
        let [high, low] = value.to_be_bytes();
        bus.increment_cycle(self.sp);
        self.sp = self.sp.wrapping_sub(1);
        bus.write_cycle(self.sp, high);
        self.sp = self.sp.wrapping_sub(1);
        bus.write_cycle(self.sp, low);
    }

    fn pop(&mut self, bus: &mut Bus) -> u16 {
        let low = bus.read_increment_cycle(self.sp);
        self.sp = self.sp.wrapping_add(1);
        let high = bus.read_cycle(self.sp);
        self.sp = self.sp.wrapping_add(1);
        u16::from_le_bytes([low, high])
    }

    /// Jumps to a relative offset. Taking the jump costs one extra cycle.
    fn jump_relative(&mut self, bus: &mut Bus, offset: u8) {
        self.pc = self.pc.wrapping_add_signed(offset as i8 as i16);
        bus.tick();
    }

    // ---------------------------------------------------------------
    // Execution
    // ---------------------------------------------------------------

    /// Runs one instruction (or services an interrupt, or waits one cycle
    /// while halted) and returns the clock cycles used.
    pub fn step(&mut self, bus: &mut Bus) -> u32 {
        let start = bus.cycles;

        // EI takes effect after the instruction that follows it, so this
        // step still uses the old value.
        let interrupts_enabled = self.ime;
        if self.ime_pending {
            self.ime_pending = false;
            self.ime = true;
        }

        // Opcode fetch. While halted the CPU keeps fetching the next opcode
        // without running it, until an interrupt is pending (even with IME off).
        let opcode = bus.read_cycle(self.pc);
        if self.halted {
            if bus.pending_interrupts() == 0 {
                return (bus.cycles - start) as u32;
            }
            self.halted = false;
        }

        // Interrupts are checked at the end of the opcode fetch. If one is
        // pending the fetched opcode is thrown away and the interrupt is serviced instead.
        if interrupts_enabled && bus.pending_interrupts() != 0 {
            self.service_interrupt(bus);
        } else {
            if self.halt_bug {
                self.halt_bug = false;
            } else {
                self.pc = self.pc.wrapping_add(1);
            }
            self.execute(bus, opcode);
        }

        (bus.cycles - start) as u32
    }

    /// Calls the handler of the highest priority pending interrupt. Together
    /// with the discarded opcode fetch this takes 5 machine cycles.
    fn service_interrupt(&mut self, bus: &mut Bus) {
        self.ime = false;

        bus.increment_cycle(self.sp);
        let [high, low] = self.pc.to_be_bytes();
        self.sp = self.sp.wrapping_sub(1);
        bus.write_cycle(self.sp, high);

        // The interrupt is chosen only now. If pushing the high byte
        // overwrote IE (SP was 0x0000), the interrupt can get cancelled,
        // in which case the CPU jumps to 0x0000.
        let pending = bus.pending_interrupts();

        self.sp = self.sp.wrapping_sub(1);
        bus.write_cycle(self.sp, low);

        if pending == 0 {
            self.pc = 0x0000;
        } else {
            // Lowest bit has highest priority: VBlank, STAT, Timer, Serial, Joypad.
            let bit = pending.trailing_zeros() as u16;
            bus.interrupt_flag &= !(1 << bit);
            self.pc = 0x0040 + bit * 8;
        }
        bus.tick();
    }

    fn execute(&mut self, bus: &mut Bus, opcode: u8) {
        // Most opcodes keep their operands in these bit fields.
        let r8_dst = (opcode >> 3) & 0x07;
        let r8_src = opcode & 0x07;
        let r16 = (opcode >> 4) & 0x03;
        let cond = (opcode >> 3) & 0x03;

        match opcode {
            // NOP
            0x00 => {}

            // LD rr,d16
            0x01 | 0x11 | 0x21 | 0x31 => {
                let value = self.fetch_word(bus);
                self.write_r16(r16, value);
            }

            // LD (BC),A / LD (DE),A
            0x02 => bus.write_cycle(self.bc(), self.a),
            0x12 => bus.write_cycle(self.de(), self.a),
            // LD (HL+),A / LD (HL-),A
            0x22 => {
                let hl = self.hl();
                bus.write_cycle(hl, self.a);
                self.set_hl(hl.wrapping_add(1));
            }
            0x32 => {
                let hl = self.hl();
                bus.write_cycle(hl, self.a);
                self.set_hl(hl.wrapping_sub(1));
            }

            // LD A,(BC) / LD A,(DE)
            0x0A => self.a = bus.read_cycle(self.bc()),
            0x1A => self.a = bus.read_cycle(self.de()),
            // LD A,(HL+) / LD A,(HL-)
            0x2A => {
                let hl = self.hl();
                self.a = bus.read_increment_cycle(hl);
                self.set_hl(hl.wrapping_add(1));
            }
            0x3A => {
                let hl = self.hl();
                self.a = bus.read_increment_cycle(hl);
                self.set_hl(hl.wrapping_sub(1));
            }

            // INC rr / DEC rr (no flags, one extra cycle)
            0x03 | 0x13 | 0x23 | 0x33 => {
                let value = self.read_r16(r16);
                bus.increment_cycle(value);
                self.write_r16(r16, value.wrapping_add(1));
            }
            0x0B | 0x1B | 0x2B | 0x3B => {
                let value = self.read_r16(r16);
                bus.increment_cycle(value);
                self.write_r16(r16, value.wrapping_sub(1));
            }

            // INC r
            0x04 | 0x0C | 0x14 | 0x1C | 0x24 | 0x2C | 0x34 | 0x3C => {
                let value = self.read_r8(bus, r8_dst);
                let result = value.wrapping_add(1);
                self.write_r8(bus, r8_dst, result);
                self.set_flag(FLAG_Z, result == 0);
                self.set_flag(FLAG_N, false);
                self.set_flag(FLAG_H, value & 0x0F == 0x0F);
            }

            // DEC r
            0x05 | 0x0D | 0x15 | 0x1D | 0x25 | 0x2D | 0x35 | 0x3D => {
                let value = self.read_r8(bus, r8_dst);
                let result = value.wrapping_sub(1);
                self.write_r8(bus, r8_dst, result);
                self.set_flag(FLAG_Z, result == 0);
                self.set_flag(FLAG_N, true);
                self.set_flag(FLAG_H, value & 0x0F == 0);
            }

            // LD r,d8
            0x06 | 0x0E | 0x16 | 0x1E | 0x26 | 0x2E | 0x36 | 0x3E => {
                let value = self.fetch_byte(bus);
                self.write_r8(bus, r8_dst, value);
            }

            // Rotates on A. Unlike the CB versions these always clear Z.
            0x07 => {
                self.a = self.rlc(self.a);
                self.set_flag(FLAG_Z, false);
            }
            0x0F => {
                self.a = self.rrc(self.a);
                self.set_flag(FLAG_Z, false);
            }
            0x17 => {
                self.a = self.rl(self.a);
                self.set_flag(FLAG_Z, false);
            }
            0x1F => {
                self.a = self.rr(self.a);
                self.set_flag(FLAG_Z, false);
            }

            // LD (a16),SP
            0x08 => {
                let addr = self.fetch_word(bus);
                let [high, low] = self.sp.to_be_bytes();
                bus.write_cycle(addr, low);
                bus.write_cycle(addr.wrapping_add(1), high);
            }

            // ADD HL,rr
            0x09 | 0x19 | 0x29 | 0x39 => {
                let hl = self.hl();
                let value = self.read_r16(r16);
                let (result, carry) = hl.overflowing_add(value);
                self.set_flag(FLAG_N, false);
                self.set_flag(FLAG_H, (hl & 0x0FFF) + (value & 0x0FFF) > 0x0FFF);
                self.set_flag(FLAG_C, carry);
                self.set_hl(result);
                bus.tick();
            }

            // STOP (treated like a 2 byte NOP)
            0x10 => {
                self.fetch_byte(bus);
            }

            // JR r8
            0x18 => {
                let offset = self.fetch_byte(bus);
                self.jump_relative(bus, offset);
            }
            // JR cc,r8
            0x20 | 0x28 | 0x30 | 0x38 => {
                let offset = self.fetch_byte(bus);
                if self.condition(cond) {
                    self.jump_relative(bus, offset);
                }
            }

            // DAA: fix up A after a BCD addition or subtraction
            0x27 => {
                let mut adjust = 0;
                let mut carry = self.flag(FLAG_C);
                if self.flag(FLAG_N) {
                    if self.flag(FLAG_H) {
                        adjust |= 0x06;
                    }
                    if carry {
                        adjust |= 0x60;
                    }
                    self.a = self.a.wrapping_sub(adjust);
                } else {
                    if self.flag(FLAG_H) || self.a & 0x0F > 0x09 {
                        adjust |= 0x06;
                    }
                    if carry || self.a > 0x99 {
                        adjust |= 0x60;
                        carry = true;
                    }
                    self.a = self.a.wrapping_add(adjust);
                }
                self.set_flag(FLAG_Z, self.a == 0);
                self.set_flag(FLAG_H, false);
                self.set_flag(FLAG_C, carry);
            }

            // CPL
            0x2F => {
                self.a = !self.a;
                self.set_flag(FLAG_N, true);
                self.set_flag(FLAG_H, true);
            }
            // SCF
            0x37 => {
                self.set_flag(FLAG_N, false);
                self.set_flag(FLAG_H, false);
                self.set_flag(FLAG_C, true);
            }
            // CCF
            0x3F => {
                let carry = self.flag(FLAG_C);
                self.set_flag(FLAG_N, false);
                self.set_flag(FLAG_H, false);
                self.set_flag(FLAG_C, !carry);
            }

            // HALT: sleep until an interrupt is pending
            0x76 => {
                if !self.ime && bus.pending_interrupts() != 0 {
                    self.halt_bug = true;
                } else {
                    self.halted = true;
                }
            }

            // LD r,r
            0x40..=0x7F => {
                let value = self.read_r8(bus, r8_src);
                self.write_r8(bus, r8_dst, value);
            }

            // ADD/ADC/SUB/SBC/AND/XOR/OR/CP A,r
            0x80..=0xBF => {
                let value = self.read_r8(bus, r8_src);
                self.alu(r8_dst, value);
            }

            // Same operations with an immediate value
            0xC6 | 0xCE | 0xD6 | 0xDE | 0xE6 | 0xEE | 0xF6 | 0xFE => {
                let value = self.fetch_byte(bus);
                self.alu(r8_dst, value);
            }

            // RET cc: checking the condition takes a cycle
            0xC0 | 0xC8 | 0xD0 | 0xD8 => {
                bus.tick();
                if self.condition(cond) {
                    self.pc = self.pop(bus);
                    bus.tick();
                }
            }
            // RET
            0xC9 => {
                self.pc = self.pop(bus);
                bus.tick();
            }
            // RETI
            0xD9 => {
                self.pc = self.pop(bus);
                bus.tick();
                self.ime = true;
            }

            // POP rr
            0xC1 | 0xD1 | 0xE1 => {
                let value = self.pop(bus);
                self.write_r16(r16, value);
            }
            0xF1 => {
                let value = self.pop(bus);
                self.set_af(value);
            }

            // PUSH rr
            0xC5 | 0xD5 | 0xE5 => {
                let value = self.read_r16(r16);
                self.push(bus, value);
            }
            0xF5 => self.push(bus, self.af()),

            // JP a16
            0xC3 => {
                self.pc = self.fetch_word(bus);
                bus.tick();
            }
            // JP cc,a16
            0xC2 | 0xCA | 0xD2 | 0xDA => {
                let addr = self.fetch_word(bus);
                if self.condition(cond) {
                    self.pc = addr;
                    bus.tick();
                }
            }
            // JP HL
            0xE9 => self.pc = self.hl(),

            // CALL a16
            0xCD => {
                let addr = self.fetch_word(bus);
                self.push(bus, self.pc);
                self.pc = addr;
            }
            // CALL cc,a16
            0xC4 | 0xCC | 0xD4 | 0xDC => {
                let addr = self.fetch_word(bus);
                if self.condition(cond) {
                    self.push(bus, self.pc);
                    self.pc = addr;
                }
            }

            // RST n: call to a fixed address 0x00, 0x08, ... 0x38
            0xC7 | 0xCF | 0xD7 | 0xDF | 0xE7 | 0xEF | 0xF7 | 0xFF => {
                self.push(bus, self.pc);
                self.pc = (opcode & 0x38) as u16;
            }

            // CB prefix: bit operations
            0xCB => {
                let cb_opcode = self.fetch_byte(bus);
                self.execute_cb(bus, cb_opcode);
            }

            // LDH (a8),A / LDH A,(a8): access 0xFF00 + a8
            0xE0 => {
                let offset = self.fetch_byte(bus);
                bus.write_cycle(0xFF00 | offset as u16, self.a);
            }
            0xF0 => {
                let offset = self.fetch_byte(bus);
                self.a = bus.read_cycle(0xFF00 | offset as u16);
            }
            // LD (C),A / LD A,(C): access 0xFF00 + C
            0xE2 => bus.write_cycle(0xFF00 | self.c as u16, self.a),
            0xF2 => self.a = bus.read_cycle(0xFF00 | self.c as u16),

            // LD (a16),A / LD A,(a16)
            0xEA => {
                let addr = self.fetch_word(bus);
                bus.write_cycle(addr, self.a);
            }
            0xFA => {
                let addr = self.fetch_word(bus);
                self.a = bus.read_cycle(addr);
            }

            // ADD SP,r8
            0xE8 => {
                self.sp = self.sp_plus_offset(bus);
                bus.tick();
                bus.tick();
            }
            // LD HL,SP+r8
            0xF8 => {
                let value = self.sp_plus_offset(bus);
                self.set_hl(value);
                bus.tick();
            }
            // LD SP,HL
            0xF9 => {
                self.sp = self.hl();
                bus.tick();
            }

            // DI / EI
            0xF3 => {
                self.ime = false;
                self.ime_pending = false;
            }
            0xFB => self.ime_pending = true,

            // 0xD3, 0xDB, 0xDD, 0xE3, 0xE4, 0xEB, 0xEC, 0xED, 0xF4, 0xFC, 0xFD
            _ => panic!(
                "illegal opcode {:#04X} at {:#06X}",
                opcode,
                self.pc.wrapping_sub(1)
            ),
        }
    }

    fn execute_cb(&mut self, bus: &mut Bus, opcode: u8) {
        let reg = opcode & 0x07;
        let bit = (opcode >> 3) & 0x07;
        let value = self.read_r8(bus, reg);

        match opcode {
            // Rotates and shifts
            0x00..=0x3F => {
                let result = match bit {
                    0 => self.rlc(value),
                    1 => self.rrc(value),
                    2 => self.rl(value),
                    3 => self.rr(value),
                    4 => self.sla(value),
                    5 => self.sra(value),
                    6 => self.swap(value),
                    _ => self.srl(value),
                };
                self.write_r8(bus, reg, result);
            }
            // BIT b,r: test a bit
            0x40..=0x7F => {
                self.set_flag(FLAG_Z, value & (1 << bit) == 0);
                self.set_flag(FLAG_N, false);
                self.set_flag(FLAG_H, true);
            }
            // RES b,r: clear a bit
            0x80..=0xBF => self.write_r8(bus, reg, value & !(1 << bit)),
            // SET b,r: set a bit
            0xC0..=0xFF => self.write_r8(bus, reg, value | (1 << bit)),
        }
    }

    // ---------------------------------------------------------------
    // Arithmetic and logic
    // ---------------------------------------------------------------

    /// 8-bit ALU operation on A: 0=ADD 1=ADC 2=SUB 3=SBC 4=AND 5=XOR 6=OR 7=CP
    fn alu(&mut self, op: u8, value: u8) {
        let a = self.a;
        let carry_in = if self.flag(FLAG_C) { 1 } else { 0 };
        match op {
            0 | 1 => {
                // ADD / ADC
                let carry = if op == 1 { carry_in } else { 0 };
                let result = a as u16 + value as u16 + carry as u16;
                let half = (a & 0x0F) + (value & 0x0F) + carry > 0x0F;
                self.a = result as u8;
                self.set_flags(self.a == 0, false, half, result > 0xFF);
            }
            2 | 3 | 7 => {
                // SUB / SBC / CP
                let carry = if op == 3 { carry_in } else { 0 };
                let result = a.wrapping_sub(value).wrapping_sub(carry);
                let half = (a & 0x0F) < (value & 0x0F) + carry;
                let borrow = (a as u16) < value as u16 + carry as u16;
                self.set_flags(result == 0, true, half, borrow);
                if op != 7 {
                    self.a = result;
                }
            }
            4 => {
                self.a = a & value;
                self.set_flags(self.a == 0, false, true, false);
            }
            5 => {
                self.a = a ^ value;
                self.set_flags(self.a == 0, false, false, false);
            }
            _ => {
                self.a = a | value;
                self.set_flags(self.a == 0, false, false, false);
            }
        }
    }

    /// Reads a signed byte and adds it to SP. Used by ADD SP,r8 and LD HL,SP+r8.
    /// Flags H and C come from the unsigned addition of the low byte.
    fn sp_plus_offset(&mut self, bus: &mut Bus) -> u16 {
        let offset = self.fetch_byte(bus);
        let sp = self.sp;
        let half = (sp & 0x0F) + (offset as u16 & 0x0F) > 0x0F;
        let carry = (sp & 0xFF) + offset as u16 > 0xFF;
        self.set_flags(false, false, half, carry);
        sp.wrapping_add_signed(offset as i8 as i16)
    }

    fn rlc(&mut self, value: u8) -> u8 {
        let result = value.rotate_left(1);
        self.set_flags(result == 0, false, false, value & 0x80 != 0);
        result
    }

    fn rrc(&mut self, value: u8) -> u8 {
        let result = value.rotate_right(1);
        self.set_flags(result == 0, false, false, value & 0x01 != 0);
        result
    }

    fn rl(&mut self, value: u8) -> u8 {
        let carry_in = self.flag(FLAG_C) as u8;
        let result = (value << 1) | carry_in;
        self.set_flags(result == 0, false, false, value & 0x80 != 0);
        result
    }

    fn rr(&mut self, value: u8) -> u8 {
        let carry_in = self.flag(FLAG_C) as u8;
        let result = (value >> 1) | (carry_in << 7);
        self.set_flags(result == 0, false, false, value & 0x01 != 0);
        result
    }

    fn sla(&mut self, value: u8) -> u8 {
        let result = value << 1;
        self.set_flags(result == 0, false, false, value & 0x80 != 0);
        result
    }

    fn sra(&mut self, value: u8) -> u8 {
        let result = (value >> 1) | (value & 0x80);
        self.set_flags(result == 0, false, false, value & 0x01 != 0);
        result
    }

    fn swap(&mut self, value: u8) -> u8 {
        let result = value.rotate_left(4);
        self.set_flags(result == 0, false, false, false);
        result
    }

    fn srl(&mut self, value: u8) -> u8 {
        let result = value >> 1;
        self.set_flags(result == 0, false, false, value & 0x01 != 0);
        result
    }
}
