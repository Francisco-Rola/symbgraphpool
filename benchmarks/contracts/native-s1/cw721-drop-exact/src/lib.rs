//! S1-only exact-token CW721 drop/mint analogue for the Vegeta Ethereum conversion.
//! It preserves ownership, approval, sequential-supply, per-wallet, stage, and nonce dependencies
//! exercised by the frozen S1 selector set; proof/signature cryptography itself is intentionally
//! outside the scheduler benchmark's semantic scope.

use cosmwasm_std::{entry_point, to_json_binary, Addr, Binary, Deps, DepsMut, Env, MessageInfo, Response, StdError, StdResult};
use cw_storage_plus::{Item, Map};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct InstantiateMsg { pub admin: String, pub name: String, pub symbol: String, #[serde(default)] pub next_token_id: u64 }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecuteMsg {
    SeedMint { owner: String, token_id: u64 },
    MintDrop {
        recipient: String,
        quantity: u32,
        #[serde(default)]
        token_ids: Option<Vec<u64>>,
        stage_key: Option<String>,
        nonce_key: Option<String>,
    },
    TransferNft { recipient: String, token_id: u64 },
    ApproveNft { spender: String, token_id: u64 },
    ApproveAll { operator: String, approved: bool },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryMsg {
    OwnerOf { token_id: u64 },
    Approved { token_id: u64 },
    Balance { owner: String },
    MintedByWallet { minter: String },
    TotalSupply {},
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct Config { pub admin: Addr, pub name: String, pub symbol: String, pub next_token_id: u64, pub total_supply: u64 }
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct OwnerResponse { pub owner: String }
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct CountResponse { pub count: u64 }

const CONFIG: Item<Config> = Item::new("config");
const OWNER: Map<u64, Addr> = Map::new("owner");
const APPROVAL: Map<u64, Addr> = Map::new("approval");
const OPERATORS: Map<(&str, &str), bool> = Map::new("operators");
const OWNER_COUNT: Map<&str, u64> = Map::new("owner_count");
const MINTED_BY_WALLET: Map<&str, u64> = Map::new("minted_by_wallet");
const MINTED_BY_STAGE: Map<(&str, &str), u64> = Map::new("minted_by_stage");
const USED_NONCES: Map<(&str, &str), bool> = Map::new("used_nonces");

#[derive(Error, Debug, PartialEq)]
pub enum ContractError {
    #[error("{0}")] Std(#[from] StdError),
    #[error("unauthorized")] Unauthorized,
    #[error("token not found")] NotFound,
    #[error("invalid quantity")] InvalidQuantity,
    #[error("nonce already used")] NonceUsed,
}

#[entry_point]
pub fn instantiate(deps: DepsMut, _env: Env, _info: MessageInfo, msg: InstantiateMsg) -> Result<Response, ContractError> {
    CONFIG.save(deps.storage, &Config { admin: deps.api.addr_validate(&msg.admin)?, name: msg.name, symbol: msg.symbol, next_token_id: msg.next_token_id, total_supply: 0 })?;
    Ok(Response::new())
}

#[entry_point]
pub fn execute(deps: DepsMut, _env: Env, info: MessageInfo, msg: ExecuteMsg) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::SeedMint { owner, token_id } => seed_mint(deps, info.sender, owner, token_id),
        ExecuteMsg::MintDrop { recipient, quantity, token_ids, stage_key, nonce_key } => {
            mint_drop(deps, info.sender, recipient, quantity, token_ids, stage_key, nonce_key)
        },
        ExecuteMsg::TransferNft { recipient, token_id } => transfer(deps, info.sender, recipient, token_id),
        ExecuteMsg::ApproveNft { spender, token_id } => approve(deps, info.sender, spender, token_id),
        ExecuteMsg::ApproveAll { operator, approved } => {
            let operator = deps.api.addr_validate(&operator)?;
            OPERATORS.save(deps.storage, (info.sender.as_str(), operator.as_str()), &approved)?;
            Ok(Response::new())
        }
    }
}

