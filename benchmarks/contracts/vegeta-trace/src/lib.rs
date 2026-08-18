//! Trace-replay CosmWasm contract for the Vegeta Ethereum workload port.
//!
//! The contract intentionally separates *prediction hints* from the concrete EVM storage keys
//! that are replayed. `predicted_reads`/`predicted_writes` are consumed only by the benchmark
//! profile graph; execution uses `actual_reads`/`actual_writes`. The full JSON execution payload is
//! decoded by the generic adapter, but the candidate profile has no predicate that references the
//! concrete fields. This lets the common harness test imperfect pre-consensus dependency prediction
//! without using the exact access set for graph construction.

use cosmwasm_std::{
    entry_point, to_json_binary, Binary, Deps, DepsMut, Env, MessageInfo, Response, StdResult,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, Default)]
pub struct InstantiateMsg {}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecuteMsg {
    Replay {
        /// History-derived dependency hints. These are deliberately ignored by execution.
        #[serde(default)]
        predicted_reads: Vec<String>,
        #[serde(default)]
        predicted_writes: Vec<String>,
        /// Concrete storage accesses extracted from the historical Ethereum execution.
        actual_reads: Vec<String>,
        actual_writes: Vec<String>,
        /// Deterministic CPU work used only to retain a coarse per-transaction cost distribution.
        #[serde(default)]
        work_iterations: u64,
        tx_hash: String,
        target: String,
        selector: String,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryMsg {
    Value { key: String },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct ValueResponse {
    pub value: Option<Binary>,
}

#[entry_point]
pub fn instantiate(
    _deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    _msg: InstantiateMsg,
) -> StdResult<Response> {
    Ok(Response::new().add_attribute("action", "instantiate"))
}

#[entry_point]
pub fn execute(
    deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    msg: ExecuteMsg,
) -> StdResult<Response> {
    match msg {
        ExecuteMsg::Replay {
            predicted_reads: _,
            predicted_writes: _,
            actual_reads,
            actual_writes,
            work_iterations,
            tx_hash,
            target,
            selector,
        } => replay(
            deps,
            actual_reads,
            actual_writes,
            work_iterations,
            &tx_hash,
            &target,
            &selector,
        ),
    }
}

fn replay(
    deps: DepsMut,
    actual_reads: Vec<String>,
    actual_writes: Vec<String>,
    work_iterations: u64,
    tx_hash: &str,
    target: &str,
    selector: &str,
) -> StdResult<Response> {
    let mut checksum = stable_seed(tx_hash.as_bytes());
    checksum ^= stable_seed(target.as_bytes()).rotate_left(7);
    checksum ^= stable_seed(selector.as_bytes()).rotate_left(19);

    for key in &actual_reads {
        checksum = mix(checksum, stable_seed(key.as_bytes()));
        if let Some(value) = deps.storage.get(key.as_bytes()) {
            checksum = mix(checksum, stable_seed(&value));
        }
    }

    for round in 0..work_iterations {
        checksum = mix(checksum, round ^ 0x9E37_79B9_7F4A_7C15);
    }

    for key in &actual_writes {
        checksum = mix(checksum, stable_seed(key.as_bytes()));
        let mut value = Vec::with_capacity(24);
        value.extend_from_slice(&checksum.to_be_bytes());
        value.extend_from_slice(&stable_seed(tx_hash.as_bytes()).to_be_bytes());
        value.extend_from_slice(&stable_seed(key.as_bytes()).to_be_bytes());
        deps.storage.set(key.as_bytes(), &value);
    }

    Ok(Response::new()
        .add_attribute("action", "replay")
        .add_attribute("checksum", checksum.to_string()))
}

#[entry_point]
pub fn query(deps: Deps, _env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Value { key } => to_json_binary(&ValueResponse {
            value: deps.storage.get(key.as_bytes()).map(Binary::from),
        }),
    }
}

fn stable_seed(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xCBF2_9CE4_8422_2325_u64, |state, byte| {
        state.wrapping_mul(0x100_0000_01B3) ^ u64::from(*byte)
    })
}

fn mix(mut state: u64, value: u64) -> u64 {
    state ^= value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    state ^= state >> 30;
    state = state.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    state ^= state >> 27;
    state = state.wrapping_mul(0x94D0_49BB_1331_11EB);
    state ^ (state >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmwasm_std::testing::{mock_dependencies, mock_env, mock_info};
    use cosmwasm_std::{from_json, Storage};

    #[test]
    fn predictions_do_not_change_concrete_storage_accesses() {
        let mut deps = mock_dependencies();
        instantiate(
            deps.as_mut(),
            mock_env(),
            mock_info("creator", &[]),
            InstantiateMsg::default(),
        )
        .unwrap();

        execute(
            deps.as_mut(),
            mock_env(),
            mock_info("client", &[]),
            ExecuteMsg::Replay {
                predicted_reads: vec!["fake/read".to_owned()],
                predicted_writes: vec!["fake/write".to_owned()],
                actual_reads: vec!["evm/a/01".to_owned()],
                actual_writes: vec!["evm/a/02".to_owned()],
                work_iterations: 3,
                tx_hash: "0x01".to_owned(),
                target: "0xa".to_owned(),
                selector: "0xdeadbeef".to_owned(),
            },
        )
        .unwrap();

        assert!(deps.storage.get(b"fake/write").is_none());
        assert!(deps.storage.get(b"evm/a/02").is_some());
        let response = query(
            deps.as_ref(),
            mock_env(),
            QueryMsg::Value {
                key: "evm/a/02".to_owned(),
            },
        )
        .unwrap();
        let value: ValueResponse = from_json(response).unwrap();
        assert!(value.value.is_some());
    }
}
