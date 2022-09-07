use ruma::RoomId;
use crate::Result;

pub trait Data {
    /// Adds the room to the public room directory
    fn set_public(&self, room_id: &RoomId) -> Result<()>;

    /// Removes the room from the public room directory.
    fn set_not_public(&self, room_id: &RoomId) -> Result<()>;

    /// Returns true if the room is in the public room directory.
    fn is_public_room(&self, room_id: &RoomId) -> Result<bool>;

    /// Returns the unsorted public room directory
    fn public_rooms(&self) -> Box<dyn Iterator<Item = Result<Box<RoomId>>>>;
}
