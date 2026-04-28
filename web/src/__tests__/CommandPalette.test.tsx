import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi } from "vitest";
import { CommandPalette } from "../components/CommandPalette";

const workspaces = [
  { name: "apiari" },
  { name: "mgm" },
];

const bots = [
  { name: "Main", color: "#f5c542", role: "Assistant", watch: [] as string[] },
  { name: "Social", color: "#e85555", role: "Social bot", watch: [] as string[] },
];

const otherBots = [
  { workspace: "mgm", bot: { name: "Main", color: "#f5c542", role: "Assistant", watch: [] as string[] } },
  { workspace: "mgm", bot: { name: "Analytics", color: "#5cb85c", role: "Analytics bot", watch: [] as string[] } },
];

const defaultProps = {
  open: true,
  onOpenChange: vi.fn(),
  workspaces,
  bots,
  workers: [],
  currentWorkspace: "apiari",
  currentBot: "Main",
  onSelectWorkspace: vi.fn(),
  onSelectBot: vi.fn(),
  onSelectWorker: vi.fn(),
  otherWorkspaceBots: otherBots,
  onSelectWorkspaceBot: vi.fn(),
};

describe("CommandPalette", () => {
  it("renders current workspace bots", () => {
    render(<CommandPalette {...defaultProps} />);
    expect(screen.getByText("Main")).toBeInTheDocument();
    expect(screen.getByText("Social")).toBeInTheDocument();
  });

  it("renders other workspace bots with workspace prefix", () => {
    render(<CommandPalette {...defaultProps} />);
    expect(screen.getByText("mgm / Main")).toBeInTheDocument();
    expect(screen.getByText("mgm / Analytics")).toBeInTheDocument();
  });

  it("renders Other Workspace Bots heading", () => {
    render(<CommandPalette {...defaultProps} />);
    expect(screen.getByText("Other Workspace Bots")).toBeInTheDocument();
  });

  it("does not render Other Workspace Bots section when empty", () => {
    render(<CommandPalette {...defaultProps} otherWorkspaceBots={[]} />);
    expect(screen.queryByText("Other Workspace Bots")).not.toBeInTheDocument();
  });

  it("calls onSelectWorkspaceBot when other workspace bot selected", async () => {
    const user = userEvent.setup();
    const onSelectWorkspaceBot = vi.fn();
    const onOpenChange = vi.fn();
    render(
      <CommandPalette
        {...defaultProps}
        onSelectWorkspaceBot={onSelectWorkspaceBot}
        onOpenChange={onOpenChange}
      />
    );
    await user.click(screen.getByText("mgm / Analytics"));
    expect(onSelectWorkspaceBot).toHaveBeenCalledWith("mgm", "Analytics");
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  it("calls onSelectBot for current workspace bot", async () => {
    const user = userEvent.setup();
    const onSelectBot = vi.fn();
    render(<CommandPalette {...defaultProps} onSelectBot={onSelectBot} />);
    await user.click(screen.getByText("Social"));
    expect(onSelectBot).toHaveBeenCalledWith("Social");
  });
});
