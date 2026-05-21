import { useState, useRef, useEffect } from "react";
import { useSyncplayStore } from "../../store";
import { invoke } from "@tauri-apps/api/core";

export function ChatPanel() {
  const messages = useSyncplayStore((state) => state.messages);
  const connection = useSyncplayStore((state) => state.connection);
  const config = useSyncplayStore((state) => state.config);
  const [inputValue, setInputValue] = useState("");
  const [hasUnreadMessages, setHasUnreadMessages] = useState(false);
  const messagesContainerRef = useRef<HTMLDivElement>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const shouldStickToBottomRef = useRef(true);
  const chatInputEnabled = config?.user.chat_input_enabled ?? true;

  const isNearBottom = () => {
    const element = messagesContainerRef.current;
    if (!element) return true;
    return element.scrollHeight - element.scrollTop - element.clientHeight < 48;
  };

  const scrollToBottom = (behavior: ScrollBehavior = "smooth") => {
    messagesEndRef.current?.scrollIntoView({ behavior });
    setHasUnreadMessages(false);
  };

  const handleMessagesScroll = () => {
    const atBottom = isNearBottom();
    shouldStickToBottomRef.current = atBottom;
    if (atBottom) {
      setHasUnreadMessages(false);
    }
  };

  useEffect(() => {
    if (shouldStickToBottomRef.current) {
      scrollToBottom("smooth");
    } else if (messages.length > 0) {
      setHasUnreadMessages(true);
    }
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

  const handleKeyDown = (event: React.KeyboardEvent) => {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      void handleSendMessage();
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
      <div
        ref={messagesContainerRef}
        onScroll={handleMessagesScroll}
        className="relative flex-1 p-5 pt-7 overflow-auto space-y-0.5"
      >
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
              <div
                key={`${msg.timestamp}-${msg.username ?? "system"}-${msg.messageType}-${index}`}
                className="text-sm app-message"
              >
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
        {hasUnreadMessages && (
          <button
            type="button"
            onClick={() => scrollToBottom()}
            className="sticky bottom-2 left-1/2 -translate-x-1/2 btn-primary px-3 py-1.5 rounded-full text-xs shadow-lg"
          >
            New messages
          </button>
        )}
      </div>

      {/* Input area */}
      <div className="border-t app-divider px-4 py-3.5 app-surface">
        <input
          type="text"
          value={inputValue}
          onChange={(e) => setInputValue(e.target.value)}
          onKeyDown={handleKeyDown}
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
