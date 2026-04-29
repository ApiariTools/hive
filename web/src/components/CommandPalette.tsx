import { Command } from "cmdk";
import type { Workspace, Bot, Worker, CrossWorkspaceBot } from "../types";
import styles from "./CommandPalette.module.css";

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  workspaces: Workspace[];
  bots: Bot[];
  workers: Worker[];
  currentWorkspace: string;
  currentBot: string;
  onSelectWorkspace: (name: string, remote?: string) => void;
  onSelectBot: (name: string) => void;
  onSelectWorker: (id: string) => void;
  otherWorkspaceBots: CrossWorkspaceBot[];
  onSelectWorkspaceBot: (workspace: string, botName: string, remote?: string) => void;
  onStartResearch?: () => void;
}

export function CommandPalette({
  open,
  onOpenChange,
  workspaces,
  bots,
  workers,
  currentWorkspace,
  currentBot,
  onSelectWorkspace,
  onSelectBot,
  onSelectWorker,
  otherWorkspaceBots,
  onSelectWorkspaceBot,
  onStartResearch,
}: Props) {
  return (
    <Command.Dialog
      open={open}
      onOpenChange={onOpenChange}
      label="Command palette"
      overlayClassName={styles.overlay}
      contentClassName={styles.dialog}
    >
      <Command.Input placeholder="Type a command..." aria-label="Search commands" />
      <Command.List>
        <Command.Empty>No results found.</Command.Empty>
        <Command.Group heading="Workspaces">
          {workspaces.map((ws) => (
            <Command.Item
              key={`${ws.remote || "local"}/${ws.name}`}
              value={`workspace ${ws.name} ${ws.remote || ""}`}
              onSelect={() => {
                onSelectWorkspace(ws.name, ws.remote);
                onOpenChange(false);
              }}
            >
              {ws.name}
              {ws.remote && <span className={styles.remoteBadge}>{ws.remote}</span>}
              {ws.name === currentWorkspace && !ws.remote && (
                <span className={styles.current}>current</span>
              )}
            </Command.Item>
          ))}
        </Command.Group>
        <Command.Group heading="Bots">
          {bots.map((b) => (
            <Command.Item
              key={b.name}
              value={`bot ${b.name}`}
              onSelect={() => {
                onSelectBot(b.name);
                onOpenChange(false);
              }}
            >
              {b.name}
              {b.name === currentBot && (
                <span className={styles.current}>current</span>
              )}
            </Command.Item>
          ))}
        </Command.Group>
        {otherWorkspaceBots.length > 0 && (
          <Command.Group heading="Other Workspace Bots">
            {otherWorkspaceBots.map((entry) => (
              <Command.Item
                key={`${entry.remote || "local"}/${entry.workspace}/${entry.bot.name}`}
                value={`bot ${entry.workspace} ${entry.bot.name} ${entry.remote || ""}`}
                onSelect={() => {
                  onSelectWorkspaceBot(entry.workspace, entry.bot.name, entry.remote);
                  onOpenChange(false);
                }}
              >
                {entry.workspace} / {entry.bot.name}
                {entry.remote && <span className={styles.remoteBadge}>{entry.remote}</span>}
              </Command.Item>
            ))}
          </Command.Group>
        )}
        {onStartResearch && (
          <Command.Group heading="Actions">
            <Command.Item
              value="start research"
              onSelect={() => {
                onStartResearch();
                onOpenChange(false);
              }}
            >
              Start Research...
            </Command.Item>
          </Command.Group>
        )}
        <Command.Group heading="Workers">
          {workers.map((w) => (
            <Command.Item
              key={w.id}
              value={`worker ${w.id} ${w.branch || ""} ${w.pr_title || ""}`}
              onSelect={() => {
                onSelectWorker(w.id);
                onOpenChange(false);
              }}
            >
              <span className={styles.workerInfo}>
                <span className={styles.workerName}>{w.id}</span>
                <span className={styles.workerDesc}>
                  {w.pr_title || (w.branch ? w.branch.replace(/^swarm\//, "") : "")}
                </span>
              </span>
              <span className={styles.workerMeta}>
                <span className={styles.workerStatus}>{w.status}</span>
              </span>
            </Command.Item>
          ))}
        </Command.Group>
      </Command.List>
    </Command.Dialog>
  );
}
