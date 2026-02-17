use async_trait::async_trait;

/// A message received from or sent to a channel
#[derive(Debug, Clone)]
pub struct ChannelMessage {
    pub id: String,
    pub sender: String,
    pub content: String,
    pub channel: String,
    pub timestamp: u64,
}

/// Three-tier lifecycle for channel connections.
/// Active -> Suspended (idle timeout) -> Destroyed (explicit cleanup).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChannelState {
    /// Channel is connected and processing messages
    #[default]
    Active,
    /// Channel is paused (e.g., idle timeout), can be resumed
    Suspended,
    /// Channel is permanently closed, resources released
    Destroyed,
}

/// Core channel trait — implement for any messaging platform
#[async_trait]
pub trait Channel: Send + Sync {
    /// Human-readable channel name
    fn name(&self) -> &str;

    /// Send a message through this channel
    async fn send(&self, message: &str, recipient: &str) -> anyhow::Result<()>;

    /// Start listening for incoming messages (long-running)
    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()>;

    /// Check if channel is healthy
    async fn health_check(&self) -> bool {
        true
    }

    /// Pause the channel (e.g., on idle timeout). Default: no-op.
    async fn suspend(&self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Resume a suspended channel. Default: no-op.
    async fn resume(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_state_default_is_active() {
        assert_eq!(ChannelState::default(), ChannelState::Active);
    }

    #[test]
    fn channel_state_transitions() {
        let mut state = ChannelState::Active;
        state = ChannelState::Suspended;
        assert_eq!(state, ChannelState::Suspended);
        state = ChannelState::Active;
        assert_eq!(state, ChannelState::Active);
        state = ChannelState::Destroyed;
        assert_eq!(state, ChannelState::Destroyed);
    }

    #[test]
    fn channel_state_equality() {
        assert_ne!(ChannelState::Active, ChannelState::Suspended);
        assert_ne!(ChannelState::Suspended, ChannelState::Destroyed);
        assert_ne!(ChannelState::Active, ChannelState::Destroyed);
    }
}
