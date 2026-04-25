import { useState, useRef, useEffect } from "react";
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
  const [input, setInput] = useState("");
  const bottomRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages.length, loading, loadingStatus]);

  function handleSubmit() {
    const text = input.trim();
    if (!text || loading) return;
    setInput("");
    if (textareaRef.current) {
      textareaRef.current.style.height = "auto";
    }
    onSend(text);
  }

  function autoGrow(el: HTMLTextAreaElement) {
    el.style.height = "auto";
    el.style.height = Math.min(el.scrollHeight, 160) + "px";
  }

  function handleKeyDown(e: React.KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSubmit();
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
            value={input}
            rows={1}
            enterKeyHint="send"
            onChange={(e) => {
              setInput(e.target.value);
              autoGrow(e.target);
            }}
            onKeyDown={handleKeyDown}
          />
          <button
            type="button"
            className={styles.sendBtn}
            onClick={handleSubmit}
          >
            &uarr;
          </button>
        </div>
      </div>
    </div>
  );
}
