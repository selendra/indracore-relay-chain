// Copyright 2020 Parity Technologies (UK) Ltd.
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

#![cfg_attr(not(feature = "std"), no_std)]

//! parachain-system is a base module for cumulus-based parachains.
//!
//! This module handles low-level details of being a parachain. It's responsibilities include:
//!
//! - ingestion of the parachain validation data
//! - ingestion of incoming downward and lateral messages and dispatching them
//! - coordinating upgrades with the relay-chain
//! - communication of parachain outputs, such as sent messages, signalling an upgrade, etc.
//!
//! Users must ensure that they register this pallet as an inherent provider.

use cumulus_primitives::{
	inherents::{SystemInherentData, SYSTEM_INHERENT_IDENTIFIER},
	well_known_keys::{self, NEW_VALIDATION_CODE, VALIDATION_DATA}, AbridgedHostConfiguration, DownwardMessageHandler,
	HrmpMessageHandler, HrmpMessageSender, UpwardMessageSender,
	OnValidationData, OutboundHrmpMessage, UpwardMessage, PersistedValidationData, ParaId, relay_chain,
};
use frame_support::{
	decl_error, decl_event, decl_module, decl_storage, ensure, storage,
	weights::{DispatchClass, Weight}, dispatch::DispatchResult, traits::Get,
};
use frame_system::{ensure_none, ensure_root};
use parachain::primitives::RelayChainBlockNumber;
use sp_inherents::{InherentData, InherentIdentifier, ProvideInherent};
use sp_std::{vec::Vec, cmp};

mod relay_state_snapshot;
use relay_state_snapshot::MessagingStateSnapshot;

/// The pallet's configuration trait.
pub trait Config: frame_system::Config {
	/// The overarching event type.
	type Event: From<Event> + Into<<Self as frame_system::Config>::Event>;

	/// Something which can be notified when the validation data is set.
	type OnValidationData: OnValidationData;

	/// Returns the parachain ID we are running with.
	type SelfParaId: Get<ParaId>;

	/// The downward message handlers that will be informed when a message is received.
	type DownwardMessageHandlers: DownwardMessageHandler;

	/// The HRMP message handlers that will be informed when a message is received.
	type HrmpMessageHandlers: HrmpMessageHandler;
}

// This pallet's storage items.
decl_storage! {
	trait Store for Module<T: Config> as ParachainSystem {
		// we need to store the new validation function for the span between
		// setting it and applying it.
		PendingValidationFunction get(fn new_validation_function):
			Option<(RelayChainBlockNumber, Vec<u8>)>;

		/// Were the [`ValidationData`] updated in this block?
		DidUpdateValidationData: bool;

		/// Were the validation data set to notify the relay chain?
		DidSetValidationCode: bool;

		/// The last relay parent block number at which we signalled the code upgrade.
		LastUpgrade: relay_chain::BlockNumber;

		/// The snapshot of some state related to messaging relevant to the current parachain as per
		/// the relay parent.
		///
		/// This field is meant to be updated each block with the validation data inherent. Therefore,
		/// before processing of the inherent, e.g. in `on_initialize` this data may be stale.
		///
		/// This data is also absent from the genesis.
		RelevantMessagingState get(fn relevant_messaging_state): Option<MessagingStateSnapshot>;
		/// The parachain host configuration that was obtained from the relay parent.
		///
		/// This field is meant to be updated each block with the validation data inherent. Therefore,
		/// before processing of the inherent, e.g. in `on_initialize` this data may be stale.
		///
		/// This data is also absent from the genesis.
		HostConfiguration get(fn host_configuration): Option<AbridgedHostConfiguration>;

		PendingUpwardMessages: Vec<UpwardMessage>;

		/// Essentially `OutboundHrmpMessage`s grouped by the recipients.
		OutboundHrmpMessages: map hasher(twox_64_concat) ParaId => Vec<Vec<u8>>;
		/// HRMP channels with the given recipients are awaiting to be processed. If a `ParaId` is
		/// present in this vector then `OutboundHrmpMessages` for it should be not empty.
		NonEmptyHrmpChannels: Vec<ParaId>;
		/// The number of HRMP messages we observed in `on_initialize` and thus used that number for
		/// announcing the weight of `on_initialize` and `on_finialize`.
		AnnouncedHrmpMessagesPerCandidate: u32;
	}
}

