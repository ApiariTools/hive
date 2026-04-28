import { useEffect, useState, useCallback, useMemo, useRef } from "react";
import { TopBar } from "./components/TopBar";
import { CommandPalette } from "./components/CommandPalette";
import { BotNav } from "./components/BotNav";
import { ChatPanel } from "./components/ChatPanel";
import { ReposPanel } from "./components/ReposPanel";
import { WorkerDetail } from "./components/WorkerDetail";
import { DocsPanel } from "./components/DocsPanel";
import type { Workspace, Bot, Worker, Repo, Message, WorkerDetail as WorkerDetailData, CrossWorkspaceBot } from "./types";
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
  const [otherWorkspaceBots, setOtherWorkspaceBots] = useState<CrossWorkspaceBot[]>([]);
  const [docsOpen, setDocsOpen] = useState(false);
  const [isMobile, setIsMobile] = useState(window.innerWidth <= 768);
  const [usage, setUsage] = useState<api.UsageData>({ installed: false, providers: [], updated_at: null });
  const lastMsgId = useRef<number>(0);
  const nextTempId = useRef<number>(-1);
  const loadingRef = useRef(false);
  const tabHiddenRef = useRef(document.hidden);

  // Track mobile state
  useEffect(() => {
    const handler = () => setIsMobile(window.innerWidth <= 768);
    window.addEventListener("resize", handler);
    return () => window.removeEventListener("resize", handler);
  }, []);

  // Track tab visibility
  useEffect(() => {
    const handler = () => { tabHiddenRef.current = document.hidden; };
    document.addEventListener("visibilitychange", handler);
    return () => document.removeEventListener("visibilitychange", handler);
  }, []);

  // Keep loadingRef in sync
  useEffect(() => { loadingRef.current = loading; }, [loading]);

  // Load workspaces on mount
  useEffect(() => {
    api.getWorkspaces().then((ws) => {
      setWorkspaces(ws);
      if (!workspace && ws.length > 0) {
        setWorkspace(ws[0].name);
      }
    });
    if (window.innerWidth <= 768 && !initial.bot) {
      setBot("Main");
    }
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
        // Append message directly instead of full refetch
        if (event.workspace === workspace && event.bot === bot) {
          const role = event.role as string;
          const content = event.content as string;
          if (role === "assistant" && !content.trim()) {
            return;
          }
          const newMsg: Message = {
            id: nextTempId.current--,
            workspace: event.workspace as string,
            bot: event.bot as string,
            role,
            content,
            attachments: null,
            created_at: new Date().toISOString(),
          };
          setMessages((prev) => [...prev, newMsg]);
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

  // Load conversations + initial status when workspace or bot changes
  useEffect(() => {
    if (!workspace || !bot) return;
    setMessages([]);
    setMessagesLoading(true);
    setLoading(false);
    setLoadingStatus(undefined);
    lastMsgId.current = 0;
    api.getConversations(workspace, bot, 30).then((msgs) => {
      setMessages(msgs);
      setMessagesLoading(false);
      if (msgs.length > 0) lastMsgId.current = msgs[msgs.length - 1].id;
    });
    api.getBotStatus(workspace, bot).then((s) => {
      if (s.status !== "idle") {
        setLoading(true);
        setLoadingStatus(s.tool_name ? `Using ${s.tool_name}...` : "Thinking...");
        setStreamingContent(s.streaming_content || "");
      }
    });
    // Mark current bot as seen after a brief delay (so badges show first on load)
    const seenTimer = setTimeout(() => {
      api.markSeen(workspace, bot);
    }, 500);
    return () => clearTimeout(seenTimer);
  }, [workspace, bot]);

  // Adaptive polling: 2s when active, 10s when idle, 30s when tab hidden
  useEffect(() => {
    if (!workspace || !bot) return;

    const getInterval = () => {
      if (tabHiddenRef.current) return 30000;
      if (loadingRef.current) return 2000;
      return 10000;
    };

    let timer: ReturnType<typeof setTimeout>;
    let cancelled = false;
    function poll() {
      const convP = api.getConversations(workspace, bot, 30).then((msgs) => {
        const latestId = msgs.length > 0 ? msgs[msgs.length - 1].id : 0;
        if (latestId !== lastMsgId.current) {
          lastMsgId.current = latestId;
          setMessages(msgs);
        }
      });
      const statusP = api.getBotStatus(workspace, bot).then((s) => {
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
      Promise.all([convP, statusP]).then(() => {
        if (!cancelled) timer = setTimeout(poll, getInterval());
      });
    }
    timer = setTimeout(poll, getInterval());
    return () => {
      cancelled = true;
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

  // Poll usage every 2 minutes
  useEffect(() => {
    api.getUsage().then(setUsage).catch(() => {});
    const interval = setInterval(() => {
      api.getUsage().then(setUsage).catch(() => {});
    }, 120000);
    return () => clearInterval(interval);
  }, []);

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

  // Fetch bots for other workspaces when palette opens
  useEffect(() => {
    if (!paletteOpen || workspaces.length === 0) return;
    let cancelled = false;
    setOtherWorkspaceBots([]);
    const others = workspaces.filter((ws) => ws.name !== workspace);
    Promise.allSettled(
      others.map((ws) =>
        api.getBots(ws.name).then((bots) =>
          bots.map((b) => ({ workspace: ws.name, bot: b }))
        )
      )
    ).then((results) => {
      if (!cancelled) {
        const fulfilled = results
          .filter((r): r is PromiseFulfilledResult<CrossWorkspaceBot[]> => r.status === "fulfilled")
          .flatMap((r) => r.value);
        setOtherWorkspaceBots(fulfilled);
      }
    });
    return () => { cancelled = true; };
  }, [paletteOpen, workspaces, workspace]);

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
    setBot(isMobile ? "Main" : "");
    setWorkerId(null);
    setLoading(false);
    setLoadingStatus(undefined);
  }, [isMobile]);

  const handleSelectWorkspaceBot = useCallback((ws: string, botName: string) => {
    setWorkspace(ws);
    setBot(botName);
    setWorkerId(null);
    setDocsOpen(false);
    setMenuOpen(false);
    setLoading(false);
    setLoadingStatus(undefined);
  }, []);

  const handleSelectBot = useCallback((name: string) => {
    setBot(name);
    setWorkerId(null);
    setDocsOpen(false);
    setMenuOpen(false);
    setLoading(false);
    setLoadingStatus(undefined);
  }, []);

  const handleSelectDocs = useCallback(() => {
    setDocsOpen(true);
    setWorkerId(null);
    setMenuOpen(false);
  }, []);

  const handleSelectWorker = useCallback((id: string) => {
    setWorkerId(id);
    setDocsOpen(false);
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
        usage={usage}
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
          activeBot={docsOpen || workerId ? null : bot}
          activeWorkerId={workerId}
          onSelectBot={handleSelectBot}
          onSelectWorker={handleSelectWorker}
          mobileOpen={menuOpen}
          unread={unread}
          docsOpen={docsOpen}
          onSelectDocs={handleSelectDocs}
        />
        {docsOpen ? (
          <DocsPanel workspace={workspace} />
        ) : workerId && selectedWorker ? (
          <WorkerDetail
            worker={selectedWorker}
            detail={workerDetail}
            workspace={workspace}
            onBack={handleBackFromWorker}
          />
        ) : bot ? (
          <ChatPanel
            bot={bot}
            botDescription={bots.find((b) => b.name === bot)?.description}
            messages={messages}
            messagesLoading={messagesLoading}
            loading={loading}
            loadingStatus={loadingStatus}
            streamingContent={streamingContent}
            onSend={handleSend}
            workerCount={workers.length}
            onWorkersToggle={() => setWorkersOpen((v) => !v)}
            onCancel={loading ? () => api.cancelBot(workspace, bot) : undefined}
            ttsVoice={workspaces.find((w) => w.name === workspace)?.tts_voice}
            ttsSpeed={workspaces.find((w) => w.name === workspace)?.tts_speed}
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
        otherWorkspaceBots={otherWorkspaceBots}
        onSelectWorkspaceBot={handleSelectWorkspaceBot}
      />
    </>
  );
}
