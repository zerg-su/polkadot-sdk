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
	precompiles::{AddressMatcher, PrimitivePrecompile},
	Config,
};
use alloc::vec::Vec;
use core::marker::PhantomData;
use sp_core::hex2array;

pub struct EcRecover<T>(PhantomData<T>);

impl<T: Config> PrimitivePrecompile for EcRecover<T> {
	type T = T;
	const MATCHER: AddressMatcher =
		AddressMatcher::Fixed(hex2array!("0100000000000000000000000000000000000000"));

	fn call(
		_address: &[u8; 20],
		_input: &[u8],
		_env: &impl Ext<T = Self::T>,
	) -> Result<Vec<u8>, Vec<u8>> {
		Ok(Vec::new())
	}
}