// The pallet's dispatchable functions.
decl_module! {
	pub struct Module<T: Config> for enum Call where origin: T::Origin {
		// Initializing events
		// this is needed only if you are using events in your pallet
		fn deposit_event() = default;

		// TODO: figure out a better weight than this
		#[weight = (0, DispatchClass::Operational)]
		pub fn schedule_upgrade(origin, validation_function: Vec<u8>) {
			ensure_root(origin)?;
			<frame_system::Module<T>>::can_set_code(&validation_function)?;
			Self::schedule_upgrade_impl(validation_function)?;
		}

		/// Schedule a validation function upgrade without further checks.
		///
		/// Same as [`Module::schedule_upgrade`], but without checking that the new `validation_function`
		/// is correct. This makes it more flexible, but also opens the door to easily brick the chain.
		#[weight = (0, DispatchClass::Operational)]
		pub fn schedule_upgrade_without_checks(origin, validation_function: Vec<u8>) {
			ensure_root(origin)?;
			Self::schedule_upgrade_impl(validation_function)?;
		}

		/// Set the current validation data.
		///
		/// This should be invoked exactly once per block. It will panic at the finalization
		/// phase if the call was not invoked.
		///
		/// The dispatch origin for this call must be `Inherent`
		///
		/// As a side effect, this function upgrades the current validation function
		/// if the appropriate time has come.
		#[weight = (0, DispatchClass::Mandatory)]
		fn set_validation_data(origin, data: SystemInherentData) -> DispatchResult {
			ensure_none(origin)?;
			assert!(!DidUpdateValidationData::exists(), "ValidationData must be updated only once in a block");

			let SystemInherentData {
				validation_data: vfp,
				relay_chain_state,
				downward_messages,
				horizontal_messages,
			} = data;

			// initialization logic: we know that this runs exactly once every block,
			// which means we can put the initialization logic here to remove the
			// sequencing problem.
			if let Some((apply_block, validation_function)) = PendingValidationFunction::get() {
				if vfp.block_number >= apply_block {
					PendingValidationFunction::kill();
					LastUpgrade::put(&apply_block);
					Self::put_parachain_code(&validation_function);
					Self::deposit_event(Event::ValidationFunctionApplied(vfp.block_number));
				}
			}

			let (host_config, relevant_messaging_state) =
				relay_state_snapshot::extract_from_proof(
					T::SelfParaId::get(),
					vfp.relay_storage_root,
					relay_chain_state
				)
				.map_err(|err| {
					frame_support::debug::print!("invalid relay chain merkle proof: {:?}", err);
					Error::<T>::InvalidRelayChainMerkleProof
				})?;

			storage::unhashed::put(VALIDATION_DATA, &vfp);
			DidUpdateValidationData::put(true);
			RelevantMessagingState::put(relevant_messaging_state);
			HostConfiguration::put(host_config);

			<T::OnValidationData as OnValidationData>::on_validation_data(vfp);

			let dm_count = downward_messages.len() as u32;
			for downward_message in downward_messages {
				T::DownwardMessageHandlers::handle_downward_message(downward_message);
			}

			// Store the processed_downward_messages here so that it's will be accessible from
			// PVF's `validate_block` wrapper and collation pipeline.
			storage::unhashed::put(
				well_known_keys::PROCESSED_DOWNWARD_MESSAGES,
				&dm_count,
			);

			let mut hrmp_watermark = None;
			for (sender, channel_contents) in horizontal_messages {
				for horizontal_message in channel_contents {
					if hrmp_watermark
						.map(|w| w < horizontal_message.sent_at)
						.unwrap_or(true)
					{
						hrmp_watermark = Some(horizontal_message.sent_at);
					}

					T::HrmpMessageHandlers::handle_hrmp_message(sender, horizontal_message);
				}
			}

			// If we processed at least one message, then advance watermark to that location.
			if let Some(hrmp_watermark) = hrmp_watermark {
				storage::unhashed::put(
					well_known_keys::HRMP_WATERMARK,
					&hrmp_watermark,
				);
			}

			Ok(())
		}

		#[weight = (1_000, DispatchClass::Operational)]
		fn sudo_send_upward_message(origin, message: UpwardMessage) {
			ensure_root(origin)?;
			let _ = Self::send_upward_message(message);
		}

		#[weight = (1_000, DispatchClass::Operational)]
		fn sudo_send_hrmp_message(origin, message: OutboundHrmpMessage) {
			ensure_root(origin)?;
			let _ = Self::send_hrmp_message(message);
		}

		fn on_finalize() {
			assert!(DidUpdateValidationData::take(), "VFPs must be updated once per block");
			DidSetValidationCode::take();

			let host_config = Self::host_configuration()
				.expect("host configuration is promised to set until `on_finalize`; qed");
			let relevant_messaging_state = Self::relevant_messaging_state()
				.expect("relevant messaging state is promised to be set until `on_finalize`; qed");

			<Self as Store>::PendingUpwardMessages::mutate(|up| {
				let (count, size) = relevant_messaging_state.relay_dispatch_queue_size;

				let available_capacity = cmp::min(
					host_config.max_upward_queue_count.saturating_sub(count),
					host_config.max_upward_message_num_per_candidate,
				);
				let available_size = host_config.max_upward_queue_size.saturating_sub(size);

				// Count the number of messages we can possibly fit in the given constraints, i.e.
				// available_capacity and available_size.
				let num = up
					.iter()
					.scan(
						(available_capacity as usize, available_size as usize),
						|state, msg| {
							let (cap_left, size_left) = *state;
							match (cap_left.checked_sub(1), size_left.checked_sub(msg.len())) {
								(Some(new_cap), Some(new_size)) => {
									*state = (new_cap, new_size);
									Some(())
								}
								_ => None,
							}
						},
					)
					.count();

				// TODO: #274 Return back messages that do not longer fit into the queue.

				storage::unhashed::put(well_known_keys::UPWARD_MESSAGES, &up[0..num]);
				*up = up.split_off(num);
			});

			// Sending HRMP messages is a little bit more involved. There are the following
			// constraints:
			//
			// - a channel should exist (and it can be closed while a message is buffered),
			// - at most one message can be sent in a channel,
			// - the sent out messages should be ordered by ascension of recipient para id.
			// - the capacity and total size of the channel is limited,
			// - the maximum size of a message is limited (and can potentially be changed),

			let mut non_empty_hrmp_channels = NonEmptyHrmpChannels::get();
			// The number of messages we can send is limited by all of:
			// - the number of non empty channels
			// - the maximum number of messages per candidate according to the fresh config
			// - the maximum number of messages per candidate according to the stale config
			let outbound_hrmp_num =
				non_empty_hrmp_channels.len()
					.min(host_config.hrmp_max_message_num_per_candidate as usize)
					.min(AnnouncedHrmpMessagesPerCandidate::take() as usize);

			let mut outbound_hrmp_messages = Vec::with_capacity(outbound_hrmp_num);
			let mut prune_empty = Vec::with_capacity(outbound_hrmp_num);

			for &recipient in non_empty_hrmp_channels.iter() {
				if outbound_hrmp_messages.len() == outbound_hrmp_num {
					// We have picked the required number of messages for the batch, no reason to
					// iterate further.
					//
					// We check this condition in the beginning of the loop so that we don't include
					// a message where the limit is 0.
					break;
				}

				let idx = match relevant_messaging_state
					.egress_channels
					.binary_search_by_key(&recipient, |(recipient, _)| *recipient)
				{
					Ok(m) => m,
					Err(_) => {
						// TODO: #274 This means that there is no such channel anymore. Means that we should
						// return back the messages from this channel.
						//
						// Until then pretend it became empty
						prune_empty.push(recipient);
						continue;
					}
				};

				let channel_meta = &relevant_messaging_state.egress_channels[idx].1;
				if channel_meta.msg_count + 1 > channel_meta.max_capacity {
					// The channel is at its capacity. Skip it for now.
					continue;
				}

				let mut pending = <Self as Store>::OutboundHrmpMessages::get(&recipient);

				// This panics if `v` is empty. However, we are iterating only once over non-empty
				// channels, therefore it cannot panic.
				let message_payload = pending.remove(0);
				let became_empty = pending.is_empty();

				if channel_meta.total_size + message_payload.len() as u32 > channel_meta.max_total_size {
					// Sending this message will make the channel total size overflow. Skip it for now.
					continue;
				}

				// If we reached here, then the channel has capacity to receive this message. However,
				// it doesn't mean that we are sending it just yet.
				if became_empty {
					OutboundHrmpMessages::remove(&recipient);
					prune_empty.push(recipient);
				} else {
					OutboundHrmpMessages::insert(&recipient, pending);
				}

				if message_payload.len() as u32 > channel_meta.max_message_size {
					// Apparently, the max message size was decreased since the message while the
					// message was buffered. While it's possible to make another iteration to fetch
					// the next message, we just keep going here to not complicate the logic too much.
					//
					// TODO: #274 Return back this message to sender.
					continue;
				}

				outbound_hrmp_messages.push(OutboundHrmpMessage {
					recipient,
					data: message_payload,
				});
			}

			// Sort the outbound messages by asceding recipient para id to satisfy the acceptance
			// criteria requirement.
			outbound_hrmp_messages.sort_by_key(|m| m.recipient);

			// Prune hrmp channels that became empty. Additionally, because it may so happen that we
			// only gave attention to some channels in `non_empty_hrmp_channels` it's important to
			// change the order. Otherwise, the next `on_finalize` we will again give attention
			// only to those channels that happen to be in the beginning, until they are emptied.
			// This leads to "starvation" of the channels near to the end.
			//
			// To mitigate this we shift all processed elements towards the end of the vector using
			// `rotate_left`. To get intution how it works see the examples in its rustdoc.
			non_empty_hrmp_channels.retain(|x| !prune_empty.contains(x));
			// `prune_empty.len()` is greater or equal to `outbound_hrmp_num` because the loop above
			// can only do `outbound_hrmp_num` iterations and `prune_empty` is appended to only inside
			// the loop body.
			non_empty_hrmp_channels.rotate_left(outbound_hrmp_num - prune_empty.len());

			<Self as Store>::NonEmptyHrmpChannels::put(non_empty_hrmp_channels);
			storage::unhashed::put(
				well_known_keys::HRMP_OUTBOUND_MESSAGES,
				&outbound_hrmp_messages,
			);
		}

		fn on_initialize(n: T::BlockNumber) -> Weight {
			// To prevent removing `NEW_VALIDATION_CODE` that was set by another `on_initialize` like
			// for example from scheduler, we only kill the storage entry if it was not yet updated
			// in the current block.
			if !DidSetValidationCode::get() {
				storage::unhashed::kill(NEW_VALIDATION_CODE);
			}

			storage::unhashed::kill(VALIDATION_DATA);

			let mut weight = T::DbWeight::get().writes(3);
			storage::unhashed::kill(well_known_keys::HRMP_WATERMARK);
			storage::unhashed::kill(well_known_keys::UPWARD_MESSAGES);
			storage::unhashed::kill(well_known_keys::HRMP_OUTBOUND_MESSAGES);

			// Here, in `on_initialize` we must report the weight for both `on_initialize` and
			// `on_finalize`.
			//
			// One complication here, is that the `host_configuration` is updated by an inherent and
			// those are processed after the block initialization phase. Therefore, we have to be
			// content only with the configuration as per the previous block. That means that
			// the configuration can be either stale (or be abscent altogether in case of the
			// beginning of the chain).
			//
			// In order to mitigate this, we do the following. At the time, we are only concerned
			// about `hrmp_max_message_num_per_candidate`. We reserve the amount of weight to process
			// the number of HRMP messages according to the potentially stale configuration. In
			// `on_finalize` we will process only the maximum between the announced number of messages
			// and the actual received in the fresh configuration.
			//
			// In the common case, they will be the same. In the case the actual value is smaller
			// than the announced, we would waste some of weight. In the case the actual value is
			// greater than the announced, we will miss opportunity to send a couple of messages.
			weight += T::DbWeight::get().reads_writes(1, 1);
			let hrmp_max_message_num_per_candidate =
				Self::host_configuration()
					.map(|cfg| cfg.hrmp_max_message_num_per_candidate)
					.unwrap_or(0);
			AnnouncedHrmpMessagesPerCandidate::put(hrmp_max_message_num_per_candidate);

			// NOTE that the actual weight consumed by `on_finalize` may turn out lower.
			weight += T::DbWeight::get().reads_writes(
				3 + hrmp_max_message_num_per_candidate as u64,
				4 + hrmp_max_message_num_per_candidate as u64,
			);

			weight
		}
	}
}

