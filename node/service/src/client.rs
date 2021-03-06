// Copyright 2017-2020 Parity Technologies (UK) Ltd.
// This file is part of Polkadot.

// Polkadot is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Polkadot is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Polkadot.  If not, see <http://www.gnu.org/licenses/>.

//! Indracore Client meta trait

use std::sync::Arc;
use sp_api::{ProvideRuntimeApi, CallApiAt, NumberFor};
use sp_blockchain::HeaderBackend;
use sp_runtime::{
	Justification, generic::{BlockId, SignedBlock}, traits::{Block as BlockT, BlakeTwo256},
};
use sc_client_api::{Backend as BackendT, BlockchainEvents, KeyIterator};
use sp_storage::{StorageData, StorageKey, ChildInfo, PrefixedStorageKey};
use indracore_primitives::v1::{Block, ParachainHost, AccountId, Nonce, Balance, Header, BlockNumber, Hash};
use consensus_common::BlockStatus;

/// A set of APIs that indracore-like runtimes must implement.
pub trait RuntimeApiCollection:
	sp_transaction_pool::runtime_api::TaggedTransactionQueue<Block>
	+ sp_api::ApiExt<Block, Error = sp_blockchain::Error>
	+ babe_primitives::BabeApi<Block>
	+ grandpa_primitives::GrandpaApi<Block>
	+ ParachainHost<Block>
	+ sp_block_builder::BlockBuilder<Block>
	+ frame_system_rpc_runtime_api::AccountNonceApi<Block, AccountId, Nonce>
	+ pallet_transaction_payment_rpc_runtime_api::TransactionPaymentApi<Block, Balance>
	+ pallet_contracts_rpc_runtime_api::ContractsApi<Block, AccountId, Balance, BlockNumber>
	+ sp_api::Metadata<Block>
	+ sp_offchain::OffchainWorkerApi<Block>
	+ sp_session::SessionKeys<Block>
	+ sp_authority_discovery::AuthorityDiscoveryApi<Block>
where
	<Self as sp_api::ApiExt<Block>>::StateBackend: sp_api::StateBackend<BlakeTwo256>,
{}

impl<Api> RuntimeApiCollection for Api
where
	Api: sp_transaction_pool::runtime_api::TaggedTransactionQueue<Block>
		+ sp_api::ApiExt<Block, Error = sp_blockchain::Error>
		+ babe_primitives::BabeApi<Block>
		+ grandpa_primitives::GrandpaApi<Block>
		+ ParachainHost<Block>
		+ sp_block_builder::BlockBuilder<Block>
		+ frame_system_rpc_runtime_api::AccountNonceApi<Block, AccountId, Nonce>
		+ pallet_transaction_payment_rpc_runtime_api::TransactionPaymentApi<Block, Balance>
		+ pallet_contracts_rpc_runtime_api::ContractsApi<Block, AccountId, Balance, BlockNumber>
		+ sp_api::Metadata<Block>
		+ sp_offchain::OffchainWorkerApi<Block>
		+ sp_session::SessionKeys<Block>
		+ sp_authority_discovery::AuthorityDiscoveryApi<Block>,
	<Self as sp_api::ApiExt<Block>>::StateBackend: sp_api::StateBackend<BlakeTwo256>,
{}

/// Trait that abstracts over all available client implementations.
///
/// For a concrete type there exists [`Client`].
pub trait AbstractClient<Block, Backend>:
	BlockchainEvents<Block> + Sized + Send + Sync
	+ ProvideRuntimeApi<Block>
	+ HeaderBackend<Block>
	+ CallApiAt<
		Block,
		Error = sp_blockchain::Error,
		StateBackend = Backend::State
	>
	where
		Block: BlockT,
		Backend: BackendT<Block>,
		Backend::State: sp_api::StateBackend<BlakeTwo256>,
		Self::Api: RuntimeApiCollection<StateBackend = Backend::State>,
{}

impl<Block, Backend, Client> AbstractClient<Block, Backend> for Client
	where
		Block: BlockT,
		Backend: BackendT<Block>,
		Backend::State: sp_api::StateBackend<BlakeTwo256>,
		Client: BlockchainEvents<Block> + ProvideRuntimeApi<Block> + HeaderBackend<Block>
			+ Sized + Send + Sync
			+ CallApiAt<
				Block,
				Error = sp_blockchain::Error,
				StateBackend = Backend::State
			>,
		Client::Api: RuntimeApiCollection<StateBackend = Backend::State>,
{}

