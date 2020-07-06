//! Cryptographic hash type and function.

use std::fmt::{self, Debug, Display, Formatter};

use blake2::{
    digest::{Input, VariableOutput},
    VarBlake2b,
};
use hex_fmt::HexFmt;
use serde::{Deserialize, Serialize};

use super::Result;

/// The hash digest; a wrapped `u8` array.
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct Digest([u8; Digest::LENGTH]);

impl Digest {
    /// Length of `Digest` in bytes.
    pub const LENGTH: usize = 32;

    /// Returns the wrapped `u8` array.
    pub fn to_bytes(&self) -> [u8; Digest::LENGTH] {
        self.0
    }

    /// Returns a `Digest` parsed from a hex-encoded `Digest`.
    pub fn from_hex<T: AsRef<[u8]>>(hex_input: T) -> Result<Self> {
        let mut inner = [0; Digest::LENGTH];
        hex::decode_to_slice(hex_input, &mut inner)?;
        Ok(Digest(inner))
    }
}

impl AsRef<[u8]> for Digest {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

impl Debug for Digest {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", HexFmt(&self.0))
    }
}

impl Display for Digest {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:10}", HexFmt(&self.0))
    }
}

/// Returns the hash of `data`.
pub fn hash<T: AsRef<[u8]>>(data: T) -> Digest {
    let mut result = [0; Digest::LENGTH];

    let mut hasher = VarBlake2b::new(Digest::LENGTH).expect("should create hasher");
    hasher.input(data);
    hasher.variable_result(|slice| {
        result.copy_from_slice(slice);
    });
    Digest(result)
}

#[cfg(test)]
mod test {
    use std::iter::{self, FromIterator};

    use super::*;

    #[test]
    fn blake2b_hash_known() {
        let inputs_and_digests = [
            (
                "",
                "0e5751c026e543b2e8ab2eb06099daa1d1e5df47778f7787faab45cdf12fe3a8",
            ),
            (
                "abc",
                "bddd813c634239723171ef3fee98579b94964e3bb1cb3e427262c8c068d52319",
            ),
            (
                "The quick brown fox jumps over the lazy dog",
                "01718cec35cd3d796dd00020e0bfecb473ad23457d063b75eff29c0ffa2e58a9",
            ),
        ];
        for (known_input, expected_digest) in &inputs_and_digests {
            let known_input: &[u8] = known_input.as_ref();
            assert_eq!(*expected_digest, format!("{:?}", hash(known_input)));
        }
    }

    #[test]
    fn from_valid_hex_should_succeed() {
        for char in "abcdefABCDEF0123456789".chars() {
            let input = String::from_iter(iter::repeat(char).take(64));
            assert!(Digest::from_hex(input).is_ok());
        }
    }

    #[test]
    fn from_hex_invalid_length_should_fail() {
        for len in &[2_usize, 62, 63, 65, 66] {
            let input = String::from_iter(iter::repeat('f').take(*len));
            assert!(Digest::from_hex(input).is_err());
        }
    }

    #[test]
    fn from_hex_invalid_char_should_fail() {
        for char in "g %-".chars() {
            let input = String::from_iter(iter::repeat('f').take(63).chain(iter::once(char)));
            assert!(Digest::from_hex(input).is_err());
        }
    }
}
