# ADR-006 RISC Zero Independent V1 Settlement Batching Prototype

This directory contains an experimental RISC Zero zkVM prototype for the
settlement-rollup direction described in ADR-006.

The prototype demonstrates that multiple existing payment-channel V1 vouchers,
including vouchers authorized by different Ed25519 signers, can be validated
inside one zkVM execution without changing the existing V1 signed-message
format.

The guest validates ordinary `settle` semantics for every included channel,
rejects duplicate channels, and commits the resulting logical settlement
transitions into a fixed-size public journal.

This is not yet a complete settlement rollup.

The current channel-state snapshots and evaluation timestamp are supplied to
the guest as inputs and are not yet authenticated against canonical Solana
state.

---

## Motivation

The existing payment-channel protocol authorizes settlement using signed,
cumulative V1 vouchers.

Each voucher is signed by the channel's `authorized_signer`.

For channels with different authorized signers, settlement normally requires
repeated signature verification and repeated channel-state processing.

ADR-006 explores whether many independently signed vouchers can eventually be
validated inside one succinct proof while preserving the existing V1 voucher
contract.

The current prototype demonstrates:

```text
V1 voucher A + signer A + channel state A
V1 voucher B + signer B + channel state B
V1 voucher C + signer C + channel state C
                    |
                    v
              RISC Zero zkVM
                    |
                    +-- verify exact V1 framing
                    +-- verify each Ed25519 signature
                    +-- verify channel binding
                    +-- verify OPEN status
                    +-- verify expiry
                    +-- verify deposit ceiling
                    +-- verify strict watermark advancement
                    +-- reject duplicate channels
                    |
                    v
          logical settlement transitions
                    |
                    v
        domain-separated SHA-256 commitment
                    |
                    v
          fixed-size public journal
```

No new voucher format is introduced.

---

## Current Scope

The guest receives a `SettlementBatchInput` containing an explicit
`now_unix` value and one or more settlement candidates.

Each settlement contains:

- the existing 50-byte V1 voucher payload;
- the 64-byte Ed25519 signature;
- a minimal channel-state snapshot containing:
  - channel address;
  - channel status;
  - deposit;
  - settled watermark;
  - authorized signer.

For every settlement, the guest enforces the ordinary payment-channel
`settle` rules:

1. the channel is `OPEN`;
2. the payload is exactly 50 bytes;
3. the V1 magic bytes are `[0x56, 0x01]`;
4. the Ed25519 signature verifies over the exact V1 payload using the
   channel state's `authorized_signer`;
5. the voucher `channel_id` equals the supplied channel address;
6. `expires_at == 0 || now_unix < expires_at`;
7. `cumulative_amount <= deposit`;
8. `settled < cumulative_amount`.

The batch must be non-empty.

The same channel address may appear at most once in a batch.

Different settlements may use completely different `authorized_signer`
public keys. There is no shared-signer requirement.

After validation, the guest derives one logical transition per channel:

```text
(channel_address, previous_settled, new_settled)
```

The transition list remains internal to the guest computation.

The public journal contains only a fixed-size commitment to those validated
transitions.

---

## Independent Signers and ADR-004

The batching model here does not require all channels to share one signer.

```text
channel A -> authorized signer A -> V1 voucher A
channel B -> authorized signer B -> V1 voucher B
channel C -> authorized signer C -> V1 voucher C
                                  |
                                  v
                         one zkVM execution
```

This differs from the shared-signer batching direction discussed in ADR-004.

Conceptually, ADR-004 uses:

```text
channel A --\
channel B ----> one shared authorized signer
channel C --/              |
                           v
                  one signed batch message
```

The ADR-006 prototype instead preserves independent V1 authorization:

```text
channel A -> signer A -> V1 voucher A
channel B -> signer B -> V1 voucher B
channel C -> signer C -> V1 voucher C
```

Each signature is verified independently inside the zkVM.

This removes the shared-signer assumption from proof-side batching.

It does **not** yet remove the need for per-channel on-chain state writes.
That requires the settlement accumulator introduced in the next prototype
stage.

---

## Existing V1 Signed Message

