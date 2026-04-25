import type { Bot, Worker } from "../types";
import styles from "./BotNav.module.css";

interface Props {
  bots: Bot[];
  workers: Worker[];
  activeBot: string | null;
  activeWorkerId: string | null;
  onSelectBot: (name: string) => void;
  onSelectWorker: (id: string) => void;
  mobileOpen?: boolean;
}

const BOT_COLORS: Record<string, string> = {
  Main: "var(--accent)",
  Customer: "var(--red)",
  Performance: "var(--green)",
  Social: "var(--blue)",
  Product: "var(--purple)",
};

function botColor(bot: Bot): string {
  return bot.color || BOT_COLORS[bot.name] || "var(--text-faint)";
}

function statusColor(status: string): string {
  if (status === "running" || status === "active") return "var(--green)";
  if (status === "waiting") return "var(--accent)";
  return "var(--text-faint)";
}

export function BotNav({
  bots,
  workers,
  activeBot,
  activeWorkerId,
  onSelectBot,
  onSelectWorker,
  mobileOpen,
}: Props) {
  return (
    <div className={`${styles.panel} ${mobileOpen ? styles.mobileOpen : ""}`}>
      <div className={styles.label}>Bots</div>
      {bots.map((b) => (
        <button
          key={b.name}
          className={`${styles.botBtn} ${activeBot === b.name ? styles.active : ""}`}
          onClick={() => onSelectBot(b.name)}
        >
          <span
            className={styles.dot}
            style={{ background: botColor(b) }}
          />
          <span className={styles.name}>{b.name}</span>
        </button>
      ))}

      {workers.length > 0 && (
        <>
          <div className={`${styles.label} ${styles.labelSpaced}`}>
            Workers
          </div>
          {workers.map((w) => (
            <button
              key={w.id}
              className={`${styles.workerBtn} ${activeWorkerId === w.id ? styles.activeWorker : ""}`}
              onClick={() => onSelectWorker(w.id)}
            >
              <span
                className={`${styles.workerDot} ${w.status === "running" || w.status === "active" ? styles.running : ""}`}
                style={{ background: statusColor(w.status) }}
              />
              <span className={styles.workerId}>{w.id}</span>
              {w.pr_url && (
                <span className={styles.workerPr}>
                  PR
                </span>
              )}
            </button>
          ))}
        </>
      )}
    </div>
  );
}
