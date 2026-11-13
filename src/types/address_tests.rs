//! Static vectors from Fleet SDK (`ergoAddress.spec.ts`, `ergoAddressesTestVectors.ts`).
//! P2SH trees are the node's (`/script/addressToTree`, node 6.0.8).

use super::*;

const FEE_CONTRACT: &str = "1005040004000e36100204a00b08cd0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798ea02d192a39a8cc7a701730073011001020402d19683030193a38cc7b2a57300000193c2b2a57301007473027303830108cdeeac93b1a57304";
const FEE_MAINNET: &str = "2iHkR7CWvD1R4j1yZg5bkeDRQavjAaVPeTDFGGLZduHyfWMuYpmhHocX8GJoaieTx78FntzJbCBVL6rf96ocJoZdmWBL2fci7NqWgAirppPQmZ7fN9V6z13Ay6brPriBKYqLp1bT2Fk4FkFLCfdPpe";
const FEE_TESTNET: &str = "Bf1X9JgQTUtgntaer91B24n6kP8L2kqEiQqNf1z97BKo9UbnW3WRP9VXu8BXd1LsYCiYbHJEdWKxkF5YNx5n7m31wsDjbEuB3B13ZMDVBWkepGmWfGa71otpFViHDCuvbw1uNicAQnfuWfnj8fbCa4";

/// (address, ErgoTree, network)
const ADDRESS_TREES: &[(&str, &str, Network)] = &[
    (
        "9fRusAarL1KkrWQVsxSRVYnvWxaAT2A96cKtNn9tvPh5XUyCisr",
        "0008cd0278011ec0cf5feb92d61adb51dcb75876627ace6fd9446ab4cabc5313ab7b39a7",
        Network::Mainnet,
    ),
    (
        "9fMPy1XY3GW4T6t3LjYofqmzER6x9cV21n5UVJTWmma4Y9mAW6c",
        "0008cd026dc059d64a50d0dbf07755c2c4a4e557e3df8afa7141868b3ab200643d437ee7",
        Network::Mainnet,
    ),
    (
        "9gsLq5a12nJe33nKtjMe7NPY7o8CQAtjS9amDgALbebv1wmRXrv",
        "0008cd0335717e8ea294d927f59ded0ca77e40bae13853438a8a67fc14e46d411caa4d02",
        Network::Mainnet,
    ),
    (
        "2Z4YBkDsDvQj8BX7xiySFewjitqp2ge9c99jfes2whbtKitZTxdBYqbrVZUvZvKv6aqn9by4kp3LE1c26LCyosFnVnm6b6U1JYvWpYmL2ZnixJbXLjWAWuBThV1D6dLpqZJYQHYDznJCk49g5TUiS4q8khpag2aNmHwREV7JSsypHdHLgJT7MGaw51aJfNubyzSKxZ4AJXFS27EfXwyCLzW1K6GVqwkJtCoPvrcLqmqwacAWJPkmh78nke9H4oT88XmSbRt2n9aWZjosiZCafZ4osUDxmZcc5QVEeTWn8drSraY3eFKe8Mu9MSCcVU",
        "101004020e36100204a00b08cd0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798ea02d192a39a8cc7a7017300730110010204020404040004c0fd4f05808c82f5f6030580b8c9e5ae040580f882ad16040204c0944004c0f407040004000580f882ad16d19683030191a38cc7a7019683020193c2b2a57300007473017302830108cdeeac93a38cc7b2a573030001978302019683040193b1a5730493c2a7c2b2a573050093958fa3730673079973089c73097e9a730a9d99a3730b730c0599c1a7c1b2a5730d00938cc7b2a5730e0001a390c1a7730f",
        Network::Mainnet,
    ),
    (FEE_MAINNET, FEE_CONTRACT, Network::Mainnet),
    (FEE_TESTNET, FEE_CONTRACT, Network::Testnet),
    (
        "8sZ2fVu5VUQKEmWt4xRRDBYzuw5aevhhziPBDGB",
        "00ea02d193b4cbe4e37e0e040004300e18fd53c43ebbc8b5c53f2ccf270d1bc22740eb3855463f5faed4087e",
        Network::Mainnet,
    ),
    (
        "7g5LhysK7mxX8xmZdPLtFE42wwxGFjpp8VofStb",
        "00ea02d193b4cbe4e37e0e040004300e1888dc65bcf63bb55e6c2bfe03b1f2b14eef7d4fe0fa32d8e8d4087e",
        Network::Mainnet,
    ),
    (
        "8UApt8czfFVuTgQmMwtsRBZ4nfWquNiSwCWUjMg",
        "00ea02d193b4cbe4e37e0e040004300e18d62151f990f191c102a6fe995b89ed3d0f343a96f13789a3d4087e",
        Network::Mainnet,
    ),
];

/// Valid addresses without a tree in the vectors: (address, network, type).
const VALID: &[(&str, Network, AddressType)] = &[
    ("9gU3czAt9q4fQPRWBriBbpfLbRP7JrXRmB7kowtwdyw66PMRmaY", Network::Mainnet, AddressType::P2PK),
    ("9iPgSVU3yrRnTxtJC6hYA7bS5mMqZtjeJHrT3fNdLV7JZVpY5By", Network::Mainnet, AddressType::P2PK),
    ("3WxxVQqxoVSWEKG5B73eNttBX51ZZ6WXLW7fiVDgCFhzRK8R4gmk", Network::Testnet, AddressType::P2PK),
    ("3Wx6cHkTaavysMMXSqqvoCL1n273NmcH3auiHymFwTSpKDFzQfW3", Network::Testnet, AddressType::P2PK),
    (
        "88dhgzEuTXaSLUWK1Ro8mB5xfhwP4y8osUycdBV16EBgycjcBebwd2He7QGiXC1qiSM1KZ6bAcpE2iCv",
        Network::Mainnet,
        AddressType::P2S,
    ),
];

