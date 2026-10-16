//! Game controller support using gilrs.
//!
//! Works with Xbox, PlayStation, Switch Pro and most other USB or Bluetooth
//! controllers. Buttons are matched by position, so "South" is the bottom
//! face button (A on Xbox, Cross on PlayStation, B on Switch).

use gilrs::{Axis, EventType, Gamepad, Gilrs};
use rustygb::joypad::Button;

/// How far the left stick has to be pushed (0.0 to 1.0) to count as a direction.
const STICK_DEADZONE: f32 = 0.5;

const BUTTON_MAP: [(gilrs::Button, Button); 8] = [
    (gilrs::Button::DPadRight, Button::Right),
    (gilrs::Button::DPadLeft, Button::Left),
    (gilrs::Button::DPadUp, Button::Up),
    (gilrs::Button::DPadDown, Button::Down),
    (gilrs::Button::South, Button::A),
    (gilrs::Button::East, Button::B),
    (gilrs::Button::Select, Button::Select),
    (gilrs::Button::Start, Button::Start),
];

pub struct Gamepads {
    gilrs: Gilrs,
}

impl Gamepads {
    pub fn new() -> Result<Self, String> {
        let gilrs = Gilrs::new().map_err(|error| error.to_string())?;
        for (_, gamepad) in gilrs.gamepads() {
            println!("controller connected: {}", gamepad.name());
        }
        Ok(Gamepads { gilrs })
    }

    /// Processes pending controller events, which keeps the button state up
    /// to date. Call once per frame.
    pub fn update(&mut self) {
        while let Some(event) = self.gilrs.next_event() {
            let name = self.gilrs.gamepad(event.id).name().to_string();
            match event.event {
                EventType::Connected => println!("controller connected: {name}"),
                EventType::Disconnected => println!("controller disconnected: {name}"),
                _ => {}
            }
        }
    }

    /// Whether the Home/Guide button is held on any controller.
    /// It takes you from a game back to the library.
    pub fn menu_pressed(&self) -> bool {
        self.gilrs
            .gamepads()
            .any(|(_, gamepad)| gamepad.is_pressed(gilrs::Button::Mode))
    }

    /// Whether any connected controller is holding this Game Boy button.
    pub fn is_pressed(&self, button: Button) -> bool {
        self.gilrs.gamepads().any(|(_, gamepad)| {
            let mapped_button = BUTTON_MAP
                .iter()
                .any(|&(pad_button, gb_button)| gb_button == button && gamepad.is_pressed(pad_button));
            mapped_button || stick_points(&gamepad, button)
        })
    }
}

/// Whether the left stick points in this button's direction.
fn stick_points(gamepad: &Gamepad, button: Button) -> bool {
    let x = gamepad.value(Axis::LeftStickX);
    let y = gamepad.value(Axis::LeftStickY); // up is positive
    match button {
        Button::Right => x > STICK_DEADZONE,
        Button::Left => x < -STICK_DEADZONE,
        Button::Up => y > STICK_DEADZONE,
        Button::Down => y < -STICK_DEADZONE,
        _ => false,
    }
}
