import { useEffect, useState, useCallback } from "react";
import { TopBar } from "./components/TopBar";
import { BotNav } from "./components/BotNav";
import { ChatPanel } from "./components/ChatPanel";
import { WorkersPanel } from "./components/WorkersPanel";
import { WorkerDetail } from "./components/WorkerDetail";
import type { Workspace, Bot, Worker, Message, WorkerDetail as WorkerDetailData } from "./types";
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
  const [streamingContent, setStreamingContent] = useState("");
  const [menuOpen, setMenuOpen] = useState(false);
  const [workerDetail, setWorkerDetail] = useState<WorkerDetailData | null>(null);
  const [workersOpen, setWorkersOpen] = useState(false);
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

  // Load conversations when workspace or bot changes, poll for updates + bot status
  useEffect(() => {
    if (!workspace || !bot) return;
    setMessages([]);
    setLoading(false);
    setLoadingStatus(undefined);
    api.getConversations(workspace, bot).then(setMessages);
    api.getBotStatus(workspace, bot).then((s) => {
      if (s.status !== "idle") {
        setLoading(true);
        setLoadingStatus(s.tool_name ? `Using ${s.tool_name}...` : "Thinking...");
        setStreamingContent(s.streaming_content || "");
      }
    });

    // Poll every 2s for conversations + bot status
    const interval = setInterval(() => {
      api.getConversations(workspace, bot).then(setMessages);
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
    return () => clearInterval(interval);
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
            detail={workerDetail}
            workspace={workspace}
            onBack={handleBackFromWorker}
          />
        ) : (
          <ChatPanel
            bot={bot}
            messages={messages}
            loading={loading}
            loadingStatus={loadingStatus}
            streamingContent={streamingContent}
            onSend={handleSend}
            workerCount={workers.length}
            onWorkersToggle={() => setWorkersOpen((v) => !v)}
            onCancel={loading ? () => api.cancelBot(workspace, bot) : undefined}
          />
        )}
        <WorkersPanel
          workers={workers}
          onSelectWorker={(id) => {
            setWorkersOpen(false);
            handleSelectWorker(id);
          }}
          mobileOpen={workersOpen}
          onClose={() => setWorkersOpen(false)}
        />
      </div>
    </>
  );
}
