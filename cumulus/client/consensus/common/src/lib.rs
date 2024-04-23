// Copyright (C) Parity Technologies (UK) Ltd.
// This file is part of Cumulus.

// Cumulus is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Cumulus is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Cumulus.  If not, see <http://www.gnu.org/licenses/>.

use codec::Decode;
use polkadot_primitives::{
	Block as PBlock, Hash as PHash, HeadData, Header as PHeader, PersistedValidationData,
	ValidationCodeHash,
};

use cumulus_primitives_core::{
	relay_chain::{self, BlockId as RBlockId, OccupiedCoreAssumption},
	AbridgedHostConfiguration, ParaId,
};
use cumulus_relay_chain_interface::{RelayChainError, RelayChainInterface};

use sc_client_api::{Backend, HeaderBackend};
use sc_consensus::{shared_data::SharedData, BlockImport, ImportResult};
use sp_blockchain::{Backend as BlockchainBackend, TreeRoute};
use sp_consensus_slots::Slot;
use sp_core::H256;
use sp_runtime::traits::{Block as BlockT, Header as HeaderT};
use sp_timestamp::Timestamp;

use std::{sync::Arc, time::Duration};

mod level_monitor;
mod parachain_consensus;
#[cfg(test)]
mod tests;

pub use parachain_consensus::run_parachain_consensus;

use level_monitor::LevelMonitor;
pub use level_monitor::{LevelLimit, MAX_LEAVES_PER_LEVEL_SENSIBLE_DEFAULT};

pub mod import_queue;

const PARENT_SEARCH_LOG_TARGET: &str = "consensus::common::find_potential_parents";

/// Provides the hash of validation code used for authoring/execution of blocks at a given
/// hash.
pub trait ValidationCodeHashProvider<Hash> {
	fn code_hash_at(&self, at: Hash) -> Option<ValidationCodeHash>;
}

impl<F, Hash> ValidationCodeHashProvider<Hash> for F
where
	F: Fn(Hash) -> Option<ValidationCodeHash>,
{
	fn code_hash_at(&self, at: Hash) -> Option<ValidationCodeHash> {
		(self)(at)
	}
}

/// The result of [`ParachainConsensus::produce_candidate`].
pub struct ParachainCandidate<B> {
	/// The block that was built for this candidate.
	pub block: B,
	/// The proof that was recorded while building the block.
	pub proof: sp_trie::StorageProof,
}

/// A specific parachain consensus implementation that can be used by a collator to produce
/// candidates.
///
/// The collator will call [`Self::produce_candidate`] every time there is a free core for the
/// parachain this collator is collating for. It is the job of the consensus implementation to
/// decide if this specific collator should build a candidate for the given relay chain block. The
/// consensus implementation could, for example, check whether this specific collator is part of a
/// staked set.
#[async_trait::async_trait]
pub trait ParachainConsensus<B: BlockT>: Send + Sync + dyn_clone::DynClone {
	/// Produce a new candidate at the given parent block and relay-parent blocks.
	///
	/// Should return `None` if the consensus implementation decided that it shouldn't build a
	/// candidate or if there occurred any error.
	///
	/// # NOTE
	///
	/// It is expected that the block is already imported when the future resolves.
	async fn produce_candidate(
		&mut self,
		parent: &B::Header,
		relay_parent: PHash,
		validation_data: &PersistedValidationData,
	) -> Option<ParachainCandidate<B>>;
}

dyn_clone::clone_trait_object!(<B> ParachainConsensus<B> where B: BlockT);

#[async_trait::async_trait]
impl<B: BlockT> ParachainConsensus<B> for Box<dyn ParachainConsensus<B> + Send + Sync> {
	async fn produce_candidate(
		&mut self,
		parent: &B::Header,
		relay_parent: PHash,
		validation_data: &PersistedValidationData,
	) -> Option<ParachainCandidate<B>> {
		(*self).produce_candidate(parent, relay_parent, validation_data).await
	}
}

/// Parachain specific block import.
///
/// Specialized block import for parachains. It supports to delay setting the best block until the
/// relay chain has included a candidate in its best block. By default the delayed best block
/// setting is disabled. The block import also monitors the imported blocks and prunes by default if
/// there are too many blocks at the same height. Too many blocks at the same height can for example
/// happen if the relay chain is rejecting the parachain blocks in the validation.
pub struct ParachainBlockImport<Block: BlockT, BI, BE> {
	inner: BI,
	monitor: Option<SharedData<LevelMonitor<Block, BE>>>,
	delayed_best_block: bool,
}