/// Execute something with the client instance.
///
/// As there exist multiple chains inside Indracore, like Indracore itself, Xelendra etc,
/// there can exist different kinds of client types. As these client types differ in the generics
/// that are being used, we can not easily return them from a function. For returning them from a
/// function there exists [`Client`]. However, the problem on how to use this client instance still
/// exists. This trait "solves" it in a dirty way. It requires a type to implement this trait and
/// than the [`execute_with_client`](ExecuteWithClient::execute_with_client) function can be called
/// with any possible client instance.
///
/// In a perfect world, we could make a closure work in this way.
pub trait ExecuteWithClient {
	/// The return type when calling this instance.
	type Output;

	/// Execute whatever should be executed with the given client instance.
	fn execute_with_client<Client, Api, Backend>(self, client: Arc<Client>) -> Self::Output
		where
			<Api as sp_api::ApiExt<Block>>::StateBackend: sp_api::StateBackend<BlakeTwo256>,
			Backend: sc_client_api::Backend<Block> + 'static,
			Backend::State: sp_api::StateBackend<BlakeTwo256>,
			Api: crate::RuntimeApiCollection<StateBackend = Backend::State>,
			Client: AbstractClient<Block, Backend, Api = Api> + 'static;
}

/// A handle to a Indracore client instance.
///
/// The Indracore service supports multiple different runtimes (Westend, Indracore itself, etc). As each runtime has a
/// specialized client, we need to hide them behind a trait. This is this trait.
///
/// When wanting to work with the inner client, you need to use `execute_with`.
///
/// See [`ExecuteWithClient`](trait.ExecuteWithClient.html) for more information.
pub trait ClientHandle {
	/// Execute the given something with the client.
	fn execute_with<T: ExecuteWithClient>(&self, t: T) -> T::Output;
}

/// A client instance of Indracore.
///
/// See [`ExecuteWithClient`] for more information.
#[derive(Clone)]
pub enum Client {
	Indracore(Arc<crate::FullClient<indracore_runtime::RuntimeApi, crate::IndracoreExecutor>>),
	Xelendra(Arc<crate::FullClient<xelendra_runtime::RuntimeApi, crate::XelendraExecutor>>),
}

impl ClientHandle for Client {
	fn execute_with<T: ExecuteWithClient>(&self, t: T) -> T::Output {
		match self {
			Self::Indracore(client) => {
				T::execute_with_client::<_, _, crate::FullBackend>(t, client.clone())
			},
			Self::Xelendra(client) => {
				T::execute_with_client::<_, _, crate::FullBackend>(t, client.clone())
			}
		}
	}
}

impl sc_client_api::UsageProvider<Block> for Client {
	fn usage_info(&self) -> sc_client_api::ClientInfo<Block> {
		match self {
			Self::Indracore(client) => client.usage_info(),
			Self::Xelendra(client) => client.usage_info(),
		}
	}
}

impl sc_client_api::BlockBackend<Block> for Client {
	fn block_body(
		&self,
		id: &BlockId<Block>
	) -> sp_blockchain::Result<Option<Vec<<Block as BlockT>::Extrinsic>>> {
		match self {
			Self::Indracore(client) => client.block_body(id),
			Self::Xelendra(client) => client.block_body(id),
		}
	}

	fn block(&self, id: &BlockId<Block>) -> sp_blockchain::Result<Option<SignedBlock<Block>>> {
		match self {
			Self::Indracore(client) => client.block(id),
			Self::Xelendra(client) => client.block(id),
		}
	}

	fn block_status(&self, id: &BlockId<Block>) -> sp_blockchain::Result<BlockStatus> {
		match self {
			Self::Indracore(client) => client.block_status(id),
			Self::Xelendra(client) => client.block_status(id),
		}
	}

	fn justification(
		&self,
		id: &BlockId<Block>
	) -> sp_blockchain::Result<Option<Justification>> {
		match self {
			Self::Indracore(client) => client.justification(id),
			Self::Xelendra(client) => client.justification(id),
		}
	}

	fn block_hash(
		&self,
		number: NumberFor<Block>
	) -> sp_blockchain::Result<Option<<Block as BlockT>::Hash>> {
		match self {
			Self::Indracore(client) => client.block_hash(number),
			Self::Xelendra(client) => client.block_hash(number),
		}
	}
}

impl sc_client_api::StorageProvider<Block, crate::FullBackend> for Client {
	fn storage(
		&self,
		id: &BlockId<Block>,
		key: &StorageKey,
	) -> sp_blockchain::Result<Option<StorageData>> {
		match self {
			Self::Indracore(client) => client.storage(id, key),
			Self::Xelendra(client) => client.storage(id, key),
		}
	}

	fn storage_keys(
		&self,
		id: &BlockId<Block>,
		key_prefix: &StorageKey,
	) -> sp_blockchain::Result<Vec<StorageKey>> {
		match self {
			Self::Indracore(client) => client.storage_keys(id, key_prefix),
			Self::Xelendra(client) => client.storage_keys(id, key_prefix),
		}
	}

