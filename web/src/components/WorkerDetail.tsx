import { useState } from "react";
import type { Worker, Message } from "../types";
import styles from "./WorkerDetail.module.css";

interface Props {
  worker: Worker;
  messages: Message[];
  onBack: () => void;
  onSend: (text: string) => void;
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

export function WorkerDetail({ worker, messages, onBack, onSend }: Props) {
  const [input, setInput] = useState("");

  function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    const text = input.trim();
    if (!text) return;
    setInput("");
    onSend(text);
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
              <button className={`${styles.btn} ${styles.btnPrimary}`}>
                Merge PR
              </button>
            )}
            <button className={styles.btn}>Message worker</button>
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
          {messages.length === 0 && (
            <div className={styles.empty}>No messages yet</div>
          )}
          {messages.map((msg) => (
            <div key={msg.id} className={styles.msg}>
              <div className={styles.msgMeta}>
                <strong>{msg.role === "user" ? "You" : msg.bot}</strong>
                {" · "}
                {new Date(msg.created_at).toLocaleTimeString([], {
                  hour: "numeric",
                  minute: "2-digit",
                })}
              </div>
              <div className={styles.msgText}>{msg.content}</div>
            </div>
          ))}
        </div>

        <form className={styles.inputArea} onSubmit={handleSubmit}>
          <div className={styles.inputRow}>
            <input
              className={styles.inputField}
              placeholder="Message worker..."
              value={input}
              onChange={(e) => setInput(e.target.value)}
            />
            <button type="submit" className={styles.sendBtn}>
              &uarr;
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
