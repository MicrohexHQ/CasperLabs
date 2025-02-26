pub mod engine_config;
pub mod error;
pub mod executable_deploy_item;
pub mod execution_effect;
pub mod execution_result;
pub mod genesis;
pub mod op;
pub mod utils;

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use contract_ffi::bytesrepr::ToBytes;
use contract_ffi::contract_api::argsparser::ArgsParser;
use contract_ffi::execution::Phase;
use contract_ffi::key::{Key, HASH_SIZE};
use contract_ffi::uref::URef;
use contract_ffi::uref::{AccessRights, UREF_ADDR_SIZE};
use contract_ffi::value::account::{BlockTime, PublicKey, PurseId};
use contract_ffi::value::{Account, Value, U512};
use engine_shared::gas::Gas;
use engine_shared::motes::Motes;
use engine_shared::newtypes::{Blake2bHash, CorrelationId};
use engine_shared::transform::Transform;
use engine_storage::global_state::{CommitResult, StateProvider, StateReader};
use engine_wasm_prep::wasm_costs::WasmCosts;
use engine_wasm_prep::Preprocessor;

pub use self::engine_config::EngineConfig;
use self::error::{Error, RootNotFound};
use self::execution_result::ExecutionResult;
use self::genesis::{create_genesis_effects, GenesisResult};
use crate::engine_state::executable_deploy_item::ExecutableDeployItem;
use crate::engine_state::genesis::{POS_PAYMENT_PURSE, POS_REWARDS_PURSE};
use crate::engine_state::utils::WasmiBytes;
use crate::execution::{self, Executor, MINT_NAME, POS_NAME};
use crate::tracking_copy::{TrackingCopy, TrackingCopyExt};

// TODO?: MAX_PAYMENT && CONV_RATE values are currently arbitrary w/ real values
// TBD gas * CONV_RATE = motes
pub const MAX_PAYMENT: u64 = 10_000_000;
pub const CONV_RATE: u64 = 10;

pub const SYSTEM_ACCOUNT_ADDR: [u8; 32] = [0u8; 32];

const DEFAULT_SESSION_MOTES: u64 = 1_000_000_000;

#[derive(Debug)]
pub struct EngineState<S> {
    config: EngineConfig,
    state: S,
}

pub enum GetBondedValidatorsError<E> {
    StateError(E),
    PostStateHashNotFound(Blake2bHash),
    ProofOfStakeNotFound(Key),
}