const INVALID: &[&str] = &[
    "9fRusAarL1KkrWQVsxSRVYnvWxaAT2A96cKtNn9tvPh5XUyCiss",
    "9fRusAarL1KkrWQVsxSRVYnvWxaAT2A96c",
    "3Wx6cHkTaAvysMMXSqqvoCL1n273NmcH3auiHymFwTSpKDFzQfW3",
    "9i3g6d958MpZAqwn9hrTHcqbBiY5VPYBBY6vRDszZn4koqnahin",
    "9eBy",
    "",
    "0OIl",
];

fn tree(hex: &str) -> ErgoTree {
    HexBytes(::hex::decode(hex).unwrap())
}

#[test]
fn address_to_tree_and_back() {
    for &(address, ergo_tree, network) in ADDRESS_TREES {
        let decoded = Address::decode(address).unwrap_or_else(|e| panic!("{address}: {e}"));
        assert_eq!(decoded.ergo_tree(), &tree(ergo_tree), "{address}");
        assert_eq!(decoded.network(), network, "{address}");
        assert_eq!(Address::from_ergo_tree(tree(ergo_tree), network).to_string(), address);
    }
}

#[test]
fn address_types() {
    let types: Vec<_> = ADDRESS_TREES
        .iter()
        .map(|(a, ..)| Address::decode(a).unwrap().address_type())
        .collect();
    use AddressType::*;
    assert_eq!(types, [P2PK, P2PK, P2PK, P2S, P2S, P2S, P2SH, P2SH, P2SH]);
}

#[test]
fn valid_addresses_round_trip() {
    for &(address, network, address_type) in VALID {
        let decoded = Address::decode(address).unwrap_or_else(|e| panic!("{address}: {e}"));
        assert_eq!(decoded.network(), network, "{address}");
        assert_eq!(decoded.address_type(), address_type, "{address}");
        assert_eq!(decoded.to_string(), address);
    }
}

#[test]
fn invalid_addresses() {
    for address in INVALID {
        assert!(Address::decode(address).is_err(), "{address} decoded");
    }
    assert_eq!(Address::decode("9eBy"), Err(AddressError::TooShort));
    assert_eq!(Address::decode("0OIl"), Err(AddressError::Base58));
    assert_eq!(
        Address::decode("9fRusAarL1KkrWQVsxSRVYnvWxaAT2A96cKtNn9tvPh5XUyCiss"),
        Err(AddressError::Checksum)
    );
}

#[test]
fn valid_checksum_but_invalid_public_key() {
    for address in [
        "9dg7gpByCWzoXdx5VeCvHeQYjx3q2TuTjzHqHfrsMsHszG49Rfj",
        "2xgTiYUcGDwvJ41XhN2nxGajWkFa3xy9pXCMEKS8fwT8QpR19R",
    ] {
        assert_eq!(Address::decode(address), Err(AddressError::InvalidPublicKey), "{address}");
    }

    let infinity = [0u8; PUBLIC_KEY_LENGTH];
    let mut uncompressed = vec![0x04];
    uncompressed.extend([7u8; 64]);
    let mut odd_prefix = vec![0x01];
    odd_prefix.extend([7u8; 32]);
    for key in [&infinity[..], &uncompressed, &odd_prefix, &odd_prefix[..32]] {
        let address = encode_raw(0x01, key);
        assert_eq!(Address::decode(&address), Err(AddressError::InvalidPublicKey));
    }
}

#[test]
fn rejects_bad_heads_and_bodies() {
    assert_eq!(Address::decode(&encode_raw(0x01, &[])), Err(AddressError::EmptyBody));
    assert_eq!(
        Address::decode(&encode_raw(0x23, &[1, 2])),
        Err(AddressError::UnknownNetwork(0x20))
    );
    for address_type in [0x00, 0x04, 0x0f] {
        assert_eq!(
            Address::decode(&encode_raw(address_type, &[1, 2])),
            Err(AddressError::UnsupportedType(address_type))
        );
    }
    for len in [23, 25, 32] {
        assert_eq!(
            Address::decode(&encode_raw(0x02, &vec![9; len])),
            Err(AddressError::P2shHashLength(len))
        );
    }
}

#[test]
fn p2sh_tree_follows_the_node_not_fleet() {
    let fleet = tree(
        "00ea02d193b4cbe4e3010e040004300e18fd53c43ebbc8b5c53f2ccf270d1bc22740eb3855463f5faed40801",
    );
    let address = Address::from_ergo_tree(fleet, Network::Mainnet);
    assert_eq!(address.address_type(), AddressType::P2S);
}

#[test]
fn network_names() {
    assert_eq!("mainnet".parse::<Network>().unwrap(), Network::Mainnet);
    assert_eq!("testnet".parse::<Network>().unwrap(), Network::Testnet);
    assert!("devnet".parse::<Network>().is_err());
}