impl<Block: BlockT, BI, BE: Backend<Block>> ParachainBlockImport<Block, BI, BE> {
	/// Create a new instance.
	///
	/// The number of leaves per level limit is set to `LevelLimit::Default`.
	pub fn new(inner: BI, backend: Arc<BE>) -> Self {
		Self::new_with_limit(inner, backend, LevelLimit::Default)
	}

	/// Create a new instance with an explicit limit to the number of leaves per level.
	///
	/// This function alone doesn't enforce the limit on levels for old imported blocks,
	/// the limit is eventually enforced only when new blocks are imported.
	pub fn new_with_limit(inner: BI, backend: Arc<BE>, level_leaves_max: LevelLimit) -> Self {
		let level_limit = match level_leaves_max {
			LevelLimit::None => None,
			LevelLimit::Some(limit) => Some(limit),
			LevelLimit::Default => Some(MAX_LEAVES_PER_LEVEL_SENSIBLE_DEFAULT),
		};

		let monitor =
			level_limit.map(|level_limit| SharedData::new(LevelMonitor::new(level_limit, backend)));

		Self { inner, monitor, delayed_best_block: false }
	}

	/// Create a new instance which delays setting the best block.
	///
	/// The number of leaves per level limit is set to `LevelLimit::Default`.
	pub fn new_with_delayed_best_block(inner: BI, backend: Arc<BE>) -> Self {
		Self {
			delayed_best_block: true,
			..Self::new_with_limit(inner, backend, LevelLimit::Default)
		}
	}
}

impl<Block: BlockT, I: Clone, BE> Clone for ParachainBlockImport<Block, I, BE> {
	fn clone(&self) -> Self {
		ParachainBlockImport {
			inner: self.inner.clone(),
			monitor: self.monitor.clone(),
			delayed_best_block: self.delayed_best_block,
		}
	}
}

#[async_trait::async_trait]
impl<Block, BI, BE> BlockImport<Block> for ParachainBlockImport<Block, BI, BE>
where
	Block: BlockT,
	BI: BlockImport<Block> + Send,
	BE: Backend<Block>,
{
	type Error = BI::Error;

	async fn check_block(
		&mut self,
		block: sc_consensus::BlockCheckParams<Block>,
	) -> Result<sc_consensus::ImportResult, Self::Error> {
		self.inner.check_block(block).await
	}

	async fn import_block(
		&mut self,
		mut params: sc_consensus::BlockImportParams<Block>,
	) -> Result<sc_consensus::ImportResult, Self::Error> {
		// Blocks are stored within the backend by using POST hash.
		let hash = params.post_hash();
		let number = *params.header.number();

		if params.with_state() {
			// Force imported state finality.
			// Required for warp sync. We assume that preconditions have been
			// checked properly and we are importing a finalized block with state.
			params.finalized = true;
		}

		if self.delayed_best_block {
			// Best block is determined by the relay chain, or if we are doing the initial sync
			// we import all blocks as new best.
			params.fork_choice = Some(sc_consensus::ForkChoiceStrategy::Custom(
				params.origin == sp_consensus::BlockOrigin::NetworkInitialSync,
			));
		}

		let maybe_lock = self.monitor.as_ref().map(|monitor_lock| {
			let mut monitor = monitor_lock.shared_data_locked();
			monitor.enforce_limit(number);
			monitor.release_mutex()
		});

		let res = self.inner.import_block(params).await?;

		if let (Some(mut monitor_lock), ImportResult::Imported(_)) = (maybe_lock, &res) {
			let mut monitor = monitor_lock.upgrade();
			monitor.block_imported(number, hash);
		}

		Ok(res)
	}
}

/// Marker trait denoting a block import type that fits the parachain requirements.
pub trait ParachainBlockImportMarker {}

impl<B: BlockT, BI, BE> ParachainBlockImportMarker for ParachainBlockImport<B, BI, BE> {}

/// Parameters when searching for suitable parents to build on top of.
#[derive(Debug)]
pub struct ParentSearchParams {
	/// The relay-parent that is intended to be used.
	pub relay_parent: PHash,
	/// The ID of the parachain.
	pub para_id: ParaId,
	/// A limitation on the age of relay parents for parachain blocks that are being
	/// considered. This is relative to the `relay_parent` number.
	pub ancestry_lookback: usize,
	/// How "deep" parents can be relative to the included parachain block at the relay-parent.
	/// The included block has depth 0.
	pub max_depth: usize,
	/// Whether to only ignore "alternative" branches, i.e. branches of the chain
	/// which do not contain the block pending availability.
	pub ignore_alternative_branches: bool,
}

