import type { Workspace } from "../types";
import styles from "./TopBar.module.css";

interface Props {
  workspaces: Workspace[];
  active: string;
  onSelect: (name: string) => void;
}

export function TopBar({ workspaces, active, onSelect }: Props) {
  return (
    <div className={styles.bar}>
      <div className={styles.logo}>hive</div>
      {workspaces.map((ws) => (
        <button
          key={ws.name}
          className={`${styles.tab} ${ws.name === active ? styles.active : ""}`}
          onClick={() => onSelect(ws.name)}
        >
          {ws.name}
        </button>
      ))}
    </div>
  );
}
