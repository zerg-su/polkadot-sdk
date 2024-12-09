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

use crate::{
	exec::Ext,
	precompiles::{AddressMatcher, BuiltinPrecompile},
	Config,
};
use alloc::vec::Vec;
use alloy_core::{
	sol,
	sol_types::{Revert, SolValue},
};
use core::marker::PhantomData;
use sp_core::hex2array;

sol! {
	struct CallInfo {
		uint32 status;
		uint32 test;
	}

	interface ISystem {
		function call(address callee) external returns (CallInfo);
	}
}

pub struct System<T>(PhantomData<T>);

impl<T: Config> BuiltinPrecompile for System<T> {
	type T = T;
	type Interface = ISystem::ISystemCalls;
	const MATCHER: AddressMatcher =
		AddressMatcher::Fixed(hex2array!("0001000000000000000000000000000000000000"));
	const CHECK_COLLISION: () = ();

	fn call(
		_address: &[u8; 20],
		_input: &Self::Interface,
		_env: &impl Ext<T = Self::T>,
	) -> Result<Vec<u8>, Revert> {
		let result = CallInfo { status: 42, test: 7 };
		Ok(result.abi_encode())
	}
}
