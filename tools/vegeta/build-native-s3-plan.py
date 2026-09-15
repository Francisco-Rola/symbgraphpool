#!/usr/bin/env python3
"""Build a pre-execution native CosmWasm translation plan for Vegeta S3.

This tool is deliberately a *planner*, not an executor.  It freezes the reviewed mapping from a reviewed set of
high-impact Ethereum profile families to native CosmWasm code-family slots, preserves all S3
transactions in their original block/order, translates recognized callTracer frames into semantic
native actions, and keeps every unsupported frame as an explicit background fallback.

Historical concrete read/write sets are used only for offline translation-coverage accounting.  They
are never copied into ``native-plan.jsonl`` so the later SymbGraph predictor cannot see oracle access
information through this artifact.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

from vegeta_corpus import load_blocks, storage_contract, write_jsonl

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_CORPUS = ROOT / "benchmarks/corpora/vegeta-ethereum/s3/corpus.jsonl"
DEFAULT_CHARACTERIZATION = ROOT / "benchmarks/corpora/vegeta-ethereum/s3/characterization"
DEFAULT_MAP = ROOT / "evaluation/vegeta/s3-native-family-map.v2.json"
DEFAULT_OUTPUT = ROOT / "benchmarks/corpora/vegeta-ethereum/s3/native-plan"

DELEGATE_TYPES = {"DELEGATECALL", "CALLCODE"}
CALL_TYPES = {"CALL", "STATICCALL", "DELEGATECALL", "CALLCODE"}
SYSTEM_TRANSLATION_STATUS = "mapped-system-action"
SEMANTIC_TRANSLATION_STATUSES = {"mapped-native-call", "inlined-delegatecall", SYSTEM_TRANSLATION_STATUS}

# S3 predates the Cancun point-evaluation precompile.  Addresses 0x01..0x09 are the
# state-free Ethereum precompiles available in this historical range.  They are mapped to
# deterministic system helper actions rather than pretending that they are CosmWasm contracts.
PRECOMPILE_NAMES = {
    1: "ecrecover",
    2: "sha256",
    3: "ripemd160",
    4: "identity",
    5: "modexp",
    6: "bn256_add",
    7: "bn256_mul",
    8: "bn256_pairing",
    9: "blake2f",
}

# Semantic entrypoint translations are intentionally small and source-reviewable.  Unknown selectors
# on a mapped family remain mapped-opaque rather than being guessed.
ENTRYPOINTS: dict[str, dict[str, tuple[str, list[tuple[str, str]]]]] = {
    "cw20-base": {
        "0xa9059cbb": ("execute::transfer", [("recipient", "address"), ("amount", "uint256")]),
        "0x23b872dd": ("execute::transfer_from", [("owner", "address"), ("recipient", "address"), ("amount", "uint256")]),
        "0x70a08231": ("query::balance", [("address", "address")]),
        "0x095ea7b3": ("execute::increase_allowance_or_approve", [("spender", "address"), ("amount", "uint256")]),
        "0xdd62ed3e": ("query::allowance", [("owner", "address"), ("spender", "address")]),
        "0x313ce567": ("query::decimals", []),
        "0x18160ddd": ("query::total_supply", []),
    },
    "controlled-cw20": {
        "0xa9059cbb": ("execute::transfer", [("recipient", "address"), ("amount", "uint256")]),
        "0x23b872dd": ("execute::transfer_from", [("owner", "address"), ("recipient", "address"), ("amount", "uint256")]),
        "0x70a08231": ("query::balance", [("address", "address")]),
        "0x095ea7b3": ("execute::increase_allowance_or_approve", [("spender", "address"), ("amount", "uint256")]),
        "0xdd62ed3e": ("query::allowance", [("owner", "address"), ("spender", "address")]),
        "0x313ce567": ("query::decimals", []),
        "0x18160ddd": ("query::total_supply", []),
    },
    "fee-token-cw20": {
        "0xa9059cbb": ("execute::transfer", [("recipient", "address"), ("amount", "uint256")]),
        "0x23b872dd": ("execute::transfer_from", [("owner", "address"), ("recipient", "address"), ("amount", "uint256")]),
        "0x70a08231": ("query::balance", [("address", "address")]),
        "0x095ea7b3": ("execute::increase_allowance_or_approve", [("spender", "address"), ("amount", "uint256")]),
        "0xdd62ed3e": ("query::allowance", [("owner", "address"), ("spender", "address")]),
        "0x18160ddd": ("query::total_supply", []),
        "0x40c10f19": ("execute::mint", [("recipient", "address"), ("amount", "uint256")]),
    },
    "wrapped-native-token": {
        "0xa9059cbb": ("execute::transfer", [("recipient", "address"), ("amount", "uint256")]),
        "0x23b872dd": ("execute::transfer_from", [("owner", "address"), ("recipient", "address"), ("amount", "uint256")]),
        "0x70a08231": ("query::balance", [("address", "address")]),
        "0x095ea7b3": ("execute::increase_allowance_or_approve", [("spender", "address"), ("amount", "uint256")]),
        "0xdd62ed3e": ("query::allowance", [("owner", "address"), ("spender", "address")]),
        "0xd0e30db0": ("execute::deposit", []),
        "0x2e1a7d4d": ("execute::withdraw", [("amount", "uint256")]),
        "0x18160ddd": ("query::total_supply", []),
        "0x313ce567": ("query::decimals", []),
    },
    "astroport-pair": {
        "0x0902f1ac": ("query::reserves", []),
        "0x022c0d9f": ("execute::swap", [("amount0_out", "uint256"), ("amount1_out", "uint256"), ("recipient", "address"), ("callback_data", "bytes")]),
        "0x6a627842": ("execute::provide_liquidity_mint", [("recipient", "address")]),
        "0x0dfe1681": ("query::asset0", []),
        "0xd21220a7": ("query::asset1", []),
        "0xfff6cae9": ("execute::sync", []),
        "0x70a08231": ("query::lp_balance", [("address", "address")]),
        "0xa9059cbb": ("execute::lp_transfer", [("recipient", "address"), ("amount", "uint256")]),
        "0x23b872dd": ("execute::lp_transfer_from", [("owner", "address"), ("recipient", "address"), ("amount", "uint256")]),
        "0x095ea7b3": ("execute::lp_approve", [("spender", "address"), ("amount", "uint256")]),
    },
    "cw721-mintable": {
        "0x42842e0e": ("execute::transfer_nft", [("sender", "address"), ("recipient", "address"), ("token_id", "uint256")]),
        "0xb88d4fde": ("execute::send_or_safe_transfer_nft", [("sender", "address"), ("recipient", "address"), ("token_id", "uint256"), ("data", "bytes")]),
        "0x23b872dd": ("execute::transfer_nft", [("sender", "address"), ("recipient", "address"), ("token_id", "uint256")]),
        "0xa22cb465": ("execute::approve_all", [("operator", "address"), ("approved", "bool")]),
        "0x081812fc": ("query::approval", [("token_id", "uint256")]),
        "0x6352211e": ("query::owner_of", [("token_id", "uint256")]),
    },
    "cw721-drop": {
        "0x42842e0e": ("execute::transfer_nft", [("sender", "address"), ("recipient", "address"), ("token_id", "uint256")]),
        "0xb88d4fde": ("execute::transfer_nft", [("sender", "address"), ("recipient", "address"), ("token_id", "uint256"), ("data", "bytes")]),
        "0x23b872dd": ("execute::transfer_nft", [("sender", "address"), ("recipient", "address"), ("token_id", "uint256")]),
        "0x095ea7b3": ("execute::approve_nft", [("spender", "address"), ("token_id", "uint256")]),
        "0xa22cb465": ("execute::approve_all", [("operator", "address"), ("approved", "bool")]),
        "0x081812fc": ("query::approval", [("token_id", "uint256")]),
        "0x6352211e": ("query::owner_of", [("token_id", "uint256")]),
        "0x70a08231": ("query::balance", [("owner", "address")]),
        "0x18160ddd": ("query::total_supply", []),
        # Reviewed S1 mint/drop selectors. Complex proof/signature payloads remain public-input
        # fingerprints in the executable adapter; no historical storage keys are consumed.
        "0xb03bc27c": ("execute::multi_stage_mint_drop", [("quantity", "uint256")]),
        "0xa0712d68": ("execute::mint_drop", [("quantity", "uint256")]),
        "0x6ecd2306": ("execute::mint_drop", [("quantity", "uint256")]),
        "0x0d1d7ae5": ("execute::mint_drop", [("quantity", "uint256")]),
        "0x26092b83": ("execute::mint_drop_one", []),
        "0x2955a21d": ("execute::signed_mint_drop", [("signature", "bytes"), ("nonce", "uint256"), ("quantity", "uint256"), ("max_mints_per_wallet", "uint256"), ("recipient", "address")]),
        "0x9e852f75": ("execute::whitelist_mint_drop", [("quantity", "uint256")]),
        "0x64869dad": ("execute::mint_seadrop", [("recipient", "address"), ("quantity", "uint256")]),
        "0xefef39a1": ("execute::purchase_drop", [("quantity", "uint256")]),
        "0x2db11544": ("execute::mint_drop", [("quantity", "uint256")]),
    },
    "stargate-cw20": {
        "0xa9059cbb": ("execute::transfer", [("recipient", "address"), ("amount", "uint256")]),
        "0x23b872dd": ("execute::transfer_from", [("owner", "address"), ("recipient", "address"), ("amount", "uint256")]),
        "0x70a08231": ("query::balance", [("address", "address")]),
        "0x095ea7b3": ("execute::approve", [("spender", "address"), ("amount", "uint256")]),
        "0xdd62ed3e": ("query::allowance", [("owner", "address"), ("spender", "address")]),
        "0x18160ddd": ("query::total_supply", []),
        "0x2e15238c": ("execute::bridge_send", [("dst_chain_id", "uint256"), ("to", "bytes"), ("quantity", "uint256"), ("zro_payment_address", "address"), ("adapter_param", "bytes")]),
        "0x001d3567": ("execute::bridge_receive", [("src_chain_id", "uint256"), ("src_address", "bytes"), ("nonce", "uint256"), ("payload", "bytes")]),
    },
    "xen-like": {
        "0x1c560305": ("execute::claim_mint_reward_and_share", [("other", "address"), ("pct", "uint256")]),
        "0x9ff054df": ("execute::claim_rank", [("term", "uint256")]),
        "0xa9059cbb": ("execute::transfer", [("recipient", "address"), ("amount", "uint256")]),
        "0x70a08231": ("query::balance", [("address", "address")]),
        "0x23b872dd": ("execute::transfer_from", [("owner", "address"), ("recipient", "address"), ("amount", "uint256")]),
    },
    # Source-reviewed marketplace/router selectors. Complex tuple payloads are intentionally not
    # ABI-decoded here: the executable adapter derives a deterministic calldata fingerprint as a
    # semantic order/route identifier. Unknown observed selectors remain mapped-opaque.
    "marketplace-router": {
        # Seaport 1.1 / 1.4 settlement, cancellation, validation, and counter actions.
        "0x00000000": ("execute::settle_order", []),
        "0x87201b41": ("execute::settle_order", []),
        "0xf2d12b12": ("execute::settle_order", []),
        "0xe7acab24": ("execute::settle_order", []),
        "0xfb0f3ee1": ("execute::settle_order", []),
        "0xb3a34c4c": ("execute::settle_order", []),
        "0xed98a574": ("execute::settle_order", []),
        "0x55944a42": ("execute::settle_order", []),
        "0xfd9f1e10": ("execute::cancel_order", []),
        "0x88147732": ("execute::validate_order", []),
        "0x5b34b966": ("execute::increment_counter", []),
        # Retained for frozen S3 map compatibility; S1 maps the Universal Router owner to the
        # dedicated universal-router family below.
        "0x3593564c": ("execute::execute_route", []),
        "0x24856bc3": ("execute::execute_route", []),
        "0xfa461e33": ("execute::v3_swap_callback", []),
        # Blur Exchange execute/_execute/bulkExecute and trader nonce.
        "0xe04d94ae": ("execute::blur_settle", []),
        "0x9a1fc3a7": ("execute::blur_settle", []),
        "0xb3be57f8": ("execute::blur_settle", []),
        "0x627cdcb9": ("execute::increment_counter", []),
    },
}


# S1-only reviewed selector extensions. Keeping them out of the frozen global ENTRYPOINTS table
# preserves the already-published S3 semantic/binary workload while allowing the larger S1 trace
# to use additional source-reviewed entrypoints.
S1_ENTRYPOINT_EXTENSIONS: dict[str, dict[str, tuple[str, list[tuple[str, str]]]]] = {
    "fiat-token-cw20": {
        "0xa9059cbb": ("execute::transfer", [("recipient", "address"), ("amount", "uint256")]),
        "0x23b872dd": ("execute::transfer_from", [("owner", "address"), ("recipient", "address"), ("amount", "uint256")]),
        "0x70a08231": ("query::balance", [("address", "address")]),
        "0x095ea7b3": ("execute::increase_allowance_or_approve", [("spender", "address"), ("amount", "uint256")]),
        "0xdd62ed3e": ("query::allowance", [("owner", "address"), ("spender", "address")]),
        "0x42966c68": ("execute::burn", [("amount", "uint256")]),
        "0x40c10f19": ("execute::mint", [("recipient", "address"), ("amount", "uint256")]),
        "0xd505accf": ("execute::permit", [("owner", "address"), ("spender", "address"), ("amount", "uint256"), ("deadline", "uint256"), ("v", "uint256"), ("r", "bytes32"), ("s", "bytes32")]),
        "0x313ce567": ("query::decimals", []),
        "0x18160ddd": ("query::total_supply", []),
    },
    "cw721-drop": {
        "0xdb980f4f": ("execute::mint_phase_drop", [("phase_index", "uint256"), ("quantity", "uint256")]),
        # Bueno721Drop verified source: mintPhaseAllowlist(uint256,uint64,bytes32[]).
        "0x97474f13": ("execute::mint_phase_allowlist_drop", [("phase_index", "uint256"), ("quantity", "uint256"), ("proof", "bytes")]),
        # Bueno721Drop verified source: mintBatch(uint64[],bytes32[][],uint256[],uint64).
        # Dynamic-array quantity is decoded by the executable adapter and checked against mint logs.
        "0x9ff70755": ("execute::mint_batch_drop", []),
    },
    "xen-like": {
        "0x52c7f8dc": ("execute::claim_mint_reward", []),
    },
    "marketplace-router": {
        "0x46423aa7": ("query::get_order_status", [("order_hash", "bytes32")]),
        "0xf07ec373": ("query::get_counter", [("offerer", "address")]),
        "0x79df72bd": ("reviewed::get_order_hash_stateless", []),
        "0xa8174404": ("execute::settle_order", []),
    },
    "universal-router": {
        "0x3593564c": ("execute::execute_route", []),
        "0x24856bc3": ("execute::execute_route", []),
        "0xfa461e33": ("execute::v3_swap_callback", []),
        "0x": ("reviewed::receive_eth_stateless", []),
    },
}


# S1 owner-scoped reviewed selectors. These intentionally do not live in the family-wide table:
# a 4-byte selector can collide across unrelated contracts.
#
# Blitkin is an EIP-1967 proxy whose historical implementation is stable across S1. 0x29a0eee8 is
# independently decoded as mint(uint8,uint8). The second reviewed path, 0xc96602d9, resolves to
# allowlistMint(uint8,uint8,bytes32[]): the S1 effect audit observes a 96-byte ABI head, an 11/12-item
# bytes32 proof, a fixed 0.05 ETH payment, and exactly one committed ERC-721 mint to msg.sender per
# successful source transaction. Both adapters mint one token; the independent all-owner ERC-721
# Transfer-log sequence audit still fail-closes if translated committed mint counts diverge.
#
# MIA 0xfd883998 remains ABI-name agnostic on purpose. The owner-scoped public-log audit establishes
# the state effect needed by this benchmark: 1323/1325 calls commit, every committed call emits
# exactly one ERC-721 Transfer(from=0), every mint recipient is the EVM-visible msg.sender, the two
# reverted calls emit no committed mint, and there are no other owner mint transactions in S1. The
# execution adapter consumes the frozen audit JSON for the exact public event token ID instead of
# guessing a calldata layout.


# S4-only reviewed selector extensions. These are intentionally separate from the frozen S1/S3
# tables so later S4 family review cannot mutate the already-published workloads. The Banana Gun
# router is modeled conservatively as a router-global execution guard; SEEK 0x3bb1ee11 preserves
# only the mint dependency class (one logical mint event per source call), not exact EVM mint count.
S4_ENTRYPOINT_EXTENSIONS: dict[str, dict[str, tuple[str, list[tuple[str, str]]]]] = {
    "fiat-token-cw20": {
        "0xa9059cbb": ("execute::transfer", [("recipient", "address"), ("amount", "uint256")]),
        "0x23b872dd": ("execute::transfer_from", [("owner", "address"), ("recipient", "address"), ("amount", "uint256")]),
        "0x70a08231": ("query::balance", [("address", "address")]),
        "0x095ea7b3": ("execute::increase_allowance_or_approve", [("spender", "address"), ("amount", "uint256")]),
        "0xdd62ed3e": ("query::allowance", [("owner", "address"), ("spender", "address")]),
        "0x18160ddd": ("query::total_supply", []),
        "0x313ce567": ("query::decimals", []),
        # Source-reviewed FiatToken supply mutations; the native adapter already preserves
        # recipient/caller balance and total-supply dependency classes.
        "0x40c10f19": ("execute::mint", [("recipient", "address"), ("amount", "uint256")]),
        "0x42966c68": ("execute::burn", [("amount", "uint256")]),
    },
    "cw721-drop": {
        "0x3bb1ee11": ("execute::mint_drop_one", []),
    },
    "xen-like": {
        # XEN verified ABI: reviewed mint/stake lifecycle selectors. The native XEN adapter
        # implements the same logical global/user-mint/user-stake dependency classes.
        "0x52c7f8dc": ("execute::claim_mint_reward", []),
        "0x5bccb4c4": ("execute::claim_mint_reward_and_stake", [("pct", "uint256"), ("term", "uint256")]),
        "0x7b0472f0": ("execute::stake", [("amount", "uint256"), ("term", "uint256")]),
        "0x3ccfd60b": ("execute::withdraw", []),
    },
    "custom-swap-router": {
        "0x0162e2d0": ("execute::execute_route", []),
        "0x70fef1da": ("execute::execute_route", []),
        "0x": ("execute::execute_route", []),
    },
    "v3-pool-lock": {
        # Frozen S4 pool 0x844e... is the MUBI/WETH Uniswap-V3 pool. Preserve only the
        # observed per-pool swap dependency class; no tick/liquidity/EVM equivalence is claimed.
        "0x128acb08": ("execute::execute_route", []),
    },
    "amp-partition-collateral-lock": {
        # P12 has no direct owner selector in the frozen review queue. Keep common ERC20 paths
        # conservative if they are surfaced by later call-frame translation; partition-specific
        # selectors remain opaque until separately reviewed.
        "0xa9059cbb": ("execute::execute_route", []),
        "0x23b872dd": ("execute::execute_route", []),
        "0x095ea7b3": ("execute::execute_route", []),
    },
    "linea-rollup-lock": {
        # Frozen S4 0xd19d... selector 0x4165d6dd is explorer-identified Finalize Blocks.
        "0x4165d6dd": ("execute::execute_route", []),
    },
    "zksync-l1-system-lock": {
        # Official ZKsync SDK binding: requestL2Transaction(...).
        "0xeb672419": ("execute::execute_route", []),
    },
    "arbitrum-bridge-lock": {
        # No dominant owner selector is asserted by the frozen P15 review; family-level bridge
        # coverage is reviewed, while selector-level execution remains fail-closed.
    },
}

# S4 owner-scoped selector reviews. These selectors are deliberately not family-wide: S4's
# cw721-drop family aliases several unrelated ERC721 implementations, and a 4-byte selector alone
# is not evidence that every collection shares the same mint policy/state layout.
#
# 0x45c770... is the 81-82-83-84 ERC721 proxy. Its reviewed Collection implementation exposes
# mint(bytes16 collectionId,address user,uint16 num,uint32 nonce,bytes signature), selector
# 0xd2e8281f. collectionId/nonce/signature feed the external Alba verifier; the owner-local Collection
# state effect is represented only by the recipient/quantity mint dependency classes.
#
# 0x1c67d8... (FU STUDIO MEMBERSHIP) and 0xc374a2... (Project AEON representative) are
# source-reviewed Archetype ERC721 collections. Archetype v0.3.x mint(Auth,uint256,address,bytes)
# (0x4a21a2df) mints to msg.sender and accounts per-wallet/per-invite-key state. The nested Auth key
# is recovered from public calldata by the executable adapter; Merkle/signature verification itself
# is intentionally outside the scheduler dependency model.
S4_OWNER_ENTRYPOINT_EXTENSIONS: dict[str, dict[str, tuple[str, list[tuple[str, str]]]]] = {
    "0x45c77068a17ac94f56b7fd59dca0d4bd50457216": {
        "0xd2e8281f": (
            "execute::collection_mint_drop",
            [
                ("collection_id", "bytes32"),
                ("recipient", "address"),
                ("quantity", "uint256"),
                ("nonce", "uint256"),
                ("signature", "bytes"),
            ],
        ),
    },
    "0x1c67d8f07d7ef2d637e61ed3fbc3fa9aaf7a6267": {
        "0x4a21a2df": (
            "execute::archetype_mint_drop",
            [
                ("auth_offset", "uint256"),
                ("quantity", "uint256"),
                ("affiliate", "address"),
                ("signature", "bytes"),
            ],
        ),
    },
    "0xc374a204334d4edd4c6a62f0867c752d65e9579c": {
        "0x4a21a2df": (
            "execute::archetype_mint_drop",
            [
                ("auth_offset", "uint256"),
                ("quantity", "uint256"),
                ("affiliate", "address"),
                ("signature", "bytes"),
            ],
        ),
    },
    # Same runtime-code family as the source-reviewed Project AEON Archetype owner above.
    "0x5a7c3aedaf077accd041799f01264dcacb17eea2": {
        "0x4a21a2df": (
            "execute::archetype_mint_drop",
            [("auth_offset", "uint256"), ("quantity", "uint256"), ("affiliate", "address"), ("signature", "bytes")],
        ),
    },
    # L3E7 Worlds is an owner-scoped ERC721 proxy. Frozen calls decode as
    # whitelistMint(uint256,bytes32[]); preserve caller/quantity plus a deterministic
    # proof-derived whitelist dependency rather than claiming Merkle verification equivalence.
    "0x20577896ea6113ed8c94b2f08f3893bdc08eba22": {
        "0xd2cab056": ("execute::whitelist_mint_drop", [("quantity", "uint256")]),
    },
    # Arbitrum One Bridge state-mutating entrypoints. Both feed the already-reviewed
    # per-bridge singleton dependency abstraction; delayedMessageCount() stays read/opaque.
    "0x8315177ab297ba92a06054ce80a67ed4dbd7ed3a": {
        "0x7a88b107": ("execute::execute_route", []),
        "0x86598a56": ("execute::execute_route", []),
    },
    # Arbitrum SequencerInbox batch submission mutates inbox/bridge sequencing state.
    "0x1c479675ad559dc151f6ec7ed3fbf8cee79582b6": {
        "0x8f111f3c": ("execute::execute_route", []),
    },
    # Banana Gun's standard Uniswap-V3 callback is source/ABI-identifiable; keep the
    # less certain 0x244a7353 selector opaque.
    "0xdb5889e35e379ef0498aae126fc2cce1fbd23216": {
        "0xfa461e33": ("execute::v3_swap_callback", []),
    },
    # Legacy Synthetix implementation: proxy bookkeeping and transfer both mutate the
    # reviewed per-owner system-lock namespace.
    "0xd0da9cbea9c3852c5d63a95f9abcc4f6ea0f9032": {
        "0xbc67f832": ("execute::execute_route", []),
        "0xa9059cbb": ("execute::execute_route", []),
    },
    # Starknet Core L1->L2 message publication increments messaging state/nonces.
    "0xc662c410c0ecf747543f5ba90660f6abebd9c8c4": {
        "0x3e3aa6c5": ("execute::execute_route", []),
    },
    # zkSync Era main DiamondProxy executor facet. These are the source-reviewed
    # commit/prove/execute block-state transitions and share the existing per-chain lock.
    "0x32400084c286cf3e17e7b677ea9583e60a000324": {
        "0x7739cbe7": ("execute::execute_route", []),
        "0x0c4dd810": ("execute::execute_route", []),
        "0xce9dcf16": ("execute::execute_route", []),
    },
    # Arbitrum Bridge delayed-message enqueue is a bridge-global mutation. The three
    # count/accumulator views are reviewed as reads of that same conservative singleton.
    "0x8315177ab297ba92a06054ce80a67ed4dbd7ed3a": {
        "0x7a88b107": ("execute::execute_route", []),
        "0x86598a56": ("execute::execute_route", []),
        "0x8db5993b": ("execute::execute_route", []),
        "0xeca067ad": ("query::read_route_lock", []),
        "0x0084120c": ("query::read_route_lock", []),
        "0x16bf5579": ("query::read_route_lock", [("index", "uint256")]),
    },
    # Linea L1 Message Service sendMessage mutates the reviewed global message-service state.
    "0xd19d4b5d358258f05d7b411e21a1460d11b0876f": {
        "0x9f3ce55a": ("execute::execute_route", []),
    },
    # The reviewed MUBI/WETH Uniswap-V3 pool uses the same per-pool conservative lock for
    # liquidity burn/mint state transitions.
    "0x844eb5c280f38c7462316aad3f338ef9bda62668": {
        "0xa34123a7": ("execute::execute_route", []),
        "0x3c8a7d8d": ("execute::execute_route", []),
    },
    # BetIT verified source is a fee/limit token; enableTrading mutates shared trading policy.
    "0xa3c519683010d59fa54a4a6c4cac0f55cb20bb3f": {
        "0x8a8c523c": ("execute::set_policy_marker", []),
    },
    # Standard transferFrom on the reviewed Synthetix implementation mutates its system state.
    "0xd0da9cbea9c3852c5d63a95f9abcc4f6ea0f9032": {
        "0xbc67f832": ("execute::execute_route", []),
        "0xa9059cbb": ("execute::execute_route", []),
        "0x23b872dd": ("execute::execute_route", []),
    },
    # Final scheduler-fidelity micro-closure. These custom/admin token selectors are
    # owner-scoped and deliberately collapse only to cw20-base's singleton policy marker.
    # For OnBot, the exact function names remain unresolved, but the frozen successful
    # frames are state-capable on this exact ERC20 owner. The scheduler profile therefore
    # reviews only their conservative owner-local mutation dependency class, not their
    # return values, authorization, storage layout, or tokenomics.
    "0xb912cfd8cd814988b794b5301785c12c71b51651": {
        "0xfbd75753": ("execute::set_policy_marker", []),
        "0x27193bc4": ("execute::set_policy_marker", []),
        "0xf576f539": ("execute::set_policy_marker", []),
    },
    # Spacecraft admin-policy paths: updateTaxes(), renounceOwnership(), plus one
    # frozen state-capable owner-admin selector whose exact text signature is unresolved.
    "0xc8d2f14c33064c810efa29ee7648ccca0cd1f772": {
        "0x1006ee0c": ("execute::set_policy_marker", []),
        "0x715018a6": ("execute::set_policy_marker", []),
        "0xf319ae77": ("execute::set_policy_marker", []),
    },
    # Owner-scoped OpenZeppelin renounceOwnership() on the reviewed Pepe token owner.
    "0x224da25c58574b852876a1c4e289be9eb7345322": {
        "0x715018a6": ("execute::set_policy_marker", []),
    },
    # Exact selector signature removeLimits(); this owner remains selector-scoped and
    # only this newly reviewed policy mutation is added.
    "0x0b3ddf435d7e0a3cad97d85f94633a0e3a69fc01": {
        "0x751039fc": ("execute::set_policy_marker", []),
    },
    # BetIT source-backed fee-policy mutation in addition to enableTrading().
    "0xa3c519683010d59fa54a4a6c4cac0f55cb20bb3f": {
        "0x8a8c523c": ("execute::set_policy_marker", []),
        "0x02dbd8f8": ("execute::set_policy_marker", [("marketing_fee", "uint256"), ("dev_fee", "uint256")]),
    },
    # Standard ERC20 mint(address,uint256) on a frozen successful cw20-base owner.
    # The native cw20-base Mint path is intentionally permissionless; the source caller is
    # retained only as provenance because the source transaction already succeeded.
    "0x143d7a700a533b4baf6d693449b278a8a2f5927d": {
        "0x40c10f19": ("execute::mint", [("recipient", "address"), ("amount", "uint256")]),
    },
    # Tiny Linea residuals: a successful state-capable system call plus a standard approve
    # observed on reviewed Linea owners. Scheduler fidelity needs only the conservative
    # per-owner system-lock dependency; exact EVM state/return-value equivalence is excluded.
    "0xf64bae65f6f2a5277571143a24faafdfc0c2a737": {
        "0xf6a3c090": ("execute::execute_route", []),
    },
    "0x046eee2cc3188071c02bfc1745a6b17c656e3f3d": {
        "0x095ea7b3": ("execute::execute_route", [("spender", "address"), ("amount", "uint256")]),
    },
}


S1_OWNER_ENTRYPOINT_EXTENSIONS: dict[str, dict[str, tuple[str, list[tuple[str, str]]]]] = {
    "0xbd18e233e12f2a066f5b5a351285ab5a39b1f2ac": {
        "0x29a0eee8": (
            "execute::mint_drop_one",
            [("trunk_id", "uint8"), ("critter_id", "uint8")],
        ),
        # allowlistMint(uint8,uint8,bytes32[]). Only the two static trait words are decoded here;
        # the dynamic Merkle proof is validation input, not a native storage key.
        "0xc96602d9": (
            "execute::allowlist_mint_drop_one",
            [("trunk_id", "uint8"), ("critter_id", "uint8")],
        ),
    },
    "0x885523263378d6f27a5b8c533ad3b05ab9e105b5": {
        "0xfd883998": ("execute::mint_verified_event", []),
    },
    # Owner-scoped ERC-721 mint/airdrop paths recovered by the strict public Transfer-log
    # reconciliation. Keep these out of the family-wide selector table: the same 4-byte selector
    # may have unrelated semantics on another drop implementation. Dynamic-array paths are decoded
    # again by the executable adapter and fail closed if the observed S1 shape is not representable.
    "0x798116c6858dc4be729820d36554c4c427629744": {
        # Bueno721Drop: airdropPublic(uint64[] quantities,address[] recipients).
        "0xba09f3d7": ("execute::airdrop_public_drop", []),
    },
    "0x925fe29ff5db1614e1344c803543ccbf60fd1641": {
        "0xcc47a40b": ("execute::reserve_drop", [("recipient", "address"), ("quantity", "uint256")]),
    },
    "0xf66ef61f504a6d326d7bf1771f4b613af57c7126": {
        "0xcc47a40b": ("execute::reserve_drop", [("recipient", "address"), ("quantity", "uint256")]),
    },
    "0x0e6d176b5c50e2600da92c8ea7f4eed178e9bd07": {
        "0x8ba4cc3c": ("execute::airdrop_drop", [("recipient", "address"), ("quantity", "uint256")]),
    },
    "0x1b1d2dccc2d3f25d7791e9dc4751856ec5eeafaa": {
        # airdrop(address[] recipients,uint256 numberOfTokens). S1 has one recipient in this path.
        "0xc204642c": ("execute::airdrop_array_drop", []),
    },
    "0x7974e0b19d8ee4daf3fdfecb2420507c198d3dbe": {
        # Bueno721Drop: airdropForPhase(uint256,uint64[],address[]).
        "0x93a69f89": ("execute::airdrop_phase_drop", []),
    },
    # One Bitcoin Bandits mint path remains ABI-name agnostic. Its only S1 invocation is promoted
    # from the frozen public mint-event effect, not from a guessed function signature.
    "0xeae506c1bcd0f77f0802ca630f65bca442ba0bd9": {
        "0x2f6f98e1": ("execute::mint_event_backed_drop", []),
    },
    # These two in-window CREATE frames each commit an ERC-721 constructor mint. Native instances
    # are pre-instantiated for replay, so model the constructor's mint as one event-backed execute.
    "0x50f210587307f0d0f5a963b77f6d25885567e336": {
        "0x": ("execute::constructor_mint_event_backed_drop", []),
    },
    "0xe3d28e90f110db9ccc056240aab5330a609b7c2e": {
        "0x": ("execute::constructor_mint_event_backed_drop", []),
    },
    # Blur Exchange V1 proxy. The verified Blur V1 ABI binds 0xf4acd740 to
    # cancelOrder((address,uint8,address,address,uint256,uint256,address,uint256,uint256,uint256,
    # (uint16,address)[],uint256,bytes)). Keep this owner-scoped because the tuple is Blur-specific
    # and a 4-byte selector must not promote an unrelated marketplace contract. The executable
    # marketplace adapter fingerprints the full order calldata, giving each cancelled order a
    # deterministic native order-state key without importing EVM storage slots.
    "0x000000000000ad05ccc4f10045630fb830b95127": {
        "0xf4acd740": ("execute::cancel_order", []),
    },
}


SEMANTIC_STATE_READ = "STATE_READ"
SEMANTIC_STATE_WRITE = "STATE_WRITE"
SEMANTIC_READ_WRITE = "READ_WRITE"
SEMANTIC_PURE = "PURE"
SEMANTIC_OPAQUE = "OPAQUE"


def semantic_effect_for_entrypoint(entrypoint: str | None, dispatch: str | None = None) -> str:
    """Classify reviewed entrypoints by state dependency, not by call mutability alone.

    The S1 public-RPC conflict corpus is a touched-state instrument. Queries therefore count as
    reviewed STATE_READ semantics; pure/public-input computations do not. Execute paths are kept
    conservatively READ_WRITE because the scheduler only needs to know that the reviewed path can
    participate in a state conflict, not reproduce Solidity mutability annotations exactly.
    """
    ep = str(entrypoint or "")
    if not ep:
        return SEMANTIC_OPAQUE
    if ep.startswith("reviewed::"):
        return SEMANTIC_PURE
    if ep.startswith("query::"):
        return SEMANTIC_STATE_READ
    if ep.startswith("execute::"):
        return SEMANTIC_READ_WRITE
    if ep == "system::bank_send":
        return SEMANTIC_READ_WRITE
    if ep.startswith("system::"):
        return SEMANTIC_PURE
    if dispatch in {"system-bank-transfer"}:
        return SEMANTIC_READ_WRITE
    return SEMANTIC_OPAQUE


def read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def write_json_atomic(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    tmp.replace(path)


def normalize_address(value: str | None) -> str | None:
    if value is None:
        return None
    text = str(value).lower()
    if text.startswith("0x"):
        text = text[2:]
    if len(text) != 40:
        return None
    try:
        int(text, 16)
    except ValueError:
        return None
    return "0x" + text


def normalize_runtime_code(code: str | None) -> str:
    text = str(code or "").lower()
    if text.startswith("0x"):
        text = text[2:]
    if not text:
        return ""
    if len(text) % 2:
        text = "0" + text
    bytes.fromhex(text)
    return text


def runtime_code_family(code: str | None) -> str | None:
    normalized = normalize_runtime_code(code)
    if not normalized:
        return None
    return hashlib.sha256(bytes.fromhex(normalized)).hexdigest()


def load_code_cache(path: Path) -> dict[str, dict]:
    raw = read_json(path)
    if not isinstance(raw, dict):
        raise ValueError(f"{path}: code cache must be an object")
    return {str(key).lower(): value for key, value in raw.items() if isinstance(value, dict)}


def load_call_cache(blocks: list[dict], cache_dir: Path) -> dict[int, dict]:
    result: dict[int, dict] = {}
    for block in blocks:
        number = int(block["block_number"])
        path = cache_dir / f"{number}.json"
        if not path.exists():
            raise FileNotFoundError(f"missing callTracer cache for block {number}: {path}")
        cached = read_json(path)
        traced = cached.get("transactions") or []
        transactions = block.get("transactions") or []
        if len(traced) != len(transactions):
            raise ValueError(
                f"block {number}: corpus has {len(transactions)} transactions but call cache has {len(traced)}"
            )
        expected_hash = str(block.get("block_hash") or "").lower()
        observed_hash = str(cached.get("block_hash") or "").lower()
        if expected_hash and expected_hash != observed_hash:
            raise ValueError(f"block {number}: call cache block hash mismatch")
        for index, (source, trace) in enumerate(zip(transactions, traced)):
            if str(source.get("tx_hash") or "").lower() != str(trace.get("tx_hash") or "").lower():
                raise ValueError(f"block {number} tx {index}: call cache transaction hash mismatch")
            if not isinstance(trace.get("result"), dict):
                raise ValueError(f"block {number} tx {index}: call cache has no root frame")
        result[number] = cached
    return result


class FamilyResolver:
    def __init__(self, frozen_map: dict, code_cache: dict[str, dict], mapping_candidates: dict):
        self.dataset = str(frozen_map.get("dataset") or "")
        self.profile_to_native = {
            str(item["ethereum_profile_family"]): str(item["native_code_family"])
            for item in frozen_map.get("profile_mappings", [])
        }
        self.explicit_owner_to_profile: dict[str, str] = {}
        for item in frozen_map.get("profile_mappings", []):
            profile = str(item.get("ethereum_profile_family") or "")
            for raw_owner in item.get("storage_owner_scope") or []:
                owner = normalize_address(raw_owner)
                if owner is None or not profile:
                    continue
                previous = self.explicit_owner_to_profile.get(owner)
                if previous is not None and previous != profile:
                    raise ValueError(
                        f"family map assigns storage owner {owner} to multiple profiles: "
                        f"{previous}, {profile}"
                    )
                self.explicit_owner_to_profile[owner] = profile
        self.code_cache = code_cache
        self.resolution_by_owner = {
            normalize_address(item.get("storage_owner")): item
            for item in mapping_candidates.get("resolution_records", [])
            if normalize_address(item.get("storage_owner")) is not None
        }
        self._direct_cache: dict[str, str | None] = {}

    def direct_profile(self, address: str | None) -> str | None:
        address = normalize_address(address)
        if address is None:
            return None
        if address in self._direct_cache:
            return self._direct_cache[address]
        entry = self.code_cache.get(address)
        family = runtime_code_family(entry.get("code")) if entry is not None else None
        self._direct_cache[address] = family
        return family

    def profile_for_storage_context(self, address: str | None) -> str | None:
        address = normalize_address(address)
        if address is None:
            return None
        explicit = self.explicit_owner_to_profile.get(address)
        if explicit is not None:
            return explicit
        resolution = self.resolution_by_owner.get(address)
        if resolution is not None:
            return str(resolution.get("recommended_profile_family") or "") or None
        return self.direct_profile(address)

    def native_family_for_storage_context(self, address: str | None) -> tuple[str | None, str | None]:
        profile = self.profile_for_storage_context(address)
        return profile, self.profile_to_native.get(profile) if profile else None

    def runtime_code_status(self, address: str | None) -> str:
        """Return ``empty``, ``nonempty`` or ``unknown`` for a historical address.

        The planner only turns an address into an EOA/no-code system action when the historical
        code cache positively records empty runtime bytecode.  Missing cache data remains fallback.
        """
        address = normalize_address(address)
        if address is None:
            return "unknown"
        entry = self.code_cache.get(address)
        if entry is None:
            return "unknown"
        return "nonempty" if normalize_runtime_code(entry.get("code")) else "empty"


def _calldata_bytes(input_hex: str | None) -> bytes:
    text = str(input_hex or "0x").lower()
    if text.startswith("0x"):
        text = text[2:]
    if len(text) % 2:
        text += "0"
    try:
        return bytes.fromhex(text)
    except ValueError:
        return b""


def _decode_static_word(word: bytes, kind: str) -> Any:
    if len(word) != 32:
        return None
    if kind == "address":
        return "0x" + word[-20:].hex()
    if kind == "uint256":
        return int.from_bytes(word, "big")
    if kind == "uint8":
        value = int.from_bytes(word, "big")
        return value if value <= 0xFF else None
    if kind == "bool":
        return bool(int.from_bytes(word, "big"))
    if kind == "bytes32":
        return "0x" + word.hex()
    return None


def decode_arguments(input_hex: str | None, schema: list[tuple[str, str]]) -> dict[str, Any]:
    data = _calldata_bytes(input_hex)
    payload = data[4:] if len(data) >= 4 else b""
    result: dict[str, Any] = {}
    for index, (name, kind) in enumerate(schema):
        start = index * 32
        word = payload[start:start + 32]
        if len(word) != 32:
            result[name] = {"decode_status": "missing-word"}
            continue
        if kind != "bytes":
            result[name] = _decode_static_word(word, kind)
            continue
        offset = int.from_bytes(word, "big")
        if offset + 32 > len(payload):
            result[name] = {"decode_status": "invalid-offset"}
            continue
        length = int.from_bytes(payload[offset:offset + 32], "big")
        value = payload[offset + 32:offset + 32 + length]
        if len(value) != length:
            result[name] = {"decode_status": "truncated-dynamic-bytes"}
        else:
            result[name] = "0x" + value.hex()
    return result


def selector_from_frame(frame: dict) -> str:
    data = str(frame.get("input") or "0x").lower()
    return data[:10] if len(data) >= 10 else "0x"


def int_value(value: Any) -> int:
    if isinstance(value, int):
        return max(0, value)
    text = str(value or "0").strip().lower()
    try:
        return int(text, 16) if text.startswith("0x") else int(text)
    except ValueError:
        return 0


def precompile_name(address: str | None) -> str | None:
    address = normalize_address(address)
    if address is None:
        return None
    number = int(address[2:], 16)
    return PRECOMPILE_NAMES.get(number)


def classify_system_call(frame: dict, resolver: FamilyResolver) -> dict[str, Any] | None:
    """Map state-free/basic EVM calls that do not require a symbolic contract profile."""
    call_type = str(frame.get("type") or "UNKNOWN").upper()
    if call_type not in {"CALL", "STATICCALL"}:
        return None
    code_address = normalize_address(frame.get("to"))
    if code_address is None:
        return None

    precompile = precompile_name(code_address)
    if precompile is not None:
        return {
            "dispatch": "system-precompile",
            "entrypoint": f"system::precompile::{precompile}",
            "system_action_kind": "ethereum-precompile",
            "arguments": {
                "precompile": precompile,
                "input": str(frame.get("input") or "0x").lower(),
                "value": int_value(frame.get("value")),
            },
        }

    if resolver.runtime_code_status(code_address) != "empty":
        return None
    value = int_value(frame.get("value"))
    if call_type == "CALL" and value > 0:
        return {
            "dispatch": "system-bank-transfer",
            "entrypoint": "system::bank_send",
            "system_action_kind": "plain-value-transfer",
            "arguments": {
                "sender": normalize_address(frame.get("from")),
                "recipient": code_address,
                "amount_wei": value,
            },
        }
    return {
        "dispatch": "system-noop-call",
        "entrypoint": "system::noop_call",
        "system_action_kind": "empty-code-noop",
        "arguments": {
            "caller": normalize_address(frame.get("from")),
            "recipient": code_address,
            "input": str(frame.get("input") or "0x").lower(),
        },
    }


def translate_call_tree(
    root: dict,
    resolver: FamilyResolver,
) -> list[dict]:
    actions: list[dict] = []

    def walk(
        frame: dict,
        depth: int,
        parent_action: int | None,
        parent_storage_context: str | None,
        parent_native_family: str | None,
        parent_instance: str | None,
        parent_msg_sender: str | None,
    ) -> None:
        call_type = str(frame.get("type") or "UNKNOWN").upper()
        code_address = normalize_address(frame.get("to"))
        frame_from = normalize_address(frame.get("from"))
        # geth callTracer `from` is the address that initiated this call frame.  For CALL and
        # STATICCALL that is also the callee-visible msg.sender.  DELEGATECALL is different:
        # EIP-7 preserves CALLER (msg.sender) from the parent execution scope even though the trace
        # frame itself is initiated by the proxy/current execution context.
        msg_sender = parent_msg_sender if call_type == "DELEGATECALL" and parent_msg_sender else frame_from
        system = None if call_type in DELEGATE_TYPES else classify_system_call(frame, resolver)

        if system is not None:
            storage_context = None
            profile = None
            native_family = None
            instance = None
            status = SYSTEM_TRANSLATION_STATUS
        elif call_type in DELEGATE_TYPES:
            storage_context = parent_storage_context or normalize_address(frame.get("from"))
            profile, native_family = resolver.native_family_for_storage_context(storage_context)
            if native_family is None:
                native_family = parent_native_family
            instance = parent_instance if parent_instance else (
                f"{native_family}:{storage_context}" if native_family and storage_context else None
            )
            status = "inlined-delegatecall" if native_family else "background-fallback"
        else:
            storage_context = code_address
            profile, native_family = resolver.native_family_for_storage_context(storage_context)
            instance = (
                f"{native_family}:{storage_context}" if native_family and storage_context else None
            )
            status = "mapped-native-call" if native_family else "background-fallback"

        selector = selector_from_frame(frame) if call_type in CALL_TYPES else "0x"
        entry = ENTRYPOINTS.get(native_family or "", {}).get(selector)
        if resolver.dataset == "vegeta-s1":
            entry = S1_ENTRYPOINT_EXTENSIONS.get(native_family or "", {}).get(selector, entry)
            # Apply target-specific review only after resolving the actual storage namespace. This
            # also covers implementation DELEGATECALL frames because their storage context is the
            # proxy owner, while keeping the selector opaque for every other cw721-drop instance.
            entry = S1_OWNER_ENTRYPOINT_EXTENSIONS.get(storage_context or "", {}).get(selector, entry)
        elif resolver.dataset == "vegeta-s4":
            entry = S4_ENTRYPOINT_EXTENSIONS.get(native_family or "", {}).get(selector, entry)
            # As with S1, apply target-specific review only after resolving the storage namespace.
            # This prevents a selector reviewed for one S4 ERC721 implementation from becoming a
            # family-wide claim for unrelated cw721-drop aliases.
            entry = S4_OWNER_ENTRYPOINT_EXTENSIONS.get(storage_context or "", {}).get(selector, entry)
        system_action_kind = None
        semantic_entrypoint = None
        semantic_effect = SEMANTIC_OPAQUE
        if system is not None:
            dispatch = system["dispatch"]
            entrypoint = system["entrypoint"]
            arguments = system["arguments"]
            system_action_kind = system["system_action_kind"]
        elif status == "inlined-delegatecall":
            # S1 retains reviewed semantic evidence from implementation frames for the proxy storage
            # namespace, but never executes those frames a second time. Frozen S3 keeps its original
            # inlined-helper dispatch byte-for-byte.
            if resolver.dataset in {"vegeta-s1", "vegeta-s4"} and entry is not None:
                semantic_entrypoint, semantic_schema = entry
                dispatch = "inlined-reviewed-entrypoint"
                semantic_effect = semantic_effect_for_entrypoint(semantic_entrypoint, dispatch)
            else:
                dispatch = "inlined-helper-or-implementation"
                semantic_entrypoint = None
                semantic_effect = SEMANTIC_OPAQUE
            entrypoint = None
            arguments = {}
        elif native_family is None:
            dispatch = "background-fallback"
            entrypoint = None
            arguments = {}
        elif entry is None:
            dispatch = "mapped-opaque-selector"
            entrypoint = f"opaque::{selector}"
            arguments = {}
        else:
            entrypoint, schema = entry
            dispatch = "reviewed-stateless-entrypoint" if entrypoint.startswith("reviewed::") else "mapped-entrypoint"
            arguments = decode_arguments(frame.get("input"), schema)

        if status != "inlined-delegatecall":
            semantic_entrypoint = entrypoint
            semantic_effect = semantic_effect_for_entrypoint(entrypoint, dispatch)

        action_id = len(actions)
        action = {
            "action_id": action_id,
            "parent_action_id": parent_action,
            "depth": depth,
            "call_type": call_type,
            "translation_status": status,
            "dispatch": dispatch,
            "system_action_kind": system_action_kind,
            # Compatibility/provenance field: raw geth callTracer frame initiator (`from`).
            # Do not use this as contract msg.sender for DELEGATECALL.
            "ethereum_caller": frame_from,
            "ethereum_msg_sender": msg_sender,
            "ethereum_code_address": code_address,
            "storage_context_address": storage_context,
            "ethereum_profile_family": profile,
            "native_code_family": native_family,
            "native_instance_id": instance,
            "selector": selector,
            "native_entrypoint": entrypoint,
            "arguments": arguments,
            "ethereum_input": str(frame.get("input") or "0x").lower(),
            "ethereum_value": str(frame.get("value") or "0x0").lower(),
            "failed_frame": bool(frame.get("error")),
        }
        if resolver.dataset == "vegeta-s1":
            action["semantic_entrypoint"] = semantic_entrypoint
            action["semantic_effect"] = semantic_effect
        actions.append(action)
        for child in frame.get("calls") or []:
            if isinstance(child, dict):
                walk(
                    child,
                    depth + 1,
                    action_id,
                    storage_context if system is None else parent_storage_context,
                    native_family if system is None else parent_native_family,
                    instance if system is None else parent_instance,
                    msg_sender if system is None else parent_msg_sender,
                )

    walk(root, 0, None, None, None, None, None)
    return actions

def _conflict_pairs_for_key(readers: set[int], writers: set[int]) -> set[tuple[int, int]]:
    touched = readers | writers
    pairs: set[tuple[int, int]] = set()
    for writer in writers:
        for other in touched:
            if writer == other:
                continue
            pairs.add((min(writer, other), max(writer, other)))
    return pairs


def block_conflicts_by_owner(block: dict) -> tuple[set[tuple[int, int]], dict[str, set[tuple[int, int]]]]:
    readers: dict[str, set[int]] = defaultdict(set)
    writers: dict[str, set[int]] = defaultdict(set)
    for index, tx in enumerate(block.get("transactions") or []):
        for key in set(tx.get("reads") or []):
            readers[key].add(index)
        for key in set(tx.get("writes") or []):
            writers[key].add(index)
    all_pairs: set[tuple[int, int]] = set()
    owner_pairs: dict[str, set[tuple[int, int]]] = defaultdict(set)
    for key in set(readers) | set(writers):
        if not writers.get(key):
            continue
        pairs = _conflict_pairs_for_key(readers.get(key, set()), writers[key])
        all_pairs.update(pairs)
        owner = storage_contract(key)
        if owner is not None:
            owner_pairs["0x" + owner].update(pairs)
    return all_pairs, owner_pairs


def source_conflict_coverage(blocks: list[dict], resolver: FamilyResolver) -> dict:
    total: set[tuple[int, int, int]] = set()
    mapped: set[tuple[int, int, int]] = set()
    by_family: dict[str, set[tuple[int, int, int]]] = defaultdict(set)
    per_block = []
    for block in blocks:
        number = int(block["block_number"])
        block_pairs, owner_pairs = block_conflicts_by_owner(block)
        total_ids = {(number, left, right) for left, right in block_pairs}
        mapped_ids: set[tuple[int, int, int]] = set()
        total.update(total_ids)
        for owner, pairs in owner_pairs.items():
            profile, native = resolver.native_family_for_storage_context(owner)
            if native is None or profile is None:
                continue
            ids = {(number, left, right) for left, right in pairs}
            mapped.update(ids)
            mapped_ids.update(ids)
            by_family[native].update(ids)
        per_block.append({
            "block_number": number,
            "total_conflict_pairs": len(total_ids),
            "selected_family_conflict_pairs": len(mapped_ids),
            "coverage": (len(mapped_ids) / len(total_ids)) if total_ids else 1.0,
        })
    return {
        "total_unique_conflict_pairs": len(total),
        "selected_family_unique_conflict_pairs": len(mapped),
        "coverage": (len(mapped) / len(total)) if total else 1.0,
        "by_native_family": [
            {"native_code_family": family, "unique_conflict_pairs": len(pairs)}
            for family, pairs in sorted(by_family.items(), key=lambda item: (-len(item[1]), item[0]))
        ],
        "per_block": per_block,
    }


def percentile(values: list[float], q: float) -> float | None:
    if not values:
        return None
    ordered = sorted(float(value) for value in values)
    if len(ordered) == 1:
        return ordered[0]
    position = (len(ordered) - 1) * q
    low = math.floor(position)
    high = math.ceil(position)
    if low == high:
        return ordered[low]
    fraction = position - low
    return ordered[low] * (1.0 - fraction) + ordered[high] * fraction


def block_balanced_conflict_metrics(source_conflicts: dict) -> dict:
    per_block = list(source_conflicts.get("per_block") or [])
    conflict_blocks = [item for item in per_block if int(item.get("total_conflict_pairs", 0)) > 0]
    coverages = [float(item.get("coverage", 0.0)) for item in conflict_blocks]
    thresholds = [0.50, 0.75, 0.90, 0.95]
    total_pairs = sum(int(item.get("total_conflict_pairs", 0)) for item in conflict_blocks)
    ranked = sorted(
        conflict_blocks,
        key=lambda item: (-int(item.get("total_conflict_pairs", 0)), int(item.get("block_number", 0))),
    )
    concentration = {}
    for n in (1, 5, 10):
        pairs = sum(int(item.get("total_conflict_pairs", 0)) for item in ranked[:n])
        concentration[f"top_{n}_blocks"] = {
            "conflict_pairs": pairs,
            "share": (pairs / total_pairs) if total_pairs else 0.0,
        }
    return {
        "conflict_bearing_blocks": len(conflict_blocks),
        "zero_conflict_blocks": len(per_block) - len(conflict_blocks),
        "median_coverage": percentile(coverages, 0.50),
        "p10_coverage": percentile(coverages, 0.10),
        "p25_coverage": percentile(coverages, 0.25),
        "minimum_coverage": min(coverages) if coverages else None,
        "blocks_meeting_threshold": {
            f"{int(threshold * 100)}pct": sum(value + 1e-12 >= threshold for value in coverages)
            for threshold in thresholds
        },
        "threshold_denominator": len(conflict_blocks),
        "source_conflict_concentration": concentration,
    }


def mapped_storage_access_coverage(blocks: list[dict], resolver: FamilyResolver) -> dict:
    total = mapped = 0
    mapped_txs = 0
    total_txs = 0
    for block in blocks:
        for tx in block.get("transactions") or []:
            total_txs += 1
            tx_mapped = False
            for key in list(tx.get("reads") or []) + list(tx.get("writes") or []):
                total += 1
                owner = storage_contract(key)
                if owner is None:
                    continue
                _, native = resolver.native_family_for_storage_context("0x" + owner)
                if native is not None:
                    mapped += 1
                    tx_mapped = True
            if tx_mapped:
                mapped_txs += 1
    return {
        "total_access_records": total,
        "selected_family_access_records": mapped,
        "access_record_coverage": (mapped / total) if total else 1.0,
        "transactions_touching_selected_family_storage": mapped_txs,
        "transaction_coverage": (mapped_txs / total_txs) if total_txs else 1.0,
    }


def build_instance_catalog(
    frozen_map: dict,
    resolver: FamilyResolver,
    observed_actions: list[dict],
) -> dict:
    observed = {
        action.get("native_instance_id")
        for action in observed_actions
        if action.get("native_instance_id")
    }
    entries: dict[str, dict] = {}
    for owner, resolution in resolver.resolution_by_owner.items():
        profile = str(resolution.get("recommended_profile_family") or "")
        native = resolver.profile_to_native.get(profile)
        if native is None:
            continue
        instance = f"{native}:{owner}"
        entries[instance] = {
            "native_instance_id": instance,
            "native_code_family": native,
            "ethereum_profile_family": profile,
            "source_storage_owner": owner,
            "profile_resolution_status": resolution.get("resolution_status"),
            "observed_in_call_plan": instance in observed,
        }
    # A selected direct-code family can be invoked but not own a conflict in resolution_records.
    for action in observed_actions:
        instance = action.get("native_instance_id")
        native = action.get("native_code_family")
        profile = action.get("ethereum_profile_family")
        owner = action.get("storage_context_address")
        if not instance or not native or not owner:
            continue
        entries.setdefault(instance, {
            "native_instance_id": instance,
            "native_code_family": native,
            "ethereum_profile_family": profile,
            "source_storage_owner": owner,
            "profile_resolution_status": "direct-or-call-only",
            "observed_in_call_plan": True,
        })
    ranked = sorted(entries.values(), key=lambda item: (item["native_code_family"], item["source_storage_owner"]))
    counts = Counter(item["native_code_family"] for item in ranked)
    return {
        "schema_version": 1,
        "namespace_rule": "one native instance per distinct Ethereum storage-owner address; code and symbolic profile may be shared",
        "instances": ranked,
        "native_code_family_counts": dict(sorted(counts.items())),
        "total_instances": len(ranked),
    }


def implementation_readiness(frozen_map: dict, root: Path) -> dict:
    families = []
    for family, config in sorted((frozen_map.get("native_code_families") or {}).items()):
        source = root / str(config["native_contract_source"])
        symbolic = root / str(config["symbolic_analysis"])
        families.append({
            "native_code_family": family,
            "native_contract_source": str(config["native_contract_source"]),
            "native_contract_source_present": source.exists(),
            "symbolic_analysis": str(config["symbolic_analysis"]),
            "symbolic_analysis_present": symbolic.exists(),
        })
    return {
        "families": families,
        "missing_native_contract_sources": [item["native_code_family"] for item in families if not item["native_contract_source_present"]],
        "missing_symbolic_analyses": [item["native_code_family"] for item in families if not item["symbolic_analysis_present"]],
        "native_execution_ready": all(item["native_contract_source_present"] and item["symbolic_analysis_present"] for item in families),
    }


def render_coverage_text(report: dict) -> str:
    calls = report["calls"]
    source = report["source_conflict_coverage"]
    balanced = report["block_balanced_conflict_coverage"]
    storage = report["storage_access_coverage"]
    ready = report["implementation_readiness"]
    txc = report["transaction_semantic_coverage"]
    system = calls["system_actions"]
    threshold_counts = balanced["blocks_meeting_threshold"]
    denominator = balanced["threshold_denominator"]
    lines = [
        "Vegeta S3 native translation pre-execution coverage",
        "",
        f"blocks retained: {report['blocks_retained']} / {report['source_blocks']}",
        f"transactions retained: {report['transactions_retained']} / {report['source_transactions']} ({report['transaction_retention'] * 100:.2f}%)",
        f"call frames: {calls['total_frames']} total; {calls['mapped_native_frames']} mapped-owner native; {calls['semantic_frames']} reviewed semantic; {calls['mapped_system_frames']} system; {calls['inlined_delegatecall_frames']} inlined delegatecall; {calls['background_fallback_frames']} background fallback",
        f"system actions: bank-transfer={system['plain_value_transfer']} precompile={system['ethereum_precompile']} empty-code-noop={system['empty_code_noop']}",
        f"reviewed semantic transactions: {txc['transactions_with_semantic_action']} / {report['source_transactions']} ({txc['semantic_transaction_coverage'] * 100:.2f}%)",
        f"  fully semantic: {txc['fully_semantic_transactions']}",
        f"  mixed semantic+fallback: {txc['mixed_semantic_fallback_transactions']}",
        f"  background only: {txc['background_only_transactions']}",
        "",
        "Source-trace coverage of the frozen 11-family mapping:",
        f"  conflict pairs: {source['selected_family_unique_conflict_pairs']} / {source['total_unique_conflict_pairs']} ({source['coverage'] * 100:.2f}%)",
        f"  storage access records: {storage['selected_family_access_records']} / {storage['total_access_records']} ({storage['access_record_coverage'] * 100:.2f}%)",
        f"  transactions touching selected-family storage: {storage['transactions_touching_selected_family_storage']} ({storage['transaction_coverage'] * 100:.2f}%)",
        "",
        "Block-balanced source-conflict coverage (conflict-bearing blocks only):",
        f"  blocks: {balanced['conflict_bearing_blocks']}",
        f"  median: {balanced['median_coverage'] * 100:.2f}%" if balanced['median_coverage'] is not None else "  median: n/a",
        f"  p10: {balanced['p10_coverage'] * 100:.2f}%" if balanced['p10_coverage'] is not None else "  p10: n/a",
        f"  >=50%: {threshold_counts['50pct']} / {denominator}",
        f"  >=75%: {threshold_counts['75pct']} / {denominator}",
        f"  >=90%: {threshold_counts['90pct']} / {denominator}",
        f"  >=95%: {threshold_counts['95pct']} / {denominator}",
        f"  conflict share in hottest block: {balanced['source_conflict_concentration']['top_1_blocks']['share'] * 100:.2f}%",
        "",
        f"native code families: {report['native_code_families']}",
        f"native instance namespaces: {report['native_instances']}",
        f"missing native contract sources: {', '.join(ready['missing_native_contract_sources']) or '-'}",
        f"missing genuine symbolic analyses: {', '.join(ready['missing_symbolic_analyses']) or '-'}",
        f"native execution ready: {'yes' if ready['native_execution_ready'] else 'no'}",
        "",
        "Important: source-family conflict coverage is not native conflict-topology fidelity.",
        "Native precision/recall/critical-chain fidelity remain unmeasured until real native execution.",
    ]
    return "\n".join(lines) + "\n"

def build_plan(
    blocks: list[dict],
    call_cache: dict[int, dict],
    frozen_map: dict,
    resolver: FamilyResolver,
    root: Path = ROOT,
) -> tuple[list[dict], dict, dict]:
    plan_blocks: list[dict] = []
    all_actions: list[dict] = []
    fully_semantic_transactions = 0
    mixed_transactions = 0
    background_only_transactions = 0
    total_frames = mapped_frames = system_frames = inline_frames = fallback_frames = opaque_frames = reviewed_semantic_frames = 0
    system_action_counts = Counter()

    for block in blocks:
        number = int(block["block_number"])
        traced = call_cache[number]["transactions"]
        plan_txs = []
        for tx, traced_tx in zip(block.get("transactions") or [], traced):
            actions = translate_call_tree(traced_tx["result"], resolver)
            all_actions.extend(actions)
            total_frames += len(actions)
            semantic_here = any(
                action["dispatch"] in {"mapped-entrypoint", "reviewed-stateless-entrypoint", "system-bank-transfer", "system-precompile", "system-noop-call", "inlined-helper-or-implementation"}
                and not action.get("failed_frame")
                for action in actions
            )
            fallback_here = any(
                action["translation_status"] == "background-fallback" or action["dispatch"] == "mapped-opaque-selector"
                for action in actions
            )
            if semantic_here and not fallback_here:
                semantic_class = "fully-semantic"
                fully_semantic_transactions += 1
            elif semantic_here:
                semantic_class = "mixed-semantic-fallback"
                mixed_transactions += 1
            else:
                semantic_class = "background-only"
                background_only_transactions += 1
            mapped_frames += sum(action["translation_status"] == "mapped-native-call" for action in actions)
            system_frames += sum(action["translation_status"] == SYSTEM_TRANSLATION_STATUS for action in actions)
            inline_frames += sum(action["translation_status"] == "inlined-delegatecall" for action in actions)
            fallback_frames += sum(action["translation_status"] == "background-fallback" for action in actions)
            opaque_frames += sum(action["dispatch"] == "mapped-opaque-selector" for action in actions)
            reviewed_semantic_frames += sum(
                action["dispatch"] in {"mapped-entrypoint", "reviewed-stateless-entrypoint", "system-bank-transfer", "system-precompile", "system-noop-call", "inlined-helper-or-implementation"}
                and not action.get("failed_frame")
                for action in actions
            )
            system_action_counts.update(
                action.get("system_action_kind") for action in actions if action.get("system_action_kind")
            )
            plan_txs.append({
                "tx_index": int(tx["tx_index"]),
                "tx_hash": str(tx["tx_hash"]).lower(),
                "from": str(tx.get("from") or "0x").lower(),
                "to": str(tx.get("to") or "<create>").lower(),
                "selector": str(tx.get("selector") or "0x").lower(),
                "value": str(tx.get("value") or "0x0").lower(),
                "gas_used_compute_proxy": int(tx.get("gas_used", tx.get("opcode_steps", 0)) or 0),
                "source_failed": bool(tx.get("failed")),
                "translation_class": semantic_class,
                "native_actions": actions,
            })
        plan_blocks.append({
            "schema_version": 2,
            "block_number": number,
            "block_hash": str(block.get("block_hash") or "").lower(),
            "timestamp": int(block.get("timestamp", 0) or 0),
            "transactions": plan_txs,
        })

    source_conflicts = source_conflict_coverage(blocks, resolver)
    balanced_conflicts = block_balanced_conflict_metrics(source_conflicts)
    storage_coverage = mapped_storage_access_coverage(blocks, resolver)
    catalog = build_instance_catalog(frozen_map, resolver, all_actions)
    readiness = implementation_readiness(frozen_map, root)
    source_transactions = sum(len(block.get("transactions") or []) for block in blocks)
    semantic_transactions = fully_semantic_transactions + mixed_transactions
    semantic_frames = reviewed_semantic_frames
    coverage = {
        "schema_version": 2,
        "dataset": frozen_map.get("dataset", "vegeta-s3"),
        "source_blocks": len(blocks),
        "blocks_retained": len(plan_blocks),
        "source_transactions": source_transactions,
        "transactions_retained": sum(len(block["transactions"]) for block in plan_blocks),
        "transaction_retention": 1.0 if source_transactions else 1.0,
        "calls": {
            "total_frames": total_frames,
            "mapped_native_frames": mapped_frames,
            "mapped_system_frames": system_frames,
            "semantic_frames": semantic_frames,
            "semantic_frame_coverage": (semantic_frames / total_frames) if total_frames else 1.0,
            "inlined_delegatecall_frames": inline_frames,
            "background_fallback_frames": fallback_frames,
            "mapped_opaque_selector_frames": opaque_frames,
            "system_actions": {
                "plain_value_transfer": int(system_action_counts["plain-value-transfer"]),
                "ethereum_precompile": int(system_action_counts["ethereum-precompile"]),
                "empty_code_noop": int(system_action_counts["empty-code-noop"]),
            },
        },
        "transaction_semantic_coverage": {
            "fully_semantic_transactions": fully_semantic_transactions,
            "mixed_semantic_fallback_transactions": mixed_transactions,
            "background_only_transactions": background_only_transactions,
            "transactions_with_semantic_action": semantic_transactions,
            "semantic_transaction_coverage": (semantic_transactions / source_transactions) if source_transactions else 1.0,
        },
        "source_conflict_coverage": source_conflicts,
        "block_balanced_conflict_coverage": balanced_conflicts,
        "storage_access_coverage": storage_coverage,
        "native_code_families": len(frozen_map.get("native_code_families") or {}),
        "native_instances": catalog["total_instances"],
        "implementation_readiness": readiness,
        "native_topology_fidelity": {
            "status": "not-measured-preexecution",
            "conflict_pair_precision": None,
            "conflict_pair_recall": None,
            "critical_chain_ratio_error": None,
            "note": "measure only after real native contract execution; source family coverage must not be reported as native topology fidelity",
        },
        "prediction_leakage_guard": "native-plan.jsonl contains public call/calldata metadata and decoded semantic arguments, but no historical concrete reads/writes",
    }
    return plan_blocks, coverage, catalog

def validate_frozen_map(frozen_map: dict) -> None:
    mappings = frozen_map.get("profile_mappings") or []
    families = frozen_map.get("native_code_families") or {}
    expected_mappings = int(frozen_map.get("expected_profile_mappings", 11))
    expected_families = int(frozen_map.get("expected_native_code_families", 7))
    if len(mappings) != expected_mappings:
        raise ValueError(
            f"family map must contain exactly {expected_mappings} reviewed profile mappings, "
            f"got {len(mappings)}"
        )
    if len(families) != expected_families:
        raise ValueError(
            f"family map must contain exactly {expected_families} native code families, "
            f"got {len(families)}"
        )
    profiles = [item.get("ethereum_profile_family") for item in mappings]
    if len(set(profiles)) != len(profiles):
        raise ValueError("frozen map contains duplicate Ethereum profile families")
    for item in mappings:
        if item.get("native_code_family") not in families:
            raise ValueError(f"mapping rank {item.get('rank')} references unknown native family {item.get('native_code_family')}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--corpus", type=Path, default=DEFAULT_CORPUS)
    parser.add_argument("--characterization-dir", type=Path, default=DEFAULT_CHARACTERIZATION)
    parser.add_argument("--family-map", type=Path, default=DEFAULT_MAP)
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUTPUT)
    args = parser.parse_args()

    blocks = load_blocks(args.corpus)
    frozen_map = read_json(args.family_map)
    validate_frozen_map(frozen_map)
    mapping_path = args.characterization_dir / "native-family-mapping-candidates.json"
    code_cache_path = args.characterization_dir / "code-cache.json"
    if not mapping_path.exists():
        raise FileNotFoundError(f"missing proxy-resolved mapping candidates: {mapping_path}")
    if not code_cache_path.exists():
        raise FileNotFoundError(f"missing relevant-address code cache: {code_cache_path}")
    mapping_candidates = read_json(mapping_path)
    code_cache = load_code_cache(code_cache_path)
    call_cache = load_call_cache(blocks, args.characterization_dir / "call-cache")
    resolver = FamilyResolver(frozen_map, code_cache, mapping_candidates)

    plan_blocks, coverage, catalog = build_plan(blocks, call_cache, frozen_map, resolver)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    plan_path = args.output_dir / "native-plan.jsonl"
    write_jsonl(plan_path, plan_blocks)
    write_json_atomic(args.output_dir / "translation-coverage.json", coverage)
    (args.output_dir / "translation-coverage.txt").write_text(
        render_coverage_text(coverage), encoding="utf-8"
    )
    write_json_atomic(args.output_dir / "native-instance-catalog.json", catalog)
    write_json_atomic(args.output_dir / "manifest.json", {
        "schema_version": 2,
        "dataset": frozen_map.get("dataset", "vegeta-s3"),
        "source_corpus": str(args.corpus),
        "family_map": str(args.family_map),
        "family_map_sha256": hashlib.sha256(args.family_map.read_bytes()).hexdigest(),
        "call_trace_semantics": "geth-callTracer-v1",
        "ethereum_caller_provenance": "geth-callTracer.from (frame initiator; compatibility field, not DELEGATECALL msg.sender)",
        "ethereum_msg_sender_provenance": "derived from geth callTracer tree; DELEGATECALL inherits parent execution-scope msg.sender per EIP-7",
        "plan": "native-plan.jsonl",
        "translation_coverage": "translation-coverage.json",
        "instance_catalog": "native-instance-catalog.json",
        "execution_gate": "requires all reviewed native source slots and genuine symbolic-analysis files",
        "oracle_accesses_embedded_in_plan": False,
    })
    print(render_coverage_text(coverage), end="")
    print(f"wrote {plan_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
