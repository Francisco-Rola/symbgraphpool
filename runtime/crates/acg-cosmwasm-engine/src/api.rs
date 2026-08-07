use cosmwasm_vm::{BackendApi, BackendError, BackendResult, GasInfo};

#[derive(Clone, Default)]
pub(crate) struct EngineApi;

impl BackendApi for EngineApi {
    fn addr_validate(&self, input: &str) -> BackendResult<()> {
        let result = validate_address(input).map_err(BackendError::user_err);
        (result, GasInfo::with_externally_used(input.len() as u64))
    }

    fn addr_canonicalize(&self, human: &str) -> BackendResult<Vec<u8>> {
        let result = validate_address(human)
            .map(|_| human.as_bytes().to_vec())
            .map_err(BackendError::user_err);
        (result, GasInfo::with_externally_used(human.len() as u64))
    }

    fn addr_humanize(&self, canonical: &[u8]) -> BackendResult<String> {
        let result = String::from_utf8(canonical.to_vec())
            .map_err(|error| BackendError::user_err(error.to_string()))
            .and_then(|value| {
                validate_address(&value)
                    .map(|_| value)
                    .map_err(BackendError::user_err)
            });
        (
            result,
            GasInfo::with_externally_used(canonical.len() as u64),
        )
    }
}

pub(crate) fn validate_address(input: &str) -> Result<(), String> {
    if input.is_empty() {
        return Err("address must not be empty".to_owned());
    }
    if input.len() > 128 {
        return Err("address exceeds 128 bytes".to_owned());
    }
    if input.trim() != input {
        return Err("address must not start or end with whitespace".to_owned());
    }
    if input.chars().any(char::is_control) {
        return Err("address must not contain control characters".to_owned());
    }
    Ok(())
}