The prototype preserves the existing V1 payload exactly:

```text
offset  size  field
------  ----  ----------------------------------------
0       2     magic = [0x56, 0x01]
2       32    channel_id
34      8     cumulative_amount (u64 little-endian)
42      8     expires_at (i64 little-endian)

total: 50 bytes
```

The Ed25519 signature is verified over these exact 50 bytes.

No additional epoch or nonce field is added.

The existing `channel_id` remains the signed replay scope.

---

## Settlement Semantics

For one channel, the logical state transition is:

```text
previous settled watermark
          |
          | require voucher cumulative_amount > settled
          |
          v
voucher cumulative_amount
          |
          v
new settled watermark
```

For example:

```text
deposit            = 100
previous settled   = 40
voucher cumulative = 75

result:

previous_settled = 40
new_settled      = 75
delta            = 35
```

A later voucher with:

```text
cumulative_amount = 60
```

would be rejected because:

```text
60 <= 75
```

and the settlement watermark must advance strictly.

The cumulative amount is also bounded by the channel deposit:

```text
cumulative_amount <= deposit
```

Equality with the deposit is valid.

---

## Expiry Semantics

The existing V1 rule is preserved exactly:

```text
expires_at == 0 || now_unix < expires_at
```

Therefore:

```text
expires_at = 0
-> no expiry

now_unix < expires_at
-> valid

now_unix == expires_at
-> expired

now_unix > expires_at
-> expired
```

The current prototype validates this rule against the supplied `now_unix`.

However, `now_unix` is not yet proven to originate from Solana's `Clock`.
That binding belongs to a later on-chain verification stage.

---

## Batch Atomicity

Every settlement in the batch must be valid.

Conceptually:

```text
settlement A -> valid
settlement B -> valid
settlement C -> invalid
                    |
                    v
             whole batch rejected
```

The guest does not emit a successful journal for a partially valid batch.

---

## Duplicate-Channel Protection

A logical channel may advance at most once per batch.

Valid:

```text
A
B
C
```

Rejected:

```text
A
B
A
```

The prototype currently uses a simple deterministic `O(n^2)` duplicate scan.

This is intentional for the prototype because it is:

- dependency-free;
- deterministic;
- easy to review;
- easy to audit.

A different data structure can be considered later if benchmarks show that
duplicate detection becomes a meaningful proving bottleneck.

---

## Fixed-Size Settlement Journal

Validated transitions are committed using a canonical, order-sensitive,
domain-separated SHA-256 encoding.

The commitment is:

```text
SHA256(
    "zkchannel:settlement-batch:v1"
    ||
    settlement_count:u32 little-endian
    ||
    now_unix:i64 little-endian
    ||
    for each transition, in batch order:
        channel_address:[u8;32]
        ||
        previous_settled:u64 little-endian
        ||
        new_settled:u64 little-endian
)
```

Each transition contributes exactly:

```text
channel_address       32 bytes
previous_settled       8 bytes
new_settled            8 bytes
                      --------
                      48 bytes
```

The full transition list is hashed inside the guest.

The public `SettlementBatchJournal` contains:

```text
field                    size
-----------------------  --------
now_unix                 8 bytes
settlement_count         4 bytes
transitions_commitment   32 bytes
                         --------
total                    44 bytes
```

The Borsh-encoded journal therefore remains exactly **44 bytes** whether the
guest validates:

```text
1 settlement
3 settlements
10 settlements
N settlements
```

The transition commitment is order-sensitive.

Therefore:

```text
[A, B, C]
```

and:

```text
[C, B, A]
```

produce different commitments.

The fixed-size journal compresses the public representation of the validated
logical transitions.

It does **not** by itself authenticate the supplied channel states or update
canonical Solana state.

---

## What the Proof Statement Currently Means

The current prototype establishes a statement approximately equivalent to:

> Given these supplied channel-state snapshots, this supplied evaluation
> timestamp, and these independently signed V1 vouchers, every settlement
> satisfies the ordinary V1 `settle` authorization and semantic rules, no
> channel appears twice, and the resulting logical transitions commit to this
> public transition digest.

It does **not** yet establish:

> These supplied channel-state snapshots are the canonical current settlement
> state of the Solana payment-channel program.

That distinction is fundamental.

At the current stage:

```text
signature verification             implemented
V1 framing                          implemented
channel binding                     implemented
OPEN status                         implemented
expiry semantics                    implemented
deposit ceiling                     implemented
strict settlement advancement       implemented
independent-signer batching         implemented
duplicate rejection                 implemented
batch atomicity                     implemented
fixed-size transition commitment    implemented

canonical state authentication      not yet implemented
accumulator membership              not yet implemented
old_root -> new_root transition     not yet implemented
trusted Solana Clock binding        not yet implemented
Solana-side proof verification      not yet implemented
```

---

## Workspace Layout

```text
risc0/
├── host/
│   ├── Cargo.toml
│   └── src/
│       └── main.rs
├── methods/
│   ├── Cargo.toml
│   ├── build.rs
│   ├── src/
│   │   └── lib.rs
│   └── guest/
│       ├── Cargo.toml
│       ├── Cargo.lock
│       └── src/
│           └── main.rs
├── shared/
│   ├── Cargo.toml
│   └── src/
│       └── lib.rs
├── .gitignore
├── Cargo.toml
└── Cargo.lock
```

### `shared`

Contains the Borsh types and protocol helpers shared between the host and
guest.

The original V1 authorization prototype types remain available:

- `V1VoucherInput`
- `V1VoucherJournal`

The settlement-batching prototype additionally defines:

- `ChannelStateInput`
- `SettlementInput`
- `SettlementBatchInput`
- `SettlementTransition`
- `SettlementBatchJournal`

The shared crate also defines:

- `V1_PAYLOAD_LEN`;
- `V1_MAGIC`;
- the `OPEN` channel-status value used by the prototype;
- the transition-commitment domain separator;
- the canonical SHA-256 settlement-batch commitment function.

Keeping the commitment function in the shared crate ensures that the host and
guest use the same explicit protocol encoding.

### `methods/guest`

Contains the RISC Zero guest program.

The guest performs:

```text
deserialize SettlementBatchInput
        |
        v
require non-empty batch
        |
        v
reject duplicate channel addresses
        |
        v
for each settlement
        |
        +-- require channel OPEN
        +-- require exact 50-byte V1 payload
        +-- require V1 magic
        +-- verify independent Ed25519 signature
        +-- parse authenticated V1 fields
        +-- verify voucher/channel binding
        +-- verify expiry
        +-- verify deposit ceiling
        +-- verify strict watermark advancement
        |
        v
derive logical settlement transitions
        |
        v
compute domain-separated SHA-256 commitment
        |
        v
commit fixed-size SettlementBatchJournal
```

### `methods`

Uses `risc0-build` to compile and embed the guest.

The generated output exposes:

- the guest binary;
- the guest path;
- the guest image ID.

The image ID cryptographically identifies the exact guest computation being
verified.

Any change to guest logic changes the image ID.

### `host`

The host:

1. loads the canonical V1 fixture;
2. constructs settlement-channel state around the fixture;
3. serializes `SettlementBatchInput` using Borsh;
4. constructs a RISC Zero `ExecutorEnv`;
5. executes or proves the guest;
6. verifies the receipt against the generated guest image ID;
7. decodes the fixed-size `SettlementBatchJournal`;
8. independently derives the expected transition commitment;
9. checks the guest journal against the expected result.

The host also generates deterministic independently signed settlement inputs
for batch tests and exercises semantic boundaries and adversarial cases.

---

## Canonical V1 Fixture

The prototype uses the deterministic fixture at:

```text
../fixtures/v1-valid.json
```

The fixture represents a valid V1 voucher with:

```text
channel_id        = 0707070707070707070707070707070707070707070707070707070707070707
cumulative_amount = 500000
expires_at        = 0
```

The fixture is generated independently from the payment-channel V1 wire format
and is used as a stable compatibility test vector.

The canonical fixture is intentionally preserved so the settlement-batching
prototype remains grounded in the same existing V1 voucher bytes used by the
earlier authorization prototype.

---

## Settlement and Batch Tests

