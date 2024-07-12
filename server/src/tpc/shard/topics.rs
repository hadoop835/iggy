use crate::streaming::topics::topic::Topic;
use crate::tpc::shard::shard::ShardInfo;
use iggy::error::IggyError;
use iggy::identifier::Identifier;
use iggy::models::resource_namespace::IggyResourceNamespace;
use iggy::utils::byte_size::IggyByteSize;
use iggy::{
    compression::compression_algorithm::CompressionAlgorithm, utils::hash::hash_resource_namespace,
};
use std::borrow::BorrowMut;
use tracing::error;

use super::shard::IggyShard;

impl IggyShard {
    pub fn find_topic(
        &self,
        client_id: u32,
        stream_id: &Identifier,
        topic_id: &Identifier,
    ) -> Result<Topic, IggyError> {
        let user_id = self.ensure_authenticated(client_id)?;
        let stream = self.get_stream(stream_id)?;
        let topic = stream.get_topic(topic_id)?;
        self.permissioner
            .borrow()
            .get_topic(user_id, stream.stream_id, topic.topic_id)?;
        Ok(topic.clone())
    }

    pub fn find_topics(
        &self,
        client_id: u32,
        stream_id: &Identifier,
    ) -> Result<Vec<Topic>, IggyError> {
        let user_id = self.ensure_authenticated(client_id)?;
        let stream = self.get_stream(stream_id)?;
        self.permissioner
            .borrow()
            .get_topics(user_id, stream.stream_id)?;
        Ok(stream.get_topics())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_topic(
        &self,
        client_id: u32,
        stream_id: &Identifier,
        topic_id: Option<u32>,
        name: String,
        partitions_count: u32,
        message_expiry: Option<u32>,
        compression_algorithm: CompressionAlgorithm,
        max_topic_size: Option<IggyByteSize>,
        replication_factor: Option<u8>,
        should_persist: bool,
    ) -> Result<(), IggyError> {
        let user_id = self.ensure_authenticated(client_id)?;
        {
            let stream = self.get_stream(stream_id)?;
            self.permissioner
                .borrow()
                .create_topic(user_id, stream.stream_id)?;
        }

        let stream = self.get_stream_mut(stream_id)?.borrow_mut();
        let stream_u32_id = stream.stream_id;

        let (topic_id, partition_ids) = stream
            .create_topic(
                topic_id,
                name,
                partitions_count,
                message_expiry,
                compression_algorithm,
                max_topic_size,
                replication_factor.unwrap_or(1),
                should_persist,
            )
            .await?;

        for partition_id in partition_ids {
            let shards_count = self.get_available_shards_count();
            let resource_ns = IggyResourceNamespace::new(stream_u32_id, topic_id, partition_id);
            let hash = resource_ns.generate_hash();
            let shard_id = hash % shards_count;
            error!("Shard ID: {}", shard_id);
            let shard_info = ShardInfo {
                id: shard_id as u16,
            };
            self.insert_shart_table_record(resource_ns, shard_info);
        }
        self.metrics.increment_topics(1);
        self.metrics.increment_partitions(partitions_count);
        self.metrics.increment_segments(partitions_count);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_topic(
        &self,
        client_id: u32,
        stream_id: &Identifier,
        topic_id: &Identifier,
        name: String,
        message_expiry: Option<u32>,
        compression_algorithm: CompressionAlgorithm,
        max_topic_size: Option<IggyByteSize>,
        replication_factor: Option<u8>,
    ) -> Result<(), IggyError> {
        let user_id = self.ensure_authenticated(client_id)?;
        let stream = self.get_stream(stream_id)?;
        let topic = stream.get_topic(topic_id)?;
        self.permissioner
            .borrow()
            .update_topic(user_id, stream.stream_id, topic.topic_id)?;

        self.get_stream_mut(stream_id)?
            .update_topic(
                topic_id,
                name,
                message_expiry,
                compression_algorithm,
                max_topic_size,
                replication_factor.unwrap_or(1),
            )
            .await?;

        // TODO: if message_expiry is changed, we need to check if we need to purge messages based on the new expiry
        // TODO: if max_size_bytes is changed, we need to check if we need to purge messages based on the new size
        // TODO: if replication_factor is changed, we need to do `something`
        Ok(())
    }

    pub async fn delete_topic(
        &self,
        client_id: u32,
        stream_id: &Identifier,
        topic_id: &Identifier,
    ) -> Result<(), IggyError> {
        let user_id = self.ensure_authenticated(client_id)?;
        let stream_id_value;
        let stream = self.get_stream(stream_id)?;
        let topic = stream.get_topic(topic_id)?;
        self.permissioner
            .borrow()
            .delete_topic(user_id, stream.stream_id, topic.topic_id)?;
        stream_id_value = stream.stream_id;

        let topic = self
            .get_stream_mut(stream_id)?
            .delete_topic(topic_id)
            .await?;

        self.metrics.decrement_topics(1);
        self.metrics
            .decrement_partitions(topic.get_partitions_count());
        self.metrics.decrement_messages(topic.get_messages_count());
        self.metrics
            .decrement_segments(topic.get_segments_count().await);
        self.client_manager
            .borrow_mut()
            .delete_consumer_groups_for_topic(stream_id_value, topic.topic_id);
        Ok(())
    }

    pub async fn purge_topic(
        &self,
        client_id: u32,
        stream_id: &Identifier,
        topic_id: &Identifier,
    ) -> Result<(), IggyError> {
        let user_id = self.ensure_authenticated(client_id)?;
        let stream = self.get_stream(stream_id)?;
        let topic = stream.get_topic(topic_id)?;
        self.permissioner
            .borrow()
            .purge_topic(user_id, stream.stream_id, topic.topic_id)?;
        topic.purge().await
    }
}
