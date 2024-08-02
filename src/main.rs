#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use core::cell::RefCell;
use core::ops::{Deref, DerefMut};

use critical_section::{CriticalSection, Mutex};
use ds323x::{Datelike, Timelike};
use esp_backtrace as _;
use esp_hal::{
    Blocking, clock::ClockControl, delay::Delay, gpio, interrupt, peripherals::Peripherals,
    prelude::*, psram, system::SystemControl, time,
};
use esp_hal::gpio::{AnyOutput, Event, Input, Io, Level, Pull};
use esp_hal::i2c::I2C;
use esp_hal::interrupt::Priority;
use esp_hal::peripherals::{Interrupt, TIMG0};
use esp_hal::timer::timg::{Timer, Timer0, TimerGroup};
use rotary_encoder_embedded::{Direction, RotaryEncoder};
use rotary_encoder_embedded::standard::StandardMode;

use crate::config::Config;
use crate::display::Display;
use crate::topt::Token;

mod config;
mod display;
mod rtc;
mod topt;

#[global_allocator]
static ALLOCATOR: esp_alloc::EspHeap = esp_alloc::EspHeap::empty();

fn init_psram_heap() {
    unsafe {
        ALLOCATOR.init(psram::psram_vaddr_start() as *mut u8, psram::PSRAM_BYTES);
    }
}

#[derive(Default)]
struct AuthParams {
    current: usize,
    digits: [i8; 6],
}

struct AppParams {
    current: usize,
    token_len: usize,
    last_token: Option<Token>,
    bar: u8,
}

enum Mode {
    Init,
    Auth(AuthParams),
    App(AppParams),
}

impl Mode {
    fn inc(&mut self) {
        match self {
            Self::Auth(params) => {
                params.digits[params.current] += 1;
                if params.digits[params.current] > 9 {
                    params.digits[params.current] = 0;
                }
            }
            Self::App(params) => {
                params.current += 1;
                if params.current >= params.token_len {
                    params.current = 0;
                }
            }
            _ => {}
        }
    }

    fn dec(&mut self) {
        match self {
            Self::Auth(params) => {
                params.digits[params.current] -= 1;
                if params.digits[params.current] < 0 {
                    params.digits[params.current] = 9;
                }
            }
            Self::App(params) => {
                params.current = match params.current.checked_sub(1) {
                    None => params.token_len - 1,
                    Some(v) => v,
                };
            }
            _ => {}
        }
    }

    fn advance(&mut self) -> Option<String> {
        if let Self::Auth(params) = self {
            if params.current == params.digits.len() - 1 {
                return Some(format!(
                    "{}{}{}{}{}{}",
                    params.digits[0],
                    params.digits[1],
                    params.digits[2],
                    params.digits[3],
                    params.digits[4],
                    params.digits[5]
                ));
            }
            params.current += 1;
        }
        None
    }
}

type Global<T> = Mutex<RefCell<T>>;
type GlobalOpt<T> = Mutex<RefCell<Option<T>>>;

static ROTARY_ENCODER: GlobalOpt<
    RotaryEncoder<StandardMode, Input<gpio::Gpio2>, Input<gpio::Gpio42>>,
> = Mutex::new(RefCell::new(None));
static ROTARY_SWITCH: GlobalOpt<Input<gpio::Gpio1>> = Mutex::new(RefCell::new(None));
static ROTARY_SWITCH_DEBOUNCE: GlobalOpt<fugit::Instant<u64, 1, 1_000_000>> =
    Mutex::new(RefCell::new(None));

static MODE: Global<Mode> = Mutex::new(RefCell::new(Mode::Init));
static CONFIG: GlobalOpt<Config> = Mutex::new(RefCell::new(None));

static TOTP_GEN: GlobalOpt<topt::Generator> = Mutex::new(RefCell::new(None));

static TIMER0: GlobalOpt<Timer<Timer0<TIMG0>, Blocking>> = Mutex::new(RefCell::new(None));

static DISPLAY: GlobalOpt<Display> = Mutex::new(RefCell::new(None));

