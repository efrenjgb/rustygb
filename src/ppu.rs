use crate::bus::{INT_STAT, INT_VBLANK};

pub const SCREEN_WIDTH: usize = 160;
pub const SCREEN_HEIGHT: usize = 144;

const DOTS_PER_LINE: u16 = 456;
const LINES_PER_FRAME: u8 = 154;
const OAM_SCAN_DOTS: u16 = 80;
/// Mode 3 takes at least this long. Scrolling, the window and sprites make it longer.
const MIN_DRAWING_DOTS: u16 = 172;

/// The kinds of CPU access that trigger the OAM corruption bug.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OamBugAccess {
    Read,
    /// A write, or a 16-bit increment or decrement of an address in OAM.
    Write,
    /// A read and an increment in the same cycle, like `LD A,(HL+)` and `POP`.
    ReadIncrement,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    HBlank = 0,
    VBlank = 1,
    OamScan = 2,
    Drawing = 3,
}

/// The picture processing unit. It owns video RAM and OAM, walks through
/// the 154 lines of each frame one dot (clock) at a time, and draws each
/// line into `framebuffer` when that line's drawing mode ends.
///
/// Each line is 456 dots: OAM scan (mode 2, 80 dots), drawing (mode 3,
/// 172-289 dots) and horizontal blank (mode 0, the rest). Lines 144-153
/// are vertical blank (mode 1).
pub struct Ppu {
    pub vram: [u8; 0x2000],
    pub oam: [u8; 0xA0],
    /// One shade per pixel, 0 (lightest) to 3 (darkest).
    pub framebuffer: [u8; SCREEN_WIDTH * SCREEN_HEIGHT],
    /// Set when vblank starts, meaning `framebuffer` holds a complete frame.
    pub frame_ready: bool,

    lcdc: u8,
    /// Only the interrupt enable bits (3-6) are stored here.
    stat: u8,
    scy: u8,
    scx: u8,
    /// The LY register. Usually equal to `line`, except at the end of line 153.
    ly: u8,
    lyc: u8,
    bgp: u8,
    obp0: u8,
    obp1: u8,
    wy: u8,
    wx: u8,

    /// The mode the PPU is really in. Drives rendering and the STAT interrupt.
    mode: Mode,
    /// The mode reported in STAT, which also decides when the CPU can access
    /// VRAM and OAM. It changes a little after `mode`, see `calculate_visible_mode`.
    visible_mode: Mode,
    /// The line being drawn, 0..154.
    line: u8,
    /// Position within the current line, 0..456.
    dot: u16,
    /// How long mode 3 takes on the current line.
    drawing_dots: u16,
    /// The window has its own line counter, which only advances on lines where it was drawn.
    window_line: u8,
    /// The first frame after the LCD is switched on is not shown on a real
    /// DMG. The PPU still runs as normal, but the screen stays blank.
    first_frame_after_enable: bool,
    /// The first line after the LCD is switched on is different: it starts
    /// 4 dots in, skips the OAM scan and starts drawing 2 dots late.
    first_line_after_enable: bool,
    /// The LY=LYC comparison result shown in STAT. It's only updated while
    /// the LCD is on, so it keeps its value while the LCD is off.
    lyc_flag: bool,
    /// All STAT interrupt sources ORed together. The interrupt is only
    /// requested when this goes from low to high, so one source being active
    /// blocks the others from triggering.
    stat_line: bool,
}

impl Ppu {
    pub fn new() -> Self {
        Ppu {
            vram: [0; 0x2000],
            oam: [0; 0xA0],
            framebuffer: [0; SCREEN_WIDTH * SCREEN_HEIGHT],
            frame_ready: false,
            lcdc: 0x91,
            stat: 0,
            scy: 0,
            scx: 0,
            ly: 0,
            lyc: 0,
            bgp: 0xFC,
            obp0: 0xFF,
            obp1: 0xFF,
            wy: 0,
            wx: 0,
            mode: Mode::OamScan,
            visible_mode: Mode::OamScan,
            line: 0,
            dot: 0,
            drawing_dots: MIN_DRAWING_DOTS,
            window_line: 0,
            first_frame_after_enable: false,
            first_line_after_enable: false,
            lyc_flag: false,
            stat_line: false,
        }
    }