The host currently exercises **20 development-mode tests** covering:

- Ed25519 authorization;
- V1 message integrity;
- ordinary `settle` semantics;
- expiry boundaries;
- deposit boundaries;
- strict settlement-watermark advancement;
- independent-signer batching;
- duplicate-channel protection;
- atomic batch validation;
- fixed-size journal encoding.

Important acceptance cases include:

```text
canonical V1 settlement                  -> accepted
expires_at = 0                           -> accepted
now_unix < expires_at                    -> accepted
cumulative_amount == deposit             -> accepted
settled < cumulative_amount              -> accepted
multiple independent authorized signers  -> accepted
```

Important rejection cases include:

```text
now_unix == expires_at                    -> rejected
now_unix > expires_at                     -> rejected
cumulative_amount > deposit               -> rejected
settled == cumulative_amount              -> rejected
settled > cumulative_amount               -> rejected
non-OPEN channel                          -> rejected
wrong channel binding                     -> rejected
wrong authorized signer                   -> rejected
mutated Ed25519 signature                 -> rejected
mutated signed payload                    -> rejected
duplicate channel in one batch            -> rejected
empty batch                               -> rejected
one invalid member in a batch             -> whole batch rejected
```

The suite also checks that the Borsh-encoded public journal remains exactly
44 bytes for both single-settlement and multi-settlement batches.

The rejection tests intentionally cause the guest to abort.

In RISC Zero development mode, the host observes the failed guest execution
and treats that failure as the expected result for negative test cases.

---

## Independent-Signer Batch Test

The batch suite includes a positive test containing three channels with three
different Ed25519 signing keys.

Conceptually:

```text
channel [10; 32]
signer  [10; 32] seed
40 -> 100

channel [11; 32]
signer  [11; 32] seed
75 -> 200

channel [12; 32]
signer  [12; 32] seed
125 -> 300
```

The three resulting verifying keys are different.

All three signatures are independently validated inside the same guest
execution.

This test is intended to demonstrate the central independent-signer property
of the ADR-006 prototype rather than merely testing repeated vouchers from one
shared authority.

---

## Building the Guest

From this directory:

```bash
RISC0_BUILD_LOCKED=1 \
cargo build --locked -p zkchannel-zk-methods
```

The guest is compiled for the RISC Zero zkVM RISC-V target.

The resulting image ID is generated by `risc0-build` and embedded into the
`zkchannel-zk-methods` crate.

The guest image ID should be treated as part of the verifier configuration
because it identifies the exact computation whose receipts may be accepted.

---

## Running the Demo

For fast local development:

```bash
RISC0_DEV_MODE=1 \
RISC0_BUILD_LOCKED=1 \
cargo run --locked -p zkchannel-zk-host
```

Expected output reports successful receipt verification and the settlement
batch journal, including:

```text
now_unix
settlement_count
transitions_commitment
```

The exact guest image ID depends on the compiled guest program and locked
dependency graph.

---

## Running the Tests

Run the test suite in RISC Zero development mode:

```bash
RISC0_DEV_MODE=1 \
RISC0_BUILD_LOCKED=1 \
cargo test --locked \
  -p zkchannel-zk-host \
  -- --test-threads=1
```

The expected result is:

```text
20 passed
0 failed
```

The tests are executed sequentially because zkVM execution can be relatively
resource-intensive.

Guest panic messages are expected during negative tests.

For example, a test may intentionally trigger:

```text
voucher expired
```

or:

```text
duplicate channel in settlement batch
```

while the Rust test itself still passes because rejection is the expected
behavior.

---

## Development Mode

`RISC0_DEV_MODE=1` is intended only for fast development and testing.

In development mode, RISC Zero executes the guest logic and produces the
expected journal behavior, but it does not generate a secure cryptographic
proof.

Therefore:

```text
development mode
    |
    +-- useful for guest logic testing
    +-- useful for adversarial tests
    +-- useful for protocol iteration
    |
    `-- NOT a production proof
