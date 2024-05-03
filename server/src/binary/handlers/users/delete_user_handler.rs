use crate::binary::sender::Sender;
use crate::streaming::session::Session;
use crate::streaming::systems::system::SharedSystem;
use anyhow::Result;
use iggy::error::IggyError;
use iggy::users::delete_user::DeleteUser;
use tracing::debug;

pub async fn handle(
    command: &DeleteUser,
    sender: &mut impl Sender,
    session: &Session,
    system: &SharedSystem,
) -> Result<(), IggyError> {
    debug!("session: {session}, command: {command}");
    let mut system = system.write();
    system.delete_user(session, &command.user_id).await?;
    sender.send_empty_ok_response().await?;
    Ok(())
}