    fn lcd_enabled(&self) -> bool {
        self.lcdc & 0x80 != 0
    }

    /// VRAM can't be used by the CPU while the PPU is drawing. It's blocked as
    /// soon as the PPU starts drawing, until STAT reports that it has finished.
    pub fn vram_accessible(&self) -> bool {
        self.mode != Mode::Drawing && self.visible_mode != Mode::Drawing
    }

    /// OAM can't be used by the CPU while the PPU is scanning or drawing.
    pub fn oam_accessible(&self) -> bool {
        let busy = |mode| mode == Mode::OamScan || mode == Mode::Drawing;
        !busy(self.mode) && !busy(self.visible_mode)
    }

    /// Writes land a little later in the machine cycle than reads, so they
    /// get through at a few points where reads are still blocked.
    pub fn vram_writable(&self) -> bool {
        self.visible_mode != Mode::Drawing
    }

    pub fn oam_writable(&self) -> bool {
        let scanning = self.mode == Mode::OamScan && self.visible_mode == Mode::OamScan;
        !scanning && self.visible_mode != Mode::Drawing
    }

    /// The OAM corruption bug. During OAM scan the PPU reads OAM one 8-byte
    /// row per machine cycle. If the CPU puts an address in FE00-FEFF on the
    /// bus at the same time (to read, write, or just to increment or
    /// decrement a 16-bit register), the row the PPU is reading gets mixed
    /// with the row before it.
    pub fn trigger_oam_bug(&mut self, access: OamBugAccess, addr: u16) {
        if !(0xFE00..=0xFEFF).contains(&addr) || self.mode != Mode::OamScan {
            return;
        }
        // The first row has no row before it, so it's never corrupted.
        let row = self.dot as usize / 4;
        if row == 0 {
            return;
        }

        if access == OamBugAccess::ReadIncrement && (4..19).contains(&row) {
            // The row before is mixed with the rows around it, then copied
            // over the current row and the one two rows back.
            for i in 0..2 {
                let a = self.oam[(row - 2) * 8 + i];
                let b = self.oam[(row - 1) * 8 + i];
                let c = self.oam[row * 8 + i];
                let d = self.oam[(row - 1) * 8 + 4 + i];
                self.oam[(row - 1) * 8 + i] = (b & (a | c | d)) | (a & c & d);
            }
            for i in 0..8 {
                let value = self.oam[(row - 1) * 8 + i];
                self.oam[row * 8 + i] = value;
                self.oam[(row - 2) * 8 + i] = value;
            }
        }

        // The first word (2 bytes) of the row is mixed with the first and
        // third words of the row before. The rest of the row is copied from
        // the row before. The mixing works bit by bit, so it can be done a byte at a time.
        for i in 0..2 {
            let a = self.oam[row * 8 + i];
            let b = self.oam[(row - 1) * 8 + i];
            let c = self.oam[(row - 1) * 8 + 4 + i];
            if access == OamBugAccess::Write {
                self.oam[row * 8 + i] = ((a ^ c) & (b ^ c)) ^ c;
            } else {
                let value = b | (a & c);
                self.oam[row * 8 + i] = value;
                self.oam[(row - 1) * 8 + i] = value;
            }
        }
        for i in 2..8 {
            self.oam[row * 8 + i] = self.oam[(row - 1) * 8 + i];
        }
    }

    /// Compares LY with LYC. The comparison runs 4 dots behind LY: for the
    /// first 4 dots of a line nothing matches yet. On line 153, where LY
    /// already reads 0 from dot 4, the comparison still sees 153 for dots 4-7
    /// and 0 after that, so both LYC=153 and LYC=0 get their match.
    fn update_lyc_flag(&mut self) {
        if !self.lcd_enabled() {
            return;
        }
        let compared_ly = if self.dot < 4 {
            None
        } else if self.line == 153 && self.dot < 8 {
            Some(153)
        } else {
            Some(self.ly)
        };
        self.lyc_flag = compared_ly == Some(self.lyc);
    }

    /// The dot where mode 3 starts on the current line.
    fn drawing_start(&self) -> u16 {
        if self.first_line_after_enable {
            OAM_SCAN_DOTS + 2
        } else {
            OAM_SCAN_DOTS
        }
    }

