// This file is part of Substrate.

// Copyright (C) Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Contains all the interactions with [`Config::Currency`] to manipulate the underlying staking
//! asset.

use frame_support::traits::{
	fungible::{
		hold::{Balanced as FunHoldBalanced, Inspect as FunHoldInspect, Mutate as FunHoldMutate},
		Balanced, Inspect as FunInspect,
	},
	tokens::Precision,
	Defensive,
};
use sp_runtime::{traits::Zero, DispatchResult};

use crate::{BalanceOf, Config, HoldReason, NegativeImbalanceOf, PositiveImbalanceOf};

/// Existential deposit for the chain.
pub fn existential_deposit<T: Config>() -> BalanceOf<T> {
	T::Currency::minimum_balance()
}

/// Total issuance of the chain.
pub fn total_issuance<T: Config>() -> BalanceOf<T> {
	T::Currency::total_issuance()
}

/// Total balance of `who`. Includes both, free and reserved.
pub fn total_balance<T: Config>(who: &T::AccountId) -> BalanceOf<T> {
	T::Currency::total_balance(who)
}

/// Stakeable balance of `who`.
///
/// This includes balance free to stake along with any balance that is already staked.
pub fn stakeable_balance<T: Config>(who: &T::AccountId) -> BalanceOf<T> {
	T::Currency::balance(who) + T::Currency::balance_on_hold(&HoldReason::Staking.into(), who)
}

/// Balance of `who` that is currently at stake.
///
/// The staked amount is locked and cannot be transferred out of `who`s account.
pub fn staked<T: Config>(who: &T::AccountId) -> BalanceOf<T> {
	T::Currency::balance_on_hold(&HoldReason::Staking.into(), who)
}

/// Set balance that can be staked for `who`.
///
/// `Value` must be greater than already staked plus existential deposit for free balance.
///
/// Should only be used with test.
#[cfg(any(test, feature = "runtime-benchmarks"))]
pub fn set_stakeable_balance<T: Config>(who: &T::AccountId, value: BalanceOf<T>) {
	use frame_support::traits::fungible::Mutate;

	let staked_balance = staked::<T>(who);
	// if value is greater than staked balance, we need to increase the free balance.
	if value > staked_balance {
		let _ = T::Currency::set_balance(who, value - staked_balance);
	} else {
		// else reduce the staked balance.
		update_stake::<T>(who, value).expect("can remove from what is staked");
		// burn all free
		let _ = T::Currency::set_balance(who, Zero::zero());
	}

	assert_eq!(total_balance::<T>(who), value);
}

/// Update `amount` at stake for `who`.
///
/// Overwrites the existing stake amount. If passed amount is lower than the existing stake, the
/// difference is unlocked.
pub fn update_stake<T: Config>(who: &T::AccountId, amount: BalanceOf<T>) -> DispatchResult {
	// if first stake, inc provider. This allows us to stake all free balance.
	if staked::<T>(who) == Zero::zero() && amount > Zero::zero() {
		frame_system::Pallet::<T>::inc_providers(who);
	}

	T::Currency::set_on_hold(&HoldReason::Staking.into(), who, amount)
}

pub fn kill_stake<T: Config>(who: &T::AccountId) -> DispatchResult {
	T::Currency::release_all(&HoldReason::Staking.into(), who, Precision::BestEffort)
		.map(|_| ())?;
	// dec provider that we incremented for a new stake.
	let _ = frame_system::Pallet::<T>::dec_providers(who).defensive();
	Ok(())
}

/// Slash the value from `who`.
///
/// A negative imbalance is returned which can be resolved to deposit the slashed value.
pub fn slash<T: Config>(
	who: &T::AccountId,
	value: BalanceOf<T>,
) -> (NegativeImbalanceOf<T>, BalanceOf<T>) {
	T::Currency::slash(&HoldReason::Staking.into(), who, value)
}

/// Mint `value` into an existing account.
///
/// This does not increase the total issuance.
pub fn mint_existing<T: Config>(
	who: &T::AccountId,
	value: BalanceOf<T>,
) -> Option<PositiveImbalanceOf<T>> {
	// since the account already exists, we mint exact value even if value is below ED.
	T::Currency::deposit(who, value, Precision::Exact).ok()
}

/// Mint `value` and create account for `who` if it does not exist.
///
/// If value is below existential deposit, the account is not created.
///
/// This does not increase the total issuance.
pub fn mint_creating<T: Config>(who: &T::AccountId, value: BalanceOf<T>) -> PositiveImbalanceOf<T> {
	T::Currency::deposit(who, value, Precision::BestEffort).unwrap_or_default()
}

/// Deposit newly issued or slashed `value` into `who`.
pub fn deposit_slashed<T: Config>(who: &T::AccountId, value: NegativeImbalanceOf<T>) {
	let _ = T::Currency::resolve(who, value);
}

/// Issue `value` increasing total issuance.
///
/// Creates a negative imbalance.
pub fn issue<T: Config>(value: BalanceOf<T>) -> NegativeImbalanceOf<T> {
	T::Currency::issue(value)
}

/// Burn the amount from the total issuance.
#[cfg(feature = "runtime-benchmarks")]
pub fn burn<T: Config>(amount: BalanceOf<T>) -> PositiveImbalanceOf<T> {
	T::Currency::rescind(amount)
}
