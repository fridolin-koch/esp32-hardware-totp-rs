use core::cell::RefCell;

use ds323x::interface::I2cInterface;
use ds323x::{ic, DateTimeAccess, Datelike, Ds323x, NaiveDateTime, Timelike};
use embedded_sdmmc::{TimeSource, Timestamp};
use esp_hal::i2c::I2C;
use esp_hal::peripherals::I2C0;
use esp_hal::Blocking;

pub(crate) struct Rtc<'d> {
    rtc: RefCell<Ds323x<I2cInterface<I2C<'d, I2C0, Blocking>>, ic::DS3231>>,
}

impl<'d> Rtc<'d> {
    pub(crate) fn new(i2c: I2C<'d, I2C0, Blocking>) -> Self {
        Rtc {
            rtc: RefCell::new(Ds323x::new_ds3231(i2c)),
        }
    }
    pub(crate) fn datetime(&self) -> NaiveDateTime {
        let mut rtc = self.rtc.borrow_mut();
        rtc.datetime().unwrap()
    }
}

impl<'d> TimeSource for &Rtc<'d> {
    fn get_timestamp(&self) -> Timestamp {
        let mut rtc = self.rtc.borrow_mut();
        let dt = match rtc.datetime() {
            Err(_) => {
                return Timestamp {
                    year_since_1970: 0,
                    zero_indexed_month: 0,
                    zero_indexed_day: 0,
                    hours: 0,
                    minutes: 0,
                    seconds: 0,
                }
            }
            Ok(dt) => dt,
        };

        Timestamp {
            year_since_1970: (dt.year() - 1970) as u8,
            zero_indexed_month: dt.month0() as u8,
            zero_indexed_day: dt.day0() as u8,
            hours: dt.hour() as u8,
            minutes: dt.minute() as u8,
            seconds: dt.second() as u8,
        }
    }
}
