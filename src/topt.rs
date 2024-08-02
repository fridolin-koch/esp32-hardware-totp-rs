use alloc::vec::Vec;

use ds323x::NaiveDateTime;
use esp_hal::{Blocking, peripherals};
use esp_hal::peripheral::Peripheral;
use esp_hal::prelude::nb::block;
use esp_hal::sha::{Sha, ShaMode};

use crate::rtc::Rtc;

pub(crate) struct Token {
    pub(crate) code: u32,
    pub(crate) valid_until: u64,
}

pub(crate) struct Generator<'a> {
    hasher: Sha<'a, Blocking>,
    rtc: Rtc<'a>,
}

impl<'a> Generator<'a> {
    pub(crate) fn new(sha: impl Peripheral<P = peripherals::SHA> + 'a, rtc: Rtc<'a>) -> Self {
        Generator {
            hasher: Sha::new(sha, ShaMode::SHA1),
            rtc,
        }
    }

    const IPAD: u8 = 0x36;
    const OPAD: u8 = 0x5C;
    const BLOCK_SIZE: usize = 64;
    pub(crate) fn token(&mut self, key: &[u8], timestamp: u64) -> Token {
        let mut key_padded = [0u8; Self::BLOCK_SIZE];
        if key.len() > Self::BLOCK_SIZE {
            let key_hash = self.hash(key);
            key_padded[..key.len()].copy_from_slice(&key_hash);
        } else {
            key_padded[..key.len()].copy_from_slice(key);
        }

        let mut ipad_key = key_padded;
        for b in ipad_key.iter_mut() {
            *b ^= Self::IPAD;
        }
        let t = timestamp / 30;
        let msg = t.to_be_bytes();

        let mut content = Vec::with_capacity(ipad_key.len() + msg.len());
        content.extend_from_slice(&ipad_key);
        content.extend_from_slice(&msg);

        let h1 = self.hash(content.as_slice());

        let mut opad_key = key_padded;
        for b in opad_key.iter_mut() {
            *b ^= Self::OPAD;
        }

        content.clear();
        content.extend_from_slice(&opad_key);
        content.extend_from_slice(&h1);
        let hmac = self.hash(content.as_slice());

        let offset = (hmac.last().unwrap() & 0x0F) as usize;
        Token {
            code: (u32::from_be_bytes(hmac[offset..=offset + 3].try_into().unwrap()) & 0x7fff_ffff)
                % 1000000,
            valid_until: (t + 1) * 30,
        }
    }

    pub(crate) fn timestamp(&self) -> u64 {
        self.rtc.datetime().and_utc().timestamp() as u64
    }

    pub(crate) fn datetime(&self) -> NaiveDateTime {
        self.rtc.datetime()
    }

    fn hash(&mut self, data: &[u8]) -> [u8; 20] {
        let mut remaining = data;
        while !remaining.is_empty() {
            remaining = block!(self.hasher.update(remaining)).unwrap();
        }
        let mut output = [0u8; 20];
        block!(self.hasher.finish(output.as_mut_slice())).unwrap();
        output
    }
}
