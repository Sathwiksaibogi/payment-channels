use std::{fs, path::PathBuf};

use borsh::{to_vec, BorshDeserialize};
use risc0_zkvm::{default_prover, ExecutorEnv};
use serde::Deserialize;

use zkchannel_zk_methods::{ZKCHANNEL_VOUCHER_GUEST_ELF, ZKCHANNEL_VOUCHER_GUEST_ID};
use zkchannel_zk_shared::{
    settlement_batch_commitment, ChannelStateInput, SettlementBatchInput, SettlementBatchJournal,
    SettlementInput, SettlementTransition, CHANNEL_STATUS_OPEN, V1_PAYLOAD_LEN,
};

const TEST_DEPOSIT: u64 = 1_000_000;
const TEST_SETTLED: u64 = 0;
const TEST_NOW_UNIX: i64 = 1_700_000_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    payload_hex: String,
    authorized_signer_hex: String,
    signature_hex: String,
    channel_id_hex: String,
    cumulative_amount: String,
    expires_at: String,
}

fn load_fixture() -> Fixture {
    let fixture_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/v1-valid.json");

    let fixture_json =
        fs::read_to_string(fixture_path).expect("failed to read canonical V1 fixture");

    serde_json::from_str(&fixture_json).expect("failed to parse canonical V1 fixture")
}

fn decode_channel_id(fixture: &Fixture) -> [u8; 32] {
    hex::decode(&fixture.channel_id_hex)
        .expect("invalid channel ID hex")
        .try_into()
        .expect("channel ID must be 32 bytes")
}

fn decode_authorized_signer(fixture: &Fixture) -> [u8; 32] {
    hex::decode(&fixture.authorized_signer_hex)
        .expect("invalid authorized signer hex")
        .try_into()
        .expect("authorized signer must be 32 bytes")
}

fn decode_signature(fixture: &Fixture) -> [u8; 64] {
    hex::decode(&fixture.signature_hex)
        .expect("invalid signature hex")
        .try_into()
        .expect("signature must be 64 bytes")
}

fn fixture_input(fixture: &Fixture) -> SettlementBatchInput {
    let payload = hex::decode(&fixture.payload_hex).expect("invalid payload hex");

    assert_eq!(
        payload.len(),
        V1_PAYLOAD_LEN,
        "fixture payload must be exactly 50 bytes"
    );

    let cumulative_amount: u64 = fixture
        .cumulative_amount
        .parse()
        .expect("invalid cumulative amount");

    let expires_at: i64 = fixture.expires_at.parse().expect("invalid expiry");

    assert_eq!(
        cumulative_amount,
        u64::from_le_bytes(
            payload[34..42]
                .try_into()
                .expect("invalid cumulative amount slice")
        ),
        "fixture cumulative amount does not match payload"
    );

    assert_eq!(
        expires_at,
        i64::from_le_bytes(payload[42..50].try_into().expect("invalid expiry slice")),
        "fixture expiry does not match payload"
    );

    SettlementBatchInput {
        now_unix: TEST_NOW_UNIX,
        settlements: vec![SettlementInput {
            channel: ChannelStateInput {
                channel_address: decode_channel_id(fixture),
                status: CHANNEL_STATUS_OPEN,
                deposit: TEST_DEPOSIT,
                settled: TEST_SETTLED,
                authorized_signer: decode_authorized_signer(fixture),
            },
            payload,
            signature: decode_signature(fixture),
        }],
    }
}

fn payload_cumulative_amount(payload: &[u8]) -> u64 {
    u64::from_le_bytes(
        payload[34..42]
            .try_into()
            .expect("invalid cumulative amount slice"),
    )
}

fn expected_journal(input: &SettlementBatchInput) -> SettlementBatchJournal {
    let transitions: Vec<SettlementTransition> = input
        .settlements
        .iter()
        .map(|settlement| SettlementTransition {
            channel_address: settlement.channel.channel_address,
            previous_settled: settlement.channel.settled,
            new_settled: payload_cumulative_amount(&settlement.payload),
        })
        .collect();

    let settlement_count = u32::try_from(transitions.len()).expect("settlement count exceeds u32");

    let transitions_commitment = settlement_batch_commitment(input.now_unix, &transitions);

    SettlementBatchJournal {
        now_unix: input.now_unix,
        settlement_count,
        transitions_commitment,
    }
}

