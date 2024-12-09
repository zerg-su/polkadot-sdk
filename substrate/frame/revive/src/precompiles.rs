// This file is part of Substrate.

// Copyright (C) Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

mod builtin;

#[cfg(test)]
mod tests;

pub use crate::{exec::Ext, Config};
pub use alloy_core as alloy;

use crate::precompiles::builtin::Builtin;
use alloc::vec::Vec;
use alloy::sol_types::{Panic, PanicKind, Revert, SolError, SolInterface};

pub(crate) type AllPrecompiles<T> = (Builtin<T>, <T as Config>::Precompiles);

pub enum AddressMatcher {
	Fixed([u8; 20]),
	Prefix([u8; 16]),
}

pub trait Precompile {
	type T: Config;
	type Interface: SolInterface;
	const MATCHER: AddressMatcher;

	fn call(
		address: &[u8; 20],
		input: &Self::Interface,
		env: &impl Ext<T = Self::T>,
	) -> Result<Vec<u8>, Revert>;
}

pub(crate) trait BuiltinPrecompile {
	type T: Config;
	type Interface: SolInterface;
	const MATCHER: AddressMatcher;
	const CHECK_COLLISION: ();

	fn call(
		address: &[u8; 20],
		input: &Self::Interface,
		env: &impl Ext<T = Self::T>,
	) -> Result<Vec<u8>, Revert>;
}

pub(crate) trait PrimitivePrecompile {
	type T: Config;
	const MATCHER: AddressMatcher;

	fn call(
		address: &[u8; 20],
		input: &[u8],
		env: &impl Ext<T = Self::T>,
	) -> Result<Vec<u8>, Vec<u8>>;
}

pub(crate) trait Precompiles<T: Config> {
	const CHECK_COLLISION: ();

	fn matches(address: &[u8; 20]) -> bool;
	fn call(
		address: &[u8; 20],
		input: &[u8],
		env: &impl Ext<T = T>,
	) -> Option<Result<Vec<u8>, Vec<u8>>>;
}

impl<P: Precompile> BuiltinPrecompile for P {
	type T = <Self as Precompile>::T;
	type Interface = <Self as Precompile>::Interface;
	const MATCHER: AddressMatcher = P::MATCHER;
	const CHECK_COLLISION: () = {
		let bytes = Self::MATCHER.bytes();
		let mut i = 2;
		let mut invalid_prefix = true;
		while i < bytes.len() {
			if bytes[i] != 0 {
				invalid_prefix = false;
			}
			i += 1;
		}
		if invalid_prefix {
			panic!("Precompile addresses in the range 0x00-0xFFFF are reserved.");
		}
	};

	fn call(
		address: &[u8; 20],
		input: &Self::Interface,
		env: &impl Ext<T = Self::T>,
	) -> Result<Vec<u8>, Revert> {
		let _ = Self::CHECK_COLLISION;
		Self::call(address, input, env)
	}
}

impl<P: BuiltinPrecompile> PrimitivePrecompile for P {
	type T = <Self as BuiltinPrecompile>::T;
	const MATCHER: AddressMatcher = P::MATCHER;

	fn call(
		address: &[u8; 20],
		input: &[u8],
		env: &impl Ext<T = Self::T>,
	) -> Result<Vec<u8>, Vec<u8>> {
		let call = <Self as BuiltinPrecompile>::Interface::abi_decode(input, true)
			.map_err(|_| Panic::from(PanicKind::Generic).abi_encode())?;
		match Self::call(address, &call, env) {
			Ok(value) => Ok(value),
			Err(err) => Err(err.abi_encode()),
		}
	}
}

#[impl_trait_for_tuples::impl_for_tuples(10)]
#[tuple_types_custom_trait_bound(PrimitivePrecompile<T=T>)]
impl<T: Config> Precompiles<T> for Tuple {
	const CHECK_COLLISION: () = {
		let matchers = [for_tuples!( #( Tuple::MATCHER ),* )];
		AddressMatcher::check_collision(&matchers);
	};

	fn matches(address: &[u8; 20]) -> bool {
		let _ = <Self as Precompiles<T>>::CHECK_COLLISION;
		for_tuples!(
			#(
				if Tuple::MATCHER.matches(address) {
					return true;
				}
			)*
		);
		false
	}

	fn call(
		address: &[u8; 20],
		input: &[u8],
		env: &impl Ext<T = T>,
	) -> Option<Result<Vec<u8>, Vec<u8>>> {
		for_tuples!(
			#(
				if Self::matches(address) {
					return Some(Tuple::call(address, input, env));
				}
			)*
		);
		None
	}
}

impl<T: Config> Precompiles<T> for (Builtin<T>, <T as Config>::Precompiles) {
	const CHECK_COLLISION: () = ();

	fn matches(address: &[u8; 20]) -> bool {
		<Builtin<T>>::matches(address) || <T as Config>::Precompiles::matches(address)
	}

	fn call(
		address: &[u8; 20],
		input: &[u8],
		env: &impl Ext<T = T>,
	) -> Option<Result<Vec<u8>, Vec<u8>>> {
		<Builtin<T>>::call(address, input, env)
			.or_else(|| <T as Config>::Precompiles::call(address, input, env))
	}
}

impl AddressMatcher {
	const fn bytes(&self) -> &[u8] {
		match self {
			AddressMatcher::Fixed(needle) => needle.as_slice(),
			AddressMatcher::Prefix(prefix) => prefix.as_slice(),
		}
	}

	const fn matches(&self, address: &[u8; 20]) -> bool {
		Self::cmp_prefix(self.bytes(), address)
	}

	const fn cmp_prefix(a: &[u8], b: &[u8]) -> bool {
		let mut i = 0;
		while i < a.len() && i < b.len() {
			if a[i] != b[i] {
				return false
			}
			i += 1;
		}
		return true
	}

	const fn check_collision(list: &[Self]) {
		let len = list.len();
		let mut i = 0;
		let mut collision = false;
		'outer: while i < len {
			let mut j = i + 1;
			while j < len {
				match (&list[i], &list[j]) {
					(Self::Fixed(addr_a), Self::Fixed(addr_b)) => {
						if Self::cmp_prefix(addr_a, addr_b) {
							collision = true;
							break 'outer
						}
					},
					(Self::Fixed(addr_a), Self::Prefix(pref_b)) =>
						if Self::cmp_prefix(addr_a, pref_b) {
							collision = true;
							break 'outer
						},
					(Self::Prefix(pref_a), Self::Fixed(addr_b)) =>
						if Self::cmp_prefix(pref_a, addr_b) {
							collision = true;
							break 'outer
						},
					(Self::Prefix(pref_a), Self::Prefix(pref_b)) =>
						if Self::cmp_prefix(pref_a, pref_b) {
							collision = true;
							break 'outer
						},
				}
				j += 1;
			}
			i += 1;
		}

		if collision {
			panic!("Collision between pre-ompile addresses detected.");
		}
	}
}
