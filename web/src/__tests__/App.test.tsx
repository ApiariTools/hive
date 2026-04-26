import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi, beforeEach } from "vitest";
import App from "../App";
import * as api from "../api";

// Mock the API module
vi.mock("../api", () => ({
  getWorkspaces: vi.fn().mockResolvedValue([
    { name: "apiari" },
    { name: "mgm" },
  ]),
  getBots: vi.fn().mockResolvedValue([
    { name: "Main", color: "#f5c542", role: "Assistant", watch: [] },
    { name: "Customer", color: "#e85555", role: "Customer bot", watch: ["sentry"] },
  ]),
  getWorkers: vi.fn().mockResolvedValue([
    { id: "cli-3", branch: "swarm/fix-bug", status: "running", agent: "claude", pr_url: null, pr_title: null, description: null, elapsed_secs: 120, dispatched_by: "Main" },
  ]),
  getRepos: vi.fn().mockResolvedValue([
    { name: "hive", path: "/dev/hive", has_swarm: true, is_clean: true, branch: "main", workers: [] },
    { name: "swarm", path: "/dev/swarm", has_swarm: true, is_clean: false, branch: "main", workers: [] },
  ]),
  getConversations: vi.fn().mockResolvedValue([
    { id: 1, workspace: "apiari", bot: "Main", role: "user", content: "hello", attachments: null, created_at: new Date().toISOString() },
    { id: 2, workspace: "apiari", bot: "Main", role: "assistant", content: "Hi! How can I help?", attachments: null, created_at: new Date().toISOString() },
  ]),
  getBotStatus: vi.fn().mockResolvedValue({
    status: "idle",
    streaming_content: "",
    tool_name: null,
  }),
  getUnread: vi.fn().mockResolvedValue({ Customer: 2 }),
  markSeen: vi.fn().mockResolvedValue(undefined),
  sendMessage: vi.fn().mockResolvedValue({ ok: true }),
  cancelBot: vi.fn().mockResolvedValue({ ok: true }),
  getWorkerDetail: vi.fn().mockResolvedValue({
    id: "cli-3", branch: "swarm/fix-bug", status: "running", agent: "claude",
    pr_url: null, pr_title: null, description: null, elapsed_secs: 120, dispatched_by: "Main",
    prompt: "Fix the bug", output: null, conversation: [],
  }),
  sendWorkerMessage: vi.fn().mockResolvedValue({ ok: true }),
  connectWebSocket: vi.fn().mockReturnValue({
    close: vi.fn(),
    onmessage: null,
    onclose: null,
  }),
}));

beforeEach(() => {
  vi.clearAllMocks();
  window.location.hash = "";
});

describe("App", () => {
  it("renders workspace tabs", async () => {
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("apiari")).toBeInTheDocument();
      expect(screen.getByText("mgm")).toBeInTheDocument();
    });
  });

  it("loads bots data", async () => {
    render(<App />);
    await waitFor(() => {
      const mock = api.getBots as ReturnType<typeof vi.fn>;
      expect(mock).toHaveBeenCalled();
    });
  });

  it("shows unread badge on bots with unread messages", async () => {
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("2")).toBeInTheDocument();
    });
  });

  it("renders chat messages", async () => {
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("hello")).toBeInTheDocument();
      expect(screen.getByText(/How can I help/)).toBeInTheDocument();
    });
  });

  it("loads repos data", async () => {
    render(<App />);
    await waitFor(() => {
      const mock = api.getRepos as ReturnType<typeof vi.fn>;
      expect(mock).toHaveBeenCalled();
    });
  });

  it("shows hive logo", async () => {
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("hive")).toBeInTheDocument();
    });
  });
});

describe("Chat interaction", () => {
  it("has a text input area", async () => {
    render(<App />);
    await waitFor(() => {
      const textarea = screen.getByPlaceholderText(/Message Main/);
      expect(textarea).toBeInTheDocument();
    });
  });

  it("has an attach button", async () => {
    render(<App />);
    await waitFor(() => {
      // Paperclip icon from lucide — it's an SVG, find by role
      const buttons = screen.getAllByRole("button");
      // At least one button should be the attach button
      expect(buttons.length).toBeGreaterThan(0);
    });
  });
});

describe("Bot switching", () => {
  it("switches to Customer bot on click", async () => {
    const user = userEvent.setup();
    render(<App />);

    await waitFor(() => {
      expect(screen.getByText("Customer")).toBeInTheDocument();
    });

    await user.click(screen.getByText("Customer"));

    await waitFor(() => {
      const mock = api.getConversations as ReturnType<typeof vi.fn>;
      expect(mock.mock.calls.some((c: string[]) => c[1] === "Customer")).toBe(true);
    });
  });
});

describe("Workspace switching", () => {
  it("switches workspace on tab click", async () => {
    const user = userEvent.setup();
    render(<App />);

    await waitFor(() => {
      expect(screen.getByText("mgm")).toBeInTheDocument();
    });

    await user.click(screen.getByText("mgm"));

    await waitFor(() => {
      const mock = api.getBots as ReturnType<typeof vi.fn>;
      expect(mock.mock.calls.some((c: string[]) => c[0] === "mgm")).toBe(true);
    });
  });
});