```

Real proving must be performed without:

```text
RISC0_DEV_MODE=1
```

Development-mode test success must not be presented as evidence that a secure
receipt was generated.

---

## Reproducibility

The prototype pins:

```text
risc0-zkvm  = 3.0.3
risc0-build = 3.0.3
```

The workspace and guest use separate lockfiles.

This is intentional because RISC Zero builds the guest as a separate RISC-V
workspace.

Both lockfiles are committed so that:

- the host dependency graph is reproducible;
- the guest dependency graph is reproducible;
- guest compilation does not silently drift to incompatible transitive
  versions;
- the resulting guest image can be reproduced from the pinned source and
  dependencies.

The RISC Zero dependency graph used here is aligned with the dependency graph
used by the RISC Zero Solana `v3.0.0` release examples.

The settlement commitment uses:

```text
sha2 = 0.10.9
```

with the canonical encoding documented in this README.

---

## Ed25519 Verification

The guest currently uses:

```text
ed25519-dalek = 2.2.0
```

with strict Ed25519 verification.

The prototype demonstrates that:

- the canonical payment-channel V1 signature is accepted;
- multiple independently generated V1 signatures are accepted;
- a mutated signature is rejected;
- a mutated signed payload is rejected;
- using a different authorized signer is rejected.

However, this does **not** yet establish full behavioral equivalence with
Solana's Ed25519 precompile.

Before treating the zkVM verifier as a production replacement for runtime
precompile semantics, differential compatibility testing is required.

That testing should compare acceptance and rejection behavior for cases such
as:

- malformed public keys;
- malformed signatures;
- non-canonical encodings;
- signature malleability edge cases;
- mutated messages;
- boundary conditions accepted or rejected by the Solana runtime.

The goal is protocol-equivalence testing, not merely demonstrating that normal
Ed25519 signatures verify in both environments.

---

## Current Limitations

The prototype now validates independently signed V1 settlement batches and
commits their logical settlement transitions, but several trust and integration
layers remain outside the proof statement.

### Channel State Is Not Yet Authenticated

`ChannelStateInput` is currently supplied by the prover.

The guest verifies that a voucher is valid relative to the supplied:

```text
channel_address
status
deposit
settled
authorized_signer
```

but does not yet prove that those values are the canonical current Solana
state.

For example, if canonical state says:

```text
settled = 70
```

but a prover supplies:

```text
settled = 40
```

the current PR #2 proof statement alone does not establish which value is
canonical.

Authenticated settlement state is the responsibility of the accumulator stage.

### Time Is Not Yet Authenticated

`now_unix` is also supplied as an input.

The guest correctly enforces:

```text
expires_at == 0 || now_unix < expires_at
```

relative to that value.

However, the proof does not yet establish that `now_unix` came from Solana's
`Clock`.

Future Solana integration must bind the public proof statement to an acceptable
runtime time value.

### Remaining Work

The remaining work includes:

- authenticated channel-state membership;
- canonical accumulator leaf encoding;
- settlement accumulator membership proofs;
- `old_root -> new_root` transitions;
- stale-root protection;
- replay protection;
- authoritative Solana Clock binding;
- coexistence rules with ordinary settlement and lifecycle instructions;
- Solana-side receipt verification;
- Groth16 seal generation;
- RISC Zero Router integration;
- exact image-ID and journal-digest binding;
- verifier deployment and upgrade governance;
- full differential Ed25519 compatibility testing;
- real-proof performance benchmarks;
- Solana compute-unit benchmarks;
- independent cryptographic/security review.

### Payout Watermark

`payout_watermark` is intentionally not part of ordinary V1 `settle`
validation.

The existing voucher-validity path uses the payer-authorized `settled`
watermark.

Later accumulator and lifecycle integration must still preserve the protocol
invariant:

```text
payout_watermark <= settled <= deposit
```

---

## Why a Succinct Proof Alone Is Not Enough

Verifying many vouchers inside one proof can compress computation.

However, if settlement still writes every individual Channel PDA, the protocol
still pays for:

```text
N writable channel accounts
+
N state updates
+
N write locks
```

Therefore ADR-006 requires more than proof aggregation.

The current PR #2 prototype addresses:

```text
authorization compression
+
semantic-validation batching
+
fixed-size public transition commitment
```

but not yet:

```text
canonical state-update compression
```

The intended direction is to combine succinct verification with a settlement
state accumulator.

---

## Computation Compression vs State Compression

The distinction is:

```text
PR #2

