use crate::{utils, Error, Result};
use ruma_identifiers::UserId;
use std::convert::TryFrom;

pub struct Users {
    pub(super) userid_password: sled::Tree,
    pub(super) userid_displayname: sled::Tree,
    pub(super) userid_avatarurl: sled::Tree,
    pub(super) userdeviceids: sled::Tree,
    pub(super) userdeviceid_token: sled::Tree,
    pub(super) token_userdeviceid: sled::Tree,
}

impl Users {
    /// Check if a user has an account on this homeserver.
    pub fn exists(&self, user_id: &UserId) -> Result<bool> {
        Ok(self.userid_password.contains_key(user_id.to_string())?)
    }

    /// Create a new user account on this homeserver.
    pub fn create(&self, user_id: &UserId, hash: &str) -> Result<()> {
        self.userid_password.insert(user_id.to_string(), hash)?;
        Ok(())
    }

    /// Find out which user an access token belongs to.
    pub fn find_from_token(&self, token: &str) -> Result<Option<(UserId, String)>> {
        self.token_userdeviceid
            .get(token)?
            .map_or(Ok(None), |bytes| {
                let mut parts = bytes.split(|&b| b == 0xff);
                let user_bytes = parts
                    .next()
                    .ok_or(Error::BadDatabase("token_userdeviceid value invalid"))?;
                let device_bytes = parts
                    .next()
                    .ok_or(Error::BadDatabase("token_userdeviceid value invalid"))?;

                Ok(Some((
                    UserId::try_from(utils::string_from_bytes(&user_bytes)?)?,
                    utils::string_from_bytes(&device_bytes)?,
                )))
            })
    }

    /// Returns an iterator over all users on this homeserver.
    pub fn iter(&self) -> impl Iterator<Item = Result<UserId>> {
        self.userid_password.iter().keys().map(|r| {
            utils::string_from_bytes(&r?).and_then(|string| Ok(UserId::try_from(&*string)?))
        })
    }

    /// Returns the password hash for the given user.
    pub fn password_hash(&self, user_id: &UserId) -> Result<Option<String>> {
        self.userid_password
            .get(user_id.to_string())?
            .map_or(Ok(None), |bytes| utils::string_from_bytes(&bytes).map(Some))
    }

    /// Returns the displayname of a user on this homeserver.
    pub fn displayname(&self, user_id: &UserId) -> Result<Option<String>> {
        self.userid_displayname
            .get(user_id.to_string())?
            .map_or(Ok(None), |bytes| utils::string_from_bytes(&bytes).map(Some))
    }

    /// Sets a new displayname or removes it if displayname is None. You still need to nofify all rooms of this change.
    pub fn set_displayname(&self, user_id: &UserId, displayname: Option<String>) -> Result<()> {
        if let Some(displayname) = displayname {
            self.userid_displayname
                .insert(user_id.to_string(), &*displayname)?;
        } else {
            self.userid_displayname.remove(user_id.to_string())?;
        }

        Ok(())
    }

    /// Get a the avatar_url of a user.
    pub fn avatar_url(&self, user_id: &UserId) -> Result<Option<String>> {
        self.userid_avatarurl
            .get(user_id.to_string())?
            .map_or(Ok(None), |bytes| utils::string_from_bytes(&bytes).map(Some))
    }

    /// Sets a new avatar_url or removes it if avatar_url is None.
    pub fn set_avatar_url(&self, user_id: &UserId, avatar_url: Option<String>) -> Result<()> {
        if let Some(avatar_url) = avatar_url {
            self.userid_avatarurl
                .insert(user_id.to_string(), &*avatar_url)?;
        } else {
            self.userid_avatarurl.remove(user_id.to_string())?;
        }

        Ok(())
    }

    /// Adds a new device to a user.
    pub fn create_device(&self, user_id: &UserId, device_id: &str, token: &str) -> Result<()> {
        if !self.exists(user_id)? {
            return Err(Error::BadRequest(
                "tried to create device for nonexistent user",
            ));
        }

        let mut key = user_id.to_string().as_bytes().to_vec();
        key.push(0xff);
        key.extend_from_slice(device_id.as_bytes());

        self.userdeviceids.insert(key, &[])?;

        self.set_token(user_id, device_id, token)?;

        Ok(())
    }

    /// Replaces the access token of one device.
    pub fn set_token(&self, user_id: &UserId, device_id: &str, token: &str) -> Result<()> {
        let mut userdeviceid = user_id.to_string().as_bytes().to_vec();
        userdeviceid.push(0xff);
        userdeviceid.extend_from_slice(device_id.as_bytes());

        if self.userdeviceids.get(&userdeviceid)?.is_none() {
            return Err(Error::BadRequest(
                "Tried to set token for nonexistent device",
            ));
        }

        // Remove old token
        if let Some(old_token) = self.userdeviceid_token.get(&userdeviceid)? {
            self.token_userdeviceid.remove(old_token)?;
            // It will be removed from userdeviceid_token by the insert later
        }

        // Assign token to user device combination
        self.userdeviceid_token.insert(&userdeviceid, &*token)?;
        self.token_userdeviceid.insert(token, userdeviceid)?;

        Ok(())
    }
}
