mod data;
use std::collections::HashSet;

pub use data::Data;
use ruma::{RoomId, events::{room::{member::MembershipState, create::RoomCreateEventContent}, AnyStrippedStateEvent, StateEventType}, UserId, EventId, serde::Raw, RoomVersionId};
use serde::Deserialize;
use tracing::warn;

use crate::{Result, services, PduEvent, Error, utils::calculate_hash};

use super::state_compressor::CompressedStateEvent;

pub struct Service<D: Data> {
    db: D,
}

impl<D: Data> Service<D> {
    /// Set the room to the given statehash and update caches.
    pub fn force_state(
        &self,
        room_id: &RoomId,
        shortstatehash: u64,
        statediffnew: HashSet<CompressedStateEvent>,
        statediffremoved: HashSet<CompressedStateEvent>,
    ) -> Result<()> {

        for event_id in statediffnew.into_iter().filter_map(|new| {
            services().rooms.state_compressor.parse_compressed_state_event(new)
                .ok()
                .map(|(_, id)| id)
        }) {
            let pdu = match services().rooms.timeline.get_pdu_json(&event_id)? {
                Some(pdu) => pdu,
                None => continue,
            };

            if pdu.get("type").and_then(|val| val.as_str()) != Some("m.room.member") {
                continue;
            }

            let pdu: PduEvent = match serde_json::from_str(
                &serde_json::to_string(&pdu).expect("CanonicalJsonObj can be serialized to JSON"),
            ) {
                Ok(pdu) => pdu,
                Err(_) => continue,
            };

            #[derive(Deserialize)]
            struct ExtractMembership {
                membership: MembershipState,
            }

            let membership = match serde_json::from_str::<ExtractMembership>(pdu.content.get()) {
                Ok(e) => e.membership,
                Err(_) => continue,
            };

            let state_key = match pdu.state_key {
                Some(k) => k,
                None => continue,
            };

            let user_id = match UserId::parse(state_key) {
                Ok(id) => id,
                Err(_) => continue,
            };

            services().room.state_cache.update_membership(room_id, &user_id, membership, &pdu.sender, None, false)?;
        }

        services().room.state_cache.update_joined_count(room_id)?;

        self.db.set_room_state(room_id, shortstatehash);

        Ok(())
    }

    /// Generates a new StateHash and associates it with the incoming event.
    ///
    /// This adds all current state events (not including the incoming event)
    /// to `stateid_pduid` and adds the incoming event to `eventid_statehash`.
    #[tracing::instrument(skip(self, state_ids_compressed))]
    pub fn set_event_state(
        &self,
        event_id: &EventId,
        room_id: &RoomId,
        state_ids_compressed: HashSet<CompressedStateEvent>,
    ) -> Result<()> {
        let shorteventid = services().short.get_or_create_shorteventid(event_id)?;

        let previous_shortstatehash = self.db.get_room_shortstatehash(room_id)?;

        let state_hash = calculate_hash(
            &state_ids_compressed
                .iter()
                .map(|s| &s[..])
                .collect::<Vec<_>>(),
        );

        let (shortstatehash, already_existed) =
            services().short.get_or_create_shortstatehash(&state_hash)?;

        if !already_existed {
            let states_parents = previous_shortstatehash
                .map_or_else(|| Ok(Vec::new()), |p| services().room.state_compressor.load_shortstatehash_info(p))?;

            let (statediffnew, statediffremoved) =
                if let Some(parent_stateinfo) = states_parents.last() {
                    let statediffnew: HashSet<_> = state_ids_compressed
                        .difference(&parent_stateinfo.1)
                        .copied()
                        .collect();

                    let statediffremoved: HashSet<_> = parent_stateinfo
                        .1
                        .difference(&state_ids_compressed)
                        .copied()
                        .collect();

                    (statediffnew, statediffremoved)
                } else {
                    (state_ids_compressed, HashSet::new())
                };
            services().room.state_compressor.save_state_from_diff(
                shortstatehash,
                statediffnew,
                statediffremoved,
                1_000_000, // high number because no state will be based on this one
                states_parents,
            )?;
        }

        self.db.set_event_state(&shorteventid.to_be_bytes(), &shortstatehash.to_be_bytes())?;

        Ok(())
    }