N vouchers
N signatures
N semantic checks
       |
       v
one zkVM execution
       |
       v
one fixed-size transition commitment
```

This compresses proof-side computation and public transition representation.

But if those transitions still require:

```text
N Channel PDA writes
```

then on-chain state scaling remains linear.

PR #3 therefore introduces:

```text
old accumulator root
        |
        v
authenticated channel leaves
        |
        v
validated settlement transitions
        |
        v
new accumulator root
```

so that many logical state changes can eventually be represented through one
canonical root transition.

---

## Intended ADR-006 Direction

The target architecture is:

```text
N independent payment channels
N independent authorized signers
N existing V1 vouchers
              |
              v
       RISC Zero zkVM
              |
              +-- verify each V1 Ed25519 signature
              +-- verify channel binding
              +-- verify OPEN status
              +-- verify expiry
              +-- verify deposit bounds
              +-- verify watermark advancement
              +-- reject duplicate channels
              +-- authenticate old logical state
              |
              v
       prove state transition
              |
              v
       old accumulator root
              |
              v
       new accumulator root
              |
              v
        succinct receipt
              |
              v
         Solana verifier
              |
              v
    one canonical root update
```

The overall target is:

```text
proof / computation compression
              +
state-update compression
```

rather than proof compression alone.

---

## Planned Follow-Up Work

The next prototype stage focuses on authenticated settlement state.

### Settlement Accumulator

The next stage is expected to:

1. define a canonical accumulator leaf encoding;
2. authenticate logical channel settlement state;
3. prove membership against an `old_root`;
4. validate the existing independently signed V1 vouchers;
5. apply logical settlement transitions;
6. derive a `new_root`;
7. reject stale roots;
8. reject duplicate channels;
9. prevent replayed state transitions;
10. define coexistence with existing channel lifecycle instructions.

### Solana Verification

Later integration stages are expected to add:

- real cryptographic proof generation;
- Groth16 receipt/seal generation;
- RISC Zero Solana Router verification;
- exact guest image-ID binding;
- journal-digest binding;
- Solana Clock binding;
- one canonical accumulator-root update.

### Benchmarking

The project should eventually compare:

```text
individual settlement
vs
conventional batching
vs
zkVM batch verification
vs
zkVM + accumulator root transition
```

Relevant measurements include:

- transaction size;
- writable-account count;
- Solana compute units;
- proof generation time;
- guest execution cycles;
- proof size;
- journal size;
- batch-size scaling;
- state-write scaling.

---

## Proof and State Trust Boundaries

The intended trust model is:

```text
payer / authorized signer
        |
        | signs existing V1 voucher
        v
untrusted prover
        |
        | constructs zkVM witness
        v
RISC Zero guest
        |
        | checks authorization + semantics
        v
public proof / receipt
        |
        | verified against exact image ID
        v
Solana verifier
        |
        | validates journal / state-root transition
        v
canonical settlement accumulator
```

The prover is not intended to be trusted.

Therefore every value that affects canonical settlement correctness must
eventually be either:

- authenticated inside the proof;
- committed in the public journal;
- or independently checked by the Solana program.

The current prototype has not yet completed that trust boundary because
channel state and time are still supplied inputs.

---

## Security Status

This code is experimental.

It has not undergone an independent security audit.

It should not be used to authorize production funds.

The current prototype is intended to validate:

- compatibility with the existing V1 voucher contract;
- independent-signer zkVM batching;
- exact ordinary-settle semantics;
- deterministic batch commitments;
- fixed-size public output;
- the architecture needed before accumulator integration.

Production use would require substantially more work, including:

- authenticated canonical state;
- Solana-side verifier integration;
- Ed25519 differential compatibility testing;
- verifier/image governance;
- real-proof benchmarking;
- protocol review;
- cryptographic review;
- security audit.

The purpose of this prototype is to validate the ADR-006 architecture,
compatibility assumptions, and performance characteristics before any
production integration is considered.
