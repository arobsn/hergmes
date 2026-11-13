use std::fmt::{self, Debug, Display, Formatter};
use std::str::FromStr;

use hex::{FromHex, ToHex};
use serde::de::Error;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A 32-byte hash digest.
pub type HashDigest = Digest<32>;

/// A fixed-size byte array represented as a hex string in serialization.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest<const N: usize>(pub [u8; N]);

impl<const N: usize> Display for Digest<N> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.encode_hex::<String>())
    }
}

impl<const N: usize> From<Digest<N>> for String {
    fn from(val: Digest<N>) -> String {
        val.to_string()
    }
}

impl<const N: usize> Debug for Digest<N> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.encode_hex::<String>())
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum DigestError {
    #[error(transparent)]
    Hex(#[from] hex::FromHexError),
    #[error("expected {expected} bytes, got {got}")]
    Length { expected: usize, got: usize },
}

impl<const N: usize> FromStr for Digest<N> {
    type Err = DigestError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = Vec::from_hex(s)?;
        let got = bytes.len();
        let bytes: [u8; N] = bytes
            .try_into()
            .map_err(|_| DigestError::Length { expected: N, got })?;
        Ok(Digest(bytes))
    }
}

impl<'de, const N: usize> Deserialize<'de> for Digest<N> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }
}

impl<const N: usize> Serialize for Digest<N> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

/// A byte vector represented as a hex string in serialization.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HexBytes(pub Vec<u8>);

impl Display for HexBytes {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.encode_hex::<String>())
    }
}

impl From<HexBytes> for String {
    fn from(hex_bytes: HexBytes) -> Self {
        hex_bytes.to_string()
    }
}

impl Debug for HexBytes {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.encode_hex::<String>())
    }
}

impl<'de> Deserialize<'de> for HexBytes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let hex_str = String::deserialize(deserializer)?;
        let bytes = Vec::from_hex(&hex_str).map_err(D::Error::custom)?;
        Ok(HexBytes(bytes))
    }
}

impl Serialize for HexBytes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0.encode_hex::<String>())
    }
}