impl<T: Config> Module<T> {
	/// Get validation data.
	///
	/// Returns `Some(_)` after the inherent set the data for the current block.
	pub fn validation_data() -> Option<PersistedValidationData> {
		storage::unhashed::get(VALIDATION_DATA)
	}

	/// Put a new validation function into a particular location where indracore
	/// monitors for updates. Calling this function notifies indracore that a new
	/// upgrade has been scheduled.
	fn notify_indracore_of_pending_upgrade(code: &[u8]) {
		storage::unhashed::put_raw(NEW_VALIDATION_CODE, code);
		DidSetValidationCode::put(true);
	}

	/// Put a new validation function into a particular location where this
	/// parachain will execute it on subsequent blocks.
	fn put_parachain_code(code: &[u8]) {
		storage::unhashed::put_raw(sp_core::storage::well_known_keys::CODE, code);
	}

	/// The maximum code size permitted, in bytes.
	///
	/// Returns `None` if the relay chain parachain host configuration hasn't been submitted yet.
	pub fn max_code_size() -> Option<u32> {
		HostConfiguration::get().map(|cfg| cfg.max_code_size)
	}

	/// Returns if a PVF/runtime upgrade could be signalled at the current block, and if so
	/// when the new code will take the effect.
	fn code_upgrade_allowed(
		vfp: &PersistedValidationData,
		cfg: &AbridgedHostConfiguration,
	) -> Option<relay_chain::BlockNumber> {
		if PendingValidationFunction::get().is_some() {
			// There is already upgrade scheduled. Upgrade is not allowed.
			return None;
		}

		let relay_blocks_since_last_upgrade = vfp
			.block_number
			.saturating_sub(LastUpgrade::get());

		if relay_blocks_since_last_upgrade <= cfg.validation_upgrade_frequency {
			// The cooldown after the last upgrade hasn't elapsed yet. Upgrade is not allowed.
			return None;
		}

		Some(vfp.block_number + cfg.validation_upgrade_delay)
	}

