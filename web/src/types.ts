export interface Workspace {
  name: string;
}

export interface Bot {
  name: string;
  color?: string;
  role?: string;
  description?: string;
  watch: string[];
}

export interface Worker {
  id: string;
  branch: string;
  status: string;
  agent: string;
  pr_url: string | null;
  pr_title: string | null;
  description: string | null;
  elapsed_secs: number | null;
  dispatched_by: string | null;
  review_state?: string;
  ci_status?: string;
  total_comments?: number;
  open_comments?: number;
  resolved_comments?: number;
}

export interface Repo {
  name: string;
  path: string;
  has_swarm: boolean;
  is_clean: boolean;
  branch: string;
  workers: Worker[];
}

export interface Message {
  id: number;
  workspace: string;
  bot: string;
  role: string;
  content: string;
  attachments: string | null;
  created_at: string;
}

export interface WorkerDetail extends Worker {
  prompt: string | null;
  output: string | null;
  conversation: WorkerMessage[];
}

export interface WorkerMessage {
  role: string;
  content: string;
}