    pub fn read_register(&self, addr: u16) -> u8 {
        match addr {
            0xFF40 => self.lcdc,
            0xFF41 => {
                let coincidence = if self.lyc_flag { 0x04 } else { 0 };
                0x80 | self.stat | coincidence | self.visible_mode as u8
            }
            0xFF42 => self.scy,
            0xFF43 => self.scx,
            0xFF44 => self.ly,
            0xFF45 => self.lyc,
            0xFF47 => self.bgp,
            0xFF48 => self.obp0,
            0xFF49 => self.obp1,
            0xFF4A => self.wy,
            0xFF4B => self.wx,
            _ => 0xFF,
        }
    }

    pub fn write_register(&mut self, addr: u16, value: u8) -> u8 {
        match addr {
            0xFF40 => {
                let was_enabled = self.lcd_enabled();
                self.lcdc = value;
                if was_enabled && !self.lcd_enabled() {
                    // Turning the LCD off blanks the screen and resets the
                    // PPU to the top of the screen.
                    self.framebuffer.fill(0);
                    self.line = 0;
                    self.ly = 0;
                    self.dot = 0;
                    self.mode = Mode::HBlank;
                    self.visible_mode = Mode::HBlank;
                    self.window_line = 0;
                } else if !was_enabled && self.lcd_enabled() {
                    self.first_line_after_enable = true;
                    self.first_frame_after_enable = true;
                    self.dot = 4;
                }
            }
            0xFF41 => self.stat = value & 0x78,
            0xFF42 => self.scy = value,
            0xFF43 => self.scx = value,
            0xFF44 => {} // LY is read only
            0xFF45 => self.lyc = value,
            0xFF47 => self.bgp = value,
            0xFF48 => self.obp0 = value,
            0xFF49 => self.obp1 = value,
            0xFF4A => self.wy = value,
            0xFF4B => self.wx = value,
            _ => {}
        }
        // Changing LYC, the STAT enables or switching the LCD on can trigger
        // a STAT interrupt right away.
        self.update_lyc_flag();
        self.update_stat_line()
    }

    /// Advances the PPU by some dots and returns any interrupt flags raised.
    pub fn tick(&mut self, dots: u32) -> u8 {
        let mut interrupts = 0;
        if self.lcd_enabled() {
            for _ in 0..dots {
                interrupts |= self.tick_dot();
            }
        }
        interrupts
    }

    fn tick_dot(&mut self) -> u8 {
        let mut interrupts = 0;

        self.dot += 1;
        if self.dot == DOTS_PER_LINE {
            self.dot = 0;
            self.first_line_after_enable = false;
            self.line = (self.line + 1) % LINES_PER_FRAME;
            self.ly = self.line;
            if self.line == 0 {
                self.window_line = 0;
            }
        }

        // On the last line LY already reads 0 shortly after the line starts.
        if self.line == 153 && self.dot == 4 {
            self.ly = 0;
        }

        if self.line < SCREEN_HEIGHT as u8 && self.dot == self.drawing_start() {
            self.drawing_dots = self.calculate_drawing_dots();
        }

        let new_mode = if self.line >= SCREEN_HEIGHT as u8 {
            Mode::VBlank
        } else if self.dot < self.drawing_start() {
            if self.first_line_after_enable {
                Mode::HBlank
            } else {
                Mode::OamScan
            }
        } else if self.dot < self.drawing_start() + self.drawing_dots {
            Mode::Drawing
        } else {
            Mode::HBlank
        };

        self.visible_mode = self.calculate_visible_mode();

        if new_mode != self.mode {
            self.mode = new_mode;
            match new_mode {
                Mode::HBlank => self.render_scanline(),
                Mode::VBlank => {
                    interrupts |= INT_VBLANK;
                    self.first_frame_after_enable = false;
                    self.frame_ready = true;
                }
                _ => {}
            }
        }

        self.update_lyc_flag();
        interrupts | self.update_stat_line()
    }

