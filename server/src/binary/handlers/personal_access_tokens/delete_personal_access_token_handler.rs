use crate::binary::handlers::personal_access_tokens::COMPONENT;
use crate::binary::sender::Sender;
use crate::state::command::EntryCommand;
use crate::streaming::session::Session;
use crate::streaming::systems::system::SharedSystem;
use anyhow::Result;
use error_set::ResultContext;
use iggy::error::IggyError;
use iggy::personal_access_tokens::delete_personal_access_token::DeletePersonalAccessToken;
use tracing::{debug, instrument};

#[instrument(skip_all, fields(iggy_user_id = session.get_user_id(), iggy_client_id = session.client_id))]
pub async fn handle(
    command: DeletePersonalAccessToken,
    sender: &mut dyn Sender,
    session: &Session,
    system: &SharedSystem,
) -> Result<(), IggyError> {
    debug!("session: {session}, command: {command}");
    let token_name = command.name.clone();

    {
        let mut system = system.write().await;
        system
            .delete_personal_access_token(session, &command.name)
            .await
            .with_error(|_| {format!(
                "{COMPONENT} - failed to delete personal access token with name: {token_name}, session: {session}"
            )})?;
    }

    let system = system.read().await;
    system
        .state
        .apply(
            session.get_user_id(),
            EntryCommand::DeletePersonalAccessToken(command),
        )
        .await
        .with_error(|_| {format!(
            "{COMPONENT} - failed to apply delete personal access token with name: {token_name}, session: {session}"
        )})?;
    sender.send_empty_ok_response().await?;
    Ok(())
}