fn prove_and_decode(input: &SettlementBatchInput) -> Option<SettlementBatchJournal> {
    let encoded_input = to_vec(input).ok()?;

    let env = ExecutorEnv::builder()
        .write_slice(&encoded_input)
        .build()
        .ok()?;

    let prove_info = default_prover()
        .prove(env, ZKCHANNEL_VOUCHER_GUEST_ELF)
        .ok()?;

    let receipt = prove_info.receipt;

    receipt.verify(ZKCHANNEL_VOUCHER_GUEST_ID).ok()?;

    SettlementBatchJournal::try_from_slice(&receipt.journal.bytes).ok()
}

fn main() {
    let fixture = load_fixture();
    let input = fixture_input(&fixture);

    println!("Proving ZKChannel single-channel settlement semantics...");

    let journal = prove_and_decode(&input).expect("canonical V1 settlement was rejected");

    assert_eq!(journal, expected_journal(&input));

    println!("Receipt verified against ZKChannel guest image ID.");

    println!("Settlement batch verified:");
    println!("  now_unix               = {}", journal.now_unix);
    println!("  settlement_count       = {}", journal.settlement_count);
    println!(
        "  transitions_commitment = {}",
        hex::encode(journal.transitions_commitment)
    );

    println!("ZKChannel settlement batch validation succeeded.");
}

#[cfg(test)]
mod tests {
    use super::*;

    use ed25519_dalek::{Signer, SigningKey};

    // The deterministic seed used to create the canonical V1 fixture.
    const FIXTURE_SIGNING_SEED: [u8; 32] = [1u8; 32];

    fn settlement_mut(input: &mut SettlementBatchInput) -> &mut SettlementInput {
        &mut input.settlements[0]
    }

    fn signed_settlement(
        channel_address: [u8; 32],
        signing_seed: [u8; 32],
        cumulative_amount: u64,
        expires_at: i64,
        deposit: u64,
        settled: u64,
    ) -> SettlementInput {
        let signing_key = SigningKey::from_bytes(&signing_seed);

        let mut payload = Vec::with_capacity(V1_PAYLOAD_LEN);

        payload.extend_from_slice(&zkchannel_zk_shared::V1_MAGIC);
        payload.extend_from_slice(&channel_address);
        payload.extend_from_slice(&cumulative_amount.to_le_bytes());
        payload.extend_from_slice(&expires_at.to_le_bytes());

        assert_eq!(payload.len(), V1_PAYLOAD_LEN);

        let signature = signing_key.sign(&payload).to_bytes();

        SettlementInput {
            channel: ChannelStateInput {
                channel_address,
                status: CHANNEL_STATUS_OPEN,
                deposit,
                settled,
                authorized_signer: signing_key.verifying_key().to_bytes(),
            },
            payload,
            signature,
        }
    }

    fn resign_payload(input: &mut SettlementBatchInput) {
        let signing_key = SigningKey::from_bytes(&FIXTURE_SIGNING_SEED);

        let expected_signer = signing_key.verifying_key().to_bytes();

        assert_eq!(
            expected_signer, input.settlements[0].channel.authorized_signer,
            "fixture signing seed does not match fixture authorized signer"
        );

        let signature = signing_key.sign(&input.settlements[0].payload);

        input.settlements[0].signature = signature.to_bytes();
    }

    fn input_with_expiry(
        fixture: &Fixture,
        expires_at: i64,
        now_unix: i64,
    ) -> SettlementBatchInput {
        let mut input = fixture_input(fixture);

        input.now_unix = now_unix;

        settlement_mut(&mut input).payload[42..50].copy_from_slice(&expires_at.to_le_bytes());

        resign_payload(&mut input);

        input
    }

    #[test]
    fn accepts_canonical_v1_settlement() {
        let fixture = load_fixture();
        let input = fixture_input(&fixture);

        let journal = prove_and_decode(&input).expect("canonical V1 settlement was rejected");

        assert_eq!(journal, expected_journal(&input));
    }