    /// The mode STAT reports. It switches to OAM scan and drawing 4 dots
    /// after the PPU really does, and to HBlank 1 dot after.
    fn calculate_visible_mode(&self) -> Mode {
        const LAG: u16 = 4;
        let drawing_end = self.drawing_start() + self.drawing_dots + 1;

        if self.line >= SCREEN_HEIGHT as u8 {
            if self.line == SCREEN_HEIGHT as u8 && self.dot < LAG {
                Mode::HBlank
            } else {
                Mode::VBlank
            }
        } else if self.dot < LAG {
            // Still showing the end of the previous line.
            if self.line == 0 && !self.first_line_after_enable {
                Mode::VBlank
            } else {
                Mode::HBlank
            }
        } else if self.dot < OAM_SCAN_DOTS + LAG {
            if self.first_line_after_enable {
                Mode::HBlank
            } else {
                Mode::OamScan
            }
        } else if self.dot < drawing_end {
            Mode::Drawing
        } else {
            Mode::HBlank
        }
    }

    /// Recomputes the STAT interrupt line, returning INT_STAT on a rising edge.
    fn update_stat_line(&mut self) -> u8 {
        if !self.lcd_enabled() {
            return 0;
        }
        let lyc_source = self.stat & 0x40 != 0 && self.lyc_flag;
        let mode0_source = self.stat & 0x08 != 0 && self.mode == Mode::HBlank;
        let mode1_source = self.stat & 0x10 != 0 && self.mode == Mode::VBlank;
        // The mode 2 source also fires at the start of line 144, as vblank begins.
        let mode2_source = self.stat & 0x20 != 0
            && (self.mode == Mode::OamScan || (self.line == 144 && self.dot == 0));

        let was_high = self.stat_line;
        self.stat_line = lyc_source || mode0_source || mode1_source || mode2_source;
        if self.stat_line && !was_high {
            INT_STAT
        } else {
            0
        }
    }

    /// How many dots mode 3 takes on this line. The PPU pauses for
    /// discarding the scrolled-off pixels, starting the window, and fetching each sprite.
    fn calculate_drawing_dots(&self) -> u16 {
        let mut dots = MIN_DRAWING_DOTS + (self.scx % 8) as u16;

        if self.window_visible_on_line() {
            dots += 6;
        }

        if self.lcdc & 0x02 != 0 {
            // Each sprite costs 6 dots, plus a wait for the background fetch
            // if it's the first sprite in its background tile.
            let mut tiles_with_sprite = Vec::new();
            for sprite in self.sprites_on_line() {
                let x = self.oam[sprite * 4 + 1];
                if x >= 168 {
                    continue; // fully off the right edge
                }
                let bg_x = x as u16 + self.scx as u16;
                let tile = bg_x / 8;
                if !tiles_with_sprite.contains(&tile) {
                    tiles_with_sprite.push(tile);
                    dots += 5u16.saturating_sub(bg_x % 8);
                }
                dots += 6;
            }
        }
        dots
    }

    fn window_visible_on_line(&self) -> bool {
        let window_enabled = self.lcdc & 0x01 != 0 && self.lcdc & 0x20 != 0;
        window_enabled && self.line >= self.wy && self.wx <= 166
    }

    /// The first 10 sprites in OAM that overlap the current line, as found by the OAM scan.
    fn sprites_on_line(&self) -> Vec<usize> {
        let line = self.line as i32;
        let height = if self.lcdc & 0x04 != 0 { 16 } else { 8 };
        (0..40)
            .filter(|&i| {
                let top = self.oam[i * 4] as i32 - 16;
                line >= top && line < top + height
            })
            .take(10)
            .collect()
    }

    /// Returns the color number (0-3) of one pixel of a tile.
    /// `tile_addr` is the address of the tile data relative to the start of VRAM.
    fn tile_pixel(&self, tile_addr: usize, row: u8, col: u8) -> u8 {
        let addr = tile_addr + row as usize * 2;
        let low = self.vram[addr];
        let high = self.vram[addr + 1];
        let bit = 7 - col;
        (((high >> bit) & 1) << 1) | ((low >> bit) & 1)
    }

    /// Finds the tile data address for a background/window tile number.
    /// Depending on LCDC bit 4 the tile number is unsigned from 0x8000 or signed from 0x9000.
    fn bg_tile_addr(&self, tile_number: u8) -> usize {
        if self.lcdc & 0x10 != 0 {
            tile_number as usize * 16
        } else {
            (0x1000 + (tile_number as i8 as i32) * 16) as usize
        }
    }