    /// Generates a new StateHash and associates it with the incoming event.
    ///
    /// This adds all current state events (not including the incoming event)
    /// to `stateid_pduid` and adds the incoming event to `eventid_statehash`.
    #[tracing::instrument(skip(self, new_pdu))]
    pub fn append_to_state(
        &self,
        new_pdu: &PduEvent,
    ) -> Result<u64> {
        let shorteventid = self.get_or_create_shorteventid(&new_pdu.event_id)?;

        let previous_shortstatehash = self.get_room_shortstatehash(&new_pdu.room_id)?;

        if let Some(p) = previous_shortstatehash {
            self.shorteventid_shortstatehash
                .insert(&shorteventid.to_be_bytes(), &p.to_be_bytes())?;
        }

        if let Some(state_key) = &new_pdu.state_key {
            let states_parents = previous_shortstatehash
                .map_or_else(|| Ok(Vec::new()), |p| self.load_shortstatehash_info(p))?;

            let shortstatekey = self.get_or_create_shortstatekey(
                &new_pdu.kind.to_string().into(),
                state_key,
            )?;

            let new = self.compress_state_event(shortstatekey, &new_pdu.event_id)?;

            let replaces = states_parents
                .last()
                .map(|info| {
                    info.1
                        .iter()
                        .find(|bytes| bytes.starts_with(&shortstatekey.to_be_bytes()))
                })
                .unwrap_or_default();

            if Some(&new) == replaces {
                return Ok(previous_shortstatehash.expect("must exist"));
            }

            // TODO: statehash with deterministic inputs
            let shortstatehash = services().globals.next_count()?;

            let mut statediffnew = HashSet::new();
            statediffnew.insert(new);

            let mut statediffremoved = HashSet::new();
            if let Some(replaces) = replaces {
                statediffremoved.insert(*replaces);
            }

            self.save_state_from_diff(
                shortstatehash,
                statediffnew,
                statediffremoved,
                2,
                states_parents,
            )?;

            Ok(shortstatehash)
        } else {
            Ok(previous_shortstatehash.expect("first event in room must be a state event"))
        }
    }

    #[tracing::instrument(skip(self, invite_event))]
    pub fn calculate_invite_state(
        &self,
        invite_event: &PduEvent,
    ) -> Result<Vec<Raw<AnyStrippedStateEvent>>> {
        let mut state = Vec::new();
        // Add recommended events
        if let Some(e) =
            self.room_state_get(&invite_event.room_id, &StateEventType::RoomCreate, "")?
        {
            state.push(e.to_stripped_state_event());
        }
        if let Some(e) =
            self.room_state_get(&invite_event.room_id, &StateEventType::RoomJoinRules, "")?
        {
            state.push(e.to_stripped_state_event());
        }
        if let Some(e) = self.room_state_get(
            &invite_event.room_id,
            &StateEventType::RoomCanonicalAlias,
            "",
        )? {
            state.push(e.to_stripped_state_event());
        }
        if let Some(e) =
            self.room_state_get(&invite_event.room_id, &StateEventType::RoomAvatar, "")?
        {
            state.push(e.to_stripped_state_event());
        }
        if let Some(e) =
            self.room_state_get(&invite_event.room_id, &StateEventType::RoomName, "")?
        {
            state.push(e.to_stripped_state_event());
        }
        if let Some(e) = self.room_state_get(
            &invite_event.room_id,
            &StateEventType::RoomMember,
            invite_event.sender.as_str(),
        )? {
            state.push(e.to_stripped_state_event());
        }

        state.push(invite_event.to_stripped_state_event());
        Ok(state)
    }

    #[tracing::instrument(skip(self))]
    pub fn set_room_state(&self, room_id: &RoomId, shortstatehash: u64) -> Result<()> {
        self.roomid_shortstatehash
            .insert(room_id.as_bytes(), &shortstatehash.to_be_bytes())?;

        Ok(())
    }

    /// Returns the room's version.
    #[tracing::instrument(skip(self))]
    pub fn get_room_version(&self, room_id: &RoomId) -> Result<RoomVersionId> {
        let create_event = self.room_state_get(room_id, &StateEventType::RoomCreate, "")?;

        let create_event_content: Option<RoomCreateEventContent> = create_event
            .as_ref()
            .map(|create_event| {
                serde_json::from_str(create_event.content.get()).map_err(|e| {
                    warn!("Invalid create event: {}", e);
                    Error::bad_database("Invalid create event in db.")
                })
            })
            .transpose()?;
        let room_version = create_event_content
            .map(|create_event| create_event.room_version)
            .ok_or_else(|| Error::BadDatabase("Invalid room version"))?;
        Ok(room_version)
    }

    pub fn get_room_shortstatehash(&self, room_id: &RoomId) -> Result<Option<u64>> {
        self.db.get_room_shortstatehash(room_id)
    }
}
