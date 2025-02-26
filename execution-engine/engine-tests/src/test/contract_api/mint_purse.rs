use std::collections::HashMap;

use crate::support::test_support::{
    WasmTestBuilder, DEFAULT_BLOCK_TIME, STANDARD_PAYMENT_CONTRACT,
};
use contract_ffi::value::U512;
use engine_core::engine_state::MAX_PAYMENT;

const GENESIS_ADDR: [u8; 32] = [7u8; 32];
const SYSTEM_ADDR: [u8; 32] = [0u8; 32];

#[ignore]
#[test]
fn should_run_mint_purse_contract() {
    WasmTestBuilder::default()
        .run_genesis(GENESIS_ADDR, HashMap::new())
        .exec_with_args(
            GENESIS_ADDR,
            STANDARD_PAYMENT_CONTRACT,
            (U512::from(MAX_PAYMENT),),
            "transfer_to_account_01.wasm",
            (SYSTEM_ADDR,),
            DEFAULT_BLOCK_TIME,
            [1u8; 32],
        )
        .commit()
        .expect_success()
        .exec(
            SYSTEM_ADDR,
            "mint_purse.wasm",
            DEFAULT_BLOCK_TIME,
            [2u8; 32],
        )
        .commit()
        .expect_success();
}

#[ignore]
#[test]
fn should_not_allow_non_system_accounts_to_mint() {
    assert!(WasmTestBuilder::default()
        .run_genesis(GENESIS_ADDR, HashMap::new())
        .exec(
            GENESIS_ADDR,
            "mint_purse.wasm",
            DEFAULT_BLOCK_TIME,
            [3u8; 32]
        )
        .commit()
        .is_error());
}
