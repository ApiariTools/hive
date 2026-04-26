import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi } from "vitest";
import { ChatPanel } from "../components/ChatPanel";
import type { Message } from "../types";

const mockMessages: Message[] = [
  { id: 1, workspace: "test", bot: "Main", role: "user", content: "hello", attachments: null, created_at: new Date().toISOString() },
  { id: 2, workspace: "test", bot: "Main", role: "assistant", content: "Hi there! How can I help?", attachments: null, created_at: new Date().toISOString() },
];

const defaultProps = {
  bot: "Main",
  messages: mockMessages,
  loading: false,
  loadingStatus: undefined,
  streamingContent: "",
  onSend: vi.fn(),
  workerCount: 0,
  onWorkersToggle: vi.fn(),
  onCancel: undefined,
};

describe("ChatPanel", () => {
  it("renders messages", () => {
    render(<ChatPanel {...defaultProps} />);
    expect(screen.getByText("hello")).toBeInTheDocument();
    expect(screen.getByText(/How can I help/)).toBeInTheDocument();
  });

  it("renders bot name in header", () => {
    render(<ChatPanel {...defaultProps} />);
    const mains = screen.getAllByText("Main");
    expect(mains.length).toBeGreaterThanOrEqual(1);
  });

  it("renders user messages with 'You' label", () => {
    render(<ChatPanel {...defaultProps} />);
    expect(screen.getByText("You")).toBeInTheDocument();
  });

  it("renders assistant messages with bot name", () => {
    render(<ChatPanel {...defaultProps} />);
    const botLabels = screen.getAllByText("Main");
    expect(botLabels.length).toBeGreaterThanOrEqual(1);
  });

  it("shows empty state when no messages", () => {
    render(<ChatPanel {...defaultProps} messages={[]} />);
    expect(screen.getByText(/Start a conversation/)).toBeInTheDocument();
  });

  it("shows thinking dots when loading", () => {
    render(<ChatPanel {...defaultProps} loading={true} loadingStatus="Thinking..." />);
    expect(screen.getByText("Thinking...")).toBeInTheDocument();
  });

  it("shows tool name when using a tool", () => {
    render(<ChatPanel {...defaultProps} loading={true} loadingStatus="Using Read..." />);
    expect(screen.getByText("Using Read...")).toBeInTheDocument();
  });

  it("shows streaming content while loading", () => {
    render(<ChatPanel {...defaultProps} loading={true} streamingContent="I'm working on..." />);
    expect(screen.getByText(/working on/)).toBeInTheDocument();
  });

  it("shows stop button when loading with onCancel", () => {
    render(<ChatPanel {...defaultProps} loading={true} loadingStatus="Thinking..." onCancel={() => {}} />);
    expect(screen.getByText("Stop")).toBeInTheDocument();
  });

  it("does not show stop button when not loading", () => {
    render(<ChatPanel {...defaultProps} />);
    expect(screen.queryByText("Stop")).not.toBeInTheDocument();
  });

  it("shows workers button with count", () => {
    render(<ChatPanel {...defaultProps} workerCount={3} />);
    expect(screen.getByText("3 workers")).toBeInTheDocument();
  });

  it("shows 'No workers' when count is 0", () => {
    render(<ChatPanel {...defaultProps} workerCount={0} />);
    expect(screen.getByText("No workers")).toBeInTheDocument();
  });

  it("has a textarea input", () => {
    render(<ChatPanel {...defaultProps} />);
    expect(screen.getByPlaceholderText(/Message Main/)).toBeInTheDocument();
  });

  it("renders markdown in assistant messages", () => {
    const msgs: Message[] = [
      { id: 1, workspace: "test", bot: "Main", role: "assistant", content: "**bold text**", attachments: null, created_at: new Date().toISOString() },
    ];
    render(<ChatPanel {...defaultProps} messages={msgs} />);
    expect(screen.getByText("bold text")).toBeInTheDocument();
  });

  it("renders image attachments", () => {
    const msgs: Message[] = [
      { id: 1, workspace: "test", bot: "Main", role: "user", content: "see this",
        attachments: JSON.stringify([{ name: "photo.jpg", type: "image/jpeg", dataUrl: "data:image/jpeg;base64,abc" }]),
        created_at: new Date().toISOString() },
    ];
    render(<ChatPanel {...defaultProps} messages={msgs} />);
    const img = screen.getByAltText("photo.jpg");
    expect(img).toBeInTheDocument();
  });

  it("shows system messages", () => {
    const msgs: Message[] = [
      { id: 1, workspace: "test", bot: "Main", role: "system", content: "Session reset — bot configuration was updated.", attachments: null, created_at: new Date().toISOString() },
    ];
    render(<ChatPanel {...defaultProps} messages={msgs} />);
    expect(screen.getByText(/Session reset/)).toBeInTheDocument();
  });

  it("does not show loading and empty state simultaneously", () => {
    render(<ChatPanel {...defaultProps} messages={[]} loading={true} loadingStatus="Thinking..." />);
    expect(screen.queryByText(/Start a conversation/)).not.toBeInTheDocument();
    expect(screen.getByText("Thinking...")).toBeInTheDocument();
  });
});