impl<S> EngineState<S>
where
    S: StateProvider,
    S::Error: Into<execution::Error>,
{
    pub fn new(state: S, config: EngineConfig) -> EngineState<S> {
        EngineState { config, state }
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    #[allow(clippy::too_many_arguments)]
    pub fn commit_genesis(
        &self,
        correlation_id: CorrelationId,
        genesis_account_addr: [u8; 32],
        initial_motes: U512,
        mint_code_bytes: &[u8],
        proof_of_stake_code_bytes: &[u8],
        genesis_validators: Vec<(PublicKey, U512)>,
        protocol_version: u64,
    ) -> Result<GenesisResult, Error> {
        let mint_code = WasmiBytes::new(mint_code_bytes, WasmCosts::free())?;
        let pos_code = WasmiBytes::new(proof_of_stake_code_bytes, WasmCosts::free())?;

        let effects = create_genesis_effects(
            genesis_account_addr,
            initial_motes,
            mint_code,
            pos_code,
            genesis_validators,
            protocol_version,
        )?;
        let prestate_hash = self.state.empty_root();
        let commit_result = self
            .state
            .commit(correlation_id, prestate_hash, effects.transforms.to_owned())
            .map_err(Into::into)?;

        let genesis_result = GenesisResult::from_commit_result(commit_result, effects);

        Ok(genesis_result)
    }

    pub fn tracking_copy(
        &self,
        hash: Blake2bHash,
    ) -> Result<Option<TrackingCopy<S::Reader>>, Error> {
        match self.state.checkout(hash).map_err(Into::into)? {
            Some(tc) => Ok(Some(TrackingCopy::new(tc))),
            None => Ok(None),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn run_deploy_item<A, P: Preprocessor<A>, E: Executor<A>>(
        &self,
        session: ExecutableDeployItem,
        payment: ExecutableDeployItem,
        address: Key,
        authorization_keys: BTreeSet<PublicKey>,
        blocktime: BlockTime,
        deploy_hash: [u8; 32],
        prestate_hash: Blake2bHash,
        protocol_version: u64,
        correlation_id: CorrelationId,
        executor: &E,
        preprocessor: &P,
    ) -> Result<ExecutionResult, RootNotFound> {
        self.deploy(
            session,
            payment,
            address,
            authorization_keys,
            blocktime,
            deploy_hash,
            prestate_hash,
            protocol_version,
            correlation_id,
            executor,
            preprocessor,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn run_deploy<A, P: Preprocessor<A>, E: Executor<A>>(
        &self,
        session_module_bytes: &[u8],
        session_args: &[u8],
        payment_module_bytes: &[u8],
        payment_args: &[u8],
        address: Key,
        authorization_keys: BTreeSet<PublicKey>,
        blocktime: BlockTime,
        deploy_hash: [u8; 32],
        prestate_hash: Blake2bHash,
        protocol_version: u64,
        correlation_id: CorrelationId,
        executor: &E,
        preprocessor: &P,
    ) -> Result<ExecutionResult, RootNotFound> {
        let session = ExecutableDeployItem::ModuleBytes {
            module_bytes: session_module_bytes.into(),
            args: session_args.into(),
        };
        let payment = ExecutableDeployItem::ModuleBytes {
            module_bytes: payment_module_bytes.into(),
            args: payment_args.into(),
        };

        self.deploy(
            session,
            payment,
            address,
            authorization_keys,
            blocktime,
            deploy_hash,
            prestate_hash,
            protocol_version,
            correlation_id,
            executor,
            preprocessor,
        )
    }

    pub fn get_module<A, P: Preprocessor<A>>(
        &self,
        tracking_copy: Rc<RefCell<TrackingCopy<<S as StateProvider>::Reader>>>,
        deploy_item: &ExecutableDeployItem,
        account: &Account,
        correlation_id: CorrelationId,
        preprocessor: &P,
    ) -> Result<A, error::Error> {
        match deploy_item {
            ExecutableDeployItem::ModuleBytes { module_bytes, .. } => {
                let module = preprocessor.preprocess(&module_bytes)?;
                Ok(module)
            }
            ExecutableDeployItem::StoredContractByHash { hash, .. } => {
                let stored_contract_key = {
                    let hash_len = hash.len();
                    if hash_len != HASH_SIZE {
                        return Err(error::Error::InvalidHashLength {
                            expected: HASH_SIZE,
                            actual: hash_len,
                        });
                    }
                    let mut arr = [0u8; HASH_SIZE];
                    arr.copy_from_slice(&hash);
                    Key::Hash(arr)
                };
                let contract = tracking_copy
                    .borrow_mut()
                    .get_contract(correlation_id, stored_contract_key)?;
                let (ret, _, _) = contract.destructure();
                let module = preprocessor.deserialize(&ret)?;
                Ok(module)
            }
            ExecutableDeployItem::StoredContractByName { name, .. } => {
                let stored_contract_key = account.urefs_lookup().get(name).ok_or_else(|| {
                    error::Error::ExecError(execution::Error::URefNotFound(name.to_string()))
                })?;
                if let Key::URef(uref) = stored_contract_key {
                    if !uref.is_readable() {
                        return Err(error::Error::ExecError(execution::Error::ForgedReference(
                            *uref,
                        )));
                    }
                }
                let contract = tracking_copy
                    .borrow_mut()
                    .get_contract(correlation_id, stored_contract_key.normalize())?;
                let (ret, _, _) = contract.destructure();
                let module = preprocessor.deserialize(&ret)?;
                Ok(module)
            }
            ExecutableDeployItem::StoredContractByURef { uref, .. } => {
                let stored_contract_key = {
                    let len = uref.len();
                    if len != UREF_ADDR_SIZE {
                        return Err(error::Error::InvalidHashLength {
                            expected: UREF_ADDR_SIZE,
                            actual: len,
                        });
                    }
                    let read_only_uref = {
                        let mut arr = [0u8; UREF_ADDR_SIZE];
                        arr.copy_from_slice(&uref);
                        URef::new(arr, AccessRights::READ)
                    };
                    let normalized_uref = Key::URef(read_only_uref).normalize();
                    let maybe_known_uref = account
                        .urefs_lookup()
                        .values()
                        .find(|&known_uref| known_uref.normalize() == normalized_uref);
                    match maybe_known_uref {
                        Some(Key::URef(known_uref)) if known_uref.is_readable() => normalized_uref,
                        Some(Key::URef(_)) => {
                            return Err(error::Error::ExecError(
                                execution::Error::ForgedReference(read_only_uref),
                            ));
                        }
                        Some(key) => {
                            return Err(error::Error::ExecError(execution::Error::TypeMismatch(
                                engine_shared::transform::TypeMismatch::new(
                                    "Key::URef".to_string(),
                                    key.type_string(),
                                ),
                            )));
                        }
                        None => {
                            return Err(error::Error::ExecError(execution::Error::KeyNotFound(
                                Key::URef(read_only_uref),
                            )));
                        }
                    }
                };
                let contract = tracking_copy
                    .borrow_mut()
                    .get_contract(correlation_id, stored_contract_key)?;
                let (ret, _, _) = contract.destructure();
                let module = preprocessor.deserialize(&ret)?;
                Ok(module)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn deploy<A, P: Preprocessor<A>, E: Executor<A>>(
        &self,
        session: ExecutableDeployItem,
        payment: ExecutableDeployItem,
        address: Key,
        authorization_keys: BTreeSet<PublicKey>,
        blocktime: BlockTime,
        deploy_hash: [u8; 32],
        prestate_hash: Blake2bHash,
        protocol_version: u64,
        correlation_id: CorrelationId,
        executor: &E,
        preprocessor: &P,
    ) -> Result<ExecutionResult, RootNotFound> {
        // spec: https://casperlabs.atlassian.net/wiki/spaces/EN/pages/123404576/Payment+code+execution+specification

        // Create tracking copy (which functions as a deploy context)
        // validation_spec_2: prestate_hash check
        let tracking_copy = match self.tracking_copy(prestate_hash) {
            Err(error) => return Ok(ExecutionResult::precondition_failure(error)),
            Ok(None) => return Err(RootNotFound(prestate_hash)),
            Ok(Some(tracking_copy)) => Rc::new(RefCell::new(tracking_copy)),
        };

        // Get addr bytes from `address` (which is actually a Key)
        // validation_spec_3: account validity
        let account_addr = match address.as_account() {
            Some(account_addr) => account_addr,
            None => {
                return Ok(ExecutionResult::precondition_failure(
                    error::Error::AuthorizationError,
                ))
            }
        };

        // Get account from tracking copy
        // validation_spec_3: account validity
        let account: Account = match tracking_copy
            .borrow_mut()
            .get_account(correlation_id, account_addr)
        {
            Ok(account) => account,
            Err(_) => {
                return Ok(ExecutionResult::precondition_failure(
                    error::Error::AuthorizationError,
                ));
            }
        };

        // Authorize using provided authorization keys
        // validation_spec_3: account validity
        if authorization_keys.is_empty() || !account.can_authorize(&authorization_keys) {
            return Ok(ExecutionResult::precondition_failure(
                crate::engine_state::error::Error::AuthorizationError,
            ));
        }

        // Check total key weight against deploy threshold
        // validation_spec_4: deploy validity
        if !account.can_deploy_with(&authorization_keys) {
            return Ok(ExecutionResult::precondition_failure(
                // TODO?:this doesn't happen in execution any longer, should error variant be moved
                execution::Error::DeploymentAuthorizationFailure.into(),
            ));
        }

        // Create session code `A` from provided session bytes
        // validation_spec_1: valid wasm bytes
        let session_module = match self.get_module(
            Rc::clone(&tracking_copy),
            &session,
            &account,
            correlation_id,
            preprocessor,
        ) {
            Ok(module) => module,
            Err(error) => {
                return Ok(ExecutionResult::precondition_failure(error));
            }
        };

        // --- REMOVE BELOW --- //

        // If payment logic is turned off, execute only session code
        if !(self.config.use_payment_code()) {
            // DEPLOY WITH NO PAYMENT

            let session_motes = Motes::from_u64(DEFAULT_SESSION_MOTES);

            let gas_limit = Gas::from_motes(session_motes, CONV_RATE).unwrap_or_default();

            // Session code execution
            let session_result = executor.exec(
                session_module,
                session.args(),
                address,
                &account,
                authorization_keys,
                blocktime,
                deploy_hash,
                gas_limit,
                protocol_version,
                correlation_id,
                Rc::clone(&tracking_copy),
                Phase::Session,
            );

            return Ok(session_result);
        }

        // --- REMOVE ABOVE --- //

        let max_payment_cost: Motes = Motes::from_u64(MAX_PAYMENT);

        // Get mint system contract details
        // payment_code_spec_6: system contract validity
        let mint_inner_uref = {
            // Get mint system contract URef from account (an account on a different network
            // may have a mint contract other than the CLMint)
            // payment_code_spec_6: system contract validity
            let mint_public_uref: Key = match account.urefs_lookup().get(MINT_NAME) {
                Some(uref) => uref.normalize(),
                None => {
                    return Ok(ExecutionResult::precondition_failure(
                        Error::MissingSystemContractError(MINT_NAME.to_string()),
                    ));
                }
            };

            // FIXME: This is inefficient; we don't need to get the entire contract.
            let mint_info = match tracking_copy
                .borrow_mut()
                .get_system_contract_info(correlation_id, mint_public_uref)
            {
                Ok(contract_info) => contract_info,
                Err(error) => {
                    return Ok(ExecutionResult::precondition_failure(error.into()));
                }
            };

            // Safe to unwrap here, as `get_system_contract_info` checks that the key is
            // the proper variant.
            *mint_info.inner_key().as_uref().unwrap()
        };

        // Get proof of stake system contract URef from account (an account on a
        // different network may have a pos contract other than the CLPoS)
        // payment_code_spec_6: system contract validity
        let proof_of_stake_public_uref: Key = match account.urefs_lookup().get(POS_NAME) {
            Some(uref) => uref.normalize(),
            None => {
                return Ok(ExecutionResult::precondition_failure(
                    Error::MissingSystemContractError(POS_NAME.to_string()),
                ));
            }
        };

        // Get proof of stake system contract details
        // payment_code_spec_6: system contract validity
        let proof_of_stake_info = match tracking_copy
            .borrow_mut()
            .get_system_contract_info(correlation_id, proof_of_stake_public_uref)
        {
            Ok(contract_info) => contract_info,
            Err(error) => {
                return Ok(ExecutionResult::precondition_failure(error.into()));
            }
        };

        // Get rewards purse balance key
        // payment_code_spec_6: system contract validity
        let rewards_purse_balance_key: Key = {
            // Get reward purse Key from proof of stake contract
            // payment_code_spec_6: system contract validity
            let rewards_purse_key: Key = match proof_of_stake_info
                .contract()
                .urefs_lookup()
                .get(POS_REWARDS_PURSE)
            {
                Some(key) => *key,
                None => {
                    return Ok(ExecutionResult::precondition_failure(Error::DeployError));
                }
            };

            match tracking_copy.borrow_mut().get_purse_balance_key(
                correlation_id,
                mint_inner_uref,
                rewards_purse_key,
            ) {
                Ok(key) => key,
                Err(error) => {
                    return Ok(ExecutionResult::precondition_failure(error.into()));
                }
            }
        };

        // Get account main purse balance key
        // validation_spec_5: account main purse minimum balance
        let account_main_purse_balance_key: Key = {
            let account_key = Key::URef(account.purse_id().value());
            match tracking_copy.borrow_mut().get_purse_balance_key(
                correlation_id,
                mint_inner_uref,
                account_key,
            ) {
                Ok(key) => key,
                Err(error) => {
                    return Ok(ExecutionResult::precondition_failure(error.into()));
                }
            }
        };

        // Get account main purse balance to enforce precondition and in case of forced
        // transfer validation_spec_5: account main purse minimum balance
        let account_main_purse_balance: Motes = match tracking_copy
            .borrow_mut()
            .get_purse_balance(correlation_id, account_main_purse_balance_key)
        {
            Ok(balance) => balance,
            Err(error) => return Ok(ExecutionResult::precondition_failure(error.into())),
        };

        // Enforce minimum main purse balance validation
        // validation_spec_5: account main purse minimum balance
        if account_main_purse_balance < max_payment_cost {
            return Ok(ExecutionResult::precondition_failure(
                Error::InsufficientPaymentError,
            ));
        }

        // Finalization is executed by system account (currently genesis account)
        // payment_code_spec_5: system executes finalization
        let system_account = Account::new(
            SYSTEM_ACCOUNT_ADDR,
            Default::default(),
            PurseId::new(URef::new(Default::default(), AccessRights::READ_ADD_WRITE)),
            Default::default(),
            Default::default(),
            Default::default(),
        );

        // `[ExecutionResultBuilder]` handles merging of multiple execution results
        let mut execution_result_builder = execution_result::ExecutionResultBuilder::new();

        // Execute provided payment code
        let payment_result = {
            // payment_code_spec_1: init pay environment w/ gas limit == (max_payment_cost /
            // conv_rate)
            let pay_gas_limit = Gas::from_motes(max_payment_cost, CONV_RATE).unwrap_or_default();

            // Create payment code module from bytes
            // validation_spec_1: valid wasm bytes
            let payment_module = match self.get_module(
                Rc::clone(&tracking_copy),
                &payment,
                &account,
                correlation_id,
                preprocessor,
            ) {
                Ok(module) => module,
                Err(error) => {
                    return Ok(ExecutionResult::precondition_failure(error));
                }
            };

            // payment_code_spec_2: execute payment code
            executor.exec(
                payment_module,
                payment.args(),
                address,
                &account,
                authorization_keys.clone(),
                blocktime,
                deploy_hash,
                pay_gas_limit,
                protocol_version,
                correlation_id,
                Rc::clone(&tracking_copy),
                Phase::Payment,
            )
        };

        let payment_result_cost = payment_result.cost();

        // payment_code_spec_3: fork based upon payment purse balance and cost of
        // payment code execution
        let payment_purse_balance: Motes = {
            // Get payment purse Key from proof of stake contract
            // payment_code_spec_6: system contract validity
            let payment_purse: Key = match proof_of_stake_info
                .contract()
                .urefs_lookup()
                .get(POS_PAYMENT_PURSE)
            {
                Some(key) => *key,
                None => return Ok(ExecutionResult::precondition_failure(Error::DeployError)),
            };

            let purse_balance_key = match tracking_copy.borrow_mut().get_purse_balance_key(
                correlation_id,
                mint_inner_uref,
                payment_purse,
            ) {
                Ok(key) => key,
                Err(error) => {
                    return Ok(ExecutionResult::precondition_failure(error.into()));
                }
            };

            match tracking_copy
                .borrow_mut()
                .get_purse_balance(correlation_id, purse_balance_key)
            {
                Ok(balance) => balance,
                Err(error) => {
                    return Ok(ExecutionResult::precondition_failure(error.into()));
                }
            }
        };

        if let Some(failure) = execution_result_builder
            .set_payment_execution_result(payment_result)
            .check_forced_transfer(
                max_payment_cost,
                account_main_purse_balance,
                payment_purse_balance,
                account_main_purse_balance_key,
                rewards_purse_balance_key,
            )
        {
            return Ok(failure);
        }

        let post_payment_tc = tracking_copy.borrow();
        let session_tc = Rc::new(RefCell::new(post_payment_tc.fork()));

        // session_code_spec_2: execute session code
        let session_result = {
            // payment_code_spec_3_b_i: if (balance of PoS pay purse) >= (gas spent during
            // payment code execution) * conv_rate, yes session
            // session_code_spec_1: gas limit = ((balance of PoS payment purse) / conv_rate)
            // - (gas spent during payment execution)
            let session_gas_limit: Gas = Gas::from_motes(payment_purse_balance, CONV_RATE)
                .unwrap_or_default()
                - payment_result_cost;

            executor.exec(
                session_module,
                session.args(),
                address,
                &account,
                authorization_keys.clone(),
                blocktime,
                deploy_hash,
                session_gas_limit,
                protocol_version,
                correlation_id,
                Rc::clone(&session_tc),
                Phase::Session,
            )
        };

        let post_session_rc = if session_result.is_failure() {
            // If session code fails we do not include its effects,
            // so we start again from the post-payment state.
            Rc::new(RefCell::new(post_payment_tc.fork()))
        } else {
            session_tc
        };

        let _session_result_cost = session_result.cost();

        // NOTE: session_code_spec_3: (do not include session execution effects in
        // results) is enforced in execution_result_builder.build()
        execution_result_builder.set_session_execution_result(session_result);

        // payment_code_spec_5: run finalize process
        let finalize_result = {
            let post_session_tc = post_session_rc.borrow();
            let finalization_tc = Rc::new(RefCell::new(post_session_tc.fork()));

            // validation_spec_1: valid wasm bytes
            let proof_of_stake_module =
                match preprocessor.deserialize(&proof_of_stake_info.module_bytes()) {
                    Err(error) => return Ok(ExecutionResult::precondition_failure(error.into())),
                    Ok(module) => module,
                };

            let proof_of_stake_args = {
                //((gas spent during payment code execution) + (gas spent during session code execution)) * conv_rate
                let finalize_cost_motes: Motes = Motes::from_gas(execution_result_builder.total_cost(), CONV_RATE).expect("motes overflow");
                let args = ("finalize_payment", finalize_cost_motes.value(), account_addr);
                ArgsParser::parse(&args)
                    .and_then(|args| args.to_bytes())
                    .expect("args should parse")
            };

            // The PoS keys may have changed because of effects during payment and/or
            // session, so we need to look them up again from the tracking copy
            let mut proof_of_stake_keys = finalization_tc
                .borrow_mut()
                .get_system_contract_info(correlation_id, proof_of_stake_public_uref)
                .expect("PoS must be found because we found it earlier")
                .contract()
                .urefs_lookup()
                .clone();

            let base_key = proof_of_stake_info.inner_key();
            let gas_limit = Gas::from_u64(std::u64::MAX);

            executor.exec_direct(
                proof_of_stake_module,
                &proof_of_stake_args,
                &mut proof_of_stake_keys,
                base_key,
                &system_account,
                authorization_keys.clone(),
                blocktime,
                deploy_hash,
                gas_limit,
                protocol_version,
                correlation_id,
                finalization_tc,
                Phase::FinalizePayment,
            )
        };

        execution_result_builder.set_finalize_execution_result(finalize_result);

        // We panic here to indicate that the builder was not used properly.
        let ret = execution_result_builder
            .build(tracking_copy.borrow().reader(), correlation_id)
            .expect("ExecutionResultBuilder not initialized properly");

        // NOTE: payment_code_spec_5_a is enforced in execution_result_builder.build()
        // payment_code_spec_6: return properly combined set of transforms and
        // appropriate error
        Ok(ret)
    }

    pub fn apply_effect(
        &self,
        correlation_id: CorrelationId,
        prestate_hash: Blake2bHash,
        effects: HashMap<Key, Transform>,
    ) -> Result<CommitResult, S::Error> {
        self.state.commit(correlation_id, prestate_hash, effects)
    }

    /// Calculates bonded validators at `root_hash` state.
    pub fn get_bonded_validators(
        &self,
        root_hash: Blake2bHash,
        pos_key: &Key, /* Address of the PoS as currently bonded validators are stored in its
                        * known urefs map. */
        correlation_id: CorrelationId,
    ) -> Result<HashMap<PublicKey, U512>, GetBondedValidatorsError<S::Error>> {
        self.state
            .checkout(root_hash)
            .map_err(GetBondedValidatorsError::StateError)
            .and_then(|maybe_reader| match maybe_reader {
                Some(reader) => match reader.read(correlation_id, &pos_key.normalize()) {
                    Ok(Some(Value::Contract(contract))) => {
                        let bonded_validators = contract
                            .urefs_lookup()
                            .keys()
                            .filter_map(|entry| utils::pos_validator_to_tuple(entry))
                            .collect::<HashMap<PublicKey, U512>>();
                        Ok(bonded_validators)
                    }
                    Ok(_) => Err(GetBondedValidatorsError::ProofOfStakeNotFound(*pos_key)),
                    Err(error) => Err(GetBondedValidatorsError::StateError(error)),
                },
                None => Err(GetBondedValidatorsError::PostStateHashNotFound(root_hash)),
            })
    }
}
