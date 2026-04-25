import { useRef, useEffect, useState } from "react";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { Message } from "../types";
import styles from "./ChatPanel.module.css";

export interface Attachment {
  name: string;
  type: string;
  dataUrl: string;
}

interface Props {
  bot: string;
  messages: Message[];
  loading: boolean;
  loadingStatus?: string;
  streamingContent?: string;
  workerCount?: number;
  onWorkersToggle?: () => void;
  onCancel?: () => void;
  onSend: (text: string, attachments?: Attachment[]) => void;
}

export function ChatPanel({ bot, messages, loading, loadingStatus, streamingContent, onSend, workerCount, onWorkersToggle, onCancel }: Props) {
  const bottomRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [micState, setMicState] = useState<"idle" | "recording" | "stopping" | "transcribing">("idle");
  const [transcribeError, setTranscribeError] = useState<string | null>(null);
  const mediaRecorderRef = useRef<MediaRecorder | null>(null);
  const mediaStreamRef = useRef<MediaStream | null>(null);
  const audioChunksRef = useRef<Blob[]>([]);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages.length, loading, loadingStatus]);

  useEffect(() => {
    return () => {
      mediaStreamRef.current?.getTracks().forEach((t) => t.stop());
      mediaStreamRef.current = null;
    };
  }, []);

  function send() {
    const el = textareaRef.current;
    if (!el || loading) return;
    const text = el.value.trim();
    if (!text && attachments.length === 0) return;
    el.value = "";
    el.style.height = "auto";
    onSend(text, attachments.length > 0 ? attachments : undefined);
    setAttachments([]);
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

  function handleFiles(files: FileList | null) {
    if (!files) return;
    Array.from(files).forEach((file) => {
      const reader = new FileReader();
      reader.onload = () => {
        setAttachments((prev) => [
          ...prev,
          { name: file.name, type: file.type, dataUrl: reader.result as string },
        ]);
      };
      reader.readAsDataURL(file);
    });
  }

  function removeAttachment(index: number) {
    setAttachments((prev) => prev.filter((_, i) => i !== index));
  }

  function stopStreamTracks() {
    mediaStreamRef.current?.getTracks().forEach((t) => t.stop());
    mediaStreamRef.current = null;
  }

  async function startRecording() {
    setTranscribeError(null);
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      mediaStreamRef.current = stream;
      const mediaRecorder = new MediaRecorder(stream);
      mediaRecorderRef.current = mediaRecorder;
      audioChunksRef.current = [];

      mediaRecorder.ondataavailable = (e) => {
        if (e.data.size > 0) audioChunksRef.current.push(e.data);
      };

      mediaRecorder.onstop = async () => {
        stopStreamTracks();
        const blob = new Blob(audioChunksRef.current, { type: "audio/webm" });
        await transcribeAudio(blob);
      };

      mediaRecorder.start();
      setMicState("recording");
    } catch {
      stopStreamTracks();
      setTranscribeError("Microphone access denied");
    }
  }

  function stopRecording() {
    const recorder = mediaRecorderRef.current;
    if (!recorder || recorder.state === "inactive") return;
    setMicState("stopping");
    recorder.stop();
  }

  async function transcribeAudio(blob: Blob) {
    setMicState("transcribing");
    setTranscribeError(null);
    try {
      const form = new FormData();
      form.append("audio", blob, "audio.webm");
      const res = await fetch("/api/transcribe", { method: "POST", body: form });
      if (!res.ok) {
        let msg = `Server error (${res.status})`;
        try { const data = await res.json(); if (data.error) msg = data.error; } catch {}
        setTranscribeError(msg);
        return;
      }
      const data = await res.json();
      if (data.error) {
        setTranscribeError(data.error);
      } else if (data.text) {
        const el = textareaRef.current;
        if (el) {
          const current = el.value;
          el.value = current ? current + " " + data.text : data.text;
          autoGrow();
        }
      }
    } catch {
      setTranscribeError("Transcription failed");
    } finally {
      setMicState("idle");
    }
  }

  function handleMicClick() {
    if (micState === "recording") {
      stopRecording();
    } else if (micState === "idle") {
      startRecording();
    }
  }

  function formatTime(iso: string): string {
    const d = new Date(iso);
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
        <div className={styles.headerName}>{bot}</div>
        {onWorkersToggle && (
          <button className={styles.workersBtn} onClick={onWorkersToggle}>
            {workerCount ? `${workerCount} worker${workerCount !== 1 ? "s" : ""}` : "No workers"}
          </button>
        )}
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

      <div className={styles.inputArea}>
        {attachments.length > 0 && (
          <div className={styles.attachmentPreview}>
            {attachments.map((a, i) => (
              <div key={i} className={styles.attachmentChip}>
                {a.type.startsWith("image/") ? (
                  <img src={a.dataUrl} alt={a.name} className={styles.attachmentThumb} />
                ) : (
                  <span className={styles.attachmentName}>{a.name}</span>
                )}
                <button className={styles.attachmentRemove} onClick={() => removeAttachment(i)}>
                  &times;
                </button>
              </div>
            ))}
          </div>
        )}
        <div className={styles.inputRow}>
          <input
            ref={fileInputRef}
            type="file"
            multiple
            accept="image/*,.pdf,.txt,.md,.json,.csv,.ts,.tsx,.js,.jsx,.py,.rs,.go,.rb,.swift"
            style={{ display: "none" }}
            onChange={(e) => handleFiles(e.target.files)}
          />
          <button
            type="button"
            className={styles.attachBtn}
            onClick={() => fileInputRef.current?.click()}
          >
            +
          </button>
          <button
            type="button"
            className={`${styles.micBtn} ${micState === "recording" ? styles.micRecording : ""}`}
            onClick={handleMicClick}
            disabled={loading || micState === "stopping" || micState === "transcribing"}
            title={micState === "recording" ? "Stop recording" : "Record audio"}
          >
            {micState === "transcribing" || micState === "stopping" ? "..." : micState === "recording" ? "\u25A0" : "\uD83C\uDFA4"}
          </button>
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
        {micState === "transcribing" && <div className={styles.transcribeStatus}>Transcribing...</div>}
        {transcribeError && <div className={styles.transcribeError}>{transcribeError}</div>}
      </div>
    </div>
  );
}
