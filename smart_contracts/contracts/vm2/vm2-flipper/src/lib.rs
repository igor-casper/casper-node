#![cfg_attr(target_arch = "wasm32", no_main)]
#![cfg_attr(target_arch = "wasm32", no_std)]

use casper_macros::casper;

/// This contract implements a simple flipper.
#[casper(state)]
pub struct Flipper {
    /// The current state of the flipper.
    value: bool,
}

impl Default for Flipper {
    fn default() -> Self {
        panic!("Unable to instantiate contract without a constructor");
    }
}

#[casper]
impl Flipper {
    #[casper(constructor)]
    pub fn new(init_value: bool) -> Self {
        Self { value: init_value }
    }

    #[casper(constructor)]
    pub fn default() -> Self {
        Self::new(Default::default())
    }

    pub fn flip(&mut self) {
        self.value = !self.value;
    }

    pub fn get(&self) -> bool {
        self.value
    }
}