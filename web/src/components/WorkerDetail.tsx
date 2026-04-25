import { useState, useRef, useEffect } from "react";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { Worker, WorkerDetail as WorkerDetailData } from "../types";
import * as api from "../api";
import styles from "./WorkerDetail.module.css";

interface Props {
  worker: Worker;
  detail: WorkerDetailData | null;
  workspace: string;
  onBack: () => void;
}

function branchName(branch: string): string {
  return branch.replace(/^swarm\//, "");
}

export function WorkerDetail({ worker, detail, workspace, onBack }: Props) {
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const bottomRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [detail?.conversation.length]);

  async function handleSend() {
    const text = input.trim();
    if (!text || sending) return;
    setSending(true);
    setInput("");
    await api.sendWorkerMessage(workspace, worker.id, text);
    setSending(false);
  }

  return (
    <div className={styles.layout}>
      {/* Left: task info */}
      <div className={styles.info}>
        <button className={styles.back} onClick={onBack}>
          &larr; Back
        </button>

        <h1 className={styles.title}>{worker.id}</h1>
        <div className={styles.subtitle}>{branchName(worker.branch)}</div>

        <div className={styles.stats}>
          <div className={styles.stat}>
            <div className={styles.statLabel}>Status</div>
            <div className={styles.statValue}>
              <span
                className={`${styles.statusDot} ${worker.status === "running" || worker.status === "active" ? styles.running : ""}`}
                style={{
                  background:
                    worker.status === "running" || worker.status === "active"
                      ? "var(--green)"
                      : worker.status === "waiting"
                        ? "var(--accent)"
                        : "var(--text-faint)",
                }}
              />
              {worker.status}
            </div>
          </div>
          <div className={styles.stat}>
            <div className={styles.statLabel}>Agent</div>
            <div className={styles.statValue}>{worker.agent}</div>
          </div>
        </div>

        {worker.pr_url && (
          <div className={styles.section}>
            <div className={styles.sectionTitle}>Pull Request</div>
            <div className={styles.card}>
              <div className={styles.cardRow}>
                <span className={styles.cardValue}>
                  <a href={worker.pr_url} target="_blank" rel="noopener noreferrer">
                    {worker.pr_title || "View PR"}
                  </a>
                </span>
              </div>
            </div>
          </div>
        )}

        {detail?.output && (
          <div className={styles.section}>
            <div className={styles.sectionTitle}>Output</div>
            <div className={styles.outputText}>
              <Markdown remarkPlugins={[remarkGfm]}>{detail.output}</Markdown>
            </div>
          </div>
        )}

        {detail?.prompt && (
          <div className={styles.section}>
            <div className={styles.sectionTitle}>Task Prompt</div>
            <div className={styles.taskText}>
              <Markdown remarkPlugins={[remarkGfm]}>{detail.prompt}</Markdown>
            </div>
          </div>
        )}

        <div className={styles.section}>
          <div className={styles.actions}>
            {worker.pr_url && (
              <a
                href={worker.pr_url}
                target="_blank"
                rel="noopener noreferrer"
                className={`${styles.btn} ${styles.btnPrimary}`}
              >
                View PR
              </a>
            )}
            <button className={`${styles.btn} ${styles.btnDanger}`}>
              Close worker
            </button>
          </div>
        </div>
      </div>

      {/* Right: agent conversation */}
      <div className={styles.chat}>
        <div className={styles.chatHeader}>
          <div className={styles.chatTitle}>Conversation</div>
        </div>

        <div className={styles.messages}>
          {(!detail || detail.conversation.length === 0) && (
            <div className={styles.empty}>No conversation data available</div>
          )}
          {detail?.conversation.map((msg, i) => (
            <div key={i} className={`${styles.msg} ${msg.role === "user" ? styles.userMsg : ""}`}>
              <div className={styles.msgMeta}>
                <strong>{msg.role === "user" ? "You" : worker.id}</strong>
              </div>
              <div className={styles.msgText}>
                <Markdown remarkPlugins={[remarkGfm]}>{msg.content}</Markdown>
              </div>
            </div>
          ))}
          <div ref={bottomRef} />
        </div>

        <div className={styles.inputArea}>
          <div className={styles.inputRow}>
            <input
              className={styles.inputField}
              placeholder="Message worker..."
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") handleSend();
              }}
            />
            <button
              className={styles.sendBtn}
              onClick={handleSend}
              disabled={sending}
            >
              &uarr;
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
