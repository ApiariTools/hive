import { useRef, useEffect, useState } from "react";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { ChevronDown, Square, Volume2 } from "lucide-react";
import * as api from "../api";
import type { Message } from "../types";
import { ChatInput } from "./ChatInput";
import type { Attachment } from "./ChatInput";
import styles from "./ChatPanel.module.css";

export type { Attachment };

interface Props {
  bot: string;
  botDescription?: string;
  messages: Message[];
  messagesLoading: boolean;
  loading: boolean;
  loadingStatus?: string;
  streamingContent?: string;
  workerCount?: number;
  onWorkersToggle?: () => void;
  onCancel?: () => void;
  onSend: (text: string, attachments?: Attachment[]) => void;
}

export function ChatPanel({ bot, botDescription, messages, messagesLoading, loading, loadingStatus, streamingContent, onSend, workerCount, onWorkersToggle, onCancel }: Props) {
  const bottomRef = useRef<HTMLDivElement>(null);
  const [showScrollBtn, setShowScrollBtn] = useState(false);
  const [playingId, setPlayingId] = useState<number | null>(null);
  const ttsSourceRef = useRef<AudioBufferSourceNode | null>(null);
  const ttsAudioCtxRef = useRef<AudioContext | null>(null);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
    setShowScrollBtn(false);
  }, [messages.length, loading, loadingStatus]);

  function handleMessagesScroll(e: React.UIEvent<HTMLDivElement>) {
    const el = e.currentTarget;
    const distanceFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    setShowScrollBtn(distanceFromBottom > 40);
  }

  function scrollToBottom() {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }

  useEffect(() => {
    return () => {
      if (ttsSourceRef.current) {
        ttsSourceRef.current.stop();
        ttsSourceRef.current = null;
      }
      if (ttsAudioCtxRef.current) {
        ttsAudioCtxRef.current.close();
        ttsAudioCtxRef.current = null;
      }
    };
  }, []);

  async function playMessage(msg: Message) {
    if (ttsSourceRef.current) {
      ttsSourceRef.current.stop();
      ttsSourceRef.current = null;
    }
    if (playingId === msg.id) {
      setPlayingId(null);
      return;
    }
    const audioData = await api.textToSpeech(msg.content);
    if (!audioData) return;
    if (!ttsAudioCtxRef.current) {
      ttsAudioCtxRef.current = new AudioContext();
    }
    const audioCtx = ttsAudioCtxRef.current;
    const buffer = await audioCtx.decodeAudioData(audioData);
    const source = audioCtx.createBufferSource();
    source.buffer = buffer;
    source.connect(audioCtx.destination);
    source.onended = () => {
      setPlayingId(null);
      ttsSourceRef.current = null;
    };
    ttsSourceRef.current = source;
    setPlayingId(msg.id);
    source.start();
  }

  function formatTime(iso: string): string {
    // Append Z if no timezone indicator — old DB rows lack it
    const normalized = iso.includes("Z") || iso.includes("+") ? iso : iso + "Z";
    const d = new Date(normalized);
    return d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  }

  function renderAttachments(json: string | null) {
    if (!json) return null;
    try {
      const atts: Attachment[] = JSON.parse(json);
      return (
        <div className={styles.msgAttachments}>
          {atts.map((a, i) =>
            a.type.startsWith("image/") ? (
              <img key={i} src={a.dataUrl} alt={a.name} className={styles.msgImage} />
            ) : (
              <div key={i} className={styles.msgFile}>{a.name}</div>
            ),
          )}
        </div>
      );
    } catch {
      return null;
    }
  }

  return (
    <div className={styles.panel}>
      <div className={styles.header}>
        <div className={styles.headerInfo}>
          <div className={styles.headerName}>{bot}</div>
          {botDescription && (
            <div className={styles.headerDescription}>{botDescription}</div>
          )}
        </div>
        {onWorkersToggle && (
          <button className={styles.workersBtn} onClick={onWorkersToggle}>
            {workerCount ? `${workerCount} worker${workerCount !== 1 ? "s" : ""}` : "No workers"}
          </button>
        )}
      </div>

      <div className={styles.messagesWrap}>
      <div className={styles.messages} onScroll={handleMessagesScroll}>
        {messagesLoading && messages.length === 0 && (
          <div className={styles.empty}>Loading...</div>
        )}
        {!messagesLoading && messages.length === 0 && !loading && (
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
              {msg.role === "assistant" && (
                <button
                  className={styles.playBtn}
                  onClick={() => playMessage(msg)}
                  aria-label={playingId === msg.id ? "Stop" : "Play"}
                >
                  {playingId === msg.id ? <Square size={12} /> : <Volume2 size={12} />}
                </button>
              )}
            </div>
            {renderAttachments(msg.attachments)}
            <div className={styles.text}>
              {msg.role === "assistant" ? (
                <Markdown remarkPlugins={[remarkGfm]}>{msg.content}</Markdown>
              ) : (
                msg.content
              )}
            </div>
          </div>
        ))}
        {loading && (
          <div className={styles.msg}>
            <div className={styles.meta}>
              <strong>{bot}</strong>
              {onCancel && (
                <button className={styles.cancelBtn} onClick={onCancel}>Stop</button>
              )}
            </div>
            {streamingContent ? (
              <>
                <div className={styles.text}>
                  <Markdown remarkPlugins={[remarkGfm]}>{streamingContent}</Markdown>
                </div>
                <div className={styles.streamingIndicator}>
                  <span className={styles.thinkingDots}>
                    <span />
                    <span />
                    <span />
                  </span>
                  {loadingStatus && (
                    <span className={styles.thinkingStatus}>{loadingStatus}</span>
                  )}
                </div>
              </>
            ) : (
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
            )}
          </div>
        )}
        <div ref={bottomRef} />
      </div>
      <button
        className={`${styles.scrollToBottom} ${showScrollBtn ? styles.scrollToBottomVisible : ""}`}
        onClick={scrollToBottom}
        aria-label="Scroll to bottom"
        tabIndex={showScrollBtn ? 0 : -1}
        aria-hidden={!showScrollBtn}
        disabled={!showScrollBtn}
      >
        <ChevronDown size={20} />
      </button>
      </div>

      <ChatInput
        placeholder={loading ? `${bot} is thinking...` : `Message ${bot}...`}
        disabled={loading}
        onSend={onSend}
      />
    </div>
  );
}
