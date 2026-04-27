import { Search } from "lucide-react";
import type { Workspace } from "../types";
import type { UsageData } from "../api";
import styles from "./TopBar.module.css";

interface Props {
  workspaces: Workspace[];
  active: string;
  onSelect: (name: string) => void;
  onMenuToggle?: () => void;
  onOpenPalette?: () => void;
  usage?: UsageData;
}

function dotColor(p: { status: string; usage_percent: number | null }): string {
  if (p.status === "rate_limited") return "var(--red)";
  if (p.status === "error") return "var(--text-faint)";
  const pct = p.usage_percent ?? 0;
  if (pct > 80) return "var(--red)";
  if (pct > 50) return "var(--accent)";
  return "var(--green)";
}

function dotTitle(p: { name: string; status: string; usage_percent: number | null; remaining: string | null; resets_at: string | null }): string {
  let t = `${p.name}: ${p.usage_percent != null ? `${Math.round(p.usage_percent)}% used` : p.status}`;
  if (p.remaining) t += ` — ${p.remaining} remaining`;
  if (p.resets_at) t += ` — resets ${p.resets_at}`;
  return t;
}

export function TopBar({ workspaces, active, onSelect, onMenuToggle, onOpenPalette, usage }: Props) {
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
      {usage && usage.providers.length > 0 && (
        <div className={styles.usageDots}>
          {usage.providers.map((p) => (
            <span
              key={p.name}
              className={styles.usageDot}
              style={{ background: dotColor(p) }}
              title={dotTitle(p)}
            />
          ))}
        </div>
      )}
      <button
        className={styles.searchBtn}
        onClick={() => onOpenPalette?.()}
        aria-label="Open command palette"
      >
        <Search size={16} />
      </button>
    </div>
  );
}
