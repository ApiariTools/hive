import { useRef, useEffect, useState } from "react";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Mic, Square, Paperclip, ArrowUp, ChevronDown, Volume2 } from "lucide-react";
import * as api from "../api";
import type { Message } from "../types";
import styles from "./ChatPanel.module.css";

export interface Attachment {
  name: string;
  type: string;
  dataUrl: string;
}

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
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [micState, setMicState] = useState<"idle" | "recording" | "stopping" | "transcribing">("idle");
  const [hasText, setHasText] = useState(false);
  const [showScrollBtn, setShowScrollBtn] = useState(false);
  const [transcribeError, setTranscribeError] = useState<string | null>(null);
  const longPressTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const didLongPress = useRef(false);
  const mediaRecorderRef = useRef<MediaRecorder | null>(null);
  const mediaStreamRef = useRef<MediaStream | null>(null);
  const audioChunksRef = useRef<Blob[]>([]);
  const analyserRef = useRef<AnalyserNode | null>(null);
  const animFrameRef = useRef<number>(0);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
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

  // Start/stop waveform drawing when recording state changes
  useEffect(() => {
    if (micState === "recording" && analyserRef.current) {
      animFrameRef.current = requestAnimationFrame(drawWaveform);
    }
    return () => {
      if (animFrameRef.current) cancelAnimationFrame(animFrameRef.current);
    };
  }, [micState]);

  useEffect(() => {
    return () => {
      mediaStreamRef.current?.getTracks().forEach((t) => t.stop());
      mediaStreamRef.current = null;
    };
  }, []);

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

  function send() {
    const el = textareaRef.current;
    if (!el || loading) return;
    const text = el.value.trim();
    if (!text && attachments.length === 0) return;
    el.value = "";
    el.style.height = "auto";
    setHasText(false);
    onSend(text, attachments.length > 0 ? attachments : undefined);
    setAttachments([]);
  }

  function autoGrow() {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = Math.min(el.scrollHeight, 160) + "px";
    setHasText(el.value.trim().length > 0);
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
    if (animFrameRef.current) cancelAnimationFrame(animFrameRef.current);
    analyserRef.current = null;
  }

  const smoothedBars = useRef<number[]>([]);

  function drawWaveform() {
    const canvas = canvasRef.current;
    const analyser = analyserRef.current;
    if (!canvas || !analyser) return;

    const rect = canvas.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    canvas.width = rect.width * dpr;
    canvas.height = rect.height * dpr;

    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.scale(dpr, dpr);

    const w = rect.width;
    const h = rect.height;

    // Use frequency data for bars instead of waveform
    const freqData = new Uint8Array(analyser.frequencyBinCount);
    analyser.getByteFrequencyData(freqData);

    // 12 unique bars, mirrored for 24 total (symmetric from center)
    const halfCount = 12;
    const usableBins = Math.floor(freqData.length * 0.4);

    if (smoothedBars.current.length !== halfCount) {
      smoothedBars.current = new Array(halfCount).fill(0);
    }

    ctx.clearRect(0, 0, w, h);

    const totalBars = halfCount * 2;
    const gap = 3;
    const barWidth = (w - gap * (totalBars - 1)) / totalBars;
    const centerY = h / 2;

    // Compute the 12 unique values
    for (let i = 0; i < halfCount; i++) {
      const startBin = Math.floor(Math.pow(i / halfCount, 1.5) * usableBins);
      const endBin = Math.floor(Math.pow((i + 1) / halfCount, 1.5) * usableBins);
      const binCount = Math.max(1, endBin - startBin);

      let sum = 0;
      for (let j = startBin; j < startBin + binCount; j++) {
        sum += freqData[j];
      }
      const raw = sum / binCount / 255;

      const gated = Math.max(0, raw - 0.2);
      const scaled = Math.pow(gated / 0.8, 0.9) * 1.4;
      const target = Math.min(scaled, 1.0);

      const prev = smoothedBars.current[i];
      smoothedBars.current[i] = target > prev
        ? prev + (target - prev) * 0.7
        : prev + (target - prev) * 0.25;
    }

    // Draw mirrored: center out
    for (let i = 0; i < halfCount; i++) {
      const barH = Math.max(4, 4 + smoothedBars.current[i] * (h - 8));

      // Right half (from center)
      const rightIdx = halfCount + i;
      const xRight = rightIdx * (barWidth + gap);
      ctx.fillStyle = "#e85555";
      ctx.beginPath();
      ctx.roundRect(xRight, centerY - barH / 2, barWidth, barH, 2);
      ctx.fill();

      // Left half (mirrored)
      const leftIdx = halfCount - 1 - i;
      const xLeft = leftIdx * (barWidth + gap);
      ctx.beginPath();
      ctx.roundRect(xLeft, centerY - barH / 2, barWidth, barH, 2);
      ctx.fill();
    }

    animFrameRef.current = requestAnimationFrame(drawWaveform);
  }

  async function startRecording() {
    setTranscribeError(null);
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      mediaStreamRef.current = stream;

      // Set up audio analyser for waveform
      const audioCtx = new AudioContext();
      const source = audioCtx.createMediaStreamSource(stream);
      const analyser = audioCtx.createAnalyser();
      analyser.fftSize = 256;
      source.connect(analyser);
      analyserRef.current = analyser;

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

      <div className={styles.inputArea}>
        {micState === "recording" && (
          <canvas ref={canvasRef} className={styles.waveform} />
        )}
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
            <Paperclip size={16} />
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
          {micState === "recording" ? (
            <button
              type="button"
              className={`${styles.actionBtn} ${styles.micRecording}`}
              onClick={stopRecording}
              onTouchEnd={(e) => { e.preventDefault(); stopRecording(); }}
            >
              <Square size={16} />
            </button>
          ) : micState === "transcribing" || micState === "stopping" ? (
            <button type="button" className={styles.actionBtn} disabled>
              ...
            </button>
          ) : (
            <button
              type="button"
              className={`${styles.actionBtn} ${hasText || attachments.length > 0 ? styles.actionBtnSend : ""}`}
              disabled={loading}
              onMouseDown={(e) => {
                e.preventDefault();
                didLongPress.current = false;
                longPressTimer.current = setTimeout(() => {
                  didLongPress.current = true;
                  startRecording();
                }, 500);
              }}
              onMouseUp={() => {
                if (longPressTimer.current) { clearTimeout(longPressTimer.current); longPressTimer.current = null; }
                if (didLongPress.current) { stopRecording(); return; }
                if (hasText || attachments.length > 0) { send(); } else { startRecording(); }
              }}
              onMouseLeave={() => {
                if (longPressTimer.current) { clearTimeout(longPressTimer.current); longPressTimer.current = null; }
              }}
              onTouchStart={() => {
                didLongPress.current = false;
                longPressTimer.current = setTimeout(() => {
                  didLongPress.current = true;
                  startRecording();
                }, 500);
              }}
              onTouchEnd={(e) => {
                e.preventDefault();
                if (longPressTimer.current) { clearTimeout(longPressTimer.current); longPressTimer.current = null; }
                if (didLongPress.current) { stopRecording(); return; }
                if (hasText || attachments.length > 0) { send(); } else { startRecording(); }
              }}
            >
              {hasText || attachments.length > 0 ? <ArrowUp size={18} /> : <Mic size={16} />}
            </button>
          )}
        </div>
        {micState === "transcribing" && <div className={styles.transcribeStatus}>Transcribing...</div>}
        {transcribeError && <div className={styles.transcribeError}>{transcribeError}</div>}
      </div>
    </div>
  );
}