/// A potential parent block returned from [`find_potential_parents`]
#[derive(PartialEq)]
pub struct PotentialParent<B: BlockT> {
	/// The hash of the block.
	pub hash: B::Hash,
	/// The header of the block.
	pub header: B::Header,
	/// The depth of the block.
	pub depth: usize,
	/// Whether the block is the included block, is itself pending on-chain, or descends
	/// from the block pending availability.
	pub aligned_with_pending: bool,
}

impl<B: BlockT> std::fmt::Debug for PotentialParent<B> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("PotentialParent")
			.field("hash", &self.hash)
			.field("depth", &self.depth)
			.field("aligned_with_pending", &self.aligned_with_pending)
			.field("number", &self.header.number())
			.finish()
	}
}

/// Fetch the included and pending block from the relay chain.
async fn fetch_included_pending_from_relay<B: BlockT>(
	relay_client: &impl RelayChainInterface,
	backend: &impl Backend<B>,
	para_id: ParaId,
	relay_parent: PHash,
) -> Result<Option<(B::Header, B::Hash, Option<HeadData>)>, RelayChainError> {
	let included_header = relay_client
		.persisted_validation_data(relay_parent, para_id, OccupiedCoreAssumption::TimedOut)
		.await?;
	let included_header = match included_header {
		Some(pvd) => pvd.parent_head,
		None => return Ok(None), // this implies the para doesn't exist.
	};

	// Fetch the pending header from the relay chain.
	let pending_pvd = relay_client
		.persisted_validation_data(relay_parent, para_id, OccupiedCoreAssumption::Included)
		.await?
		.and_then(|x| if x.parent_head != included_header { Some(x.parent_head) } else { None });

	let included_header = match B::Header::decode(&mut &included_header.0[..]).ok() {
		None => return Ok(None),
		Some(x) => x,
	};

	let included_hash = included_header.hash();
	// If the included block is not locally known, we can't do anything.
	match backend.blockchain().header(included_hash) {
		Ok(None) | Err(_) => {
			tracing::warn!(
				target: PARENT_SEARCH_LOG_TARGET,
				%included_hash,
				"Failed to get header for included block.",
			);
			return Ok(None)
		},
		_ => {},
	};

	Ok(Some((included_header, included_hash, pending_pvd)))
}

