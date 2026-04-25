import type { Repo } from "../types";
import styles from "./ReposPanel.module.css";

interface Props {
  repos: Repo[];
  onSelectWorker: (id: string) => void;
  mobileOpen?: boolean;
  onClose?: () => void;
}

function formatElapsed(secs: number | null): string {
  if (secs == null) return "";
  if (secs < 60) return `${secs}s`;
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins} min`;
  const hrs = Math.floor(mins / 60);
  const rem = mins % 60;
  return rem > 0 ? `${hrs}h ${rem}m` : `${hrs}h`;
}

function branchName(branch: string): string {
  return branch.replace(/^swarm\//, "");
}

function syncLabel(repo: Repo): string {
  if (repo.sync_status === "synced") return "\u2713";
  if (repo.sync_status === "behind") return `\u2193${repo.behind}`;
  if (repo.sync_status === "ahead") return `\u2191${repo.ahead}`;
  if (repo.sync_status === "diverged") return `\u2191${repo.ahead} \u2193${repo.behind}`;
  return "?";
}

function syncColorClass(status: string): { dot: string; badge: string } {
  switch (status) {
    case "synced": return { dot: styles.dotSynced, badge: styles.syncSynced };
    case "behind": return { dot: styles.dotBehind, badge: styles.syncBehind };
    case "ahead": return { dot: styles.dotAhead, badge: styles.syncAhead };
    case "diverged": return { dot: styles.dotDiverged, badge: styles.syncDiverged };
    default: return { dot: styles.dotUnknown, badge: styles.syncUnknown };
  }
}

export function ReposPanel({ repos, onSelectWorker, mobileOpen, onClose }: Props) {
  return (
    <>
      {mobileOpen && (
        <div className={styles.backdrop} onClick={onClose} />
      )}
      <div className={`${styles.panel} ${mobileOpen ? styles.mobileOpen : ""}`}>
        <div className={styles.title}>Repos</div>
        {repos.map((repo) => {
          const colors = syncColorClass(repo.sync_status);
          return (
            <div key={repo.path} className={styles.repoRow}>
              <div className={styles.repoHeader}>
                <span className={`${styles.repoDot} ${colors.dot}`} />
                <span className={styles.repoName}>{repo.name}</span>
                <span className={`${styles.syncBadge} ${colors.badge}`}>
                  {syncLabel(repo)}
                </span>
              </div>
              {repo.workers.length > 0 && (
                <div className={styles.workerList}>
                  {repo.workers.map((w) => (
                    <div
                      key={w.id}
                      className={styles.workerCard}
                      onClick={() => onSelectWorker(w.id)}
                    >
                      <div className={styles.workerTop}>
                        <span
                          className={`${styles.workerDot} ${w.status === "running" || w.status === "active" ? styles.running : ""}`}
                          style={{
                            background:
                              w.status === "running" || w.status === "active"
                                ? "var(--green)"
                                : w.status === "waiting"
                                  ? "var(--accent)"
                                  : "var(--text-faint)",
                          }}
                        />
                        <span className={styles.workerId}>{w.id}</span>
                        <span className={styles.workerTime}>
                          {formatElapsed(w.elapsed_secs)}
                        </span>
                      </div>
                      <div className={styles.workerDesc}>
                        {w.description || branchName(w.branch)}
                      </div>
                      <div className={styles.tags}>
                        {w.pr_url && (
                          <a
                            href={w.pr_url}
                            className={`${styles.tag} ${styles.tagPr}`}
                            onClick={(e) => e.stopPropagation()}
                            target="_blank"
                            rel="noopener noreferrer"
                          >
                            {w.pr_title ? `PR: ${w.pr_title}` : "PR"}
                          </a>
                        )}
                        {w.dispatched_by && (
                          <span className={`${styles.tag} ${styles.tagBot}`}>
                            via {w.dispatched_by}
                          </span>
                        )}
                      </div>
                    </div>
                  ))}
                </div>
              )}
            </div>
          );
        })}
      </div>
    </>
  );
}