	fn storage_hash(
		&self,
		id: &BlockId<Block>,
		key: &StorageKey,
	) -> sp_blockchain::Result<Option<<Block as BlockT>::Hash>> {
		match self {
			Self::Indracore(client) => client.storage_hash(id, key),
			Self::Xelendra(client) => client.storage_hash(id, key),
		}
	}

	fn storage_pairs(
		&self,
		id: &BlockId<Block>,
		key_prefix: &StorageKey,
	) -> sp_blockchain::Result<Vec<(StorageKey, StorageData)>> {
		match self {
			Self::Indracore(client) => client.storage_pairs(id, key_prefix),
			Self::Xelendra(client) => client.storage_pairs(id, key_prefix),
		}
	}

	fn storage_keys_iter<'a>(
		&self,
		id: &BlockId<Block>,
		prefix: Option<&'a StorageKey>,
		start_key: Option<&StorageKey>,
	) -> sp_blockchain::Result<KeyIterator<'a, <crate::FullBackend as sc_client_api::Backend<Block>>::State, Block>> {
		match self {
			Self::Indracore(client) => client.storage_keys_iter(id, prefix, start_key),
			Self::Xelendra(client) => client.storage_keys_iter(id, prefix, start_key),
		}
	}

	fn child_storage(
		&self,
		id: &BlockId<Block>,
		child_info: &ChildInfo,
		key: &StorageKey,
	) -> sp_blockchain::Result<Option<StorageData>> {
		match self {
			Self::Indracore(client) => client.child_storage(id, child_info, key),
			Self::Xelendra(client) => client.child_storage(id, child_info, key),
		}
	}

	fn child_storage_keys(
		&self,
		id: &BlockId<Block>,
		child_info: &ChildInfo,
		key_prefix: &StorageKey,
	) -> sp_blockchain::Result<Vec<StorageKey>> {
		match self {
			Self::Indracore(client) => client.child_storage_keys(id, child_info, key_prefix),
			Self::Xelendra(client) => client.child_storage_keys(id, child_info, key_prefix),
		}
	}

	fn child_storage_hash(
		&self,
		id: &BlockId<Block>,
		child_info: &ChildInfo,
		key: &StorageKey,
	) -> sp_blockchain::Result<Option<<Block as BlockT>::Hash>> {
		match self {
			Self::Indracore(client) => client.child_storage_hash(id, child_info, key),
			Self::Xelendra(client) => client.child_storage_hash(id, child_info, key),
		}
	}

	fn max_key_changes_range(
		&self,
		first: NumberFor<Block>,
		last: BlockId<Block>,
	) -> sp_blockchain::Result<Option<(NumberFor<Block>, BlockId<Block>)>> {
		match self {
			Self::Indracore(client) => client.max_key_changes_range(first, last),
			Self::Xelendra(client) => client.max_key_changes_range(first, last),
		}
	}

	fn key_changes(
		&self,
		first: NumberFor<Block>,
		last: BlockId<Block>,
		storage_key: Option<&PrefixedStorageKey>,
		key: &StorageKey,
	) -> sp_blockchain::Result<Vec<(NumberFor<Block>, u32)>> {
		match self {
			Self::Indracore(client) => client.key_changes(first, last, storage_key, key),
			Self::Xelendra(client) => client.key_changes(first, last, storage_key, key),
		}
	}
}

impl sp_blockchain::HeaderBackend<Block> for Client {
	fn header(&self, id: BlockId<Block>) -> sp_blockchain::Result<Option<Header>> {
		match self {
			Self::Indracore(client) => client.header(&id),
			Self::Xelendra(client) => client.header(&id),
		}
	}

	fn info(&self) -> sp_blockchain::Info<Block> {
		match self {
			Self::Indracore(client) => client.info(),
			Self::Xelendra(client) => client.info(),
		}
	}

	fn status(&self, id: BlockId<Block>) -> sp_blockchain::Result<sp_blockchain::BlockStatus> {
		match self {
			Self::Indracore(client) => client.status(id),
			Self::Xelendra(client) => client.status(id),
		}
	}

	fn number(&self, hash: Hash) -> sp_blockchain::Result<Option<BlockNumber>> {
		match self {
			Self::Indracore(client) => client.number(hash),
			Self::Xelendra(client) => client.number(hash),
		}
	}

	fn hash(&self, number: BlockNumber) -> sp_blockchain::Result<Option<Hash>> {
		match self {
			Self::Indracore(client) => client.hash(number),
			Self::Xelendra(client) => client.hash(number),
		}
	}
}