    #[test]
    fn rejects_non_open_channel() {
        let fixture = load_fixture();
        let mut input = fixture_input(&fixture);

        settlement_mut(&mut input).channel.status = 1;

        assert!(
            prove_and_decode(&input).is_none(),
            "non-OPEN channel was accepted"
        );
    }

    #[test]
    fn rejects_wrong_channel_binding() {
        let fixture = load_fixture();
        let mut input = fixture_input(&fixture);

        settlement_mut(&mut input).channel.channel_address = [8u8; 32];

        assert!(
            prove_and_decode(&input).is_none(),
            "voucher for a different channel was accepted"
        );
    }

    #[test]
    fn accepts_zero_expiry() {
        let fixture = load_fixture();
        let input = fixture_input(&fixture);

        assert!(
            prove_and_decode(&input).is_some(),
            "expires_at = 0 should mean no expiry"
        );
    }

    #[test]
    fn accepts_before_expiry() {
        let fixture = load_fixture();

        let input = input_with_expiry(&fixture, 2_000, 1_999);

        assert!(
            prove_and_decode(&input).is_some(),
            "voucher should be valid before expiry"
        );
    }

    #[test]
    fn rejects_at_expiry() {
        let fixture = load_fixture();

        let input = input_with_expiry(&fixture, 2_000, 2_000);

        assert!(
            prove_and_decode(&input).is_none(),
            "voucher should expire when now == expires_at"
        );
    }

    #[test]
    fn rejects_after_expiry() {
        let fixture = load_fixture();

        let input = input_with_expiry(&fixture, 2_000, 2_001);

        assert!(
            prove_and_decode(&input).is_none(),
            "voucher should expire when now > expires_at"
        );
    }

    #[test]
    fn accepts_cumulative_equal_to_deposit() {
        let fixture = load_fixture();
        let mut input = fixture_input(&fixture);

        let cumulative = payload_cumulative_amount(&input.settlements[0].payload);

        settlement_mut(&mut input).channel.deposit = cumulative;

        assert!(
            prove_and_decode(&input).is_some(),
            "cumulative == deposit should be valid"
        );
    }

    #[test]
    fn rejects_cumulative_over_deposit() {
        let fixture = load_fixture();
        let mut input = fixture_input(&fixture);

        let cumulative = payload_cumulative_amount(&input.settlements[0].payload);

        settlement_mut(&mut input).channel.deposit = cumulative - 1;

        assert!(
            prove_and_decode(&input).is_none(),
            "cumulative > deposit was accepted"
        );
    }

    #[test]
    fn accepts_strictly_higher_watermark() {
        let fixture = load_fixture();
        let mut input = fixture_input(&fixture);

        let cumulative = payload_cumulative_amount(&input.settlements[0].payload);

        settlement_mut(&mut input).channel.settled = cumulative - 1;

        assert!(
            prove_and_decode(&input).is_some(),
            "settled < cumulative should be valid"
        );
    }

    #[test]
    fn rejects_equal_watermark() {
        let fixture = load_fixture();
        let mut input = fixture_input(&fixture);

        let cumulative = payload_cumulative_amount(&input.settlements[0].payload);

        settlement_mut(&mut input).channel.settled = cumulative;

        assert!(
            prove_and_decode(&input).is_none(),
            "settled == cumulative was accepted"
        );
    }

    #[test]
    fn rejects_watermark_below_existing_settled() {
        let fixture = load_fixture();
        let mut input = fixture_input(&fixture);

        let cumulative = payload_cumulative_amount(&input.settlements[0].payload);

        settlement_mut(&mut input).channel.settled = cumulative + 1;

        assert!(
            prove_and_decode(&input).is_none(),
            "cumulative < settled was accepted"
        );
    }

    #[test]
    fn rejects_wrong_authorized_signer() {
        let fixture = load_fixture();
        let mut input = fixture_input(&fixture);

        let wrong_signing_key = SigningKey::from_bytes(&[2u8; 32]);

        settlement_mut(&mut input).channel.authorized_signer =
            wrong_signing_key.verifying_key().to_bytes();

        assert!(
            prove_and_decode(&input).is_none(),
            "voucher signed by a different authorized signer was accepted"
        );
    }