	/// The implementation of the runtime upgrade scheduling.
	fn schedule_upgrade_impl(
		validation_function: Vec<u8>,
	) -> DispatchResult {
		ensure!(
			!PendingValidationFunction::exists(),
			Error::<T>::OverlappingUpgrades
		);
		let vfp = Self::validation_data().ok_or(Error::<T>::ValidationDataNotAvailable)?;
		let cfg = Self::host_configuration().ok_or(Error::<T>::HostConfigurationNotAvailable)?;
		ensure!(
			validation_function.len() <= cfg.max_code_size as usize,
			Error::<T>::TooBig
		);
		let apply_block =
			Self::code_upgrade_allowed(&vfp, &cfg).ok_or(Error::<T>::ProhibitedByIndracore)?;

		// When a code upgrade is scheduled, it has to be applied in two
		// places, synchronized: both indracore and the individual parachain
		// have to upgrade on the same relay chain block.
		//
		// `notify_indracore_of_pending_upgrade` notifies indracore; the `PendingValidationFunction`
		// storage keeps track locally for the parachain upgrade, which will
		// be applied later.
		Self::notify_indracore_of_pending_upgrade(&validation_function);
		PendingValidationFunction::put((apply_block, validation_function));
		Self::deposit_event(Event::ValidationFunctionStored(apply_block));

		Ok(())
	}
}


