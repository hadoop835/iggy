use crate::sourcing::views::SystemView;
use crate::streaming::session::Session;
use crate::streaming::systems::system::System;
use crate::streaming::users::user::User;
use crate::streaming::utils::crypto;
use iggy::bytes_serializable::BytesSerializable;
use iggy::command::CREATE_USER_CODE;
use iggy::error::IggyError;
use iggy::identifier::{IdKind, Identifier};
use iggy::locking::IggySharedMutFn;
use iggy::models::permissions::Permissions;
use iggy::models::user_status::UserStatus;
use iggy::users::create_user::CreateUser;
use iggy::users::defaults::*;
use iggy::utils::text;
use std::env;
use std::sync::atomic::{AtomicU32, Ordering};
use tracing::{error, info, warn};

static USER_ID: AtomicU32 = AtomicU32::new(1);
const MAX_USERS: usize = u32::MAX as usize;

impl System {
    pub(crate) async fn load_users(&mut self, view: &SystemView) -> Result<(), IggyError> {
        info!("Loading users...");
        if view.users.is_empty() {
            info!("No users found, creating the root user...");
            let root = Self::create_root_user();
            let command = CreateUser {
                username: root.username.clone(),
                password: root.password.clone(),
                status: root.status,
                permissions: root.permissions.clone(),
            };
            self.metadata
                .apply(CREATE_USER_CODE, &command.as_bytes())
                .await?;

            self.users.insert(root.id, root);
            info!("Created the root user.");
        }

        for (_, user_view) in view.users.iter() {
            let user = User::with_password(
                user_view.id,
                &user_view.username,
                user_view.password_hash.to_owned(),
                user_view.status,
                user_view.permissions.clone(),
            );
            self.users.insert(user_view.id, user);
        }

        let users_count = self.users.len();
        let current_user_id = self.users.keys().max().unwrap_or(&1);
        USER_ID.store(current_user_id + 1, Ordering::SeqCst);
        self.permissioner
            .init(&self.users.values().collect::<Vec<&User>>());
        self.metrics.increment_users(users_count as u32);
        info!("Initialized {} user(s).", users_count);
        Ok(())
    }

    fn create_root_user() -> User {
        let username = env::var("IGGY_ROOT_USERNAME");
        let password = env::var("IGGY_ROOT_PASSWORD");
        if (username.is_ok() && password.is_err()) || (username.is_err() && password.is_ok()) {
            panic!("When providing the custom root user credentials, both username and password must be set.");
        }
        if username.is_ok() && password.is_ok() {
            info!("Using the custom root user credentials.");
        } else {
            info!("Using the default root user credentials.");
        }

        let username = username.unwrap_or(DEFAULT_ROOT_USERNAME.to_string());
        let password = password.unwrap_or(DEFAULT_ROOT_PASSWORD.to_string());
        if username.is_empty() || password.is_empty() {
            panic!("Root user credentials are not set.");
        }
        if username.len() < MIN_USERNAME_LENGTH {
            panic!("Root username is too short.");
        }
        if username.len() > MAX_USERNAME_LENGTH {
            panic!("Root username is too long.");
        }
        if password.len() < MIN_PASSWORD_LENGTH {
            panic!("Root password is too short.");
        }
        if password.len() > MAX_PASSWORD_LENGTH {
            panic!("Root password is too long.");
        }

        User::root(&username, &password)
    }

    pub fn find_user(&self, session: &Session, user_id: &Identifier) -> Result<&User, IggyError> {
        self.ensure_authenticated(session)?;
        let user = self.get_user(user_id)?;
        let session_user_id = session.get_user_id();
        if user.id != session_user_id {
            self.permissioner.get_user(session_user_id)?;
        }

        Ok(user)
    }

    pub fn get_user(&self, user_id: &Identifier) -> Result<&User, IggyError> {
        match user_id.kind {
            IdKind::Numeric => self
                .users
                .get(&user_id.get_u32_value()?)
                .ok_or(IggyError::ResourceNotFound(user_id.to_string())),
            IdKind::String => {
                let username = user_id.get_cow_str_value()?;
                self.users
                    .iter()
                    .find(|(_, user)| user.username == username)
                    .map(|(_, user)| user)
                    .ok_or(IggyError::ResourceNotFound(user_id.to_string()))
            }
        }
    }

