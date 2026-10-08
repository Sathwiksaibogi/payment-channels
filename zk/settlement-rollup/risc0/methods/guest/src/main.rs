#![no_main]

use borsh::{BorshDeserialize, BorshSerialize};
use ed25519_dalek::{Signature, VerifyingKey};
use risc0_zkvm::guest::env;

use zkchannel_zk_shared::{
    SettlementBatchInput, SettlementBatchJournal, SettlementInput,
    SettlementTransition, CHANNEL_STATUS_OPEN, V1_MAGIC, V1_PAYLOAD_LEN,settlement_batch_commitment,
};

risc0_zkvm::guest::entry!(main);

fn validate_settlement(
    settlement: &SettlementInput,
    now_unix: i64,
) -> SettlementTransition {
    let channel = &settlement.channel;
    let payload = &settlement.payload;

    // Ordinary `settle` is valid only while the channel is OPEN.
    assert_eq!(
        channel.status,
        CHANNEL_STATUS_OPEN,
        "channel must be OPEN"
    );

    // Preserve the exact existing V1 signed-message contract.
    assert_eq!(
        payload.len(),
        V1_PAYLOAD_LEN,
        "V1 voucher payload must be exactly 50 bytes"
    );

    assert_eq!(
        &payload[0..2],
        &V1_MAGIC,
        "invalid V1 voucher magic"
    );

    // In the existing program the Ed25519 precompile has already verified
    // the signed message before verify_voucher() evaluates channel semantics.
    //
    // Here the zkVM performs that authorization directly using the signer
    // committed in the supplied channel state.
    let verifying_key =
        VerifyingKey::from_bytes(&channel.authorized_signer)
            .expect("invalid channel authorized_signer");

    let signature = Signature::from_bytes(&settlement.signature);

    verifying_key
        .verify_strict(payload, &signature)
        .expect("invalid V1 voucher Ed25519 signature");

    // Parse the already-authenticated V1 payload.
    let mut voucher_channel_id = [0u8; 32];
    voucher_channel_id.copy_from_slice(&payload[2..34]);

    let cumulative_amount = u64::from_le_bytes(
        payload[34..42]
            .try_into()
            .expect("invalid cumulative amount slice"),
    );

    let expires_at = i64::from_le_bytes(
        payload[42..50]
            .try_into()
            .expect("invalid expiry slice"),
    );

    // Match ordinary payment-channels `verify_voucher` semantics.
    assert_eq!(
        voucher_channel_id,
        channel.channel_address,
        "voucher channel_id does not match channel"
    );

    assert!(
        expires_at == 0 || now_unix < expires_at,
        "voucher expired"
    );

    assert!(
        cumulative_amount <= channel.deposit,
        "voucher cumulative_amount exceeds channel deposit"
    );

    assert!(
        channel.settled < cumulative_amount,
        "voucher watermark not strictly monotonic"
    );

    SettlementTransition {
        channel_address: channel.channel_address,
        previous_settled: channel.settled,
        new_settled: cumulative_amount,
    }
}

fn main() {
    let input =
        SettlementBatchInput::deserialize_reader(&mut env::stdin())
            .expect("invalid settlement batch input encoding");

    assert!(
        !input.settlements.is_empty(),
        "settlement batch must not be empty"
    );

    // A logical channel may advance at most once per batch.
    //
    // This intentionally uses an O(n^2) comparison for the prototype:
    // it is deterministic, dependency-free, and easy to audit.
    for i in 0..input.settlements.len() {
        for j in 0..i {
            assert_ne!(
                input.settlements[i].channel.channel_address,
                input.settlements[j].channel.channel_address,
                "duplicate channel in settlement batch"
            );
        }
    }

    let mut transitions =
        Vec::with_capacity(input.settlements.len());

    for settlement in &input.settlements {
        transitions.push(
            validate_settlement(
                settlement,
                input.now_unix,
            ),
        );
    }

    let settlement_count =
        u32::try_from(transitions.len())
            .expect("settlement count exceeds u32");

    let transitions_commitment =
        settlement_batch_commitment(
            input.now_unix,
            &transitions,
        );

    let journal = SettlementBatchJournal {
        now_unix: input.now_unix,
        settlement_count,
        transitions_commitment,
    };

    journal
        .serialize(&mut env::journal())
        .expect("failed to serialize settlement batch journal");
}
