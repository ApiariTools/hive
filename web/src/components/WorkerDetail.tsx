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

type InfoTab = "output" | "task";

export function WorkerDetail({ worker, detail, workspace, onBack }: Props) {
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [infoTab, setInfoTab] = useState<InfoTab>("output");
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
      {/* Left: worker info */}
      <div className={styles.info}>
        {/* Header with back, title, status, actions */}
        <div className={styles.infoHeader}>
          <button className={styles.back} onClick={onBack}>&larr;</button>
          <div className={styles.headerMid}>
            <div className={styles.title}>{worker.id}</div>
            <div className={styles.subtitle}>
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
              {worker.status} &middot; {worker.agent} &middot; {branchName(worker.branch)}
            </div>
          </div>
          <div className={styles.headerActions}>
            {worker.pr_url && (
              <a
                href={worker.pr_url}
                target="_blank"
                rel="noopener noreferrer"
                className={styles.headerBtn}
              >
                PR
              </a>
            )}
            <button className={`${styles.headerBtn} ${styles.headerBtnDanger}`}>
              Close
            </button>
          </div>
        </div>

        {/* PR review summary */}
        {(worker.review_state || worker.ci_status || (worker.open_comments != null && worker.open_comments > 0)) && (
          <div className={styles.reviewSummary}>
            {worker.review_state && (
              <span className={styles.reviewBadge} data-state={worker.review_state.toLowerCase()}>
                {worker.review_state === "APPROVED" ? "Approved" :
                 worker.review_state === "CHANGES_REQUESTED" ? "Changes requested" :
                 "Review pending"}
              </span>
            )}
            {worker.ci_status && (
              <span className={styles.ciBadge} data-status={worker.ci_status.toLowerCase()}>
                {worker.ci_status === "SUCCESS" ? "CI passing" :
                 worker.ci_status === "FAILURE" ? "CI failing" :
                 "CI pending"}
              </span>
            )}
            {worker.open_comments != null && worker.open_comments > 0 && (
              <span className={styles.commentCount}>
                {worker.open_comments} open / {worker.resolved_comments ?? 0} resolved comments
              </span>
            )}
          </div>
        )}

        {/* Tabs */}
        <div className={styles.tabs}>
          <button
            className={`${styles.tab} ${infoTab === "output" ? styles.tabActive : ""}`}
            onClick={() => setInfoTab("output")}
          >
            Output
          </button>
          <button
            className={`${styles.tab} ${infoTab === "task" ? styles.tabActive : ""}`}
            onClick={() => setInfoTab("task")}
          >
            Task
          </button>
        </div>

        {/* Tab content */}
        <div className={styles.tabContent}>
          {infoTab === "output" && (
            detail?.output ? (
              <div className={styles.prose}>
                <Markdown remarkPlugins={[remarkGfm]}>{detail.output}</Markdown>
              </div>
            ) : (
              <div className={styles.empty}>No output yet</div>
            )
          )}
          {infoTab === "task" && (
            detail?.prompt ? (
              <div className={styles.prose}>
                <Markdown remarkPlugins={[remarkGfm]}>{detail.prompt}</Markdown>
              </div>
            ) : (
              <div className={styles.empty}>No task prompt</div>
            )
          )}
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
            <div key={i} className={`${styles.msg} ${msg.role === "user" ? styles.userMsg : ""} ${msg.role === "tool" ? styles.toolMsg : ""}`}>
              {msg.role === "tool" ? (
                <div className={styles.toolLabel}>{msg.content}</div>
              ) : (
                <>
                  <div className={styles.msgMeta}>
                    <strong>{msg.role === "user" ? "You" : worker.id}</strong>
                  </div>
                  <div className={styles.msgText}>
                    <Markdown remarkPlugins={[remarkGfm]}>{msg.content}</Markdown>
                  </div>
                </>
              )}
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