/// An error that can be raised upon sending an upward message.
#[derive(Debug, PartialEq)]
pub enum SendUpErr {
	/// The message sent is too big.
	TooBig,
}

/// An error that can be raised upon sending a horizontal message.
#[derive(Debug, PartialEq)]
pub enum SendHorizontalErr {
	/// The message sent is too big.
	TooBig,
	/// There is no channel to the specified destination.
	NoChannel,
}

impl<T: Config> Module<T> {
	pub fn send_upward_message(message: UpwardMessage) -> Result<(), SendUpErr> {
		// Check if the message fits into the relay-chain constraints.
		//
		// Note, that we are using `host_configuration` here which may be from the previous
		// block, in case this is called from `on_initialize`, i.e. before the inherent with fresh
		// data is submitted.
		//
		// That shouldn't be a problem since this is a preliminary check and the actual check would
		// be performed just before submitting the message from the candidate, and it already can
		// happen that during the time the message is buffered for sending the relay-chain setting
		// may change so that the message is no longer valid.
		//
		// However, changing this setting is expected to be rare.
		match Self::host_configuration() {
			Some(cfg) => {
				if message.len() > cfg.max_upward_message_size as usize {
					return Err(SendUpErr::TooBig)
				}
			},
			None => {
				// This storage field should carry over from the previous block. So if it's None
				// then it must be that this is an edge-case where a message is attempted to be
				// sent at the first block.
				//
				// Let's pass this message through. I think it's not unreasonable to expect that the
				// message is not huge and it comes through, but if it doesn't it can be returned
				// back to the sender.
				//
				// Thus fall through here.
			}
		};
		<Self as Store>::PendingUpwardMessages::append(message);
		Ok(())
	}

