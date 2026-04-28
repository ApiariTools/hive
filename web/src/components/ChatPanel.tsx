import { useRef, useEffect, useState, useCallback } from "react";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { ChevronDown, Loader2, Square, Volume2, AudioLines } from "lucide-react";
import { Howl, Howler } from "howler";
import type { Message } from "../types";
import { splitSentences } from "../voice";
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
  ttsVoice?: string;
  ttsSpeed?: number;
}

interface QueuedMessage {
  text: string;
  attachments?: Attachment[];
}

export function ChatPanel({ bot, botDescription, messages, messagesLoading, loading, loadingStatus, streamingContent, onSend, workerCount, onWorkersToggle, onCancel, ttsVoice, ttsSpeed }: Props) {
  const bottomRef = useRef<HTMLDivElement>(null);
  const [showScrollBtn, setShowScrollBtn] = useState(false);
  const [messageQueue, setMessageQueue] = useState<QueuedMessage[]>([]);
  const [playingId, setPlayingId] = useState<number | null>(null);
  const [loadingTtsId, setLoadingTtsId] = useState<number | null>(null);
  const [voiceMode, setVoiceMode] = useState(false);
  const [triggerRecord, setTriggerRecord] = useState(0);
  const voiceModeRef = useRef(false);
  const lastMsgCountRef = useRef(0);

  // ── Message queue ──
  const handleSendOrQueue = useCallback(
    (text: string, attachments?: Attachment[]) => {
      if (loading) {
        setMessageQueue((q) => [...q, { text, attachments }]);
      } else {
        onSend(text, attachments);
      }
    },
    [loading, onSend],
  );

  // Clear queue on bot switch so queued messages don't leak across bots
  const prevBotRef = useRef(bot);
  useEffect(() => {
    if (prevBotRef.current !== bot) {
      setMessageQueue([]);
      prevBotRef.current = bot;
    }
  }, [bot]);

  // Drain queue when bot finishes responding
  const prevLoadingRef = useRef(loading);
  useEffect(() => {
    if (prevLoadingRef.current && !loading && messageQueue.length > 0) {
      const [next, ...rest] = messageQueue;
      setMessageQueue(rest);
      onSend(next.text, next.attachments);
    }
    prevLoadingRef.current = loading;
  }, [loading, messageQueue, onSend]);

  // ── TTS playback (Howler — for user-tapped play buttons) ──
  const howlRef = useRef<Howl | null>(null);
  const sentenceQueueRef = useRef<string[]>([]);
  const readyQueueRef = useRef<Howl[]>([]);
  const playingMsgRef = useRef<number | null>(null);

  // ── Auto-scroll ──
  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
    setShowScrollBtn(false);
  }, [messages.length, loading, loadingStatus]);

  // ── Voice mode: auto-read bot responses via Howler's unlocked AudioContext ──
  // We use Howler.ctx directly (unlocked by the greeting tap) to play decoded
  // audio buffers. This avoids creating new Howl instances which re-trigger
  // iPad's gesture check.
  const autoPlayedRef = useRef<Set<number>>(new Set());
  const voiceSourceRef = useRef<AudioBufferSourceNode | null>(null);

  // Play a single TTS URL via Howler's unlocked AudioContext
  async function playViaCx(url: string, onEnd?: () => void) {
    const ctx = Howler.ctx;
    if (!ctx) { onEnd?.(); return; }
    if (ctx.state === "suspended") await ctx.resume();
    try {
      const res = await fetch(url);
      if (!res.ok) { onEnd?.(); return; }
      const arrayBuf = await res.arrayBuffer();
      const audioBuf = await ctx.decodeAudioData(arrayBuf);
      const source = ctx.createBufferSource();
      source.buffer = audioBuf;
      source.connect(ctx.destination);
      voiceSourceRef.current = source;
      source.onended = () => { voiceSourceRef.current = null; onEnd?.(); };
      source.start();
    } catch { onEnd?.(); }
  }

  async function playVoiceChain(sentences: string[], idx: number, msgId: number) {
    if (idx >= sentences.length || playingMsgRef.current !== msgId || !voiceModeRef.current) {
      stopPlaying(true);
      return;
    }
    await playViaCx(buildTtsUrl(sentences[idx]), () => playVoiceChain(sentences, idx + 1, msgId));
  }

  useEffect(() => {
    if (!voiceModeRef.current) {
      lastMsgCountRef.current = messages.length;
      return;
    }
    if (messages.length > lastMsgCountRef.current && !loading) {
      const newMsg = messages[messages.length - 1];
      if (newMsg.role === "assistant" && !autoPlayedRef.current.has(newMsg.id) && !playingMsgRef.current) {
        autoPlayedRef.current.add(newMsg.id);
        const sentences = splitSentences(newMsg.content);
        if (sentences.length === 0) return;

        setTimeout(() => {
          if (!voiceModeRef.current || playingMsgRef.current) return;
          setPlayingId(newMsg.id);
          playingMsgRef.current = newMsg.id;
          playVoiceChain(sentences, 0, newMsg.id);
        }, 200);
      }
    }
    lastMsgCountRef.current = messages.length;
  }, [messages.length, loading]);

  // ── Cleanup ──
  useEffect(() => {
    return () => {
      if (howlRef.current) { howlRef.current.unload(); howlRef.current = null; }
    };
  }, []);

  // ── TTS controls ──

  function stopPlaying(natural = false) {
    // Stop Howler playback
    sentenceQueueRef.current = [];
    playingMsgRef.current = null;
    if (howlRef.current) { howlRef.current.stop(); howlRef.current.unload(); howlRef.current = null; }
    for (const h of readyQueueRef.current) h.unload();
    readyQueueRef.current = [];
    // Stop voice chain playback
    if (voiceSourceRef.current) {
      try { voiceSourceRef.current.stop(); } catch {}
      voiceSourceRef.current = null;
    }
    speechSynthesis.cancel();

    setPlayingId(null);
    setLoadingTtsId(null);

    if (natural && voiceModeRef.current) {
      setTriggerRecord((n) => n + 1);
    }
  }

  function buildTtsUrl(sentence: string): string {
    const params = new URLSearchParams({ text: sentence });
    if (ttsVoice) params.set("voice", ttsVoice);
    if (ttsSpeed) params.set("speed", String(ttsSpeed));
    return `/api/tts/speak?${params.toString()}`;
  }

  // Chunked Howler playback — used by play button (has user gesture)
  function enqueueGeneration() {
    const queue = sentenceQueueRef.current;
    if (queue.length === 0 || playingMsgRef.current === null) return;

    const sentence = queue.shift()!;
    const howl = new Howl({
      src: [buildTtsUrl(sentence)],
      format: ["wav"],
      html5: true,
      preload: true,
      onload: () => {
        if (playingMsgRef.current === null) { howl.unload(); return; }
        readyQueueRef.current.push(howl);
        if (!howlRef.current) playFromReady();
      },
      onloaderror: () => { if (!howlRef.current) stopPlaying(); },
    });
  }

  function playFromReady() {
    if (readyQueueRef.current.length === 0) {
      if (sentenceQueueRef.current.length === 0) stopPlaying(true);
      return;
    }

    const howl = readyQueueRef.current.shift()!;
    howlRef.current = howl;

    howl.on("play", () => { setLoadingTtsId(null); enqueueGeneration(); });
    howl.on("end", () => { howlRef.current = null; playFromReady(); });
    howl.on("playerror", () => stopPlaying());

    howl.play();
  }

  function playMessage(msg: Message) {
    if (playingId === msg.id || loadingTtsId === msg.id) { stopPlaying(); return; }
    stopPlaying();

    const sentences = splitSentences(msg.content);
    if (sentences.length === 0) return;

    setLoadingTtsId(msg.id);
    setPlayingId(msg.id);
    playingMsgRef.current = msg.id;
    sentenceQueueRef.current = sentences;
    enqueueGeneration();
    enqueueGeneration();
  }

  // ── Voice mode toggle ──

  const keepAliveRef = useRef<ReturnType<typeof setInterval> | null>(null);

  function toggleVoiceMode() {
    if (voiceMode) {
      voiceModeRef.current = false;
      setVoiceMode(false);
      stopPlaying();
      // Stop keep-alive
      if (keepAliveRef.current) { clearInterval(keepAliveRef.current); keepAliveRef.current = null; }
    } else {
      // Play greeting via Howler (user gesture context — unlocks AudioContext on iPad)
      const params = new URLSearchParams({ text: "Voice mode on." });
      if (ttsVoice) params.set("voice", ttsVoice);
      if (ttsSpeed) params.set("speed", String(ttsSpeed));
      new Howl({ src: [`/api/tts/speak?${params.toString()}`], format: ["wav"], html5: true }).play();

      // Keep Howler's AudioContext alive by playing silent audio every 4 seconds
      if (keepAliveRef.current) clearInterval(keepAliveRef.current);
      keepAliveRef.current = setInterval(() => {
        const ctx = Howler.ctx;
        if (ctx && ctx.state === "suspended") ctx.resume();
        if (ctx && ctx.state === "running") {
          const buf = ctx.createBuffer(1, 1, 22050);
          const src = ctx.createBufferSource();
          src.buffer = buf;
          src.connect(ctx.destination);
          src.start();
        }
      }, 4000);

      voiceModeRef.current = true;
      setVoiceMode(true);
      lastMsgCountRef.current = messages.length;
      setTriggerRecord((n) => n + 1);
      setTimeout(() => bottomRef.current?.scrollIntoView({ behavior: "smooth" }), 100);
    }
  }

  // ── Helpers ──

  function handleMessagesScroll(e: React.UIEvent<HTMLDivElement>) {
    const el = e.currentTarget;
    setShowScrollBtn(el.scrollHeight - el.scrollTop - el.clientHeight > 40);
  }

  function scrollToBottom() {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }

  function formatTime(iso: string): string {
    const normalized = iso.includes("Z") || iso.includes("+") ? iso : iso + "Z";
    return new Date(normalized).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
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
    } catch { return null; }
  }

  // ── Render ──

  return (
    <div className={styles.panel}>
      <div className={styles.header}>
        <div className={styles.headerInfo}>
          <div className={styles.headerName}>{bot}</div>
          {botDescription && <div className={styles.headerDescription}>{botDescription}</div>}
        </div>
        <div className={styles.headerActions}>
          <button
            className={`${styles.voiceModeBtn} ${voiceMode ? styles.voiceModeActive : ""}`}
            onClick={toggleVoiceMode}
            aria-label={voiceMode ? "Exit voice mode" : "Enter voice mode"}
          >
            <AudioLines size={16} />
          </button>
          {onWorkersToggle && (
            <button className={styles.workersBtn} onClick={onWorkersToggle}>
              {workerCount ? `${workerCount} worker${workerCount !== 1 ? "s" : ""}` : "No workers"}
            </button>
          )}
        </div>
      </div>

      <div className={styles.messagesWrap}>
      <div className={styles.messages} onScroll={handleMessagesScroll}>
        {messagesLoading && messages.length === 0 && (
          <div className={styles.empty}>Loading...</div>
        )}
        {!messagesLoading && messages.length === 0 && !loading && (
          <div className={styles.empty}>Start a conversation with {bot}</div>
        )}
        {messages.map((msg) => (
          <div key={msg.id} className={`${styles.msg} ${msg.role === "user" ? styles.user : ""}`}>
            <div className={styles.meta}>
              <strong>{msg.role === "user" ? "You" : bot}</strong>
              {" · "}
              {formatTime(msg.created_at)}
              {msg.role === "assistant" && (
                <button
                  className={`${styles.playBtn} ${playingId === msg.id ? styles.playBtnActive : ""}`}
                  onClick={() => playMessage(msg)}
                  aria-label={playingId === msg.id ? "Stop" : loadingTtsId === msg.id ? "Loading" : "Play"}
                >
                  {loadingTtsId === msg.id ? (
                    <Loader2 size={12} className={styles.ttsSpinner} />
                  ) : playingId === msg.id ? (
                    <Square size={12} />
                  ) : (
                    <Volume2 size={12} />
                  )}
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
              {onCancel && <button className={styles.cancelBtn} onClick={onCancel}>Stop</button>}
            </div>
            {streamingContent ? (
              <>
                <div className={styles.text}>
                  <Markdown remarkPlugins={[remarkGfm]}>{streamingContent}</Markdown>
                </div>
                <div className={styles.streamingIndicator}>
                  <span className={styles.thinkingDots}><span /><span /><span /></span>
                  {loadingStatus && <span className={styles.thinkingStatus}>{loadingStatus}</span>}
                </div>
              </>
            ) : (
              <div className={styles.thinking}>
                <span className={styles.thinkingDots}><span /><span /><span /></span>
                {loadingStatus && <span className={styles.thinkingStatus}>{loadingStatus}</span>}
              </div>
            )}
          </div>
        )}
        <div ref={bottomRef} style={{ paddingBottom: voiceMode ? 100 : 0 }} />
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
        placeholder={`Message ${bot}...`}
        disabled={loading}
        onSend={handleSendOrQueue}
        voiceMode={voiceMode}
        triggerRecord={triggerRecord}
        playTts={voiceMode ? playViaCx : undefined}
        queueCount={messageQueue.length}
      />
    </div>
  );
}