fn seed_mint(deps: DepsMut, sender: Addr, owner: String, token_id: u64) -> Result<Response, ContractError> {
    let mut config = CONFIG.load(deps.storage)?;
    if config.admin != sender { return Err(ContractError::Unauthorized); }
    let owner = deps.api.addr_validate(&owner)?;
    OWNER.save(deps.storage, token_id, &owner)?;
    OWNER_COUNT.update(deps.storage, owner.as_str(), |value| -> StdResult<_> { Ok(value.unwrap_or_default() + 1) })?;
    config.total_supply = config.total_supply.saturating_add(1);
    config.next_token_id = config.next_token_id.max(token_id.saturating_add(1));
    CONFIG.save(deps.storage, &config)?;
    Ok(Response::new())
}

fn mint_drop(
    deps: DepsMut,
    minter: Addr,
    recipient: String,
    quantity: u32,
    token_ids: Option<Vec<u64>>,
    stage_key: Option<String>,
    nonce_key: Option<String>,
) -> Result<Response, ContractError> {
    if quantity == 0 || quantity > 10_000 { return Err(ContractError::InvalidQuantity); }
    if let Some(ids) = token_ids.as_ref() {
        if ids.len() != quantity as usize {
            return Err(ContractError::InvalidQuantity);
        }
    }
    let recipient = deps.api.addr_validate(&recipient)?;
    if let Some(nonce) = nonce_key.as_deref() {
        if USED_NONCES.may_load(deps.storage, (minter.as_str(), nonce))?.unwrap_or(false) { return Err(ContractError::NonceUsed); }
        USED_NONCES.save(deps.storage, (minter.as_str(), nonce), &true)?;
    }
    let mut config = CONFIG.load(deps.storage)?;
    if let Some(token_ids) = token_ids {
        for token_id in token_ids {
            OWNER.save(deps.storage, token_id, &recipient)?;
            config.next_token_id = config.next_token_id.max(token_id.saturating_add(1));
            config.total_supply = config.total_supply.saturating_add(1);
        }
    } else {
        for _ in 0..quantity {
            let token_id = config.next_token_id;
            OWNER.save(deps.storage, token_id, &recipient)?;
            config.next_token_id = config.next_token_id.saturating_add(1);
            config.total_supply = config.total_supply.saturating_add(1);
        }
    }
    CONFIG.save(deps.storage, &config)?;
    OWNER_COUNT.update(deps.storage, recipient.as_str(), |value| -> StdResult<_> { Ok(value.unwrap_or_default() + u64::from(quantity)) })?;
    MINTED_BY_WALLET.update(deps.storage, minter.as_str(), |value| -> StdResult<_> { Ok(value.unwrap_or_default() + u64::from(quantity)) })?;
    if let Some(stage) = stage_key.as_deref() {
        MINTED_BY_STAGE.update(deps.storage, (minter.as_str(), stage), |value| -> StdResult<_> { Ok(value.unwrap_or_default() + u64::from(quantity)) })?;
    }
    Ok(Response::new())
}

fn authorized(deps: Deps, sender: &Addr, token_id: u64) -> Result<Addr, ContractError> {
    let owner = OWNER.may_load(deps.storage, token_id)?.ok_or(ContractError::NotFound)?;
    if owner == *sender { return Ok(owner); }
    if APPROVAL.may_load(deps.storage, token_id)?.as_ref() == Some(sender) { return Ok(owner); }
    if OPERATORS.may_load(deps.storage, (owner.as_str(), sender.as_str()))?.unwrap_or(false) { return Ok(owner); }
    Err(ContractError::Unauthorized)
}

fn transfer(deps: DepsMut, sender: Addr, recipient: String, token_id: u64) -> Result<Response, ContractError> {
    let old_owner = authorized(deps.as_ref(), &sender, token_id)?;
    let recipient = deps.api.addr_validate(&recipient)?;
    OWNER.save(deps.storage, token_id, &recipient)?;
    APPROVAL.remove(deps.storage, token_id);
    OWNER_COUNT.update(deps.storage, old_owner.as_str(), |value| -> StdResult<_> { Ok(value.unwrap_or_default().saturating_sub(1)) })?;
    OWNER_COUNT.update(deps.storage, recipient.as_str(), |value| -> StdResult<_> { Ok(value.unwrap_or_default() + 1) })?;
    Ok(Response::new())
}

