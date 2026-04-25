import type { Repo } from "../types";
import styles from "./ReposPanel.module.css";

interface Props {
  repos: Repo[];
  mobileOpen?: boolean;
  onClose?: () => void;
}

export function ReposPanel({ repos, mobileOpen, onClose }: Props) {
  return (
    <>
      {mobileOpen && (
        <div className={styles.backdrop} onClick={onClose} />
      )}
      <div className={`${styles.panel} ${mobileOpen ? styles.mobileOpen : ""}`}>
        <div className={styles.title}>Repos</div>
        {repos.map((repo) => (
          <div key={repo.path} className={styles.repoRow}>
            <div className={styles.repoHeader}>
              <span className={styles.repoName}>{repo.name}</span>
              {repo.worker_count > 0 && (
                <span className={styles.workerBadge}>
                  {repo.worker_count} worker{repo.worker_count !== 1 ? "s" : ""}
                </span>
              )}
            </div>
          </div>
        ))}
        {repos.length === 0 && (
          <div className={styles.empty}>No repos found</div>
        )}
      </div>
    </>
  );
}
