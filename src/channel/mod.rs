//! Channel abstraction for messaging integrations (Telegram, future Discord/Slack).

pub mod telegram;

use async_trait::async_trait;
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;

/// An event received from a channel.
#[derive(Debug, Clone)]
pub enum ChannelEvent {
    /// A regular text message from a user.
    Message {
        chat_id: i64,
        message_id: i64,
        user_id: i64,
        user_name: String,
        text: String,
    },

    /// A slash command from a user (e.g. /reset, /history).
    Command {
        chat_id: i64,
        message_id: i64,
        user_id: i64,
        user_name: String,
        command: String,
        args: String,
    },
}

/// A message to send back through a channel.
#[derive(Debug, Clone)]
pub struct OutboundMessage {
    pub chat_id: i64,
    pub text: String,
}

/// Trait for messaging channel integrations.
///
/// Implementations run a background loop that produces `ChannelEvent`s
/// and can send outbound messages.
#[async_trait]
pub trait Channel: Send + Sync {
    /// Human-readable name for logging.
    fn name(&self) -> &str;

    /// Run the channel's receive loop, sending events to `tx`.
    /// Should run until `cancel` is triggered.
    async fn run(&self, tx: Sender<ChannelEvent>, cancel: CancellationToken);

    /// Send a message through this channel.
    async fn send_message(&self, msg: &OutboundMessage) -> color_eyre::Result<()>;
}
