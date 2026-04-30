import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import { ChatInput } from "../components/ChatInput";

describe("ChatInput", () => {
  it("renders textarea with placeholder", () => {
    render(<ChatInput placeholder="Type here..." onSend={vi.fn()} />);
    expect(screen.getByPlaceholderText("Type here...")).toBeInTheDocument();
  });

  it("shows attach button when showAttachments is true (default)", () => {
    render(<ChatInput placeholder="msg" onSend={vi.fn()} />);
    expect(screen.getByRole("button", { name: "Attach file" })).toBeInTheDocument();
  });

  it("hides attach button when showAttachments is false", () => {
    render(<ChatInput placeholder="msg" onSend={vi.fn()} showAttachments={false} />);
    expect(screen.queryByRole("button", { name: "Attach file" })).not.toBeInTheDocument();
  });

  it("calls onSend with text on Enter", () => {
    const onSend = vi.fn();
    render(<ChatInput placeholder="msg" onSend={onSend} />);
    const textarea = screen.getByPlaceholderText("msg");
    fireEvent.input(textarea, { target: { value: "hello" } });
    // Manually set value since fireEvent.input doesn't update uncontrolled textarea
    (textarea as HTMLTextAreaElement).value = "hello";
    fireEvent.keyDown(textarea, { key: "Enter", shiftKey: false });
    expect(onSend).toHaveBeenCalledWith("hello", undefined);
  });

  it("does not send on Shift+Enter", () => {
    const onSend = vi.fn();
    render(<ChatInput placeholder="msg" onSend={onSend} />);
    const textarea = screen.getByPlaceholderText("msg");
    (textarea as HTMLTextAreaElement).value = "hello";
    fireEvent.keyDown(textarea, { key: "Enter", shiftKey: true });
    expect(onSend).not.toHaveBeenCalled();
  });

  it("does not send on Enter on touch devices", () => {
    const original = navigator.maxTouchPoints;
    Object.defineProperty(navigator, "maxTouchPoints", { value: 1, configurable: true });
    try {
      const onSend = vi.fn();
      render(<ChatInput placeholder="msg" onSend={onSend} />);
      const textarea = screen.getByPlaceholderText("msg");
      (textarea as HTMLTextAreaElement).value = "hello";
      fireEvent.keyDown(textarea, { key: "Enter", shiftKey: false });
      expect(onSend).not.toHaveBeenCalled();
    } finally {
      Object.defineProperty(navigator, "maxTouchPoints", { value: original, configurable: true });
    }
  });

  it("sends even when disabled (queue handled by parent)", () => {
    const onSend = vi.fn();
    render(<ChatInput placeholder="msg" onSend={onSend} disabled />);
    const textarea = screen.getByPlaceholderText("msg");
    (textarea as HTMLTextAreaElement).value = "hello";
    fireEvent.keyDown(textarea, { key: "Enter", shiftKey: false });
    expect(onSend).toHaveBeenCalledWith("hello", undefined);
  });

  it("shows mic button by default (no text)", () => {
    render(<ChatInput placeholder="msg" onSend={vi.fn()} />);
    expect(screen.getByRole("button", { name: "Record voice" })).toBeInTheDocument();
  });

  it("shows queue indicator when queueCount > 0", () => {
    render(<ChatInput placeholder="msg" onSend={vi.fn()} queueCount={2} />);
    expect(screen.getByText("2 messages queued")).toBeInTheDocument();
  });

  it("hides queue indicator when queueCount is 0", () => {
    render(<ChatInput placeholder="msg" onSend={vi.fn()} queueCount={0} />);
    expect(screen.queryByText(/queued/)).not.toBeInTheDocument();
  });
});
