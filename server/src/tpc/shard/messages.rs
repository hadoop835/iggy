use crate::streaming::cache::memory_tracker::CacheMemoryTracker;
use crate::streaming::polling_consumer::PollingConsumer;
use bytes::Bytes;
use iggy::messages::poll_messages::PollingStrategy;
use iggy::messages::send_messages::Message;
use iggy::messages::send_messages::Partitioning;
use iggy::models::messages::{PolledMessage, PolledMessages};
use iggy::{error::IggyError, identifier::Identifier};
use std::borrow::Borrow;
use tracing::{error, trace};

use super::shard::IggyShard;

impl IggyShard {
    pub async fn poll_messages(
        &self,
        client_id: u32,
        partition_id: u32,
        consumer: PollingConsumer,
        stream_id: &Identifier,
        topic_id: &Identifier,
        args: PollingArgs,
    ) -> Result<PolledMessages, IggyError> {
        let user_id = self.ensure_authenticated(client_id)?;
        if args.count == 0 {
            return Err(IggyError::InvalidMessagesCount);
        }

        let stream_lock = self.streams.read().await;
        let stream = self.get_stream(&stream_lock, stream_id)?;
        let topic = stream.get_topic(topic_id)?;
        self.permissioner
            .borrow()
            .poll_messages(user_id, stream.stream_id, topic.topic_id)?;

        if !topic.has_partitions() {
            return Err(IggyError::NoPartitions(topic.topic_id, topic.stream_id));
        }

        let mut polled_messages: PolledMessages = topic
            .get_messages(consumer, partition_id, args.strategy, args.count)
            .await?;

        if polled_messages.messages.is_empty() {
            return Ok(polled_messages);
        }

        let offset = polled_messages.messages.last().unwrap().offset;
        if args.auto_commit {
            trace!("Last offset: {} will be automatically stored for {}, stream: {}, topic: {}, partition: {}", offset, consumer, stream_id, topic_id, partition_id);
            topic.store_consumer_offset(consumer, offset, true).await?;
        }
        drop(stream_lock);

        if self.encryptor.is_none() {
            return Ok(polled_messages);
        }

        let encryptor = self.encryptor.as_ref().unwrap();
        let mut decrypted_messages = Vec::with_capacity(polled_messages.messages.len());
        for message in polled_messages.messages.iter() {
            let payload = encryptor.decrypt(&message.payload);
            match payload {
                Ok(payload) => {
                    decrypted_messages.push(PolledMessage {
                        id: message.id,
                        state: message.state,
                        offset: message.offset,
                        timestamp: message.timestamp,
                        checksum: message.checksum,
                        length: payload.len() as u32,
                        payload: Bytes::from(payload),
                        headers: message.headers.clone(),
                    });
                }
                Err(error) => {
                    error!("Cannot decrypt the message. Error: {}", error);
                    return Err(IggyError::CannotDecryptData);
                }
            }
        }
        polled_messages.messages = decrypted_messages;
        Ok(polled_messages)
    }

    pub async fn append_messages(
        &self,
        client_id: u32,
        stream_id: Identifier,
        topic_id: Identifier,
        partitioning: Partitioning,
        messages: Vec<Message>,
    ) -> Result<(), IggyError> {
        let user_id = self.ensure_authenticated(client_id)?;
        let stream_lock = self.streams.read().await;
        let stream = self.get_stream(&stream_lock, &stream_id)?;
        let topic = stream.borrow().get_topic(&topic_id)?;
        self.permissioner
            .borrow()
            .append_messages(user_id, stream.stream_id, topic.topic_id)?;

        let mut batch_size_bytes = 0;
        let mut messages = messages;
        if let Some(encryptor) = &self.encryptor {
            for message in messages.iter_mut() {
                let payload = encryptor.encrypt(&message.payload);
                match payload {
                    Ok(payload) => {
                        message.payload = Bytes::from(payload);
                        batch_size_bytes += message.get_size_bytes() as u64;
                    }
                    Err(error) => {
                        error!("Cannot encrypt the message. Error: {}", error);
                        return Err(IggyError::CannotEncryptData);
                    }
                }
            }
        } else {
            batch_size_bytes = messages.iter().map(|msg| msg.get_size_bytes() as u64).sum();
        }

        if let Some(memory_tracker) = CacheMemoryTracker::get_instance() {
            if !memory_tracker.will_fit_into_cache(batch_size_bytes) {
                self.clean_cache(batch_size_bytes).await;
            }
        }
        let messages_count = messages.len() as u64;
        topic
            .append_messages(batch_size_bytes, partitioning, messages)
            .await?;
        drop(stream_lock);
        self.metrics.increment_messages(messages_count);
        Ok(())
    }
}

#[derive(Debug)]
pub struct PollingArgs {
    pub strategy: PollingStrategy,
    pub count: u32,
    pub auto_commit: bool,
}

impl PollingArgs {
    pub fn new(strategy: PollingStrategy, count: u32, auto_commit: bool) -> Self {
        Self {
            strategy,
            count,
            auto_commit,
        }
    }
}
