import { useState, useRef, useEffect } from "react";
import { useSyncplayStore } from "../../store";
import { invoke } from "@tauri-apps/api/core";

export function ChatPanel() {
  const messages = useSyncplayStore((state) => state.messages);
  const connection = useSyncplayStore((state) => state.connection);
  const config = useSyncplayStore((state) => state.config);
  const [inputValue, setInputValue] = useState("");
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const chatInputEnabled = config?.user.chat_input_enabled ?? true;

  // Auto-scroll to bottom when new messages arrive
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  const handleSendMessage = async () => {
    if (!inputValue.trim() || !connection.connected || !chatInputEnabled) return;

    try {
      await invoke("send_chat_message", { message: inputValue });
      setInputValue("");
    } catch (error) {
      console.error("Failed to send message:", error);
    }
  };

  const handleKeyPress = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSendMessage();
    }
  };

  const formatTimestamp = (timestamp: string) => {
    const date = new Date(timestamp);
    return date.toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
      hour12: false,
    });
  };

  const getMessageStyle = (messageType: string) => {
    switch (messageType) {
      case "system":
        return "app-text-warning italic";
      case "error":
        return "app-text-danger";
      default:
        return "";
    }
  };

  return (
    <div className="flex flex-col h-full">
      {/* Messages area */}
      <div className="flex-1 p-5 pt-7 overflow-auto space-y-0.5">
        {messages.length === 0 ? (
          <p className="app-text-muted">
            {connection.connected
              ? "No messages yet. Start chatting!"
              : "Welcome to Syncplay! Connect to a server to get started."}
          </p>
        ) : (
          messages.map((msg, index) => {
            // Strip accidental leading/trailing whitespace without affecting internal spacing/newlines.
            const displayMessage = msg.message.trim();

            return (
              <div key={index} className="text-sm app-message">
                <span className="app-text-muted text-xs">{formatTimestamp(msg.timestamp)}</span>
                {msg.username && (
                  <span className="app-text-accent font-medium ml-2">{msg.username}:</span>
                )}
                <span className={`ml-2 ${getMessageStyle(msg.messageType)}`}>{displayMessage}</span>
              </div>
            );
          })
        )}
        <div ref={messagesEndRef} />
      </div>

      {/* Input area */}
      <div className="border-t app-divider px-4 py-3.5 app-surface">
        <input
          type="text"
          value={inputValue}
          onChange={(e) => setInputValue(e.target.value)}
          onKeyPress={handleKeyPress}
          placeholder={
            !connection.connected
              ? "Not connected"
              : chatInputEnabled
                ? "Type a message... (or /help for commands)"
                : "Chat input disabled"
          }
          className="w-full h-8 app-input px-3 py-0 leading-4 rounded-md focus:outline-none focus:border-blue-500"
          disabled={!connection.connected || !chatInputEnabled}
        />
      </div>
    </div>
  );
}
