use std::collections::HashMap;

use crate::support::test_support::{DeployBuilder, ExecRequestBuilder, WasmTestBuilder};
use contract_ffi::value::account::PublicKey;
use contract_ffi::value::U512;
use engine_core::engine_state::EngineConfig;

const GENESIS_ADDR: [u8; 32] = [12; 32];
const ACCOUNT_1_ADDR: [u8; 32] = [42u8; 32];

#[ignore]
#[test]
fn should_raise_precondition_authorization_failure_invalid_account() {
    let genesis_addr = GENESIS_ADDR;
    let account_1_public_key = PublicKey::new(ACCOUNT_1_ADDR);
    let nonexistent_account_addr = [99u8; 32];
    let payment_purse_amount = 10_000_000;
    let transferred_amount = 1;

    let engine_config = EngineConfig::new().set_use_payment_code(true);

    let exec_request = {
        let deploy = DeployBuilder::new()
            .with_address(genesis_addr)
            .with_deploy_hash([1; 32])
            .with_session_code(
                "transfer_purse_to_account.wasm",
                (account_1_public_key, U512::from(transferred_amount)),
            )
            .with_address(nonexistent_account_addr)
            .with_payment_code("standard_payment.wasm", (U512::from(payment_purse_amount),))
            .with_authorization_keys(&[PublicKey::new(nonexistent_account_addr)])
            .build();

        ExecRequestBuilder::new().push_deploy(deploy).build()
    };

    let transfer_result = WasmTestBuilder::new(engine_config)
        .run_genesis(GENESIS_ADDR, HashMap::default())
        .exec_with_exec_request(exec_request)
        .finish();

    let response = transfer_result
        .builder()
        .get_exec_response(0)
        .expect("there should be a response")
        .clone();

    let precondition_failure = crate::support::test_support::get_precondition_failure(&response);

    assert_eq!(
        precondition_failure.message, "Authorization failure: not authorized.",
        "expected authorization failure"
    );
}

#[ignore]
#[test]
fn should_raise_precondition_authorization_failure_empty_authorized_keys() {
    let genesis_addr = GENESIS_ADDR;

    let engine_config = EngineConfig::new().set_use_payment_code(true);

    let exec_request = {
        let deploy = DeployBuilder::new()
            .with_address(genesis_addr)
            .with_deploy_hash([1; 32])
            // empty authorization keys to force error
            .with_authorization_keys(&[])
            .build();

        ExecRequestBuilder::new().push_deploy(deploy).build()
    };

    let transfer_result = WasmTestBuilder::new(engine_config)
        .run_genesis(GENESIS_ADDR, HashMap::default())
        .exec_with_exec_request(exec_request)
        .finish();

    let response = transfer_result
        .builder()
        .get_exec_response(0)
        .expect("there should be a response")
        .clone();

    let precondition_failure = crate::support::test_support::get_precondition_failure(&response);

    assert_eq!(
        precondition_failure.message, "Authorization failure: not authorized.",
        "expected authorization failure"
    );
}

#[ignore]
#[test]
fn should_raise_precondition_authorization_failure_invalid_authorized_keys() {
    let genesis_addr = GENESIS_ADDR;
    let account_1_public_key = PublicKey::new(ACCOUNT_1_ADDR);
    let nonexistent_account_addr = [99u8; 32];
    let payment_purse_amount = 10_000_000;
    let transferred_amount = 1;

    let engine_config = EngineConfig::new().set_use_payment_code(true);

    let exec_request = {
        let deploy = DeployBuilder::new()
            .with_address(genesis_addr)
            .with_deploy_hash([1; 32])
            .with_session_code(
                "transfer_purse_to_account.wasm",
                (account_1_public_key, U512::from(transferred_amount)),
            )
            .with_payment_code("standard_payment.wasm", (U512::from(payment_purse_amount),))
            // invalid authorization key to force error
            .with_authorization_keys(&[PublicKey::new(nonexistent_account_addr)])
            .build();

        ExecRequestBuilder::new().push_deploy(deploy).build()
    };

    let transfer_result = WasmTestBuilder::new(engine_config)
        .run_genesis(GENESIS_ADDR, HashMap::default())
        .exec_with_exec_request(exec_request)
        .finish();

    let response = transfer_result
        .builder()
        .get_exec_response(0)
        .expect("there should be a response")
        .clone();

    let precondition_failure = crate::support::test_support::get_precondition_failure(&response);

    assert_eq!(
        precondition_failure.message, "Authorization failure: not authorized.",
        "expected authorization failure"
    );
}
