import type { Workspace, Bot, Worker, WorkerDetail, Message, Repo, Doc, ResearchTask } from "./types";

const BASE = "/api";

async function get<T>(path: string): Promise<T> {
  const res = await fetch(`${BASE}${path}`);
  if (!res.ok) throw new Error(`GET ${path}: ${res.status}`);
  return res.json();
}

/** Build the workspace path prefix, routing through the proxy for remote workspaces */
function wsPath(workspace: string, remote?: string): string {
  if (remote) return `/remotes/${remote}/workspaces/${workspace}`;
  return `/workspaces/${workspace}`;
}

export function getWorkspaces(): Promise<Workspace[]> {
  return get("/workspaces");
}

export function getBots(workspace: string, remote?: string): Promise<Bot[]> {
  return get(`${wsPath(workspace, remote)}/bots`);
}

export function getWorkers(workspace: string, remote?: string): Promise<Worker[]> {
  return get(`${wsPath(workspace, remote)}/workers`);
}

export function getRepos(workspace: string, remote?: string): Promise<Repo[]> {
  return get(`${wsPath(workspace, remote)}/repos`);
}

export function getConversations(
  workspace: string,
  bot: string,
  limit?: number,
  remote?: string,
): Promise<Message[]> {
  const params = limit ? `?limit=${limit}` : "";
  return get(`${wsPath(workspace, remote)}/conversations/${bot}${params}`);
}

export function getWorkerDetail(
  workspace: string,
  workerId: string,
  remote?: string,
): Promise<WorkerDetail> {
  return get(`${wsPath(workspace, remote)}/workers/${workerId}`);
}

export async function sendWorkerMessage(
  workspace: string,
  workerId: string,
  message: string,
  remote?: string,
): Promise<{ ok: boolean; error?: string }> {
  const res = await fetch(
    `${BASE}${wsPath(workspace, remote)}/workers/${workerId}/send`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ message }),
    },
  );
  return res.json();
}

export interface BotStatus {
  status: string;
  streaming_content: string;
  tool_name: string | null;
}

export function getBotStatus(
  workspace: string,
  bot: string,
  remote?: string,
): Promise<BotStatus> {
  return get(`${wsPath(workspace, remote)}/bots/${bot}/status`);
}

export async function cancelBot(
  workspace: string,
  bot: string,
  remote?: string,
): Promise<{ ok: boolean }> {
  const res = await fetch(`${BASE}${wsPath(workspace, remote)}/bots/${bot}/cancel`, {
    method: "POST",
  });
  return res.json();
}

export function getUnread(workspace: string, remote?: string): Promise<Record<string, number>> {
  return get(`${wsPath(workspace, remote)}/unread`);
}

export async function markSeen(workspace: string, bot: string, remote?: string): Promise<void> {
  await fetch(`${BASE}${wsPath(workspace, remote)}/seen/${bot}`, { method: "POST" });
}

export function connectWebSocket(
  onEvent: (event: { type: string; workspace: string; bot: string; [key: string]: unknown }) => void,
): WebSocket {
  const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  const wsUrl = `${protocol}//${window.location.host}/ws`;
  const ws = new WebSocket(wsUrl);
  ws.onmessage = (e) => {
    try {
      const event = JSON.parse(e.data);
      onEvent(event);
    } catch {}
  };
  ws.onclose = () => {
    // Reconnect after 3s
    setTimeout(() => connectWebSocket(onEvent), 3000);
  };
  return ws;
}

export async function textToSpeech(text: string, voice?: string): Promise<ArrayBuffer | null> {
  try {
    const res = await fetch(`${BASE}/tts`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ text, ...(voice ? { voice } : {}) }),
    });
    if (!res.ok) return null;
    return await res.arrayBuffer();
  } catch {
    return null;
  }
}

export interface ProviderUsage {
  name: string;
  status: string;
  usage_percent: number | null;
  remaining: string | null;
  limit: string | null;
  resets_at: string | null;
}

export interface UsageData {
  installed: boolean;
  providers: ProviderUsage[];
  updated_at: string | null;
}

export function getUsage(): Promise<UsageData> {
  return get("/usage");
}

export function getDocs(workspace: string, remote?: string): Promise<Doc[]> {
  return get(`${wsPath(workspace, remote)}/docs`);
}

export function getDoc(workspace: string, filename: string, remote?: string): Promise<Doc> {
  return get(`${wsPath(workspace, remote)}/docs/${encodeURIComponent(filename)}`);
}

export async function saveDoc(
  workspace: string,
  filename: string,
  content: string,
  remote?: string,
): Promise<{ ok: boolean }> {
  const res = await fetch(`${BASE}${wsPath(workspace, remote)}/docs/${encodeURIComponent(filename)}`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ content }),
  });
  if (!res.ok) throw new Error(`PUT docs/${filename}: ${res.status}`);
  return res.json();
}

export async function deleteDoc(
  workspace: string,
  filename: string,
  remote?: string,
): Promise<{ ok: boolean }> {
  const res = await fetch(`${BASE}${wsPath(workspace, remote)}/docs/${encodeURIComponent(filename)}`, {
    method: "DELETE",
  });
  if (!res.ok) throw new Error(`DELETE docs/${filename}: ${res.status}`);
  return res.json();
}

export function getResearchTasks(workspace: string, remote?: string): Promise<ResearchTask[]> {
  return get(`${wsPath(workspace, remote)}/research`);
}

export async function startResearch(
  workspace: string,
  topic: string,
  remote?: string,
): Promise<{ id: string; topic: string; status: string }> {
  const res = await fetch(`${BASE}${wsPath(workspace, remote)}/research`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ topic }),
  });
  if (!res.ok) throw new Error(`POST research: ${res.status}`);
  return res.json();
}

export async function sendMessage(
  workspace: string,
  bot: string,
  message: string,
  attachments?: Array<{ name: string; type: string; dataUrl: string }>,
  remote?: string,
): Promise<{ ok: boolean }> {
  const res = await fetch(`${BASE}${wsPath(workspace, remote)}/chat/${bot}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ message, attachments }),
  });
  return res.json();
}