#[entry]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    let peripherals = Peripherals::take();
    //
    // init psram
    //
    psram::init_psram(peripherals.PSRAM);
    init_psram_heap();

    //
    // System Init
    //
    let system = SystemControl::new(peripherals.SYSTEM);
    let clocks = ClockControl::max(system.clock_control).freeze();
    let delay = Delay::new(&clocks);
    let mut io = Io::new(peripherals.GPIO, peripherals.IO_MUX);
    io.set_interrupt_handler(encoder_handler);

    //
    // Init RTC
    //
    let i2c = I2C::new(
        peripherals.I2C0,
        io.pins.gpio4,
        io.pins.gpio6,
        100.kHz(),
        &clocks,
    );
    let clock = rtc::Rtc::new(i2c);

    //
    // Init Display
    //
    let display = Display::new(
        AnyOutput::new(io.pins.gpio46, Level::Low), // RS
        AnyOutput::new(io.pins.gpio13, Level::Low), // EN
        AnyOutput::new(io.pins.gpio12, Level::Low),
        AnyOutput::new(io.pins.gpio11, Level::Low),
        AnyOutput::new(io.pins.gpio10, Level::Low),
        AnyOutput::new(io.pins.gpio9, Level::Low),
        delay,
    );

    //display.render_auth(0, [0i8; 6]);
    critical_section::with(|cs| {
        DISPLAY.borrow_ref_mut(cs).replace(display);
    });

    //
    // Load Config from SD-Card
    //
    let config = config::load_config(
        peripherals.SPI2,
        io.pins.gpio18, // Purple
        io.pins.gpio17, // Green
        io.pins.gpio15, // White
        io.pins.gpio16, // Orange
        &clocks,
        delay,
        &clock,
    )
    .unwrap();
    critical_section::with(|cs| {
        CONFIG.replace(cs, Some(config));
    });

    //
    // TOPT Generator
    let topt_gen = topt::Generator::new(peripherals.SHA, clock);
    critical_section::with(|cs| {
        TOTP_GEN.replace(cs, Some(topt_gen));
    });

    //
    // Rotary encoder
    //
    let mut clk = Input::new(io.pins.gpio42, Pull::Up); // Orange
    clk.listen(Event::FallingEdge);
    let mut dt = Input::new(io.pins.gpio2, Pull::Up); // Blue
    dt.listen(Event::FallingEdge);
    let mut sw = Input::new(io.pins.gpio1, Pull::Up); // Green
    sw.listen(Event::FallingEdge);

    let rotary_encoder = RotaryEncoder::new(dt, clk).into_standard_mode();
    critical_section::with(|cs| {
        ROTARY_ENCODER.borrow_ref_mut(cs).replace(rotary_encoder);
        ROTARY_SWITCH.borrow_ref_mut(cs).replace(sw);
    });

    //
    // Timer interrupt for rotary encoder
    //
    let timg0 = TimerGroup::new(peripherals.TIMG0, &clocks);
    let timer0 = timg0.timer0;
    timer0.set_interrupt_handler(topt_handler);
    interrupt::enable(Interrupt::TG0_T0_LEVEL, Priority::Priority1).unwrap();
    timer0.load_value(50u64.millis()).unwrap();
    timer0.start();
    timer0.listen();

    critical_section::with(|cs| {
        TIMER0.borrow_ref_mut(cs).replace(timer0);
    });

    loop {}
}

#[handler]
fn encoder_handler() {
    #[derive(PartialEq, Eq)]
    enum Action {
        None,
        UpdateAuth,
        UpdateToken,
        Decrypt(String),
    }
    let action = critical_section::with(|cs| {
        let mut mode = MODE.borrow_ref_mut(cs);
        let mode = mode.deref_mut();
        let mut next_action = Action::None;

        // handle rotary
        let mut rotary = ROTARY_ENCODER.borrow_ref_mut(cs);
        let rotary = rotary.as_mut().unwrap();
        let check = {
            let (dt, clk) = rotary.pins_mut();
            dt.is_interrupt_set() || clk.is_interrupt_set()
        };
        if check {
            if match rotary.update() {
                Direction::Clockwise => {
                    mode.dec();
                    true
                }
                Direction::Anticlockwise => {
                    mode.inc();
                    true
                }
                Direction::None => false,
            } {
                next_action = match mode {
                    Mode::Auth(_) => Action::UpdateAuth,
                    Mode::App(state) => {
                        state.last_token = None;
                        state.bar = 0;
                        Action::UpdateToken
                    }
                    _ => Action::None,
                };
            }
            {
                let (dt, clk) = rotary.pins_mut();
                dt.clear_interrupt();
                clk.clear_interrupt();
            };
        }
        // check button push in auth mode to advance the cursor
        match mode {
            Mode::Auth(_) => {
                if let Some(switch) = ROTARY_SWITCH.borrow_ref_mut(cs).as_mut() {
                    if switch.is_interrupt_set() {
                        let now = time::current_time();
                        let last = ROTARY_SWITCH_DEBOUNCE.replace(cs, Some(now));
                        let advance = match last {
                            None => true,
                            Some(last) => now - last > 250u64.millis::<1, 1_000_000>(),
                        };
                        if advance {
                            next_action = match mode.advance() {
                                Some(pin) => {
                                    switch.unlisten(); // switch has no use anymore!
                                    Action::Decrypt(pin)
                                }
                                None => Action::UpdateAuth,
                            };
                        }
                        switch.clear_interrupt();
                    };
                }
            }
            Mode::Init => {
                // switch app mode
                *mode = Mode::Auth(AuthParams {
                    current: 0,
                    digits: [0i8; 6],
                });
                ROTARY_SWITCH_DEBOUNCE.replace(cs, Some(time::current_time()));
                next_action = Action::UpdateAuth;
            }
            _ => {}
        }
        next_action
    });

    critical_section::with(|cs| match action {
        Action::UpdateAuth => {
            let mut display = DISPLAY.borrow_ref_mut(cs);
            let mode = MODE.borrow_ref_mut(cs);
            if let Mode::Auth(params) = mode.deref() {
                display
                    .as_mut()
                    .unwrap()
                    .render_auth(params.current, params.digits);
            }
        }
        Action::Decrypt(pin) => {
            let mut display = DISPLAY.borrow_ref_mut(cs);
            let display = display.as_mut().unwrap();
            display.write_clear((0, 0), "Decrypting...");

            let mut config = CONFIG.borrow_ref_mut(cs);
            let config = config.as_mut().unwrap();

            match config::decrypt(config, pin) {
                Err(err) => display.write_clear((0, 0), format!("Error: {:?}", err).as_str()),
                Ok(_) => display.write((0, 1), "Done!"),
            }

            display.toggle_cursor(false);

            // switch app mode
            MODE.replace(
                cs,
                Mode::App(AppParams {
                    current: 0,
                    token_len: config.tokens.len(),
                    last_token: None,
                    bar: 0,
                }),
            );
            // initial update
            let mut timer0 = TIMER0.borrow_ref_mut(cs);
            let timer0 = timer0.as_mut().unwrap();
            timer0.load_value(1u64.millis()).unwrap();
            timer0.start();
        }
        Action::UpdateToken => update_token(cs),
        _ => {}
    });
}

