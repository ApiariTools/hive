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
    render(<App />);
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
    render(<App />);
    await waitFor(() => {
      expect(screen.getByPlaceholderText(/Message Main/)).toBeInTheDocument();
    });
  });

  it("calls markSeen on mount", async () => {
    render(<App />);
    await waitFor(() => {
      expect(api.markSeen).toHaveBeenCalled();
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
});
