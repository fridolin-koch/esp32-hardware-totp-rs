use alloc::{format};
use alloc::string::String;
use alloc::vec::Vec;

use aes::cipher::{KeyIvInit, StreamCipher};
use data_encoding::BASE32_NOPAD;
use embedded_sdmmc::Mode;
use esp_hal::clock::Clocks;
use esp_hal::delay::Delay;
use esp_hal::gpio::{InputPin, Level, NO_PIN, Output, OutputPin};
use esp_hal::peripheral::Peripheral;
use esp_hal::peripherals;
use esp_hal::prelude::_fugit_RateExtU32;
use esp_hal::spi::master::Spi;
use esp_hal::spi::SpiMode;
use serde::Deserialize;

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct Token {
    pub(crate) name: String,
    pub(crate) key: String,
}

impl Token {
    pub(crate) fn key_as_bytes(&self) -> Vec<u8> {
        BASE32_NOPAD.decode(self.key.as_bytes()).unwrap()
    }
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct Config {
    pub(crate) nonce: Option<String>,
    pub(crate) tokens: Vec<Token>,
}

#[derive(Debug)]
pub(crate) enum Error {
    SD(String),
    Deserialize(serde_json::Error),
    MissingIV,
    InvalidIV,
    Base32(data_encoding::DecodeError),
}

type Result<T> = core::result::Result<T, Error>;

pub(crate) fn load_config<SCK: OutputPin, MOSI: OutputPin, MISO: InputPin, CS: OutputPin>(
    spi2: impl Peripheral<P = peripherals::SPI2>,
    sck: impl Peripheral<P = SCK>,
    mosi: impl Peripheral<P = MOSI>,
    miso: impl Peripheral<P = MISO>,
    cs: impl Peripheral<P = CS>,
    clocks: &Clocks,
    delay: Delay,
    rtc: &crate::rtc::Rtc,
) -> Result<Config> {
    let spi = Spi::new(spi2, 400.kHz(), SpiMode::Mode0, clocks).with_pins(
        Some(sck),
        Some(mosi),
        Some(miso),
        NO_PIN,
    );
    let spi_dev =
        embedded_hal_bus::spi::ExclusiveDevice::new(spi, Output::new(cs, Level::Low), delay)
            .unwrap();

    let sdcard = embedded_sdmmc::SdCard::new(spi_dev, delay);
    let mut volume_mgr = embedded_sdmmc::VolumeManager::new(sdcard, rtc);
    let mut volume0 = volume_mgr
        .open_volume(embedded_sdmmc::VolumeIdx(0))
        .unwrap();
    // Open the root directory (mutably borrows from the volume).
    let mut root_dir = volume0.open_root_dir().unwrap();
    let mut file = root_dir
        .open_file_in_dir("CFG", Mode::ReadOnly)
        .map_err(|err| Error::SD(format!("A {:?}", err)))?;
    let mut data = Vec::with_capacity(file.length() as usize);
    while !file.is_eof() {
        let mut buffer = [0u8; 64];
        let len = file
            .read(&mut buffer)
            .map_err(|err| Error::SD(format!("{:?}", err)))?;
        data.extend_from_slice(&buffer[..len]);
    }
    serde_json::from_slice(data.as_slice()).map_err(Error::Deserialize)
}

type Aes128Ctr64LE = ctr::Ctr64LE<aes::Aes128>;

pub(crate) fn decrypt(config: &mut Config, pin: String) -> Result<()> {
    let iv = match &config.nonce {
        None => return Err(Error::MissingIV),
        Some(nonce) => {
            let nonce = BASE32_NOPAD.decode(nonce.as_bytes()).unwrap();
            if nonce.len() != 16 {
                return Err(Error::InvalidIV);
            }
            let mut iv = [0u8; 16];
            iv.copy_from_slice(nonce.as_slice());
            iv
        }
    };
    let mut key = [0u8; 16];
    let pin = pin.as_bytes();
    key[..pin.len()].copy_from_slice(pin);

    let mut cipher = Aes128Ctr64LE::new(&key.into(), &iv.into());

    for token in config.tokens.iter_mut() {
        let mut raw = BASE32_NOPAD
            .decode(token.key.as_bytes())
            .map_err(Error::Base32)?;
        cipher.apply_keystream(&mut raw);
        token.key = BASE32_NOPAD.encode(&raw);
    }

    Ok(())
}
