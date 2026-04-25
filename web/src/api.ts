import type { Workspace, Bot, Worker, Message } from "./types";

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

export interface StreamCallbacks {
  onText: (text: string) => void;
  onToolUse: (tool: string) => void;
  onDone: (fullText: string) => void;
  onError: (error: string) => void;
}

export async function sendMessageStream(
  workspace: string,
  bot: string,
  message: string,
  callbacks: StreamCallbacks,
  attachments?: Array<{ name: string; type: string; dataUrl: string }>,
): Promise<void> {
  const res = await fetch(`${BASE}/workspaces/${workspace}/chat/${bot}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ message, attachments }),
  });

  if (!res.ok || !res.body) {
    callbacks.onError(`Request failed: ${res.status}`);
    return;
  }

  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";

  while (true) {
    const { done, value } = await reader.read();
    if (done) break;

    buffer += decoder.decode(value, { stream: true });

    // Parse SSE events from buffer
    const parts = buffer.split("\n\n");
    buffer = parts.pop() || "";

    for (const part of parts) {
      const dataLine = part
        .split("\n")
        .find((line) => line.startsWith("data: "));
      if (!dataLine) continue;

      try {
        const data = JSON.parse(dataLine.slice(6));
        switch (data.type) {
          case "text":
            callbacks.onText(data.content);
            break;
          case "tool_use":
            callbacks.onToolUse(data.tool);
            break;
          case "done":
            callbacks.onDone(data.content);
            break;
          case "error":
            callbacks.onError(data.content);
            break;
        }
      } catch {
        // Skip malformed events
      }
    }
  }
}