fn approve(deps: DepsMut, sender: Addr, spender: String, token_id: u64) -> Result<Response, ContractError> {
    let owner = OWNER.may_load(deps.storage, token_id)?.ok_or(ContractError::NotFound)?;
    if owner != sender && !OPERATORS.may_load(deps.storage, (owner.as_str(), sender.as_str()))?.unwrap_or(false) { return Err(ContractError::Unauthorized); }
    APPROVAL.save(deps.storage, token_id, &deps.api.addr_validate(&spender)?)?;
    Ok(Response::new())
}

#[entry_point]
pub fn query(deps: Deps, _env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::OwnerOf { token_id } => to_json_binary(&OwnerResponse { owner: OWNER.load(deps.storage, token_id)?.into_string() }),
        QueryMsg::Approved { token_id } => to_json_binary(&APPROVAL.may_load(deps.storage, token_id)?.map(Addr::into_string)),
        QueryMsg::Balance { owner } => to_json_binary(&CountResponse { count: OWNER_COUNT.may_load(deps.storage, &owner)?.unwrap_or_default() }),
        QueryMsg::MintedByWallet { minter } => to_json_binary(&CountResponse { count: MINTED_BY_WALLET.may_load(deps.storage, &minter)?.unwrap_or_default() }),
        QueryMsg::TotalSupply {} => to_json_binary(&CountResponse { count: CONFIG.load(deps.storage)?.total_supply }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmwasm_std::testing::{mock_dependencies, mock_env, mock_info};

    #[test]
    fn public_mint_tracks_supply_wallet_and_owner() {
        let mut deps = mock_dependencies();
        instantiate(deps.as_mut(), mock_env(), mock_info("x", &[]), InstantiateMsg { admin: "admin".into(), name: "Drop".into(), symbol: "DROP".into(), next_token_id: 10 }).unwrap();
        execute(deps.as_mut(), mock_env(), mock_info("alice", &[]), ExecuteMsg::MintDrop { recipient: "alice".into(), quantity: 2, token_ids: None, stage_key: Some("public".into()), nonce_key: None }).unwrap();
        assert_eq!(OWNER.load(deps.as_ref().storage, 10).unwrap(), Addr::unchecked("alice"));
        assert_eq!(CONFIG.load(deps.as_ref().storage).unwrap().total_supply, 2);
        assert_eq!(MINTED_BY_WALLET.load(deps.as_ref().storage, "alice").unwrap(), 2);
    }

    #[test]
    fn event_backed_mint_uses_exact_ids_without_changing_drop_counters() {
        let mut deps = mock_dependencies();
        instantiate(deps.as_mut(), mock_env(), mock_info("x", &[]), InstantiateMsg { admin: "admin".into(), name: "Drop".into(), symbol: "DROP".into(), next_token_id: 463 }).unwrap();
        execute(deps.as_mut(), mock_env(), mock_info("alice", &[]), ExecuteMsg::MintDrop { recipient: "alice".into(), quantity: 2, token_ids: Some(vec![484, 485]), stage_key: Some("public".into()), nonce_key: None }).unwrap();
        assert_eq!(OWNER.load(deps.as_ref().storage, 484).unwrap(), Addr::unchecked("alice"));
        assert_eq!(OWNER.load(deps.as_ref().storage, 485).unwrap(), Addr::unchecked("alice"));
        assert!(OWNER.may_load(deps.as_ref().storage, 463).unwrap().is_none());
        let config = CONFIG.load(deps.as_ref().storage).unwrap();
        assert_eq!(config.next_token_id, 486);
        assert_eq!(config.total_supply, 2);
        assert_eq!(MINTED_BY_WALLET.load(deps.as_ref().storage, "alice").unwrap(), 2);
        assert_eq!(MINTED_BY_STAGE.load(deps.as_ref().storage, ("alice", "public")).unwrap(), 2);
    }
}