    /// Color number of a background or window pixel from the given tile map.
    fn map_pixel(&self, map_addr: usize, x: u8, y: u8) -> u8 {
        let tile_index = (y as usize / 8) * 32 + (x as usize / 8);
        let tile_number = self.vram[map_addr + tile_index];
        self.tile_pixel(self.bg_tile_addr(tile_number), y % 8, x % 8)
    }

    fn render_scanline(&mut self) {
        let ly = self.line;
        let line_start = ly as usize * SCREEN_WIDTH;
        // Background/window color numbers on this line, needed for sprite priority.
        let mut bg_colors = [0u8; SCREEN_WIDTH];

        // Background
        if self.lcdc & 0x01 != 0 {
            let map = if self.lcdc & 0x08 != 0 { 0x1C00 } else { 0x1800 };
            let y = ly.wrapping_add(self.scy);
            for x in 0..SCREEN_WIDTH {
                let color = self.map_pixel(map, (x as u8).wrapping_add(self.scx), y);
                bg_colors[x] = color;
                self.framebuffer[line_start + x] = apply_palette(self.bgp, color);
            }
        } else {
            for x in 0..SCREEN_WIDTH {
                self.framebuffer[line_start + x] = apply_palette(self.bgp, 0);
            }
        }

        // Window: a second background layer drawn on top, not affected by scrolling.
        if self.window_visible_on_line() {
            let map = if self.lcdc & 0x40 != 0 { 0x1C00 } else { 0x1800 };
            let window_x = self.wx as i32 - 7;
            for x in 0..SCREEN_WIDTH {
                if (x as i32) < window_x {
                    continue;
                }
                let color = self.map_pixel(map, (x as i32 - window_x) as u8, self.window_line);
                bg_colors[x] = color;
                self.framebuffer[line_start + x] = apply_palette(self.bgp, color);
            }
            self.window_line += 1;
        }

        // Sprites
        if self.lcdc & 0x02 != 0 {
            self.render_sprites(line_start, &bg_colors);
        }

        // The line is still drawn so the PPU's state stays right, but the
        // first frame after switching the LCD on isn't shown.
        if self.first_frame_after_enable {
            self.framebuffer[line_start..line_start + SCREEN_WIDTH].fill(0);
        }
    }

    fn render_sprites(&mut self, line_start: usize, bg_colors: &[u8; SCREEN_WIDTH]) {
        let ly = self.line as i32;
        let height = if self.lcdc & 0x04 != 0 { 16 } else { 8 };
        let mut sprites = self.sprites_on_line();

        // On DMG, the sprite with the smaller X wins, then the one earlier in OAM.
        // Sort by priority and draw the lowest priority first so the winners end up on top.
        sprites.sort_by_key(|&i| (self.oam[i * 4 + 1], i));

        for &i in sprites.iter().rev() {
            let top = self.oam[i * 4] as i32 - 16;
            let left = self.oam[i * 4 + 1] as i32 - 8;
            let mut tile = self.oam[i * 4 + 2];
            let flags = self.oam[i * 4 + 3];

            let behind_bg = flags & 0x80 != 0;
            let flip_y = flags & 0x40 != 0;
            let flip_x = flags & 0x20 != 0;
            let palette = if flags & 0x10 != 0 { self.obp1 } else { self.obp0 };

            let mut row = (ly - top) as u8;
            if flip_y {
                row = height as u8 - 1 - row;
            }
            if height == 16 {
                tile &= 0xFE;
            }
            // 8x16 sprites use two consecutive tiles, so row 8-15 lands in the second tile.
            let tile_addr = tile as usize * 16;

            for col in 0..8u8 {
                let x = left + col as i32;
                if !(0..SCREEN_WIDTH as i32).contains(&x) {
                    continue;
                }
                let pixel_col = if flip_x { 7 - col } else { col };
                let color = self.tile_pixel(tile_addr, row, pixel_col);
                if color == 0 {
                    continue; // color 0 is transparent for sprites
                }
                if behind_bg && bg_colors[x as usize] != 0 {
                    continue;
                }
                self.framebuffer[line_start + x as usize] = apply_palette(palette, color);
            }
        }
    }
}

/// Palettes map each color number (0-3) to a shade using 2 bits each.
fn apply_palette(palette: u8, color: u8) -> u8 {
    (palette >> (color * 2)) & 0x03
}