fn update_token(cs: CriticalSection) {
    let mut mode = MODE.borrow_ref_mut(cs);
    // get the display
    let mut display = DISPLAY.borrow_ref_mut(cs);
    let display = display.as_mut().unwrap();
    // topt gen
    let mut gen = TOTP_GEN.borrow_ref_mut(cs);
    let gen = gen.as_mut().unwrap();
    match mode.deref_mut() {
        Mode::App(ref mut state) => {
            if let Some(config) = CONFIG.borrow_ref_mut(cs).deref() {
                if state.last_token.is_none() {
                    display.write_clear((0, 0), config.tokens[state.current].name.as_str());
                }
                // check if we need to update the token
                let timestamp = gen.timestamp();
                let (token, changed) = match state.last_token.take() {
                    None => (
                        gen.token(
                            config.tokens[state.current].key_as_bytes().as_slice(),
                            timestamp,
                        ),
                        true,
                    ),
                    Some(last) => {
                        let remaining = last.valid_until as i64 - timestamp as i64;
                        if remaining <= 0 {
                            (
                                gen.token(
                                    config.tokens[state.current].key_as_bytes().as_slice(),
                                    timestamp,
                                ),
                                true,
                            )
                        } else {
                            (last, false)
                        }
                    }
                };
                // write code
                if changed {
                    display.write((0, 1), format!("{:06}", token.code).as_str());
                }
                // remaining time
                let remaining = token.valid_until as i64 - timestamp as i64;
                let bar = match remaining {
                    // todo: there must be a "math-solution" for that!
                    26..=30 => 1,
                    21..=25 => 2,
                    16..=20 => 3,
                    11..=15 => 4,
                    6..=10 => 5,
                    _ => 6,
                };
                if bar != state.bar {
                    state.bar = bar;
                    for i in 0..6 {
                        display.write((7 + i, 1), if i < bar { "*" } else { " " });
                    }
                }
                state.last_token = Some(token);
                // calculate time until next update and set timer
                let mut timer0 = TIMER0.borrow_ref_mut(cs);
                let timer0 = timer0.as_mut().unwrap();
                if timer0.is_running() {
                    timer0.stop();
                }
                timer0.clear_interrupt();
                timer0.load_value(1.secs()).unwrap();
                timer0.start();
            }
        }
        Mode::Init => {
            let now = gen.datetime();
            display.write_clear(
                (0, 0),
                format!("{:02}.{:02}.{:02}", now.day(), now.month(), now.year(),).as_str(),
            );
            display.write(
                (0, 1),
                format!(
                    "{:02}:{:02}:{:02} Cont.?",
                    now.hour(),
                    now.minute(),
                    now.second()
                )
                .as_str(),
            );
            // calculate time until next update and set timer
            let mut timer0 = TIMER0.borrow_ref_mut(cs);
            let timer0 = timer0.as_mut().unwrap();
            if timer0.is_running() {
                timer0.stop();
            }
            timer0.clear_interrupt();
            timer0.load_value(500.millis()).unwrap();
            timer0.start();
        }
        _ => {}
    }
}

#[handler]
fn topt_handler() {
    critical_section::with(update_token);
}
