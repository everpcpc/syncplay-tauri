use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use std::sync::Arc;
use tracing::{debug, info};

/// Chat message type
#[derive(Debug, Clone, PartialEq)]
pub enum ChatMessageType {
    /// Regular user message
    User,
    /// System message
    System,
    /// Server message
    Server,
    /// Error message
    Error,
}

/// Chat message
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub timestamp: DateTime<Utc>,
    pub username: Option<String>,
    pub message: String,
    pub message_type: ChatMessageType,
}

impl ChatMessage {
    pub fn user(username: String, message: String) -> Self {
        Self {
            timestamp: Utc::now(),
            username: Some(username),
            message,
            message_type: ChatMessageType::User,
        }
    }

    pub fn system(message: String) -> Self {
        Self {
            timestamp: Utc::now(),
            username: None,
            message,
            message_type: ChatMessageType::System,
        }
    }

    pub fn server(message: String) -> Self {
        Self {
            timestamp: Utc::now(),
            username: None,
            message,
            message_type: ChatMessageType::Server,
        }
    }

    pub fn error(message: String) -> Self {
        Self {
            timestamp: Utc::now(),
            username: None,
            message,
            message_type: ChatMessageType::Error,
        }
    }
}

/// Chat command
#[derive(Debug, Clone, PartialEq)]
pub enum ChatCommand {
    /// Change room: /room <name>
    Room(String),
    /// List users: /list
    List,
    /// Show help: /help
    Help,
    /// Set ready: /ready
    Ready,
    /// Set not ready: /unready
    Unready,
    /// Unknown command
    Unknown(String),
}

impl ChatCommand {
    /// Parse a chat command from a message
    pub fn parse(message: &str) -> Option<Self> {
        if !message.starts_with('/') {
            return None;
        }

        let parts: Vec<&str> = message.split_whitespace().collect();
        if parts.is_empty() {
            return None;
        }

        let command = parts[0].to_lowercase();
        match command.as_str() {
            "/room" | "/r" => {
                if parts.len() > 1 {
                    Some(ChatCommand::Room(parts[1..].join(" ")))
                } else {
                    Some(ChatCommand::Unknown("Usage: /room <name>".to_string()))
                }
            }
            "/list" | "/l" => Some(ChatCommand::List),
            "/help" | "/h" | "/?" => Some(ChatCommand::Help),
            "/ready" => Some(ChatCommand::Ready),
            "/unready" => Some(ChatCommand::Unready),
            _ => Some(ChatCommand::Unknown(format!("Unknown command: {}", command))),
        }
    }

    /// Get help text for all commands
    pub fn help_text() -> String {
        r#"Available commands:
/room <name> or /r <name> - Change to a different room
/list or /l - List all users in the current room
/ready - Mark yourself as ready
/unready - Mark yourself as not ready
/help or /h or /? - Show this help message"#
            .to_string()
    }
}

/// Chat manager
pub struct ChatManager {
    messages: RwLock<Vec<ChatMessage>>,
    max_messages: usize,
}

impl ChatManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            messages: RwLock::new(Vec::new()),
            max_messages: 1000,
        })
    }

    pub fn with_max_messages(max_messages: usize) -> Arc<Self> {
        Arc::new(Self {
            messages: RwLock::new(Vec::new()),
            max_messages,
        })
    }

    /// Add a message to chat history
    pub fn add_message(&self, message: ChatMessage) {
        let mut messages = self.messages.write();
        messages.push(message);

        // Trim old messages if we exceed the limit
        if messages.len() > self.max_messages {
            let excess = messages.len() - self.max_messages;
            messages.drain(0..excess);
        }
    }

    /// Add a user message
    pub fn add_user_message(&self, username: String, message: String) {
        info!("Chat message from {}: {}", username, message);
        self.add_message(ChatMessage::user(username, message));
    }

    /// Add a system message
    pub fn add_system_message(&self, message: String) {
        info!("System message: {}", message);
        self.add_message(ChatMessage::system(message));
    }

    /// Add a server message
    pub fn add_server_message(&self, message: String) {
        info!("Server message: {}", message);
        self.add_message(ChatMessage::server(message));
    }

    /// Add an error message
    pub fn add_error_message(&self, message: String) {
        info!("Error message: {}", message);
        self.add_message(ChatMessage::error(message));
    }

    /// Get all messages
    pub fn get_messages(&self) -> Vec<ChatMessage> {
        self.messages.read().clone()
    }

    /// Get recent messages (last N)
    pub fn get_recent_messages(&self, count: usize) -> Vec<ChatMessage> {
        let messages = self.messages.read();
        let start = messages.len().saturating_sub(count);
        messages[start..].to_vec()
    }

    /// Clear all messages
    pub fn clear(&self) {
        info!("Clearing chat history");
        self.messages.write().clear();
    }

    /// Get message count
    pub fn len(&self) -> usize {
        self.messages.read().len()
    }

    /// Check if chat is empty
    pub fn is_empty(&self) -> bool {
        self.messages.read().is_empty()
    }
}

