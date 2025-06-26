//!
//! Address encoding, decoding, and construction utilities for the Midnight blockchain.
//!
//! Provides types and functions for working with bech32m-encoded Midnight addresses, including
//! support for network and type information, and conversion from wallet keys.

use bech32::Bech32m;
use thiserror::Error;

use midnight_node_ledger_helpers::*;

/// Errors that can occur when working with Midnight addresses
#[derive(Error, Debug)]
pub enum MidnightAddressError {
    #[error("prefix first part != 'mn'")]
    PrefixInvalidConstant,
    #[error("prefix missing type")]
    PrefixMissingType,
}

/// Represents a Midnight blockchain address with network and type information
#[derive(Debug, Clone)]
pub struct MidnightAddress {
    pub type_: String,
    pub network: Option<String>,
    pub data: Vec<u8>,
}

impl MidnightAddress {
    /// Decodes a bech32m-encoded Midnight address string
    pub fn decode(encoded_data: &str) -> Result<Self, MidnightAddressError> {
        let (hrp, data) = bech32::decode(encoded_data).expect("Failed while bech32 decoding");
        let prefix_parts = hrp.as_str().split('_').collect::<Vec<&str>>();
        prefix_parts
            .first()
            .filter(|c| *c == &"mn")
            .ok_or(MidnightAddressError::PrefixInvalidConstant)?;
        let type_ = prefix_parts
            .get(1)
            .ok_or(MidnightAddressError::PrefixMissingType)?
            .to_string();
        let network = prefix_parts.get(2).map(|s| s.to_string());

        Ok(Self {
            type_,
            network,
            data,
        })
    }

    /// Encodes the address to a bech32m string representation
    pub fn encode(&self) -> String {
        let network_str = match &self.network {
            Some(network) => format!("_{}", network),
            None => "".to_string(),
        };

        bech32::encode::<Bech32m>(
            bech32::Hrp::parse(&format!("mn_{}{}", self.type_, network_str))
                .expect("Failed while bech32 parsing"),
            &self.data,
        )
        .expect("Failed while bech32 encoding")
    }

    /// Creates a Midnight address from a wallet and network ID
    pub fn from_wallet<D: DB>(wallet: &Wallet<D>, network: NetworkId) -> Self {
        let network_str = match network {
            NetworkId::MainNet => None,
            NetworkId::DevNet => Some("dev".to_string()),
            NetworkId::TestNet => Some("test".to_string()),
            NetworkId::Undeployed => Some("undeployed".to_string()),
            _ => None,
        };

        let coin_pub_key = wallet.secret_keys.coin_public_key().0.0;
        let mut enc_pub_key = Vec::new();
        Serializable::serialize(&wallet.secret_keys.enc_public_key(), &mut enc_pub_key)
            .expect("Failed serializing secret keys");

        Self {
            type_: "shield-addr".to_string(),
            network: network_str,
            data: [&coin_pub_key[..], &enc_pub_key[..]].concat(),
        }
    }
}

/// Implements conversion from a MidnightAddress to a NetworkId.
///
/// This allows extracting the network identifier (e.g., MainNet, DevNet, TestNet, Undeployed)
/// from the network field of a Midnight address. Returns an error string if the network is unknown.
impl TryFrom<&MidnightAddress> for NetworkId {
    type Error = String;

    fn try_from(value: &MidnightAddress) -> Result<Self, Self::Error> {
        match value.network {
            Some(ref network) => match network.as_str() {
                "dev" => Ok(NetworkId::DevNet),
                "test" => Ok(NetworkId::TestNet),
                "undeployed" => Ok(NetworkId::Undeployed),
                _ => Err(network.to_string()),
            },
            None => Ok(NetworkId::MainNet),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bech32::{Bech32m, Hrp};

    #[test]
    fn test_parse() {
        let encoded_str = bech32::encode::<Bech32m>(
            Hrp::parse("mn_shield-addr_test").expect("Failed while bech32 parsing"),
            &[1, 2, 3],
        )
        .expect("Failed while bech32 encoding");
        let address =
            MidnightAddress::decode(&encoded_str).expect("Failed while decoding `MidnightAddress");
        assert_eq!(address.type_, "shield-addr".to_string());
        assert_eq!(address.network, Some("test".to_string()));
        assert_eq!(address.data, vec![1u8, 2u8, 3u8]);
    }
}