    #[test]
    fn rejects_mutated_signature() {
        let fixture = load_fixture();
        let mut input = fixture_input(&fixture);

        settlement_mut(&mut input).signature[0] ^= 0x01;

        assert!(
            prove_and_decode(&input).is_none(),
            "mutated signature was accepted"
        );
    }

    #[test]
    fn rejects_mutated_signed_payload() {
        let fixture = load_fixture();
        let mut input = fixture_input(&fixture);

        settlement_mut(&mut input).payload[34] ^= 0x01;

        assert!(
            prove_and_decode(&input).is_none(),
            "mutated signed payload was accepted"
        );
    }

    #[test]
    fn rejects_duplicate_channel_in_batch() {
        let channel_address = [21u8; 32];
        let signing_seed = [21u8; 32];

        let input = SettlementBatchInput {
            now_unix: TEST_NOW_UNIX,
            settlements: vec![
                signed_settlement(channel_address, signing_seed, 100, 0, 1_000, 20),
                signed_settlement(channel_address, signing_seed, 200, 0, 1_000, 20),
            ],
        };

        assert!(
            prove_and_decode(&input).is_none(),
            "duplicate channel was accepted"
        );
    }

    #[test]
    fn rejects_batch_if_any_settlement_is_invalid() {
        let valid = signed_settlement([30u8; 32], [30u8; 32], 100, 0, 1_000, 10);

        let mut invalid = signed_settlement([31u8; 32], [31u8; 32], 200, 0, 1_000, 20);

        invalid.channel.deposit = 199;

        let input = SettlementBatchInput {
            now_unix: TEST_NOW_UNIX,
            settlements: vec![valid, invalid],
        };

        assert!(
            prove_and_decode(&input).is_none(),
            "batch with invalid settlement was accepted"
        );
    }

    #[test]
    fn rejects_empty_batch() {
        let input = SettlementBatchInput {
            now_unix: TEST_NOW_UNIX,
            settlements: vec![],
        };

        assert!(
            prove_and_decode(&input).is_none(),
            "empty settlement batch was accepted"
        );
    }

    #[test]
    fn accepts_multiple_independent_signers() {
        let input = SettlementBatchInput {
            now_unix: TEST_NOW_UNIX,
            settlements: vec![
                signed_settlement([10u8; 32], [10u8; 32], 100, 0, 1_000, 40),
                signed_settlement([11u8; 32], [11u8; 32], 200, 0, 1_000, 75),
                signed_settlement([12u8; 32], [12u8; 32], 300, 0, 1_000, 125),
            ],
        };

        assert_ne!(
            input.settlements[0].channel.authorized_signer,
            input.settlements[1].channel.authorized_signer,
        );

        assert_ne!(
            input.settlements[1].channel.authorized_signer,
            input.settlements[2].channel.authorized_signer,
        );

        let journal = prove_and_decode(&input).expect("independent-signer batch was rejected");

        assert_eq!(journal, expected_journal(&input));

        assert_eq!(journal.settlement_count, 3);

        assert_eq!(
            hex::encode(journal.transitions_commitment),
            "d485997ce91ea64cebe80942afe56f854adc054f9edf20e660faf03bc3b02424"
        );
    }

    #[test]
    fn settlement_batch_journal_is_fixed_size() {
        let fixture = load_fixture();

        let one = expected_journal(&fixture_input(&fixture));

        let three_input = SettlementBatchInput {
            now_unix: TEST_NOW_UNIX,
            settlements: vec![
                signed_settlement([10u8; 32], [10u8; 32], 100, 0, 1_000, 40),
                signed_settlement([11u8; 32], [11u8; 32], 200, 0, 1_000, 75),
                signed_settlement([12u8; 32], [12u8; 32], 300, 0, 1_000, 125),
            ],
        };

        let three = expected_journal(&three_input);

        let one_bytes = to_vec(&one).expect("failed to encode one-item journal");

        let three_bytes = to_vec(&three).expect("failed to encode three-item journal");

        assert_eq!(one_bytes.len(), 44);
        assert_eq!(three_bytes.len(), 44);
        assert_eq!(one_bytes.len(), three_bytes.len());
    }
}