impl Default for ChatManager {
    fn default() -> Self {
        Self {
            messages: RwLock::new(Vec::new()),
            max_messages: 1000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chat_command_parse_room() {
        let cmd = ChatCommand::parse("/room test");
        assert_eq!(cmd, Some(ChatCommand::Room("test".to_string())));

        let cmd = ChatCommand::parse("/r test room");
        assert_eq!(cmd, Some(ChatCommand::Room("test room".to_string())));
    }

    #[test]
    fn test_chat_command_parse_list() {
        let cmd = ChatCommand::parse("/list");
        assert_eq!(cmd, Some(ChatCommand::List));

        let cmd = ChatCommand::parse("/l");
        assert_eq!(cmd, Some(ChatCommand::List));
    }

    #[test]
    fn test_chat_command_parse_help() {
        let cmd = ChatCommand::parse("/help");
        assert_eq!(cmd, Some(ChatCommand::Help));

        let cmd = ChatCommand::parse("/h");
        assert_eq!(cmd, Some(ChatCommand::Help));

        let cmd = ChatCommand::parse("/?");
        assert_eq!(cmd, Some(ChatCommand::Help));
    }

    #[test]
    fn test_chat_command_parse_ready() {
        let cmd = ChatCommand::parse("/ready");
        assert_eq!(cmd, Some(ChatCommand::Ready));

        let cmd = ChatCommand::parse("/unready");
        assert_eq!(cmd, Some(ChatCommand::Unready));
    }

    #[test]
    fn test_chat_command_parse_unknown() {
        let cmd = ChatCommand::parse("/unknown");
        assert!(matches!(cmd, Some(ChatCommand::Unknown(_))));
    }

    #[test]
    fn test_chat_command_parse_not_command() {
        let cmd = ChatCommand::parse("hello world");
        assert_eq!(cmd, None);
    }

    #[test]
    fn test_chat_manager_add_messages() {
        let manager = ChatManager::new();
        manager.add_user_message("user1".to_string(), "Hello".to_string());
        manager.add_system_message("System message".to_string());

        assert_eq!(manager.len(), 2);
    }

    #[test]
    fn test_chat_manager_max_messages() {
        let manager = ChatManager::with_max_messages(5);

        for i in 0..10 {
            manager.add_user_message("user".to_string(), format!("Message {}", i));
        }

        assert_eq!(manager.len(), 5);
        let messages = manager.get_messages();
        assert_eq!(messages[0].message, "Message 5");
    }

    #[test]
    fn test_chat_manager_recent_messages() {
        let manager = ChatManager::new();

        for i in 0..10 {
            manager.add_user_message("user".to_string(), format!("Message {}", i));
        }

        let recent = manager.get_recent_messages(3);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].message, "Message 7");
        assert_eq!(recent[2].message, "Message 9");
    }

    #[test]
    fn test_chat_manager_clear() {
        let manager = ChatManager::new();
        manager.add_user_message("user".to_string(), "Hello".to_string());

        manager.clear();
        assert!(manager.is_empty());
    }
}
