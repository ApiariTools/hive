import { Search } from "lucide-react";
import type { Workspace } from "../types";
import styles from "./TopBar.module.css";

interface Props {
  workspaces: Workspace[];
  active: string;
  onSelect: (name: string) => void;
  onMenuToggle?: () => void;
  onOpenPalette?: () => void;
}

export function TopBar({ workspaces, active, onSelect, onMenuToggle, onOpenPalette }: Props) {
  return (
    <div className={styles.bar}>
      <button className={styles.hamburger} onClick={onMenuToggle}>
        <span /><span /><span />
      </button>
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
      <button
        className={styles.searchBtn}
        onClick={onOpenPalette}
        aria-label="Open command palette"
      >
        <Search size={16} />
      </button>
    </div>
  );
}