	pub fn send_hrmp_message(message: OutboundHrmpMessage) -> Result<(), SendHorizontalErr> {
		let OutboundHrmpMessage { recipient, data } = message;

		// First, check if the message is addressed into an opened channel.
		//
		// Note, that we are using `relevant_messaging_state` which may be from the previous
		// block, in case this is called from `on_initialize`, i.e. before the inherent with fresh
		// data is submitted.
		//
		// That shouldn't be a problem though because this is anticipated and already can happen.
		// This is because sending implies that a message is buffered until there is space to send
		// a message in the candidate. After a while waiting in a buffer, it may be discovered that
		// the channel to which a message were addressed is now closed. Another possibility, is that
		// the maximum message size was decreased so that a message in the bufer doesn't fit. Should
		// any of that happen the sender should be notified about the message was discarded.
		//
		// Here it a similar case, with the difference that the realization that the channel is closed
		// came the same block.
		let relevant_messaging_state = match Self::relevant_messaging_state() {
			Some(s) => s,
			None => {
				// This storage field should carry over from the previous block. So if it's None
				// then it must be that this is an edge-case where a message is attempted to be
				// sent at the first block. It should be safe to assume that there are no channels
				// opened at all so early. At least, relying on this assumption seems to be a better
				// tradeoff, compared to introducing an error variant that the clients should be
				// prepared to handle.
				return Err(SendHorizontalErr::NoChannel)
			}
		};
		let channel_meta = match relevant_messaging_state
			.egress_channels
			.binary_search_by_key(&recipient, |(recipient, _)| *recipient)
		{
			Ok(idx) => &relevant_messaging_state.egress_channels[idx].1,
			Err(_) => return Err(SendHorizontalErr::NoChannel),
		};
		if data.len() as u32 > channel_meta.max_message_size {
			return Err(SendHorizontalErr::TooBig)
		}

		// And then at last update the storage.
		<Self as Store>::OutboundHrmpMessages::append(&recipient, data);
		<Self as Store>::NonEmptyHrmpChannels::mutate(|v| {
			if !v.contains(&recipient) {
				v.push(recipient);
			}
		});

		Ok(())
	}
}

impl<T: Config> UpwardMessageSender for Module<T> {
	fn send_upward_message(message: UpwardMessage) -> Result<(), ()> {
		Self::send_upward_message(message).map_err(|_| ())
	}
}

impl<T: Config> HrmpMessageSender for Module<T> {
	fn send_hrmp_message(message: OutboundHrmpMessage) -> Result<(), ()> {
		Self::send_hrmp_message(message).map_err(|_| ())
	}
}

impl<T: Config> ProvideInherent for Module<T> {
	type Call = Call<T>;
	type Error = sp_inherents::MakeFatalError<()>;
	const INHERENT_IDENTIFIER: InherentIdentifier = SYSTEM_INHERENT_IDENTIFIER;

	fn create_inherent(data: &InherentData) -> Option<Self::Call> {
		let data: SystemInherentData = data
			.get_data(&SYSTEM_INHERENT_IDENTIFIER)
			.ok()
			.flatten()
			.expect("validation function params are always injected into inherent data; qed");

		Some(Call::set_validation_data(data))
	}
}

decl_event! {
	pub enum Event {
		// The validation function has been scheduled to apply as of the contained relay chain block number.
		ValidationFunctionStored(RelayChainBlockNumber),
		// The validation function was applied as of the contained relay chain block number.
		ValidationFunctionApplied(RelayChainBlockNumber),
	}
}

decl_error! {
	pub enum Error for Module<T: Config> {
		/// Attempt to upgrade validation function while existing upgrade pending
		OverlappingUpgrades,
		/// Indracore currently prohibits this parachain from upgrading its validation function
		ProhibitedByIndracore,
		/// The supplied validation function has compiled into a blob larger than Indracore is willing to run
		TooBig,
		/// The inherent which supplies the validation data did not run this block
		ValidationDataNotAvailable,
		/// The inherent which supplies the host configuration did not run this block
		HostConfigurationNotAvailable,
		/// Invalid relay-chain storage merkle proof
		InvalidRelayChainMerkleProof,
	}
}
