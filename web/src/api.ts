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
