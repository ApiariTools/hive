import type { Workspace, Bot, Worker, WorkerDetail, Message } from "./types";

const BASE = "/api";

async function get<T>(path: string): Promise<T> {
  const res = await fetch(`${BASE}${path}`);
  if (!res.ok) throw new Error(`GET ${path}: ${res.status}`);
  return res.json();
}

export function getWorkspaces(): Promise<Workspace[]> {
  return get("/workspaces");
}

export function getBots(workspace: string): Promise<Bot[]> {
  return get(`/workspaces/${workspace}/bots`);
}

export function getWorkers(workspace: string): Promise<Worker[]> {
  return get(`/workspaces/${workspace}/workers`);
}

export function getConversations(
  workspace: string,
  bot: string,
): Promise<Message[]> {
  return get(`/workspaces/${workspace}/conversations/${bot}`);
}

export function getWorkerDetail(
  workspace: string,
  workerId: string,
): Promise<WorkerDetail> {
  return get(`/workspaces/${workspace}/workers/${workerId}`);
}

export async function sendWorkerMessage(
  workspace: string,
  workerId: string,
  message: string,
): Promise<{ ok: boolean; error?: string }> {
  const res = await fetch(
    `${BASE}/workspaces/${workspace}/workers/${workerId}/send`,
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
): Promise<BotStatus> {
  return get(`/workspaces/${workspace}/bots/${bot}/status`);
}

export async function cancelBot(
  workspace: string,
  bot: string,
): Promise<{ ok: boolean }> {
  const res = await fetch(`${BASE}/workspaces/${workspace}/bots/${bot}/cancel`, {
    method: "POST",
  });
  return res.json();
}

export function getUnread(workspace: string): Promise<Record<string, number>> {
  return get(`/workspaces/${workspace}/unread`);
}

export async function markSeen(workspace: string, bot: string): Promise<void> {
  await fetch(`${BASE}/workspaces/${workspace}/seen/${bot}`, { method: "POST" });
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

export async function sendMessage(
  workspace: string,
  bot: string,
  message: string,
  attachments?: Array<{ name: string; type: string; dataUrl: string }>,
): Promise<{ ok: boolean }> {
  const res = await fetch(`${BASE}/workspaces/${workspace}/chat/${bot}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ message, attachments }),
  });
  return res.json();
}
