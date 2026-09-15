// crates/aws-kms-omega-adapter/src/envelope.rs
//! EIP-1559 RLP envelope signing using AWS KMS for the digest signature.
//!
//! Field order matches EIP-1559 (and Omega's `encode_eip1559_*` helpers):
//!   unsigned: 0x02 || rlp([chain_id, nonce, max_priority_fee, max_fee,
//!                          gas_limit, to, value, data, access_list])
//!   digest  = keccak256(unsigned)
//!   signed  = 0x02 || rlp([..., y_parity, r, s])

use crate::error::AdapterError;
use alloy_primitives::{Address, U256};
use aws_kms_signer::AwsKmsSigner;
use rlp::RlpStream;
use sha3::{Digest, Keccak256};

/// Sign an EIP-1559 transaction envelope with AWS KMS.
///
/// `data` is the already-built outer calldata (e.g. `execute(bytes,bytes)`).
/// This is the exact boundary where production today does:
///   `tx_key_manager.active_secret_key()` + `secp.sign_ecdsa_recoverable`.
pub async fn sign_eip1559_envelope(
    kms: &AwsKmsSigner,
    chain_id: u64,
    nonce: u64,
    max_priority_fee_per_gas: U256,
    max_fee_per_gas: U256,
    gas_limit: u64,
    to: Address,
    value: U256,
    data: &[u8],
) -> Result<Vec<u8>, AdapterError> {
    if chain_id == 0 {
        return Err(AdapterError::ZeroChainId);
    }
    if gas_limit == 0 {
        return Err(AdapterError::ZeroGasLimit);
    }
    if data.is_empty() {
        return Err(AdapterError::EmptyCalldata);
    }

    let unsigned = encode_eip1559_unsigned(
        chain_id,
        nonce,
        max_priority_fee_per_gas,
        max_fee_per_gas,
        gas_limit,
        to,
        value,
        data,
    );

    let digest = keccak256(&unsigned);
    let sig = kms.sign_hash(&digest).await?;

    // AwsKmsSigner returns r(32) || s(32) || v(0|1)
    let y_parity = sig[64];
    let r = &sig[..32];
    let s = &sig[32..64];

    Ok(encode_eip1559_signed(
        chain_id,
        nonce,
        max_priority_fee_per_gas,
        max_fee_per_gas,
        gas_limit,
        to,
        value,
        data,
        y_parity,
        r,
        s,
    ))
}

fn keccak256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(data);
    hasher.finalize().into()
}

fn encode_eip1559_unsigned(
    chain_id: u64,
    nonce: u64,
    max_priority_fee_per_gas: U256,
    max_fee_per_gas: U256,
    gas_limit: u64,
    to: Address,
    value: U256,
    data: &[u8],
) -> Vec<u8> {
    let mut stream = RlpStream::new_list(9);
    stream.append(&chain_id);
    stream.append(&nonce);
    append_u256(&mut stream, max_priority_fee_per_gas);
    append_u256(&mut stream, max_fee_per_gas);
    stream.append(&gas_limit);
    stream.append(&to.as_slice());
    append_u256(&mut stream, value);
    stream.append(&data);
    stream.begin_list(0); // empty access list

    let mut out = Vec::with_capacity(1 + stream.as_raw().len());
    out.push(0x02);
    out.extend_from_slice(stream.as_raw());
    out
}

fn encode_eip1559_signed(
    chain_id: u64,
    nonce: u64,
    max_priority_fee_per_gas: U256,
    max_fee_per_gas: U256,
    gas_limit: u64,
    to: Address,
    value: U256,
    data: &[u8],
    y_parity: u8,
    r: &[u8],
    s: &[u8],
) -> Vec<u8> {
    let mut stream = RlpStream::new_list(12);
    stream.append(&chain_id);
    stream.append(&nonce);
    append_u256(&mut stream, max_priority_fee_per_gas);
    append_u256(&mut stream, max_fee_per_gas);
    stream.append(&gas_limit);
    stream.append(&to.as_slice());
    append_u256(&mut stream, value);
    stream.append(&data);
    stream.begin_list(0); // empty access list
    stream.append(&y_parity);
    stream.append(&trim_leading_zeros(r));
    stream.append(&trim_leading_zeros(s));

    let mut out = Vec::with_capacity(1 + stream.as_raw().len());
    out.push(0x02);
    out.extend_from_slice(stream.as_raw());
    out
}

fn append_u256(stream: &mut RlpStream, value: U256) {
    let be = value.to_be_bytes::<32>();
    stream.append(&trim_leading_zeros(&be));
}

fn trim_leading_zeros(bytes: &[u8]) -> &[u8] {
    let first_nonzero = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    if first_nonzero == bytes.len() {
        &[] // zero value encodes as empty byte string in RLP
    } else {
        &bytes[first_nonzero..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsigned_envelope_starts_with_type_2() {
        let raw = encode_eip1559_unsigned(
            42161,
            0,
            U256::from(1_000_000_000u64),
            U256::from(2_000_000_000u64),
            21000,
            Address::ZERO,
            U256::ZERO,
            &[0x01, 0x02],
        );
        assert_eq!(raw[0], 0x02);
        assert!(raw.len() > 1);
    }
}