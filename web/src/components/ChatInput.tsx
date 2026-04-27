import { useRef, useEffect, useState } from "react";
import { Mic, Square, Paperclip, ArrowUp } from "lucide-react";
import styles from "./ChatInput.module.css";

export interface Attachment {
  name: string;
  type: string;
  dataUrl: string;
}

interface Props {
  placeholder: string;
  disabled?: boolean;
  onSend: (text: string, attachments?: Attachment[]) => void;
  showAttachments?: boolean;
}

export function ChatInput({ placeholder, disabled, onSend, showAttachments = true }: Props) {
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [micState, setMicState] = useState<"idle" | "recording" | "stopping" | "transcribing">("idle");
  const [hasText, setHasText] = useState(false);
  const [transcribeError, setTranscribeError] = useState<string | null>(null);
  const longPressTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const didLongPress = useRef(false);
  const mediaRecorderRef = useRef<MediaRecorder | null>(null);
  const mediaStreamRef = useRef<MediaStream | null>(null);
  const audioChunksRef = useRef<Blob[]>([]);
  const analyserRef = useRef<AnalyserNode | null>(null);
  const audioCtxRef = useRef<AudioContext | null>(null);
  const animFrameRef = useRef<number>(0);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const smoothedBars = useRef<number[]>([]);

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
      audioCtxRef.current?.close();
      audioCtxRef.current = null;
      if (longPressTimer.current) {
        clearTimeout(longPressTimer.current);
        longPressTimer.current = null;
      }
    };
  }, []);

  function send() {
    const el = textareaRef.current;
    if (!el || disabled) return;
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
    audioCtxRef.current?.close();
    audioCtxRef.current = null;
  }

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

    const freqData = new Uint8Array(analyser.frequencyBinCount);
    analyser.getByteFrequencyData(freqData);

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

    for (let i = 0; i < halfCount; i++) {
      const barH = Math.max(4, 4 + smoothedBars.current[i] * (h - 8));

      const rightIdx = halfCount + i;
      const xRight = rightIdx * (barWidth + gap);
      ctx.fillStyle = "#e85555";
      ctx.beginPath();
      ctx.roundRect(xRight, centerY - barH / 2, barWidth, barH, 2);
      ctx.fill();

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

      audioCtxRef.current?.close();
      const audioCtx = new AudioContext();
      audioCtxRef.current = audioCtx;
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

  return (
    <div className={styles.inputArea}>
      {micState === "recording" && (
        <canvas ref={canvasRef} className={styles.waveform} />
      )}
      {showAttachments && attachments.length > 0 && (
        <div className={styles.attachmentPreview}>
          {attachments.map((a, i) => (
            <div key={i} className={styles.attachmentChip}>
              {a.type.startsWith("image/") ? (
                <img src={a.dataUrl} alt={a.name} className={styles.attachmentThumb} />
              ) : (
                <span className={styles.attachmentName}>{a.name}</span>
              )}
              <button
                type="button"
                className={styles.attachmentRemove}
                aria-label={`Remove ${a.name}`}
                onClick={() => removeAttachment(i)}
              >
                &times;
              </button>
            </div>
          ))}
        </div>
      )}
      <div className={styles.inputRow}>
        {showAttachments && (
          <>
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
              aria-label="Attach file"
              onClick={() => fileInputRef.current?.click()}
            >
              <Paperclip size={16} />
            </button>
          </>
        )}
        <textarea
          ref={textareaRef}
          className={styles.inputField}
          placeholder={placeholder}
          rows={1}
          readOnly={disabled}
          enterKeyHint="send"
          onInput={autoGrow}
          onKeyDown={handleKeyDown}
        />
        {micState === "recording" ? (
          <button
            type="button"
            className={`${styles.actionBtn} ${styles.micRecording}`}
            aria-label="Stop recording"
            onClick={stopRecording}
            onTouchEnd={(e) => { e.preventDefault(); stopRecording(); }}
          >
            <Square size={16} />
          </button>
        ) : micState === "transcribing" || micState === "stopping" ? (
          <button type="button" className={styles.actionBtn} aria-label="Transcribing" disabled>
            ...
          </button>
        ) : (
          <button
            type="button"
            className={`${styles.actionBtn} ${hasText || attachments.length > 0 ? styles.actionBtnSend : ""}`}
            aria-label={hasText || attachments.length > 0 ? "Send message" : "Record voice"}
            disabled={disabled}
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
  );
}