    pub fn get_user_mut(&mut self, user_id: &Identifier) -> Result<&mut User, IggyError> {
        match user_id.kind {
            IdKind::Numeric => self
                .users
                .get_mut(&user_id.get_u32_value()?)
                .ok_or(IggyError::ResourceNotFound(user_id.to_string())),
            IdKind::String => {
                let username = user_id.get_cow_str_value()?;
                self.users
                    .iter_mut()
                    .find(|(_, user)| user.username == username)
                    .map(|(_, user)| user)
                    .ok_or(IggyError::ResourceNotFound(user_id.to_string()))
            }
        }
    }

    pub async fn get_users(&self, session: &Session) -> Result<Vec<&User>, IggyError> {
        self.ensure_authenticated(session)?;
        self.permissioner.get_users(session.get_user_id())?;
        Ok(self.users.values().collect())
    }

    pub async fn create_user(
        &mut self,
        session: &Session,
        username: &str,
        password: &str,
        status: UserStatus,
        permissions: Option<Permissions>,
    ) -> Result<(), IggyError> {
        self.ensure_authenticated(session)?;
        self.permissioner.create_user(session.get_user_id())?;
        let username = text::to_lowercase_non_whitespace(username);
        if self.users.iter().any(|(_, user)| user.username == username) {
            error!("User: {username} already exists.");
            return Err(IggyError::UserAlreadyExists);
        }

        if self.users.len() >= MAX_USERS {
            error!("Available users limit reached.");
            return Err(IggyError::UsersLimitReached);
        }

        let user_id = USER_ID.fetch_add(1, Ordering::SeqCst);
        info!("Creating user: {username} with ID: {user_id}...");
        let user = User::new(user_id, &username, password, status, permissions.clone());
        self.permissioner
            .init_permissions_for_user(user_id, permissions);
        self.users.insert(user.id, user);
        info!("Created user: {username} with ID: {user_id}.");
        self.metrics.increment_users(1);
        Ok(())
    }

    pub async fn delete_user(
        &mut self,
        session: &Session,
        user_id: &Identifier,
    ) -> Result<User, IggyError> {
        self.ensure_authenticated(session)?;
        let existing_user_id;
        let existing_username;
        {
            self.permissioner.delete_user(session.get_user_id())?;
            let user = self.get_user(user_id)?;
            if user.is_root() {
                error!("Cannot delete the root user.");
                return Err(IggyError::CannotDeleteUser(user.id));
            }

            existing_user_id = user.id;
            existing_username = user.username.clone();
        }

        info!("Deleting user: {existing_username} with ID: {user_id}...");
        let user = self
            .users
            .remove(&existing_user_id)
            .ok_or(IggyError::ResourceNotFound(user_id.to_string()))?;
        self.permissioner
            .delete_permissions_for_user(existing_user_id);
        let mut client_manager = self.client_manager.write().await;
        client_manager
            .delete_clients_for_user(existing_user_id)
            .await?;
        info!("Deleted user: {existing_username} with ID: {user_id}.");
        self.metrics.decrement_users(1);
        Ok(user)
    }

    pub async fn update_user(
        &mut self,
        session: &Session,
        user_id: &Identifier,
        username: Option<String>,
        status: Option<UserStatus>,
    ) -> Result<&User, IggyError> {
        self.ensure_authenticated(session)?;
        self.permissioner.update_user(session.get_user_id())?;

        if let Some(username) = username.clone() {
            let username = text::to_lowercase_non_whitespace(&username);
            let user = self.get_user(user_id)?;
            let existing_user = self.get_user(&username.clone().try_into()?);
            if existing_user.is_ok() && existing_user.unwrap().id != user.id {
                error!("User: {username} already exists.");
                return Err(IggyError::UserAlreadyExists);
            }
        }

        let user = self.get_user_mut(user_id)?;
        if let Some(username) = username {
            user.username = username;
        }

        if let Some(status) = status {
            user.status = status;
        }

        info!("Updated user: {} with ID: {}.", user.username, user.id);
        Ok(user)
    }

