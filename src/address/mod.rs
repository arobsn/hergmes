use blake2::{Blake2b, Digest};
use hex::ToHex;
use thiserror::Error;

const CHECKSUM_LENGTH: usize = 4;
const BLAKE_256_HASH_LENGTH: usize = 32;

const P2PK_ERGOTREE_PREFIX: [u8; 3] = [0x00, 0x08, 0xcd];
const P2PK_ERGOTREE_LENGTH: usize = 36;
const P2PK_PUBKEY_LENGTH: usize = 33;

const P2SH_ERGOTREE_PREFIX: [u8; 17] = [
    0x00, 0xea, 0x02, 0xd1, 0x93, 0xb4, 0xcb, 0xe4, 0xe3, 0x01, 0x0e, 0x04, 0x00, 0x04, 0x30, 0x0e,
    0x18,
];
const P2SH_ERGOTREE_SUFFIX: [u8; 3] = [0xd4, 0x08, 0x01];
const P2SH_ERGOTREE_LENGTH: usize = 44;
const P2SH_HASH_LENGTH: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Network {
    Mainnet = 0x00,
    Testnet = 0x10,
}

impl Network {
    pub fn from_head_byte(byte: u8) -> Self {
        match byte & 0xf0 {
            0x00 => Network::Mainnet,
            0x10 => Network::Testnet,
            _ => Network::Mainnet,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AddressType {
    P2PK = 1,
    P2SH = 2,
    P2S = 3,
}

impl AddressType {
    pub fn from_head_byte(byte: u8) -> Option<Self> {
        match byte & 0x0f {
            1 => Some(AddressType::P2PK),
            2 => Some(AddressType::P2SH),
            3 => Some(AddressType::P2S),
            _ => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum AddressError {
    #[error("Invalid base58 encoding")]
    Base58DecodeError,

    #[error("Address too short (minimum 5 bytes)")]
    AddressTooShort,

    #[error("Invalid checksum")]
    InvalidChecksum,

    #[error("Invalid address type")]
    InvalidAddressType,

    #[error("Invalid ErgoTree format")]
    InvalidErgoTree,

    #[error("Invalid hex encoding: {0}")]
    HexDecodeError(#[from] hex::FromHexError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErgoAddress {
    ergo_tree: Vec<u8>,
    network: Network,
    address_type: AddressType,
}

impl ErgoAddress {
    pub fn from_ergo_tree(ergo_tree: Vec<u8>, network: Network) -> Self {
        let address_type = Self::get_ergo_tree_type(&ergo_tree);
        Self { ergo_tree, network, address_type }
    }

    pub fn from_ergo_tree_hex(ergo_tree_hex: &str, network: Network) -> Result<Self, AddressError> {
        let ergo_tree = hex::decode(ergo_tree_hex)?;
        Ok(Self::from_ergo_tree(ergo_tree, network))
    }

    pub fn decode(encoded: &str) -> Result<Self, AddressError> {
        let bytes = bs58::decode(encoded)
            .into_vec()
            .map_err(|_| AddressError::Base58DecodeError)?;
        let unpacked = Self::unpack_address(&bytes)?;

        if !Self::validate_checksum(&unpacked) {
            return Err(AddressError::InvalidChecksum);
        }

        Self::from_unpacked(unpacked)
    }

    /// Decode an address without validating the checksum.
    ///
    /// # When to use
    /// - Batch processing P2PK/P2SH addresses from trusted sources (e.g., blockchain data)
    /// - Hot paths where addresses were already validated elsewhere
    /// - ~16-17% faster for short addresses (P2PK, P2SH)
    ///
    /// # When it's useless
    /// - Long P2S addresses: base58 decoding dominates, checksum overhead is <1%
    /// - User input: always use `decode()` to catch typos/corruption
    /// - One-off operations: the ~200ns saved won't matter
    pub fn decode_unsafe(encoded: &str) -> Result<Self, AddressError> {
        let bytes = bs58::decode(encoded)
            .into_vec()
            .map_err(|_| AddressError::Base58DecodeError)?;
        let unpacked = Self::unpack_address(&bytes)?;
        Self::from_unpacked(unpacked)
    }

    pub fn validate(encoded: &str) -> bool {
        Self::decode(encoded).is_ok()
    }

    pub fn get_network_type(encoded: &str) -> Result<Network, AddressError> {
        let bytes = bs58::decode(encoded)
            .into_vec()
            .map_err(|_| AddressError::Base58DecodeError)?;
        if bytes.is_empty() {
            return Err(AddressError::AddressTooShort);
        }
        Ok(Network::from_head_byte(bytes[0]))
    }

    pub fn get_address_type(encoded: &str) -> Result<AddressType, AddressError> {
        let bytes = bs58::decode(encoded)
            .into_vec()
            .map_err(|_| AddressError::Base58DecodeError)?;
        if bytes.is_empty() {
            return Err(AddressError::AddressTooShort);
        }
        AddressType::from_head_byte(bytes[0]).ok_or(AddressError::InvalidAddressType)
    }

    pub fn encode(&self) -> String {
        self.encode_for_network(self.network)
    }

    pub fn encode_for_network(&self, network: Network) -> String {
        let body: &[u8] = match self.address_type {
            AddressType::P2PK => &self.ergo_tree[P2PK_ERGOTREE_PREFIX.len()..],
            AddressType::P2SH => {
                &self.ergo_tree
                    [P2SH_ERGOTREE_PREFIX.len()..P2SH_ERGOTREE_PREFIX.len() + P2SH_HASH_LENGTH]
            }
            AddressType::P2S => &self.ergo_tree,
        };

        encode_address(network, self.address_type, body)
    }

    pub fn ergo_tree_hex(&self) -> String {
        self.ergo_tree.encode_hex()
    }

    pub fn ergo_tree_bytes(&self) -> &[u8] {
        &self.ergo_tree
    }

    pub fn into_ergo_tree(self) -> Vec<u8> {
        self.ergo_tree
    }

    pub fn network(&self) -> Network {
        self.network
    }

    pub fn address_type(&self) -> AddressType {
        self.address_type
    }

    pub fn get_public_key(&self) -> Option<&[u8]> {
        if self.address_type == AddressType::P2PK {
            Some(&self.ergo_tree[P2PK_ERGOTREE_PREFIX.len()..])
        } else {
            None
        }
    }

    fn get_ergo_tree_type(ergo_tree: &[u8]) -> AddressType {
        if ergo_tree.len() == P2PK_ERGOTREE_LENGTH && ergo_tree.starts_with(&P2PK_ERGOTREE_PREFIX) {
            return AddressType::P2PK;
        }

        if ergo_tree.len() == P2SH_ERGOTREE_LENGTH
            && ergo_tree.starts_with(&P2SH_ERGOTREE_PREFIX)
            && ergo_tree.ends_with(&P2SH_ERGOTREE_SUFFIX)
        {
            return AddressType::P2SH;
        }

        AddressType::P2S
    }

    fn unpack_address(bytes: &[u8]) -> Result<UnpackedAddress, AddressError> {
        if bytes.len() < 5 {
            return Err(AddressError::AddressTooShort);
        }

        let head = bytes[0];
        let body = &bytes[1..bytes.len() - CHECKSUM_LENGTH];
        let checksum = &bytes[bytes.len() - CHECKSUM_LENGTH..];

        let network = Network::from_head_byte(head);
        let address_type =
            AddressType::from_head_byte(head).ok_or(AddressError::InvalidAddressType)?;

        Ok(UnpackedAddress {
            head,
            body: body.to_vec(),
            checksum: checksum.to_vec(),
            network,
            address_type,
        })
    }

    fn validate_checksum(unpacked: &UnpackedAddress) -> bool {
        let mut content = vec![unpacked.head];
        content.extend_from_slice(&unpacked.body);

        let hash = blake2b256(&content);
        hash[..CHECKSUM_LENGTH] == unpacked.checksum
    }

    fn from_unpacked(unpacked: UnpackedAddress) -> Result<Self, AddressError> {
        match unpacked.address_type {
            AddressType::P2PK if unpacked.body.len() != P2PK_PUBKEY_LENGTH => {
                return Err(AddressError::InvalidErgoTree);
            }
            AddressType::P2SH if unpacked.body.len() != P2SH_HASH_LENGTH => {
                return Err(AddressError::InvalidErgoTree);
            }
            _ => {}
        }

        let ergo_tree = match unpacked.address_type {
            AddressType::P2PK => {
                let mut tree = P2PK_ERGOTREE_PREFIX.to_vec();
                tree.extend_from_slice(&unpacked.body);
                tree
            }
            AddressType::P2SH => {
                let mut tree = P2SH_ERGOTREE_PREFIX.to_vec();
                tree.extend_from_slice(&unpacked.body);
                tree.extend_from_slice(&P2SH_ERGOTREE_SUFFIX);
                tree
            }
            AddressType::P2S => unpacked.body.clone(),
        };

        Ok(Self { ergo_tree, network: unpacked.network, address_type: unpacked.address_type })
    }
}

impl std::fmt::Display for ErgoAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.encode())
    }
}

struct UnpackedAddress {
    head: u8,
    body: Vec<u8>,
    checksum: Vec<u8>,
    network: Network,
    address_type: AddressType,
}

pub fn tree_to_base58(ergo_tree: &[u8], network: Network) -> Result<String, AddressError> {
    if ergo_tree.is_empty() {
        return Err(AddressError::InvalidErgoTree);
    }

    let address_type = ErgoAddress::get_ergo_tree_type(ergo_tree);
    let body: &[u8] = match address_type {
        AddressType::P2PK => &ergo_tree[P2PK_ERGOTREE_PREFIX.len()..],
        AddressType::P2SH => {
            &ergo_tree[P2SH_ERGOTREE_PREFIX.len()..P2SH_ERGOTREE_PREFIX.len() + P2SH_HASH_LENGTH]
        }
        AddressType::P2S => ergo_tree,
    };

    Ok(encode_address(network, address_type, body))
}

pub fn base58_to_tree(encoded: &str) -> Result<Vec<u8>, AddressError> {
    Ok(ErgoAddress::decode(encoded)?.into_ergo_tree())
}

fn encode_address(network: Network, address_type: AddressType, body: &[u8]) -> String {
    let head = network as u8 + address_type as u8;

    let mut content = vec![head];
    content.extend_from_slice(body);

    let hash = blake2b256(&content);
    let checksum = &hash[..CHECKSUM_LENGTH];

    content.extend_from_slice(checksum);

    bs58::encode(&content).into_string()
}

fn blake2b256(data: &[u8]) -> [u8; BLAKE_256_HASH_LENGTH] {
    let mut hasher = Blake2b::<blake2::digest::consts::U32>::new();
    hasher.update(data);
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEE_CONTRACT: &str = "1005040004000e36100204a00b08cd0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798ea02d192a39a8cc7a701730073011001020402d19683030193a38cc7b2a57300000193c2b2a57301007473027303830108cdeeac93b1a57304";
    const FEE_MAINNET_ADDRESS: &str = "2iHkR7CWvD1R4j1yZg5bkeDRQavjAaVPeTDFGGLZduHyfWMuYpmhHocX8GJoaieTx78FntzJbCBVL6rf96ocJoZdmWBL2fci7NqWgAirppPQmZ7fN9V6z13Ay6brPriBKYqLp1bT2Fk4FkFLCfdPpe";
    const FEE_TESTNET_ADDRESS: &str = "Bf1X9JgQTUtgntaer91B24n6kP8L2kqEiQqNf1z97BKo9UbnW3WRP9VXu8BXd1LsYCiYbHJEdWKxkF5YNx5n7m31wsDjbEuB3B13ZMDVBWkepGmWfGa71otpFViHDCuvbw1uNicAQnfuWfnj8fbCa4";
    const P2S_LONG_TREE: &str = "101004020e36100204a00b08cd0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798ea02d192a39a8cc7a7017300730110010204020404040004c0fd4f05808c82f5f6030580b8c9e5ae040580f882ad16040204c0944004c0f407040004000580f882ad16d19683030191a38cc7a7019683020193c2b2a57300007473017302830108cdeeac93a38cc7b2a573030001978302019683040193b1a5730493c2a7c2b2a573050093958fa3730673079973089c73097e9a730a9d99a3730b730c0599c1a7c1b2a5730d00938cc7b2a5730e0001a390c1a7730f";
    const P2S_LONG_ADDRESS: &str = "2Z4YBkDsDvQj8BX7xiySFewjitqp2ge9c99jfes2whbtKitZTxdBYqbrVZUvZvKv6aqn9by4kp3LE1c26LCyosFnVnm6b6U1JYvWpYmL2ZnixJbXLjWAWuBThV1D6dLpqZJYQHYDznJCk49g5TUiS4q8khpag2aNmHwREV7JSsypHdHLgJT7MGaw51aJfNubyzSKxZ4AJXFS27EfXwyCLzW1K6GVqwkJtCoPvrcLqmqwacAWJPkmh78nke9H4oT88XmSbRt2n9aWZjosiZCafZ4osUDxmZcc5QVEeTWn8drSraY3eFKe8Mu9MSCcVU";

    #[test]
    fn test_decode_p2pk_mainnet() {
        let address =
            ErgoAddress::decode("9fRusAarL1KkrWQVsxSRVYnvWxaAT2A96cKtNn9tvPh5XUyCisr").unwrap();
        assert_eq!(address.address_type(), AddressType::P2PK);
        assert_eq!(address.network(), Network::Mainnet);
        assert_eq!(
            address.ergo_tree_hex(),
            "0008cd0278011ec0cf5feb92d61adb51dcb75876627ace6fd9446ab4cabc5313ab7b39a7"
        );
    }

    #[test]
    fn test_decode_p2pk_testnet() {
        let address =
            ErgoAddress::decode("3Wx6cHkTaavysMMXSqqvoCL1n273NmcH3auiHymFwTSpKDFzQfW3").unwrap();
        assert_eq!(address.address_type(), AddressType::P2PK);
        assert_eq!(address.network(), Network::Testnet);
    }

    #[test]
    fn test_decode_p2s_mainnet() {
        let address = ErgoAddress::decode(FEE_MAINNET_ADDRESS).unwrap();
        assert_eq!(address.address_type(), AddressType::P2S);
        assert_eq!(address.network(), Network::Mainnet);
        assert_eq!(address.ergo_tree_hex(), FEE_CONTRACT);
    }

    #[test]
    fn test_decode_p2s_testnet() {
        let address = ErgoAddress::decode(FEE_TESTNET_ADDRESS).unwrap();
        assert_eq!(address.address_type(), AddressType::P2S);
        assert_eq!(address.network(), Network::Testnet);
        assert_eq!(address.ergo_tree_hex(), FEE_CONTRACT);
    }

    #[test]
    fn test_decode_p2s_long() {
        let address = ErgoAddress::decode(P2S_LONG_ADDRESS).unwrap();
        assert_eq!(address.address_type(), AddressType::P2S);
        assert_eq!(address.network(), Network::Mainnet);
        assert_eq!(address.ergo_tree_hex(), P2S_LONG_TREE);
    }

    #[test]
    fn test_decode_p2sh() {
        let address = ErgoAddress::decode("8sZ2fVu5VUQKEmWt4xRRDBYzuw5aevhhziPBDGB").unwrap();
        assert_eq!(address.address_type(), AddressType::P2SH);
        assert_eq!(address.network(), Network::Mainnet);
        assert_eq!(
            address.ergo_tree_hex(),
            "00ea02d193b4cbe4e3010e040004300e18fd53c43ebbc8b5c53f2ccf270d1bc22740eb3855463f5faed40801"
        );
    }

    #[test]
    fn test_invalid_checksum() {
        let result = ErgoAddress::decode("9fRusAarL1KkrWQVsxSRVYnvWxaAT2A96cKtNn9tvPh5XUyCiss");
        assert!(result.is_err());
    }

    #[test]
    fn test_from_ergo_tree_p2pk() {
        let ergo_tree = "0008cd0278011ec0cf5feb92d61adb51dcb75876627ace6fd9446ab4cabc5313ab7b39a7";
        let address = ErgoAddress::from_ergo_tree_hex(ergo_tree, Network::Mainnet).unwrap();
        assert_eq!(address.address_type(), AddressType::P2PK);
        assert_eq!(address.encode(), "9fRusAarL1KkrWQVsxSRVYnvWxaAT2A96cKtNn9tvPh5XUyCisr");
    }

    #[test]
    fn test_from_ergo_tree_p2sh() {
        let ergo_tree = "00ea02d193b4cbe4e3010e040004300e18fd53c43ebbc8b5c53f2ccf270d1bc22740eb3855463f5faed40801";
        let address = ErgoAddress::from_ergo_tree_hex(ergo_tree, Network::Mainnet).unwrap();
        assert_eq!(address.address_type(), AddressType::P2SH);
        assert_eq!(address.encode(), "8sZ2fVu5VUQKEmWt4xRRDBYzuw5aevhhziPBDGB");
    }

    #[test]
    fn test_from_ergo_tree_p2s() {
        let address = ErgoAddress::from_ergo_tree_hex(FEE_CONTRACT, Network::Mainnet).unwrap();
        assert_eq!(address.address_type(), AddressType::P2S);
        assert_eq!(address.encode(), FEE_MAINNET_ADDRESS);
    }

    #[test]
    fn test_encode_for_different_network() {
        let address = ErgoAddress::from_ergo_tree_hex(FEE_CONTRACT, Network::Mainnet).unwrap();
        assert_eq!(address.encode_for_network(Network::Mainnet), FEE_MAINNET_ADDRESS);
        assert_eq!(address.encode_for_network(Network::Testnet), FEE_TESTNET_ADDRESS);
    }

    #[test]
    fn test_p2s_long_roundtrip() {
        let address = ErgoAddress::from_ergo_tree_hex(P2S_LONG_TREE, Network::Mainnet).unwrap();
        assert_eq!(address.address_type(), AddressType::P2S);
        assert_eq!(address.encode(), P2S_LONG_ADDRESS);
    }

    #[test]
    fn test_roundtrip_p2pk() {
        let addresses = [
            "9fRusAarL1KkrWQVsxSRVYnvWxaAT2A96cKtNn9tvPh5XUyCisr",
            "9hY16vzHmmfyVBwKeFGHvb2bMFsG94A1u7To1QWtUokACyFVENQ",
            "9emAvMvreC9QEGHLV9pupwmteHuJt62qvkH6HnPjUESgQRotfaC",
        ];
        for addr in addresses {
            let decoded = ErgoAddress::decode(addr).unwrap();
            assert_eq!(decoded.encode(), addr);
        }
    }

    #[test]
    fn test_roundtrip_p2sh() {
        let test_vectors = [
            (
                "8sZ2fVu5VUQKEmWt4xRRDBYzuw5aevhhziPBDGB",
                "00ea02d193b4cbe4e3010e040004300e18fd53c43ebbc8b5c53f2ccf270d1bc22740eb3855463f5faed40801",
            ),
            (
                "7g5LhysK7mxX8xmZdPLtFE42wwxGFjpp8VofStb",
                "00ea02d193b4cbe4e3010e040004300e1888dc65bcf63bb55e6c2bfe03b1f2b14eef7d4fe0fa32d8e8d40801",
            ),
            (
                "8UApt8czfFVuTgQmMwtsRBZ4nfWquNiSwCWUjMg",
                "00ea02d193b4cbe4e3010e040004300e18d62151f990f191c102a6fe995b89ed3d0f343a96f13789a3d40801",
            ),
        ];

        for (encoded, ergo_tree) in test_vectors {
            let from_address = ErgoAddress::decode(encoded).unwrap();
            assert_eq!(from_address.ergo_tree_hex(), ergo_tree);
            assert_eq!(from_address.encode(), encoded);

            let from_tree = ErgoAddress::from_ergo_tree_hex(ergo_tree, Network::Mainnet).unwrap();
            assert_eq!(from_tree.encode(), encoded);
        }
    }

    #[test]
    fn test_public_key_extraction() {
        let test_vectors = [
            (
                "038d39af8c37583609ff51c6a577efe60684119da2fbd0d75f9c72372886a58a63",
                "9hY16vzHmmfyVBwKeFGHvb2bMFsG94A1u7To1QWtUokACyFVENQ",
            ),
            (
                "02200a1c1b8fa17ec82de54bcaef96f23d7b34196c0410f6f578abdbf163b14b25",
                "9emAvMvreC9QEGHLV9pupwmteHuJt62qvkH6HnPjUESgQRotfaC",
            ),
        ];

        for (public_key, base58) in test_vectors {
            let address = ErgoAddress::decode(base58).unwrap();
            let pk = address.get_public_key().unwrap();
            assert_eq!(hex::encode(pk), public_key);
        }
    }

    #[test]
    fn test_validate() {
        assert!(ErgoAddress::validate("9iPgSVU3yrRnTxtJC6hYA7bS5mMqZtjeJHrT3fNdLV7JZVpY5By"));
        assert!(ErgoAddress::validate("3Wx6cHkTaavysMMXSqqvoCL1n273NmcH3auiHymFwTSpKDFzQfW3"));
        assert!(ErgoAddress::validate(FEE_MAINNET_ADDRESS));
        assert!(ErgoAddress::validate(FEE_TESTNET_ADDRESS));
        assert!(ErgoAddress::validate("8sZ2fVu5VUQKEmWt4xRRDBYzuw5aevhhziPBDGB"));
        assert!(ErgoAddress::validate("7g5LhysK7mxX8xmZdPLtFE42wwxGFjpp8VofStb"));
        assert!(ErgoAddress::validate("8UApt8czfFVuTgQmMwtsRBZ4nfWquNiSwCWUjMg"));

        assert!(!ErgoAddress::validate("9i3g6d958MpZAqwn9hrTHcqbBiY5VPYBBY6vRDszZn4koqnahin"));
        assert!(!ErgoAddress::validate("9eBy"));
    }

    #[test]
    fn test_get_network_type() {
        assert_eq!(ErgoAddress::get_network_type(FEE_MAINNET_ADDRESS).unwrap(), Network::Mainnet);
        assert_eq!(ErgoAddress::get_network_type(FEE_TESTNET_ADDRESS).unwrap(), Network::Testnet);
        assert_eq!(
            ErgoAddress::get_network_type("9iPgSVU3yrRnTxtJC6hYA7bS5mMqZtjeJHrT3fNdLV7JZVpY5By")
                .unwrap(),
            Network::Mainnet
        );
        assert_eq!(
            ErgoAddress::get_network_type("3Wx6cHkTaavysMMXSqqvoCL1n273NmcH3auiHymFwTSpKDFzQfW3")
                .unwrap(),
            Network::Testnet
        );
    }

    #[test]
    fn test_get_address_type() {
        assert_eq!(
            ErgoAddress::get_address_type("9iPgSVU3yrRnTxtJC6hYA7bS5mMqZtjeJHrT3fNdLV7JZVpY5By")
                .unwrap(),
            AddressType::P2PK
        );
        assert_eq!(ErgoAddress::get_address_type(FEE_MAINNET_ADDRESS).unwrap(), AddressType::P2S);
        assert_eq!(
            ErgoAddress::get_address_type("8sZ2fVu5VUQKEmWt4xRRDBYzuw5aevhhziPBDGB").unwrap(),
            AddressType::P2SH
        );
    }

    #[test]
    fn test_ergo_ts_test_vectors() {
        let test_vectors = [
            (
                "9fRusAarL1KkrWQVsxSRVYnvWxaAT2A96cKtNn9tvPh5XUyCisr",
                "0008cd0278011ec0cf5feb92d61adb51dcb75876627ace6fd9446ab4cabc5313ab7b39a7",
                Network::Mainnet,
                true,
            ),
            ("9gsLq5a12nJe33nKtjMe7NPY7o8CQAtjS9amDgALbebv1wmRXrv", "", Network::Mainnet, true),
            ("9gU3czAt9q4fQPRWBriBbpfLbRP7JrXRmB7kowtwdyw66PMRmaY", "", Network::Mainnet, true),
            ("3WxxVQqxoVSWEKG5B73eNttBX51ZZ6WXLW7fiVDgCFhzRK8R4gmk", "", Network::Testnet, true),
            (
                "2Z4YBkDsDvQj8BX7xiySFewjitqp2ge9c99jfes2whbtKitZTxdBYqbrVZUvZvKv6aqn9by4kp3LE1c26LCyosFnVnm6b6U1JYvWpYmL2ZnixJbXLjWAWuBThV1D6dLpqZJYQHYDznJCk49g5TUiS4q8khpag2aNmHwREV7JSsypHdHLgJT7MGaw51aJfNubyzSKxZ4AJXFS27EfXwyCLzW1K6GVqwkJtCoPvrcLqmqwacAWJPkmh78nke9H4oT88XmSbRt2n9aWZjosiZCafZ4osUDxmZcc5QVEeTWn8drSraY3eFKe8Mu9MSCcVU",
                "101004020e36100204a00b08cd0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798ea02d192a39a8cc7a7017300730110010204020404040004c0fd4f05808c82f5f6030580b8c9e5ae040580f882ad16040204c0944004c0f407040004000580f882ad16d19683030191a38cc7a7019683020193c2b2a57300007473017302830108cdeeac93a38cc7b2a573030001978302019683040193b1a5730493c2a7c2b2a573050093958fa3730673079973089c73097e9a730a9d99a3730b730c0599c1a7c1b2a5730d00938cc7b2a5730e0001a390c1a7730f",
                Network::Mainnet,
                true,
            ),
            (
                "88dhgzEuTXaSLUWK1Ro8mB5xfhwP4y8osUycdBV16EBgycjcBebwd2He7QGiXC1qiSM1KZ6bAcpE2iCv",
                "",
                Network::Mainnet,
                true,
            ),
            (
                "9fMPy1XY3GW4T6t3LjYofqmzER6x9cV21n5UVJTWmma4Y9mAW6c",
                "0008cd026dc059d64a50d0dbf07755c2c4a4e557e3df8afa7141868b3ab200643d437ee7",
                Network::Mainnet,
                true,
            ),
            ("9fRusAarL1KkrWQVsxSRVYnvWxaAT2A96cKtNn9tvPh5XUyCiss", "", Network::Mainnet, false),
            ("9fRusAarL1KkrWQVsxSRVYnvWxaAT2A96c", "", Network::Mainnet, false),
            (P2S_LONG_ADDRESS, P2S_LONG_TREE, Network::Mainnet, true),
            (FEE_MAINNET_ADDRESS, FEE_CONTRACT, Network::Mainnet, true),
            (FEE_TESTNET_ADDRESS, FEE_CONTRACT, Network::Testnet, true),
        ];

        for (address, ergo_tree, expected_network, is_valid) in test_vectors {
            assert_eq!(
                ErgoAddress::validate(address),
                is_valid,
                "Validation mismatch for {}",
                address
            );

            if is_valid {
                let decoded = ErgoAddress::decode(address).unwrap();
                assert_eq!(decoded.network(), expected_network, "Network mismatch for {}", address);

                if !ergo_tree.is_empty() {
                    assert_eq!(
                        decoded.ergo_tree_hex(),
                        ergo_tree,
                        "ErgoTree mismatch for {}",
                        address
                    );

                    let from_tree =
                        ErgoAddress::from_ergo_tree_hex(ergo_tree, expected_network).unwrap();
                    assert_eq!(from_tree.encode(), address, "Roundtrip failed for {}", address);
                }
            }
        }
    }

    #[test]
    fn test_public_key_test_vectors() {
        let test_vectors = [
            (
                "038d39af8c37583609ff51c6a577efe60684119da2fbd0d75f9c72372886a58a63",
                "9hY16vzHmmfyVBwKeFGHvb2bMFsG94A1u7To1QWtUokACyFVENQ",
            ),
            (
                "02200a1c1b8fa17ec82de54bcaef96f23d7b34196c0410f6f578abdbf163b14b25",
                "9emAvMvreC9QEGHLV9pupwmteHuJt62qvkH6HnPjUESgQRotfaC",
            ),
            (
                "02f4e68cc26759e7b6dc63505c3427b2d565ab839e7f80306b2ce9d1c7def94cfa",
                "9gNvAv97W71Wm33GoXgSQBFJxinFubKvE6wh2dEhFTSgYEe783j",
            ),
            (
                "02fd223c95ba74d48d04a8ecb5e86eda30df4e22f46aacc299f59230a9f8e93366",
                "9gSYUbWtusShcjVPQR4NbPcavCTcce2z38iZgxwZaxWmMt7zLDE",
            ),
            (
                "025fb675cfd8a58210d6b7dbb56d02c3b5fd37431fa50f600e21d0977e4c32f6c4",
                "9fFDNKVyC6LLyRGZY8pJh964oKz7RPFMhTmRgVjSvNm96iDSBcg",
            ),
            (
                "03ce25569fa8f219c6411159f22820940553e53b1e3993f2d18ceda4e36f51a2e9",
                "9i2bQmRpCPLmDdVgBNyeAy7dDXqBQfjvcxVVt5YMzbDud6AvJS8",
            ),
            (
                "0371ae73c460c888d224bf268622f80563032b5f5a6c746f73d9f58543e8afe728",
                "9hKsXnZXXZqzoxBwuk3Vn1sRtGJHvs3Fn67uPN9KK9wxk4GSEqR",
            ),
            (
                "032da0d1beaa729d4645a84a3cfc30c5b423c7c531ccaed92ada9da190371fcc4f",
                "9gouChj1vGQBxZ9VQxWbnjguWwWAKvEbqRMr6ERQ4ffndUHWeYF",
            ),
            (
                "03abae6e65bed69e7f3493299876172b9111ed236195cd4adb345eb2351dac9d2d",
                "9hmR4Xh9mRQEV2JvkFzsTVDEbxqz9Y9ukV2ojuKyUigvNXn6Tkz",
            ),
            (
                "0316a1356adb2b965097d1cd6e4e47137be0e4e4a392604b2905b345a8d0f3a172",
                "9gdmjiWxCJZg3DzcZNri4THvLyxok7z3QGhMiSYFnJqiKRudKUw",
            ),
        ];

        for (public_key, base58) in test_vectors {
            let address = ErgoAddress::decode(base58).unwrap();
            let pk = address.get_public_key().unwrap();
            assert_eq!(hex::encode(pk), public_key, "Public key mismatch for {}", base58);

            let ergo_tree = format!("0008cd{}", public_key);
            let from_tree = ErgoAddress::from_ergo_tree_hex(&ergo_tree, Network::Mainnet).unwrap();
            assert_eq!(
                from_tree.encode(),
                base58,
                "Address mismatch for public key {}",
                public_key
            );
        }
    }

    #[test]
    fn test_base58_to_tree_helpers() {
        let tree = base58_to_tree(FEE_MAINNET_ADDRESS).unwrap();
        assert_eq!(hex::encode(tree), FEE_CONTRACT);

        let tree = base58_to_tree(P2S_LONG_ADDRESS).unwrap();
        assert_eq!(hex::encode(tree), P2S_LONG_TREE);

        let tree = base58_to_tree("9fRusAarL1KkrWQVsxSRVYnvWxaAT2A96cKtNn9tvPh5XUyCisr").unwrap();
        assert_eq!(
            hex::encode(tree),
            "0008cd0278011ec0cf5feb92d61adb51dcb75876627ace6fd9446ab4cabc5313ab7b39a7"
        );
    }

    #[test]
    fn test_tree_to_base58_helpers() {
        let fee_tree = hex::decode(FEE_CONTRACT).unwrap();
        assert_eq!(tree_to_base58(&fee_tree, Network::Mainnet).unwrap(), FEE_MAINNET_ADDRESS);
        assert_eq!(tree_to_base58(&fee_tree, Network::Testnet).unwrap(), FEE_TESTNET_ADDRESS);

        let long_tree = hex::decode(P2S_LONG_TREE).unwrap();
        assert_eq!(tree_to_base58(&long_tree, Network::Mainnet).unwrap(), P2S_LONG_ADDRESS);
    }

    #[test]
    fn test_tree_to_base58_rejects_empty_tree() {
        let err = tree_to_base58(&[], Network::Mainnet).unwrap_err();
        matches!(err, AddressError::InvalidErgoTree);
    }
}
