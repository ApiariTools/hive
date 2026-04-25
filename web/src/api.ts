import type { Workspace, Bot, Worker, Message } from "./types";

const BASE = "/api";

async function get<T>(path: string): Promise<T> {
  const res = await fetch(`${BASE}${path}`);
  if (!res.ok) throw new Error(`GET ${path}: ${res.status}`);
  return res.json();
}

async function post<T>(path: string, body: unknown): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new Error(`POST ${path}: ${res.status}`);
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

export async function sendMessage(
  workspace: string,
  bot: string,
  message: string,
): Promise<string> {
  const res = await post<{ reply: string }>(
    `/workspaces/${workspace}/chat/${bot}`,
    { message },
  );
  return res.reply;
}
