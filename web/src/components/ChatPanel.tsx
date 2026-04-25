import { useRef, useEffect } from "react";
import type { Message } from "../types";
import styles from "./ChatPanel.module.css";

interface Props {
  bot: string;
  messages: Message[];
  loading: boolean;
  loadingStatus?: string;
  onSend: (text: string) => void;
}

export function ChatPanel({ bot, messages, loading, loadingStatus, onSend }: Props) {
  const bottomRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages.length, loading, loadingStatus]);

  function send() {
    const el = textareaRef.current;
    if (!el || loading) return;
    const text = el.value.trim();
    if (!text) return;
    el.value = "";
    el.style.height = "auto";
    onSend(text);
  }

  function autoGrow() {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = Math.min(el.scrollHeight, 160) + "px";
  }

  function handleKeyDown(e: React.KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  }

  function formatTime(iso: string): string {
    const d = new Date(iso);
    return d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  }

  return (
    <div className={styles.panel}>
      <div className={styles.header}>
        <div className={styles.headerName}>{bot}</div>
      </div>

      <div className={styles.messages}>
        {messages.length === 0 && !loading && (
          <div className={styles.empty}>
            Start a conversation with {bot}
          </div>
        )}
        {messages.map((msg) => (
          <div
            key={msg.id}
            className={`${styles.msg} ${msg.role === "user" ? styles.user : ""}`}
          >
            <div className={styles.meta}>
              <strong>{msg.role === "user" ? "You" : bot}</strong>
              {" · "}
              {formatTime(msg.created_at)}
            </div>
            <div className={styles.text}>{msg.content}</div>
          </div>
        ))}
        {loading && (
          <div className={styles.msg}>
            <div className={styles.meta}>
              <strong>{bot}</strong>
            </div>
            <div className={styles.thinking}>
              <span className={styles.thinkingDots}>
                <span />
                <span />
                <span />
              </span>
              {loadingStatus && (
                <span className={styles.thinkingStatus}>{loadingStatus}</span>
              )}
            </div>
          </div>
        )}
        <div ref={bottomRef} />
      </div>

      <div className={styles.inputArea}>
        <div className={styles.inputRow}>
          <textarea
            ref={textareaRef}
            className={styles.inputField}
            placeholder={loading ? `${bot} is thinking...` : `Message ${bot}...`}
            rows={1}
            readOnly={loading}
            enterKeyHint="send"
            onInput={autoGrow}
            onKeyDown={handleKeyDown}
          />
          <button
            type="button"
            className={styles.sendBtn}
            onMouseDown={(e) => e.preventDefault()}
            onClick={send}
          >
            &uarr;
          </button>
        </div>
      </div>
    </div>
  );
}
