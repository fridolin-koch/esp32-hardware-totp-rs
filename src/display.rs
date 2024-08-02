use alloc::format;

use esp_hal::delay::Delay;
use esp_hal::gpio::AnyOutput;
use hd44780_driver::{Cursor, CursorBlink, HD44780};
use hd44780_driver::bus::FourBitBus;

pub(crate) struct Display<'d> {
    display: HD44780<
        FourBitBus<
            AnyOutput<'d>,
            AnyOutput<'d>,
            AnyOutput<'d>,
            AnyOutput<'d>,
            AnyOutput<'d>,
            AnyOutput<'d>,
        >,
    >,
    delay: Delay,
}

impl<'d> Display<'d> {
    pub(crate) fn new(
        rs: AnyOutput<'d>,
        en: AnyOutput<'d>,
        d4: AnyOutput<'d>,
        d5: AnyOutput<'d>,
        d6: AnyOutput<'d>,
        d7: AnyOutput<'d>,
        mut delay: Delay,
    ) -> Self {
        let mut display = HD44780::new_4bit(rs, en, d4, d5, d6, d7, &mut delay).unwrap();
        display.reset(&mut delay).unwrap();
        display.clear(&mut delay).unwrap();
        Display { display, delay }
    }

    pub(crate) fn render_auth(&mut self, current: usize, digits: [i8; 6]) {
        self.display.reset(&mut self.delay).unwrap();
        self.display.clear(&mut self.delay).unwrap();

        self.display
            .write_str("Enter Code:", &mut self.delay)
            .unwrap();

        self.display.set_cursor_xy((0, 1), &mut self.delay).unwrap();

        self.display
            .write_str(
                format!(
                    "{}{}{}{}{}{}",
                    digits[0], digits[1], digits[2], digits[3], digits[4], digits[5]
                )
                .as_str(),
                &mut self.delay,
            )
            .unwrap();

        self.display
            .set_cursor_xy((current as u8, 1), &mut self.delay)
            .unwrap();
    }

    pub(crate) fn write(&mut self, position: (u8, u8), text: &str) {
        self.display
            .set_cursor_xy(position, &mut self.delay)
            .unwrap();
        self.display.write_str(text, &mut self.delay).unwrap();
    }
    pub(crate) fn write_clear(&mut self, position: (u8, u8), text: &str) {
        self.clear();
        self.write(position, text);
    }

    pub(crate) fn clear(&mut self) {
        self.display.clear(&mut self.delay).unwrap();
    }

    pub(crate) fn toggle_cursor(&mut self, visible: bool) {
        self.display
            .set_cursor_visibility(
                match visible {
                    true => Cursor::Visible,
                    false => Cursor::Invisible,
                },
                &mut self.delay,
            )
            .unwrap();
        self.display
            .set_cursor_blink(CursorBlink::Off, &mut self.delay)
            .unwrap();
    }
}
