import { useState } from "react";
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

function formatElapsed(secs: number | null): string {
  if (secs == null) return "—";
  if (secs < 60) return `${secs}s`;
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins} min`;
  const hrs = Math.floor(mins / 60);
  const rem = mins % 60;
  return rem > 0 ? `${hrs}h ${rem}m` : `${hrs}h`;
}

export function WorkerDetail({ worker, detail, workspace, onBack }: Props) {
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);

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
      {/* Left: worker info */}
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
            <div className={styles.statLabel}>Duration</div>
            <div className={styles.statValue}>
              {formatElapsed(worker.elapsed_secs)}
            </div>
          </div>
          {worker.dispatched_by && (
            <div className={styles.stat}>
              <div className={styles.statLabel}>Dispatched by</div>
              <div className={styles.statValue}>{worker.dispatched_by}</div>
            </div>
          )}
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
                <span className={styles.cardLabel}>PR</span>
                <span className={styles.cardValue}>
                  <a href={worker.pr_url} target="_blank" rel="noopener noreferrer">
                    {worker.pr_title || worker.pr_url}
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

        {worker.description && (
          <div className={styles.section}>
            <div className={styles.sectionTitle}>Task</div>
            <div className={styles.taskText}>{worker.description}</div>
          </div>
        )}

        <div className={styles.section}>
          <div className={styles.sectionTitle}>Actions</div>
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

      {/* Right: worker conversation */}
      <div className={styles.chat}>
        <div className={styles.chatHeader}>
          <div className={styles.chatTitle}>Worker conversation</div>
          <div className={styles.chatSub}>
            {worker.id} · {branchName(worker.branch)}
          </div>
        </div>

        <div className={styles.messages}>
          {(!detail || detail.conversation.length === 0) && (
            <div className={styles.empty}>No messages yet</div>
          )}
          {detail?.conversation.map((msg, i) => (
            <div key={i} className={styles.msg}>
              <div className={styles.msgMeta}>
                <strong>
                  {msg.role === "system"
                    ? "Task"
                    : msg.role === "user"
                      ? "You"
                      : worker.id}
                </strong>
                {msg.timestamp && ` · ${msg.timestamp}`}
              </div>
              <div className={styles.msgText}>
                <Markdown remarkPlugins={[remarkGfm]}>{msg.content}</Markdown>
              </div>
            </div>
          ))}
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