/// Perform a recursive search through blocks to find potential
/// parent blocks for a new block.
///
/// This accepts a relay-chain block to be used as an anchor and a maximum search depth,
/// along with some arguments for filtering parachain blocks and performs a recursive search
/// for parachain blocks. The search begins at the last included parachain block and returns
/// a set of [`PotentialParent`]s which could be potential parents of a new block with this
/// relay-parent according to the search parameters.
///
/// A parachain block is a potential parent if it is either the last included parachain block, the
/// pending parachain block (when `max_depth` >= 1), or all of the following hold:
///   * its parent is a potential parent
///   * its relay-parent is within `ancestry_lookback` of the targeted relay-parent.
///   * its relay-parent is within the same session as the targeted relay-parent.
///   * the block number is within `max_depth` blocks of the included block
pub async fn find_potential_parents<B: BlockT>(
	params: ParentSearchParams,
	backend: &impl Backend<B>,
	relay_client: &impl RelayChainInterface,
) -> Result<Vec<PotentialParent<B>>, RelayChainError> {
	tracing::trace!("Parent search parameters: {params:?}");
	// Get the included block.
	let Some((included_header, included_hash, pending_pvd)) = fetch_included_pending_from_relay(
		relay_client,
		backend,
		params.para_id,
		params.relay_parent,
	)
	.await?
	else {
		return Ok(Default::default())
	};

	let only_included = vec![PotentialParent {
		hash: included_hash,
		header: included_header.clone(),
		depth: 0,
		aligned_with_pending: true,
	}];

	if params.max_depth == 0 {
		return Ok(only_included)
	};

	// Pending header and hash.
	let maybe_pending = {
		// Try to decode the pending header.
		let pending_header = pending_pvd.and_then(|p| B::Header::decode(&mut &p.0[..]).ok());

		// If the pending block is not locally known, we can't do anything.
		if let Some(header) = pending_header {
			let pending_hash = header.hash();
			match backend.blockchain().header(pending_hash) {
				// We are supposed to ignore branches that don't contain the pending block, but we
				// do not know the pending block locally.
				Ok(None) | Err(_) if params.ignore_alternative_branches => {
					tracing::warn!(
						target: PARENT_SEARCH_LOG_TARGET,
						%pending_hash,
						"Failed to get header for pending block.",
					);
					return Ok(only_included)
				},
				Ok(Some(_)) => Some((header, pending_hash)),
				_ => None,
			}
		} else {
			None
		}
	};

	let maybe_route = maybe_pending
		.as_ref()
		.map(|(_, pending)| {
			sp_blockchain::tree_route(backend.blockchain(), included_hash, *pending)
		})
		.transpose()?;

	// If we want to ignore alternative branches there is no reason to start
	// the parent search at the included block. We can add the included block and
	// the path to the pending block to the potential parents directly (limited by max_depth).
	let (frontier, potential_parents) = match (
		&maybe_pending,
		params.ignore_alternative_branches,
		&maybe_route,
	) {
		(Some((pending_header, pending_hash)), true, Some(ref route_to_pending)) => {
			let mut potential_parents = only_included;

			// This is a defensive check, should never happen.
			if !route_to_pending.retracted().is_empty() {
				tracing::warn!(target: PARENT_SEARCH_LOG_TARGET, "Included block not an ancestor of pending block. This should not happen.");
				return Ok(Default::default())
			}

			// Add all items on the path included -> pending - 1 to the potential parents, but
			// not more than `max_depth`.
			let num_parents_on_path =
				route_to_pending.enacted().len().saturating_sub(1).min(params.max_depth);
			for (num, block) in
				route_to_pending.enacted().iter().take(num_parents_on_path).enumerate()
			{
				let Ok(Some(header)) = backend.blockchain().header(block.hash) else { continue };

				potential_parents.push(PotentialParent {
					hash: block.hash,
					header,
					depth: 1 + num,
					aligned_with_pending: true,
				});
			}

			// The search for additional potential parents should now start at the children of
			// the pending block.
			(
				vec![PotentialParent {
					hash: *pending_hash,
					header: pending_header.clone(),
					depth: route_to_pending.enacted().len(),
					aligned_with_pending: true,
				}],
				potential_parents,
			)
		},
		_ => (only_included, Default::default()),
	};

	if potential_parents.len() > params.max_depth {
		return Ok(potential_parents);
	}

	// Build up the ancestry record of the relay chain to compare against.
	let rp_ancestry =
		build_relay_parent_ancestry(params.ancestry_lookback, params.relay_parent, relay_client)
			.await?;

	Ok(search_child_branches_for_parents(
		frontier,
		maybe_route,
		included_header,
		maybe_pending.map(|(_, hash)| hash),
		backend,
		params.max_depth,
		params.ignore_alternative_branches,
		rp_ancestry,
		potential_parents,
	))
}

/// Build an ancestry of relay parents that are acceptable.
///
/// An acceptable relay parent is one that is no more than `ancestry_lookback` + 1 blocks below the
/// relay parent we want to build on. Parachain blocks anchored on relay parents older than that can
/// not be considered potential parents for block building. They have no chance of still getting
/// included, so our newly build parachain block would also not get included.
///
/// On success, returns a vector of `(header_hash, state_root)` of the relevant relay chain
/// ancestry blocks.
async fn build_relay_parent_ancestry(
	ancestry_lookback: usize,
	relay_parent: PHash,
	relay_client: &impl RelayChainInterface,
) -> Result<Vec<(H256, H256)>, RelayChainError> {
	let mut ancestry = Vec::with_capacity(ancestry_lookback + 1);
	let mut current_rp = relay_parent;
	let mut required_session = None;
	while ancestry.len() <= ancestry_lookback {
		let header = match relay_client.header(RBlockId::hash(current_rp)).await? {
			None => break,
			Some(h) => h,
		};

		let session = relay_client.session_index_for_child(current_rp).await?;
		if let Some(required_session) = required_session {
			// Respect the relay-chain rule not to cross session boundaries.
			if session != required_session {
				break
			}
		} else {
			required_session = Some(session);
		}

		ancestry.push((current_rp, *header.state_root()));
		current_rp = *header.parent_hash();

		// don't iterate back into the genesis block.
		if header.number == 1 {
			break
		}
	}
	Ok(ancestry)
}

