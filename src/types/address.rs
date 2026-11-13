//! Ergo address encoding: base58 addresses to ErgoTrees and back.

use std::{fmt, str::FromStr};

use blake2::{Blake2b256, Digest};

use crate::types::{HexBytes, ergo::ErgoTree};

const CHECKSUM_LENGTH: usize = 4;
const PUBLIC_KEY_LENGTH: usize = 33;
const P2SH_HASH_LENGTH: usize = 24;

const P2PK_TREE_PREFIX: [u8; 3] = [0x00, 0x08, 0xcd];
const P2SH_TREE_PREFIX: [u8; 17] = [
    0x00, 0xea, 0x02, 0xd1, 0x93, 0xb4, 0xcb, 0xe4, 0xe3, 0x7e, 0x0e, 0x04, 0x00, 0x04, 0x30, 0x0e,
    0x18,
];
const P2SH_TREE_SUFFIX: [u8; 3] = [0xd4, 0x08, 0x7e];
const P2PK_TREE_LENGTH: usize = P2PK_TREE_PREFIX.len() + PUBLIC_KEY_LENGTH;
const P2SH_TREE_LENGTH: usize = P2SH_TREE_PREFIX.len() + P2SH_HASH_LENGTH + P2SH_TREE_SUFFIX.len();

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Network {
    Mainnet = 0x00,
    Testnet = 0x10,
}

impl FromStr for Network {
    type Err = UnknownNetwork;

    /// Parses the network name the node reports in `/info`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "mainnet" => Ok(Network::Mainnet),
            "testnet" => Ok(Network::Testnet),
            _ => Err(UnknownNetwork(s.to_string())),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown network `{0}`")]
pub struct UnknownNetwork(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddressType {
    P2PK = 1,
    P2SH = 2,
    P2S = 3,
}

impl AddressType {
    fn of_tree(tree: &[u8]) -> Self {
        if tree.len() == P2PK_TREE_LENGTH && tree.starts_with(&P2PK_TREE_PREFIX) {
            AddressType::P2PK
        } else if tree.len() == P2SH_TREE_LENGTH
            && tree.starts_with(&P2SH_TREE_PREFIX)
            && tree.ends_with(&P2SH_TREE_SUFFIX)
        {
            AddressType::P2SH
        } else {
            AddressType::P2S
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AddressError {
    #[error("not valid base58")]
    Base58,
    #[error("too short")]
    TooShort,
    #[error("empty body")]
    EmptyBody,
    #[error("checksum mismatch")]
    Checksum,
    #[error("unknown network prefix {0:#04x}")]
    UnknownNetwork(u8),
    #[error("unsupported address type {0}")]
    UnsupportedType(u8),
    #[error("invalid public key")]
    InvalidPublicKey,
    #[error("P2SH hash must be {P2SH_HASH_LENGTH} bytes, got {0}")]
    P2shHashLength(usize),
}

/// A decoded address: the ErgoTree it stands for, and its network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    network: Network,
    ergo_tree: ErgoTree,
}

impl Address {
    pub fn from_ergo_tree(ergo_tree: ErgoTree, network: Network) -> Self {
        Self { network, ergo_tree }
    }

    /// Decodes and validates a base58 address.
    pub fn decode(encoded: &str) -> Result<Self, AddressError> {
        let bytes = bs58::decode(encoded)
            .into_vec()
            .map_err(|_| AddressError::Base58)?;
        if bytes.len() <= CHECKSUM_LENGTH {
            return Err(AddressError::TooShort);
        }

        let (content, checksum) = bytes.split_at(bytes.len() - CHECKSUM_LENGTH);
        let (head, body) = (content[0], &content[1..]);
        if body.is_empty() {
            return Err(AddressError::EmptyBody);
        }
        if checksum != &self::checksum(content)[..] {
            return Err(AddressError::Checksum);
        }

        let network = match head & 0xf0 {
            0x00 => Network::Mainnet,
            0x10 => Network::Testnet,
            other => return Err(AddressError::UnknownNetwork(other)),
        };

        let ergo_tree = match head & 0x0f {
            1 => {
                let valid_prefix = matches!(body.first(), Some(0x02 | 0x03));
                if body.len() != PUBLIC_KEY_LENGTH || !valid_prefix {
                    return Err(AddressError::InvalidPublicKey);
                }
                [&P2PK_TREE_PREFIX[..], body].concat()
            }
            2 => {
                if body.len() != P2SH_HASH_LENGTH {
                    return Err(AddressError::P2shHashLength(body.len()));
                }
                [&P2SH_TREE_PREFIX[..], body, &P2SH_TREE_SUFFIX[..]].concat()
            }
            3 => body.to_vec(),
            other => return Err(AddressError::UnsupportedType(other)),
        };

        Ok(Self { network, ergo_tree: HexBytes(ergo_tree) })
    }

    pub fn network(&self) -> Network {
        self.network
    }

    pub fn address_type(&self) -> AddressType {
        AddressType::of_tree(&self.ergo_tree.0)
    }

    pub fn ergo_tree(&self) -> &ErgoTree {
        &self.ergo_tree
    }

    pub fn into_ergo_tree(self) -> ErgoTree {
        self.ergo_tree
    }
}

impl FromStr for Address {
    type Err = AddressError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Address::decode(s)
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&encode(&self.ergo_tree.0, self.network))
    }
}

/// Encodes an ErgoTree as a base58 address.
pub fn encode(ergo_tree: &[u8], network: Network) -> String {
    let address_type = AddressType::of_tree(ergo_tree);
    let body = match address_type {
        AddressType::P2PK => &ergo_tree[P2PK_TREE_PREFIX.len()..],
        AddressType::P2SH => {
            &ergo_tree[P2SH_TREE_PREFIX.len()..P2SH_TREE_PREFIX.len() + P2SH_HASH_LENGTH]
        }
        AddressType::P2S => ergo_tree,
    };
    encode_raw(network as u8 | address_type as u8, body)
}

fn encode_raw(head: u8, body: &[u8]) -> String {
    let mut bytes = Vec::with_capacity(1 + body.len() + CHECKSUM_LENGTH);
    bytes.push(head);
    bytes.extend_from_slice(body);
    let checksum = checksum(&bytes);
    bytes.extend_from_slice(&checksum);
    bs58::encode(bytes).into_string()
}

fn checksum(content: &[u8]) -> [u8; CHECKSUM_LENGTH] {
    let hash = Blake2b256::digest(content);
    let mut out = [0; CHECKSUM_LENGTH];
    out.copy_from_slice(&hash[..CHECKSUM_LENGTH]);
    out
}

#[cfg(test)]
#[path = "address_tests.rs"]
mod tests;