    pub async fn update_permissions(
        &mut self,
        session: &Session,
        user_id: &Identifier,
        permissions: Option<Permissions>,
    ) -> Result<(), IggyError> {
        self.ensure_authenticated(session)?;

        {
            self.permissioner
                .update_permissions(session.get_user_id())?;
            let user = self.get_user(user_id)?;
            if user.is_root() {
                error!("Cannot change the root user permissions.");
                return Err(IggyError::CannotChangePermissions(user.id));
            }

            self.permissioner
                .update_permissions_for_user(user.id, permissions.clone());
        }

        {
            let user = self.get_user_mut(user_id)?;
            user.permissions = permissions;
            info!(
                "Updated permissions for user: {} with ID: {user_id}.",
                user.username
            );
        }

        Ok(())
    }

    pub async fn change_password(
        &mut self,
        session: &Session,
        user_id: &Identifier,
        current_password: &str,
        new_password: &str,
    ) -> Result<(), IggyError> {
        self.ensure_authenticated(session)?;

        {
            let user = self.get_user(user_id)?;
            let session_user_id = session.get_user_id();
            if user.id != session_user_id {
                self.permissioner.change_password(session_user_id)?;
            }
        }

        let user = self.get_user_mut(user_id)?;
        if !crypto::verify_password(current_password, &user.password) {
            error!(
                "Invalid current password for user: {} with ID: {user_id}.",
                user.username
            );
            return Err(IggyError::InvalidCredentials);
        }

        user.password = crypto::hash_password(new_password);
        info!(
            "Changed password for user: {} with ID: {user_id}.",
            user.username
        );
        Ok(())
    }

    pub async fn login_user(
        &self,
        username: &str,
        password: &str,
        session: Option<&Session>,
    ) -> Result<&User, IggyError> {
        self.login_user_with_credentials(username, Some(password), session)
            .await
    }

    pub async fn login_user_with_credentials(
        &self,
        username: &str,
        password: Option<&str>,
        session: Option<&Session>,
    ) -> Result<&User, IggyError> {
        let user = match self.get_user(&username.try_into()?) {
            Ok(user) => user,
            Err(_) => {
                error!("Cannot login user: {username} (not found).");
                return Err(IggyError::InvalidCredentials);
            }
        };

        info!("Logging in user: {username} with ID: {}...", user.id);
        if !user.is_active() {
            warn!("User: {username} with ID: {} is inactive.", user.id);
            return Err(IggyError::UserInactive);
        }

        if let Some(password) = password {
            if !crypto::verify_password(password, &user.password) {
                warn!(
                    "Invalid password for user: {username} with ID: {}.",
                    user.id
                );
                return Err(IggyError::InvalidCredentials);
            }
        }

        info!("Logged in user: {username} with ID: {}.", user.id);
        if session.is_none() {
            return Ok(user);
        }

        let session = session.unwrap();
        if session.is_authenticated() {
            warn!(
                "User: {} with ID: {} was already authenticated, removing the previous session...",
                user.username,
                session.get_user_id()
            );
            self.logout_user(session).await?;
        }

        session.set_user_id(user.id);
        let mut client_manager = self.client_manager.write().await;
        client_manager
            .set_user_id(session.client_id, user.id)
            .await?;
        Ok(user)
    }

    pub async fn logout_user(&self, session: &Session) -> Result<(), IggyError> {
        self.ensure_authenticated(session)?;
        let user = self.get_user(&Identifier::numeric(session.get_user_id())?)?;
        info!(
            "Logging out user: {} with ID: {}...",
            user.username, user.id
        );
        if session.client_id > 0 {
            let mut client_manager = self.client_manager.write().await;
            client_manager.clear_user_id(session.client_id).await?;
            info!(
                "Cleared user ID: {} for client: {}.",
                user.id, session.client_id
            );
        }
        info!("Logged out user: {} with ID: {}.", user.username, user.id);
        Ok(())
    }
}