/// Start search for child blocks that can be used as parents.
pub fn search_child_branches_for_parents<Block: BlockT>(
	mut frontier: Vec<PotentialParent<Block>>,
	maybe_route_to_pending: Option<TreeRoute<Block>>,
	included_header: Block::Header,
	pending_hash: Option<Block::Hash>,
	backend: &impl Backend<Block>,
	max_depth: usize,
	ignore_alternative_branches: bool,
	rp_ancestry: Vec<(H256, H256)>,
	mut potential_parents: Vec<PotentialParent<Block>>,
) -> Vec<PotentialParent<Block>> {
	let included_hash = included_header.hash();
	let is_hash_in_ancestry = |hash| rp_ancestry.iter().any(|x| x.0 == hash);
	let is_root_in_ancestry = |root| rp_ancestry.iter().any(|x| x.1 == root);

	// The distance between pending and included block. Is later used to check if a child
	// is aligned with pending when it is between pending and included block.
	let pending_distance = maybe_route_to_pending.as_ref().map(|route| route.enacted().len());

	// If a block is on the path included -> pending, we consider it `aligned_with_pending`.
	let is_child_in_path_to_pending = |hash| {
		maybe_route_to_pending
			.as_ref()
			.map_or(true, |route| route.enacted().iter().any(|x| x.hash == hash))
	};

	tracing::trace!(target: PARENT_SEARCH_LOG_TARGET, ?included_hash, included_num = ?included_header.number(), ?pending_hash , ?rp_ancestry, "Searching relay chain ancestry.");
	while let Some(entry) = frontier.pop() {
		// TODO Adjust once we can fetch multiple pending blocks.
		// https://github.com/paritytech/polkadot-sdk/issues/3967
		let is_pending = pending_hash.as_ref().map_or(false, |h| &entry.hash == h);
		let is_included = included_hash == entry.hash;

		// note: even if the pending block or included block have a relay parent
		// outside of the expected part of the relay chain, they are always allowed
		// because they have already been posted on chain.
		let is_potential = is_pending || is_included || {
			let digest = entry.header.digest();
			let is_hash_in_ancestry_check = cumulus_primitives_core::extract_relay_parent(digest)
				.map_or(false, is_hash_in_ancestry);
			let is_root_in_ancestry_check =
				cumulus_primitives_core::rpsr_digest::extract_relay_parent_storage_root(digest)
					.map(|(r, _n)| r)
					.map_or(false, is_root_in_ancestry);

			is_hash_in_ancestry_check || is_root_in_ancestry_check
		};

		let parent_aligned_with_pending = entry.aligned_with_pending;
		let child_depth = entry.depth + 1;
		let hash = entry.hash;

		tracing::trace!(target: PARENT_SEARCH_LOG_TARGET, root_in_ancestry = is_potential && !is_pending && !is_included, ?hash, is_pending, is_included, "Checking potential parent.");
		if is_potential {
			potential_parents.push(entry);
		}

		if !is_potential || child_depth > max_depth {
			continue
		}

		// push children onto search frontier.
		for child in backend.blockchain().children(hash).ok().into_iter().flatten() {
			tracing::trace!(target: PARENT_SEARCH_LOG_TARGET, ?child, child_depth, ?pending_distance, "Looking at child.");

			let aligned_with_pending = parent_aligned_with_pending &&
				(pending_distance.map_or(true, |dist| child_depth > dist) ||
					pending_hash.as_ref().map_or(true, |h| &child == h) ||
					is_child_in_path_to_pending(child));

			if ignore_alternative_branches && !aligned_with_pending {
				tracing::trace!(target: PARENT_SEARCH_LOG_TARGET, ?child, "Child is not aligned with pending block.");
				continue
			}

			let Ok(Some(header)) = backend.blockchain().header(child) else { continue };

			frontier.push(PotentialParent {
				hash: child,
				header,
				depth: child_depth,
				aligned_with_pending,
			});
		}
	}

	potential_parents
}

/// Get the relay-parent slot and timestamp from a header.
pub fn relay_slot_and_timestamp(
	relay_parent_header: &PHeader,
	relay_chain_slot_duration: Duration,
) -> Option<(Slot, Timestamp)> {
	sc_consensus_babe::find_pre_digest::<PBlock>(relay_parent_header)
		.map(|babe_pre_digest| {
			let slot = babe_pre_digest.slot();
			let t = Timestamp::new(relay_chain_slot_duration.as_millis() as u64 * *slot);

			(slot, t)
		})
		.ok()
}

/// Reads abridged host configuration from the relay chain storage at the given relay parent.
pub async fn load_abridged_host_configuration(
	relay_parent: PHash,
	relay_client: &impl RelayChainInterface,
) -> Result<Option<AbridgedHostConfiguration>, RelayChainError> {
	relay_client
		.get_storage_by_key(relay_parent, relay_chain::well_known_keys::ACTIVE_CONFIG)
		.await?
		.map(|bytes| {
			AbridgedHostConfiguration::decode(&mut &bytes[..])
				.map_err(RelayChainError::DeserializationError)
		})
		.transpose()
}
