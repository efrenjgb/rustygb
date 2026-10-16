#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Right,
    Left,
    Up,
    Down,
    A,
    B,
    Select,
    Start,
}

/// The P1/JOYP register at 0xFF00.
///
/// The game writes bit 4 or 5 low to choose which group of buttons it wants
/// to read, then reads the lower 4 bits where 0 means "pressed".
pub struct Joypad {
    select: u8,
    directions: u8,
    actions: u8,
}

impl Joypad {
    pub fn new() -> Self {
        Joypad {
            select: 0x30,
            directions: 0x0F,
            actions: 0x0F,
        }
    }

    pub fn read(&self) -> u8 {
        let mut low = 0x0F;
        if self.select & 0x10 == 0 {
            low &= self.directions;
        }
        if self.select & 0x20 == 0 {
            low &= self.actions;
        }
        0xC0 | self.select | low
    }

    pub fn write(&mut self, value: u8) {
        self.select = value & 0x30;
    }

    /// Updates a button. Returns true if the button was newly pressed,
    /// which should raise the joypad interrupt.
    pub fn set_button(&mut self, button: Button, pressed: bool) -> bool {
        let (group, bit) = match button {
            Button::Right => (&mut self.directions, 0),
            Button::Left => (&mut self.directions, 1),
            Button::Up => (&mut self.directions, 2),
            Button::Down => (&mut self.directions, 3),
            Button::A => (&mut self.actions, 0),
            Button::B => (&mut self.actions, 1),
            Button::Select => (&mut self.actions, 2),
            Button::Start => (&mut self.actions, 3),
        };
        let was_pressed = *group & (1 << bit) == 0;
        if pressed {
            *group &= !(1 << bit);
        } else {
            *group |= 1 << bit;
        }
        pressed && !was_pressed
    }
}
