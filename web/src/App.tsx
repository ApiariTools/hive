import { useEffect, useState, useCallback } from "react";
import { TopBar } from "./components/TopBar";
import { BotNav } from "./components/BotNav";
import { ChatPanel } from "./components/ChatPanel";
import { WorkersPanel } from "./components/WorkersPanel";
import { WorkerDetail } from "./components/WorkerDetail";
import type { Workspace, Bot, Worker, Message } from "./types";
import * as api from "./api";

// ── Route parsing ──

interface Route {
  workspace: string;
  bot: string;
  workerId: string | null;
}

function parseHash(): Route {
  const raw = window.location.hash.replace(/^#\/?/, "");
  const parts = raw.split("/").filter(Boolean);
  return {
    workspace: parts[0] || "",
    bot: parts[1] || "Main",
    workerId: parts[2] === "worker" ? parts[3] || null : null,
  };
}

function buildHash(r: Route): string {
  if (!r.workspace) return "";
  let h = `#/${r.workspace}/${r.bot}`;
  if (r.workerId) h += `/worker/${r.workerId}`;
  return h;
}

function pushHash(r: Route) {
  const h = buildHash(r);
  if (window.location.hash !== h) history.pushState(null, "", h || "/");
}

// ── App ──

export default function App() {
  const initial = parseHash();
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);
  const [workspace, setWorkspace] = useState(initial.workspace);
  const [bot, setBot] = useState(initial.bot);
  const [workerId, setWorkerId] = useState<string | null>(initial.workerId);
  const [bots, setBots] = useState<Bot[]>([]);
  const [workers, setWorkers] = useState<Worker[]>([]);
  const [messages, setMessages] = useState<Message[]>([]);
  const [loading, setLoading] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const [loadingStatus, setLoadingStatus] = useState<string | undefined>();

  // Load workspaces on mount
  useEffect(() => {
    api.getWorkspaces().then((ws) => {
      setWorkspaces(ws);
      if (!workspace && ws.length > 0) {
        setWorkspace(ws[0].name);
      }
    });
  }, []);

  // Load bots + workers when workspace changes
  useEffect(() => {
    if (!workspace) return;
    api.getBots(workspace).then(setBots);
    api.getWorkers(workspace).then(setWorkers);
  }, [workspace]);

  // Load conversations when workspace or bot changes
  useEffect(() => {
    if (!workspace || !bot) return;
    api.getConversations(workspace, bot).then(setMessages);
  }, [workspace, bot]);

  // Poll workers every 5s
  useEffect(() => {
    if (!workspace) return;
    const interval = setInterval(() => {
      api.getWorkers(workspace).then(setWorkers);
    }, 5000);
    return () => clearInterval(interval);
  }, [workspace]);

  // Sync hash
  useEffect(() => {
    pushHash({ workspace, bot, workerId });
  }, [workspace, bot, workerId]);

  // Browser back/forward
  useEffect(() => {
    const onPop = () => {
      const r = parseHash();
      setWorkspace(r.workspace);
      setBot(r.bot);
      setWorkerId(r.workerId);
    };
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);

  const handleSelectWorkspace = useCallback((ws: string) => {
    setWorkspace(ws);
    setBot("Main");
    setWorkerId(null);
  }, []);

  const handleSelectBot = useCallback((name: string) => {
    setBot(name);
    setWorkerId(null);
    setMenuOpen(false);
  }, []);

  const handleSelectWorker = useCallback((id: string) => {
    setWorkerId(id);
    setMenuOpen(false);
  }, []);

  const handleBackFromWorker = useCallback(() => {
    setWorkerId(null);
  }, []);

  const handleSend = useCallback(
    async (text: string) => {
      const userMsg: Message = {
        id: Date.now(),
        workspace,
        bot,
        role: "user",
        content: text,
        attachments: null,
        created_at: new Date().toISOString(),
      };
      const streamId = Date.now() + 1;
      setMessages((prev) => [...prev, userMsg]);
      setLoading(true);
      setLoadingStatus("Thinking...");

      // Add an empty assistant message that we'll stream into
      setMessages((prev) => [
        ...prev,
        {
          id: streamId,
          workspace,
          bot,
          role: "assistant",
          content: "",
          attachments: null,
          created_at: new Date().toISOString(),
        },
      ]);

      try {
        await api.sendMessageStream(workspace, bot, text, {
          onText: (chunk) => {
            setMessages((prev) =>
              prev.map((m) =>
                m.id === streamId
                  ? { ...m, content: m.content + chunk }
                  : m,
              ),
            );
            setLoadingStatus(undefined);
          },
          onToolUse: (tool) => {
            setLoadingStatus(`Using ${tool}...`);
          },
          onDone: () => {
            setLoading(false);
            setLoadingStatus(undefined);
          },
          onError: (error) => {
            setMessages((prev) =>
              prev.map((m) =>
                m.id === streamId
                  ? { ...m, content: `Error: ${error}` }
                  : m,
              ),
            );
            setLoading(false);
            setLoadingStatus(undefined);
          },
        });
      } catch {
        setLoading(false);
        setLoadingStatus(undefined);
      }
    },
    [workspace, bot],
  );

  const selectedWorker = workerId
    ? workers.find((w) => w.id === workerId) || null
    : null;

  return (
    <>
      <TopBar
        workspaces={workspaces}
        active={workspace}
        onSelect={handleSelectWorkspace}
        onMenuToggle={() => setMenuOpen((v) => !v)}
      />
      <div style={{ flex: 1, display: "flex", overflow: "hidden", position: "relative" }}>
        {/* Mobile drawer overlay */}
        {menuOpen && (
          <div
            className="drawer-backdrop"
            onClick={() => setMenuOpen(false)}
          />
        )}
        <BotNav
          bots={bots}
          workers={workers}
          activeBot={workerId ? null : bot}
          activeWorkerId={workerId}
          onSelectBot={handleSelectBot}
          onSelectWorker={handleSelectWorker}
          mobileOpen={menuOpen}
        />
        {workerId && selectedWorker ? (
          <WorkerDetail
            worker={selectedWorker}
            messages={[]}
            onBack={handleBackFromWorker}
            onSend={() => {}}
          />
        ) : (
          <>
            <ChatPanel
              bot={bot}
              messages={messages}
              loading={loading}
              loadingStatus={loadingStatus}
              onSend={handleSend}
            />
            <WorkersPanel
              workers={workers}
              onSelectWorker={handleSelectWorker}
            />
          </>
        )}
      </div>
    </>
  );
}
