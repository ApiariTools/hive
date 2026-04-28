import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("../api");

import App from "../App";
import * as api from "../api";

beforeEach(() => {
  vi.clearAllMocks();
  window.location.hash = "";
});

async function renderAndSelectBot(name = "Main") {
  const user = userEvent.setup();
  render(<App />);
  await waitFor(() => expect(screen.getByText(name)).toBeInTheDocument());
  await user.click(screen.getByText(name));
  return user;
}

describe("App", () => {
  it("renders workspace tabs", async () => {
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("apiari")).toBeInTheDocument();
      expect(screen.getByText("mgm")).toBeInTheDocument();
    });
  });

  it("loads bots on mount", async () => {
    render(<App />);
    await waitFor(() => {
      expect(api.getBots).toHaveBeenCalled();
    });
  });

  it("loads repos on mount", async () => {
    render(<App />);
    await waitFor(() => {
      expect(api.getRepos).toHaveBeenCalled();
    });
  });

  it("renders chat messages", async () => {
    await renderAndSelectBot("Main");
    await waitFor(() => {
      expect(screen.getByText("hello")).toBeInTheDocument();
      expect(screen.getByText(/How can I help/)).toBeInTheDocument();
    });
  });

  it("shows unread badge", async () => {
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("2")).toBeInTheDocument();
    });
  });

  it("shows hive logo", async () => {
    render(<App />);
    expect(screen.getByText("hive")).toBeInTheDocument();
  });

  it("has a text input", async () => {
    await renderAndSelectBot("Main");
    await waitFor(() => {
      expect(screen.getByPlaceholderText(/Message Main/)).toBeInTheDocument();
    });
  });

  it("calls markSeen on bot select", async () => {
    render(<App />);
    await waitFor(() => expect(screen.getByText("Main")).toBeInTheDocument());
    expect(api.markSeen).not.toHaveBeenCalled();
    const user = userEvent.setup();
    await user.click(screen.getByText("Main"));
    await waitFor(() => {
      expect(api.markSeen).toHaveBeenCalledWith("apiari", "Main");
    });
  });

  it("connects websocket on mount", async () => {
    render(<App />);
    await waitFor(() => {
      expect(api.connectWebSocket).toHaveBeenCalled();
    });
  });
});

describe("Bot switching", () => {
  it("calls getConversations with new bot", async () => {
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => expect(screen.getByText("Customer")).toBeInTheDocument());
    await user.click(screen.getByText("Customer"));
    await waitFor(() => {
      const mock = api.getConversations as ReturnType<typeof vi.fn>;
      expect(mock.mock.calls.some((c: string[]) => c[1] === "Customer")).toBe(true);
    });
  });
});

describe("Workspace switching", () => {
  it("calls getBots with new workspace", async () => {
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => expect(screen.getByText("mgm")).toBeInTheDocument());
    await user.click(screen.getByText("mgm"));
    await waitFor(() => {
      const mock = api.getBots as ReturnType<typeof vi.fn>;
      expect(mock.mock.calls.some((c: string[]) => c[0] === "mgm")).toBe(true);
    });
  });

  it("auto-selects Main bot on mobile when switching workspaces", async () => {
    Object.defineProperty(window, "innerWidth", { value: 600, writable: true });
    window.dispatchEvent(new Event("resize"));
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => expect(screen.getByText("mgm")).toBeInTheDocument());
    await user.click(screen.getByText("mgm"));
    await waitFor(() => {
      expect(screen.getByPlaceholderText(/Message Main/)).toBeInTheDocument();
    });
    Object.defineProperty(window, "innerWidth", { value: 1024, writable: true });
  });
});

describe("Mobile auto-select", () => {
  it("auto-selects Main bot on mobile initial load without bot in hash", async () => {
    window.location.hash = "";
    Object.defineProperty(window, "innerWidth", { value: 600, writable: true });
    render(<App />);
    await waitFor(() => {
      expect(screen.getByPlaceholderText(/Message Main/)).toBeInTheDocument();
    });
    Object.defineProperty(window, "innerWidth", { value: 1024, writable: true });
  });
});

describe("WebSocket message dedup", () => {
  it("fetches conversations on WS message event instead of appending directly", async () => {
    // Capture the WS callback so we can simulate events
    let wsCallback: (event: Record<string, unknown>) => void = () => {};
    (api.connectWebSocket as ReturnType<typeof vi.fn>).mockImplementation(
      (cb: (event: Record<string, unknown>) => void) => {
        wsCallback = cb;
        return { close: vi.fn() };
      },
    );

    await renderAndSelectBot("Main");

    // Clear call counts from initial load
    (api.getConversations as ReturnType<typeof vi.fn>).mockClear();

    // Return a new message set to simulate a new message in DB
    const updatedMsgs = [
      { id: 1, workspace: "apiari", bot: "Main", role: "user", content: "hello", attachments: null, created_at: new Date().toISOString() },
      { id: 2, workspace: "apiari", bot: "Main", role: "assistant", content: "Hi! How can I help?", attachments: null, created_at: new Date().toISOString() },
      { id: 3, workspace: "apiari", bot: "Main", role: "user", content: "new message", attachments: null, created_at: new Date().toISOString() },
    ];
    (api.getConversations as ReturnType<typeof vi.fn>).mockResolvedValueOnce(updatedMsgs);

    // Simulate a WS message event for the active bot
    wsCallback({
      type: "message",
      workspace: "apiari",
      bot: "Main",
      role: "user",
      content: "new message",
    });

    // Should trigger getConversations fetch (not a direct append)
    await waitFor(() => {
      expect(api.getConversations).toHaveBeenCalledWith("apiari", "Main", 30);
    });

    // The new message should appear exactly once
    await waitFor(() => {
      expect(screen.getByText("new message")).toBeInTheDocument();
    });
    const matches = screen.getAllByText("new message");
    expect(matches).toHaveLength(1);
  });
});
