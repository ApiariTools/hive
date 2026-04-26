import { Command } from "cmdk";
import type { Workspace, Bot, Worker } from "../types";
import styles from "./CommandPalette.module.css";

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  workspaces: Workspace[];
  bots: Bot[];
  workers: Worker[];
  currentWorkspace: string;
  currentBot: string;
  onSelectWorkspace: (name: string) => void;
  onSelectBot: (name: string) => void;
  onSelectWorker: (id: string) => void;
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
              key={ws.name}
              value={`workspace ${ws.name}`}
              onSelect={() => {
                onSelectWorkspace(ws.name);
                onOpenChange(false);
              }}
            >
              {ws.name}
              {ws.name === currentWorkspace && (
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
        <Command.Group heading="Workers">
          {workers.map((w) => (
            <Command.Item
              key={w.id}
              value={`worker ${w.id} ${w.branch || ""}`}
              onSelect={() => {
                onSelectWorker(w.id);
                onOpenChange(false);
              }}
            >
              {w.id}
              <span className={styles.workerStatus}>{w.status}</span>
            </Command.Item>
          ))}
        </Command.Group>
      </Command.List>
    </Command.Dialog>
  );
}
