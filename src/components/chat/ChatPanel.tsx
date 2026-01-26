import { useState, useRef, useEffect } from "react";
import { useSyncplayStore } from "../../store";
import { invoke } from "@tauri-apps/api";

export function ChatPanel() {
  const messages = useSyncplayStore((state) => state.messages);
  const connection = useSyncplayStore((state) => state.connection);
  const [inputValue, setInputValue] = useState("");
  const messagesEndRef = useRef<HTMLDivElement>(null);

  // Auto-scroll to bottom when new messages arrive
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  const handleSendMessage = async () => {
    if (!inputValue.trim() || !connection.connected) return;

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
    });
  };

  const getMessageStyle = (messageType: string) => {
    switch (messageType) {
      case "system":
        return "text-yellow-400 italic";
      case "error":
        return "text-red-400";
      default:
        return "text-gray-200";
    }
  };

  return (
    <div className="flex flex-col h-full">
      {/* Messages area */}
      <div className="flex-1 p-4 overflow-auto space-y-2">
        {messages.length === 0 ? (
          <p className="text-gray-400">
            {connection.connected
              ? "No messages yet. Start chatting!"
              : "Welcome to Syncplay! Connect to a server to get started."}
          </p>
        ) : (
          messages.map((msg, index) => (
            <div key={index} className="text-sm">
              <span className="text-gray-500 text-xs">
                {formatTimestamp(msg.timestamp)}
              </span>
              {msg.username && (
                <span className="text-blue-400 font-medium ml-2">
                  {msg.username}:
                </span>
              )}
              <span className={`ml-2 ${getMessageStyle(msg.messageType)}`}>
                {msg.message}
              </span>
            </div>
          ))
        )}
        <div ref={messagesEndRef} />
      </div>

      {/* Input area */}
      <div className="border-t border-gray-700 p-4">
        <input
          type="text"
          value={inputValue}
          onChange={(e) => setInputValue(e.target.value)}
          onKeyPress={handleKeyPress}
          placeholder={
            connection.connected
              ? "Type a message... (or /help for commands)"
              : "Not connected"
          }
          className="w-full bg-gray-800 text-white px-4 py-2 rounded border border-gray-700 focus:outline-none focus:border-blue-500"
          disabled={!connection.connected}
        />
      </div>
    </div>
  );
}
