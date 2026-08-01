import { useState, useRef, useEffect } from "react";
import { useSyncplayStore } from "../../store";
import { invoke } from "@tauri-apps/api/core";

type ChatFilter = "all" | "chat" | "events";

const CHAT_FILTER_STORAGE_KEY = "syncplay.chatFilter";

const loadChatFilter = (): ChatFilter => {
  const stored = window.localStorage.getItem(CHAT_FILTER_STORAGE_KEY);
  return stored === "chat" || stored === "events" ? stored : "all";
};

interface DisplayMessage {
  collapseKey: string | null;
  timestamp: string;
  username: string | null;
  message: string;
  messageType: string;
  count: number;
}

// Sync events look like "haruru paused at 17:38" / "haruru unpaused" /
// "haruru jumped from 17:47 to 17:38". Collapse runs of the same
// actor + action into one line with a xN badge.
const SYNC_EVENT_PATTERN = /^(\S+)\s+(paused|unpaused|jumped)\b/i;

const collapseKeyFor = (msg: {
  message: string;
  messageType: string;
}): string | null => {
  if (msg.messageType !== "system") return null;
  const match = msg.message.trim().match(SYNC_EVENT_PATTERN);
  return match ? `${match[1].toLowerCase()}:${match[2].toLowerCase()}` : null;
};

const collapseMessages = (
  list: { timestamp: string; username: string | null; message: string; messageType: string }[]
): DisplayMessage[] => {
  const result: DisplayMessage[] = [];
  for (const msg of list) {
    const collapseKey = collapseKeyFor(msg);
    const last = result[result.length - 1];
    if (collapseKey !== null && last && last.collapseKey === collapseKey) {
      last.message = msg.message;
      last.timestamp = msg.timestamp;
      last.count += 1;
      continue;
    }
    result.push({
      collapseKey,
      timestamp: msg.timestamp,
      username: msg.username,
      message: msg.message,
      messageType: msg.messageType,
      count: 1,
    });
  }
  return result;
};

const FILTER_TABS: { value: ChatFilter; label: string }[] = [
  { value: "all", label: "All" },
  { value: "chat", label: "Chat" },
  { value: "events", label: "Events" },
];

export function ChatPanel() {
  const messages = useSyncplayStore((state) => state.messages);
  const connection = useSyncplayStore((state) => state.connection);
  const config = useSyncplayStore((state) => state.config);
  const [inputValue, setInputValue] = useState("");
  const [filter, setFilter] = useState<ChatFilter>(loadChatFilter);
  const [hasUnreadMessages, setHasUnreadMessages] = useState(false);
  const messagesContainerRef = useRef<HTMLDivElement>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const shouldStickToBottomRef = useRef(true);
  const chatInputEnabled = config?.user.chat_input_enabled ?? true;

  const handleFilterChange = (next: ChatFilter) => {
    setFilter(next);
    window.localStorage.setItem(CHAT_FILTER_STORAGE_KEY, next);
  };

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

  const visibleMessages = messages.filter((msg) => {
    if (filter === "chat") return msg.messageType !== "system";
    if (filter === "events") return msg.messageType === "system";
    return true;
  });
  const displayMessages = collapseMessages(visibleMessages);

  const emptyHint = !connection.connected
    ? "Welcome to Syncplay! Connect to a server to get started."
    : filter === "chat"
      ? "No chat messages yet. Start chatting!"
      : filter === "events"
        ? "No sync events yet."
        : "No messages yet. Start chatting!";

  return (
    <div className="flex flex-col h-full">
      {/* Filter tabs — extra top padding clears the macOS traffic lights
          that float over the overlay titlebar area. */}
      <div className="flex items-center gap-4 px-5 pt-8 pb-0 border-b app-divider app-surface">
        {FILTER_TABS.map((tab) => (
          <button
            key={tab.value}
            type="button"
            onClick={() => handleFilterChange(tab.value)}
            className={`app-tab px-1 pb-2 text-xs ${
              filter === tab.value ? "app-tab-active app-text-accent" : ""
            }`}
          >
            {tab.label}
          </button>
        ))}
      </div>

      {/* Messages area */}
      <div
        ref={messagesContainerRef}
        onScroll={handleMessagesScroll}
        className="relative flex-1 p-5 pt-4 overflow-auto space-y-0.5"
      >
        {displayMessages.length === 0 ? (
          <p className="app-text-muted">{emptyHint}</p>
        ) : (
          displayMessages.map((msg, index) => {
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
                {msg.count > 1 && (
                  <span
                    className="ml-2 text-[10px] px-1.5 py-0 rounded-full app-tag-muted"
                    aria-label={`Repeated ${msg.count} times`}
                  >
                    ×{msg.count}
                  </span>
                )}
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
