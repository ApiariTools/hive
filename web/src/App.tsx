import { useEffect, useState, useCallback, useMemo } from "react";
import { TopBar } from "./components/TopBar";
import { CommandPalette } from "./components/CommandPalette";
import { BotNav } from "./components/BotNav";
import { ChatPanel } from "./components/ChatPanel";
import { ReposPanel } from "./components/ReposPanel";
import { WorkerDetail } from "./components/WorkerDetail";
import type { Workspace, Bot, Worker, Repo, Message, WorkerDetail as WorkerDetailData } from "./types";
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
    bot: parts[1] || "",
    workerId: parts[2] === "worker" ? parts[3] || null : null,
  };
}

function buildHash(r: Route): string {
  if (!r.workspace) return "";
  let h = r.bot ? `#/${r.workspace}/${r.bot}` : `#/${r.workspace}`;
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
  const [repos, setRepos] = useState<Repo[]>([]);
  const [messages, setMessages] = useState<Message[]>([]);
  const [messagesLoading, setMessagesLoading] = useState(true);
  const [loading, setLoading] = useState(false);
  const [streamingContent, setStreamingContent] = useState("");
  const [menuOpen, setMenuOpen] = useState(false);
  const [workerDetail, setWorkerDetail] = useState<WorkerDetailData | null>(null);
  const [workersOpen, setWorkersOpen] = useState(false);
  const [loadingStatus, setLoadingStatus] = useState<string | undefined>();
  const [unread, setUnread] = useState<Record<string, number>>({});
  const [paletteOpen, setPaletteOpen] = useState(false);

  // Load workspaces on mount
  useEffect(() => {
    api.getWorkspaces().then((ws) => {
      setWorkspaces(ws);
      if (!workspace && ws.length > 0) {
        setWorkspace(ws[0].name);
      }
    });
  }, []);

  // WebSocket for real-time updates
  useEffect(() => {
    const ws = api.connectWebSocket((event) => {
      if (event.type === "bot_status") {
        if (event.workspace === workspace && event.bot === bot) {
          if (event.status === "idle") {
            setLoading(false);
            setLoadingStatus(undefined);
            setStreamingContent("");
            // Refresh conversations
            api.getConversations(workspace, bot).then(setMessages);
          } else {
            setLoading(true);
            setLoadingStatus(
              event.tool_name ? `Using ${event.tool_name}...` : "Thinking...",
            );
          }
        }
      }
      if (event.type === "message") {
        // Refresh unread counts
        if (workspace) api.getUnread(workspace).then(setUnread);
        // If it's the current bot, refresh messages
        if (event.workspace === workspace && event.bot === bot) {
          api.getConversations(workspace, bot).then(setMessages);
        }
      }
    });
    return () => ws.close();
  }, [workspace, bot]);

  // Load bots + workers when workspace changes
  useEffect(() => {
    if (!workspace) return;
    api.getBots(workspace).then(setBots);
    api.getWorkers(workspace).then(setWorkers);
    api.getRepos(workspace).then(setRepos);
    api.getUnread(workspace).then(setUnread);
  }, [workspace]);

  // Load conversations when workspace or bot changes, poll for updates + bot status
  useEffect(() => {
    if (!workspace || !bot) return;
    setMessages([]);
    setMessagesLoading(true);
    setLoading(false);
    setLoadingStatus(undefined);
    api.getConversations(workspace, bot, 30).then((msgs) => {
      setMessages(msgs);
      setMessagesLoading(false);
    });
    // Mark current bot as seen after a brief delay (so badges show first on load)
    const timer = setTimeout(() => {
      api.markSeen(workspace, bot);
    }, 500);
    api.getBotStatus(workspace, bot).then((s) => {
      if (s.status !== "idle") {
        setLoading(true);
        setLoadingStatus(s.tool_name ? `Using ${s.tool_name}...` : "Thinking...");
        setStreamingContent(s.streaming_content || "");
      }
    });

    // Poll every 3s for bot status + conversations
    const interval = setInterval(() => {
      api.getConversations(workspace, bot, 30).then(setMessages);
      api.getBotStatus(workspace, bot).then((s) => {
        if (s.status === "idle") {
          setLoading(false);
          setLoadingStatus(undefined);
          setStreamingContent("");
        } else {
          setLoading(true);
          setLoadingStatus(s.tool_name ? `Using ${s.tool_name}...` : "Thinking...");
          setStreamingContent(s.streaming_content || "");
        }
      });
    }, 2000);
    return () => {
      clearInterval(interval);
      clearTimeout(timer);
    };
  }, [workspace, bot]);

  // Poll workers every 5s, repos every 30s
  useEffect(() => {
    if (!workspace) return;
    const workerInterval = setInterval(() => {
      api.getWorkers(workspace).then(setWorkers);
    }, 5000);
    const repoInterval = setInterval(() => {
      api.getRepos(workspace).then(setRepos);
    }, 30000);
    return () => {
      clearInterval(workerInterval);
      clearInterval(repoInterval);
    };
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

  // Cmd+K: command palette, Cmd+J: focus chat
  useEffect(() => {
    function handleGlobalKeyDown(e: KeyboardEvent) {
      if (e.repeat) return;
      const key = e.key.toLowerCase();
      if ((e.metaKey || e.ctrlKey) && key === "k") {
        e.preventDefault();
        setPaletteOpen((v) => !v);
      }
      if ((e.metaKey || e.ctrlKey) && key === "j") {
        e.preventDefault();
        document.querySelector<HTMLTextAreaElement>('textarea[enterkeyhint="send"]')?.focus();
      }
    }
    window.addEventListener("keydown", handleGlobalKeyDown);
    return () => window.removeEventListener("keydown", handleGlobalKeyDown);
  }, []);

  const handleSelectWorkspace = useCallback((ws: string) => {
    setWorkspace(ws);
    setBot("");
    setWorkerId(null);
    setLoading(false);
    setLoadingStatus(undefined);
  }, []);

  const handleSelectBot = useCallback((name: string) => {
    setBot(name);
    setWorkerId(null);
    setMenuOpen(false);
    setLoading(false);
    setLoadingStatus(undefined);
  }, []);

  const handleSelectWorker = useCallback((id: string) => {
    setWorkerId(id);
    setMenuOpen(false);
    if (workspace) {
      api.getWorkerDetail(workspace, id).then(setWorkerDetail).catch(() => setWorkerDetail(null));
    }
  }, [workspace]);

  // Poll worker detail while viewing a worker
  useEffect(() => {
    if (!workspace || !workerId) return;
    const interval = setInterval(() => {
      api.getWorkerDetail(workspace, workerId).then(setWorkerDetail).catch(() => {});
    }, 3000);
    return () => clearInterval(interval);
  }, [workspace, workerId]);

  const handleBackFromWorker = useCallback(() => {
    setWorkerId(null);
  }, []);

  const handleSend = useCallback(
    async (text: string, attachments?: import("./components/ChatPanel").Attachment[]) => {
      const apiAttachments = attachments?.map((a) => ({
        name: a.name,
        type: a.type,
        dataUrl: a.dataUrl,
      }));

      // Fire and forget — daemon handles everything
      setLoading(true);
      setLoadingStatus("Thinking...");
      await api.sendMessage(workspace, bot, text, apiAttachments);
      // Polling will pick up the user message + bot response from DB
    },
    [workspace, bot],
  );

  // Merge fresh worker data (5s poll) into repos (30s poll) so worker status stays current
  const reposWithFreshWorkers = useMemo(() => {
    const workerMap = new Map(workers.map((w) => [w.id, w]));
    return repos.map((repo) => ({
      ...repo,
      workers: repo.workers.map((rw) => workerMap.get(rw.id) || rw),
    }));
  }, [repos, workers]);

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
        onOpenPalette={() => setPaletteOpen(true)}
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
          unread={unread}
        />
        {workerId && selectedWorker ? (
          <WorkerDetail
            worker={selectedWorker}
            detail={workerDetail}
            workspace={workspace}
            onBack={handleBackFromWorker}
          />
        ) : bot ? (
          <ChatPanel
            bot={bot}
            botDescription={bots.find(b => b.name === bot)?.description}
            messages={messages}
            messagesLoading={messagesLoading}
            loading={loading}
            loadingStatus={loadingStatus}
            streamingContent={streamingContent}
            onSend={handleSend}
            workerCount={workers.length}
            onWorkersToggle={() => setWorkersOpen((v) => !v)}
            onCancel={loading ? () => api.cancelBot(workspace, bot) : undefined}
          />
        ) : (
          <div style={{ flex: 1, display: "flex", alignItems: "center", justifyContent: "center", flexDirection: "column", gap: 8 }}>
            <div style={{ fontSize: 14, color: "var(--text-faint)" }}>Select a bot to start chatting</div>
          </div>
        )}
        <ReposPanel
          repos={reposWithFreshWorkers}
          onSelectWorker={(id) => {
            setWorkersOpen(false);
            handleSelectWorker(id);
          }}
          mobileOpen={workersOpen}
          onClose={() => setWorkersOpen(false)}
        />
      </div>
      <CommandPalette
        open={paletteOpen}
        onOpenChange={setPaletteOpen}
        workspaces={workspaces}
        bots={bots}
        workers={workers}
        currentWorkspace={workspace}
        currentBot={bot}
        onSelectWorkspace={handleSelectWorkspace}
        onSelectBot={handleSelectBot}
        onSelectWorker={handleSelectWorker}
      />
    </>
  );
}
